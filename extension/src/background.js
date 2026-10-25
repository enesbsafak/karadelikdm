// Arka plan: Chromium'da MV3 service worker, Firefox'ta olay sayfası (event page). İkisi de boşta kalınca
// uyutulur; bu yüzden kalıcı olması gereken durum storage'da tutulur ve dinleyiciler üst düzeyde, eşzamanlı
// kaydedilir (uyanmaya sebep olan olay kaçmasın).

import * as L from './lib.js';

const api = globalThis.browser ?? globalThis.chrome;
const HAS_FILENAME_HOOK = !!api.downloads.onDeterminingFilename;
const IS_FIREFOX = typeof globalThis.browser !== 'undefined' && !!api.runtime.getBrowserInfo;

const KEEPALIVE_MS = 20000; // Chrome 116+: WebSocket trafiği 30 sn'lik uyku penceresini sıfırlar
const REQUEST_TIMEOUT_MS = 8000;
const HELLO_TIMEOUT_MS = 5000;
const ACCENT = '#ff8a1f';

const state = {
  ws: null,
  status: 'offline', // connecting | connected | offline
  config: null,
  connecting: null,
  connectingLaunch: false,
  pending: new Map(),
  nextId: 1,
  keepalive: null,
  retryTimer: null,
  attempt: 0,
};

/** Sekme kimliği → [{url, kind, mime, size, ts}] */
const media = new Map();
let mediaSaveTimer = null;

const t = (key, subs) => api.i18n.getMessage(key, subs) || key;

// ---------------------------------------------------------------- keşif ve bağlantı

/** Native host'tan `{port, token}` alır. `launch`: uygulama kapalıysa başlatılsın mı. */
function discover(launch) {
  return new Promise((resolve, reject) => {
    let port;
    try {
      port = api.runtime.connectNative(L.HOST_NAME);
    } catch (e) {
      reject(e);
      return;
    }
    let done = false;
    // Host uygulamayı başlatırsa en çok 10 sn bekler; üstüne pay
    const timer = setTimeout(() => finish(new Error('nmh_timeout')), launch ? 15000 : 5000);
    function finish(err, val) {
      if (done) return;
      done = true;
      clearTimeout(timer);
      try {
        port.disconnect();
      } catch {}
      if (err) reject(err);
      else resolve(val);
    }
    port.onMessage.addListener((msg) => {
      if (msg && msg.ok && msg.port && msg.token) finish(null, { port: msg.port, token: msg.token });
      else finish(new Error((msg && msg.error) || 'nmh_error'));
    });
    port.onDisconnect.addListener((p) => {
      const err = (p && p.error) || api.runtime.lastError;
      finish(new Error((err && err.message) || 'nmh_disconnected'));
    });
    port.postMessage({ type: 'discover', launch: !!launch });
  });
}

function openSocket(port, token) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    let authed = false;
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error('hello_timeout'));
    }, HELLO_TIMEOUT_MS);
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: 'hello', token, version: api.runtime.getManifest().version }));
    };
    ws.onmessage = (ev) => {
      let msg;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        return;
      }
      if (!authed) {
        if (msg.type === 'hello' && msg.ok) {
          authed = true;
          clearTimeout(timer);
          state.ws = ws;
          state.attempt = 0;
          clearTimeout(state.retryTimer);
          state.retryTimer = null;
          applyConfig(msg.config);
          setStatus('connected');
          startKeepalive();
          api.storage.session.remove(['retryAt', 'attempt']).catch(() => {});
          resolve();
        }
        return;
      }
      onServerMessage(msg);
    };
    ws.onerror = () => {}; // ardından onclose gelir
    ws.onclose = () => {
      clearTimeout(timer);
      if (!authed) {
        reject(new Error('closed'));
        return;
      }
      if (state.ws !== ws) return;
      state.ws = null;
      stopKeepalive();
      for (const p of state.pending.values()) p.reject(new Error('closed'));
      state.pending.clear();
      setStatus('offline');
      scheduleRetry();
    };
  });
}

/**
 * Bağlıysa hemen döner. `launch:false` arka plan denemesidir: kullanıcı uygulamayı kapattıysa uzantı onu
 * kendiliğinden açmaz. Kullanıcı eylemleri (indirme, menü, açılır pencere) `launch:true` ister.
 */
