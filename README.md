# Lanclip

Copy on one computer, paste on another, from the right-click menu. Works on your local network with no cloud and no account. Lanclip runs silently in the background.

| Right-click on… | Entry | What it does |
|---|---|---|
| files / folders | **Copy to LAN** | Sends them to every paired device (folders keep their structure) |
| empty space in a folder, or a folder | **Paste from LAN** | Saves the last thing copied on *any* device into that folder. Text and images also go onto your clipboard. Existing files are never overwritten (`name (2).ext`). |
| empty space in a folder | **Copy clipboard to LAN** | Sends whatever is on the clipboard: text, an image, or files copied with Ctrl/Cmd+C |
| selected text in any app (macOS) | **Copy text to LAN** | Sends the selected text |

Where the entries show up:

- **Windows**: Explorer. On Windows 11 they're under **Show more options** (or Shift+right-click), because the new compact menu only accepts signed packaged extensions.
- **macOS**: Finder → right-click → **Quick Actions** (or **Services**). If they don't appear, enable them in System Settings → Keyboard → Keyboard Shortcuts → Services.
- **Linux**: Nautilus/Files and Caja (under **Scripts**), Nemo, Dolphin, and Thunar.

A small tray/menu-bar icon shows connected devices. It also has *Paste to Downloads*, an optional history window, *Auto-sync clipboard* (off by default), *Start at login*, and *Remove context menu*.

Phones can join through the browser: open *History & settings…* from the tray and scan the QR code.

## Install / run

```bash
npm install
```

```bash
npm start
```

On first start Lanclip installs the context-menu entries for the current user (no admin needed) and enables start-at-login. After that it lives in the background.

**Pairing:** every device gets a random pairing key. Open *History & settings…* from the tray on one device, then copy its key into the others. Devices with the same key on the same network find each other within a few seconds.

**Firewall:** allow Lanclip on *private* networks. It uses TCP 7878 (or the next free port) and UDP 41234.

## Build installers

```bash
npm run dist
```

This produces an NSIS installer on Windows, `.dmg`/`.zip` on macOS, and AppImage + `.deb` on Linux, all in `dist/`. Build each platform on its own OS. The Windows uninstaller removes the Explorer entries. On other platforms, use tray → *Remove context menu* before deleting the app, or run the app with `--lanclip-uninstall`.

## Command line

The context-menu entries simply call the app with these flags. A second launch hands the command to the running instance and exits, so you can also script it:

```
Lanclip --lanclip-send <file-or-folder>...
Lanclip --lanclip-paste <folder>
Lanclip --lanclip-send-clipboard
Lanclip --lanclip-open            # history window
Lanclip --lanclip-install         # reinstall context-menu entries
Lanclip --lanclip-uninstall       # remove them
```

`npm run headless` runs a GUI-less relay/web UI (for a home server or NAS).

## How it works

| Piece | File |
|---|---|
| Item store, HTTP + WebSocket API, discovery, relay | [src/core.js](src/core.js) |
| Copy/paste logic (folders, batches, unique names) | [src/actions.js](src/actions.js) |
| Context-menu installers per OS | [src/integration.js](src/integration.js) |
| Background service, tray, clipboard, command handling | [src/main.js](src/main.js) |
| History window / phone web UI | [ui/](ui/) |

- Devices announce themselves every 3 s over UDP broadcast. Each announcement carries an HMAC tag of the pairing key, so keys can be compared without sending them.
- **Copy to LAN** stores the selection locally and pushes it over HTTP to each paired peer, one item at a time per peer. Everything selected together forms a *batch*. Explorer launches once per selected file, so launches within 600 ms are merged into one batch.
- Each device therefore already holds the latest batch, and **Paste from LAN** is a local copy. If a large batch is still arriving, paste waits for it to finish.
- Data lives in the OS app-data folder. The last 500 items are kept.

## Security notes

- Every request needs the pairing key. Treat it like a Wi‑Fi password, and use **New** in settings to rotate it.
- Traffic is plain HTTP on your LAN, so only use Lanclip on networks you trust.
- Settings can only be changed from the device itself.

## Test

```bash
npm test
```

This starts two instances and checks pairing, auth, relay integrity, folder copy → paste with structure and no overwrites, text paste, path-traversal protection, and key mismatch.
