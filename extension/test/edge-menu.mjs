// Elle çalıştırılan uçtan uca test (npm test'e dahil değil): gerçek Edge'de sağ tık menüsü.
// Bağlantıya sağ tıklanır (DevTools), açılan yerel menüde öğe UI Automation ile çağrılır (uia-click.ps1).
// Karadelik DM açık olmalı (KDM_DATA, browser_show_dialog=false; WebView2 hata ayıklama portu verilirse toplu ekleme
// penceresi de okunur).
// Kullanım: node edge-menu.mjs <msedge.exe> <profil> <uzantı klasörü> <KDM_DATA> <test sayfası url> [webview2 portu]
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const [edge, profile, extDir, dataDir, pageUrl, wvPort] = process.argv.slice(2);
const UIA = path.join(path.dirname(fileURLToPath(import.meta.url)), 'uia-click.ps1');
fs.rmSync(profile, { recursive: true, force: true });
const child = spawn(edge, [`--user-data-dir=${profile}`, '--remote-debugging-pipe', '--enable-unsafe-extension-debugging', '--no-first-run', '--no-default-browser-check', '--disable-sync', '--window-size=1000,700', 'about:blank'],
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
const evalIn = async (s, expr) => (await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, s)).result?.result?.value;
async function attach(pred) {
  for (let i = 0; i < 20; i++) {
    const t = (await send('Target.getTargets')).result.targetInfos.find(pred);
    if (t) return (await send('Target.attachToTarget', { targetId: t.targetId, flatten: true })).result.sessionId;
    await sleep(500);
  }
  return null;
}
async function rightClick(s, x, y) {
  await send('Page.bringToFront', {}, s);
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y }, s);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'right', clickCount: 1 }, s);
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'right', clickCount: 1 }, s);
}
// Çıkış kodu 0: öğe bulundu ve çağrıldı (PowerShell çıktısının kodlamasına güvenilmez)
function uia(name) {
  try {
    execFileSync('powershell.exe', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', UIA, '-Name', name], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

await sleep(2000);
const ext = (await send('Extensions.loadUnpacked', { path: extDir })).result?.id;
const sw = await attach((t) => t.type === 'service_worker' && t.url.includes(ext));
let cfg = 'null';
for (let i = 0; i < 20 && cfg === 'null'; i++) { await sleep(1000); cfg = await evalIn(sw, "chrome.storage.local.get('lastConfig').then(o => JSON.stringify(o.lastConfig || null))"); }
check('uzantı yüklendi ve uygulamaya bağlandı', ext && cfg !== 'null');

const page = await attach((t) => t.type === 'page');
await send('Page.enable', {}, page);
await send('Page.navigate', { url: pageUrl }, page);
await sleep(2500);

// 1) Bağlantıda "Karadelik ile indir"
const rect = JSON.parse(await evalIn(page, "JSON.stringify(document.querySelector('a[href=\"dosya1.deb\"]').getBoundingClientRect())"));
await rightClick(page, rect.x + rect.width / 2, rect.y + rect.height / 2);
check('bağlantı menüsünde "Karadelik ile indir" çağrıldı', uia('Karadelik ile indir'));
const target = new URL('dosya1.deb', pageUrl).href;
let rec = null;
for (let i = 0; i < 30 && !(rec && rec.status === 'completed'); i++) {
  await sleep(500);
  try { rec = JSON.parse(fs.readFileSync(`${dataDir}/downloads.json`, 'utf8')).find((r) => r.url === target); } catch {}
}
check('menüden gönderilen bağlantı uygulamada indi', rec && rec.status === 'completed', rec ? `${rec.file_name} ${rec.size} bayt, referer ${rec.referer}` : 'kayıt yok');

// 2) Sayfada "Tüm bağlantıları Karadelik ile indir"
await rightClick(page, 600, 400);
check('sayfa menüsünde "Tüm bağlantıları..." çağrıldı', uia('Tüm bağlantıları Karadelik ile indir'));
if (wvPort) {
  await sleep(2500);
  const list = await (await fetch(`http://127.0.0.1:${wvPort}/json/list`)).json();
  const add = list.filter((t) => t.url.includes('/add.html')).pop();
  let urls = '';
  if (add) {
    const ws = new WebSocket(add.webSocketDebuggerUrl);
    await new Promise((r) => { ws.onopen = r; });
    urls = await new Promise((r) => {
      ws.onmessage = (e) => r(JSON.parse(e.data).result?.result?.value || '');
      setTimeout(() => ws.send(JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression: "document.getElementById('urls').value", returnByValue: true } })), 1000);
    });
    ws.close();
  }
  const got = urls.split('\n').filter(Boolean);
  check('toplu ekleme penceresi sayfanın bağlantılarıyla açıldı', got.some((u) => u.endsWith('dosya1.deb')) && got.some((u) => u.endsWith('dosya2.deb')) && got.some((u) => u.endsWith('disari.pdf')), got.join(', '));
}
await send('Browser.close');
await sleep(500);
child.kill();
process.exit(results.every(Boolean) ? 0 : 1);