function connect(launch = false) {
  if (state.status === 'connected' && state.ws) return Promise.resolve(true);
  if (state.connecting) {
    if (!launch || state.connectingLaunch) return state.connecting;
    return state.connecting.then((ok) => ok || connect(true));
  }
  state.connectingLaunch = launch;
  setStatus('connecting');
  state.connecting = (async () => {
    try {
      const { port, token } = await discover(launch);
      await openSocket(port, token);
      return true;
    } catch (e) {
      console.debug('Karadelik: bağlanılamadı:', e && e.message);
      setStatus('offline');
      scheduleRetry();
      return false;
    } finally {
      state.connecting = null;
    }
  })();
  return state.connecting;
}

function scheduleRetry(delay) {
  if (state.retryTimer) return;
  const wait = delay ?? L.backoffDelay(state.attempt++, 2000, 60000);
  state.retryTimer = setTimeout(() => {
    state.retryTimer = null;
    connect(false);
  }, wait);
  // Uyku sonrası uyanınca aynı sıklığı korumak için
  api.storage.session.set({ retryAt: Date.now() + wait, attempt: state.attempt }).catch(() => {});
}

function startKeepalive() {
  stopKeepalive();
  state.keepalive = setInterval(() => {
    if (state.ws && state.ws.readyState === WebSocket.OPEN) state.ws.send('{"type":"ping"}');
  }, KEEPALIVE_MS);
}

function stopKeepalive() {
  if (state.keepalive) clearInterval(state.keepalive);
  state.keepalive = null;
}

function onServerMessage(msg) {
  if (msg.type === 'config') {
    applyConfig(msg.config);
    return;
  }
  if ((msg.type === 'result' || msg.type === 'pong') && msg.id != null) {
    const p = state.pending.get(msg.id);
    if (p) {
      state.pending.delete(msg.id);
      p.resolve(msg);
    }
  }
}

function applyConfig(cfg) {
  if (!cfg) return;
  state.config = { enabled: !!cfg.enabled, exts: L.normalizeExts(cfg.exts), min_size_kb: Number(cfg.min_size_kb) || 0 };
  // Uyanışta, bağlantı kurulmadan ön eleme yapabilmek için
  api.storage.local.set({ lastConfig: state.config }).catch(() => {});
  notifyPopup();
}

function setStatus(s) {
  if (state.status === s) return;
  state.status = s;
  notifyPopup();
}

function notifyPopup() {
  api.runtime.sendMessage({ event: 'state' }).catch(() => {}); // açılır pencere kapalıysa alıcı yok
}

/** Uygulamaya istek gönderir; başarısızlıkta hata fırlatır. */
async function request(msg, { launch = false } = {}) {
  if (!(await connect(launch))) throw new Error('offline');
  const id = state.nextId++;
  const reply = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      state.pending.delete(id);
      reject(new Error('timeout'));
    }, REQUEST_TIMEOUT_MS);
    state.pending.set(id, {
      resolve: (m) => {
        clearTimeout(timer);
        resolve(m);
      },
      reject: (e) => {
        clearTimeout(timer);
        reject(e);
      },
    });
    try {
      state.ws.send(JSON.stringify({ ...msg, id }));
    } catch (e) {
      state.pending.delete(id);
      clearTimeout(timer);
      reject(e);
    }
  });
  if (!reply.ok) throw new Error(reply.error || 'failed');
  return reply;
}

// ---------------------------------------------------------------- tercihler

async function getPrefs() {
  const [{ excluded = [] }, { paused = false }] = await Promise.all([
    api.storage.local.get('excluded'),
    api.storage.session.get('paused'),
  ]);
  return { excluded, paused };
}

async function cookiesFor(url) {
  try {
    // Firefox'ta birinci taraf yalıtımı açıksa alan belirtilmeden çerez dönmez
    const q = IS_FIREFOX ? { url, firstPartyDomain: null } : { url };
    return L.cookieHeader(await api.cookies.getAll(q));
  } catch {
    try {
      return L.cookieHeader(await api.cookies.getAll({ url }));
    } catch {
      return '';
    }
  }
}

// ---------------------------------------------------------------- indirme yakalama

