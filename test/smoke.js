// Starts two Lanclip cores on this machine and checks they pair and relay items.
const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const { Core } = require('../src/core');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'lanclip-test-'));
const uiDir = path.join(__dirname, '..', 'ui');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(fn, what, ms = 8000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return; await sleep(100); }
  throw new Error(`timed out waiting for ${what}`);
}

(async () => {
  const mk = (name, port) => {
    const dataDir = path.join(tmp, name);
    fs.mkdirSync(dataDir, { recursive: true });
    fs.writeFileSync(path.join(dataDir, 'settings.json'), JSON.stringify({ name, key: 'TESTKEY123', manualPeers: [] }));
    return new Core({ dataDir, uiDir, port, discoveryPort: 41999 });
  };
  const a = mk('alpha', 18101);
  const b = mk('beta', 18201);
  await a.start(); await b.start();
  // Manual peers make the test independent of whether the OS loops broadcasts back.
  a.updateSettings({ manualPeers: [`127.0.0.1:${b.port}`] });
  b.updateSettings({ manualPeers: [`127.0.0.1:${a.port}`] });

  await waitFor(() => a.peerList().some((p) => p.name === 'beta' && p.sameKey) && b.peerList().some((p) => p.name === 'alpha' && p.sameKey), 'pairing');
  console.log('ok  peers paired:', a.peerList().map((p) => `${p.name}(${p.source})`).join(', '));

  const base = `http://127.0.0.1:${a.port}`;
  const post = (body, meta, key = 'TESTKEY123') => fetch(`${base}/api/items`, {
    method: 'POST', body,
    headers: { 'x-lanclip-key': key, 'x-lanclip-meta': encodeURIComponent(JSON.stringify(meta)) },
  });

  // auth
  assert.strictEqual((await fetch(`${base}/api/state`)).status, 401);
  assert.strictEqual((await post('x', { kind: 'text' }, 'WRONG')).status, 401);
  console.log('ok  wrong key rejected');

  // text relay
  const r1 = await post('héllo from alpha 👋', { kind: 'text', name: 'text.txt' });
  assert.strictEqual(r1.status, 200);
  await waitFor(() => b.items.some((it) => it.text === 'héllo from alpha 👋'), 'text on beta');
  console.log('ok  text relayed');

  // binary file relay (2 MB) with integrity check
  const blob = crypto.randomBytes(2 * 1024 * 1024);
  await post(blob, { kind: 'file', name: 'report final.pdf', mime: 'application/pdf' });
  await waitFor(() => b.items.some((it) => it.name === 'report final.pdf'), 'file on beta');
  const onB = b.items.find((it) => it.name === 'report final.pdf');
  assert.ok(fs.readFileSync(b.contentPath(onB)).equals(blob), 'file content mismatch');
  console.log('ok  2 MB file relayed intact');

  // download endpoint
  const dl = await fetch(`http://127.0.0.1:${b.port}/api/items/${onB.id}/content?key=TESTKEY123&download=1`);
  assert.ok(Buffer.from(await dl.arrayBuffer()).equals(blob));
  assert.match(dl.headers.get('content-disposition'), /attachment/);
  console.log('ok  download works');

  // no echo loop: each item exists exactly once on each side
  await sleep(500);
  assert.strictEqual(a.items.length, 2);
  assert.strictEqual(b.items.length, 2);
  assert.strictEqual(fs.readdirSync(b.itemsDir).length, 2);
  console.log('ok  no duplicate/echo items');

  // path traversal in names is neutralised
  await post('x', { kind: 'file', name: '../../evil.txt' });
  await waitFor(() => b.items.length === 3, 'third item');
  assert.ok(b.contentPath(b.items[0]).startsWith(path.join(b.itemsDir, b.items[0].id)));
  console.log('ok  file names sanitised');

  // Copy to LAN (folder + loose file) on alpha → Paste from LAN on beta
  const actions = require('../src/actions');
  const src = path.join(tmp, 'src');
  fs.mkdirSync(path.join(src, 'Photos', 'trip'), { recursive: true });
  fs.writeFileSync(path.join(src, 'Photos', 'a.jpg'), 'AAA');
  fs.writeFileSync(path.join(src, 'Photos', 'trip', 'b.txt'), 'BBB');
  fs.writeFileSync(path.join(src, 'notes.md'), '# hi');
  const sent = await actions.sendPaths(a, [path.join(src, 'Photos'), path.join(src, 'notes.md')]);
  assert.strictEqual(sent.files, 3);
  const dest = path.join(tmp, 'dest');
  fs.mkdirSync(path.join(dest, 'Photos'), { recursive: true }); // pre-existing → pasted as "Photos (2)"
  fs.writeFileSync(path.join(dest, 'notes.md'), 'old');
  const pasted = await actions.pasteLatest(b, dest); // waits for the whole batch to arrive
  assert.strictEqual(pasted.batch.items.length, 3);
  assert.strictEqual(fs.readFileSync(path.join(dest, 'Photos (2)', 'a.jpg'), 'utf8'), 'AAA');
  assert.strictEqual(fs.readFileSync(path.join(dest, 'Photos (2)', 'trip', 'b.txt'), 'utf8'), 'BBB');
  assert.strictEqual(fs.readFileSync(path.join(dest, 'notes (2).md'), 'utf8'), '# hi');
  assert.strictEqual(fs.readFileSync(path.join(dest, 'notes.md'), 'utf8'), 'old');
  console.log('ok  copy folder to LAN → paste on other device (structure kept, no overwrite)');

  // text paste becomes a .txt file
  await actions.sendText(a, 'pasted text');
  await waitFor(() => b.latestBatch().items[0].text === 'pasted text', 'text batch');
  const t = await actions.pasteLatest(b, dest);
  assert.match(path.basename(t.written[0]), /^LAN text .*\.txt$/);
  assert.strictEqual(fs.readFileSync(t.written[0], 'utf8'), 'pasted text');
  console.log('ok  text paste');

  // a malicious relPath from a peer can't escape the paste folder
  const { Readable } = require('stream');
  await b.ingest({ kind: 'file', name: 'x', relPath: '../../../escape/x', batch: 'evilbatch1', origin: { id: 'z', name: 'z' } }, Readable.from([Buffer.from('x')]));
  const evil = await actions.pasteLatest(b, dest);
  assert.ok(evil.written[0].startsWith(dest + path.sep), evil.written[0]);
  console.log('ok  relPath traversal blocked');

  // key mismatch → peers no longer sync
  b.updateSettings({ key: 'OTHERKEY99' });
  await waitFor(() => a.peerList().some((p) => p.name === 'beta' && !p.sameKey), 'mismatch detected');
  console.log('ok  key mismatch detected');

  a.stop(); b.stop();
  fs.rmSync(tmp, { recursive: true, force: true });
  console.log('\nall smoke tests passed');
  process.exit(0);
})().catch((err) => { console.error('FAIL', err); process.exit(1); });
