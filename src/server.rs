// HTTP API + embedded web UI. Peers, phones/browsers and the local CLI all talk to this.
use crate::actions;
use crate::app::{App, APP_TAG};
use crate::store::Meta;
use crate::util::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");

type Resp = Response<Cursor<Vec<u8>>>;

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes().to_vec(), value.as_bytes().to_vec()).expect("valid header")
}

fn json_resp(code: u16, v: Value) -> Resp {
    Response::from_data(v.to_string().into_bytes())
        .with_status_code(code)
        .with_header(header("content-type", "application/json"))
        .with_header(header("cache-control", "no-store"))
}

fn text_resp(code: u16, body: &str, ctype: &str) -> Resp {
    Response::from_data(body.as_bytes().to_vec()).with_status_code(code).with_header(header("content-type", ctype))
}

fn get_header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn safe_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn start(app: Arc<App>) -> std::io::Result<u16> {
    let preferred: u16 = std::env::var("LANCLIP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7878);
    let mut server = None;
    for port in preferred..preferred + 20 {
        if let Ok(s) = Server::http(("0.0.0.0", port)) {
            server = Some((s, port));
            break;
        }
    }
    let (server, port) = server.ok_or_else(|| std::io::Error::other("no free port"))?;
    app.port.store(port, std::sync::atomic::Ordering::Relaxed);
    let server = Arc::new(server);
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let app = app.clone();
            std::thread::spawn(move || handle(&app, req));
        }
    });
    Ok(port)
}

fn handle(app: &Arc<App>, mut req: Request) {
    let (path, query) = parse_query(req.url());
    let local = req.remote_addr().map(|a| match a.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback(),
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v| v.is_loopback()),
    }).unwrap_or(false);
    let settings = app.settings();
    let key = get_header(&req, "x-lanclip-key").map(str::to_string).or_else(|| query.get("key").cloned()).unwrap_or_default();
    let authorized = safe_eq(&key, &settings.key);

    // Static UI
    if !path.starts_with("/api/") {
        let resp = match path.as_str() {
            "/" | "/index.html" => text_resp(200, INDEX_HTML, "text/html; charset=utf-8"),
            "/app.js" => text_resp(200, APP_JS, "text/javascript; charset=utf-8"),
            "/style.css" => text_resp(200, STYLE_CSS, "text/css; charset=utf-8"),
            _ => text_resp(404, "not found", "text/plain"),
        };
        let _ = req.respond(resp);
        return;
    }

    if path == "/api/hello" {
        let _ = req.respond(json_resp(200, json!({ "app": APP_TAG, "id": settings.id, "name": settings.name, "port": app.port(), "authorized": authorized, "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION") })));
        return;
    }
    if path == "/api/pair/request" && *req.method() == Method::Post {
        let remote = req.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        let body: Value = serde_json::from_str(&read_body(&mut req, 16 * 1024)).unwrap_or(Value::Null);
        let resp = match crate::pairing::on_request(app, &body, &remote) {
            Ok(()) => json_resp(200, json!({ "ok": true })),
            Err(e) => json_resp(400, json!({ "error": e })),
        };
        let _ = req.respond(resp);
        return;
    }
    if !authorized {
        let _ = req.respond(json_resp(401, json!({ "error": "bad key" })));
        return;
    }

    let method = req.method().clone();
    let segs: Vec<&str> = path.trim_start_matches("/api/").split('/').collect();

    // Item content download (streams a file, so handled before the JSON router)
    if let (Method::Get, ["items", id, "content"]) = (&method, segs.as_slice()) {
        let item = app.store.lock().unwrap().get(id);
        let Some(item) = item else {
            let _ = req.respond(json_resp(404, json!({ "error": "not found" })));
            return;
        };
        match File::open(app.content_path(&item)) {
            Ok(f) => {
                let disp = if query.contains_key("download") { "attachment" } else { "inline" };
                let ctype = if item.kind == "text" { "text/plain; charset=utf-8".to_string() } else { item.mime.clone() };
                let resp = Response::from_file(f)
                    .with_header(header("content-type", &ctype))
                    .with_header(header("content-disposition", &format!("{disp}; filename*=UTF-8''{}", pct_encode(&item.name))))
                    .with_header(header("cache-control", "private, max-age=86400"));
                let _ = req.respond(resp);
            }
            Err(_) => { let _ = req.respond(json_resp(404, json!({ "error": "missing" }))); }
        }
        return;
    }

    if let (Method::Post, ["items"]) = (&method, segs.as_slice()) {
        let resp = receive_item(app, &mut req, local);
        let _ = req.respond(resp);
        return;
    }

    let resp = route(app, &mut req, &method, &segs, &query, local);
    let _ = req.respond(resp);
}

