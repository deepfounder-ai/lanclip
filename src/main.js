// Lanclip background service. Runs in the tray (no window) and executes context-menu commands:
//   --lanclip-send <paths...>        Copy to LAN (files / folders)
//   --lanclip-paste <dir>            Paste from LAN (latest batch into dir; text/images also to clipboard)
//   --lanclip-send-clipboard         Copy clipboard to LAN (text, image, or copied files)
//   --lanclip-send-text-file <file>  Copy text to LAN (macOS text service; file is deleted after)
//   --lanclip-open                   Open the history window
//   --lanclip-install / --lanclip-uninstall   (Re)install or remove context-menu entries
// A second launch hands its command to the running instance and exits immediately.
const { app, BrowserWindow, Tray, Menu, clipboard, nativeImage, ipcMain, shell, dialog, Notification } = require('electron');
const path = require('path');
const fs = require('fs');
const crypto = require('crypto');
const { Core } = require('./core');
const actions = require('./actions');
const integration = require('./integration');

// ---------- command line ----------
const FLAGS = ['--lanclip-send', '--lanclip-paste', '--lanclip-send-clipboard', '--lanclip-send-text-file',
  '--lanclip-open', '--lanclip-install', '--lanclip-uninstall', '--lanclip-background'];

function parseCommand(argv) {
  const i = argv.findIndex((a) => FLAGS.includes(a));
  if (i < 0) return null;
  return { cmd: argv[i].replace('--lanclip-', ''), args: argv.slice(i + 1).filter((a) => !a.startsWith('--')) };
}

const startCommand = parseCommand(process.argv);

if (startCommand?.cmd === 'uninstall') {
  // Used by uninstallers: remove menu entries without starting the service.
  integration.uninstall().finally(() => process.exit(0));
} else if (!app.requestSingleInstanceLock({ command: startCommand })) {
  app.quit();
  process.exit(0);
}

// Command used by the context menus to reach this app.
function launcher() {
  if (process.env.APPIMAGE) return [process.env.APPIMAGE];
  return app.isPackaged ? [process.execPath] : [process.execPath, app.getAppPath()];
}

let core, win, tray;
let quitting = false;
const hash = (buf) => crypto.createHash('sha1').update(buf).digest('hex');

// ---------- app icon (drawn in code so the repo needs no binary assets) ----------
function makeIcon(size = 32) {
  const buf = Buffer.alloc(size * size * 4);
  const r = size * 0.22;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4;
      const dx = Math.max(r - x, 0, x - (size - 1 - r));
      const dy = Math.max(r - y, 0, y - (size - 1 - r));
      if (dx * dx + dy * dy > r * r) continue;
      const u = x / size, v = y / size;
      const board = u > 0.27 && u < 0.73 && v > 0.24 && v < 0.82;
      const clip = u > 0.39 && u < 0.61 && v > 0.16 && v < 0.3;
      const line = board && u > 0.35 && u < 0.65 && ((v > 0.42 && v < 0.47) || (v > 0.55 && v < 0.6) || (v > 0.68 && v < 0.73));
      let [R, G, B] = [79, 70, 229];
      if ((board || clip) && !line) [R, G, B] = [255, 255, 255];
      buf[i] = B; buf[i + 1] = G; buf[i + 2] = R; buf[i + 3] = 255; // BGRA
    }
  }
  return nativeImage.createFromBitmap(buf, { width: size, height: size });
}

function iconFile() {
  // Explorer menu icon. Packaged Windows builds use the exe's own icon.
  if (app.isPackaged) return process.execPath;
  const file = path.join(app.getPath('userData'), 'lanclip.png');
  try { fs.writeFileSync(file, makeIcon(64).toPNG()); } catch {}
  return file;
}

function notify(title, body) {
  if (!Notification.isSupported()) return;
  new Notification({ title, body, silent: true, icon: makeIcon(64) }).show();
}

const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;
const fmtSize = (n) => n < 1048576 ? `${Math.max(1, Math.round(n / 1024))} KB` : `${(n / 1048576).toFixed(1)} MB`;
function peersNote(peers) {
  return peers ? `to ${plural(peers, 'device')}` : '— no other devices found yet; they will be able to paste it once they connect';
}

// ---------- clipboard ----------
const last = { text: null, image: null };
let applyingRemote = false;

