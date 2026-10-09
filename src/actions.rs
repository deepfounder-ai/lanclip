// Copy to LAN / Paste from LAN / clipboard commands, with user notifications.
use crate::app::App;
use crate::notify::notify;
use crate::store::{Item, Meta};
use crate::util::*;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_FILES: usize = 10_000;

struct Pick {
    file: PathBuf,
    name: String,
    rel: Option<String>,
    size: u64,
}

/// Expand the selection into files. Folders keep their structure via rel ("Folder/sub/file").
fn collect_files(paths: &[PathBuf]) -> Vec<Pick> {
    fn walk(abs: &Path, rel: String, out: &mut Vec<Pick>) {
        if out.len() >= MAX_FILES { return; }
        let Ok(meta) = fs::metadata(abs) else { return };
        if meta.is_dir() {
            let Ok(entries) = fs::read_dir(abs) else { return };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = e.file_name().to_string_lossy().into_owned();
                walk(&e.path(), format!("{rel}/{name}"), out);
            }
        } else if meta.is_file() {
            out.push(Pick {
                file: abs.to_path_buf(),
                name: abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into()),
                rel: rel.contains('/').then_some(rel),
                size: meta.len(),
            });
        }
    }
    let mut out = vec![];
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        walk(p, name, &mut out);
    }
    out
}

fn image_mime(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

pub struct SendResult {
    pub files: usize,
    pub bytes: u64,
    pub peers: usize,
}

pub fn send_paths(app: &Arc<App>, paths: &[PathBuf]) -> std::io::Result<SendResult> {
    let picks = collect_files(paths);
    let batch = new_id();
    for p in &picks {
        let mime = image_mime(&p.name);
        let meta = Meta {
            kind: Some(if mime.is_some() && p.rel.is_none() { "image" } else { "file" }.into()),
            name: Some(p.name.clone()),
            mime: Some(mime.unwrap_or("application/octet-stream").into()),
            batch: Some(batch.clone()),
            batch_total: Some(picks.len() as f64),
            rel_path: p.rel.clone(),
            ..Default::default()
        };
        app.add_local(meta, &mut File::open(&p.file)?)?;
    }
    Ok(SendResult { files: picks.len(), bytes: picks.iter().map(|p| p.size).sum(), peers: app.active_peers().len() })
}

pub fn send_text(app: &Arc<App>, text: &str) -> std::io::Result<Item> {
    let meta = Meta { kind: Some("text".into()), name: Some("text.txt".into()), mime: Some("text/plain".into()), ..Default::default() };
    app.add_local(meta, &mut text.as_bytes())
}

pub fn send_png(app: &Arc<App>, png: &[u8]) -> std::io::Result<Item> {
    let meta = Meta { kind: Some("image".into()), name: Some(format!("image-{}.png", now_ms())), mime: Some("image/png".into()), ..Default::default() };
    app.add_local(meta, &mut &png[..])
}

fn peers_note(peers: usize) -> String {
    if peers > 0 { format!("to {}", plural(peers, "device")) } else { "— no other devices found yet; they can paste it once they connect".into() }
}

fn preview(text: &str) -> String {
    let t: String = text.chars().take(80).collect();
    if text.chars().count() > 80 { format!("{t}…") } else { t }
}

// ---------- commands (called by the CLI through the local API) ----------

/// Explorer runs a verb once per selected file, so sends arriving close together become one batch.
pub fn queue_send(app: &Arc<App>, paths: Vec<PathBuf>) {
    let mut p = app.pending_send.lock().unwrap();
    p.0.extend(paths);
    p.1 = Some(Instant::now() + Duration::from_millis(600));
    if p.2 { return; }
    p.2 = true;
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(100));
        let mut p = app.pending_send.lock().unwrap();
        if p.1.is_some_and(|d| Instant::now() >= d) {
            let mut paths = std::mem::take(&mut p.0);
            paths.dedup();
            p.2 = false;
            drop(p);
            cmd_send(&app, &paths);
            return;
        }
    });
}

fn cmd_send(app: &Arc<App>, paths: &[PathBuf]) {
    match send_paths(app, paths) {
        Ok(r) if r.files == 0 => notify("Copy to LAN", "Nothing to send."),
        Ok(r) => {
            let what = if paths.len() == 1 && r.files == 1 {
                paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                plural(r.files, "file")
            };
            notify("Copied to LAN", &format!("{what} ({}) {}", fmt_size(r.bytes), peers_note(r.peers)));
        }
        Err(e) => notify("Copy to LAN failed", &e.to_string()),
    }
}

pub fn cmd_send_text(app: &Arc<App>, text: &str) {
    if text.is_empty() { return; }
    match send_text(app, text) {
        Ok(_) => notify("Copied text to LAN", &format!("{}\n{}", preview(text), peers_note(app.active_peers().len()))),
        Err(e) => notify("Copy to LAN failed", &e.to_string()),
    }
}

pub fn cmd_send_clipboard(app: &Arc<App>) {
    let files = app.clip.read_files();
    if !files.is_empty() {
        return cmd_send(app, &files);
    }
    if let Some(text) = app.clip.read_text() {
        *app.clip.last_text.lock().unwrap() = Some(text.clone());
        return cmd_send_text(app, &text);
    }
    if let Some((png, h)) = app.clip.read_image() {
        *app.clip.last_image.lock().unwrap() = Some(h);
        return match send_png(app, &png) {
            Ok(_) => notify("Copied image to LAN", &peers_note(app.active_peers().len())),
            Err(e) => notify("Copy to LAN failed", &e.to_string()),
        };
    }
    notify("Lanclip", "The clipboard is empty.");
}

