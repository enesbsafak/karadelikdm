import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as L from '../src/lib.js';

const cfg = { enabled: true, exts: ['zip', 'iso', 'mp4'], min_size_kb: 100 };
const item = (o = {}) => ({
  id: 1,
  url: 'https://cdn.example.com/files/setup.zip',
  finalUrl: 'https://cdn.example.com/files/setup.zip',
  filename: 'setup.zip',
  totalBytes: 5 * 1024 * 1024,
  fileSize: -1,
  mime: 'application/zip',
  referrer: 'https://example.com/download',
  state: 'in_progress',
  incognito: false,
  ...o,
});

test('isHttp yalnız http ve https', () => {
  assert.equal(L.isHttp('https://a.b/c'), true);
  assert.equal(L.isHttp('http://a.b/c'), true);
  for (const u of ['blob:https://a.b/x', 'data:text/plain,hi', 'file:///C:/x.zip', 'ftp://a/b', 'javascript:alert(1)', '', 'garbage']) {
    assert.equal(L.isHttp(u), false, u);
  }
});

test('extOf ve baseName', () => {
  assert.equal(L.extOf('a.ZIP'), 'zip');
  assert.equal(L.extOf('arsiv.tar.gz'), 'gz');
  assert.equal(L.extOf('C:\\Users\\x\\Downloads\\film.mkv'), 'mkv');
  assert.equal(L.extOf('/home/x/.bashrc'), '');
  assert.equal(L.extOf('README'), '');
  assert.equal(L.extOf('dosya.'), '');
  assert.equal(L.baseName('C:\\a\\b.iso'), 'b.iso');
  assert.equal(L.baseName('/a/b/c.iso'), 'c.iso');
});

test('fileNameFromUrl sorgu ve kodlamayı ayıklar', () => {
  assert.equal(L.fileNameFromUrl('https://a/x/Kurulum%20Dosyas%C4%B1.exe?token=1#f'), 'Kurulum Dosyası.exe');
  assert.equal(L.fileNameFromUrl('https://a/'), '');
  assert.equal(L.fileNameFromUrl('https://a/%E0%A4%A'), '%E0%A4%A');
});

test('normalizeExts uygulamadaki kuralı izler', () => {
  assert.deepEqual(L.normalizeExts('zip, .RAR;7z  iso'), ['zip', 'rar', '7z', 'iso']);
  assert.deepEqual(L.normalizeExts(['.Zip', 'zip', '']), ['zip']);
});

test('isExcluded alt alan adlarını kapsar, üst alanı kapsamaz', () => {
  const ex = ['example.com', 'files.other.org'];
  assert.equal(L.isExcluded('https://example.com/a', ex), true);
  assert.equal(L.isExcluded('https://dl.example.com/a', ex), true);
  assert.equal(L.isExcluded('https://notexample.com/a', ex), false);
  assert.equal(L.isExcluded('https://other.org/a', ex), false);
  assert.equal(L.isExcluded('https://x.files.other.org/a', ex), true);
  assert.equal(L.isExcluded('https://a.b', undefined), false);
});

test('shouldCapture: eşleşen indirme yakalanır', () => {
  const r = L.shouldCapture(item(), cfg, {});
  assert.equal(r.capture, true);
  assert.equal(r.name, 'setup.zip');
  assert.equal(r.size, 5 * 1024 * 1024);
});

test('shouldCapture: elenme nedenleri', () => {
  const cases = [
    [item(), { ...cfg, enabled: false }, {}, 'disabled'],
    [item(), null, {}, 'disabled'],
    [item(), cfg, { paused: true }, 'paused'],
    [item({ byExtensionId: 'me' }), cfg, {}, 'self', 'me'],
    [item({ incognito: true }), cfg, {}, 'incognito'],
    [item({ state: 'complete' }), cfg, {}, 'state'],
    [item({ url: 'blob:https://x/1', finalUrl: '' }), cfg, {}, 'scheme'],
    [item({ url: 'data:application/zip;base64,AA', finalUrl: undefined }), cfg, {}, 'scheme'],
    [item(), cfg, { excluded: ['example.com'] }, 'excluded'],
    [item({ filename: 'page.html', url: 'https://a/page', finalUrl: 'https://a/page' }), cfg, {}, 'ext'],
    [item({ totalBytes: 10 * 1024 }), cfg, {}, 'size'],
  ];
  for (const [it, c, prefs, reason, self] of cases) {
    const r = L.shouldCapture(it, c, prefs, self);
    assert.equal(r.capture, false, reason);
    assert.equal(r.reason, reason);
  }
});