/** `true`: uygulama indirmeyi aldı, tarayıcınınki iptal edildi. Her hata tarayıcıya bırakır. */
async function maybeCapture(item) {
  const prefs = await getPrefs();
  const cached = state.config || (await api.storage.local.get('lastConfig')).lastConfig;
  // Ön eleme: eşleşmeyen her indirme için uygulamayı uyandırmamak gerek
  if (!L.shouldCapture(item, cached, prefs, api.runtime.id).capture) return false;
  if (!(await connect(true))) return false;
  // Ayar uygulamada değişmiş olabilir; güncel yapılandırmayla yeniden karar
  const dec = L.shouldCapture(item, state.config, prefs, api.runtime.id);
  if (!dec.capture) return false;
  const url = item.finalUrl || item.url;
  try {
    await request(
      {
        type: 'download',
        url,
        referer: item.referrer || '',
        cookies: await cookiesFor(url),
        user_agent: navigator.userAgent,
        file_name: dec.name || '',
        size: dec.size,
        mime: item.mime || '',
        page_url: item.referrer || '',
      },
      { launch: true },
    );
  } catch (e) {
    console.warn('Karadelik: indirme devredilemedi, tarayıcıda kalıyor:', e && e.message);
    return false;
  }
  try {
    await api.downloads.cancel(item.id);
  } catch {}
  try {
    await api.downloads.erase({ id: item.id });
  } catch {}
  return true;
}

if (HAS_FILENAME_HOOK) {
  // Chromium: dosya adı ve boyut belli, dosya henüz hedefe yazılmadı; karar verilene kadar indirme bekler
  api.downloads.onDeterminingFilename.addListener((item, suggest) => {
    maybeCapture(item)
      .catch(() => false)
      .then(() => {
        try {
          suggest();
        } catch {}
      });
    return true;
  });
} else {
  // Firefox: onDeterminingFilename yok; indirme başladıktan sonra iptal edilir
  api.downloads.onCreated.addListener((item) => {
    maybeCapture(item).catch(() => {});
  });
}

// ---------------------------------------------------------------- bağlam menüsü

const MENU_ONE = 'kdm-download';
const MENU_ALL = 'kdm-download-all';

async function createMenus() {
  try {
    await api.contextMenus.removeAll();
  } catch {}
  const quiet = () => void api.runtime.lastError;
  api.contextMenus.create({ id: MENU_ONE, title: t('menuDownload'), contexts: ['link', 'image', 'video', 'audio'] }, quiet);
  api.contextMenus.create({ id: MENU_ALL, title: t('menuDownloadAll'), contexts: ['page'] }, quiet);
}

async function flashBadge(tabId, text, color) {
  if (tabId == null || tabId < 0) return;
  try {
    await api.action.setBadgeBackgroundColor({ tabId, color });
    await api.action.setBadgeText({ tabId, text });
  } catch {}
  setTimeout(() => updateBadge(tabId), 2500);
}

const flashOk = (tabId) => flashBadge(tabId, '✓', '#3ddc84');
const flashErr = (tabId) => flashBadge(tabId, '!', '#ff5d5d');

/** Sayfa bağlamında çalışır: yalnız DOM okur. */
function collectLinks() {
  return Array.from(document.querySelectorAll('a[href]'), (a) => a.href);
}

async function onMenu(info, tab) {
  const tabId = tab && tab.id;
  const pageUrl = info.pageUrl || (tab && tab.url) || '';
  if (info.menuItemId === MENU_ONE) {
    const url = info.linkUrl || info.srcUrl || '';
    const magnet = L.isMagnet(url);
    if (!L.isHttp(url) && !magnet) {
      flashErr(tabId);
      return;
    }
    await request(
      {
        type: 'download',
        url,
        referer: pageUrl,
        cookies: magnet ? '' : await cookiesFor(url),
        user_agent: navigator.userAgent,
        file_name: '',
        size: null,
        mime: '',
        page_url: pageUrl,
      },
      { launch: true },
    );
    flashOk(tabId);
  } else if (info.menuItemId === MENU_ALL) {
    const results = await api.scripting.executeScript({ target: { tabId, allFrames: true }, func: collectLinks });
    const urls = L.cleanLinks(results.flatMap((r) => (Array.isArray(r.result) ? r.result : [])));
    if (!urls.length) {
      flashErr(tabId);
      return;
    }
    // Links tek bir çerez alanı taşır; farklı sitelere aynı çerezi göndermemek için yalnız tek ana makinede
    const web = urls.filter((u) => L.isHttp(u));
    const cookies = web.length && L.singleHost(web) ? await cookiesFor(web[0]) : '';
    await request({ type: 'links', urls, referer: pageUrl, cookies, user_agent: navigator.userAgent }, { launch: true });
    flashOk(tabId);
  }
}

api.contextMenus.onClicked.addListener((info, tab) => {
  onMenu(info, tab).catch((e) => {
    console.warn('Karadelik:', e && e.message);
    flashErr(tab && tab.id);
  });
});

// ---------------------------------------------------------------- medya tespiti

const mediaReady = api.storage.session
  .get('media')
  .then(({ media: saved }) => {
    for (const [k, v] of Object.entries(saved || {})) if (!media.has(Number(k))) media.set(Number(k), v);
  })
  .catch(() => {});

