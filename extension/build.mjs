// Uzantı paketleri: `src/` ortak kaynaktır; tarayıcı farkları yalnız manifestte (`chromium.json`, `firefox.json`).
// Kullanım: `node extension/build.mjs` → `extension/chromium/` ve `extension/firefox/` (paketlenmemiş yüklenebilir).
//           `node extension/build.mjs --store` → ek olarak `extension/store/` altında mağazalara yüklenecek zip'ler.
// Chrome Web Store ve Edge Eklentileri manifestte `key` kabul etmez (kimliği kendileri verir); mağaza paketinde çıkarılır.

import { createHash } from 'node:crypto';
import { cpSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = join(HERE, 'src');
const REPO = resolve(HERE, '..');
export const TARGETS = ['chromium', 'firefox'];
// Manifest parçaları pakete kopyalanmaz
const MANIFEST_PARTS = new Set(['manifest.base.json', 'chromium.json', 'firefox.json']);

const readJson = (p) => JSON.parse(readFileSync(p, 'utf8'));

/** Nesneler birleşir, diziler ve düz değerler üzerine yazılır. */
export function merge(base, over) {
  const out = { ...base };
  for (const [k, v] of Object.entries(over)) {
    out[k] = v && typeof v === 'object' && !Array.isArray(v) && base[k] && typeof base[k] === 'object' ? merge(base[k], v) : v;
  }
  return out;
}

/** Chrome kimliği: SPKI DER'in SHA-256'sının ilk 16 baytı, her onaltılık basamak 'a'+değer. */
export function extensionIdFromKey(keyB64) {
  const hex = createHash('sha256').update(Buffer.from(keyB64, 'base64')).digest('hex').slice(0, 32);
  return [...hex].map((c) => String.fromCharCode(97 + parseInt(c, 16))).join('');
}

/** Sürüm uygulamayla aynı kalsın: workspace `Cargo.toml`'daki `[workspace.package] version`. */
function workspaceVersion() {
  try {
    const toml = readFileSync(join(REPO, 'Cargo.toml'), 'utf8');
    const sec = toml.split(/^\[workspace\.package\]/m)[1] || '';
    const m = /^version\s*=\s*"([0-9.]+)"/m.exec(sec);
    return m ? m[1] : null;
  } catch {
    return null;
  }
}

/** Rust tarafındaki `CHROME_EXTENSION_ID` ile manifest anahtarı tutarlı olmalı; yoksa native host uzantıyı reddeder. */
function rustChromeId() {
  try {
    const rs = readFileSync(join(REPO, 'crates/kdm-bridge/src/lib.rs'), 'utf8');
    return /CHROME_EXTENSION_ID:\s*&str\s*=\s*"([a-p]{32})"/.exec(rs)?.[1] ?? null;
  } catch {
    return null;
  }
}

export function manifestFor(target) {
  const base = readJson(join(SRC, 'manifest.base.json'));
  const m = merge(base, readJson(join(SRC, `${target}.json`)));
  const v = workspaceVersion();
  if (v) m.version = v;
  return m;
}

export function build(outRoot = HERE) {
  const result = {};
  for (const target of TARGETS) {
    const out = join(outRoot, target);
    rmSync(out, { recursive: true, force: true });
    mkdirSync(out, { recursive: true });
    for (const entry of readdirSync(SRC)) {
      if (MANIFEST_PARTS.has(entry)) continue;
      cpSync(join(SRC, entry), join(out, entry), { recursive: true });
    }
    const manifest = manifestFor(target);
    writeFileSync(join(out, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
    result[target] = { dir: out, manifest };
  }
  const id = extensionIdFromKey(result.chromium.manifest.key);
  const rust = rustChromeId();
  if (rust && rust !== id) throw new Error(`Chrome kimliği uyuşmuyor: manifest ${id}, Rust ${rust}`);
  result.chromeId = id;
  return result;
}

// ---------- mağaza paketi ----------

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function listFiles(dir, base = '') {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name);
    const rel = base ? `${base}/${name}` : name;
    if (statSync(p).isDirectory()) out.push(...listFiles(p, rel));
    else out.push(rel);
  }
  return out;
}

/**
 * Sıkıştırmasız (stored) ZIP. Dosyalar kökte durur (mağazalar `manifest.json`'u kökte ister); tarih sabittir,
 * böylece aynı kaynak her derlemede bayt bayt aynı paketi verir.
 */
export function zipStored(files) {
  const locals = [];
  const centrals = [];
  let offset = 0;
  const DOS_TIME = 0;
  const DOS_DATE = (2026 - 1980) << 9 | 1 << 5 | 1;
  for (const { name, data } of files) {
    const nameBuf = Buffer.from(name, 'utf8');
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0x0800, 6); // UTF-8 adlar
    local.writeUInt16LE(0, 8);
    local.writeUInt16LE(DOS_TIME, 10);
    local.writeUInt16LE(DOS_DATE, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    local.writeUInt16LE(0, 28);
    locals.push(local, nameBuf, data);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8);
    central.writeUInt16LE(0, 10);
    central.writeUInt16LE(DOS_TIME, 12);
    central.writeUInt16LE(DOS_DATE, 14);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(nameBuf.length, 28);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, nameBuf);
    offset += 30 + nameBuf.length + data.length;
  }
  const centralBuf = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralBuf.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, centralBuf, end]);
}

/** Mağaza zip'leri: Chromium paketinden `key` çıkarılır; Firefox paketi olduğu gibi. */
export function storePackages(result, outDir = join(HERE, 'store')) {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  const made = {};
  for (const target of TARGETS) {
    const { dir, manifest } = result[target];
    const files = listFiles(dir).map((name) => {
      if (name === 'manifest.json') {
        const m = { ...manifest };
        delete m.key;
        return { name, data: Buffer.from(JSON.stringify(m, null, 2) + '\n') };
      }
      return { name, data: readFileSync(join(dir, name)) };
    });
    const file = join(outDir, `karadelikdm-${target}-${manifest.version}.zip`);
    writeFileSync(file, zipStored(files));
    made[target] = file;
  }
  return made;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const r = build();
  for (const t of TARGETS) console.log(`${t}: ${r[t].dir} (v${r[t].manifest.version})`);
  console.log(`Chromium uzantı kimliği: ${r.chromeId}`);
  if (process.argv.includes('--store')) {
    const z = storePackages(r);
    for (const t of TARGETS) console.log(`mağaza paketi: ${z[t]}`);
  }
}