function rememberClipboard() {
  last.text = clipboard.readText();
  const img = clipboard.readImage();
  last.image = img.isEmpty() ? null : hash(img.toBitmap());
}

// Files copied in Explorer / Finder / Linux file managers.
function clipboardFiles() {
  try {
    if (process.platform === 'win32') {
      const buf = clipboard.readBuffer('FileNameW');
      if (buf.length) return buf.toString('utf16le').split('\0').filter(Boolean).filter((p) => fs.existsSync(p));
    } else if (process.platform === 'darwin') {
      const url = clipboard.read('public.file-url');
      if (url) return [decodeURIComponent(new URL(url).pathname)].filter((p) => fs.existsSync(p));
    } else {
      const list = clipboard.read('text/uri-list') || clipboard.read('x-special/gnome-copied-files');
      if (list) {
        return list.split(/\r?\n/).filter((l) => l.startsWith('file://'))
          .map((l) => decodeURIComponent(new URL(l).pathname)).filter((p) => fs.existsSync(p));
      }
    }
  } catch { /* format not available */ }
  return [];
}

async function sendClipboard() {
  const files = clipboardFiles();
  if (files.length) return cmdSend(files);
  const text = clipboard.readText();
  if (text) {
    last.text = text;
    const { peers } = await actions.sendText(core, text);
    return notify('Copied text to LAN', `${text.slice(0, 80)}${text.length > 80 ? '…' : ''}\n${peersNote(peers)}`);
  }
  const img = clipboard.readImage();
  if (!img.isEmpty()) {
    last.image = hash(img.toBitmap());
    await core.addLocal({ kind: 'image', name: `image-${Date.now()}.png`, mime: 'image/png' }, img.toPNG());
    return notify('Copied image to LAN', peersNote(core.activePeers().length));
  }
  notify('Lanclip', 'The clipboard is empty.');
}

// Optional continuous sync (off by default; toggle from the tray).
async function pollClipboard() {
  if (applyingRemote || !core.settings.autoSync) return;
  const text = clipboard.readText();
  if (text && text !== last.text) {
    last.text = text;
    await core.addLocal({ kind: 'text', name: 'text.txt', mime: 'text/plain' }, text);
    return;
  }
  if (!text) {
    const img = clipboard.readImage();
    if (img.isEmpty()) return;
    const h = hash(img.toBitmap());
    if (h !== last.image) {
      last.image = h;
      await core.addLocal({ kind: 'image', name: `image-${Date.now()}.png`, mime: 'image/png' }, img.toPNG());
    }
  }
}

function writeItemToClipboard(item) {
  applyingRemote = true;
  try {
    if (item.kind === 'text') clipboard.writeText(core.readText(item));
    else if (item.kind === 'image') {
      const img = nativeImage.createFromPath(core.contentPath(item));
      if (img.isEmpty()) return false;
      clipboard.writeImage(img);
    } else return false;
    rememberClipboard();
    return true;
  } finally {
    applyingRemote = false;
  }
}

// ---------- commands ----------
// Explorer runs the verb once per selected file, so sends arriving close together become one batch.
let pendingSend = [], sendTimer = null;
function queueSend(paths) {
  pendingSend.push(...paths);
  clearTimeout(sendTimer);
  sendTimer = setTimeout(() => {
    const paths = [...new Set(pendingSend)];
    pendingSend = [];
    cmdSend(paths).catch((e) => notify('Copy to LAN failed', e.message));
  }, 600);
}

async function cmdSend(paths) {
  const res = await actions.sendPaths(core, paths);
  if (!res.files) return notify('Copy to LAN', 'Nothing to send.');
  const what = paths.length === 1 ? path.basename(paths[0]) : plural(res.files, 'file');
  notify('Copied to LAN', `${what} (${fmtSize(res.bytes)}) ${peersNote(res.peers)}`);
}

