// One-click pairing. Device A asks device B; B's user accepts in a pop-up; the devices swap keys.
//
//   A -> B  POST /api/pair/request  { id, name, port, key: A's key, code }   (no auth: B doesn't know A yet)
//   B shows "A wants to connect (code 1234)"; on Accept B stores A's key and calls
//   B -> A  POST /api/pair/accept   { id, name, key: B's key }                (auth: A's key, proving B got the request)
//   ...or   POST /api/pair/decline  { id }
// Either side can later POST /api/pair/remove { id } (auth: the other's key) to unpair.
use crate::app::{App, IncomingRequest};
use crate::notify::notify;
use crate::store::PairedPeer;
use crate::util::*;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REQUEST_TTL: Duration = Duration::from_secs(120);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build()
}

fn post(host: &str, port: u16, path: &str, key: Option<&str>, body: Value) -> Result<(), String> {
    let mut req = agent().post(&format!("http://{host}:{port}{path}"));
    if let Some(k) = key {
        req = req.set("x-lanclip-key", k);
    }
    req.send_string(&body.to_string()).map(|_| ()).map_err(|e| e.to_string())
}

fn add_paired(app: &App, id: &str, name: &str, key: &str) {
    let mut store = app.store.lock().unwrap();
    store.settings.paired.insert(id.to_string(), PairedPeer { name: name.chars().take(60).collect(), key: key.to_string() });
    store.save_settings();
}

/// Local user clicked "Connect" on a discovered device.
pub fn request(app: &Arc<App>, peer_id: &str) -> Result<String, String> {
    let peer = app.peers.lock().unwrap().get(peer_id).cloned().ok_or("That device is no longer on the network")?;
    let code = format!("{:04}", u16::from_le_bytes(random_bytes::<2>()) % 10000);
    let s = app.settings();
    app.outgoing.lock().unwrap().insert(peer.id.clone(), (code.clone(), Instant::now()));
    app.changed();
    let body = json!({ "id": s.id, "name": s.name, "port": app.port(), "key": s.key, "code": code });
    if let Err(e) = post(&peer.host, peer.port, "/api/pair/request", None, body) {
        app.outgoing.lock().unwrap().remove(&peer.id);
        app.changed();
        return Err(format!("Couldn't reach {}: {e}", peer.name));
    }
    Ok(code)
}

/// Another device asked to pair with us.
pub fn on_request(app: &Arc<App>, body: &Value, remote_ip: &str) -> Result<(), String> {
    let id = body["id"].as_str().filter(|s| valid_id(s)).ok_or("bad id")?.to_string();
    let key = body["key"].as_str().filter(|k| k.len() >= 8 && k.len() <= 128).ok_or("bad key")?.to_string();
    let name: String = body["name"].as_str().unwrap_or("device").chars().take(60).collect();
    let code: String = body["code"].as_str().unwrap_or("").chars().filter(char::is_ascii_digit).take(6).collect();
    let port = body["port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(7878);
    if id == app.settings().id {
        return Err("that's me".into());
    }
    let req = IncomingRequest { name: name.clone(), host: remote_ip.to_string(), port, key, code: code.clone(), at: Instant::now() };
    let first = {
        let mut incoming = app.incoming.lock().unwrap();
        // One dialog per device at a time; repeated requests just refresh the pending one.
        let first = !incoming.contains_key(&id);
        incoming.insert(id.clone(), req);
        first
    };
    app.changed();
    if first {
        let app = app.clone();
        std::thread::spawn(move || {
            let msg = format!("\u{201c}{name}\u{201d} wants to connect to this computer to share the clipboard and files.\n\nCode: {code} (it should match the one shown on {name})");
            match crate::prompt::ask("Lanclip: connect device?", &msg, "Accept", "Decline") {
                Some(accept) => {
                    if let Err(e) = respond(&app, &id, accept) {
                        notify("Lanclip", &e);
                    }
                }
                // No native dialog available: show the request on the settings page instead.
                None if std::env::var_os("LANCLIP_NO_PROMPT").is_some() => {}
                None => crate::open_settings_page(app.port()),
            }
        });
    }
    Ok(())
}

/// Local user answered a pairing request (pop-up or settings page).
pub fn respond(app: &Arc<App>, peer_id: &str, accept: bool) -> Result<(), String> {
    let Some(req) = app.incoming.lock().unwrap().remove(peer_id) else { return Ok(()) }; // already answered
    app.changed();
    let s = app.settings();
    if !accept {
        let _ = post(&req.host, req.port, "/api/pair/decline", Some(&req.key), json!({ "id": s.id }));
        return Ok(());
    }
    add_paired(app, peer_id, &req.name, &req.key);
    app.changed();
    post(&req.host, req.port, "/api/pair/accept", Some(&req.key), json!({ "id": s.id, "name": s.name, "key": s.key }))
        .map_err(|e| format!("Couldn't confirm with {}: {e}", req.name))?;
    notify("Lanclip", &format!("Connected to {}", req.name));
    Ok(())
}

/// The device we asked accepted (caller authenticated with our key).
pub fn on_accept(app: &App, body: &Value) -> Result<(), String> {
    let id = body["id"].as_str().ok_or("bad id")?;
    let key = body["key"].as_str().filter(|k| k.len() >= 8).ok_or("bad key")?;
    if app.outgoing.lock().unwrap().remove(id).is_none() {
        return Err("no pending request".into());
    }
    let name = body["name"].as_str().unwrap_or("device");
    add_paired(app, id, name, key);
    app.changed();
    notify("Lanclip", &format!("Connected to {name}"));
    Ok(())
}

pub fn on_decline(app: &App, body: &Value) {
    let id = body["id"].as_str().unwrap_or("");
    if app.outgoing.lock().unwrap().remove(id).is_some() {
        let name = app.peers.lock().unwrap().get(id).map(|p| p.name.clone()).unwrap_or_else(|| "The other device".into());
        app.changed();
        notify("Lanclip", &format!("{name} declined the connection"));
    }
}

/// Local user clicked "Disconnect". Also tells the other device (best effort).
pub fn unpair(app: &App, peer_id: &str) {
    let removed = {
        let mut store = app.store.lock().unwrap();
        let r = store.settings.paired.remove(peer_id);
        store.save_settings();
        r
    };
    app.changed();
    let peer = app.peers.lock().unwrap().get(peer_id).cloned();
    if let (Some(p), Some(paired)) = (peer, removed) {
        let me = app.settings().id;
        std::thread::spawn(move || {
            let _ = post(&p.host, p.port, "/api/pair/remove", Some(&paired.key), json!({ "id": me }));
        });
    }
}

/// The other device unpaired us (caller authenticated with our key).
pub fn on_remove(app: &App, body: &Value) {
    let id = body["id"].as_str().unwrap_or("");
    let mut store = app.store.lock().unwrap();
    if store.settings.paired.remove(id).is_some() {
        store.save_settings();
        drop(store);
        app.changed();
    }
}

pub fn expire(app: &App) {
    let mut changed = false;
    app.outgoing.lock().unwrap().retain(|_, (_, at)| { let keep = at.elapsed() < REQUEST_TTL; changed |= !keep; keep });
    app.incoming.lock().unwrap().retain(|_, r| { let keep = r.at.elapsed() < REQUEST_TTL; changed |= !keep; keep });
    if changed {
        app.changed();
    }
}
