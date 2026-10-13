//! Bounded, in-memory snapshot downloads. The generic downloader can persist an
//! arbitrarily large blob; history checks its header BEFORE reading payload.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use iroh::{Endpoint, EndpointId};
use iroh_blobs::{get::fsm, protocol::GetRequest, Hash};

use super::{read_snapshot, HistoryRecord, MAX_SNAPSHOT_BYTES, SNAPSHOT_HEADER};

/// No disk cache or exports, and no unchecked sender-selected allocation.
/// Call from a worker. This does not store records or emit live chat events.
#[cfg(test)]
pub(crate) async fn fetch_snapshot(
    endpoint: &Endpoint,
    limiter: &tokio::sync::Semaphore,
    provider: EndpointId,
    hash: [u8; 32],
    offered_size: u64,
    room: &[u8; 32],
) -> Result<Vec<HistoryRecord>> {
    fetch_snapshot_over(
        endpoint,
        limiter,
        provider,
        hash,
        offered_size,
        room,
        iroh_blobs::ALPN,
        None,
    )
    .await
}

pub(crate) async fn fetch_snapshot_over(
    endpoint: &Endpoint,
    limiter: &tokio::sync::Semaphore,
    provider: EndpointId,
    hash: [u8; 32],
    offered_size: u64,
    room: &[u8; 32],
    alpn: &[u8],
    sealed: Option<&crate::ticket::SealedRoomTicket>,
) -> Result<Vec<HistoryRecord>> {
    let maximum = if let Some(ticket) = sealed {
        if ticket.topic.as_bytes() != room {
            bail!("sealed history ticket does not match room");
        }
        super::MAX_SEALED_SNAPSHOT_BYTES
    } else {
        MAX_SNAPSHOT_BYTES
    };
    if offered_size < SNAPSHOT_HEADER as u64 || offered_size > maximum as u64 {
        bail!("history snapshot offer exceeds bounds");
    }
    let _permit = limiter
        .try_acquire()
        .map_err(|_| anyhow!("history snapshot fetch limit reached"))?;
    tokio::time::timeout(Duration::from_secs(15), async {
        let connection = endpoint.connect(provider, alpn).await?;
        let request = GetRequest::blob(Hash::from(hash));
        let connected = fsm::start(connection, request, Default::default())
            .next()
            .await?;
        let fsm::ConnectedNext::StartRoot(root) = connected.next().await? else {
            bail!("history snapshot response has no root");
        };
        let (content, size) = root.next().next().await?;
        // Bao's decoder uses this tree size to bound and authenticate all leaves.
        // Reject lies before concatenate_into_vec can allocate/read payload.
        if size != offered_size || size > maximum as u64 {
            bail!("history snapshot size does not match offer");
        }
        let (end, bytes) = content.concatenate_into_vec().await?;
        let fsm::EndBlobNext::Closing(closing) = end.next() else {
            bail!("history snapshot unexpectedly contains children");
        };
        closing.next().await?;
        if bytes.len() as u64 != size {
            bail!("history snapshot length does not match header");
        }
        let records = match sealed {
            Some(ticket) => super::read_sealed_snapshot(ticket, provider, &bytes),
            None => read_snapshot(room, &bytes),
        };
        records.ok_or_else(|| anyhow!("invalid history snapshot framing or authentication"))
    })
    .await
    .map_err(|_| anyhow!("history snapshot fetch timed out"))?
}
