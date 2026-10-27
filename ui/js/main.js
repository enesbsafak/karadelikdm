// Ana pencere: kenar çubuğu (kategori/kuyruk), durum bölmesi, indirme listesi, ayrıntı paneli, durum satırı.
let state = { downloads: [], queues: [], categories: [] };
// Kenar çubuğu seçimi (tümü / kategori / kuyruk) ve durum bölmesi birlikte süzer
let filter = { kind: 'all', value: '' };
let view = 'all';
let sort = { key: 'added', dir: -1 };
let selection = new Set();
let anchor = null;
let search = '';
const rowMap = new Map();

const STATUS_ORDER = { downloading: 0, queued: 1, stopped: 2, error: 3, completed: 4 };
const SORT_KEYS = ['added', 'name', 'size', 'progress', 'speed', 'eta', 'status'];
const SORT_LABEL = { added: 'col.added', name: 'col.name', size: 'col.size', progress: 'col.progress', speed: 'col.speed', eta: 'col.eta', status: 'col.status' };

function catOf(d) { return d.category || 'general'; }

function inView(d) {
  switch (view) {
    case 'unfinished': return d.status !== 'completed';
    case 'completed': return d.status === 'completed';
    case 'error': return d.status === 'error';
    default: return true;
  }
}

function matches(d) {
  if (search && !d.file_name.toLowerCase().includes(search) && !d.url.toLowerCase().includes(search)) return false;
  if (!inView(d)) return false;
  switch (filter.kind) {
    case 'cat': return catOf(d) === filter.value;
    case 'queue': return d.queue === filter.value;
    default: return true;
  }
}

function progressOf(d) {
  if (d.status === 'completed') return 1;
  if (d.parts && d.parts[1]) return d.parts[0] / d.parts[1];
  return d.size ? d.done / d.size : 0;
}

function sortValue(d, key) {
  switch (key) {
    case 'name': return d.file_name.toLowerCase();
    case 'size': return d.size ?? -1;
    case 'progress': return progressOf(d);
    case 'speed': return d.speed;
    case 'eta': return d.eta ?? Number.MAX_SAFE_INTEGER;
    case 'status': return STATUS_ORDER[d.status] ?? 9;
    default: return d.added * 1e6 + d.id;
  }
}

function visible() {
  const list = state.downloads.filter(matches);
  list.sort((a, b) => {
    const x = sortValue(a, sort.key);
    const y = sortValue(b, sort.key);
    return (x < y ? -1 : x > y ? 1 : 0) * sort.dir;
  });
  return list;
}

function statusText(d) {
  const t = d.torrent;
  if (t && d.status === 'downloading' && t.resolving) return T('st.t_resolving');
  if (t && d.status === 'downloading' && t.checking) return T('st.t_checking');
  if (t && d.status === 'completed' && t.seeding) return T('st.seeding');
  if (t && d.status === 'completed' && t.checking) return T('st.t_checking');
  if (d.status === 'downloading') return T('st.downloading');
  if (d.status === 'error') return T('st.error');
  if (d.status === 'queued') return T('st.queued');
  if (d.status === 'completed') return T('st.completed');
  return d.done > 0 ? T('st.paused') : T('st.stopped');
}

/** Satırın ikinci satırı: boyut ve ilerleme bilgisi. */
function metaText(d) {
  const parts = [];
  if (d.status === 'completed') {
    parts.push(fmtSize(d.size));
  } else if (d.parts && d.parts[1]) {
    parts.push(`${fmtSize(d.done)} · ${TF('det.stream_parts', d.parts[0], d.parts[1])}`);
  } else {
    parts.push(d.size ? `${fmtSize(d.done)} / ${fmtSize(d.size)}` : fmtSize(d.done));
  }
  if (d.torrent && (d.status === 'downloading' || d.torrent.seeding)) parts.push(TF('row.peers', d.torrent.peers));
  if (d.media) parts.push('HLS / DASH');
  return parts.join(' · ');
}

