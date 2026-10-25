// Tercihler penceresi.
let meta = null;
let s = null;

const CHECKS = ['run_at_startup', 'start_minimized', 'close_to_tray', 'notify_complete', 'clipboard_monitor',
  'use_categories', 'speed_limit_enabled', 'browser_enabled', 'browser_show_dialog', 'av_enabled'];

function fill() {
  for (const [code, name] of [['', T('prefs.system_language')], ...meta.languages]) {
    $('language').append(el('option', { value: code }, name));
  }
  $('language').value = s.language;
  for (const k of CHECKS) $(k).checked = !!s[k];
  $('download_dir').value = s.download_dir;
  for (const n of [1, 2, 4, 6, 8, 12, 16, 24, 32]) $('max_connections').append(el('option', { value: String(n) }, String(n)));
  $('max_connections').value = String(s.max_connections);
  $('speed_limit_kb').value = Math.round(s.speed_limit / 1024);
  $('proxy').value = s.net.proxy;
  $('user_agent').value = s.net.user_agent;
  $('timeout').value = s.net.timeout || 30;
  $('ftp_tls').value = s.net.ftp_tls || 'off';
  $('browser_min_size_kb').value = s.browser_min_size_kb;
  $('capture_exts').value = s.capture_exts;
  $('quota_enabled').checked = s.quota.enabled;
  $('quota_mb').value = s.quota.megabytes;
  $('quota_hours').value = s.quota.hours;
  $('av_command').value = s.av_command;
  $('ffmpeg_path').value = s.ffmpeg_path;
  const t = s.torrent;
  $('t_port').value = t.listen_port;
  $('t_dht').checked = t.dht;
  $('t_lsd').checked = t.lsd;
  $('t_upnp').checked = t.upnp;
  $('t_ratio').value = t.seed_ratio;
  $('t_upload_kb').value = Math.round(t.upload_limit / 1024);
  $('dataDir').textContent = TF('prefs.data_dir', meta.data_dir);
  const tb = $('cats').querySelector('tbody');
  for (const c of s.categories) {
    tb.append(el('tr', { 'data-id': c.id },
      el('td', {}, T(`cat.${c.id}`)),
      el('td', {}, c.id === 'general' ? el('span', { class: 'muted' }, T('prefs.other_files')) : el('input', { class: 'field', 'data-k': 'exts', value: c.exts })),
      el('td', {}, el('input', { class: 'field', 'data-k': 'dir', value: c.dir, placeholder: T('prefs.base_folder') })),
    ));
  }
}

function collect() {
  const out = structuredClone(s);
  out.language = $('language').value;
  for (const k of CHECKS) out[k] = $(k).checked;
  out.download_dir = $('download_dir').value.trim();
  out.max_connections = Number($('max_connections').value);
  out.speed_limit = Math.max(1, Number($('speed_limit_kb').value) || 1) * 1024;
  out.net = { proxy: $('proxy').value.trim(), user_agent: $('user_agent').value.trim(), timeout: Number($('timeout').value) || 30, ftp_tls: $('ftp_tls').value };
  out.browser_min_size_kb = Number($('browser_min_size_kb').value) || 0;
  out.capture_exts = $('capture_exts').value.trim();
  out.quota = { enabled: $('quota_enabled').checked, megabytes: Number($('quota_mb').value) || 1, hours: Math.min(24, Math.max(1, Number($('quota_hours').value) || 1)) };
  out.av_command = $('av_command').value.trim();
  out.ffmpeg_path = $('ffmpeg_path').value.trim();
  out.torrent = {
    listen_port: Math.min(65535, Math.max(0, Math.round(Number($('t_port').value) || 0))),
    dht: $('t_dht').checked,
    lsd: $('t_lsd').checked,
    upnp: $('t_upnp').checked,
    seed_ratio: Math.min(100, Math.max(0, Number($('t_ratio').value) || 0)),
    upload_limit: Math.max(0, Math.round(Number($('t_upload_kb').value) || 0)) * 1024,
  };
  out.categories = s.categories.map((c) => {
    const tr = $('cats').querySelector(`tr[data-id="${c.id}"]`);
    const v = (k) => tr.querySelector(`[data-k="${k}"]`)?.value.trim() ?? c[k];
    return { id: c.id, exts: v('exts'), dir: v('dir') };
  });
  return out;
}

// ---------- uzantı kurulumu ----------
async function renderExt() {
  let st;
  try { st = await invoke('ext_status'); } catch { return; }
  const state = $('extState');
  state.textContent = st.connected ? T('prefs.ext_connected') : T('prefs.ext_not_connected');
  state.classList.toggle('on', st.connected);
  const list = $('extList');
  list.textContent = '';
  for (const b of st.browsers) {
    const label = !b.found ? T('prefs.ext_not_found') : b.mode === 'store' ? T('prefs.ext_add_store') : b.mode === 'manual' ? T('prefs.ext_install') : T('prefs.ext_after_store');
    const btn = el('button', { class: `btn${b.found && b.mode !== 'unavailable' ? ' primary' : ''}`, type: 'button' }, label);
    btn.disabled = !b.found || b.mode === 'unavailable';
    btn.addEventListener('click', () => installExt(b.id));
    list.append(el('div', { class: 'ext-row' }, el('span', { class: 'name' }, b.name), btn));
  }
}

async function installExt(id) {
  $('error').classList.add('hidden');
  try {
    const r = await invoke('ext_install', { browser: id });
    if (r.mode === 'manual') {
      $('extPage').textContent = r.page;
      $('extPath').textContent = r.path;
      $('extSteps').classList.remove('hidden');
      await navigator.clipboard.writeText(r.path).catch(() => {});
    }
  } catch (e) {
    const er = $('error');
    er.textContent = String(e);
    er.classList.remove('hidden');
  }
}

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  meta = await invoke('settings_get');
  s = meta.settings;
  fill();
  document.querySelectorAll('.tab').forEach((t) => t.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach((x) => x.classList.toggle('active', x === t));
    document.querySelectorAll('[data-pane]').forEach((p) => p.classList.toggle('hidden', p.dataset.pane !== t.dataset.tab));
  }));
  $('browseDir').addEventListener('click', async () => {
    const d = await invoke('pick_folder', { current: $('download_dir').value });
    if (d) $('download_dir').value = d;
  });
  $('resetExts').addEventListener('click', (e) => { e.preventDefault(); $('capture_exts').value = meta.default_capture; });
  $('resetAv').addEventListener('click', (e) => { e.preventDefault(); $('av_command').value = meta.default_av; });
  $('cancel').addEventListener('click', () => currentWindow().close());
  renderExt();
  setInterval(renderExt, 3000);
  $('copyPage').addEventListener('click', () => navigator.clipboard.writeText($('extPage').textContent).catch(() => {}));
  $('copyPath').addEventListener('click', () => navigator.clipboard.writeText($('extPath').textContent).catch(() => {}));
  $('t_register').addEventListener('click', async () => {
    try {
      await invoke('torrent_register_handlers');
      $('t_register_msg').textContent = T('prefs.t_registered');
    } catch (e) {
      $('t_register_msg').textContent = String(e);
    }
  });
  $('save').addEventListener('click', async () => {
    try {
      await invoke('settings_save', { s: collect() });
      currentWindow().close();
    } catch (e) {
      const er = $('error');
      er.textContent = String(e);
      er.classList.remove('hidden');
    }
  });
});
