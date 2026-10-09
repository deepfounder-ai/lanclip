// Lanclip: "Copy to LAN" / "Paste from LAN" from the file manager context menu.
//
//   lanclip                    start the background service if needed and open settings in the browser
//   lanclip daemon             run the background service (used by start-at-login)
//   lanclip send <paths...>    Copy to LAN
//   lanclip paste [folder]     Paste from LAN
//   lanclip send-clipboard     Copy clipboard to LAN
//   lanclip send-text          Copy text from stdin to LAN
//   lanclip install | uninstall | quit
#![cfg_attr(windows, windows_subsystem = "windows")]

mod actions;
mod app;
mod clip;
mod discovery;
mod integration;
mod notify;
mod pairing;
mod prompt;
mod server;
mod store;
mod util;

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use util::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    let result = match cmd {
        "daemon" => run_daemon(rest.iter().any(|a| a == "--open")),
        "" | "open" => launch(),
        "send" => {
            let paths: Vec<String> = rest.iter().map(|p| absolute(p)).collect();
            if paths.is_empty() { return; }
            local_command("send", json!({ "paths": paths }))
        }
        "paste" => local_command("paste", json!({ "dir": rest.first().map(|p| absolute(p)).unwrap_or_default() })),
        "send-clipboard" => local_command("send-clipboard", json!({})),
        "send-text" => {
            let mut text = String::new();
            let _ = std::io::stdin().read_to_string(&mut text);
            local_command("send-text", json!({ "text": text }))
        }
        "install" => launch(),
        "uninstall" => uninstall(),
        "quit" => { let _ = call_daemon("/api/local/quit", json!({}), Duration::from_secs(2)); Ok(()) }
        "--version" | "-V" => { println!("lanclip {}", env!("CARGO_PKG_VERSION")); Ok(()) }
        _ => {
            println!("usage: lanclip [daemon | send <paths> | paste [dir] | send-clipboard | send-text | uninstall | quit]");
            Ok(())
        }
    };
    if let Err(e) = result {
        log(&e);
        notify::notify("Lanclip", &e);
        std::process::exit(1);
    }
}

fn absolute(p: &str) -> String {
    let path = PathBuf::from(p);
    std::path::absolute(&path).unwrap_or(path).display().to_string()
}

// ---------- talking to the running service ----------

#[derive(serde::Deserialize, serde::Serialize)]
struct Runtime { port: u16, pid: u32 }

fn runtime_file() -> PathBuf { data_dir().join("runtime.json") }

fn key() -> String {
    store::read_json::<store::Settings>(&data_dir().join("settings.json")).map(|s| s.key).unwrap_or_default()
}

fn agent(timeout: Duration) -> ureq::Agent { ureq::AgentBuilder::new().timeout(timeout).build() }

/// The running service's (port, version) if it answers with our key.
fn daemon_hello() -> Option<(u16, String)> {
    let rt = store::read_json::<Runtime>(&runtime_file())?;
    let resp = agent(Duration::from_millis(800)).get(&format!("http://127.0.0.1:{}/api/hello", rt.port)).set("x-lanclip-key", &key()).call().ok()?;
    let v: Value = serde_json::from_str(&resp.into_string().ok()?).ok()?;
    (v["app"] == app::APP_TAG && v["authorized"] == true).then(|| (rt.port, v["version"].as_str().unwrap_or("").to_string()))
}

fn daemon_port() -> Option<u16> {
    daemon_hello().map(|(port, _)| port)
}