/** Sağ sütun: üstte hız ya da durum, altta kalan süre ya da tarih. */
function sideText(d) {
  if (d.status === 'downloading') {
    const top = d.torrent && (d.torrent.resolving || d.torrent.checking) ? statusText(d) : (fmtSpeed(d.speed) ? `↓ ${fmtSpeed(d.speed)}` : statusText(d));
    return [top, d.eta != null ? TF('row.left', fmtEta(d.eta)) : ''];
  }
  if (d.status === 'completed' && d.torrent && d.torrent.seeding) return [statusText(d), `↑ ${fmtSpeed(d.torrent.upload_speed) || '0 B/s'}`];
  if (d.status === 'completed') return [statusText(d), fmtDate(d.finished || d.added)];
  return [statusText(d), fmtDate(d.added)];
}

// ---------- kenar çubuğu ----------
function renderSidebar() {
  const nav = $('sidebar');
  nav.textContent = '';
  const count = (fn) => state.downloads.filter((d) => inView(d) && fn(d)).length;
  const item = (kind, value, label, ico, n, extra) => {
    const active = filter.kind === kind && filter.value === value;
    const e = el('button', { class: `side-item${active ? ' active' : ''}`, type: 'button', onclick: () => { filter = { kind, value }; renderAll(); } });
    if (extra) e.append(extra); else e.insertAdjacentHTML('beforeend', icon(ico));
    e.append(el('span', { class: 'label' }, label), el('span', { class: 'count' }, n ? String(n) : ''));
    return e;
  };
  nav.append(el('div', { class: 'side-title' }, T('side.categories')));
  nav.append(item('all', '', T('side.all'), 'all', count(() => true)));
  for (const c of state.categories) nav.append(item('cat', c.id, c.name, c.id, count((d) => catOf(d) === c.id)));
  nav.append(el('div', { class: 'side-title' }, T('side.queues')));
  for (const q of state.queues) {
    nav.append(item('queue', q.id, q.name, null, count((d) => d.queue === q.id), el('span', { class: `qdot${q.active ? ' on' : ''}` })));
  }
}

function listTitle() {
  if (filter.kind === 'cat') return (state.categories.find((c) => c.id === filter.value) || {}).name || '';
  if (filter.kind === 'queue') return (state.queues.find((q) => q.id === filter.value) || {}).name || '';
  return T('side.all');
}

function renderHead() {
  $('listTitle').textContent = listTitle();
  document.querySelectorAll('#viewSeg [data-view]').forEach((b) => {
    const on = b.dataset.view === view;
    b.classList.toggle('on', on);
    b.setAttribute('aria-selected', on ? 'true' : 'false');
  });
  const sel = $('sortSel');
  if (sel.options.length !== SORT_KEYS.length) {
    sel.textContent = '';
    for (const k of SORT_KEYS) sel.append(el('option', { value: k }, T(SORT_LABEL[k])));
  } else {
    [...sel.options].forEach((o) => { o.textContent = T(SORT_LABEL[o.value]); });
  }
  sel.value = sort.key;
  $('sortDir').innerHTML = icon(sort.dir < 0 ? 'sort_desc' : 'sort_asc');
}

// ---------- liste ----------
function makeRow(d) {
  const row = el('div', { class: 'dlrow', role: 'row', tabindex: '-1' });
  row.dataset.id = d.id;
  const tile = el('div', { class: 'ftile' });
  const name = el('div', { class: 'fname' });
  const meta = el('div', { class: 'fmeta' });
  const bar = el('div', { class: 'pbar' }, el('div'));
  const main = el('div', { class: 'fmain' }, el('div', { class: 'fline' }, name, meta), bar);
  const top = el('span', { class: 'top' });
  const sub = el('span', { class: 'sub' });
  const side = el('div', { class: 'fside' }, top, sub);
  const btn = el('button', { class: 'row-act', type: 'button' });
  btn.addEventListener('mousedown', (e) => e.stopPropagation());
  btn.addEventListener('click', (e) => { e.stopPropagation(); openItem(d.id); });
  row.append(tile, main, side, btn);
  row.addEventListener('mousedown', (e) => onRowClick(e, d.id));
  row.addEventListener('dblclick', () => openItem(d.id));
  row.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (!selection.has(d.id)) { selection = new Set([d.id]); anchor = d.id; renderSelection(); }
    showContextMenu(e.clientX, e.clientY);
  });
  const r = { row, tile, name, meta, bar, top, sub, btn, key: '' };
  rowMap.set(d.id, r);
  return r;
}

function tileKind(d) {
  if (d.torrent) return 'torrent';
  if (d.media) return 'stream';
  return catOf(d);
}