async function cmdPaste(dir) {
  if (!dir || !fs.existsSync(dir)) dir = app.getPath('downloads');
  if (fs.statSync(dir).isFile()) dir = path.dirname(dir);
  const latest = core.latestBatch();
  if (!latest) return notify('Paste from LAN', 'Nothing has been copied to the LAN yet.');
  if (latest.items.length < latest.total) notify('Paste from LAN', `Still receiving ${latest.items.length}/${latest.total} files…`);

  const { written, batch } = await actions.pasteLatest(core, dir);
  const from = batch.items[0].origin.id === core.settings.id ? 'this device' : batch.items[0].origin.name;
  const single = batch.items.length === 1 ? batch.items[0] : null;
  if (single && (single.kind === 'text' || single.kind === 'image')) writeItemToClipboard(single);

  const missing = batch.total - batch.items.length;
  const body = single?.kind === 'text'
    ? `Text from ${from} is on your clipboard and saved as ${path.basename(written[0])}`
    : `${written.length === 1 ? path.basename(written[0]) : plural(batch.items.length, 'file')} from ${from}` +
      (single?.kind === 'image' ? ' (also on your clipboard)' : '') +
      (missing > 0 ? ` — ${missing} file(s) never arrived` : '');
  notify('Pasted from LAN', body);
}

async function cmdSendTextFile(file) {
  try {
    const text = fs.readFileSync(file, 'utf8');
    if (!text) return;
    const { peers } = await actions.sendText(core, text);
    notify('Copied text to LAN', `${text.slice(0, 80)}${text.length > 80 ? '…' : ''}\n${peersNote(peers)}`);
  } finally {
    fs.rmSync(file, { force: true });
  }
}

async function runCommand(command) {
  if (!command) return;
  const { cmd, args } = command;
  try {
    if (cmd === 'send') queueSend(args);
    else if (cmd === 'paste') await cmdPaste(args[0]);
    else if (cmd === 'send-clipboard') await sendClipboard();
    else if (cmd === 'send-text-file') await cmdSendTextFile(args[0]);
    else if (cmd === 'open') showWindow();
    else if (cmd === 'install') { await installIntegration(); notify('Lanclip', 'Context menu entries installed.'); }
  } catch (e) {
    console.error(e);
    notify('Lanclip error', e.message);
  }
}

// ---------- integration / autostart ----------
async function installIntegration() {
  await integration.install(launcher(), { iconPath: iconFile() });
}

function autostartEnabled() {
  if (process.platform === 'linux') return integration.linuxAutostartEnabled();
  return app.getLoginItemSettings(loginItemOptions()).openAtLogin;
}

function loginItemOptions() {
  const [exe, ...rest] = launcher();
  return { path: exe, args: [...rest, '--lanclip-background'], name: 'Lanclip' };
}

function setAutostart(enabled) {
  if (process.platform === 'linux') return integration.setLinuxAutostart(enabled, launcher());
  app.setLoginItemSettings({ openAtLogin: enabled, ...loginItemOptions() });
}

// ---------- history window (optional, from the tray) ----------
function createWindow() {
  win = new BrowserWindow({
    width: 980, height: 720, minWidth: 380, minHeight: 480,
    title: 'Lanclip', icon: makeIcon(64), backgroundColor: '#0f1115', autoHideMenuBar: true, show: false,
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, sandbox: true },
  });
  win.loadURL(`http://127.0.0.1:${core.port}/?key=${encodeURIComponent(core.settings.key)}&native=1`);
  win.webContents.setWindowOpenHandler(({ url }) => { shell.openExternal(url); return { action: 'deny' }; });
  win.once('ready-to-show', () => win.show());
  win.on('close', (e) => { if (!quitting) { e.preventDefault(); win.hide(); } });
}

function showWindow() {
  if (process.platform === 'darwin') app.dock?.show();
  if (!win || win.isDestroyed()) createWindow(); else { win.show(); win.focus(); }
}

function buildTrayMenu() {
  const peers = core.activePeers();
  tray.setToolTip(`Lanclip — ${peers.length ? plural(peers.length, 'device') : 'no devices'} connected`);
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: peers.length ? `Connected: ${peers.map((p) => p.name).join(', ')}` : 'No other devices found', enabled: false },
    { type: 'separator' },
    { label: 'Copy clipboard to LAN', click: () => runCommand({ cmd: 'send-clipboard', args: [] }) },
    { label: 'Paste from LAN to Downloads', click: () => runCommand({ cmd: 'paste', args: [app.getPath('downloads')] }) },
    { label: 'History & settings…', click: showWindow },
    { type: 'separator' },
    { label: 'Auto-sync clipboard', type: 'checkbox', checked: core.settings.autoSync, click: (m) => core.updateSettings({ autoSync: m.checked }) },
    { label: 'Start at login', type: 'checkbox', checked: autostartEnabled(), click: (m) => { setAutostart(m.checked); buildTrayMenu(); } },
    { label: 'Reinstall context menu', click: () => runCommand({ cmd: 'install', args: [] }) },
    { label: 'Remove context menu', click: async () => { await integration.uninstall(); notify('Lanclip', 'Context menu entries removed.'); } },
    { type: 'separator' },
    { label: 'Quit Lanclip', click: () => { quitting = true; app.quit(); } },
  ]));
}