/// Stop a running service of a different version (after an upgrade) so the new binary takes over.
fn replace_old_daemon() {
    if daemon_hello().is_some_and(|(_, v)| v != env!("CARGO_PKG_VERSION")) {
        let _ = call_daemon("/api/local/quit", json!({}), Duration::from_secs(2));
        let start = Instant::now();
        while daemon_port().is_some() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

fn spawn_daemon(exe: &Path, open: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon");
    if open { cmd.arg("--open"); }
    quiet(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // survive the file manager / Automator closing
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("could not start Lanclip: {e}"))
}

fn ensure_daemon() -> Result<u16, String> {
    if let Some(p) = daemon_port() { return Ok(p); }
    spawn_daemon(&std::env::current_exe().map_err(|e| e.to_string())?, false)?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(p) = daemon_port() { return Ok(p); }
    }
    Err("Lanclip service did not start".into())
}

fn call_daemon(path: &str, body: Value, timeout: Duration) -> Result<(), String> {
    let port = daemon_port().ok_or("Lanclip is not running")?;
    agent(timeout)
        .post(&format!("http://127.0.0.1:{port}{path}"))
        .set("x-lanclip-key", &key())
        .send_string(&body.to_string())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn local_command(cmd: &str, body: Value) -> Result<(), String> {
    ensure_daemon()?;
    call_daemon(&format!("/api/local/{cmd}"), body, Duration::from_secs(30))
}

pub fn open_settings_page(port: u16) {
    open_settings(port);
}

fn open_settings(port: u16) {
    let _ = open::that_detached(format!("http://127.0.0.1:{port}/?key={}", pct_encode(&key())));
}

/// Double-click / Start Menu / Launchpad: install if needed, make sure the service runs, show settings.
fn launch() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = integration::install_target();
    let installed_here = std::fs::canonicalize(&exe).ok() == std::fs::canonicalize(&target).ok();
    if installed_here || cfg!(target_os = "macos") {
        replace_old_daemon();
        if let Some(port) = daemon_port() {
            open_settings(port);
            return Ok(());
        }
        return spawn_daemon(&exe, true);
    }
    // Running a freshly downloaded copy: stop any older service, install, start the installed copy.
    let _ = call_daemon("/api/local/quit", json!({}), Duration::from_secs(2));
    let installed = integration::install_binary().map_err(|e| format!("install failed: {e}"))?;
    spawn_daemon(&installed, true)
}

fn uninstall() -> Result<(), String> {
    if call_daemon("/api/local/uninstall", json!({}), Duration::from_secs(5)).is_err() {
        integration::uninstall();
    }
    notify::notify("Lanclip", "Lanclip and its context menu entries were removed.");
    Ok(())
}

// ---------- the background service ----------

fn run_daemon(open: bool) -> Result<(), String> {
    if let Some(port) = daemon_port() {
        if open { open_settings(port); }
        return Ok(());
    }
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let app = app::App::new(dir.clone());
    let port = server::start(app.clone()).map_err(|e| format!("server: {e}"))?;
    store::write_json(&runtime_file(), &Runtime { port, pid: std::process::id() });
    if let Err(e) = discovery::start(app.clone()) {
        log(format!("discovery disabled: {e}"));
    }
    actions::start_auto_sync(app.clone());
    // Retention: drop history older than the limit even when nothing new arrives.
    let a = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(600));
        if a.store.lock().unwrap().prune() {
            a.changed();
        }
    });

    // Menu entries point at this binary, so refresh them on every start (it may have moved/updated).
    let integrate = std::env::var_os("LANCLIP_NO_INTEGRATION").is_none();
    if integrate {
        integration::install_menus(&app.exe);
    }
    let marker = dir.join(".installed-native");
    let first_run = integrate && !marker.exists();
    if first_run {
        integration::set_autostart(&app.exe, true);
        let _ = std::fs::write(&marker, env!("CARGO_PKG_VERSION"));
        let where_ = if cfg!(windows) { "Explorer (Windows 11: \"Show more options\")" } else if cfg!(target_os = "macos") { "Finder ??? Quick Actions" } else { "your file manager" };
        notify::notify("Lanclip is running", &format!("Right-click files ??? \"Copy to LAN\", right-click in a folder ??? \"Paste from LAN\" in {where_}."));
    }
    log(format!("listening on port {port}"));
    if open { open_settings(port); }
    loop { std::thread::park(); }
}