function updateRow(r, d) {
  const p = progressOf(d);
  const key = [d.file_name, d.size, d.done, d.speed, d.eta, d.status, d.category, d.parts, d.error, I18N['st.queued'], d.torrent ? JSON.stringify(d.torrent) : ''].join('|');
  if (r.key === key) return;
  r.key = key;
  const kind = tileKind(d);
  if (r.tile.dataset.kind !== kind) {
    r.tile.dataset.kind = kind;
    r.tile.innerHTML = icon(kind);
  }
  r.row.title = d.url;
  r.name.textContent = d.file_name;
  r.meta.textContent = d.status === 'error' && d.error ? d.error : metaText(d);
  r.meta.classList.toggle('err', d.status === 'error');
  r.bar.firstChild.style.width = `${(p * 100).toFixed(1)}%`;
  const unknown = d.status === 'downloading' && !d.size && !(d.parts && d.parts[1]);
  r.bar.className = `pbar ${d.status}${unknown ? ' unknown' : ''}${d.torrent && d.torrent.seeding ? ' seeding' : ''}`;
  const [top, sub] = sideText(d);
  r.top.textContent = top;
  r.top.className = `top st ${d.status}${d.torrent && d.torrent.seeding ? ' seeding' : ''}`;
  r.sub.textContent = sub;
  r.top.title = d.error || '';
  const act = d.status === 'completed' ? ['open', 'ctx.open'] : d.status === 'downloading' || d.status === 'queued' ? ['pause', 'ctx.stop'] : d.status === 'error' ? ['retry', 'ctx.start'] : ['play', 'ctx.start'];
  r.btn.innerHTML = icon(act[0]);
  r.btn.title = T(act[1]);
  r.btn.setAttribute('aria-label', T(act[1]));
}

function renderList() {
  const list = visible();
  const box = $('rows');
  const seen = new Set();
  for (const d of list) {
    const r = rowMap.get(d.id) || makeRow(d);
    updateRow(r, d);
    seen.add(d.id);
    box.append(r.row); // var olan düğümü sıraya taşır
  }
  for (const [id, r] of rowMap) {
    if (!seen.has(id)) { r.row.remove(); if (!state.downloads.some((d) => d.id === id)) rowMap.delete(id); }
  }
  for (const id of [...selection]) if (!state.downloads.some((d) => d.id === id)) selection.delete(id);
  $('emptyList').classList.toggle('hidden', state.downloads.length > 0);
  renderHead();
}

function renderSelection() {
  for (const [id, r] of rowMap) {
    r.row.classList.toggle('sel', selection.has(id));
    r.row.setAttribute('aria-selected', selection.has(id) ? 'true' : 'false');
  }
  renderDetails();
  renderToolbar();
}

function onRowClick(e, id) {
  if (e.button === 2 && selection.has(id)) return;
  const list = visible().map((d) => d.id);
  if (e.shiftKey && anchor != null) {
    const a = list.indexOf(anchor);
    const b = list.indexOf(id);
    const [x, y] = a < b ? [a, b] : [b, a];
    if (!(e.ctrlKey || e.metaKey)) selection.clear();
    list.slice(x, y + 1).forEach((i) => selection.add(i));
  } else if (e.ctrlKey || e.metaKey) {
    if (selection.has(id)) selection.delete(id); else selection.add(id);
    anchor = id;
  } else {
    selection = new Set([id]);
    anchor = id;
  }
  renderSelection();
}

const selected = () => state.downloads.filter((d) => selection.has(d.id));

// ---------- ayrıntılar (sağ panel) ----------
function ring(p, status) {
  const r = 31;
  const c = 2 * Math.PI * r;
  const color = status === 'completed' ? 'var(--ok)' : status === 'error' ? 'var(--err)' : 'var(--accent)';
  return `<svg viewBox="0 0 72 72" width="72" height="72" aria-hidden="true"><circle cx="36" cy="36" r="${r}" fill="none" stroke="var(--line)" stroke-width="6"/>`
    + `<circle cx="36" cy="36" r="${r}" fill="none" stroke="${color}" stroke-width="6" stroke-linecap="round" stroke-dasharray="${(c * p).toFixed(1)} ${c.toFixed(1)}" transform="rotate(-90 36 36)"/></svg>`;
}

