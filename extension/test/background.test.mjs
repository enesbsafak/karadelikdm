// background.js'i sahte bir tarayıcı API'siyle yükler ve gerçek akışları çalıştırır: indirme yakalama ve sağ tık
// menüsü. lib.js testleri background.js'teki bir hatayı (ör. tanımsız değişken) göremez; bu test görür.
import { test } from 'node:test';
import assert from 'node:assert/strict';

const listeners = {};
const sent = [];
const calls = [];
const event = (name) => ({ addListener: (fn) => { listeners[name] = fn; } });
const store = (init = {}) => {
  const d = { ...init };
  return {
    get: async (k) => (typeof k === 'string' ? (k in d ? { [k]: d[k] } : {}) : { ...d }),
    set: async (o) => Object.assign(d, o),
    remove: async () => {},
  };
};
// Uygulama bağlantısı ön elemeden sonra kurulur; ön eleme son bilinen ayarı depodan okur
const CONFIG = { enabled: true, exts: ['bin', 'deb'], min_size_kb: 0 };
// Canlı tutma ve yeniden bağlanma zamanlayıcıları test sürecini açık tutmasın
// (Kısa zamanlayıcılar dokunulmadan kalır: sahte WebSocket'in yanıtları onlarla gelir; işaretlenirse süreç yanıtlar
// gelmeden kapanabilir.)
for (const name of ['setTimeout', 'setInterval']) {
  const orig = globalThis[name];
  globalThis[name] = (fn, ms, ...rest) => {
    const t = orig(fn, ms, ...rest);
    if (name === 'setInterval' || ms >= 1000) t.unref?.();
    return t;
  };
}

globalThis.chrome = {
  runtime: {
    id: 'test-ext',
    getURL: (p) => `chrome-extension://test-ext/${p}`,
    getManifest: () => ({ version: '0.0.0' }),
    sendMessage: async () => {},
    onMessage: event('runtime.onMessage'),
    onStartup: event('runtime.onStartup'),
    onInstalled: event('runtime.onInstalled'),
    connectNative: () => {
      const port = { onMessage: event('port.onMessage'), onDisconnect: event('port.onDisconnect'), disconnect() {} };
      port.postMessage = () => setTimeout(() => listeners['port.onMessage']({ ok: true, port: 1, token: 't' }), 0);
      return port;
    },
  },
  i18n: { getMessage: (k) => k },
  storage: { local: store({ lastConfig: CONFIG }), session: store(), onChanged: event('storage.onChanged') },
  downloads: {
    onDeterminingFilename: event('downloads.onDeterminingFilename'),
    onCreated: event('downloads.onCreated'),
    cancel: async (id) => calls.push(['cancel', id]),
    erase: async (q) => calls.push(['erase', q.id]),
  },
  contextMenus: { removeAll: async () => {}, create: () => {}, onClicked: event('contextMenus.onClicked') },
  action: { setBadgeText: async () => {}, setBadgeBackgroundColor: async () => {}, setBadgeTextColor: async () => {}, onClicked: event('action.onClicked') },
  cookies: { getAll: async () => [{ name: 'oturum', value: '1', path: '/' }] },
  tabs: { onRemoved: event('tabs.onRemoved'), onUpdated: event('tabs.onUpdated'), onActivated: event('tabs.onActivated') },
  webRequest: { onHeadersReceived: event('webRequest.onHeadersReceived'), onBeforeRequest: event('webRequest.onBeforeRequest') },
  scripting: { executeScript: async () => [] },
  alarms: { create: () => {}, onAlarm: event('alarms.onAlarm') },
};

globalThis.WebSocket = class {
  static OPEN = 1;
  constructor() {
    this.readyState = 1;
    setTimeout(() => this.onopen && this.onopen(), 0);
  }
  send(data) {
    const m = JSON.parse(data);
    if (m.type === 'hello') {
      setTimeout(() => this.onmessage({ data: JSON.stringify({ type: 'hello', ok: true, config: CONFIG }) }), 0);
    } else if (m.id != null) {
      sent.push(m);
      setTimeout(() => this.onmessage({ data: JSON.stringify({ type: 'result', id: m.id, ok: true }) }), 0);
    }
  }
  close() {}
};

await import('../src/background.js');

test('indirme yakalanır, uygulamaya gönderilir ve tarayıcıdaki iptal edilir', async () => {
  const onName = listeners['downloads.onDeterminingFilename'];
  assert.ok(onName, 'onDeterminingFilename dinleyicisi yok');
  await new Promise((resolve) => {
    onName({ id: 7, url: 'https://ornek.com/dosya.bin', filename: 'C:\\İndirilenler\\dosya.bin', fileSize: 1000, state: 'in_progress', referrer: 'https://ornek.com/' }, resolve);
  });
  const req = sent.find((m) => m.type === 'download' && m.url === 'https://ornek.com/dosya.bin');
  assert.ok(req, `istek gitmedi: ${JSON.stringify(sent)}`);
  assert.equal(req.cookies, 'oturum=1');
  assert.equal(req.file_name, 'dosya.bin');
  assert.deepEqual(calls.filter((c) => c[1] === 7).map((c) => c[0]), ['cancel', 'erase']);
});

