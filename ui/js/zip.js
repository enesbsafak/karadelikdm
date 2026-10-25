// ZIP önizleme: arşivin içeriği indirmeden listelenir, yalnız seçilen dosyalar indirilip çıkarılır.
let spec = null;
let entries = [];
const picked = new Set();

function setError(msg) {
  const e = $('error');
  e.textContent = msg || '';
  e.classList.toggle('hidden', !msg);
}

function renderNote() {
  const sel = entries.filter((e) => picked.has(e.index));
  $('note').textContent = TF('zip.selected', sel.length, fmtSize(sel.reduce((s, e) => s + e.compressed, 0)));
  $('extract').disabled = !sel.length;
}

function render() {
  const box = $('entries');
  box.textContent = '';
  for (const e of entries) {
    if (e.is_dir) continue;
    const cb = el('input', { type: 'checkbox', disabled: !e.supported, checked: picked.has(e.index), onchange: (ev) => { if (ev.target.checked) picked.add(e.index); else picked.delete(e.index); renderNote(); } });
    box.append(el('label', { class: 'item', title: e.supported ? '' : T('zip.unsupported') }, cb, el('span', { class: 'name' }, e.name), el('span', { class: 'muted' }, fmtSize(e.size))));
  }
  renderNote();
}

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  spec = await invoke('page_spec');
  $('url').textContent = spec.url;
  $('dir').value = spec.dir || '';
  $('status').textContent = T('zip.reading');
  $('progress').classList.remove('hidden');
  $('progress').classList.add('indeterminate');
  try {
    entries = await invoke('zip_list', { url: spec.url, referer: spec.referer || '', cookies: spec.cookies || '' });
    const files = entries.filter((e) => !e.is_dir);
    $('status').textContent = TF('zip.count', files.length, fmtSize(files.reduce((s, e) => s + e.size, 0)));
  } catch (e) {
    $('status').textContent = '';
    setError(String(e));
  }
  $('progress').classList.add('hidden');
  render();

  $('all').addEventListener('change', (ev) => {
    for (const e of entries) if (e.supported && !e.is_dir) { if (ev.target.checked) picked.add(e.index); else picked.delete(e.index); }
    render();
  });
  $('browse').addEventListener('click', async () => {
    const d = await invoke('pick_folder', { current: $('dir').value });
    if (d) $('dir').value = d;
  });
  $('close').addEventListener('click', () => currentWindow().close());
  $('whole').addEventListener('click', async () => {
    await invoke('open_add_url', { urls: [spec.url] });
    currentWindow().close();
  });
  $('extract').addEventListener('click', async () => {
    setError('');
    try {
      await invoke('zip_extract', { url: spec.url, referer: spec.referer || '', cookies: spec.cookies || '', indexes: [...picked], dir: $('dir').value });
      $('extract').disabled = true;
      $('progress').classList.remove('hidden');
    } catch (e) { setError(String(e)); }
  });
  listen('zip_progress', (p) => {
    $('status').textContent = `${p.done + 1}/${p.total} · ${p.name}`;
    $('progress').classList.remove('indeterminate');
    $('progress').firstElementChild.style.width = `${((p.done) / p.total) * 100}%`;
  });
  listen('zip_done', (r) => {
    $('progress').classList.add('hidden');
    if (r.ok) currentWindow().close(); else { setError(r.error); $('extract').disabled = false; }
  });
});