function kindText(d) {
  if (d.torrent) return 'BitTorrent';
  if (d.media) return 'HLS / DASH';
  const scheme = (d.url.split(':')[0] || '').toUpperCase();
  return scheme === 'FTPS' || scheme === 'FTP' || scheme === 'HTTPS' || scheme === 'HTTP' ? scheme : '';
}

function renderDetails() {
  const box = $('details');
  const sel = selected();
  if (sel.length !== 1) { box.classList.add('hidden'); return; }
  const d = sel[0];
  box.classList.remove('hidden');
  box.textContent = '';
  const tor = d.torrent;
  const p = progressOf(d);
  const range = tor ? T('range.pieces') : d.media ? T('range.segments') : { yes: T('range.yes'), no: T('range.no'), probable: T('range.probable'), unknown: T('range.unknown') }[d.range];
  const q = state.queues.find((x) => x.id === d.queue);
  const sep = IS_MAC ? '/' : '\\';

  const ringBox = el('div', { class: 'ring' }, el('div', { html: ring(p, d.status) }), el('span', {}, `${Math.round(p * 100)}%`));
  const head = el('div', { class: 'insp-head' },
    ringBox,
    el('strong', { title: d.file_name }, d.file_name),
    el('span', { class: 'kind' }, [kindText(d), statusText(d)].filter(Boolean).join(' · ')),
  );

  const rows = [];
  const kv = (k, v, opts = {}) => { rows.push(el('dt', {}, k), el('dd', opts, v)); };
  kv(T('det.size'), d.parts && d.parts[1] ? `${fmtSize(d.done)} · ${TF('det.stream_parts', d.parts[0], d.parts[1])}` : `${fmtSize(d.done)} / ${fmtSize(d.size)}`);
  if (tor) {
    kv(T('det.speed'), d.status === 'downloading' || tor.seeding ? `↓ ${fmtSpeed(d.speed) || '0 B/s'} · ↑ ${fmtSpeed(tor.upload_speed) || '0 B/s'}` : '—');
    kv(T('det.peers_label'), TF('det.peers', tor.peers, tor.seen));
    kv(T('det.uploaded'), TF('det.ratio', fmtSize(tor.uploaded), d.size ? (tor.uploaded / d.size).toFixed(2) : '0.00'));
  } else {
    kv(T('det.speed'), d.status === 'downloading' ? `${fmtSpeed(d.speed) || '0 B/s'} · ${TF('det.connections', d.connections)}` : '—');
  }
  if (d.status === 'downloading' && d.eta != null) kv(T('col.eta'), fmtEta(d.eta));
  kv(T('det.resume'), range);
  kv(T('det.queue'), q ? q.name : '—');
  kv(T('det.saved_to'), `${d.dir}${sep}${d.file_name}`, { title: `${d.dir}${sep}${d.file_name}`, class: 'path' });
  kv(T('det.url'), d.url, { title: d.url, class: 'path' });
  if (tor) kv(T('det.info_hash'), tor.info_hash, { class: 'mono' });

  box.append(head, el('dl', { class: 'kv' }, rows));
  if (d.error) box.append(el('div', { class: d.status === 'error' ? 'insp-err' : 'insp-note' }, d.error));
  if (d.size && d.status !== 'completed' && d.segments.length) {
    const bar = el('div', { class: 'segbar', title: T('det.segments_hint') });
    for (const [start, , done] of d.segments) {
      bar.append(el('div', { style: `left:${(start / d.size) * 100}%;width:${(done / d.size) * 100}%` }));
      if (start > 0) bar.append(el('i', { style: `left:${(start / d.size) * 100}%` }));
    }
    box.append(bar, el('div', { class: 'seglegend' }, TF('det.segments', d.segments.length)));
  }
  const actions = el('div', { class: 'insp-actions' }, el('button', { class: 'btn', type: 'button', onclick: () => act('folder') }, T('ctx.folder')));
  if (tor && d.status === 'completed') {
    actions.append(tor.seeding
      ? el('button', { class: 'btn', type: 'button', onclick: () => act('stop') }, T('ctx.seed_stop'))
      : el('button', { class: 'btn', type: 'button', disabled: tor.checking, onclick: () => act('seed') }, T('ctx.seed')));
  }
  if (d.status === 'completed') actions.append(el('button', { class: 'btn primary', type: 'button', onclick: () => act('open') }, T('ctx.open')));
  else if (d.status === 'downloading' || d.status === 'queued') actions.append(el('button', { class: 'btn primary', type: 'button', onclick: () => act('stop') }, T('ctx.stop')));
  else actions.append(el('button', { class: 'btn primary', type: 'button', onclick: () => act('start') }, T('ctx.start')));
  box.append(actions);
}

