// Saf yardımcılar: tarayıcı API'si kullanmaz, Node testleri doğrudan içe aktarır.

export const HOST_NAME = 'dev.karadelik.dm';

/** http(s) dışı adresler (blob:, data:, file:, ftp:) uygulamaya devredilemez. */
export function isHttp(url) {
  try {
    const u = new URL(url);
    return u.protocol === 'http:' || u.protocol === 'https:';
  } catch {
    return false;
  }
}

/** BitTorrent v1 özeti taşıyan magnet bağlantısı (uygulama torrent olarak indirir; çerez gönderilmez). */
export function isMagnet(url) {
  return typeof url === 'string' && /^magnet:\?/i.test(url) && /[?&]xt=urn:btih:[0-9a-z]{32,40}(&|$)/i.test(url);
}

export function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return '';
  }
}

/** Yoldaki son bölüm; Windows ve Unix ayırıcılarının ikisi de olabilir (Firefox tam yol verir). */
export function baseName(path) {
  if (!path) return '';
  const s = String(path).split(/[\\/]/).pop() || '';
  return s;
}

/** Noktasız, küçük harf uzantı. `arsiv.tar.gz` → `gz`, uzantısız → ''. */
export function extOf(name) {
  const b = baseName(name);
  const i = b.lastIndexOf('.');
  if (i <= 0 || i === b.length - 1) return '';
  return b.slice(i + 1).toLowerCase();
}

/** URL yolundan dosya adı (sorgu ve parça hariç, yüzde kodlaması çözülmüş). */
export function fileNameFromUrl(url) {
  try {
    const seg = new URL(url).pathname.split('/').pop() || '';
    try {
      return decodeURIComponent(seg);
    } catch {
      return seg;
    }
  } catch {
    return '';
  }
}

/** Uygulamadaki `capture_ext_list` ile aynı kural: boşluk, virgül, noktalı virgül; baştaki nokta atılır. */
export function normalizeExts(list) {
  const arr = Array.isArray(list) ? list : String(list || '').split(/[\s,;]+/);
  return [...new Set(arr.map((e) => String(e).trim().replace(/^\.+/, '').toLowerCase()).filter(Boolean))];
}

/**
 * Hariç tutulan site eşleşmesi: `example.com` girişi `example.com` ve `*.example.com`'u kapsar.
 * Alt alan adı girişi üst alanı kapsamaz.
 */
export function isExcluded(url, excluded) {
  const h = hostOf(url);
  if (!h || !Array.isArray(excluded)) return false;
  return excluded.some((e) => {
    const x = String(e).toLowerCase().replace(/^\*\./, '');
    return x && (h === x || h.endsWith('.' + x));
  });
}

/** Bilinen boyut (bayt) ya da null. Chrome bilinmeyeni -1, Firefox 0/-1 verebilir. */
export function knownSize(item) {
  for (const v of [item.totalBytes, item.fileSize]) {
    if (typeof v === 'number' && v > 0) return v;
  }
  return null;
}

/**
 * Tarayıcı indirmesinin uygulamaya devredilip devredilmeyeceği.
 * `prefs`: { paused, excluded: [host] }, `config`: { enabled, exts, min_size_kb }.
 * Dönen `reason` hata ayıklama içindir.
 */
export function shouldCapture(item, config, prefs = {}, selfId = '') {
  const url = item.finalUrl || item.url || '';
  if (!config || !config.enabled) return { capture: false, reason: 'disabled' };
  if (prefs.paused) return { capture: false, reason: 'paused' };
  if (selfId && item.byExtensionId === selfId) return { capture: false, reason: 'self' };
  if (item.incognito) return { capture: false, reason: 'incognito' };
  if (item.state && item.state !== 'in_progress') return { capture: false, reason: 'state' };
  if (!isHttp(url)) return { capture: false, reason: 'scheme' };
  if (isExcluded(url, prefs.excluded) || (item.referrer && isExcluded(item.referrer, prefs.excluded))) {
    return { capture: false, reason: 'excluded' };
  }
  const name = baseName(item.filename) || fileNameFromUrl(url);
  const ext = extOf(name) || extOf(fileNameFromUrl(url));
  const exts = normalizeExts(config.exts);
  if (!ext || !exts.includes(ext)) return { capture: false, reason: 'ext' };
  const size = knownSize(item);
  // Boyut bilinmiyorsa yakalanır: sunucu Content-Length vermiyorsa genelde büyük ya da akan bir dosyadır
  if (size !== null && size < (config.min_size_kb || 0) * 1024) return { capture: false, reason: 'size' };
  return { capture: true, reason: 'ok', name, size };
}

/**
 * `cookies.getAll({url})` sonucundan `Cookie` başlığı. RFC 6265 §5.4: uzun yollu çerezler önce.
 */
export function cookieHeader(cookies) {
  if (!Array.isArray(cookies)) return '';
  return cookies
    .filter((c) => c && typeof c.name === 'string')
    .slice()
    .sort((a, b) => (b.path || '').length - (a.path || '').length)
    .map((c) => (c.name ? `${c.name}=${c.value ?? ''}` : String(c.value ?? '')))
    .join('; ');
}

const HLS_TYPES = ['application/vnd.apple.mpegurl', 'application/x-mpegurl', 'audio/mpegurl', 'audio/x-mpegurl'];
const DASH_TYPES = ['application/dash+xml'];
// Akış parçaları ayrı ayrı listelenmez; manifestin kendisi yeterli
const SEGMENT_TYPES = ['video/mp2t', 'video/iso.segment', 'audio/iso.segment'];
const SEGMENT_EXTS = ['ts', 'm4s', 'cmfv', 'cmfa', 'aac', 'vtt', 'webvtt'];
/** Tek dosya medya bunun altındaysa (reklam pikseli, kısa ses efekti) listelenmez. */
export const MIN_MEDIA_FILE = 512 * 1024;

