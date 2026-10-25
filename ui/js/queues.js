// Kuyruklar ve zamanlayıcı.
let data = null;
let queues = [];
let current = 'main';
let fileSel = null;

const DAYS = ['day.mon', 'day.tue', 'day.wed', 'day.thu', 'day.fri', 'day.sat', 'day.sun'];

const cur = () => queues.find((q) => q.id === current);
const qName = (q) => q.name || data.main_name;

function renderList() {
  const box = $('qlist');
  box.textContent = '';
  for (const q of queues) {
    box.append(el('div', { class: `side-item${q.id === current ? ' active' : ''}`, onclick: () => { save(); current = q.id; render(); } },
      el('span', { class: `qdot${data.active.includes(q.id) ? ' on' : ''}` }), el('span', {}, qName(q))));
  }
  box.append(el('button', { class: 'btn', onclick: addQueue }, T('queues.new')));
  if (current !== 'main') box.append(el('button', { class: 'btn danger', onclick: removeQueue }, T('queues.delete')));
}

function renderFiles() {
  const box = $('files');
  box.textContent = '';
  const files = data.downloads.filter((d) => d.queue === current && d.status !== 'completed').sort((a, b) => a.order - b.order);
  if (!files.length) box.append(el('div', { class: 'empty-hint', style: 'padding:10px' }, T('queues.empty')));
  for (const d of files) {
    box.append(el('div', { class: `item${fileSel === d.id ? ' sel' : ''}`, onclick: () => { fileSel = d.id; renderFiles(); } },
      el('span', { class: 'name' }, d.file_name), el('span', { class: 'muted' }, fmtSize(d.size)), el('span', { class: `st ${d.status}` }, T(`st.${d.status}`))));
  }
}

function render() {
  const q = cur();
  $('qname').value = qName(q);
  $('qname').disabled = q.id === 'main';
  $('parallel').value = q.max_parallel;
  $('retry').value = q.retry_limit;
  $('schedOn').checked = q.schedule.enabled;
  $('startAt').value = q.schedule.start;
  $('stopAt').value = q.schedule.stop;
  $('once').checked = q.schedule.once;
  $('after').value = q.after;
  const days = $('days');
  days.textContent = '';
  DAYS.forEach((k, i) => {
    days.append(el('label', {}, el('input', { type: 'checkbox', 'data-bit': String(i), checked: (q.schedule.days >> i) & 1 ? true : false }), el('span', {}, T(k))));
  });
  $('runBtn').textContent = data.active.includes(q.id) ? T('queues.stop_now') : T('queues.start_now');
  renderList();
  renderFiles();
}

function save() {
  const q = cur();
  if (!q) return;
  if (q.id !== 'main') q.name = $('qname').value.trim() || q.name;
  q.max_parallel = Math.min(16, Math.max(1, Number($('parallel').value) || 1));
  q.retry_limit = Math.min(20, Math.max(0, Number($('retry').value) || 0));
  let bits = 0;
  document.querySelectorAll('#days input').forEach((c) => { if (c.checked) bits |= 1 << Number(c.dataset.bit); });
  q.schedule = { enabled: $('schedOn').checked, start: $('startAt').value, stop: $('stopAt').value, days: bits, once: $('once').checked };
  q.after = $('after').value;
}

function addQueue() {
  save();
  const id = `q${Date.now().toString(36)}`;
  queues.push({ id, name: TF('queues.new_name', queues.length), max_parallel: 2, retry_limit: 5, after: 'none', schedule: { enabled: false, start: '02:00', stop: '', days: 127, once: false } });
  current = id;
  render();
}

function removeQueue() {
  queues = queues.filter((q) => q.id !== current);
  current = 'main';
  render();
}

async function persist() {
  save();
  await invoke('queues_save', { queues });
}

async function move(delta) {
  const files = data.downloads.filter((d) => d.queue === current && d.status !== 'completed').sort((a, b) => a.order - b.order);
  const i = files.findIndex((d) => d.id === fileSel);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= files.length) return;
  [files[i], files[j]] = [files[j], files[i]];
  await invoke('reorder', { ids: files.map((d) => d.id) });
  await reload();
}

async function reload() {
  data = await invoke('queues_get');
  if (!queues.length) queues = data.queues;
  render();
}

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  await reload();
  $('up').addEventListener('click', () => move(-1));
  $('down').addEventListener('click', () => move(1));
  $('close').addEventListener('click', () => currentWindow().close());
  $('save').addEventListener('click', async () => {
    try { await persist(); currentWindow().close(); } catch (e) { $('error').textContent = String(e); $('error').classList.remove('hidden'); }
  });
  $('runBtn').addEventListener('click', async () => {
    await persist();
    const running = data.active.includes(current);
    await invoke('run_action', { id: `${running ? 'queue_stop' : 'queue_start'}:${current}` });
    setTimeout(reload, 300);
  });
  listen('downloads', (list) => { if (data) { data.downloads = list; renderFiles(); } });
});
