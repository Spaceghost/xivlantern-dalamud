//! Direct verified downloads without the generic downloader's disk cache.

use anyhow::{anyhow, bail, Result};
use iroh::{Endpoint, EndpointAddr};
use iroh_blobs::{get::fsm, protocol::GetRequest, Hash};
use std::time::Duration;

pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Caller holds a concurrency permit through verification and local save.
/// Check the Bao tree size before any payload allocation. No remote path or
/// disk-cache write is used, and dropping this future cancels its stream.
pub(crate) async fn fetch(
    endpoint: &Endpoint,
    provider: EndpointAddr,
    hash: [u8; 32],
    offered_size: u64,
    max_size: u64,
    alpn: &[u8],
) -> Result<Vec<u8>> {
    if max_size > crate::sealed_file::MAX_CIPHERTEXT_BYTES || offered_size > max_size {
        bail!("file offer exceeds download bounds");
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        let connection = endpoint.connect(provider, alpn).await?;
        let request = GetRequest::blob(Hash::from(hash));
        let connected = fsm::start(connection, request, Default::default()).next().await?;
        let fsm::ConnectedNext::StartRoot(root) = connected.next().await? else {
            bail!("file response has no root");
        };
        let (content, size) = root.next().next().await?;
        if size != offered_size || size > max_size {
            bail!("file tree size does not match bounded offer");
        }
        // Bao authenticates every leaf against the requested content hash.
        let (end, bytes) = content.concatenate_into_vec().await?;
        let fsm::EndBlobNext::Closing(closing) = end.next() else {
            bail!("file response unexpectedly contains children");
        };
        closing.next().await?;
        if bytes.len() as u64 != size {
            bail!("file length does not match tree header");
        }
        Ok(bytes)
    }).await.map_err(|_| anyhow!("file download timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{endpoint::presets, protocol::Router};
    use iroh_blobs::{store::readonly_mem::ReadonlyMemStore, BlobsProtocol};

    #[tokio::test]
    async fn verified_content_is_bounded_before_reading_and_has_no_disk_cache() {
        let server = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled).bind().await.unwrap();
        let client = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled).bind().await.unwrap();
        let bytes = vec![42u8; 65537];
        let hash = *Hash::new(&bytes).as_bytes();
        let store = ReadonlyMemStore::new([bytes.clone()]);
        let router = Router::builder(server.clone())
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&store, None)).spawn();
        let provider = server.addr();
        assert!(fetch(&client, provider.clone(), hash, bytes.len() as u64, 65536, iroh_blobs::ALPN).await.is_err());
        let wrong_offer = fetch(&client, provider.clone(), hash, 10, MAX_FILE_BYTES, iroh_blobs::ALPN).await.unwrap_err();
        assert!(wrong_offer.to_string().contains("tree size"));
        assert_eq!(fetch(&client, provider.clone(), hash, bytes.len() as u64, MAX_FILE_BYTES, iroh_blobs::ALPN).await.unwrap(), bytes);
        assert!(fetch(&client, provider, [0; 32], 10, MAX_FILE_BYTES, iroh_blobs::ALPN).await.is_err());
        router.shutdown().await.unwrap();
        client.close().await;
    }
}
