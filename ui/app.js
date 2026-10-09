// Web UI served by the Lanclip service: history + settings on the computer itself, and the phone/browser client.
(() => {
  const $ = (id) => document.getElementById(id);
  const store = {
    get(k) { try { return localStorage.getItem(k); } catch { return null; } },
    set(k, v) { try { localStorage.setItem(k, v); } catch {} },
  };

  // ----- key handling: take it from the link, remember it, then hide it from the address bar -----
  const params = new URLSearchParams(location.search);
  let key = params.get('key') || store.get('lanclip.key') || '';
  if (params.get('key')) {
    store.set('lanclip.key', key);
    history.replaceState(null, '', location.pathname);
  }

  let state = null;
  let filterText = '';

  const api = (path, opts = {}) =>
    fetch(path, { ...opts, headers: { 'x-lanclip-key': key, ...(opts.headers || {}) } });
  const contentUrl = (item, download) =>
    `/api/items/${item.id}/content?key=${encodeURIComponent(key)}${download ? '&download=1' : ''}`;

  function toast(msg) {
    const t = $('toast');
    t.textContent = msg;
    t.classList.add('show');
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => t.classList.remove('show'), 2200);
  }

  function deviceName() {
    const ua = navigator.userAgent;
    if (/iPhone/.test(ua)) return 'iPhone';
    if (/iPad/.test(ua)) return 'iPad';
    if (/Android/.test(ua)) return 'Android';
    if (/Mac/.test(ua)) return 'Mac browser';
    if (/Windows/.test(ua)) return 'Windows browser';
    return 'Browser';
  }

  // ----- state & live updates -----
  async function refresh() {
    const res = await api('/api/state');
    if (res.status === 401) return askForKey();
    state = await res.json();
    render();
  }

  function askForKey() {
    setStatus(false, 'wrong key');
    if (askForKey.askedFor === key && key) return; // don't nag on every reconnect
    askForKey.askedFor = key;
    const k = prompt('This link is missing its access key. Scan the QR code in Lanclip → Settings on your computer, or paste the key:');
    if (k) { key = k.trim(); store.set('lanclip.key', key); connect(); }
  }

  // Live updates by long-polling /api/wait (works on every browser, no sockets needed).
  let pollGen = 0;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  async function connect() {
    const gen = ++pollGen;
    let version = 0;
    while (gen === pollGen) {
      try {
        const res = await api(`/api/wait?since=${version}`);
        if (res.status === 401) { askForKey(); await sleep(3000); continue; }
        const { version: v } = await res.json();
        setStatus(true);
        if (v !== version) { version = v; await refresh(); }
      } catch {
        setStatus(false);
        await sleep(2000);
      }
    }
  }

  function setStatus(on, label) {
    $('status').classList.toggle('on', on);
    $('status').querySelector('span').textContent = label || (on ? 'connected' : 'offline');
  }

  // ----- rendering -----
  const fmtSize = (n) => n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : n < 1073741824 ? `${(n / 1048576).toFixed(1)} MB` : `${(n / 1073741824).toFixed(2)} GB`;
  function fmtTime(ts) {
    const d = new Date(ts), now = new Date();
    const time = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    return d.toDateString() === now.toDateString() ? time : `${d.toLocaleDateString()} ${time}`;
  }
  function el(tag, props = {}, ...children) {
    const e = document.createElement(tag);
    for (const [k, v] of Object.entries(props)) {
      if (k === 'class') e.className = v;
      else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
      else if (v !== undefined && v !== null && v !== false) e.setAttribute(k, v === true ? '' : v);
    }
    for (const c of children) if (c != null) e.append(c);
    return e;
  }

  function render() {
    const s = state;
    $('selfName').textContent = s.isLocal ? `This device: ${s.self.name}` : `Connected to ${s.self.name}`;
    $('settingsBtn').hidden = !s.isLocal;
    $('clipBtn').hidden = !s.isLocal;

    renderDevices();

    const q = filterText.toLowerCase();
    const items = s.items.filter((it) => !q || it.name.toLowerCase().includes(q) || (it.text || '').toLowerCase().includes(q) || it.origin.name.toLowerCase().includes(q));
    $('items').replaceChildren(...items.map(renderItem));
    $('empty').hidden = s.items.length > 0;
  }

  // Devices: paired ones first, then others on the network with a Connect button.
  function renderDevices() {
    const s = state, box = $('peers');
    box.replaceChildren();
    if (!s.isLocal) {
      const n = s.peers.filter((p) => p.paired && p.online).length;
      box.append(el('p', { class: 'none' }, `Items you send go to ${s.self.name}` + (n ? ` and ${n} connected device${n > 1 ? 's' : ''}.` : '.')));
      return;
    }
    box.append(el('h2', {}, 'Devices'));
    if (!s.peers.length) {
      box.append(el('p', { class: 'none' }, 'Looking for other devices… Install and open Lanclip on your other computers (same Wi‑Fi/network). They will show up here.'));
      return;
    }
    const call = async (path, body, done) => {
      const res = await api(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
      const j = await res.json().catch(() => ({}));
      if (!res.ok) toast(j.error || 'Something went wrong'); else if (done) toast(done);
    };
    const list = el('ul', { class: 'devices' });
    for (const p of s.peers) {
      let status, actions = [];
      if (p.pending === 'incoming') {
        status = `wants to connect · code ${p.code}`;
        actions = [
          el('button', { class: 'small primary', type: 'button', onclick: () => call('/api/local/pair-respond', { id: p.id, accept: true }, `Connected to ${p.name}`) }, 'Accept'),
          el('button', { class: 'small ghost', type: 'button', onclick: () => call('/api/local/pair-respond', { id: p.id, accept: false }) }, 'Decline'),
        ];
      } else if (p.pending === 'outgoing') {
        status = `waiting for ${p.name} to accept · code ${p.code}`;
      } else if (p.paired) {
        status = p.online ? 'connected' : 'offline';
        actions = [el('button', { class: 'small ghost', type: 'button', onclick: () => {
          if (confirm(`Disconnect from ${p.name}? You can connect again any time.`)) call('/api/local/unpair', { id: p.id });
        } }, 'Disconnect')];
      } else {
        status = 'not connected';
        actions = [el('button', { class: 'small primary', type: 'button', onclick: () => call('/api/local/pair', { id: p.id }) }, 'Connect')];
      }
      const dot = p.paired ? (p.online ? 'on' : 'off') : p.pending ? 'wait' : 'new';
      list.append(el('li', { class: 'device' },
        el('i', { class: `dot ${dot}` }),
        el('div', { class: 'dev-text' }, el('strong', {}, p.name), el('span', {}, status)),
        el('div', { class: 'dev-actions' }, ...actions)));
    }
    box.append(list);
  }

  function renderItem(it) {
    const fromSelf = it.origin.id === state.self.id;
    const meta = el('div', { class: 'meta' },
      el('span', { class: 'kind' }, it.kind),
      el('span', { class: 'from' }, fromSelf ? 'This device' : it.origin.name),
      el('span', {}, fmtTime(it.ts)),
      it.kind !== 'text' ? el('span', {}, fmtSize(it.size)) : null,
    );

    let body;
    if (it.kind === 'text') {
      body = el('pre', { class: 'text' }, it.text + (it.truncated ? '\n…' : ''));
    } else if (it.kind === 'image') {
      body = el('img', { class: 'thumb', src: contentUrl(it), alt: it.name, loading: 'lazy' });
    } else {
      const ext = (it.name.split('.').pop() || '').slice(0, 4).toUpperCase();
      body = el('div', { class: 'file' }, el('span', { class: 'ext' }, ext || 'FILE'), el('span', { class: 'name', title: it.name }, it.name));
    }

    const actions = el('div', { class: 'actions' });
    const btn = (label, fn, cls) => el('button', { class: 'small ' + (cls || ''), type: 'button', onclick: fn }, label);
    const onDevice = state.isLocal;
    if (it.kind === 'text') actions.append(btn('Copy', () => copyText(it)));
    if (it.kind === 'image' && (onDevice || window.ClipboardItem)) actions.append(btn('Copy', () => copyImage(it)));
    if (onDevice) {
      if (it.kind !== 'text') actions.append(btn('Open', () => deviceAction(it, 'open')));
      actions.append(btn('Show in folder', () => deviceAction(it, 'reveal')));
    } else {
      actions.append(el('a', { class: 'btn ghost', href: contentUrl(it, true), download: it.name }, 'Download'));
      if (it.kind !== 'text') actions.append(el('a', { class: 'btn ghost', href: contentUrl(it), target: '_blank', rel: 'noopener' }, 'Open'));
    }
    actions.append(btn('Delete', () => api(`/api/items/${it.id}`, { method: 'DELETE' }), 'ghost del'));

    const li = el('li', { class: 'item', draggable: 'true' }, meta, body, actions);
    li.addEventListener('dragstart', (e) => {
      const url = new URL(contentUrl(it, true), location.href).href;
      e.dataTransfer.setData('DownloadURL', `${it.mime}:${it.name}:${url}`);
      e.dataTransfer.setData('text/uri-list', url);
      if (it.kind === 'text') e.dataTransfer.setData('text/plain', it.text);
      internalDrag = true;
    });
    li.addEventListener('dragend', () => { internalDrag = false; });
    return li;
  }

  // ----- copy helpers -----
  // Actions the Lanclip service performs on this computer (only offered on the device itself).
  const deviceAction = (it, action) => api(`/api/items/${it.id}/${action}`, { method: 'POST' });

  async function copyText(it) {
    if (state.isLocal) { await deviceAction(it, 'copy'); return toast('Copied'); }
    let text = it.text;
    if (it.truncated) text = await (await fetch(contentUrl(it))).text();
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // Plain-http LAN pages are not a "secure context", so fall back to the legacy API.
      const ta = el('textarea', { style: 'position:fixed;opacity:0' });
      ta.value = text;
      document.body.append(ta);
      ta.select();
      const ok = document.execCommand('copy');
      ta.remove();
      if (!ok) return toast('Copy blocked by the browser — long-press the text instead');
    }
    toast('Copied');
  }

  async function copyImage(it) {
    if (state.isLocal && it.mime === 'image/png') { await deviceAction(it, 'copy'); return toast('Image copied'); }
    try {
      const blob = await (await fetch(contentUrl(it))).blob();
      await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
      toast('Image copied');
    } catch {
      toast('This browser can’t copy images here — use Download');
    }
  }

  // ----- sending -----
  function upload(kind, name, mime, body) {
    return new Promise((resolve, reject) => {
      const row = el('div', { class: 'upload' }, `Sending ${name}…`, el('progress', { max: 1, value: 0 }));
      if (kind !== 'text') $('uploads').append(row);
      const xhr = new XMLHttpRequest();
      xhr.open('POST', '/api/items');
      xhr.setRequestHeader('x-lanclip-key', key);
      xhr.setRequestHeader('x-lanclip-meta', encodeURIComponent(JSON.stringify({ kind, name, mime, originName: deviceName() })));
      xhr.upload.onprogress = (e) => { if (e.lengthComputable) row.querySelector('progress').value = e.loaded / e.total; };
      xhr.onload = () => { row.remove(); xhr.status < 300 ? resolve() : reject(new Error(xhr.responseText)); };
      xhr.onerror = () => { row.remove(); reject(new Error('network error')); };
      xhr.send(body);
    });
  }

  const IMAGE_TYPES = /^image\/(png|jpeg|gif|webp|bmp|svg\+xml|avif|heic)$/;
  async function sendFiles(files) {
    for (const f of files) {
      const kind = IMAGE_TYPES.test(f.type) ? 'image' : 'file';
      const name = f.name && f.name !== 'image.png' ? f.name : (kind === 'image' ? `image-${Date.now()}.${(f.type.split('/')[1] || 'png').replace('+xml', '')}` : 'file');
      upload(kind, name, f.type || 'application/octet-stream', f)
        .then(() => toast(`Sent ${name}`))
        .catch((e) => toast(`Failed: ${e.message}`));
    }
  }

  async function sendText() {
    const text = $('textInput').value;
    if (!text.trim()) return;
    try {
      await upload('text', 'text.txt', 'text/plain', new Blob([text], { type: 'text/plain' }));
      $('textInput').value = '';
      toast('Sent');
    } catch (e) { toast(`Failed: ${e.message}`); }
  }

  $('sendBtn').addEventListener('click', sendText);
  $('textInput').addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); sendText(); }
  });
  $('fileInput').addEventListener('change', (e) => { sendFiles([...e.target.files]); e.target.value = ''; });
  $('clipBtn').addEventListener('click', async () => {
    await api('/api/local/send-clipboard', { method: 'POST' });
    toast('Clipboard sent');
  });
  $('clearBtn').addEventListener('click', () => {
    if (confirm('Clear the history on this device? Other devices keep their copies.')) api('/api/items', { method: 'DELETE' });
  });
  $('filter').addEventListener('input', (e) => { filterText = e.target.value; render(); });

  // Paste files/images anywhere; plain text pastes into the box as usual.
  document.addEventListener('paste', (e) => {
    const files = [...(e.clipboardData?.files || [])];
    if (files.length) { e.preventDefault(); sendFiles(files); }
  });

  // Drag & drop from the OS
  let dragDepth = 0, internalDrag = false;
  const hasFiles = (e) => [...(e.dataTransfer?.types || [])].includes('Files');
  window.addEventListener('dragenter', (e) => { if (!internalDrag && hasFiles(e)) { dragDepth++; document.body.classList.add('dragging'); } });
  window.addEventListener('dragleave', () => { if (--dragDepth <= 0) { dragDepth = 0; document.body.classList.remove('dragging'); } });
  window.addEventListener('dragover', (e) => { if (hasFiles(e)) e.preventDefault(); });
  window.addEventListener('drop', (e) => {
    dragDepth = 0; document.body.classList.remove('dragging');
    if (internalDrag) return;
    e.preventDefault();
    const files = [...(e.dataTransfer?.files || [])];
    if (files.length) return sendFiles(files);
    const text = e.dataTransfer?.getData('text/plain');
    if (text) { $('textInput').value = text; sendText(); }
  });

  // ----- settings (only on the device itself) -----
  const form = $('settingsForm');
  $('settingsBtn').addEventListener('click', () => {
    const s = state.settings;
    form.name.value = s.name;
    form.autoSync.checked = s.autoSync;
    form.startAtLogin.checked = s.startAtLogin;
    form.retentionDays.value = s.retentionDays;
    form.maxStorageGb.value = s.maxStorageGb;
    $('usage').textContent = `History currently uses ${fmtSize(s.usedBytes || 0)} on this computer. Older items are deleted automatically; files you pasted into your own folders are never touched.`;
    $('version').textContent = s.version ? `Lanclip ${s.version}` : '';
    form.manualPeers.value = s.manualPeers.join('\n');
    const links = state.self.addresses.map((a) => `http://${a}:${state.self.port}/?key=${encodeURIComponent(s.key)}`);
    $('phoneLinks').replaceChildren(...links.map((u) => el('a', { href: u, target: '_blank' }, u)));
    $('qr').src = links[0] ? `/api/qr?key=${encodeURIComponent(key)}&url=${encodeURIComponent(links[0])}` : '';
    $('settings').showModal();
  });

  $('settings').addEventListener('close', async () => {
    if ($('settings').returnValue !== 'save') return;
    const body = {
      name: form.name.value, autoSync: form.autoSync.checked, startAtLogin: form.startAtLogin.checked,
      retentionDays: Number(form.retentionDays.value), maxStorageGb: Number(form.maxStorageGb.value),
      manualPeers: form.manualPeers.value.split(/\s+/).filter(Boolean),
    };
    const res = await api('/api/settings', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
    if (!res.ok) return toast('Could not save settings');
    state = await res.json();
    render();
    toast('Saved');
  });

  $('quitBtn').addEventListener('click', async () => {
    if (!confirm('Stop Lanclip on this computer? Context menu entries stay; it starts again when you use one.')) return;
    await api('/api/local/quit', { method: 'POST' });
    $('settings').close();
    toast('Lanclip stopped');
  });
  $('uninstallBtn').addEventListener('click', async () => {
    if (!confirm('Remove Lanclip from this computer (context menu entries, start at login, the app)?')) return;
    await api('/api/local/uninstall', { method: 'POST' });
    $('settings').close();
    document.body.innerHTML = '<p style="padding:40px;text-align:center">Lanclip was removed. You can close this tab.</p>';
  });

  if (!key) askForKey(); else connect();
})();
