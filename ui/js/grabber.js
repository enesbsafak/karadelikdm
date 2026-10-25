// Site Grabber: siteyi tarar, bulunan dosyalar seçilip indirilir.
let running = false;
let found = [];
const picked = new Set();

const NUMS = ['same_site_depth', 'other_site_depth', 'min_size_kb', 'max_size_kb', 'max_pages', 'delay_ms'];
const TEXTS = ['start_url', 'file_patterns', 'exclude_patterns', 'include_paths', 'exclude_paths'];

function form() {
  const f = { respect_robots: $('respect_robots').checked, cookies: '' };
  for (const k of TEXTS) f[k] = $(k).value.trim();
  for (const k of NUMS) f[k] = Math.max(0, Number($(k).value) || 0);
  return f;
}

function setRunning(on) {
  running = on;
  $('startBtn').textContent = on ? T('grab.stop') : T('grab.start');
  $('progress').classList.toggle('hidden', !on);
  $('progress').classList.toggle('indeterminate', on);
  document.querySelectorAll('#form input').forEach((i) => { i.disabled = on; });
}

// Bozuk yüzde kodlamasında decodeURI istisna fırlatır; o zaman ham adres gösterilir
function safeDecode(u) {
  try { return decodeURI(u); } catch { return u; }
}

function renderResults() {
  const box = $('results');
  box.textContent = '';
  if (!found.length) {
    box.append(el('div', { class: 'empty-hint', style: 'padding:10px' }, T('grab.none')));
  }
  for (const f of found) {
    const cb = el('input', { type: 'checkbox', checked: picked.has(f.url), onchange: (e) => { if (e.target.checked) picked.add(f.url); else picked.delete(f.url); renderNote(); } });
    box.append(el('label', { class: 'item' }, cb, el('span', { class: 'name', title: f.url }, safeDecode(f.url)), el('span', { class: 'muted' }, fmtSize(f.size))));
  }
  renderNote();
}

function renderNote() {
  const sel = found.filter((f) => picked.has(f.url));
  const total = sel.reduce((s, f) => s + (f.size || 0), 0);
  $('note').textContent = found.length ? TF('grab.selected', sel.length, found.length, fmtSize(total)) : '';
  $('queueBtn').disabled = $('nowBtn').disabled = !sel.length || running;
}

async function add(mode) {
  const sel = found.filter((f) => picked.has(f.url));
  if (!sel.length) return;
  try {
    // Referer: dosyanın bulunduğu sayfa (bazı sunucular doğrudan bağlantıyı reddeder)
    const groups = new Map();
    for (const f of sel) {
      if (!groups.has(f.referer)) groups.set(f.referer, []);
      groups.get(f.referer).push(f.url);
    }
    for (const [referer, urls] of groups) {
      await invoke('add_downloads_cmd', { form: { urls, mode, referer, queue: 'main' } });
    }
    currentWindow().close();
  } catch (e) {
    $('error').textContent = String(e);
    $('error').classList.remove('hidden');
  }
}

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  const d = { same_site_depth: 2, other_site_depth: 0, file_patterns: '*.zip *.pdf *.mp3', max_pages: 200, delay_ms: 200, min_size_kb: 0, max_size_kb: 0 };
  for (const [k, v] of Object.entries(d)) $(k).value = v;
  $('respect_robots').checked = true;
  renderResults();

  $('startBtn').addEventListener('click', async () => {
    $('error').classList.add('hidden');
    if (running) { invoke('grab_stop'); return; }
    found = [];
    picked.clear();
    renderResults();
    try {
      await invoke('grab_start', { form: form() });
      setRunning(true);
    } catch (e) {
      $('error').textContent = String(e);
      $('error').classList.remove('hidden');
    }
  });
  $('all').addEventListener('change', (e) => {
    for (const f of found) { if (e.target.checked) picked.add(f.url); else picked.delete(f.url); }
    renderResults();
  });
  $('queueBtn').addEventListener('click', () => add('queue'));
  $('nowBtn').addEventListener('click', () => add('now'));

  listen('grabber', (p) => {
    $('status').textContent = TF('grab.progress', p.pages, p.queued, p.found);
    $('status').title = p.current;
  });
  listen('grabber_done', (r) => {
    setRunning(false);
    if (!r.ok) {
      $('error').textContent = r.error;
      $('error').classList.remove('hidden');
      return;
    }
    found = r.found;
    found.forEach((f) => picked.add(f.url));
    $('all').checked = true;
    $('status').textContent = TF('grab.finished', found.length);
    renderResults();
  });
});
