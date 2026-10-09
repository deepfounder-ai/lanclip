// Lanclip core: item store, HTTP/WebSocket server, LAN discovery and peer relay.
// Has no Electron dependency so it can also run headless (see headless.js).
const http = require('http');
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const dgram = require('dgram');
const { EventEmitter } = require('events');
const { pipeline } = require('stream/promises');
const { WebSocketServer } = require('ws');
const QRCode = require('qrcode');

const APP_TAG = 'lanclip/1';
const DISCOVERY_PORT = 41234;
const ANNOUNCE_MS = 3000;
const PEER_TTL_MS = 10000;
const MAX_INLINE_TEXT = 256 * 1024;

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png',
  '.json': 'application/json', '.ico': 'image/x-icon',
};

function randomKey() {
  const alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  const bytes = crypto.randomBytes(10);
  return Array.from(bytes, (b) => alphabet[b % alphabet.length]).join('');
}

function safeName(name) {
  const cleaned = String(name || 'file').replace(/[\\/:*?"<>|\x00-\x1f]/g, '_').trim().slice(0, 180);
  return cleaned && cleaned !== '.' && cleaned !== '..' ? cleaned : 'file';
}

// "folder/sub/file.txt" from a peer -> safe relative path (no "..", no absolute parts), or ''.
function safeRelPath(p) {
  if (typeof p !== 'string' || !p) return '';
  const parts = p.split(/[\\/]+/).filter((s) => s && s !== '.' && s !== '..').map(safeName);
  return parts.slice(0, 32).join('/');
}

function isLoopback(addr) {
  return addr === '127.0.0.1' || addr === '::1' || addr === '::ffff:127.0.0.1';
}

function lanAddresses() {
  const out = [];
  for (const list of Object.values(os.networkInterfaces())) {
    for (const a of list || []) {
      if (a.family === 'IPv4' && !a.internal) out.push({ address: a.address, netmask: a.netmask });
    }
  }
  return out;
}

function broadcastAddress({ address, netmask }) {
  const ip = address.split('.').map(Number);
  const mask = netmask.split('.').map(Number);
  return ip.map((o, i) => (o | (~mask[i] & 255))).join('.');
}

function safeEqual(a, b) {
  const x = Buffer.from(String(a)), y = Buffer.from(String(b));
  return x.length === y.length && crypto.timingSafeEqual(x, y);
}

class Core extends EventEmitter {
  constructor({ dataDir, uiDir, port = 7878, discoveryPort = DISCOVERY_PORT, deviceName }) {
    super();
    this.dataDir = dataDir;
    this.uiDir = uiDir;
    this.preferredPort = port;
    this.discoveryPort = discoveryPort;
    this.itemsDir = path.join(dataDir, 'items');
    fs.mkdirSync(this.itemsDir, { recursive: true });
    this.settings = this._loadSettings(deviceName);
    this.items = this._loadJSON('items.json', []).filter((it) => fs.existsSync(this.contentPath(it)));
    this.peers = new Map(); // id -> { id, name, host, port, lastSeen, source, sameKey }
    this.sendQueues = new Map(); // peer id -> promise chain
    this.port = null;
  }

  // ---------- persistence ----------
  _loadJSON(file, fallback) {
    try { return JSON.parse(fs.readFileSync(path.join(this.dataDir, file), 'utf8')); } catch { return fallback; }
  }
  _saveJSON(file, value) {
    const p = path.join(this.dataDir, file);
    fs.writeFileSync(p + '.tmp', JSON.stringify(value, null, 2));
    fs.renameSync(p + '.tmp', p);
  }
  _loadSettings(deviceName) {
    const s = this._loadJSON('settings.json', {});
    const settings = {
      id: s.id || crypto.randomUUID(),
      name: s.name || deviceName || os.hostname(),
      key: s.key || randomKey(),
      autoSync: s.autoSync === true, // explicit copy/paste via context menu is the default
      manualPeers: Array.isArray(s.manualPeers) ? s.manualPeers : [],
      maxItems: Math.max(Number(s.maxItems) || 0, 500),
    };
    this._saveJSON('settings.json', settings);
    return settings;
  }
  updateSettings(patch) {
    const s = this.settings;
    if (typeof patch.name === 'string' && patch.name.trim()) s.name = patch.name.trim().slice(0, 60);
    if (typeof patch.key === 'string' && patch.key.trim().length >= 4) s.key = patch.key.trim();
    if (typeof patch.autoSync === 'boolean') s.autoSync = patch.autoSync;
    if (Array.isArray(patch.manualPeers)) {
      s.manualPeers = patch.manualPeers.map((p) => String(p).trim()).filter(Boolean).slice(0, 50);
    }
    this._saveJSON('settings.json', s);
    if (patch.key !== undefined) {
      this.peers.clear();
      for (const ws of this.wss?.clients || []) ws.close(4001, 'key changed');
    }
    this._announce();
    this._probeManualPeers();
    this._changed();
    this.emit('settings', s);
  }
  _saveItems() { this._saveJSON('items.json', this.items); }

  contentPath(item) { return path.join(this.itemsDir, item.id, safeName(item.name)); }
  getItem(id) { return this.items.find((it) => it.id === id); }

  // ---------- items ----------
  // Store an item from a readable stream. Returns the stored item (or the existing one for duplicates).
  async ingest(meta, readable) {
    const id = /^[\w-]{8,64}$/.test(meta.id || '') ? meta.id : crypto.randomUUID();
    const existing = this.getItem(id);
    if (existing) { readable.resume(); return { item: existing, duplicate: true }; }

    const kind = ['text', 'image', 'file'].includes(meta.kind) ? meta.kind : 'file';
    const item = {
      id, kind,
      name: safeName(meta.name || (kind === 'text' ? 'text.txt' : kind === 'image' ? 'image.png' : 'file')),
      mime: String(meta.mime || (kind === 'text' ? 'text/plain' : 'application/octet-stream')).slice(0, 100),
      size: 0,
      ts: Number(meta.ts) || Date.now(),
      origin: { id: String(meta.origin?.id || 'unknown').slice(0, 64), name: String(meta.origin?.name || 'Unknown').slice(0, 60) },
      // Items copied together (one "Copy to LAN") share a batch and are pasted together.
      batch: /^[\w-]{8,64}$/.test(meta.batch || '') ? meta.batch : id,
      batchTotal: Math.max(1, Math.min(100000, Number(meta.batchTotal) || 1)),
    };
    const rel = safeRelPath(meta.relPath);
    if (rel) item.relPath = rel;
    const dir = path.join(this.itemsDir, id);
    fs.mkdirSync(dir, { recursive: true });
    const file = this.contentPath(item);
    let size = 0;
    readable.on('data', (c) => { size += c.length; });
    try {
      await pipeline(readable, fs.createWriteStream(file));
    } catch (err) {
      fs.rmSync(dir, { recursive: true, force: true });
      throw err;
    }
    item.size = size;
    if (kind === 'text') {
      const buf = fs.readFileSync(file);
      item.text = buf.subarray(0, MAX_INLINE_TEXT).toString('utf8');
      item.truncated = size > MAX_INLINE_TEXT;
    }
    this.items.unshift(item);
    this._prune();
    this._saveItems();
    this._changed();
    return { item, duplicate: false };
  }

  // Convenience for local producers (clipboard watcher, Electron). data: Buffer | string
  // data: Buffer | string, or { file } to stream from disk.
  async addLocal({ kind, name, mime, batch, batchTotal, relPath }, data) {
    const { Readable } = require('stream');
    const stream = data && data.file
      ? fs.createReadStream(data.file)
      : Readable.from([Buffer.isBuffer(data) ? data : Buffer.from(String(data), 'utf8')]);
    const { item } = await this.ingest(
      { kind, name, mime, batch, batchTotal, relPath, origin: { id: this.settings.id, name: this.settings.name } },
      stream,
    );
    this.relay(item);
    return item;
  }

  // The most recent batch: what "Paste from LAN" pastes.
  latestBatch() {
    const newest = this.items[0];
    if (!newest) return null;
    const items = this.items.filter((it) => it.batch === newest.batch).reverse();
    return { id: newest.batch, total: newest.batchTotal || 1, items };
  }

  readText(item) { return fs.readFileSync(this.contentPath(item), 'utf8'); }

  deleteItem(id) {
    const idx = this.items.findIndex((it) => it.id === id);
    if (idx < 0) return false;
    const [item] = this.items.splice(idx, 1);
    fs.rmSync(path.join(this.itemsDir, item.id), { recursive: true, force: true });
    this._saveItems();
    this._changed();
    return true;
  }

  clearItems() {
    for (const it of this.items) fs.rmSync(path.join(this.itemsDir, it.id), { recursive: true, force: true });
    this.items = [];
    this._saveItems();
    this._changed();
  }

  _prune() {
    const keep = this.items[0]?.batch; // never prune the batch that is still arriving / pasteable
    for (let i = this.items.length - 1; i >= 0 && this.items.length > this.settings.maxItems; i--) {
      if (this.items[i].batch === keep) continue;
      const [old] = this.items.splice(i, 1);
      fs.rmSync(path.join(this.itemsDir, old.id), { recursive: true, force: true });
    }
  }

  // Push an item to every reachable peer that shares our key. Sends to each peer are queued
  // so a big folder doesn't open hundreds of sockets and items arrive in order.
  relay(item) {
    let count = 0;
    for (const peer of this.peers.values()) {
      if (!peer.sameKey || peer.id === item.origin.id) continue;
      count++;
      const prev = this.sendQueues.get(peer.id) || Promise.resolve();
      const next = prev.then(() => this._sendToPeer(this.peers.get(peer.id) || peer, item)).catch((err) => {
        this.emit('log', `send to ${peer.name} failed: ${err.message}`);
      });
      this.sendQueues.set(peer.id, next);
    }
    return count;
  }

  activePeers() { return [...this.peers.values()].filter((p) => p.sameKey); }

  _sendToPeer(peer, item) {
    return new Promise((resolve, reject) => {
      const { text, truncated, ...meta } = item;
      const req = http.request({
        host: peer.host, port: peer.port, path: '/api/items', method: 'POST',
        headers: {
          'content-type': 'application/octet-stream',
          'content-length': item.size,
          'x-lanclip-key': this.settings.key,
          'x-lanclip-meta': encodeURIComponent(JSON.stringify(meta)),
          'x-lanclip-peer': this.settings.id,
        },
      }, (res) => {
        res.resume();
        res.statusCode < 300 ? resolve() : reject(new Error(`HTTP ${res.statusCode}`));
      });
      req.setTimeout(30000, () => req.destroy(new Error('timeout')));
      req.on('error', reject);
      fs.createReadStream(this.contentPath(item)).on('error', reject).pipe(req);
    });
  }

  // ---------- state ----------
  peerList() {
    return [...this.peers.values()]
      .map(({ id, name, host, port, source, sameKey }) => ({ id, name, host, port, source, sameKey }))
      .sort((a, b) => a.name.localeCompare(b.name));
  }

  state(isLocal) {
    const s = this.settings;
    return {
      self: { id: s.id, name: s.name, port: this.port, addresses: lanAddresses().map((a) => a.address) },
      peers: this.peerList(),
      items: this.items,
      isLocal,
      settings: isLocal ? { name: s.name, key: s.key, autoSync: s.autoSync, manualPeers: s.manualPeers } : undefined,
    };
  }

  phoneUrls() {
    return lanAddresses().map((a) => `http://${a.address}:${this.port}/?key=${encodeURIComponent(this.settings.key)}`);
  }

  _changed() {
    clearTimeout(this._changeTimer);
    this._changeTimer = setTimeout(() => {
      const msg = JSON.stringify({ type: 'changed' });
      for (const ws of this.wss?.clients || []) if (ws.readyState === 1) ws.send(msg);
      this.emit('changed');
    }, 50);
  }

  // ---------- HTTP server ----------
  _authorized(req, url) {
    const key = req.headers['x-lanclip-key'] || url.searchParams.get('key') || '';
    return safeEqual(key, this.settings.key);
  }

  async _handle(req, res) {
    const url = new URL(req.url, 'http://x');
    const local = isLoopback(req.socket.remoteAddress);
    const send = (code, body, type = 'application/json') => {
      res.writeHead(code, { 'content-type': type, 'cache-control': 'no-store' });
      res.end(type === 'application/json' ? JSON.stringify(body) : body);
    };

    if (!url.pathname.startsWith('/api/')) return this._static(url.pathname, res);

    if (url.pathname === '/api/hello') {
      return send(200, { app: APP_TAG, id: this.settings.id, name: this.settings.name, port: this.port, authorized: this._authorized(req, url) });
    }
    if (!this._authorized(req, url)) return send(401, { error: 'bad key' });

    if (url.pathname === '/api/state' && req.method === 'GET') return send(200, this.state(local));

    if (url.pathname === '/api/items' && req.method === 'POST') {
      let meta = {};
      try { meta = JSON.parse(decodeURIComponent(req.headers['x-lanclip-meta'] || '%7B%7D')); } catch { return send(400, { error: 'bad meta' }); }
      const fromPeer = req.headers['x-lanclip-peer'];
      if (!fromPeer) {
        // Posted by a UI: the local window, or a phone/browser on the LAN.
        meta.id = undefined;
        meta.origin = local
          ? { id: this.settings.id, name: this.settings.name }
          : { id: 'web:' + req.socket.remoteAddress, name: String(meta.originName || 'Browser').slice(0, 40) + ' via ' + this.settings.name };
      }
      const { item, duplicate } = await this.ingest(meta, req);
      if (!duplicate) {
        if (!fromPeer) this.relay(item);
        if (fromPeer || !local) this.emit('incoming', item);
      }
      return send(200, item);
    }

    const m = url.pathname.match(/^\/api\/items\/([\w-]+)(\/content)?$/);
    if (m) {
      const item = this.getItem(m[1]);
      if (!item) return send(404, { error: 'not found' });
      if (m[2] && req.method === 'GET') {
        const download = url.searchParams.has('download');
        res.writeHead(200, {
          'content-type': item.kind === 'text' ? 'text/plain; charset=utf-8' : item.mime,
          'content-length': item.size,
          'content-disposition': `${download ? 'attachment' : 'inline'}; filename*=UTF-8''${encodeURIComponent(item.name)}`,
          'cache-control': 'private, max-age=86400',
        });
        return fs.createReadStream(this.contentPath(item)).pipe(res);
      }
      if (!m[2] && req.method === 'DELETE') return send(200, { ok: this.deleteItem(item.id) });
    }
    if (url.pathname === '/api/items' && req.method === 'DELETE') { this.clearItems(); return send(200, { ok: true }); }

    if (url.pathname === '/api/send-clipboard' && req.method === 'POST') {
      this.emit('send-clipboard');
      return send(200, { ok: true });
    }

    if (url.pathname === '/api/settings' && req.method === 'PUT') {
      if (!local) return send(403, { error: 'settings can only be changed on the device itself' });
      const body = await readBody(req, 64 * 1024);
      try { this.updateSettings(JSON.parse(body)); } catch { return send(400, { error: 'bad json' }); }
      return send(200, this.state(true));
    }

    if (url.pathname === '/api/qr' && req.method === 'GET') {
      const target = url.searchParams.get('url') || this.phoneUrls()[0] || '';
      const svg = await QRCode.toString(target, { type: 'svg', margin: 1, errorCorrectionLevel: 'M' });
      return send(200, svg, 'image/svg+xml');
    }

    send(404, { error: 'not found' });
  }

  _static(pathname, res) {
    const rel = pathname === '/' ? 'index.html' : pathname.replace(/^\/+/, '');
    const file = path.resolve(this.uiDir, rel);
    if (!file.startsWith(path.resolve(this.uiDir) + path.sep)) { res.writeHead(403); return res.end(); }
    fs.readFile(file, (err, data) => {
      if (err) { res.writeHead(404); return res.end('not found'); }
      res.writeHead(200, { 'content-type': MIME[path.extname(file)] || 'application/octet-stream', 'cache-control': 'no-cache' });
      res.end(data);
    });
  }

  // ---------- discovery ----------
  _keyTag() {
    return crypto.createHmac('sha256', this.settings.key).update('lanclip-discovery').digest('hex').slice(0, 16);
  }

  _announce() {
    if (!this.udp) return;
    const msg = Buffer.from(JSON.stringify({ app: APP_TAG, id: this.settings.id, name: this.settings.name, port: this.port, tag: this._keyTag() }));
    const targets = new Set(['255.255.255.255', ...lanAddresses().map(broadcastAddress)]);
    for (const t of targets) this.udp.send(msg, this.discoveryPort, t, () => {});
  }

  _onAnnounce(buf, rinfo) {
    let m;
    try { m = JSON.parse(buf.toString('utf8')); } catch { return; }
    if (m.app !== APP_TAG || !m.id || m.id === this.settings.id) return;
    const known = this.peers.get(m.id);
    const peer = {
      id: String(m.id), name: String(m.name || rinfo.address).slice(0, 60), host: rinfo.address,
      port: Number(m.port) || 7878, lastSeen: Date.now(), source: 'lan', sameKey: m.tag === this._keyTag(),
    };
    this.peers.set(peer.id, peer);
    if (!known) {
      this._announce(); // let the newcomer learn about us immediately
      this._changed();
    } else if (known.name !== peer.name || known.host !== peer.host || known.sameKey !== peer.sameKey) {
      this._changed();
    }
  }

  async _probeManualPeers() {
    for (const addr of this.settings.manualPeers) {
      const [host, p] = addr.split(':');
      const port = Number(p) || 7878;
      try {
        const hello = await getJSON({ host, port, path: '/api/hello', headers: { 'x-lanclip-key': this.settings.key } });
        if (hello.app !== APP_TAG || hello.id === this.settings.id) continue;
        const known = this.peers.get(hello.id);
        this.peers.set(hello.id, { id: hello.id, name: hello.name, host, port, lastSeen: Date.now(), source: 'manual', sameKey: !!hello.authorized });
        if (!known || known.sameKey !== !!hello.authorized) this._changed();
      } catch { /* unreachable */ }
    }
  }

  _expirePeers() {
    let changed = false;
    for (const [id, p] of this.peers) {
      if (Date.now() - p.lastSeen > PEER_TTL_MS) { this.peers.delete(id); changed = true; }
    }
    if (changed) this._changed();
  }

  // ---------- lifecycle ----------
  async start() {
    this.server = http.createServer((req, res) => {
      this._handle(req, res).catch((err) => {
        this.emit('log', `request error: ${err.message}`);
        if (!res.headersSent) res.writeHead(500, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: err.message }));
      });
    });
    this.server.requestTimeout = 0; // allow big uploads

    this.port = await listenWithFallback(this.server, this.preferredPort);

    this.wss = new WebSocketServer({ noServer: true });
    this.server.on('upgrade', (req, socket, head) => {
      const url = new URL(req.url, 'http://x');
      if (url.pathname !== '/ws' || !this._authorized(req, url)) { socket.destroy(); return; }
      this.wss.handleUpgrade(req, socket, head, (ws) => ws.send(JSON.stringify({ type: 'changed' })));
    });

    this.udp = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    this.udp.on('message', (buf, rinfo) => this._onAnnounce(buf, rinfo));
    this.udp.on('error', (err) => this.emit('log', `discovery error: ${err.message}`));
    await new Promise((resolve) => this.udp.bind(this.discoveryPort, () => { this.udp.setBroadcast(true); resolve(); }));

    this._timers = [
      setInterval(() => this._announce(), ANNOUNCE_MS),
      setInterval(() => this._probeManualPeers(), ANNOUNCE_MS),
      setInterval(() => this._expirePeers(), 2000),
    ];
    this._announce();
    this._probeManualPeers();
    return this.port;
  }

  stop() {
    for (const t of this._timers || []) clearInterval(t);
    try { this.udp?.close(); } catch {}
    for (const ws of this.wss?.clients || []) ws.terminate();
    this.server?.close();
  }
}

function listenWithFallback(server, port, attempts = 20) {
  return new Promise((resolve, reject) => {
    const tryPort = (p, left) => {
      server.once('error', (err) => {
        if (err.code === 'EADDRINUSE' && left > 0) tryPort(p + 1, left - 1); else reject(err);
      });
      server.listen(p, '0.0.0.0', () => resolve(server.address().port));
    };
    tryPort(port, attempts);
  });
}

function readBody(req, limit) {
  return new Promise((resolve, reject) => {
    let size = 0; const chunks = [];
    req.on('data', (c) => { size += c.length; if (size > limit) { req.destroy(); reject(new Error('body too large')); } else chunks.push(c); });
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

function getJSON(opts) {
  return new Promise((resolve, reject) => {
    const req = http.get({ ...opts, timeout: 2000 }, (res) => {
      let body = '';
      res.on('data', (c) => { body += c; });
      res.on('end', () => { try { resolve(JSON.parse(body)); } catch (e) { reject(e); } });
    });
    req.on('timeout', () => req.destroy(new Error('timeout')));
    req.on('error', reject);
  });
}

module.exports = { Core, safeName, safeRelPath };
