// Küçük diyaloglar: giriş, onay, bilgi, geri sayım.
let spec = null;

function values() {
  return [...document.querySelectorAll('#form [name]')].map((i) => [i.name, i.value]);
}

const submit = () => invoke('dialog_submit', { values: values() });

document.addEventListener('DOMContentLoaded', async () => {
  await loadI18n();
  spec = await invoke('dialog_get');
  if (!spec) { currentWindow().close(); return; }
  $('title').textContent = spec.title;
  $('message').textContent = spec.message;
  if (spec.text) { $('text').textContent = spec.text; $('text').classList.remove('hidden'); }
  for (const f of spec.fields) {
    const input = el('input', { class: 'field', name: f.name, type: f.kind || 'text', value: f.value, spellcheck: 'false' });
    $('form').append(el('div', { class: 'row' }, el('label', { class: 'lbl', style: 'width:auto;min-width:120px' }, f.label), input));
  }
  const ok = $('ok');
  if (spec.kind === 'info') $('cancel').classList.add('hidden');
  // Turuncu zemin üzerinde kırmızı yazı okunmaz: tehlikeli eylem düğmesi danger stilinde
  if (spec.danger) { ok.classList.remove('primary'); ok.classList.add('danger'); }
  if (spec.kind === 'confirm' || spec.kind === 'countdown') ok.textContent = T('btn.yes');
  if (spec.kind === 'countdown') {
    let left = spec.seconds;
    const cd = $('countdown');
    cd.classList.remove('hidden');
    const tick = () => {
      cd.textContent = String(left);
      if (left-- <= 0) submit();
      else setTimeout(tick, 1000);
    };
    tick();
    ok.textContent = T('btn.now');
  }
  ok.addEventListener('click', () => (spec.kind === 'info' ? invoke('dialog_close') : submit()));
  $('cancel').addEventListener('click', () => invoke('dialog_close'));
  $('form').addEventListener('submit', (e) => { e.preventDefault(); submit(); });
  const first = document.querySelector('#form input');
  if (first) { first.focus(); first.select(); } else ok.focus();
});