/// Wait until the latest batch is complete (or stops making progress), then return it.
fn wait_for_latest(app: &App) -> Option<crate::store::Batch> {
    let start = Instant::now();
    let mut last = (0usize, Instant::now());
    loop {
        let batch = app.store.lock().unwrap().latest_batch()?;
        if batch.items.len() as u32 >= batch.total { return Some(batch); }
        if batch.items.len() != last.0 { last = (batch.items.len(), Instant::now()); }
        if last.1.elapsed() > Duration::from_secs(15) || start.elapsed() > Duration::from_secs(600) { return Some(batch); }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn unique_path(dir: &Path, rel: &str) -> PathBuf {
    let target = rel.split('/').fold(dir.to_path_buf(), |p, s| p.join(s));
    if !target.exists() { return target; }
    let name = target.file_name().unwrap().to_string_lossy().into_owned();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.clone(), String::new()),
    };
    (2..).map(|i| target.with_file_name(format!("{stem} ({i}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// Copy the latest batch into `dir`. Returns written top-level paths and the batch.
pub fn paste_latest(app: &App, dir: &Path) -> std::io::Result<Option<(Vec<PathBuf>, crate::store::Batch)>> {
    let Some(batch) = wait_for_latest(app) else { return Ok(None) };
    fs::create_dir_all(dir)?;
    let mut written: Vec<PathBuf> = vec![];
    let mut top_rename: std::collections::HashMap<String, String> = Default::default();
    for item in &batch.items {
        let src = app.content_path(item);
        let rel = if item.kind == "text" { "LAN text.txt".to_string() } else { item.rel_path.as_deref().and_then(safe_rel_path).unwrap_or_else(|| safe_name(&item.name)) };
        match rel.split_once('/') {
            Some((top, rest)) => {
                // A pasted folder whose name already exists is renamed as a whole ("Photos (2)/...").
                let top = top_rename.entry(top.to_string()).or_insert_with(|| unique_path(dir, top).file_name().unwrap().to_string_lossy().into_owned()).clone();
                let target = rest.split('/').fold(dir.join(&top), |p, s| p.join(s));
                fs::create_dir_all(target.parent().unwrap())?;
                fs::copy(&src, &target)?;
                let top_path = dir.join(&top);
                if !written.contains(&top_path) { written.push(top_path); }
            }
            None => {
                let target = unique_path(dir, &rel);
                fs::copy(&src, &target)?;
                written.push(target);
            }
        }
    }
    Ok(Some((written, batch)))
}

pub fn cmd_paste(app: &Arc<App>, dir: Option<PathBuf>) {
    let mut dir = dir.filter(|d| d.exists()).unwrap_or_else(|| dirs::download_dir().unwrap_or_else(std::env::temp_dir));
    if dir.is_file() {
        dir = dir.parent().map(Path::to_path_buf).unwrap_or(dir);
    }
    if let Some(b) = app.store.lock().unwrap().latest_batch() {
        if (b.items.len() as u32) < b.total {
            notify("Paste from LAN", &format!("Still receiving {}/{} files…", b.items.len(), b.total));
        }
    }
    let (written, batch) = match paste_latest(app, &dir) {
        Ok(Some(r)) => r,
        Ok(None) => return notify("Paste from LAN", "Nothing has been copied to the LAN yet."),
        Err(e) => return notify("Paste from LAN failed", &e.to_string()),
    };
    let first = &batch.items[0];
    let from = if first.origin.id == app.settings().id { "this device".to_string() } else { first.origin.name.clone() };
    let single = (batch.items.len() == 1).then_some(first);
    let mut on_clipboard = false;
    if let Some(item) = single {
        let path = app.content_path(item);
        on_clipboard = match item.kind.as_str() {
            "text" => fs::read_to_string(&path).map(|t| app.clip.write_text(&t)).unwrap_or(false),
            "image" if item.mime == "image/png" => app.clip.write_png(&path),
            _ => false,
        };
    }
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let missing = batch.total.saturating_sub(batch.items.len() as u32);
    let body = if single.is_some_and(|i| i.kind == "text") {
        format!("Text from {from}{} saved as {}", if on_clipboard { " is on your clipboard and" } else { "" }, name(&written[0]))
    } else {
        let what = if written.len() == 1 { name(&written[0]) } else { plural(batch.items.len(), "file") };
        format!("{what} from {from}{}{}", if on_clipboard { " (also on your clipboard)" } else { "" }, if missing > 0 { format!(" — {missing} file(s) never arrived") } else { String::new() })
    };
    notify("Pasted from LAN", &body);
}

/// Optional continuous clipboard sync (off by default).
pub fn start_auto_sync(app: Arc<App>) {
    app.clip.remember();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(800));
        if !app.settings().auto_sync { continue; }
        if let Some(text) = app.clip.read_text() {
            let changed = app.clip.last_text.lock().unwrap().as_deref() != Some(text.as_str());
            if changed {
                *app.clip.last_text.lock().unwrap() = Some(text.clone());
                let _ = send_text(&app, &text);
            }
            continue;
        }
        if let Some(h) = app.clip.image_hash() {
            if *app.clip.last_image.lock().unwrap() != Some(h) {
                if let Some((png, h)) = app.clip.read_image() {
                    *app.clip.last_image.lock().unwrap() = Some(h);
                    let _ = send_png(&app, &png);
                }
            }
        }
    });
}

/// Incoming text/images go straight to the clipboard when auto-sync is on.
pub fn on_incoming(app: &App, item: &Item) {
    if !app.settings().auto_sync { return; }
    let path = app.content_path(item);
    match item.kind.as_str() {
        "text" => { if let Ok(t) = fs::read_to_string(&path) { app.clip.write_text(&t); } }
        "image" if item.mime == "image/png" => { app.clip.write_png(&path); }
        _ => {}
    }
}