fn receive_item(app: &Arc<App>, req: &mut Request, local: bool) -> Resp {
    let raw = get_header(req, "x-lanclip-meta").unwrap_or("%7B%7D").to_string();
    let Ok(mut meta) = serde_json::from_str::<Meta>(&pct_decode(&raw)) else { return json_resp(400, json!({ "error": "bad meta" })) };
    let from_peer = get_header(req, "x-lanclip-peer").is_some();
    let settings = app.settings();
    if !from_peer {
        // Posted by a UI: the local browser page, or a phone on the LAN.
        meta.id = None;
        let remote = req.remote_addr().map(|a| a.ip().to_string()).unwrap_or_default();
        meta.origin = Some(if local {
            crate::store::Origin { id: settings.id.clone(), name: settings.name.clone() }
        } else {
            let who: String = meta.origin_name.clone().unwrap_or_else(|| "Browser".into()).chars().take(40).collect();
            crate::store::Origin { id: format!("web:{remote}"), name: format!("{who} via {}", settings.name) }
        });
    }
    match app.ingest(meta, req.as_reader()) {
        Ok((item, dup)) => {
            if !dup {
                if !from_peer { app.relay(&item); }
                if from_peer || !local { actions::on_incoming(app, &item); }
            }
            json_resp(200, serde_json::to_value(&item).unwrap_or_default())
        }
        Err(e) => json_resp(500, json!({ "error": e.to_string() })),
    }
}

fn read_body(req: &mut Request, limit: u64) -> String {
    let mut s = String::new();
    let _ = req.as_reader().take(limit).read_to_string(&mut s);
    s
}

