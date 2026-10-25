// Elle çalıştırılan uçtan uca test (npm test'e dahil değil): gerçek Edge'de medya tespiti ve açılır pencere yolu.
// Karadelik DM açık olmalı (KDM_DATA ile). Sayfa bir HLS listesi yükler; uzantı onu webRequest ile bulup rozette
// gösterir, gerçek açılır pencereden "gönder" ile uygulamaya iletilir. Açılır pencerenin ekran görüntüsü kaydedilir.
// Kullanım: node edge-paths.mjs <msedge.exe> <profil> <uzantı klasörü> <KDM_DATA> <test sayfası url> <görüntü.png>
import { spawn } from 'node:child_process';
import fs from 'node:fs';

const [edge, profile, extDir, dataDir, pageUrl, shot] = process.argv.slice(2);
fs.rmSync(profile, { recursive: true, force: true });
const child = spawn(edge, [`--user-data-dir=${profile}`, '--remote-debugging-pipe', '--enable-unsafe-extension-debugging', '--no-first-run', '--no-default-browser-check', '--disable-sync', 'about:blank'],
  { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], env: { ...process.env, KDM_DATA: dataDir } });
const w = child.stdio[3];
let buf = '';
let id = 0;
const pending = new Map();
child.stdio[4].on('data', (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf('\0')) >= 0) {
    const m = JSON.parse(buf.slice(0, i));
    buf = buf.slice(i + 1);
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  }
});
const send = (method, params = {}, sessionId) => new Promise((r) => { const n = ++id; pending.set(n, r); w.write(JSON.stringify({ id: n, method, params, sessionId }) + '\0'); });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail) => { results.push(!!ok); console.log(`${ok ? 'GEÇTİ ' : 'KALDI '} ${name}${detail ? ' — ' + detail : ''}`); };

async function attach(pred) {
  for (let i = 0; i < 20; i++) {
    const { result } = await send('Target.getTargets');
    const t = result.targetInfos.find(pred);
    if (t) return (await send('Target.attachToTarget', { targetId: t.targetId, flatten: true })).result.sessionId;
    await sleep(500);
  }
  return null;
}
const evalIn = async (s, expr) => {
  const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, s);
  return r.result?.result?.value ?? `HATA: ${r.result?.exceptionDetails?.exception?.description}`;
};

await sleep(2000);
const ext = (await send('Extensions.loadUnpacked', { path: extDir })).result?.id;
check('uzantı yüklendi', !!ext, ext);
const sw = await attach((t) => t.type === 'service_worker' && t.url.includes(ext));
let cfg = 'null';
for (let i = 0; i < 20 && cfg === 'null'; i++) { await sleep(1000); cfg = await evalIn(sw, "chrome.storage.local.get('lastConfig').then(o => JSON.stringify(o.lastConfig || null))"); }
check('uygulamaya bağlandı', cfg !== 'null');

// Test sayfası: HLS listesini fetch ile yükler
const page = await attach((t) => t.type === 'page');
await send('Page.enable', {}, page);
await send('Page.navigate', { url: pageUrl }, page);
await sleep(3000);
const tabId = await evalIn(sw, `chrome.tabs.query({}).then(t => t.find(x => (x.url||'').startsWith(${JSON.stringify(pageUrl)})).id)`);
const badge = await evalIn(sw, `chrome.action.getBadgeText({ tabId: ${tabId} })`);
check('sayfadaki HLS akışı tespit edildi, rozette sayı var', badge === '1', `rozet "${badge}"`);

// Gerçek açılır pencere (kullanıcının simgeye tıklamasıyla aynı yol)
await send('Target.activateTarget', { targetId: (await send('Target.getTargets')).result.targetInfos.find((t) => t.type === 'page').targetId });
const opened = await evalIn(sw, 'chrome.action.openPopup().then(() => "açıldı", e => "HATA " + e.message)');
const popup = await attach((t) => t.url.endsWith('/popup.html'));
check('açılır pencere açıldı', !!popup, opened);
if (popup) {
  await sleep(1500);
  const st = await evalIn(popup, `chrome.runtime.sendMessage({ cmd: 'state', tabId: ${tabId}, url: ${JSON.stringify(pageUrl)} }).then(JSON.stringify)`);
  const state = JSON.parse(st);
  check('açılır pencere bağlantı durumunu ve medya listesini aldı', state.status === 'connected' && state.media.length === 1, `durum ${state.status}, medya ${state.media.map((m) => m.kind + ' ' + m.url.split('/').pop())}`);
  const shotRes = await send('Page.captureScreenshot', { format: 'png' }, popup);
  if (shotRes.result?.data) fs.writeFileSync(shot, Buffer.from(shotRes.result.data, 'base64'));
  const sent = await evalIn(popup, `chrome.runtime.sendMessage({ cmd: 'sendMedia', tabId: ${tabId}, url: ${JSON.stringify(state.media[0]?.url || '')} }).then(JSON.stringify)`);
  check('akış uygulamaya gönderildi', JSON.parse(sent).ok === true, sent);
}
await send('Browser.close');
await sleep(500);
child.kill();
process.exit(results.every(Boolean) ? 0 : 1);