// ---------- araç çubuğu ----------
function renderToolbar() {
  const sel = selected();
  const canStart = sel.some((d) => d.status !== 'completed' && d.status !== 'downloading');
  const canStop = sel.some((d) => d.status === 'downloading' || d.status === 'queued' || (d.torrent && d.torrent.seeding));
  $('resumeBtn').disabled = !canStart;
  $('stopBtn').disabled = !canStop;
  $('removeBtn').disabled = sel.length === 0;
  $('stopAllBtn').disabled = !state.downloads.some((d) => d.status === 'downloading' || d.status === 'queued');
  const main = state.queues.find((q) => q.id === 'main');
  const qLabel = main && main.active ? T('tb.stop_queue') : T('tb.start_queue');
  $('queueBtn').title = qLabel;
  $('queueBtn').setAttribute('aria-label', qLabel);
  $('queueBtn').classList.toggle('on', !!(main && main.active));
}

function renderStatus() {
  const active = state.downloads.filter((d) => d.status === 'downloading');
  const speed = active.reduce((s, d) => s + d.speed, 0);
  const up = state.downloads.reduce((s, d) => s + (d.torrent ? d.torrent.upload_speed : 0), 0);
  $('total').textContent = active.length || up
    ? [active.length ? TF('ui.active', active.length) : '', fmtSpeed(speed) ? `↓ ${fmtSpeed(speed)}` : '', up ? `↑ ${fmtSpeed(up)}` : ''].filter(Boolean).join(' · ')
    : '';
  const lim = $('limitChip');
  lim.innerHTML = `${icon('speed')}`;
  lim.append(document.createTextNode(state.speed_limit_enabled ? ` ${fmtSpeed(state.speed_limit)}` : ` ${T('ui.no_limit')}`));
  lim.classList.toggle('on', state.speed_limit_enabled);
  const clip = $('clipChip');
  clip.innerHTML = `${icon('clipboard')}`;
  clip.append(document.createTextNode(` ${T('ui.clipboard_short')}`));
  clip.classList.toggle('on', state.clipboard_monitor);
  clip.setAttribute('aria-pressed', state.clipboard_monitor ? 'true' : 'false');
  $('updateNotice').classList.toggle('hidden', !state.update);
  if (state.update) $('updateText').textContent = TF('ui.update_available', state.update);
  const q = state.quota_until;
  $('quotaNotice').classList.toggle('hidden', q == null);
  if (q != null) $('quotaText').textContent = q ? TF('ui.quota_until', new Date(q * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })) : T('ui.quota_paused');
}

function renderAll() {
  renderSidebar();
  renderList();
  renderSelection();
  renderStatus();
}

// ---------- eylemler ----------
async function act(action, ids = [...selection]) {
  if (!ids.length) return;
  try {
    await invoke('dl_action', { ids, action });
  } catch (e) {
    setStatus(String(e), true);
  }
}

function openItem(id) {
  const d = state.downloads.find((x) => x.id === id);
  if (!d) return;
  if (d.status === 'completed') act('open', [id]);
  else if (d.status === 'downloading' || d.status === 'queued') act('stop', [id]);
  else act('start', [id]);
}

function removeSelected(withFiles) {
  const ids = [...selection];
  if (!ids.length) return;
  if (withFiles) invoke('confirm_remove', { ids });
  else act('remove', ids);
}

function setStatus(msg, isError = false) {
  const s = $('status');
  s.textContent = msg || '';
  s.title = msg || '';
  s.classList.toggle('error', isError);
}

// ---------- sağ tık menüsü ----------
function hideContextMenu() { document.querySelectorAll('.ctx').forEach((m) => m.remove()); }

function menuItem(label, fn, opts = {}) {
  const it = el('div', { class: `it${opts.disabled ? ' dis' : ''}${opts.danger ? ' danger' : ''}` }, el('span', {}, label));
  if (opts.kbd) it.append(el('kbd', {}, opts.kbd));
  it.addEventListener('click', (e) => { e.stopPropagation(); hideContextMenu(); fn(); });
  return it;
}

