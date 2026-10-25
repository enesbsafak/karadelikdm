// Ana pencere: kenar çubuğu (kategori/durum/kuyruk), indirme listesi, ayrıntı paneli, durum satırı.
let state = { downloads: [], queues: [], categories: [] };
let filter = { kind: 'all', value: '' };
let sort = { key: 'added', dir: -1 };
let selection = new Set();
let anchor = null;
let search = '';
const rowMap = new Map();

const STATUS_ORDER = { downloading: 0, queued: 1, stopped: 2, error: 3, completed: 4 };

function catOf(d) { return d.category || 'general'; }

function matches(d) {
  if (search && !d.file_name.toLowerCase().includes(search) && !d.url.toLowerCase().includes(search)) return false;
  switch (filter.kind) {
    case 'cat': return catOf(d) === filter.value;
    case 'status':
      if (filter.value === 'unfinished') return d.status !== 'completed';
      return d.status === filter.value;
    case 'queue': return d.queue === filter.value && d.status !== 'completed';
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
  if (d.status === 'downloading') return T('st.downloading');
  if (d.status === 'error') return T('st.error');
  if (d.status === 'queued') return T('st.queued');
  if (d.status === 'completed') return T('st.completed');
  return d.done > 0 ? T('st.paused') : T('st.stopped');
}

// ---------- kenar çubuğu ----------
function renderSidebar() {
  const nav = $('sidebar');
  nav.textContent = '';
  const count = (fn) => state.downloads.filter(fn).length;
  const item = (kind, value, label, ico, n, extra) => {
    const active = filter.kind === kind && filter.value === value;
    const e = el('div', { class: `side-item${active ? ' active' : ''}`, onclick: () => { filter = { kind, value }; renderAll(); } });
    if (extra) e.append(extra); else e.insertAdjacentHTML('beforeend', icon(ico));
    e.append(el('span', {}, label), el('span', { class: 'count' }, n == null ? '' : String(n)));
    return e;
  };
  nav.append(el('div', { class: 'side-title' }, T('side.categories')));
  nav.append(item('all', '', T('side.all'), 'all', state.downloads.length));
  for (const c of state.categories) nav.append(item('cat', c.id, c.name, c.id, count((d) => catOf(d) === c.id)));
  nav.append(el('div', { class: 'side-title' }, T('side.status')));
  nav.append(item('status', 'downloading', T('side.downloading'), 'downloading', count((d) => d.status === 'downloading')));
  nav.append(item('status', 'unfinished', T('side.unfinished'), 'unfinished', count((d) => d.status !== 'completed')));
  nav.append(item('status', 'completed', T('side.completed'), 'completed', count((d) => d.status === 'completed')));
  nav.append(item('status', 'error', T('side.failed'), 'error', count((d) => d.status === 'error')));
  nav.append(el('div', { class: 'side-title' }, T('side.queues')));
  for (const q of state.queues) {
    nav.append(item('queue', q.id, q.name, null, count((d) => d.queue === q.id && d.status !== 'completed'), el('span', { class: `qdot${q.active ? ' on' : ''}` })));
  }
}

// ---------- liste ----------
function makeRow(d) {
  const tr = el('tr', { class: 'dlrow' });
  tr.dataset.id = d.id;
  const name = el('div', { class: 'fname' });
  name.innerHTML = icon(catOf(d));
  const nameText = el('span');
  name.append(nameText);
  const bar = el('div', { class: 'bar' }, el('div'), el('span'));
  const cells = {
    name: nameText,
    icon: name,
    size: el('td', { class: 'num' }),
    bar,
    speed: el('td', { class: 'num' }),
    eta: el('td', { class: 'num' }),
    status: el('span', { class: 'st' }),
    added: el('td', { class: 'num' }),
  };
  tr.append(el('td', {}, name), cells.size, el('td', {}, bar), cells.speed, cells.eta, el('td', {}, cells.status), cells.added);
  tr.addEventListener('mousedown', (e) => onRowClick(e, d.id));
  tr.addEventListener('dblclick', () => openItem(d.id));
  tr.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (!selection.has(d.id)) { selection = new Set([d.id]); anchor = d.id; renderSelection(); }
    showContextMenu(e.clientX, e.clientY);
  });
  const r = { tr, cells, key: '' };
  rowMap.set(d.id, r);
  return r;
}

