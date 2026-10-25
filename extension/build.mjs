// Uzantı paketleri: `src/` ortak kaynaktır; tarayıcı farkları yalnız manifestte (`chromium.json`, `firefox.json`).
// Kullanım: `node extension/build.mjs` → `extension/chromium/` ve `extension/firefox/` (paketlenmemiş yüklenebilir).

import { createHash } from 'node:crypto';
import { cpSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
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

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const r = build();
  for (const t of TARGETS) console.log(`${t}: ${r[t].dir} (v${r[t].manifest.version})`);
  console.log(`Chromium uzantı kimliği: ${r.chromeId}`);
}