test('shouldCapture: boyut bilinmiyorsa yakalanır, ad yoksa URL kullanılır', () => {
  assert.equal(L.shouldCapture(item({ totalBytes: -1, fileSize: -1 }), cfg).capture, true);
  const r = L.shouldCapture(item({ filename: '', url: 'https://a/b/disk.iso?x=1', finalUrl: '' }), cfg);
  assert.equal(r.capture, true);
  assert.equal(r.name, 'disk.iso');
  // Firefox tam yol verir
  assert.equal(L.shouldCapture(item({ filename: 'C:\\Users\\u\\Downloads\\setup.zip' }), cfg).name, 'setup.zip');
  // Yönlendirme sonrası adres esas
  const red = item({ url: 'https://a/get?id=5', finalUrl: 'https://cdn/a.iso', filename: '' });
  assert.equal(L.shouldCapture(red, cfg).capture, true);
});

test('cookieHeader uzun yollu çerezleri öne alır', () => {
  const h = L.cookieHeader([
    { name: 'a', value: '1', path: '/' },
    { name: 'b', value: '2', path: '/files/' },
    { name: '', value: 'bare', path: '/' },
  ]);
  assert.equal(h, 'b=2; a=1; bare');
  assert.equal(L.cookieHeader(undefined), '');
});

test('detectMedia: HLS, DASH, dosya ve parçalar', () => {
  const ct = (v, extra = []) => [{ name: 'Content-Type', value: v }, ...extra];
  assert.equal(L.detectMedia('https://v/master.m3u8?t=1', ct('text/plain')).kind, 'hls');
  assert.equal(L.detectMedia('https://v/play', ct('application/vnd.apple.mpegurl')).kind, 'hls');
  assert.equal(L.detectMedia('https://v/play', ct('application/x-mpegURL; charset=utf-8')).kind, 'hls');
  assert.equal(L.detectMedia('https://v/manifest.mpd', ct('application/octet-stream')).kind, 'dash');
  assert.equal(L.detectMedia('https://v/x', ct('application/dash+xml')).kind, 'dash');
  const big = { name: 'Content-Length', value: String(50 * 1024 * 1024) };
  const f = L.detectMedia('https://v/film.mp4', ct('video/mp4', [big]));
  assert.deepEqual(f, { kind: 'file', mime: 'video/mp4', size: 50 * 1024 * 1024 });
  // 206 yanıtında toplam boyut Content-Range'den
  const ranged = L.detectMedia('https://v/song.mp3', ct('audio/mpeg', [{ name: 'content-range', value: 'bytes 0-1/7340032' }]), 206);
  assert.equal(ranged.size, 7340032);
  assert.equal(L.detectMedia('https://v/seg-1.ts', ct('video/mp2t')), null);
  assert.equal(L.detectMedia('https://v/seg-1.m4s', ct('video/mp4')), null);
  assert.equal(L.detectMedia('https://v/beep.mp3', ct('audio/mpeg', [{ name: 'Content-Length', value: '2000' }])), null);
  assert.equal(L.detectMedia('https://v/page', ct('text/html')), null);
  assert.equal(L.detectMedia('https://v/a.m3u8', ct('text/html'), 404), null);
  assert.equal(L.detectMedia('blob:https://v/1', ct('video/mp4')), null);
});

test('addMedia tekrarı önler ve sınırlar', () => {
  let list = [];
  for (let i = 0; i < 60; i++) list = L.addMedia(list, { url: `https://v/${i}.m3u8` });
  assert.equal(list.length, 50);
  assert.equal(list[0].url, 'https://v/59.m3u8');
  list = L.addMedia(list, { url: 'https://v/30.m3u8', kind: 'hls' });
  assert.equal(list.filter((m) => m.url === 'https://v/30.m3u8').length, 1);
  assert.equal(list[0].url, 'https://v/30.m3u8');
});

test('backoffDelay üstel ve sınırlı', () => {
  assert.equal(L.backoffDelay(0), 1000);
  assert.equal(L.backoffDelay(1), 2000);
  assert.equal(L.backoffDelay(3), 8000);
  assert.equal(L.backoffDelay(100), 60000);
  assert.equal(L.backoffDelay(-5), 1000);
});