function updateRow(r, d) {
  const p = progressOf(d);
  const key = [d.file_name, d.size, d.done, d.speed, d.eta, d.status, d.category, d.parts, I18N['st.queued'], d.torrent ? JSON.stringify(d.torrent) : ''].join('|');
  if (r.key === key) return;
  r.key = key;
  const c = r.cells;
  c.name.textContent = d.file_name;
  c.icon.title = d.url;
  c.size.textContent = fmtSize(d.size);
  c.bar.firstChild.style.width = `${(p * 100).toFixed(1)}%`;
  c.bar.lastChild.textContent = d.parts && d.parts[1] && d.status !== 'completed'
    ? `${d.parts[0]}/${d.parts[1]}`
    : d.size || d.status === 'completed' ? `${(p * 100).toFixed(1)}%` : fmtSize(d.done);
  c.bar.className = `bar${d.status === 'completed' ? ' done' : d.status === 'error' ? ' err' : ''}${d.status === 'downloading' && !d.size && !(d.parts && d.parts[1]) ? ' unknown' : ''}`;
  c.speed.textContent = d.status === 'downloading' ? fmtSpeed(d.speed)
    : d.torrent && d.torrent.seeding ? `↑ ${fmtSpeed(d.torrent.upload_speed)}` : '';
  c.eta.textContent = d.status === 'downloading' ? fmtEta(d.eta) : '';
  c.status.textContent = statusText(d);
  c.status.className = `st ${d.status}`;
  c.status.title = d.error || '';
  c.added.textContent = fmtDate(d.finished || d.added);
}

function renderList() {
  const list = visible();
  const tbody = $('rows');
  const seen = new Set();
  for (const d of list) {
    const r = rowMap.get(d.id) || makeRow(d);
    updateRow(r, d);
    seen.add(d.id);
    tbody.append(r.tr); // var olan düğümü sıraya taşır
  }
  for (const [id, r] of rowMap) {
    if (!seen.has(id)) { r.tr.remove(); if (!state.downloads.some((d) => d.id === id)) rowMap.delete(id); }
  }
  for (const id of [...selection]) if (!state.downloads.some((d) => d.id === id)) selection.delete(id);
  $('emptyList').classList.toggle('hidden', state.downloads.length > 0);
  document.querySelectorAll('#thead th').forEach((th) => {
    th.classList.toggle('sorted', th.dataset.sort === sort.key);
  });
}