function saveMedia() {
  clearTimeout(mediaSaveTimer);
  mediaSaveTimer = setTimeout(() => {
    api.storage.session.set({ media: Object.fromEntries(media) }).catch(() => {});
  }, 500);
}

async function updateBadge(tabId) {
  const n = (media.get(tabId) || []).length;
  try {
    await api.action.setBadgeBackgroundColor({ tabId, color: ACCENT });
    if (api.action.setBadgeTextColor) await api.action.setBadgeTextColor({ tabId, color: '#1a0d00' });
    await api.action.setBadgeText({ tabId, text: n ? String(n) : '' });
  } catch {}
}

api.webRequest.onHeadersReceived.addListener(
  (d) => {
    if (d.tabId == null || d.tabId < 0) return;
    mediaReady.then(() => {
      if (d.type === 'main_frame' && media.has(d.tabId)) {
        media.delete(d.tabId); // yeni sayfa
        updateBadge(d.tabId);
        saveMedia();
      }
      const m = L.detectMedia(d.url, d.responseHeaders, d.statusCode);
      if (!m) return;
      const list = media.get(d.tabId) || [];
      if (list.some((x) => x.url === d.url)) return;
      media.set(d.tabId, L.addMedia(list, { url: d.url, ...m, ts: Date.now() }));
      updateBadge(d.tabId);
      saveMedia();
    });
  },
  { urls: ['<all_urls>'] },
  ['responseHeaders'],
);

api.tabs.onRemoved.addListener((tabId) => {
  mediaReady.then(() => {
    if (media.delete(tabId)) saveMedia();
  });
});

async function sendMedia(tabId, url) {
  await mediaReady;
  const item = (media.get(tabId) || []).find((m) => m.url === url);
  if (!item) throw new Error('not_found');
  let tab = {};
  try {
    tab = await api.tabs.get(tabId);
  } catch {}
  await request(
    {
      type: 'media',
      url: item.url,
      kind: item.kind,
      title: tab.title || '',
      referer: tab.url || '',
      cookies: await cookiesFor(item.url),
      user_agent: navigator.userAgent,
    },
    { launch: true },
  );
}

// ---------------------------------------------------------------- açılır pencere

async function popupState(tabId, url) {
  await mediaReady;
  const prefs = await getPrefs();
  const host = L.isHttp(url) ? L.hostOf(url) : '';
  return {
    status: state.status,
    config: state.config,
    paused: prefs.paused,
    host,
    excluded: host ? L.isExcluded(url, prefs.excluded) : false,
    media: media.get(tabId) || [],
  };
}

async function onPopup(msg) {
  switch (msg.cmd) {
    case 'state':
      // Yalnız açılışta bir kez; durum bildirimiyle gelen yenilemeler yeniden denemesin (döngü olurdu)
      if (msg.probe && state.status === 'offline' && !state.connecting) connect(false);
      return popupState(msg.tabId, msg.url);
    case 'connect':
      return { ok: await connect(true) };
    case 'setPaused':
      await api.storage.session.set({ paused: !!msg.value });
      return { ok: true };
    case 'setExcluded': {
      const host = String(msg.host || '').toLowerCase();
      if (!host) return { ok: false };
      const { excluded = [] } = await api.storage.local.get('excluded');
      const next = excluded.filter((h) => h !== host);
      if (msg.value) next.push(host);
      await api.storage.local.set({ excluded: next });
      return { ok: true };
    }
    case 'sendMedia':
      await sendMedia(msg.tabId, msg.url);
      return { ok: true };
    default:
      throw new Error('unknown');
  }
}

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  // Yalnız kendi sayfalarımız (açılır pencere); içerik betiği kullanılmıyor
  if (sender.id !== api.runtime.id || sender.tab || !msg || !msg.cmd) return false;
  onPopup(msg).then(sendResponse, (e) => sendResponse({ ok: false, error: (e && e.message) || String(e) }));
  return true;
});

// ---------------------------------------------------------------- başlangıç

api.runtime.onInstalled.addListener(() => {
  createMenus();
});
api.runtime.onStartup.addListener(() => {
  createMenus();
});

// Her uyanışta: önceki geri çekilme süresi dolduysa bağlan, dolmadıysa kalan süreyi bekle
api.storage.session
  .get(['retryAt', 'attempt'])
  .then(({ retryAt = 0, attempt = 0 }) => {
    state.attempt = attempt;
    const wait = retryAt - Date.now();
    if (wait > 0) scheduleRetry(wait);
    else connect(false);
  })
  .catch(() => connect(false));