test('cleanLinks ve singleHost', () => {
  const urls = L.cleanLinks([
    'https://a.com/1.zip#top',
    'https://a.com/1.zip',
    'javascript:void(0)',
    'mailto:x@y',
    'https://a.com/2.zip',
  ]);
  assert.deepEqual(urls, ['https://a.com/1.zip', 'https://a.com/2.zip']);
  assert.equal(L.singleHost(urls), 'a.com');
  assert.equal(L.singleHost([...urls, 'https://b.com/x']), '');
  assert.equal(L.singleHost([]), '');
  assert.equal(L.cleanLinks(Array.from({ length: 20 }, (_, i) => `https://a/${i}`), 5).length, 5);
});

test('formatSize', () => {
  assert.equal(L.formatSize(512), '512 B');
  assert.equal(L.formatSize(1536), '1.5 KB');
  assert.equal(L.formatSize(50 * 1024 * 1024), '50 MB');
  assert.equal(L.formatSize(null), '');
  assert.equal(L.formatSize(-1), '');
});

test('isMagnet ve cleanLinks magnet bağlantılarını alır', () => {
  const m = 'magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x';
  assert.equal(L.isMagnet(m), true);
  assert.equal(L.isMagnet('magnet:?xt=urn:btih:ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'), true);
  for (const u of ['magnet:?dn=yok', 'magnet:?xt=urn:sha1:abc', 'https://a/magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567', null]) {
    assert.equal(L.isMagnet(u), false, String(u));
  }
  assert.deepEqual(L.cleanLinks(['https://a.com/1.zip', m, m, 'magnet:?dn=bozuk']), ['https://a.com/1.zip', m]);
});

test('video düğmesi: küçük oynatıcıda çıkmaz', () => {
  assert.equal(L.videoButtonFits(640, 360), true);
  assert.equal(L.videoButtonFits(199, 360), false);
  assert.equal(L.videoButtonFits(640, 100), false);
});

test('video düğmesi menüsü: akış, büyük video, ses; oynatıcı adresi eklenir', () => {
  const media = [
    { url: 'https://a.com/ses.m4a', kind: 'file', mime: 'audio/mp4', size: 9e6 },
    { url: 'https://a.com/kucuk.mp4', kind: 'file', mime: 'video/mp4', size: 2e6 },
    { url: 'https://a.com/master.m3u8', kind: 'hls', mime: '', size: null },
    { url: 'https://a.com/buyuk.mp4', kind: 'file', mime: 'video/mp4', size: 8e7 },
    { url: 'blob:https://a.com/x', kind: 'file', mime: 'video/mp4', size: 1 },
  ];
  assert.deepEqual(L.videoMenuItems(media).map((m) => m.url),
    ['https://a.com/master.m3u8', 'https://a.com/buyuk.mp4', 'https://a.com/kucuk.mp4', 'https://a.com/ses.m4a']);
  // Doğrudan oynatılan dosya: akışlardan sonra, dosyaların başına
  const withSrc = L.videoMenuItems(media, 'https://cdn.a.com/oynatici.webm');
  assert.equal(withSrc[1].url, 'https://cdn.a.com/oynatici.webm');
  assert.equal(withSrc[1].kind, 'file');
  // Listede olan adres tekrar eklenmez; blob: eklenmez
  assert.equal(L.videoMenuItems(media, 'https://a.com/buyuk.mp4').length, 4);
  assert.equal(L.videoMenuItems([], 'blob:https://a.com/1').length, 0);
  // Oynatıcı bir HLS listesini doğrudan oynatıyorsa (Safari tarzı) akış sayılır
  assert.deepEqual(L.videoMenuItems([], 'https://a.com/v/index.m3u8?t=1'), [{ url: 'https://a.com/v/index.m3u8?t=1', kind: 'hls', mime: '', size: null }]);
  // Akışlar yüklenme sırasıyla: ana liste önce (sekme listesi en yeniyi başta tutar)
  const hls = [
    { url: 'https://a.com/720p.m3u8', kind: 'hls', ts: 3 },
    { url: 'https://a.com/360p.m3u8', kind: 'hls', ts: 2 },
    { url: 'https://a.com/master.m3u8', kind: 'hls', ts: 1 },
  ];
  assert.deepEqual(L.videoMenuItems(hls).map((m) => m.url), ['https://a.com/master.m3u8', 'https://a.com/360p.m3u8', 'https://a.com/720p.m3u8']);
});