function submenu(label, items) {
  const box = el('div', { class: 'ctx' }, items);
  const it = el('div', { class: 'it sub' }, el('span', {}, label), el('kbd', {}, '›'), box);
  return it;
}

function showContextMenu(x, y) {
  hideContextMenu();
  const sel = selected();
  if (!sel.length) return;
  const one = sel.length === 1 ? sel[0] : null;
  const any = (fn) => sel.some(fn);
  const sep = () => el('div', { class: 'sep' });
  const limits = [0, 256, 512, 1024, 5120].map((kb) =>
    menuItem(kb ? fmtSpeed(kb * 1024) : T('tray.unlimited'), () => act(`limit:${kb * 1024}`)));
  const queues = state.queues.map((q) => menuItem(q.name, () => act(`queue:${q.id}`)));
  const m = el('div', { class: 'ctx' },
    menuItem(T('ctx.open'), () => act('open'), { disabled: !(one && one.status === 'completed'), kbd: 'Enter' }),
    menuItem(T('ctx.folder'), () => act('folder'), { disabled: !one }),
    sep(),
    menuItem(T('ctx.start'), () => act('start'), { disabled: !any((d) => d.status !== 'completed' && d.status !== 'downloading') }),
    menuItem(T('ctx.stop'), () => act('stop'), { disabled: !any((d) => d.status === 'downloading' || d.status === 'queued') }),
    ...(any((d) => d.torrent && d.status === 'completed') ? [
      any((d) => d.torrent && d.torrent.seeding)
        ? menuItem(T('ctx.seed_stop'), () => act('stop'))
        : menuItem(T('ctx.seed'), () => act('seed'), { disabled: !any((d) => d.torrent && d.status === 'completed' && !d.torrent.checking) }),
    ] : []),
    menuItem(T('ctx.restart'), () => act('restart')),
    submenu(T('ctx.add_to_queue'), queues),
    submenu(T('ctx.speed_limit'), limits),
    sep(),
    menuItem(T('ctx.zip'), () => act('zip'), { disabled: !(one && one.status !== 'completed' && /\.zip$/i.test(one.file_name)) }),
    menuItem(T('ctx.refresh_url'), () => act('refresh_url'), { disabled: !(one && one.status !== 'completed') }),
    menuItem(T('ctx.copy_url'), () => act('copy_url'), { disabled: !one }),
    sep(),
    menuItem(T('ctx.remove'), () => removeSelected(false), { kbd: 'Del' }),
    menuItem(T('ctx.remove_files'), () => removeSelected(true), { danger: true, kbd: 'Shift+Del' }),
  );
  document.body.append(m);
  const r = m.getBoundingClientRect();
  m.style.left = `${Math.min(x, window.innerWidth - r.width - 6)}px`;
  m.style.top = `${Math.min(y, window.innerHeight - r.height - 6)}px`;
}

// ---------- veri ----------
async function refreshState() {
  state = await invoke('get_state');
  renderAll();
  if (state.message) setStatus(state.message);
}

window.onI18n = () => {
  for (const r of rowMap.values()) r.key = '';
  renderAll();
};