fn route(app: &Arc<App>, req: &mut Request, method: &Method, segs: &[&str], query: &HashMap<String, String>, local: bool) -> Resp {
    let ok = || json_resp(200, json!({ "ok": true }));
    let forbidden = || json_resp(403, json!({ "error": "only allowed on the device itself" }));
    match (method, segs) {
        (Method::Get, ["state"]) => json_resp(200, state(app, local)),

        (Method::Get, ["wait"]) => {
            let since = query.get("since").and_then(|v| v.parse().ok()).unwrap_or(0);
            let v = app.wait_change(since, Duration::from_secs(25));
            json_resp(200, json!({ "version": v }))
        }

        (Method::Delete, ["items"]) => {
            app.store.lock().unwrap().clear();
            app.changed();
            ok()
        }
        (Method::Delete, ["items", id]) => {
            let deleted = app.store.lock().unwrap().delete(id);
            app.changed();
            json_resp(200, json!({ "ok": deleted }))
        }

        (Method::Get, ["qr"]) => {
            let target = query.get("url").cloned().unwrap_or_default();
            match qrcode::QrCode::new(target.as_bytes()) {
                Ok(code) => {
                    let svg = code.render::<qrcode::render::svg::Color>().min_dimensions(200, 200).quiet_zone(true).build();
                    text_resp(200, &svg, "image/svg+xml")
                }
                Err(_) => json_resp(400, json!({ "error": "bad url" })),
            }
        }

        // ---- pairing callbacks from the other device (authenticated with our key) ----
        (Method::Post, ["pair", what]) => {
            let body: Value = serde_json::from_str(&read_body(req, 16 * 1024)).unwrap_or(Value::Null);
            match *what {
                "accept" => match crate::pairing::on_accept(app, &body) {
                    Ok(()) => ok(),
                    Err(e) => json_resp(400, json!({ "error": e })),
                },
                "decline" => { crate::pairing::on_decline(app, &body); ok() }
                "remove" => { crate::pairing::on_remove(app, &body); ok() }
                _ => json_resp(404, json!({ "error": "not found" })),
            }
        }

        // ---- device-only endpoints (local browser page + CLI) ----
        _ if !local => forbidden(),

        (Method::Put, ["settings"]) => {
            let body = read_body(req, 64 * 1024);
            let Ok(patch) = serde_json::from_str::<Value>(&body) else { return json_resp(400, json!({ "error": "bad json" })) };
            app.update_settings(&patch);
            if let Some(b) = patch["startAtLogin"].as_bool() {
                crate::integration::set_autostart(&app.exe, b);
            }
            json_resp(200, state(app, true))
        }
        (Method::Post, ["items", id, action]) => {
            let item = app.store.lock().unwrap().get(id);
            let Some(item) = item else { return json_resp(404, json!({ "error": "not found" })) };
            let path = app.content_path(&item);
            match *action {
                "open" => { let _ = open::that_detached(&path); }
                "reveal" => crate::integration::reveal(&path),
                "copy" => {
                    match item.kind.as_str() {
                        "text" => { if let Ok(t) = std::fs::read_to_string(&path) { app.clip.write_text(&t); } }
                        "image" if item.mime == "image/png" => { app.clip.write_png(&path); }
                        _ => { app.clip.write_text(&path.to_string_lossy()); }
                    }
                }
                _ => return json_resp(404, json!({ "error": "unknown action" })),
            }
            ok()
        }
        (Method::Post, ["local", cmd]) => {
            let body = read_body(req, 16 * 1024 * 1024);
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let app2 = app.clone();
            match *cmd {
                "send" => {
                    let paths: Vec<PathBuf> = v["paths"].as_array().map(|a| a.iter().filter_map(|p| p.as_str()).map(PathBuf::from).collect()).unwrap_or_default();
                    actions::queue_send(app, paths);
                }
                "paste" => {
                    let dir = v["dir"].as_str().filter(|d| !d.is_empty()).map(PathBuf::from);
                    std::thread::spawn(move || actions::cmd_paste(&app2, dir));
                }
                "send-clipboard" => { std::thread::spawn(move || actions::cmd_send_clipboard(&app2)); }
                "send-text" => {
                    let text = v["text"].as_str().unwrap_or("").to_string();
                    std::thread::spawn(move || actions::cmd_send_text(&app2, &text));
                }
                "pair" => {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    return match crate::pairing::request(app, &id) {
                        Ok(code) => json_resp(200, json!({ "ok": true, "code": code })),
                        Err(e) => json_resp(400, json!({ "error": e })),
                    };
                }
                "pair-respond" => {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    let accept = v["accept"].as_bool().unwrap_or(false);
                    return match crate::pairing::respond(app, &id, accept) {
                        Ok(()) => ok(),
                        Err(e) => json_resp(400, json!({ "error": e })),
                    };
                }
                "unpair" => crate::pairing::unpair(app, v["id"].as_str().unwrap_or("")),
                "quit" => {
                    std::thread::spawn(|| { std::thread::sleep(Duration::from_millis(200)); std::process::exit(0); });
                }
                "uninstall" => {
                    crate::integration::uninstall();
                    std::thread::spawn(|| { std::thread::sleep(Duration::from_millis(300)); std::process::exit(0); });
                }
                _ => return json_resp(404, json!({ "error": "unknown command" })),
            }
            ok()
        }
        _ => json_resp(404, json!({ "error": "not found" })),
    }
}

fn state(app: &App, local: bool) -> Value {
    let s = app.settings();
    let items = app.store.lock().unwrap().items.clone();
    let addresses: Vec<String> = crate::discovery::lan_addrs().iter().map(|(ip, _)| ip.to_string()).collect();
    let mut v = json!({
        "self": { "id": s.id, "name": s.name, "port": app.port(), "addresses": addresses },
        "peers": app.device_list(),
        "items": items,
        "isLocal": local,
    });
    if local {
        v["settings"] = json!({
            "name": s.name, "key": s.key, "autoSync": s.auto_sync, "manualPeers": s.manual_peers,
            "startAtLogin": crate::integration::autostart_enabled(),
            "retentionDays": s.retention_days, "maxStorageGb": s.max_storage_gb,
            "usedBytes": app.store.lock().unwrap().used_bytes(),
            "version": env!("CARGO_PKG_VERSION"),
        });
    }
    v
}
