// "Copy to LAN" / "Paste from LAN" logic, independent of Electron so it can be tested with plain Node.
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { safeName, safeRelPath } = require('./core');

const IMAGE_EXT = { '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.bmp': 'image/bmp' };
const MAX_FILES = 10000;

// Expand the selection into files. Folders keep their structure via relPath ("Folder/sub/file").
function collectFiles(paths) {
  const out = [];
  const walk = (abs, rel) => {
    if (out.length >= MAX_FILES) return;
    let st;
    try { st = fs.statSync(abs); } catch { return; }
    if (st.isDirectory()) {
      for (const name of fs.readdirSync(abs)) walk(path.join(abs, name), rel ? `${rel}/${name}` : name);
    } else if (st.isFile()) {
      out.push({ file: abs, name: path.basename(abs), relPath: rel.includes('/') ? rel : undefined, size: st.size });
    }
  };
  for (const p of paths) {
    const abs = path.resolve(p);
    walk(abs, path.basename(abs));
  }
  return out;
}

async function sendPaths(core, paths) {
  const files = collectFiles(paths);
  if (!files.length) return { files: 0, peers: core.activePeers().length };
  const batch = crypto.randomUUID();
  for (const f of files) {
    const mime = IMAGE_EXT[path.extname(f.name).toLowerCase()];
    await core.addLocal({
      kind: mime && !f.relPath ? 'image' : 'file',
      name: f.name, mime: mime || 'application/octet-stream',
      batch, batchTotal: files.length, relPath: f.relPath,
    }, { file: f.file });
  }
  return { files: files.length, bytes: files.reduce((n, f) => n + f.size, 0), peers: core.activePeers().length };
}

async function sendText(core, text) {
  await core.addLocal({ kind: 'text', name: 'text.txt', mime: 'text/plain' }, text);
  return { peers: core.activePeers().length };
}

// Wait until every item of the latest batch has arrived (or it stops making progress).
async function waitForLatestBatch(core, { idleMs = 15000, maxMs = 10 * 60 * 1000 } = {}) {
  const start = Date.now();
  let batch = core.latestBatch(), lastCount = -1, lastProgress = Date.now();
  while (batch && batch.items.length < batch.total) {
    if (batch.items.length !== lastCount) { lastCount = batch.items.length; lastProgress = Date.now(); }
    if (Date.now() - lastProgress > idleMs || Date.now() - start > maxMs) break;
    await new Promise((r) => setTimeout(r, 200));
    batch = core.latestBatch();
  }
  return batch;
}

function uniquePath(dir, rel) {
  const target = path.join(dir, ...rel.split('/'));
  if (!fs.existsSync(target)) return target;
  const ext = path.extname(target), base = target.slice(0, target.length - ext.length);
  for (let i = 2; ; i++) {
    const candidate = `${base} (${i})${ext}`;
    if (!fs.existsSync(candidate)) return candidate;
  }
}

function stamp(ts) {
  const d = new Date(ts), p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}.${p(d.getMinutes())}.${p(d.getSeconds())}`;
}

// Paste the latest batch into `dir`. Returns what was written plus the batch (so the caller can
// also put text/images on the clipboard).
async function pasteLatest(core, dir, opts) {
  const batch = await waitForLatestBatch(core, opts);
  if (!batch) return { written: [], batch: null };
  fs.mkdirSync(dir, { recursive: true });

  // A pasted folder whose top-level name already exists gets renamed as a whole ("Photos (2)/...").
  const topRename = new Map();
  const written = [];
  for (const item of batch.items) {
    let rel = item.kind === 'text' ? `LAN text ${stamp(item.ts)}.txt` : (safeRelPath(item.relPath) || safeName(item.name));
    const [top, ...rest] = rel.split('/');
    if (rest.length) {
      if (!topRename.has(top)) topRename.set(top, path.basename(uniquePath(dir, top)));
      rel = [topRename.get(top), ...rest].join('/');
      const target = path.join(dir, ...rel.split('/'));
      fs.mkdirSync(path.dirname(target), { recursive: true });
      await fs.promises.copyFile(core.contentPath(item), target);
      if (!written.includes(path.join(dir, topRename.get(top)))) written.push(path.join(dir, topRename.get(top)));
    } else {
      const target = uniquePath(dir, rel);
      await fs.promises.copyFile(core.contentPath(item), target);
      written.push(target);
    }
  }
  return { written, batch };
}

module.exports = { collectFiles, sendPaths, sendText, pasteLatest, waitForLatestBatch };
