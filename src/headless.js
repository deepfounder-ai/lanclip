// Run Lanclip without a GUI (e.g. on a home server/NAS). No clipboard integration;
// it stores and relays items and serves the web UI for browsers and phones.
const path = require('path');
const os = require('os');
const { Core } = require('./core');

const dataDir = process.env.LANCLIP_DATA || path.join(os.homedir(), '.lanclip');
const core = new Core({
  dataDir,
  uiDir: path.join(__dirname, '..', 'ui'),
  port: Number(process.env.LANCLIP_PORT) || 7878,
  discoveryPort: Number(process.env.LANCLIP_DISCOVERY_PORT) || undefined,
  deviceName: process.env.LANCLIP_NAME,
});
core.on('log', (m) => console.log('[lanclip]', m));

core.start().then(() => {
  console.log(`Lanclip headless "${core.settings.name}" on port ${core.port}`);
  console.log(`Data: ${dataDir}`);
  console.log(`Pairing key: ${core.settings.key}`);
  for (const u of core.phoneUrls()) console.log(`Open: ${u}`);
});

process.on('SIGINT', () => { core.stop(); process.exit(0); });
