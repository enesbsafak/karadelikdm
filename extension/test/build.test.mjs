import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build, extensionIdFromKey, merge, TARGETS } from '../build.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = join(HERE, '..', 'src');
const PERMISSIONS = ['downloads', 'contextMenus', 'cookies', 'nativeMessaging', 'storage', 'scripting', 'webRequest', 'activeTab'];

function built() {
  const out = mkdtempSync(join(tmpdir(), 'kdm-ext-'));
  const r = build(out);
  return { out, r, cleanup: () => rmSync(out, { recursive: true, force: true }) };
}

test('iki manifest de geçerli JSON ve doğru biçimde', () => {
  const { out, r, cleanup } = built();
  try {
    for (const t of TARGETS) {
      const m = JSON.parse(readFileSync(join(out, t, 'manifest.json'), 'utf8'));
      assert.equal(m.manifest_version, 3);
      assert.match(m.version, /^\d+\.\d+\.\d+$/);
      assert.deepEqual(m.permissions, PERMISSIONS);
      assert.deepEqual(m.host_permissions, ['<all_urls>']);
      assert.equal(m.action.default_popup, 'popup.html');
      assert.deepEqual(m, r[t].manifest);
    }
    const c = r.chromium.manifest;
    assert.equal(c.background.service_worker, 'background.js');
    assert.equal(c.background.type, 'module');
    assert.ok(c.key, 'Chromium anahtarı yok');
    assert.equal(c.browser_specific_settings, undefined);

    const f = r.firefox.manifest;
    assert.deepEqual(f.background.scripts, ['background.js']);
    assert.equal(f.background.service_worker, undefined);
    assert.equal(f.key, undefined, 'Firefox manifestinde key olmamalı');
    assert.equal(f.browser_specific_settings.gecko.id, 'karadelik-dm@enesbsafak');
  } finally {
    cleanup();
  }
});

test('Chromium kimliği anahtardan türetilir ve Rust sabitiyle aynıdır', () => {
  const { r, cleanup } = built();
  try {
    assert.equal(r.chromeId, extensionIdFromKey(r.chromium.manifest.key));
    assert.match(r.chromeId, /^[a-p]{32}$/);
    const rs = readFileSync(join(HERE, '..', '..', 'crates', 'kdm-bridge', 'src', 'lib.rs'), 'utf8');
    assert.ok(rs.includes(`"${r.chromeId}"`));
  } finally {
    cleanup();
  }
});

test('manifestin işaret ettiği dosyalar pakette var, manifest parçaları yok', () => {
  const { out, r, cleanup } = built();
  try {
    for (const t of TARGETS) {
      const dir = join(out, t);
      const m = r[t].manifest;
      const files = ['popup.html', 'popup.js', 'popup.css', 'lib.js', 'background.js', ...Object.values(m.icons)];
      for (const f of files) assert.ok(existsSync(join(dir, f)), `${t}: ${f} yok`);
      for (const f of ['manifest.base.json', 'chromium.json', 'firefox.json']) assert.ok(!existsSync(join(dir, f)), `${t}: ${f} kopyalanmamalı`);
      assert.ok(existsSync(join(dir, '_locales', m.default_locale, 'messages.json')));
    }
  } finally {
    cleanup();
  }
});

test('kaynak betikler sözdizimi açısından geçerli', () => {
  for (const f of ['background.js', 'popup.js', 'lib.js']) {
    execFileSync(process.execPath, ['--check', join(SRC, f)]);
  }
});

test('dil dosyaları aynı anahtarlara sahip ve kullanılan her anahtar tanımlı', () => {
  const locales = readdirSync(join(SRC, '_locales'));
  assert.deepEqual(locales.sort(), ['en', 'tr']);
  const keys = Object.fromEntries(
    locales.map((l) => [l, Object.keys(JSON.parse(readFileSync(join(SRC, '_locales', l, 'messages.json'), 'utf8'))).sort()]),
  );
  assert.deepEqual(keys.tr, keys.en);

  const used = new Set();
  const html = readFileSync(join(SRC, 'popup.html'), 'utf8');
  for (const m of html.matchAll(/data-i18n="([A-Za-z]+)"/g)) used.add(m[1]);
  for (const f of ['background.js', 'popup.js']) {
    for (const m of readFileSync(join(SRC, f), 'utf8').matchAll(/\bt\('([A-Za-z]+)'/g)) used.add(m[1]);
  }
  const manifest = readFileSync(join(SRC, 'manifest.base.json'), 'utf8');
  for (const m of manifest.matchAll(/__MSG_([A-Za-z]+)__/g)) used.add(m[1]);
  for (const k of used) assert.ok(keys.en.includes(k), `tanımsız dil anahtarı: ${k}`);
});

test('merge nesneleri birleştirir, dizileri değiştirir', () => {
  assert.deepEqual(merge({ a: { x: 1, y: 2 }, l: [1] }, { a: { y: 3 }, l: [2] }), { a: { x: 1, y: 3 }, l: [2] });
});

/** Sıkıştırmasız zip okuyucu (yalnız test için): merkezi dizinden adları, yerel başlıktan veriyi alır. */
function readStoredZip(buf) {
  const eocd = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert.ok(eocd >= 0, 'zip sonu yok');
  const count = buf.readUInt16LE(eocd + 10);
  let p = buf.readUInt32LE(eocd + 16);
  const files = new Map();
  for (let i = 0; i < count; i++) {
    assert.equal(buf.readUInt32LE(p), 0x02014b50);
    const method = buf.readUInt16LE(p + 10);
    const size = buf.readUInt32LE(p + 24);
    const nameLen = buf.readUInt16LE(p + 28);
    const extraLen = buf.readUInt16LE(p + 30);
    const commentLen = buf.readUInt16LE(p + 32);
    const local = buf.readUInt32LE(p + 42);
    const name = buf.subarray(p + 46, p + 46 + nameLen).toString('utf8');
    assert.equal(method, 0);
    assert.equal(buf.readUInt32LE(local), 0x04034b50);
    const dataStart = local + 30 + buf.readUInt16LE(local + 26) + buf.readUInt16LE(local + 28);
    files.set(name, buf.subarray(dataStart, dataStart + size));
    p += 46 + nameLen + extraLen + commentLen;
  }
  return files;
}

test('mağaza paketleri: geçerli zip, kökte manifest, Chromium paketinde key yok', async () => {
  const { storePackages } = await import('../build.mjs');
  const out = mkdtempSync(join(tmpdir(), 'kdm-store-'));
  const r = build(join(out, 'b'));
  const z = storePackages(r, join(out, 'store'));
  for (const target of ['chromium', 'firefox']) {
    const files = readStoredZip(readFileSync(z[target]));
    const m = JSON.parse(files.get('manifest.json').toString('utf8'));
    assert.equal(m.key, undefined, `${target} paketinde key var`);
    assert.ok(files.has('background.js') && files.has('icons/logo.png'));
    assert.deepEqual(files.get('icons/logo.png'), readFileSync(join(r[target].dir, 'icons', 'logo.png')));
  }
  // Aynı kaynak aynı paketi verir
  const again = storePackages(r, join(out, 'store2'));
  assert.deepEqual(readFileSync(z.chromium), readFileSync(again.chromium));
  rmSync(out, { recursive: true, force: true });
});
