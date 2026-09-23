//! `Node::selftest`: can this node actually be reached, here, now?
//!
//! Built for proving the library inside the game's Wine prefix, where the
//! question is not "does the code work" but "does this socket layer work in
//! this process". It reports, as JSON:
//!
//! * the NodeId and the UDP sockets the endpoint bound;
//! * **direct**: a second, throwaway endpoint in the same process dials this
//!   node on its own bound addresses with relays off. Success means UDP
//!   send/receive and a QUIC handshake work here;
//! * **relay**: unless relays are off, wait for a home relay, then have a
//!   throwaway endpoint dial this node knowing *only* the relay URL. Success
//!   means the relay path works end to end.
//!
//! The probe endpoints have fresh keys and are dropped straight after.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use iroh::{
    endpoint::{presets, Connection},
    protocol::{AcceptError, ProtocolHandler},
    Endpoint, EndpointAddr,
};
use n0_future::time::timeout;

use super::{json_string, Node, Shared};
use crate::{event::Event, Error, Result};

const DIRECT_TIMEOUT: Duration = Duration::from_secs(8);
const RELAY_TIMEOUT: Duration = Duration::from_secs(15);

/// Accepts selftest dials and holds them until the prober hangs up.
#[derive(Debug, Clone)]
pub(super) struct Probe;

impl ProtocolHandler for Probe {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        let _ = timeout(Duration::from_secs(20), conn.closed()).await;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Outcome {
    ok: bool,
    ms: u64,
    detail: String,
}

impl Outcome {
    fn json(&self) -> String {
        format!(
            "{{\"ok\":{},\"ms\":{},\"detail\":{}}}",
            self.ok,
            self.ms,
            json_string(&self.detail)
        )
    }
}

/// A bound wildcard address is dialed on loopback.
fn dialable(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port()),
        IpAddr::V6(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port()),
        _ => addr,
    }
}

async fn probe(shared: &Shared, relay: Option<iroh::RelayMode>, target: EndpointAddr, limit: Duration) -> Outcome {
    let builder = match relay {
        None => Endpoint::builder(presets::Minimal).relay_mode(iroh::RelayMode::Disabled),
        Some(mode) => Endpoint::builder(presets::Minimal).relay_mode(mode),
    };
    let ep = match builder.bind().await {
        Ok(ep) => ep,
        Err(e) => {
            return Outcome {
                detail: format!("probe endpoint did not bind: {e:#}"),
                ..Default::default()
            }
        }
    };
    let started = Instant::now();
    let out = match timeout(limit, ep.connect(target, &shared.selftest_alpn)).await {
        Ok(Ok(conn)) => {
            conn.close(0u32.into(), b"ok");
            Outcome {
                ok: true,
                ms: started.elapsed().as_millis() as u64,
                detail: String::new(),
            }
        }
        Ok(Err(e)) => Outcome {
            ms: started.elapsed().as_millis() as u64,
            detail: format!("{e:#}"),
            ..Default::default()
        },
        Err(_) => Outcome {
            ms: started.elapsed().as_millis() as u64,
            detail: "timed out".into(),
            ..Default::default()
        },
    };
    ep.close().await;
    out
}

async fn run(shared: Arc<Shared>, relay: Option<iroh::RelayMode>, accept_inbound: bool) -> String {
    let endpoint = shared.endpoint.clone();
    let id = endpoint.id();
    let bound = endpoint.bound_sockets();

    let direct = if !accept_inbound {
        Outcome {
            detail: "inbound connections are turned off".into(),
            ..Default::default()
        }
    } else if bound.is_empty() {
        Outcome {
            detail: "the endpoint has no bound socket".into(),
            ..Default::default()
        }
    } else {
        let mut target = EndpointAddr::new(id);
        for a in &bound {
            target = target.with_ip_addr(dialable(*a));
        }
        probe(&shared, None, target, DIRECT_TIMEOUT).await
    };

    let (relay_json, relay_url) = match relay {
        None => ("{\"ok\":false,\"ms\":0,\"detail\":\"relays are off\"}".to_string(), String::new()),
        Some(mode) => {
            let started = Instant::now();
            if timeout(RELAY_TIMEOUT, endpoint.online()).await.is_err() {
                let status = iroh::Watcher::get(&mut endpoint.home_relay_status());
                let why = status
                    .iter()
                    .find_map(|s| s.last_error().map(|e| format!("{}: {e:#}", s.url())))
                    .unwrap_or_else(|| "no home relay after 15 s".into());
                let o = Outcome {
                    ms: started.elapsed().as_millis() as u64,
                    detail: why,
                    ..Default::default()
                };
                (o.json(), String::new())
            } else {
                let url = endpoint.addr().relay_urls().next().cloned();
                match url {
                    None => ("{\"ok\":false,\"ms\":0,\"detail\":\"online but no relay url\"}".into(), String::new()),
                    Some(url) => {
                        let target = EndpointAddr::new(id).with_relay_url(url.clone());
                        let o = probe(&shared, Some(mode), target, RELAY_TIMEOUT).await;
                        (o.json(), url.to_string())
                    }
                }
            }
        }
    };

    let bound_json: Vec<String> = bound.iter().map(|a| json_string(&a.to_string())).collect();
    format!(
        "{{\"node_id\":\"{}\",\"platform\":{},\"bound\":[{}],\"relay_url\":{},\"direct\":{},\"relay\":{}}}",
        data_encoding::HEXLOWER.encode(id.as_bytes()),
        json_string(&format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)),
        bound_json.join(","),
        json_string(&relay_url),
        direct.json(),
        relay_json,
    )
}

impl Node {
    /// Test reachability in this process. Returns a handle; `Event::SelfTest`
    /// with that handle carries the JSON report (up to ~40 s later).
    pub fn selftest(&self) -> Result<u64> {
        self.check_open()?;
        let rt = self.rt.as_ref().ok_or(Error::State("node is closed"))?;
        let handle = self.shared.handle();
        let shared = self.shared.clone();
        let relay = self.relay_mode.clone();
        let inbound = self.accept_inbound;
        rt.spawn(async move {
            let report = run(shared.clone(), relay, inbound).await;
            shared.emit(Event::SelfTest { handle, report });
        });
        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_binds_are_dialed_on_loopback() {
        assert_eq!(dialable("0.0.0.0:7".parse().unwrap()), "127.0.0.1:7".parse().unwrap());
        assert_eq!(dialable("[::]:7".parse().unwrap()), "[::1]:7".parse().unwrap());
        assert_eq!(dialable("10.0.0.2:7".parse().unwrap()), "10.0.0.2:7".parse().unwrap());
    }
}
