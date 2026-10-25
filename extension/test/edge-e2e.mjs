// Elle çalıştırılan uçtan uca test (npm test'e dahil değil): kurulu Karadelik DM + gerçek Edge + bu uzantı.
// Uygulama açık ve KDM_DATA ile verilen veri klasörünü kullanıyor olmalı; indirme adresi uygulamanın yakaladığı
// bir uzantıyla bitmeli (IDM kuruluysa onun listesinde olmayan bir tür seçin, ör. .deb).
// Gerçek Edge'de uçtan uca: uzantıyı CDP ile yükler (Extensions.loadUnpacked), uygulamaya bağlanmasını bekler,
// bir indirmeyi tetikler ve uygulamanın indirme listesini denetler.
// Kullanım: node edge_e2e.mjs <msedge.exe> <profil> <uzantı klasörü> <KDM_DATA> <indirme url>
import { spawn } from 'node:child_process';
import fs from 'node:fs';

const [edge, profile, extDir, dataDir, url] = process.argv.slice(2);
fs.rmSync(profile, { recursive: true, force: true });
const child = spawn(edge, [
  `--user-data-dir=${profile}`,
  '--remote-debugging-pipe',
  '--enable-unsafe-extension-debugging',
  '--no-first-run',
  '--no-default-browser-check',
  '--disable-sync',
  'about:blank',
], { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], env: { ...process.env, KDM_DATA: dataDir } });
const toBrowser = child.stdio[3];
const fromBrowser = child.stdio[4];

let nextId = 0;
const pending = new Map();
let buf = '';
const events = [];
fromBrowser.on('data', (d) => {
  buf += d.toString();
  let i;
  while ((i = buf.indexOf('\0')) >= 0) {
    const m = JSON.parse(buf.slice(0, i));
    buf = buf.slice(i + 1);
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } else events.push(m);
  }
});
const send = (method, params = {}, sessionId) =>
  new Promise((r) => { const id = ++nextId; pending.set(id, r); toBrowser.write(JSON.stringify({ id, method, params, sessionId }) + '\0'); });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ok = [];
const check = (name, cond, detail) => { ok.push(!!cond); console.log(`${cond ? 'GEÇTİ ' : 'KALDI '} ${name}${detail ? ' — ' + detail : ''}`); };

await sleep(2000);
const loaded = await send('Extensions.loadUnpacked', { path: extDir });
check('uzantı gerçek Edge\'e yüklendi', loaded.result?.id === 'kkifckmmeinedmmdokgacefiojpfbpfl', JSON.stringify(loaded.result || loaded.error));

// Uzantının service worker'ına bağlanıp durumunu oku
async function swEval(expr) {
  const { result } = await send('Target.getTargets');
  const sw = result.targetInfos.find((t) => t.type === 'service_worker' && t.url.includes(loaded.result?.id));
  if (!sw) return { error: 'service worker yok' };
  const att = await send('Target.attachToTarget', { targetId: sw.targetId, flatten: true });
  const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, att.result.sessionId);
  return r.result?.result?.value ?? r.result?.exceptionDetails?.exception?.description;
}

// Uyanışta connect(false) çalışır; yakalama ayarı önbelleğe düşene dek bekle
let cfg = null;
for (let i = 0; i < 20 && !cfg; i++) {
  await sleep(1000);
  const v = await swEval("chrome.storage.local.get('lastConfig').then(o => JSON.stringify(o.lastConfig || null))");
  cfg = v && v !== 'null' && !String(v).includes('yok') ? JSON.parse(v) : null;
}
check('uzantı uygulamaya bağlandı ve yakalama ayarını aldı', cfg && cfg.enabled && cfg.exts.includes('bin'), cfg ? `${cfg.exts.length} uzantı` : 'ayar yok');

// Service worker konsolunu dinle
const { result: tl } = await send('Target.getTargets');
const swT = tl.targetInfos.find((t) => t.type === 'service_worker' && t.url.includes(loaded.result?.id));
const swS = (await send('Target.attachToTarget', { targetId: swT.targetId, flatten: true })).result.sessionId;
await send('Runtime.enable', {}, swS);
// İndirmeyi var olan sekmede tetikle
const page = tl.targetInfos.find((t) => t.type === 'page');
const pS = (await send('Target.attachToTarget', { targetId: page.targetId, flatten: true })).result.sessionId;
const nav = await send('Page.navigate', { url }, pS);
console.log('gezinme:', JSON.stringify(nav.result || nav.error));
let rec = null;
for (let i = 0; i < 30 && !(rec && rec.status === 'completed'); i++) {
  await sleep(1000);
  if (i < 6) console.log('  tarayıcı indirmeleri:', await swEval("chrome.downloads.search({}).then(l => JSON.stringify(l.map(d => d.state + ' ' + (d.error||'') + ' ' + d.filename)))"));
  try {
    rec = JSON.parse(fs.readFileSync(`${dataDir}/downloads.json`, 'utf8')).find((r) => r.url === url);
  } catch {}
}
check('tarayıcı indirmesi uygulamaya devredildi ve tamamlandı', rec && rec.status === 'completed', rec ? `${rec.status} ${rec.file_name} ${rec.size}` : 'kayıt yok');
check('referer ve kullanıcı ajanı taşındı', rec && rec.user_agent.includes('Edg/'), rec ? rec.user_agent.slice(0, 60) : '');

// Tarayıcının kendi indirmesi iptal edilmiş olmalı
const dl = await swEval("chrome.downloads.search({}).then(l => JSON.stringify(l.map(d => d.state + ' ' + d.url)))");
check('tarayıcıda kendi kopyası kalmadı', dl === '[]', dl);

for (const e of events) {
  if (e.method === 'Runtime.consoleAPICalled') console.log('  sw konsol:', e.params.type, e.params.args.map((a) => a.value ?? a.description).join(' '));
  if (e.method === 'Runtime.exceptionThrown') console.log('  sw istisna:', e.params.exceptionDetails.exception?.description);
}
await send('Browser.close');
await sleep(1000);
child.kill();
process.exit(ok.every(Boolean) ? 0 : 1);