test('listede olmayan uzantı tarayıcıda kalır', async () => {
  const before = sent.length;
  await new Promise((resolve) => {
    listeners['downloads.onDeterminingFilename']({ id: 8, url: 'https://ornek.com/a.txt', filename: 'a.txt', fileSize: 10, state: 'in_progress' }, resolve);
  });
  assert.equal(sent.length, before);
  assert.ok(!calls.some((c) => c[1] === 8));
});

test('sağ tık: magnet bağlantısı çerezsiz gönderilir', async () => {
  const m = 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x';
  listeners['contextMenus.onClicked']({ menuItemId: 'kdm-download', linkUrl: m, pageUrl: 'https://site.com/' }, { id: 1 });
  for (let i = 0; i < 50 && !sent.some((x) => x.url === m); i++) await new Promise((r) => setTimeout(r, 10));
  const req = sent.find((x) => x.url === m);
  assert.ok(req, 'magnet gönderilmedi');
  assert.equal(req.cookies, '');
});

const POPUP = { url: 'chrome-extension://test-ext/popup.html' };

/** onMessage'ı tarayıcı gibi çağırır; yanıt yoksa (dinleyici false döndü) `null`. */
function message(msg, sender) {
  return new Promise((resolve) => {
    const handled = listeners['runtime.onMessage'](msg, { id: 'test-ext', ...sender }, resolve);
    if (handled !== true) resolve(null);
  });
}

test('video düğmesi: yakalanan medya listelenir, gönderilir; site gizlenince kapanır', async () => {
  const tab = { id: 5, url: 'https://video.site/izle/1', title: 'Film' };
  listeners['webRequest.onHeadersReceived']({
    tabId: 5, type: 'xmlhttprequest', statusCode: 200, url: 'https://cdn.video.site/hls/master.m3u8',
    responseHeaders: [{ name: 'content-type', value: 'application/vnd.apple.mpegurl' }],
  });
  await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(await message({ cmd: 'vbState' }, { tab }), { on: true });
  const list = await message({ cmd: 'vbList', src: 'blob:https://video.site/abc' }, { tab });
  assert.deepEqual(list.items.map((m) => [m.url, m.kind]), [['https://cdn.video.site/hls/master.m3u8', 'hls']]);

  // Yakalanmış akış
  assert.deepEqual(await message({ cmd: 'vbSend', url: 'https://cdn.video.site/hls/master.m3u8', kind: 'hls' }, { tab }), { ok: true });
  const a = sent.find((m) => m.type === 'media' && m.url === 'https://cdn.video.site/hls/master.m3u8');
  assert.ok(a, 'akış gönderilmedi');
  assert.equal(a.kind, 'hls');
  // Oynatıcının doğrudan adresi (ağdan yakalanmamış)
  assert.deepEqual(await message({ cmd: 'vbSend', url: 'https://cdn.video.site/v.mp4', kind: 'file' }, { tab }), { ok: true });
  const b = sent.find((m) => m.type === 'media' && m.url === 'https://cdn.video.site/v.mp4');
  assert.ok(b, 'doğrudan adres gönderilmedi');
  assert.equal(b.kind, 'file');
  assert.equal(b.referer, tab.url);
  assert.equal(b.title, 'Film');
  // http(s) olmayan adres reddedilir
  assert.equal((await message({ cmd: 'vbSend', url: 'file:///C:/gizli.txt', kind: 'file' }, { tab })).ok, false);

  assert.deepEqual(await message({ cmd: 'vbHideSite' }, { tab }), { ok: true });
  assert.deepEqual(await message({ cmd: 'vbState' }, { tab }), { on: false });
  // Alt alan adındaki sayfa da (iframe değil, ayrı sekme) aynı sitede sayılır
  assert.deepEqual(await message({ cmd: 'vbState' }, { tab: { id: 6, url: 'https://m.video.site/' } }), { on: false });
  // Açılır pencereden geri açılır
  await message({ cmd: 'setVideoButtonSite', host: 'video.site', value: true }, POPUP);
  assert.deepEqual(await message({ cmd: 'vbState' }, { tab }), { on: true });
  // Genel anahtar
  await message({ cmd: 'setVideoButton', value: false }, POPUP);
  assert.deepEqual(await message({ cmd: 'vbState' }, { tab }), { on: false });
  await message({ cmd: 'setVideoButton', value: true }, POPUP);
});

test('içerik betiği açılır pencere komutlarını çalıştıramaz', async () => {
  const tab = { id: 5, url: 'https://video.site/' };
  assert.equal(await message({ cmd: 'setPaused', value: true }, { tab }), null);
  assert.equal(await message({ cmd: 'sendMedia', tabId: 5, url: 'https://x/y.mp4' }, { tab }), null);
  // Sekmede açılmış kendi sayfamız (açılır pencere) açılır pencere sayılır
  assert.ok(await message({ cmd: 'state', tabId: -1, url: '' }, { ...POPUP, tab }));
  // Sayfa adresi uzantı adresine benzese de içerik betiğidir
  assert.equal(await message({ cmd: 'setPaused', value: true }, { tab, url: 'https://kotu.site/chrome-extension://test-ext/' }), null);
  // Başka uzantı
  assert.equal(await new Promise((resolve) => {
    const handled = listeners['runtime.onMessage']({ cmd: 'vbState' }, { id: 'baska', tab }, resolve);
    if (handled !== true) resolve(null);
  }), null);
});
