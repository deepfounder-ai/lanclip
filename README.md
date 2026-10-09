# Lanclip

Copy on one computer, paste on another, from the right-click menu. Everything stays on your local network: no cloud, no account. Lanclip is a ~1–3 MB background program with no window.

| Right-click on… | Entry | What it does |
|---|---|---|
| files / folders | **Copy to LAN** | Sends them to every paired device (folders keep their structure) |
| empty space in a folder, or a folder | **Paste from LAN** | Saves the last thing copied on *any* device into that folder. Text and images also go onto your clipboard. Existing files are never overwritten (`name (2).ext`). |
| empty space in a folder | **Copy clipboard to LAN** | Sends the clipboard: text, an image, or files copied with Ctrl/Cmd+C |
| selected text in any app (macOS) | **Copy text to LAN** | Sends the selected text |

Where the entries show up:

- **Windows**: Explorer. On Windows 11 they're under **Show more options** (or Shift+right-click).
- **macOS**: Finder → right-click → **Quick Actions** (or **Services**).
- **Linux**: GNOME Files/Nautilus and Caja (under **Scripts**), Nemo, Dolphin, and Thunar.

## Install

Download for your system from the latest build or release.

**Windows**: run `Lanclip-windows-x64.exe`. If SmartScreen appears, click **More info → Run anyway** (the app isn't code-signed). Lanclip installs itself for your user (no admin), adds the menu entries, starts at login, and opens its settings page. It shows up in Start Menu and in Settings → Apps for uninstalling.

**macOS**: open `Lanclip-macos.dmg` and drag **Lanclip** into **Applications**, then open it. The app is signed ad hoc, not with an Apple Developer ID, so the first launch is blocked:

- macOS 15 and later: open System Settings → Privacy & Security, scroll down, and click **Open Anyway**.
- macOS 14 and earlier: right-click Lanclip → **Open** → **Open**.
- Or remove the quarantine flag in Terminal:

  ```bash
  xattr -cr /Applications/Lanclip.app
  ```

If the Quick Actions don't appear, enable them in System Settings → Keyboard → Keyboard Shortcuts → Services.

**Linux**: download `lanclip-linux-x64`, then run it:

```bash
chmod +x lanclip-linux-x64 && ./lanclip-linux-x64
```

It installs itself to `~/.local/bin/lanclip` and adds menu entries, an app-launcher entry, and autostart. Restart your file manager once (for example, `nautilus -q`).

## Pair your devices (once)

Every device starts with its own random pairing key.

1. Open Lanclip from the Start Menu, Launchpad, or app launcher. The settings page opens in your browser.
2. Click **Settings**, copy the **Pairing key**, and paste it into the same field on your other devices.
3. The devices find each other within a few seconds.

**Phone:** scan the QR code in Settings. The phone must be on the same Wi‑Fi. You can send and download items from the phone's browser.

**Firewall:** allow Lanclip on *private* networks. It uses TCP 7878 and UDP 41234. If discovery is blocked (guest Wi‑Fi, some routers), add the other device's IP under **Manual peers**.

## Command line

The menu entries call the same binary:

```
lanclip                     open settings (starts the service if needed)
lanclip send <paths...>     Copy to LAN
lanclip paste [folder]      Paste from LAN
lanclip send-clipboard      Copy clipboard to LAN
lanclip send-text           Copy stdin to LAN
lanclip daemon              run the background service
lanclip quit | uninstall
```

## Build from source

```bash
cargo build --release
```

The binary is `target/release/lanclip` (`.exe` on Windows). To run the end-to-end test against two local instances:

```bash
python test/smoke.py
```

The GitHub workflow in `.github/workflows/build.yml` builds Windows, universal macOS (`.dmg`/`.zip`), and Linux packages and runs the test on each.

## How it works

| Piece | File |
|---|---|
| CLI, launcher, background service startup | [src/main.rs](src/main.rs) |
| State, item ingest, per-peer relay queues | [src/app.rs](src/app.rs) |
| HTTP API and embedded web UI | [src/server.rs](src/server.rs), [ui/](ui/) |
| UDP discovery and manual peers | [src/discovery.rs](src/discovery.rs) |
| Copy/paste logic and notifications | [src/actions.rs](src/actions.rs), [src/notify.rs](src/notify.rs) |
| Clipboard (text, PNG images, copied files) | [src/clip.rs](src/clip.rs) |
| Menu entries, autostart, self-install per OS | [src/integration.rs](src/integration.rs) |

- Devices announce themselves every 3 s over UDP broadcast, with an HMAC tag of the pairing key so keys can be compared without sending them.
- **Copy to LAN** stores the selection and pushes it over HTTP to each paired peer, in order. Everything selected together is one *batch*. Explorer launches the command once per file, so launches within 600 ms are merged.
- Every device already holds the latest batch, so **Paste from LAN** is a local copy. It waits if a large batch is still arriving.
- Menu commands are tiny calls to the running service on `127.0.0.1`. If the service isn't running, the command starts it.

## Security notes

- Every request needs the pairing key. Treat it like a Wi‑Fi password.
- Traffic is plain HTTP on your LAN, so only use Lanclip on networks you trust.
- Settings and local actions (open, reveal, quit, uninstall) are only accepted from the device itself.