document.addEventListener('DOMContentLoaded', async () => {
  document.querySelectorAll('[data-icon]').forEach((s) => { s.outerHTML = icon(s.dataset.icon); });
  $('prefsBtn').innerHTML = icon('settings');
  $('aboutBtn').innerHTML = icon('help');
  await loadI18n();
  await refreshState();

  $('addBtn').addEventListener('click', () => invoke('run_action', { id: 'add_clipboard' }));
  $('resumeBtn').addEventListener('click', () => act('start'));
  $('stopBtn').addEventListener('click', () => act('stop'));
  $('stopAllBtn').addEventListener('click', () => invoke('run_action', { id: 'pause_all' }));
  $('removeBtn').addEventListener('click', () => removeSelected(false));
  $('queueBtn').addEventListener('click', () => {
    const main = state.queues.find((q) => q.id === 'main');
    invoke('run_action', { id: main && main.active ? 'queue_stop:main' : 'queue_start:main' }).then(() => setTimeout(refreshState, 300));
  });
  $('schedBtn').addEventListener('click', () => invoke('run_action', { id: 'queues' }));
  $('updateBtn').addEventListener('click', () => invoke('run_action', { id: 'app_update' }));
  $('grabBtn').addEventListener('click', () => invoke('run_action', { id: 'grabber' }));
  $('prefsBtn').addEventListener('click', () => invoke('run_action', { id: 'prefs' }));
  $('aboutBtn').addEventListener('click', () => invoke('run_action', { id: 'about' }));
  $('limitChip').addEventListener('click', () => invoke('run_action', { id: state.speed_limit_enabled ? 'limit:0' : 'limit_toggle' }));
  $('clipChip').addEventListener('click', () => invoke('run_action', { id: 'clipboard_toggle' }));
  $('minBtn').addEventListener('click', () => currentWindow().minimize());
  $('maxBtn').addEventListener('click', () => currentWindow().toggleMaximize());
  $('closeBtn').addEventListener('click', () => currentWindow().close());
  $('search').addEventListener('input', (e) => { search = e.target.value.trim().toLowerCase(); renderList(); });
  document.querySelectorAll('#viewSeg [data-view]').forEach((b) => b.addEventListener('click', () => {
    view = b.dataset.view;
    renderAll();
  }));
  $('sortSel').addEventListener('change', (e) => {
    sort = { key: e.target.value, dir: e.target.value === 'name' ? 1 : -1 };
    renderList();
  });
  $('sortDir').addEventListener('click', () => { sort = { key: sort.key, dir: -sort.dir }; renderList(); });
  $('list').addEventListener('mousedown', (e) => {
    if (e.target.closest('.dlrow')) return;
    selection.clear();
    renderSelection();
  });

  document.addEventListener('click', hideContextMenu);
  document.addEventListener('contextmenu', (e) => { if (!e.target.closest('input, textarea')) e.preventDefault(); });
  window.addEventListener('blur', hideContextMenu);

  document.addEventListener('keydown', (e) => {
    if (e.target.closest('input, textarea, select')) return;
    if (e.key === 'Delete') { e.preventDefault(); removeSelected(e.shiftKey); }
    else if (e.key === 'Enter' && selection.size === 1) openItem([...selection][0]);
    else if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && !e.altKey) {
      e.preventDefault();
      const list = visible().map((d) => d.id);
      if (!list.length) return;
      const i = anchor != null ? list.indexOf(anchor) : -1;
      const next = list[Math.max(0, Math.min(list.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))];
      selection = new Set([next]);
      anchor = next;
      renderSelection();
      const r = rowMap.get(next);
      if (r) r.row.scrollIntoView({ block: 'nearest' });
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      selection = new Set(visible().map((d) => d.id));
      renderSelection();
    } else if ((e.ctrlKey || e.metaKey) && (e.key.toLowerCase() === 'v' || e.key.toLowerCase() === 'n')) {
      e.preventDefault();
      invoke('run_action', { id: 'add_clipboard' });
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') {
      e.preventDefault();
      $('search').focus();
    }
  });

  // Tarayıcıdan bağlantı sürükle-bırak
  const app = $('app');
  app.addEventListener('dragover', (e) => { e.preventDefault(); app.classList.add('dropzone'); });
  app.addEventListener('dragleave', (e) => { if (e.target === app || !app.contains(e.relatedTarget)) app.classList.remove('dropzone'); });
  app.addEventListener('drop', (e) => {
    e.preventDefault();
    app.classList.remove('dropzone');
    const text = e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain');
    if (text) invoke('open_add_url', { urls: text.split(/\r?\n/).filter((l) => l && !l.startsWith('#')) });
  });

  listen('downloads', (list) => {
    state.downloads = list;
    renderSidebar();
    renderList();
    renderDetails();
    renderToolbar();
    renderStatus();
  });
  listen('state', () => refreshState());
  listen('status', (msg) => setStatus(msg));
  listen('finished', (f) => {
    if (f.status === 'error' && f.message) setStatus(f.message, true);
  });
  setInterval(async () => {
    const s = await invoke('get_state');
    state.queues = s.queues;
    state.speed_limit_enabled = s.speed_limit_enabled;
    state.speed_limit = s.speed_limit;
    state.quota_until = s.quota_until;
    state.clipboard_monitor = s.clipboard_monitor;
    state.update = s.update;
    renderSidebar();
    renderToolbar();
    renderStatus();
  }, 3000);
});
