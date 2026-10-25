import { formatSize, fileNameFromUrl, hostOf } from './lib.js';

const api = globalThis.browser ?? globalThis.chrome;
const t = (key) => api.i18n.getMessage(key) || key;
const $ = (id) => document.getElementById(id);

let tab = null;
let current = null;

function localize() {
  document.documentElement.lang = api.i18n.getUILanguage();
  for (const el of document.querySelectorAll('[data-i18n]')) el.textContent = t(el.dataset.i18n);
}

const send = (msg) => api.runtime.sendMessage(msg);

async function refresh(probe = false) {
  const s = await send({ cmd: 'state', probe, tabId: tab ? tab.id : -1, url: (tab && tab.url) || '' });
  if (s && !s.error) render(s);
}

function render(s) {
  current = s;
  const status = $('status');
  status.className = `pill ${s.status}`;
  status.textContent = t(
    s.status === 'connected' ? 'statusConnected' : s.status === 'connecting' ? 'statusConnecting' : 'statusOffline',
  );
  $('offline').classList.toggle('hidden', s.status !== 'offline');
  $('disabled').classList.toggle('hidden', !(s.status === 'connected' && s.config && !s.config.enabled));

  $('paused').checked = !!s.paused;
  const ex = $('excluded');
  ex.checked = !!s.excluded;
  ex.disabled = !s.host;
  $('siteRow').classList.toggle('disabled', !s.host);
  $('host').textContent = s.host || t('noSite');

  renderMedia(s.media || []);
}

function renderMedia(list) {
  const ul = $('media');
  // Gönderim sonucu (✓ / hata) yeniden çizimde kaybolmasın
  const marks = new Map([...ul.querySelectorAll('.item')].map((b) => [b.dataset.url, b.className]));
  ul.replaceChildren();
  $('mediaEmpty').classList.toggle('hidden', list.length > 0);
  for (const m of list) {
    const li = document.createElement('li');
    const b = document.createElement('button');
    b.type = 'button';
    b.className = marks.get(m.url) || 'item';
    b.dataset.url = m.url;
    b.title = m.url;

    const kind = document.createElement('span');
    kind.className = 'kind';
    kind.textContent = m.kind === 'file' ? (m.mime || '').split('/')[1] || t('kindFile') : m.kind;

    const name = document.createElement('span');
    name.className = 'name';
    name.textContent = fileNameFromUrl(m.url) || hostOf(m.url);

    const meta = document.createElement('span');
    meta.className = 'meta';
    meta.textContent = b.classList.contains('sent') ? t('sent') : formatSize(m.size);

    b.append(kind, name, meta);
    b.addEventListener('click', () => sendMedia(b, meta, m));
    li.append(b);
    ul.append(li);
  }
}

async function sendMedia(b, meta, m) {
  b.disabled = true;
  meta.textContent = '…';
  const r = await send({ cmd: 'sendMedia', tabId: tab.id, url: m.url }).catch((e) => ({ ok: false, error: e.message }));
  b.disabled = false;
  b.className = r && r.ok ? 'item sent' : 'item failed';
  meta.textContent = r && r.ok ? t('sent') : t('sendFailed');
  if (!(r && r.ok)) b.title = `${m.url}\n${(r && r.error) || ''}`;
}

async function init() {
  localize();
  [tab] = await api.tabs.query({ active: true, currentWindow: true });
  $('paused').addEventListener('change', (e) => send({ cmd: 'setPaused', value: e.target.checked }).then(() => refresh()));
  $('excluded').addEventListener('change', (e) =>
    send({ cmd: 'setExcluded', host: current && current.host, value: e.target.checked }).then(() => refresh()),
  );
  $('start').addEventListener('click', async (e) => {
    e.target.disabled = true;
    await send({ cmd: 'connect' }).catch(() => {});
    e.target.disabled = false;
    refresh();
  });
  // Arka plan durum değişince haber verir
  api.runtime.onMessage.addListener((msg) => {
    if (msg && msg.event === 'state') refresh();
  });
  await refresh(true);
}

init();
