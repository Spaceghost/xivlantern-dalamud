//! Offline helper for the disposable Wine C fixture. It receives only public
//! node ids and bound UDP ports, then serializes localhost NodeTickets using
//! the real Rust ticket codec. No identity keys, bearer invites or network sockets here.
use std::{net::{Ipv4Addr, SocketAddr}, path::PathBuf, time::{Duration, Instant}};
use iroh::{EndpointAddr, EndpointId};
use lantern_core::ticket::NodeTicket;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(std::env::args().nth(1).ok_or("missing private fixture directory")?);
    let deadline = Instant::now() + Duration::from_secs(120);
    for label in ["a", "b"] {
        let request = directory.join(format!("{label}-hint-request.txt"));
        let text = loop {
            if let Ok(text) = std::fs::read_to_string(&request) {
                if text.lines().count() == 2 { break text; }
            }
            if Instant::now() >= deadline { return Err("Wine address request timed out".into()); }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut lines = text.lines();
        let bytes = data_encoding::HEXLOWER.decode(lines.next().ok_or("missing public id")?.as_bytes())?;
        let id = EndpointId::from_bytes(&bytes.try_into().map_err(|_| "invalid public id length")?)?;
        let port: u16 = lines.next().ok_or("missing bound port")?.parse()?;
        if port == 0 { return Err("missing IPv4 bound port".into()); }
        let ticket = NodeTicket::new(EndpointAddr::from(id).with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port))));
        let temporary = directory.join(format!("{label}-hint.tmp"));
        std::fs::write(&temporary, ticket.to_string())?;
        std::fs::rename(temporary, directory.join(format!("{label}-hint.txt")))?;
    }
    Ok(())
}