export function headerValue(headers, name) {
  if (!Array.isArray(headers)) return '';
  const n = name.toLowerCase();
  const h = headers.find((x) => x && String(x.name).toLowerCase() === n);
  return h ? String(h.value ?? '') : '';
}

/** `Content-Range: bytes 0-1/12345` toplamı, yoksa `Content-Length`. */
export function totalSizeFromHeaders(headers) {
  const range = headerValue(headers, 'content-range');
  const m = /\/(\d+)\s*$/.exec(range);
  if (m) return Number(m[1]);
  const len = Number(headerValue(headers, 'content-length'));
  return Number.isFinite(len) && len > 0 ? len : null;
}

/**
 * Yanıtın listelenecek bir medya olup olmadığı. Dönen: { kind: 'hls'|'dash'|'file', mime, size } ya da null.
 */
export function detectMedia(url, headers, statusCode = 200) {
  if (!isHttp(url) || statusCode >= 400) return null;
  const mime = headerValue(headers, 'content-type').split(';')[0].trim().toLowerCase();
  let path = '';
  try {
    path = new URL(url).pathname.toLowerCase();
  } catch {
    return null;
  }
  const ext = extOf(path);
  if (ext === 'm3u8' || HLS_TYPES.includes(mime)) return { kind: 'hls', mime, size: null };
  if (ext === 'mpd' || DASH_TYPES.includes(mime)) return { kind: 'dash', mime, size: null };
  if (!(mime.startsWith('video/') || mime.startsWith('audio/'))) return null;
  if (SEGMENT_TYPES.includes(mime) || SEGMENT_EXTS.includes(ext)) return null;
  const size = totalSizeFromHeaders(headers);
  if (size !== null && size < MIN_MEDIA_FILE) return null;
  return { kind: 'file', mime, size };
}

/** Sekme listesine ekler (aynı URL bir kez), en yeniler başta, en çok `max` öğe. Yeni dizi döner. */
export function addMedia(list, entry, max = 50) {
  const rest = (list || []).filter((m) => m.url !== entry.url);
  return [entry, ...rest].slice(0, max);
}

/** Üstel geri çekilme: 1, 2, 4 ... sn, üst sınır `max`. */
export function backoffDelay(attempt, base = 1000, max = 60000) {
  const n = Math.max(0, Math.min(attempt, 16));
  return Math.min(max, base * 2 ** n);
}

/** Sayfadan toplanan bağlantıları süzer: http(s) ve magnet, tekrarsız, parça (#) atılmış, en çok `max`. */
export function cleanLinks(urls, max = 5000) {
  const out = [];
  const seen = new Set();
  for (const raw of urls || []) {
    if (isMagnet(raw)) {
      if (!seen.has(raw)) {
        seen.add(raw);
        out.push(raw);
        if (out.length >= max) break;
      }
      continue;
    }
    if (!isHttp(raw)) continue;
    let u;
    try {
      u = new URL(raw);
    } catch {
      continue;
    }
    u.hash = '';
    const s = u.href;
    if (seen.has(s)) continue;
    seen.add(s);
    out.push(s);
    if (out.length >= max) break;
  }
  return out;
}

/** Tüm bağlantılar aynı ana makinedeyse o host, değilse ''. Çerezler yalnız bu durumda gönderilir. */
export function singleHost(urls) {
  let h = null;
  for (const u of urls) {
    const x = hostOf(u);
    if (h === null) h = x;
    else if (h !== x) return '';
  }
  return h || '';
}

export function formatSize(n) {
  if (typeof n !== 'number' || !(n > 0)) return '';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}

/** Video düğmesinin gösterileceği en küçük oynatıcı (küçük önizlemeler, reklam kutuları atlanır). */
export const VIDEO_BUTTON_MIN = { w: 200, h: 110 };

export function videoButtonFits(w, h) {
  return w >= VIDEO_BUTTON_MIN.w && h >= VIDEO_BUTTON_MIN.h;
}

/**
 * Sayfadaki video düğmesinin menüsü: önce akışlar, sonra büyükten küçüğe video dosyaları, en sonda ses.
 * Akışlarda ilk yüklenen liste başa gelir: oynatıcı önce ana listeyi (tüm kaliteler) ister, kalite listelerini
 * sonra. `src` oynatıcının doğrudan adresiyse (blob: değil) ve listede yoksa eklenir.
 * Dönen öğe: { url, kind, mime, size }.
 */
export function videoMenuItems(media, src = '') {
  const rank = (m) => (m.kind === 'hls' || m.kind === 'dash' ? 0 : String(m.mime || '').startsWith('audio/') ? 2 : 1);
  const list = (media || [])
    .filter((m) => m && isHttp(m.url))
    .sort((a, b) => rank(a) - rank(b) || (rank(a) === 0 ? (a.ts || 0) - (b.ts || 0) : (b.size || 0) - (a.size || 0)))
    .map((m) => ({ url: m.url, kind: m.kind, mime: m.mime || '', size: m.size ?? null }));
  if (isHttp(src) && !list.some((m) => m.url === src)) {
    const ext = extOf(fileNameFromUrl(src));
    const kind = ext === 'm3u8' ? 'hls' : ext === 'mpd' ? 'dash' : 'file';
    list.splice(kind === 'file' ? list.filter((m) => rank(m) === 0).length : 0, 0, { url: src, kind, mime: '', size: null });
  }
  return list;
}
