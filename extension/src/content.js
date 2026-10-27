// Sayfadaki video oynatıcılarının üstünde "Bu videoyu indir" düğmesi. Düğme sayfadan yalıtılmış bir gölge
// DOM'da çizilir; sayfanın stilleri ona, onunkiler sayfaya dokunmaz. Video adresleri ağdan yakalananlardır
// (arka plan, sekme başına); oynatıcının doğrudan adresi de (blob: değilse) eklenir. Gönderme yalnız gerçek
// kullanıcı tıklamasıyla olur: sayfanın ürettiği yapay olaylar (isTrusted=false) yok sayılır.
(() => {
  if (globalThis.__karadelikVideoButton) return;
  globalThis.__karadelikVideoButton = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const t = (key) => api.i18n.getMessage(key) || key;
  // lib.js'teki VIDEO_BUTTON_MIN ile aynı (içerik betiği modül olamaz)
  const MIN_W = 200;
  const MIN_H = 110;
  // Oynatma başlayınca düğme bu kadar görünür kalır; sonra yalnız fare üstündeyken
  const SHOW_AFTER_PLAY_MS = 4000;
  const HIDE_DELAY_MS = 1200;

  let enabled = false;
  const videos = new Set();
  const dismissed = new WeakSet();
  let target = null;
  let hideTimer = 0;
  let playUntil = 0;
  let raf = 0;
  let host = null;
  let ui = null;

  const send = (msg) => api.runtime.sendMessage(msg).catch((e) => ({ ok: false, error: e && e.message }));

  // ------------------------------------------------------------ çizim

  const CSS = `
    :host { all: initial; }
    .wrap { position: fixed; z-index: 2147483647; display: none; flex-direction: column; align-items: flex-end; gap: 6px;
      font: 500 13px/1.3 system-ui, -apple-system, "Segoe UI", sans-serif; color: #f4f2fa; pointer-events: auto; }
    .wrap.show { display: flex; }
    .pill { display: flex; align-items: stretch; border-radius: 999px; overflow: hidden; background: rgba(22, 20, 30, .88);
      backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px); box-shadow: 0 4px 18px rgba(0, 0, 0, .45), inset 0 0 0 1px rgba(255, 255, 255, .1); }
    button { all: unset; cursor: pointer; box-sizing: border-box; }
    .dl { display: flex; align-items: center; gap: 7px; padding: 7px 12px 7px 9px; }
    .dl:hover, .x:hover { background: rgba(255, 255, 255, .08); }
    .dl .ic { width: 18px; height: 18px; border-radius: 50%; background: #ff8a1f; display: grid; place-items: center; flex-shrink: 0; }
    .dl .ic svg { width: 11px; height: 11px; }
    .x { display: grid; place-items: center; width: 28px; color: #a9a6b8; border-left: 1px solid rgba(255, 255, 255, .1); font-size: 15px; }
    .menu { display: none; min-width: 260px; max-width: 380px; max-height: 300px; overflow: auto; padding: 5px; border-radius: 12px;
      background: rgba(22, 20, 30, .96); box-shadow: 0 10px 30px rgba(0, 0, 0, .5), inset 0 0 0 1px rgba(255, 255, 255, .1); }
    .menu.open { display: block; }
    .item { display: flex; align-items: center; gap: 9px; width: 100%; padding: 7px 9px; border-radius: 8px; }
    .item:hover { background: rgba(255, 138, 31, .16); }
    .kind { flex-shrink: 0; min-width: 38px; padding: 2px 6px; border-radius: 5px; background: rgba(255, 255, 255, .08); color: #ffb36b;
      font-size: 10.5px; font-weight: 700; text-transform: uppercase; text-align: center; }
    .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .meta { flex-shrink: 0; color: #a9a6b8; font-size: 12px; font-variant-numeric: tabular-nums; }
    .note { padding: 8px 9px; color: #c9c6d6; }
    .note.small { padding: 4px 9px 6px; color: #a9a6b8; font-size: 11.5px; }
    .sep { height: 1px; margin: 4px 2px; background: rgba(255, 255, 255, .1); }
    .foot { width: 100%; padding: 6px 9px; border-radius: 8px; color: #a9a6b8; font-size: 12px; }
    .foot:hover { background: rgba(255, 255, 255, .06); color: #f4f2fa; }
    .ok { color: #3ddc84; }
    .bad { color: #ff8a8a; }
  `;
  const ARROW = '<svg viewBox="0 0 16 16" fill="none" stroke="#1a0d00" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2.5v9M4 7.5l4 4 4-4"/></svg>';

  function build() {
    host = document.createElement('karadelik-video-button');
    // Sayfa stilleri dış öğeyi gizleyemesin
    host.style.setProperty('display', 'block', 'important');
    host.style.setProperty('position', 'static', 'important');
    const root = host.attachShadow({ mode: 'closed' });
    const style = document.createElement('style');
    style.textContent = CSS;
    const wrap = document.createElement('div');
    wrap.className = 'wrap';
    const pill = document.createElement('div');
    pill.className = 'pill';
    const dl = document.createElement('button');
    dl.className = 'dl';
    dl.innerHTML = `<span class="ic">${ARROW}</span>`;
    const label = document.createElement('span');
    label.textContent = t('vbDownload');
    dl.append(label);
    const x = document.createElement('button');
    x.className = 'x';
    x.textContent = '×';
    x.title = t('vbClose');
    pill.append(dl, x);
    const menu = document.createElement('div');
    menu.className = 'menu';
    wrap.append(pill, menu);
    root.append(style, wrap);
    // Tıklamalar sayfaya (oynatıcıya) ulaşmasın: düğmeye basmak videoyu duraklatmamalı
    for (const ev of ['click', 'mousedown', 'mouseup', 'pointerdown', 'pointerup', 'dblclick', 'contextmenu']) {
      wrap.addEventListener(ev, (e) => e.stopPropagation());
    }
    wrap.addEventListener('mouseenter', () => clearTimeout(hideTimer));
    wrap.addEventListener('mouseleave', () => scheduleHide());
    dl.addEventListener('click', (e) => {
      if (!e.isTrusted) return;
      if (menu.classList.contains('open')) closeMenu();
      else openMenu();
    });
    x.addEventListener('click', (e) => {
      if (!e.isTrusted) return;
      if (target) dismissed.add(target);
      hide();
    });
    ui = { wrap, menu };
    mount();
  }

  // Tam ekranda yalnız tam ekran öğesinin içi görünür; düğme oraya taşınır
  function mount() {
    if (!host) return;
    const fs = document.fullscreenElement || document.webkitFullscreenElement;
    const parent = fs && !(fs instanceof HTMLVideoElement) ? fs : document.documentElement;
    if (host.parentNode !== parent) parent.append(host);
  }

  function place() {
    raf = 0;
    if (!target || !ui || !ui.wrap.classList.contains('show')) return;
    const r = target.getBoundingClientRect();
    if (!target.isConnected || !visibleRect(target, r)) {
      hide();
      return;
    }
    ui.wrap.style.top = `${Math.max(0, r.top) + 10}px`;
    ui.wrap.style.right = `${Math.max(0, document.documentElement.clientWidth - Math.min(r.right, document.documentElement.clientWidth)) + 10}px`;
    raf = requestAnimationFrame(place);
  }

  function visibleRect(v, r) {
    if (r.width < MIN_W || r.height < MIN_H) return false;
    if (r.bottom <= 0 || r.right <= 0 || r.top >= innerHeight || r.left >= innerWidth) return false;
    const cs = getComputedStyle(v);
    return cs.visibility !== 'hidden' && cs.display !== 'none' && Number(cs.opacity) > 0.05;
  }

  function show(v) {
    if (!enabled || dismissed.has(v)) return;
    if (!host) build();
    mount();
    clearTimeout(hideTimer);
    if (target !== v) closeMenu();
    target = v;
    ui.wrap.classList.add('show');
    if (!raf) raf = requestAnimationFrame(place);
  }

  function hide() {
    clearTimeout(hideTimer);
    if (!ui) return;
    ui.wrap.classList.remove('show');
    closeMenu();
    target = null;
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (ui && ui.menu.classList.contains('open')) return;
      if (target && !target.paused && Date.now() < playUntil) return scheduleHide();
      hide();
    }, HIDE_DELAY_MS);
  }

  // ------------------------------------------------------------ menü

  function closeMenu() {
    if (ui) ui.menu.classList.remove('open');
  }

  function formatSize(n) {
    if (typeof n !== 'number' || !(n > 0)) return '';
    const u = ['B', 'KB', 'MB', 'GB'];
    let i = 0;
    while (n >= 1024 && i < u.length - 1) {
      n /= 1024;
      i++;
    }
    return `${n.toFixed(n >= 10 || i === 0 ? 0 : 1)} ${u[i]}`;
  }

  function nameOf(url) {
    try {
      const u = new URL(url);
      const seg = decodeURIComponent(u.pathname.split('/').pop() || '');
      return seg || u.host;
    } catch {
      return url;
    }
  }

  async function openMenu() {
    const menu = ui.menu;
    menu.replaceChildren(note(t('vbLoading')));
    menu.classList.add('open');
    const src = target && (target.currentSrc || target.src || '');
    const r = await send({ cmd: 'vbList', src });
    menu.replaceChildren();
    const items = (r && r.items) || [];
    if (!items.length) menu.append(note(t('vbNoMedia')));
    if (items.some((m) => m.kind !== 'file')) menu.append(note(t('vbStreamHint'), 'small'));
    for (const m of items) {
      const b = document.createElement('button');
      b.className = 'item';
      b.title = m.url;
      const kind = document.createElement('span');
      kind.className = 'kind';
      kind.textContent = m.kind === 'file' ? (m.mime.split('/')[1] || t('kindFile')).replace(/^x-/, '').slice(0, 5) : m.kind;
      const name = document.createElement('span');
      name.className = 'name';
      name.textContent = nameOf(m.url);
      const meta = document.createElement('span');
      meta.className = 'meta';
      meta.textContent = formatSize(m.size);
      b.append(kind, name, meta);
      b.addEventListener('click', async (e) => {
        if (!e.isTrusted || b.disabled) return;
        b.disabled = true;
        meta.textContent = '…';
        const res = await send({ cmd: 'vbSend', url: m.url, kind: m.kind });
        const ok = res && res.ok;
        meta.className = `meta ${ok ? 'ok' : 'bad'}`;
        meta.textContent = ok ? t('sent') : t('sendFailed');
        b.disabled = false;
        if (ok) setTimeout(() => { closeMenu(); scheduleHide(); }, 900);
      });
      menu.append(b);
    }
    const sep = document.createElement('div');
    sep.className = 'sep';
    const hideSite = document.createElement('button');
    hideSite.className = 'foot';
    hideSite.textContent = t('vbHideSite');
    hideSite.addEventListener('click', async (e) => {
      if (!e.isTrusted) return;
      await send({ cmd: 'vbHideSite' });
      hide();
    });
    menu.append(sep, hideSite);
  }

  function note(text, extra = '') {
    const d = document.createElement('div');
    d.className = `note ${extra}`.trim();
    d.textContent = text;
    return d;
  }

  // ------------------------------------------------------------ videoları izleme

  function scan() {
    for (const v of document.getElementsByTagName('video')) videos.add(v);
    for (const v of videos) if (!v.isConnected) videos.delete(v);
  }

  let scanQueued = false;
  new MutationObserver(() => {
    if (scanQueued) return;
    scanQueued = true;
    requestAnimationFrame(() => {
      scanQueued = false;
      scan();
    });
  }).observe(document.documentElement, { childList: true, subtree: true });

  // Oynatıcılar videonun üstüne şeffaf katmanlar koyar; fare olayı videoya ulaşmaz. Bu yüzden konum denetlenir.
  let lastMove = 0;
  document.addEventListener(
    'mousemove',
    (e) => {
      if (!enabled || !videos.size) return;
      const now = Date.now();
      if (now - lastMove < 100) return;
      lastMove = now;
      let hit = null;
      for (const v of videos) {
        const r = v.getBoundingClientRect();
        if (e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom && visibleRect(v, r)) {
          // Üst üste binen videolarda en küçüğü (gömülü önizleme değil, asıl oynatıcı genelde tek)
          if (!hit || r.width * r.height < hit.r.width * hit.r.height) hit = { v, r };
        }
      }
      if (hit) show(hit.v);
      else if (target) scheduleHide();
    },
    { passive: true, capture: true },
  );

  // Medya olayları kabarcıklanmaz; yakalama aşamasında dinlenir
  document.addEventListener(
    'play',
    (e) => {
      const v = e.target;
      if (!(v instanceof HTMLVideoElement)) return;
      videos.add(v);
      const r = v.getBoundingClientRect();
      if (!visibleRect(v, r)) return;
      playUntil = Date.now() + SHOW_AFTER_PLAY_MS;
      show(v);
      scheduleHide();
    },
    true,
  );

  for (const ev of ['fullscreenchange', 'webkitfullscreenchange']) document.addEventListener(ev, () => mount());
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeMenu(); }, true);

  // ------------------------------------------------------------ açma / kapama

  async function refreshEnabled() {
    const r = await send({ cmd: 'vbState' });
    enabled = !!(r && r.on);
    if (!enabled) hide();
  }

  api.storage.onChanged.addListener((changes, area) => {
    if (area === 'local' && (changes.vbOff || changes.vbHidden)) refreshEnabled();
  });

  scan();
  refreshEnabled();
})();
