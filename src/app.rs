// Shared daemon state: store, peers, relay queues and change notifications.
use crate::clip::ClipState;
use crate::store::*;
use crate::util::*;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const APP_TAG: &str = "lanclip/1";
const MAX_INLINE_TEXT: usize = 256 * 1024;

/// A Lanclip device seen on the network (paired or not).
#[derive(Clone)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub source: &'static str,
    pub last_seen: Instant,
}

/// A pairing request this device received and hasn't answered yet.
#[derive(Clone)]
pub struct IncomingRequest {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub key: String,
    pub code: String,
    pub at: Instant,
}

pub struct App {
    #[allow(dead_code)]
    pub data_dir: PathBuf,
    pub exe: PathBuf,
    pub store: Mutex<Store>,
    pub peers: Mutex<HashMap<String, Peer>>,
    pub port: AtomicU16,
    pub discovery_port: u16,
    pub clip: ClipState,
    version: Mutex<u64>,
    version_cv: Condvar,
    queues: Mutex<HashMap<String, mpsc::Sender<Item>>>,
    pub pending_send: Mutex<(Vec<PathBuf>, Option<Instant>, bool)>,
    /// Pairing requests we sent: peer id -> (code, sent at)
    pub outgoing: Mutex<HashMap<String, (String, Instant)>>,
    /// Pairing requests we received: peer id -> request
    pub incoming: Mutex<HashMap<String, IncomingRequest>>,
}

