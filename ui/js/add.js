// İndirme ekleme penceresi: adres yoklanır (ad, boyut, devam desteği), klasör kategoriye göre önerilir.
let view = null;
let nameEdited = false;
let dirEdited = false;
let probeSeq = 0;
let probeTimer = null;
let multi = false;
let media = null;
// Okunmuş torrent bilgisi ve seçili dosyalar
let tinfo = null;
let tPicked = new Set();
let tPending = false;

// Yerel dosya yolu boşluk içerebilir (C:\Kullanıcı\Film Arşivi\x.torrent); bölünmez
const LOCAL_PATH = /^([a-z]:[\\/]|\\\\|\/)/i;
const TORRENT_SRC = /^magnet:\?|\.torrent([?#]|$)/i;

function urls() {
  const raw = multi ? $('urls').value : $('url').value;
  if (!multi && LOCAL_PATH.test(raw.trim())) return [raw.trim()];
  return raw.split(/\s+/).map((s) => s.trim()).filter(Boolean);
}

function setTorrentMode(on) {
  $('torrentBox').classList.toggle('hidden', !on || !tinfo);
  $('moreBox').classList.toggle('hidden', on);
  $('nameLbl').textContent = on && tinfo && tinfo.multi_file ? T('add.torrent_folder') : T('add.name');
  $('name').disabled = on && !!tinfo && !tinfo.multi_file;
  if (!on) {
    if (tPending) invoke('torrent_inspect_cancel');
    tPending = false;
    tinfo = null;
  }
}

function renderTorrentFiles() {
  const box = $('tFiles');
  box.textContent = '';
  for (const f of tinfo.files) {
    const cb = el('input', { type: 'checkbox' });
    cb.checked = tPicked.has(f.index);
    const item = el('div', { class: `item${cb.checked ? ' sel' : ''}`, title: f.path }, cb, el('span', { class: 'name' }, f.path), el('span', { class: 'muted' }, fmtSize(f.size)));
    item.addEventListener('click', (e) => {
      if (e.target !== cb) cb.checked = !cb.checked;
      if (cb.checked) tPicked.add(f.index); else tPicked.delete(f.index);
      item.classList.toggle('sel', cb.checked);
      torrentSummary();
    });
    box.append(item);
  }
  torrentSummary();
}

function torrentSummary() {
  const sel = tinfo.files.filter((f) => tPicked.has(f.index));
  const size = sel.reduce((a, f) => a + f.size, 0);
  $('tSummary').textContent = TF('add.torrent_summary', sel.length, tinfo.files.length, fmtSize(size));
  $('tAll').checked = sel.length === tinfo.files.length;
  $('tAll').indeterminate = sel.length > 0 && sel.length < tinfo.files.length;
}

async function loadTorrent(src, seq) {
  const info = $('info');
  tinfo = null;
  setTorrentMode(true);
  info.textContent = T(/^magnet:/i.test(src) ? 'add.torrent_resolving' : 'add.torrent_reading');
  tPending = true;
  try {
    const r = await invoke('torrent_inspect', { source: src, referer: $('referer').value, cookies: $('cookies').value, userAgent: view.spec.user_agent || '' });
    if (seq !== probeSeq) return;
    tinfo = r;
    tPicked = new Set(r.files.map((f) => f.index));
    if (!nameEdited || !r.multi_file) $('name').value = r.multi_file ? r.name : (r.files[0] ? r.files[0].path : r.name);
    setTorrentMode(true);
    renderTorrentFiles();
    const parts = [TF('add.torrent_info', r.files.length, fmtSize(r.total))];
    if (r.private) parts.push(T('add.torrent_private'));
    info.textContent = parts.join(' · ');
    suggestDir();
  } catch (e) {
    if (seq !== probeSeq) return;
    // Magnet bilgisi alınamasa da eklenebilir: indirme başlayınca yeniden denenir, tüm dosyalar iner
    info.textContent = /^magnet:/i.test(src) ? `${String(e)} · ${T('add.torrent_no_meta')}` : String(e);
    info.classList.add('err');
  } finally {
    if (seq === probeSeq) tPending = false;
  }
}

function setMulti(on) {
  multi = on;
  $('urlRow').classList.toggle('hidden', on);
  $('urlsRow').classList.toggle('hidden', !on);
  $('nameRow').classList.toggle('hidden', on);
  if (on) $('note').textContent = TF('add.count', urls().length);
}

async function suggestDir() {
  if (dirEdited) return;
  const name = $('name').value || (urls()[0] || '').split(/[?#]/)[0].split('/').pop();
  $('dir').value = await invoke('suggest_dir', { name: name || '' });
}

function scheduleProbe() {
  clearTimeout(probeTimer);
  probeTimer = setTimeout(probe, 400);
}

async function probe() {
  const list = urls();
  if (multi) { $('note').textContent = TF('add.count', list.length); suggestDir(); return; }
  const url = list[0];
  const info = $('info');
  info.classList.remove('err');
  const seq = ++probeSeq;
  if (url && TORRENT_SRC.test(url) && (/^magnet:/i.test(url) || /^https?:\/\//i.test(url) || LOCAL_PATH.test(url))) {
    $('zipBtn').classList.add('hidden');
    await loadTorrent(url, seq);
    return;
  }
  setTorrentMode(false);
  if (!url || !/^(https?|ftps?):\/\//i.test(url)) { info.textContent = ''; return; }
  info.textContent = T('add.probing');
  try {
    const p = await invoke('probe_url', { url, referer: $('referer').value, cookies: $('cookies').value, userAgent: view.spec.user_agent || '' });
    if (seq !== probeSeq) return;
    if (!nameEdited) $('name').value = p.file_name;
    const parts = [p.size != null ? `${T('add.size')}: ${fmtSize(p.size)}` : T('add.size_unknown')];
    parts.push(p.resumable ? T('add.resumable') : T('add.not_resumable'));
    await loadMedia(url, p);
    if (media) parts.splice(0, parts.length, TF('add.stream', media.kind.toUpperCase(), media.variants.length));
    $('zipBtn').classList.toggle('hidden', !(p.resumable && /\.zip$/i.test($('name').value || p.file_name)));
    const dup = await invoke('find_url', { url });
    if (dup) parts.push(TF(dup.status === 'completed' ? 'add.duplicate_done' : 'add.duplicate', dup.file_name));
    info.textContent = parts.join(' · ');
    suggestDir();
  } catch (e) {
    if (seq !== probeSeq) return;
    info.textContent = String(e);
    info.classList.add('err');
  }
}

const STREAM_TYPES = /mpegurl|dash\+xml/i;

// Adres bir HLS/DASH akışıysa kalite seçimini doldurur
async function loadMedia(url, p) {
  media = null;
  $('videoRow').classList.add('hidden');
  $('audioRow').classList.add('hidden');
  const looks = view.spec.media || STREAM_TYPES.test(p.content_type || '') || /\.(m3u8?|mpd)(\?|#|$)/i.test(url);
  if (!looks) return;
  try {
    media = await invoke('media_variants', { url, referer: $('referer').value, cookies: $('cookies').value, userAgent: view.spec.user_agent || '' });
  } catch (e) { media = null; }
  if (!media) return;
  const videos = media.variants.filter((v) => v.kind !== 'audio').sort((a, b) => b.bandwidth - a.bandwidth);
  const audios = media.variants.filter((v) => v.kind === 'audio').sort((a, b) => b.bandwidth - a.bandwidth);
  const vs = $('mediaVideo');
  vs.textContent = '';
  vs.append(el('option', { value: '' }, T('add.best')));
  for (const v of videos) vs.append(el('option', { value: v.id }, v.label));
  $('videoRow').classList.toggle('hidden', !videos.length);
  const as = $('mediaAudio');
  as.textContent = '';
  as.append(el('option', { value: '' }, T('add.best')));
  for (const a of audios) as.append(el('option', { value: a.id }, a.label));
  $('audioRow').classList.toggle('hidden', !audios.length);
  if (!nameEdited) {
    const base = (view.spec.file_name || $('name').value || 'video').replace(/\.(m3u8?|mpd|ts|mp4)$/i, '');
    $('name').value = `${base}.mp4`;
  }
}

async function submit(mode) {
  const list = urls();
  $('error').classList.add('hidden');
  if (!list.length) { showError(T('add.no_url')); return; }
  if (tinfo && !multi && !tPicked.size) { showError(T('add.torrent_none_selected')); return; }
  const form = {
    urls: tinfo && !multi ? [tinfo.magnet] : list,
    file_name: multi || (tinfo && !tinfo.multi_file) || (tinfo && !nameEdited) ? '' : $('name').value.trim(),
    dir: dirEdited ? $('dir').value.trim() : '',
    queue: $('queue').value,
    mode,
    checksum_algo: $('algo').value,
    checksum: $('checksum').value.trim(),
    user: $('user').value,
    password: $('password').value,
    referer: $('referer').value.trim(),
    cookies: $('cookies').value.trim(),
    user_agent: view.spec.user_agent || '',
    description: $('description').value.trim(),
    connections: Number($('connections').value) || 0,
    media: !!media && !multi,
    media_video: media ? $('mediaVideo').value : '',
    media_audio: media ? $('mediaAudio').value : '',
    torrent_hash: tinfo && !multi ? tinfo.info_hash : '',
    torrent_files: tinfo && !multi && tPicked.size < tinfo.files.length ? [...tPicked].sort((a, b) => a - b) : null,
    torrent_name: tinfo && !multi ? (tinfo.multi_file ? tinfo.name : (tinfo.files[0] || {}).path || '') : '',
  };
  document.body.classList.add('busy');
  try {
    await invoke('add_submit', { form });
  } catch (e) {
    showError(String(e));
  } finally {
    document.body.classList.remove('busy');
  }
}

function showError(msg) {
  const e = $('error');
  e.textContent = msg;
  e.classList.remove('hidden');
}

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  view = await invoke('add_get');
  const s = view.spec;
  for (const q of view.queues) $('queue').append(el('option', { value: q.id }, q.name));
  $('queue').value = s.queue || 'main';
  $('connections').append(el('option', { value: '0' }, T('add.default')));
  for (const n of [1, 2, 4, 8, 12, 16, 24, 32]) $('connections').append(el('option', { value: String(n) }, String(n)));
  $('referer').value = s.referer || '';
  $('cookies').value = s.cookies || '';
  if (s.file_name) { $('name').value = s.file_name; nameEdited = true; }
  if (s.dir) { $('dir').value = s.dir; dirEdited = true; }
  if (s.urls && s.urls.length > 1) {
    $('urls').value = s.urls.join('\n');
    setMulti(true);
  } else {
    $('url').value = s.url || '';
  }
  suggestDir();
  probe();

  $('url').addEventListener('input', () => {
    if (urls().length > 1) { $('urls').value = urls().join('\n'); setMulti(true); }
    scheduleProbe();
  });
  $('urls').addEventListener('input', scheduleProbe);
  $('name').addEventListener('input', () => { nameEdited = true; suggestDir(); });
  $('dir').addEventListener('input', () => { dirEdited = true; });
  $('browse').addEventListener('click', async () => {
    const d = await invoke('pick_folder', { current: $('dir').value });
    if (d) { $('dir').value = d; dirEdited = true; }
  });
  $('cancel').addEventListener('click', () => invoke('dialog_close'));
  $('openTorrent').addEventListener('click', async () => {
    const p = await invoke('pick_torrent_file');
    if (!p) return;
    if (multi) setMulti(false);
    $('url').value = p;
    nameEdited = false;
    probe();
  });
  $('tAll').addEventListener('change', (e) => {
    if (!tinfo) return;
    tPicked = e.target.checked ? new Set(tinfo.files.map((f) => f.index)) : new Set();
    renderTorrentFiles();
  });
  $('later').addEventListener('click', () => submit('later'));
  $('zipBtn').addEventListener('click', () => invoke('run_action', { id: `zip_url:${urls()[0]}` }));
  $('toQueue').addEventListener('click', () => submit('queue'));
  $('now').addEventListener('click', () => submit('now'));
  document.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.target.closest('textarea')) submit('now'); });
  (multi ? $('urls') : $('url')).focus();
});