function renderSelection() {
  for (const [id, r] of rowMap) r.tr.classList.toggle('sel', selection.has(id));
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

// ---------- ayrıntılar ----------
function renderDetails() {
  const box = $('details');
  const sel = selected();
  if (sel.length !== 1) { box.classList.add('hidden'); return; }
  const d = sel[0];
  box.classList.remove('hidden');
  box.textContent = '';
  const tor = d.torrent;
  const range = tor ? T('range.pieces') : d.media ? T('range.segments') : { yes: T('range.yes'), no: T('range.no'), probable: T('range.probable'), unknown: T('range.unknown') }[d.range];
  const q = state.queues.find((x) => x.id === d.queue);
  const head = el('div', { class: 'head' }, el('strong', { title: d.file_name }, d.file_name));
  if (d.status === 'completed') {
    head.append(el('button', { class: 'btn', onclick: () => act('open') }, T('ctx.open')));
  }
  head.append(el('button', { class: 'btn', onclick: () => act('folder') }, T('ctx.folder')));
  const sep = IS_MAC ? '/' : '\\';
  const grid = el('div', { class: 'grid2' },
    el('label', {}, T('det.url')), el('span', { class: 'wide', title: d.url }, d.url),
    el('label', {}, T('det.saved_to')), el('span', { class: 'wide', title: d.dir }, `${d.dir}${sep}${d.file_name}`),
    el('label', {}, T('det.size')), el('span', {}, d.parts && d.parts[1] ? `${fmtSize(d.done)} · ${TF('det.stream_parts', d.parts[0], d.parts[1])}` : `${fmtSize(d.done)} / ${fmtSize(d.size)}`),
    el('label', {}, T('det.resume')), el('span', {}, range),
    el('label', {}, T('det.speed')), el('span', {}, tor
      ? (d.status === 'downloading' || tor.seeding ? `↓ ${fmtSpeed(d.speed) || '0 B/s'} · ↑ ${fmtSpeed(tor.upload_speed) || '0 B/s'} · ${TF('det.peers', tor.peers, tor.seen)}` : '—')
      : d.status === 'downloading' ? `${fmtSpeed(d.speed)} · ${TF('det.connections', d.connections)}` : '—'),
    el('label', {}, T('det.queue')), el('span', {}, q ? q.name : '—'),
  );
  if (tor) {
    const ratio = d.size ? (tor.uploaded / d.size).toFixed(2) : '0.00';
    grid.append(
      el('label', {}, T('det.uploaded')), el('span', { class: 'wide' }, TF('det.ratio', fmtSize(tor.uploaded), ratio)),
      el('label', {}, T('det.info_hash')), el('span', { class: 'wide', style: 'font-family:Consolas,monospace;font-size:12px;user-select:text' }, tor.info_hash),
    );
  }
  box.append(head, grid);
  if (d.error) box.append(el('div', { class: d.status === 'error' ? 'err' : 'muted', style: 'margin-top:6px;font-size:12px' }, d.error));
  if (d.size && d.status !== 'completed' && d.segments.length) {
    const bar = el('div', { class: 'segbar', title: T('det.segments_hint') });
    for (const [start, , done] of d.segments) {
      bar.append(el('div', { style: `left:${(start / d.size) * 100}%;width:${(done / d.size) * 100}%` }));
      if (start > 0) bar.append(el('i', { style: `left:${(start / d.size) * 100}%` }));
    }
    box.append(bar, el('div', { class: 'seglegend' }, TF('det.segments', d.segments.length)));
  }
}

// ---------- araç çubuğu ----------
function renderToolbar() {
  const sel = selected();
  const canStart = sel.some((d) => d.status !== 'completed' && d.status !== 'downloading');
  const canStop = sel.some((d) => d.status === 'downloading' || d.status === 'queued');
  $('resumeBtn').disabled = !canStart;
  $('stopBtn').disabled = !canStop;
  $('removeBtn').disabled = sel.length === 0;
  $('stopAllBtn').disabled = !state.downloads.some((d) => d.status === 'downloading' || d.status === 'queued');
  const main = state.queues.find((q) => q.id === 'main');
  $('queueLabel').textContent = main && main.active ? T('tb.stop_queue') : T('tb.start_queue');
}

function renderStatus() {
  const active = state.downloads.filter((d) => d.status === 'downloading');
  const speed = active.reduce((s, d) => s + d.speed, 0);
  $('total').textContent = active.length ? [TF('ui.active', active.length), fmtSpeed(speed)].filter(Boolean).join(' · ') : '';
  const lim = $('limitChip');
  lim.innerHTML = `${icon('speed')}`;
  lim.append(document.createTextNode(state.speed_limit_enabled ? ` ${fmtSpeed(state.speed_limit)}` : ` ${T('ui.no_limit')}`));
  lim.classList.toggle('on', state.speed_limit_enabled);
  const clip = $('clipChip');
  clip.innerHTML = icon('clipboard');
  clip.classList.toggle('on', state.clipboard_monitor);
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
  else if (d.status === 'downloading') act('stop', [id]);
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
  document.querySelectorAll('#thead th').forEach((th) => th.addEventListener('click', () => {
    sort = sort.key === th.dataset.sort ? { key: sort.key, dir: -sort.dir } : { key: th.dataset.sort, dir: th.dataset.sort === 'name' ? 1 : -1 };
    renderList();
  }));
  $('list').addEventListener('mousedown', (e) => {
    if (e.target.closest('tr.dlrow') || e.target.closest('th')) return;
    selection.clear();
    renderSelection();
  });

  document.addEventListener('click', hideContextMenu);
  document.addEventListener('contextmenu', (e) => { if (!e.target.closest('input, textarea')) e.preventDefault(); });
  window.addEventListener('blur', hideContextMenu);

  document.addEventListener('keydown', (e) => {
    if (e.target.closest('input, textarea')) return;
    if (e.key === 'Delete') { e.preventDefault(); removeSelected(e.shiftKey); }
    else if (e.key === 'Enter' && selection.size === 1) openItem([...selection][0]);
    else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      selection = new Set(visible().map((d) => d.id));
      renderSelection();
    } else if ((e.ctrlKey || e.metaKey) && (e.key.toLowerCase() === 'v' || e.key.toLowerCase() === 'n')) {
      e.preventDefault();
      invoke('run_action', { id: 'add_clipboard' });
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
