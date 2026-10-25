// Ortak yardımcılar: Tauri köprüsü, çeviri, biçimlendirme.
const TAURI = window.__TAURI__;
const invoke = (cmd, args) => TAURI.core.invoke(cmd, args);
const listen = (ev, fn) => TAURI.event.listen(ev, (e) => fn(e.payload));
const currentWindow = () => TAURI.window.getCurrentWindow();
const $ = (id) => document.getElementById(id);

const IS_MAC = /Mac/.test(navigator.platform || navigator.userAgent);
if (IS_MAC) document.documentElement.classList.add('mac');

let I18N = {};
const T = (key) => I18N[key] ?? key;
const TF = (key, ...args) => args.reduce((s, a, i) => s.replaceAll(`{${i}}`, a), T(key));

async function loadI18n() {
  I18N = await invoke('i18n_table');
  applyI18n();
}

function applyI18n(root = document) {
  root.querySelectorAll('[data-i18n]').forEach((e) => { e.textContent = T(e.dataset.i18n); });
  root.querySelectorAll('[data-i18n-title]').forEach((e) => { e.title = T(e.dataset.i18nTitle); });
  root.querySelectorAll('[data-i18n-ph]').forEach((e) => { e.placeholder = T(e.dataset.i18nPh); });
}

listen('i18n', (table) => { I18N = table; applyI18n(); if (window.onI18n) window.onI18n(); });

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') e.className = v;
    else if (k === 'html') e.innerHTML = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (v !== false && v != null) e.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) if (c != null) e.append(c);
  return e;
}

// ---------- biçimlendirme ----------
function fmtSize(n) {
  if (n == null) return '—';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${i === 0 ? v : v.toFixed(v < 10 ? 2 : v < 100 ? 1 : 0)} ${u[i]}`;
}

const fmtSpeed = (bps) => (bps ? `${fmtSize(bps)}/s` : '');

function fmtEta(s) {
  if (s == null) return '';
  if (s < 60) return `${s} ${T('unit.s')}`;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h ? `${h} ${T('unit.h')} ${m} ${T('unit.m')}` : `${m} ${T('unit.m')} ${s % 60} ${T('unit.s')}`;
}

function fmtDate(ts) {
  if (!ts) return '';
  const d = new Date(ts * 1000);
  const now = new Date();
  const same = d.toDateString() === now.toDateString();
  return same ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : d.toLocaleDateString();
}

// ---------- ikonlar ----------
const ICONS = {
  add: '<path d="M12 5v14M5 12h14"/>',
  play: '<path d="M7 4.5v15l13-7.5z" fill="currentColor" stroke="none"/>',
  pause: '<rect x="6" y="5" width="4" height="14" rx="1" fill="currentColor" stroke="none"/><rect x="14" y="5" width="4" height="14" rx="1" fill="currentColor" stroke="none"/>',
  stop: '<rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor" stroke="none"/>',
  trash: '<path d="M4 7h16M10 11v6M14 11v6M5 7l1 13h12l1-13M9 7V4h6v3"/>',
  queue: '<path d="M4 6h16M4 12h16M4 18h10"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  folder: '<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2h8.5A1.5 1.5 0 0 1 21 8.5v9A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5z"/>',
  all: '<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>',
  compressed: '<rect x="4" y="3" width="16" height="18" rx="2"/><path d="M12 3v4M12 9v2M12 13v2"/>',
  documents: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4M9 12h6M9 16h6"/>',
  music: '<path d="M9 18V6l11-2v12"/><circle cx="6" cy="18" r="3"/><circle cx="17" cy="16" r="3"/>',
  programs: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18"/>',
  video: '<rect x="3" y="6" width="13" height="12" rx="2"/><path d="M16 10l5-3v10l-5-3z"/>',
  general: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/>',
  downloading: '<path d="M12 4v12M7 11l5 5 5-5M5 20h14"/>',
  completed: '<path d="M5 12l5 5 9-10"/>',
  unfinished: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5"/>',
  error: '<circle cx="12" cy="12" r="9"/><path d="M12 7v6M12 16.5h.01"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
  help: '<circle cx="12" cy="12" r="9"/><path d="M9.5 9a2.5 2.5 0 1 1 3.5 2.3c-.6.3-1 .8-1 1.5V14M12 17.5h.01"/>',
  speed: '<path d="M12 14l4-6"/><path d="M4 18a8 8 0 1 1 16 0"/>',
  clipboard: '<rect x="6" y="4" width="12" height="17" rx="2"/><path d="M9 4h6v3H9z"/>',
  grabber: '<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4M8 11h6M11 8v6"/>',
};

function icon(name) {
  return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${ICONS[name] || ICONS.general}</svg>`;
}

// Esc ile diyalog/tercih pencerelerini kapat
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && document.body.dataset.escClose) currentWindow().close();
});
