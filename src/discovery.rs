// LAN discovery: UDP broadcast announcements every 3 s, plus probing of manually added peers.
use crate::app::{App, Peer, APP_TAG};
use crate::util::*;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const ANNOUNCE_EVERY: Duration = Duration::from_secs(3);
const PEER_TTL: Duration = Duration::from_secs(10);

static SOCKET: OnceLock<UdpSocket> = OnceLock::new();

/// HMAC of the pairing key, so peers can compare keys without revealing them.
pub fn key_tag(key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac accepts any key");
    mac.update(b"lanclip-discovery");
    hex(&mac.finalize().into_bytes())[..16].to_string()
}

/// Non-loopback IPv4 addresses with their netmasks.
pub fn lan_addrs() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some((v4.ip, v4.netmask)),
            _ => None,
        })
        .filter(|(ip, _)| !ip.is_link_local())
        .collect()
}

fn broadcast_of(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(ip) | !u32::from(mask))
}

/// Starts discovery. Manual peers keep working even if the UDP port can't be bound.
pub fn start(app: Arc<App>) -> std::io::Result<()> {
    let a = app.clone();
    std::thread::spawn(move || loop {
        announce(&a);
        probe_manual(&a);
        expire(&a);
        std::thread::sleep(ANNOUNCE_EVERY);
    });

    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    // macOS/BSD need SO_REUSEPORT for several listeners on one broadcast port.
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    sock.set_reuse_port(true)?;
    sock.set_broadcast(true)?;
    sock.bind(&SocketAddr::from(([0, 0, 0, 0], app.discovery_port)).into())?;
    let udp: UdpSocket = sock.into();
    let listener = udp.try_clone()?;
    let _ = SOCKET.set(udp);

    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            if let Ok((n, from)) = listener.recv_from(&mut buf) {
                on_announce(&app, &buf[..n], from);
            }
        }
    });
    Ok(())
}

pub fn announce(app: &App) {
    let Some(sock) = SOCKET.get() else { return };
    let s = app.settings();
    let msg = json!({ "app": APP_TAG, "id": s.id, "name": s.name, "port": app.port(), "tag": key_tag(&s.key) }).to_string();
    let mut targets = vec![Ipv4Addr::BROADCAST];
    for (ip, mask) in lan_addrs() {
        let b = broadcast_of(ip, mask);
        if !targets.contains(&b) {
            targets.push(b);
        }
    }
    for t in targets {
        let _ = sock.send_to(msg.as_bytes(), (t, app.discovery_port));
    }
}

fn on_announce(app: &App, data: &[u8], from: SocketAddr) {
    let Ok(m) = serde_json::from_slice::<Value>(data) else { return };
    let s = app.settings();
    let id = m["id"].as_str().unwrap_or("");
    if m["app"] != APP_TAG || id.is_empty() || id == s.id {
        return;
    }
    let peer = Peer {
        id: id.chars().take(64).collect(),
        name: m["name"].as_str().unwrap_or("device").chars().take(60).collect(),
        host: from.ip().to_string(),
        port: m["port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(7878),
        source: "lan",
        same_key: m["tag"].as_str() == Some(key_tag(&s.key).as_str()),
        last_seen: Instant::now(),
    };
    let is_new = upsert(app, peer);
    if is_new {
        announce(app); // let the newcomer learn about us right away
    }
}

/// Insert/refresh a peer. Returns true if it was not known before.
fn upsert(app: &App, peer: Peer) -> bool {
    let mut peers = app.peers.lock().unwrap();
    let prev = peers.insert(peer.id.clone(), peer.clone());
    drop(peers);
    let changed = match &prev {
        None => true,
        Some(p) => p.name != peer.name || p.host != peer.host || p.same_key != peer.same_key || p.port != peer.port,
    };
    if changed {
        app.changed();
    }
    prev.is_none()
}

fn probe_manual(app: &App) {
    let s = app.settings();
    for addr in &s.manual_peers {
        let (host, port) = match addr.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().unwrap_or(7878)),
            None => (addr.clone(), 7878),
        };
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(2)).build();
        let Ok(resp) = agent.get(&format!("http://{host}:{port}/api/hello")).set("x-lanclip-key", &s.key).call() else { continue };
        let Ok(hello) = serde_json::from_str::<Value>(&resp.into_string().unwrap_or_default()) else { continue };
        let id = hello["id"].as_str().unwrap_or("");
        if hello["app"] != APP_TAG || id.is_empty() || id == s.id {
            continue;
        }
        upsert(app, Peer {
            id: id.to_string(),
            name: hello["name"].as_str().unwrap_or(&host).chars().take(60).collect(),
            host: host.clone(),
            port,
            source: "manual",
            same_key: hello["authorized"].as_bool().unwrap_or(false),
            last_seen: Instant::now(),
        });
    }
}

fn expire(app: &App) {
    let mut peers = app.peers.lock().unwrap();
    let before = peers.len();
    peers.retain(|_, p| p.last_seen.elapsed() < PEER_TTL);
    let changed = peers.len() != before;
    drop(peers);
    if changed {
        app.changed();
    }
}
