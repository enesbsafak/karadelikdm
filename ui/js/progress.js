// İndirme ilerleme penceresi; indirme bitince aynı pencere "İndirme tamamlandı" görünümüne geçer.
let d = null;

function progressOf(x) {
  if (x.status === 'completed') return 1;
  if (x.parts && x.parts[1]) return x.parts[0] / x.parts[1];
  return x.size ? x.done / x.size : 0;
}

function kind(x) {
  if (x.torrent) return 'torrent';
  if (x.media) return 'stream';
  return x.category || 'general';
}

function stateText(x) {
  const t = x.torrent;
  if (t && t.resolving) return T('st.t_resolving');
  if (t && t.checking) return T('st.t_checking');
  if (x.status === 'downloading') return T('st.downloading');
  if (x.status === 'error') return T('st.error');
  if (x.status === 'queued') return T('st.queued');
  return x.done > 0 ? T('st.paused') : T('st.stopped');
}

function resumeText(x) {
  if (x.torrent) return T('range.pieces');
  if (x.media) return T('range.segments');
  return { yes: T('range.yes'), no: T('range.no'), probable: T('range.probable'), unknown: T('range.unknown') }[x.range];
}

const show = (id, on) => $(id).classList.toggle('hidden', !on);
const sep = () => (IS_MAC ? '/' : '\\');

function render() {
  if (!d) return;
  const done = d.status === 'completed';
  $('icon').innerHTML = icon(done ? 'completed' : kind(d));
  $('icon').dataset.kind = done ? 'done' : kind(d);
  $('name').textContent = d.file_name;
  $('name').title = d.file_name;
  let host = '';
  try { host = new URL(d.url).host; } catch {}
  $('sub').textContent = d.torrent ? 'BitTorrent' : host;

  show('running', !done);
  show('done-view', done);
  for (const id of ['hide', 'cancel', 'pause']) show(id, !done);
  for (const id of ['folder', 'close', 'open']) show(id, done);
  show('openWith', done && !IS_MAC);

  if (done) {
    $('dSize').textContent = fmtSize(d.size);
    const path = d.dir + sep() + d.file_name;
    $('dPath').textContent = path;
    $('dPath').title = path;
    return;
  }
  const p = progressOf(d);
  $('bar').style.width = `${(p * 100).toFixed(1)}%`;
  $('bar').className = d.status === 'error' ? 'err' : d.status === 'downloading' ? '' : 'idle';
  $('pct').textContent = d.size || (d.parts && d.parts[1]) ? `${Math.floor(p * 1000) / 10}%` : '';
  $('state').textContent = stateText(d);
  $('done').textContent = d.parts && d.parts[1]
    ? `${fmtSize(d.done)} · ${TF('det.stream_parts', d.parts[0], d.parts[1])}`
    : d.size ? `${fmtSize(d.done)} / ${fmtSize(d.size)}` : fmtSize(d.done);
  const running = d.status === 'downloading';
  $('speed').textContent = running ? (fmtSpeed(d.speed) || '0 B/s') : '—';
  $('eta').textContent = running && d.eta != null ? fmtEta(d.eta) : '—';
  $('conns').textContent = d.torrent ? TF('det.peers', d.torrent.peers, d.torrent.seen) : running ? String(d.connections) : '—';
  $('resume').textContent = resumeText(d);
  const segs = $('segs');
  const showSegs = !d.torrent && !d.media && d.size && d.segments.length > 0;
  show('segs', showSegs);
  if (showSegs) {
    segs.textContent = '';
    for (const [start, , got] of d.segments) {
      segs.append(el('div', { style: `left:${(start / d.size) * 100}%;width:${(got / d.size) * 100}%` }));
      if (start > 0) segs.append(el('i', { style: `left:${(start / d.size) * 100}%` }));
    }
  }
  show('err', d.status === 'error' && !!d.error);
  $('err').textContent = d.error;
  const active = d.status === 'downloading' || d.status === 'queued';
  $('pause').textContent = active ? T('prog.pause') : d.status === 'error' ? T('prog.retry') : T('prog.resume');
}

async function refresh() {
  const x = await invoke('progress_info').catch(() => null);
  if (!x) {
    // İndirme listeden kaldırıldı
    currentWindow().close();
    return;
  }
  d = x;
  render();
}

const act = (action) => invoke('dl_action', { ids: [d.id], action }).catch((e) => { $('err').textContent = String(e); show('err', true); });

window.onI18n = render;

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  await refresh();
  $('hide').addEventListener('click', () => currentWindow().close());
  $('close').addEventListener('click', () => currentWindow().close());
  $('cancel').addEventListener('click', async () => {
    if (d.status === 'downloading' || d.status === 'queued') await act('stop');
    currentWindow().close();
  });
  $('pause').addEventListener('click', () => act(d.status === 'downloading' || d.status === 'queued' ? 'stop' : 'start'));
  $('open').addEventListener('click', async () => { await act('open'); currentWindow().close(); });
  $('folder').addEventListener('click', async () => { await act('folder'); currentWindow().close(); });
  $('openWith').addEventListener('click', async () => { await act('open_with'); currentWindow().close(); });
  $('dontShow').addEventListener('change', (e) => invoke('set_show_complete', { value: !e.target.checked }));
  listen('downloads', (list) => {
    const x = d && list.find((y) => y.id === d.id);
    if (x) { d = x; render(); }
  });
  listen('finished', (f) => { if (d && f.id === d.id) refresh(); });
  // Durdurulmuş indirme olay göndermez; durum değişiklikleri için seyrek yoklama
  setInterval(refresh, 2000);
});