impl App {
    pub fn new(data_dir: PathBuf) -> Arc<App> {
        let store = Store::load(&data_dir);
        Arc::new(App {
            exe: std::env::current_exe().unwrap_or_default(),
            data_dir,
            store: Mutex::new(store),
            peers: Mutex::new(HashMap::new()),
            port: AtomicU16::new(0),
            discovery_port: std::env::var("LANCLIP_DISCOVERY_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(41234),
            clip: ClipState::default(),
            version: Mutex::new(1),
            version_cv: Condvar::new(),
            queues: Mutex::new(HashMap::new()),
            pending_send: Mutex::new((vec![], None, false)),
            outgoing: Mutex::new(HashMap::new()),
            incoming: Mutex::new(HashMap::new()),
        })
    }

    pub fn port(&self) -> u16 { self.port.load(Ordering::Relaxed) }
    pub fn settings(&self) -> Settings { self.store.lock().unwrap().settings.clone() }

    pub fn changed(&self) {
        *self.version.lock().unwrap() += 1;
        self.version_cv.notify_all();
    }

    /// Long-poll support: block until the version moves past `since` (or timeout).
    pub fn wait_change(&self, since: u64, timeout: Duration) -> u64 {
        let guard = self.version.lock().unwrap();
        let (guard, _) = self.version_cv.wait_timeout_while(guard, timeout, |v| *v == since).unwrap();
        *guard
    }

    /// Paired devices that are currently reachable.
    pub fn active_peers(&self) -> Vec<Peer> {
        let paired = self.settings().paired;
        self.peers.lock().unwrap().values().filter(|p| paired.contains_key(&p.id)).cloned().collect()
    }

    /// Devices for the UI: everything seen on the LAN plus paired devices that are offline.
    pub fn device_list(&self) -> Vec<serde_json::Value> {
        let s = self.settings();
        let peers = self.peers.lock().unwrap().clone();
        let outgoing = self.outgoing.lock().unwrap().clone();
        let incoming = self.incoming.lock().unwrap().clone();
        let mut out: Vec<serde_json::Value> = peers.values().map(|p| {
            let pending = if incoming.contains_key(&p.id) { "incoming" } else if outgoing.contains_key(&p.id) { "outgoing" } else { "" };
            let code = incoming.get(&p.id).map(|r| r.code.clone()).or_else(|| outgoing.get(&p.id).map(|o| o.0.clone()));
            serde_json::json!({ "id": p.id, "name": p.name, "host": p.host, "port": p.port, "source": p.source, "online": true,
                "paired": s.paired.contains_key(&p.id), "pending": pending, "code": code })
        }).collect();
        for (id, req) in &incoming {
            if !peers.contains_key(id) {
                out.push(serde_json::json!({ "id": id, "name": req.name, "host": req.host, "port": req.port, "online": true, "paired": false, "pending": "incoming", "code": req.code }));
            }
        }
        for (id, p) in &s.paired {
            if !peers.contains_key(id) {
                out.push(serde_json::json!({ "id": id, "name": p.name, "online": false, "paired": true, "pending": "" }));
            }
        }
        out.sort_by_key(|d| (!d["paired"].as_bool().unwrap_or(false), d["name"].as_str().unwrap_or("").to_lowercase()));
        out
    }

    pub fn content_path(&self, item: &Item) -> PathBuf {
        self.store.lock().unwrap().content_path(item)
    }

    /// Store an item read from `reader`. Returns (item, was_duplicate).
    pub fn ingest(&self, meta: Meta, reader: &mut dyn Read) -> io::Result<(Item, bool)> {
        let id = meta.id.filter(|s| valid_id(s)).unwrap_or_else(new_id);
        let items_dir = {
            let store = self.store.lock().unwrap();
            if let Some(existing) = store.get(&id) {
                drop(store);
                io::copy(reader, &mut io::sink())?;
                return Ok((existing, true));
            }
            store.items_dir.clone()
        };
        let kind = match meta.kind.as_deref() { Some("text") => "text", Some("image") => "image", _ => "file" };
        let default_name = match kind { "text" => "text.txt", "image" => "image.png", _ => "file" };
        let origin = meta.origin.unwrap_or_default();
        let mut item = Item {
            id: id.clone(),
            kind: kind.into(),
            name: safe_name(meta.name.as_deref().unwrap_or(default_name)),
            mime: meta.mime.unwrap_or_else(|| if kind == "text" { "text/plain".into() } else { "application/octet-stream".into() }).chars().take(100).collect(),
            size: 0,
            ts: meta.ts.map(|t| t as u64).filter(|t| *t > 0).unwrap_or_else(now_ms),
            origin: Origin { id: origin.id.chars().take(64).collect(), name: origin.name.chars().take(60).collect() },
            batch: meta.batch.filter(|b| valid_id(b)).unwrap_or_else(|| id.clone()),
            batch_total: meta.batch_total.map(|t| t.clamp(1.0, 100000.0) as u32).unwrap_or(1),
            rel_path: meta.rel_path.as_deref().and_then(safe_rel_path).filter(|p| p.contains('/')),
            text: None,
            truncated: false,
        };
        let dir = items_dir.join(&id);
        fs::create_dir_all(&dir)?;
        let path = dir.join(safe_name(&item.name));
        let result = (|| {
            let mut f = File::create(&path)?;
            let n = io::copy(reader, &mut f)?;
            f.flush()?;
            Ok::<u64, io::Error>(n)
        })();
        match result {
            Ok(n) => item.size = n,
            Err(e) => {
                let _ = fs::remove_dir_all(&dir);
                return Err(e);
            }
        }
        if kind == "text" {
            let mut buf = Vec::new();
            File::open(&path)?.take(MAX_INLINE_TEXT as u64).read_to_end(&mut buf)?;
            item.text = Some(String::from_utf8_lossy(&buf).into_owned());
            item.truncated = item.size as usize > MAX_INLINE_TEXT;
        }
        self.store.lock().unwrap().insert(item.clone());
        self.changed();
        Ok((item, false))
    }

    /// Add something produced on this device and push it to peers.
    pub fn add_local(self: &Arc<Self>, mut meta: Meta, reader: &mut dyn Read) -> io::Result<Item> {
        let s = self.settings();
        meta.id = None;
        meta.origin = Some(Origin { id: s.id, name: s.name });
        let (item, _) = self.ingest(meta, reader)?;
        self.relay(&item);
        Ok(item)
    }

    /// Queue the item for every paired peer. Each peer has one worker so items go out in order.
    pub fn relay(self: &Arc<Self>, item: &Item) -> usize {
        let peers = self.active_peers();
        let mut queues = self.queues.lock().unwrap();
        let mut count = 0;
        for p in peers.into_iter().filter(|p| p.id != item.origin.id) {
            let tx = queues.entry(p.id.clone()).or_insert_with(|| {
                let (tx, rx) = mpsc::channel::<Item>();
                let app = self.clone();
                let peer_id = p.id.clone();
                std::thread::spawn(move || {
                    for item in rx {
                        let peer = app.peers.lock().unwrap().get(&peer_id).cloned();
                        if let Some(peer) = peer {
                            if let Err(e) = app.send_to_peer(&peer, &item) {
                                log(format!("send to {} failed: {e}", peer.name));
                            }
                        }
                    }
                });
                tx
            });
            if tx.send(item.clone()).is_ok() {
                count += 1;
            }
        }
        count
    }

    fn send_to_peer(&self, peer: &Peer, item: &Item) -> Result<(), String> {
        let mut meta = item.clone();
        meta.text = None;
        meta.truncated = false;
        let meta = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
        let file = File::open(self.content_path(item)).map_err(|e| e.to_string())?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(3))
            .timeout_read(Duration::from_secs(60))
            .timeout_write(Duration::from_secs(60))
            .build();
        agent
            .post(&format!("http://{}:{}/api/items", peer.host, peer.port))
            .set("content-type", "application/octet-stream")
            .set("content-length", &item.size.to_string())
            .set("x-lanclip-key", &self.settings().paired.get(&peer.id).map(|p| p.key.clone()).unwrap_or_default())
            .set("x-lanclip-meta", &pct_encode(&meta))
            .set("x-lanclip-peer", &self.settings().id)
            .send(file)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn update_settings(self: &Arc<Self>, patch: &serde_json::Value) {
        {
            let mut store = self.store.lock().unwrap();
            let s = &mut store.settings;
            if let Some(n) = patch["name"].as_str().map(str::trim).filter(|n| !n.is_empty()) {
                s.name = n.chars().take(60).collect();
            }

            if let Some(b) = patch["autoSync"].as_bool() {
                s.auto_sync = b;
            }
            if let Some(list) = patch["manualPeers"].as_array() {
                s.manual_peers = list.iter().filter_map(|v| v.as_str()).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).take(50).collect();
            }
            store.save_settings();
        }
        crate::discovery::announce(self);
        self.changed();
    }
}