// ---------- IPC for the history window ----------
function itemOrThrow(id) {
  const item = core.getItem(id);
  if (!item) throw new Error('item not found');
  return item;
}
ipcMain.handle('item:copy', (_e, id) => {
  const item = itemOrThrow(id);
  if (item.kind === 'file') { clipboard.writeText(core.contentPath(item)); return true; }
  return writeItemToClipboard(item);
});
ipcMain.handle('item:open', (_e, id) => shell.openPath(core.contentPath(itemOrThrow(id))));
ipcMain.handle('item:show', (_e, id) => shell.showItemInFolder(core.contentPath(itemOrThrow(id))));
ipcMain.handle('item:saveAs', async (_e, id) => {
  const item = itemOrThrow(id);
  const { canceled, filePath } = await dialog.showSaveDialog(win, { defaultPath: path.join(app.getPath('downloads'), item.name) });
  if (canceled || !filePath) return false;
  await fs.promises.copyFile(core.contentPath(item), filePath);
  return true;
});
ipcMain.on('item:drag', (e, id) => {
  const item = core.getItem(id);
  if (!item) return;
  const file = core.contentPath(item);
  let icon = item.kind === 'image' ? nativeImage.createFromPath(file).resize({ width: 64 }) : makeIcon(48);
  if (icon.isEmpty()) icon = makeIcon(48);
  e.sender.startDrag({ file, icon });
});
ipcMain.handle('clipboard:send', () => sendClipboard());

// ---------- startup ----------
if (process.platform === 'win32') app.setAppUserModelId(app.isPackaged ? 'app.lanclip' : process.execPath);

app.on('second-instance', (_e, argv, _cwd, data) => runCommand(data?.command || parseCommand(argv)));

app.whenReady().then(async () => {
  if (startCommand?.cmd === 'uninstall') return;
  if (process.platform === 'darwin') app.dock?.hide();

  core = new Core({
    dataDir: app.getPath('userData'),
    uiDir: path.join(__dirname, '..', 'ui'),
    port: Number(process.env.LANCLIP_PORT) || 7878,
  });
  core.on('log', (m) => console.log('[lanclip]', m));
  await core.start();
  console.log(`[lanclip] listening on port ${core.port}`);

  // Context menu entries point at this executable, so refresh them on every start
  // (the app may have moved or been updated). First run also enables start-at-login.
  const firstRun = !fs.existsSync(path.join(app.getPath('userData'), '.installed'));
  try {
    await installIntegration();
    if (firstRun) {
      setAutostart(true);
      fs.writeFileSync(path.join(app.getPath('userData'), '.installed'), new Date().toISOString());
    }
  } catch (e) {
    console.error('[lanclip] context menu install failed:', e.message);
  }

  core.on('incoming', (item) => {
    if (core.settings.autoSync && (item.kind === 'text' || item.kind === 'image')) writeItemToClipboard(item);
  });
  core.on('send-clipboard', () => sendClipboard());
  core.on('settings', () => tray && buildTrayMenu());
  core.on('changed', () => tray && buildTrayMenu());

  rememberClipboard();
  setInterval(() => pollClipboard().catch((e) => console.error(e)), 800);

  tray = new Tray(makeIcon(process.platform === 'darwin' ? 18 : 32));
  tray.on('click', () => tray.popUpContextMenu());
  buildTrayMenu();

  if (firstRun && !startCommand) {
    const where = { win32: 'Explorer (Windows 11: "Show more options")', darwin: 'Finder → Quick Actions', linux: 'your file manager' }[process.platform];
    notify('Lanclip is running', `Right-click files → "Copy to LAN", right-click in a folder → "Paste from LAN" in ${where}.`);
  }
  runCommand(startCommand?.cmd === 'background' ? null : startCommand);
});

app.on('before-quit', () => { quitting = true; core?.stop(); });
app.on('window-all-closed', () => { /* keep running in the background */ });
