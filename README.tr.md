<div align="center">

<img src="ui/img/logo.png" width="96" alt="Karadelik Download Manager logosu">

[English](README.md) · **Türkçe**

# Karadelik Download Manager

**Windows ve macOS için hızlı, açık kaynaklı indirme yöneticisi.**
Çok bağlantılı ve kaldığı yerden devam eden indirmeler, kuyruklar ve zamanlayıcı, HLS/DASH akışları, FTP, BitTorrent,
Site Grabber, ZIP önizleme ve tarayıcı entegrasyonu tek pencerede. Telemetri yoktur.

![Lisans: MIT](https://img.shields.io/badge/lisans-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011%20x64-0078d6)
![Tauri 2](https://img.shields.io/badge/Tauri-2-24c8db)
![Rust](https://img.shields.io/badge/Rust-stable-dea584)

</div>

---

## Özellikler

**İndirme motoru**
- Tek bağlantıyla başlar, sunucu destekliyorsa bağlantı sayısını yarım saniyede bir ikiye katlayarak artırır (varsayılan en çok 8, ayarlanabilir 1–32).
- Bölme kararı süreye göre verilir: kalan işi 1 saniyeden uzun sürecek bağlantının aralığı ikiye bölünür. Bu sayede
  bağlantı sayısı hem hızlı hem yavaş hatlarda gereksiz yere artmaz.
- Tek bir seyrek `.kdm` dosyasına doğrudan yazar: parça dosyaları ve indirme sonunda birleştirme aşaması yoktur.
- **Güvenli devam:** `ETag`/`Last-Modified` + `If-Range` ile; sunucudaki dosya değiştiyse fark edilir ve dosya baştan
  indirilir. Uygulama kapanıp açılsa da indirme kaldığı yerden sürer.
- Range desteklemeyen ya da destekliyor görünüp yok sayan sunucuları algılar ve tek bağlantıya geçer.
- Takılan veya kopan bağlantıyı kaldığı yerden yeniden ister. Hiç veri almamış bir indirmede bağlantı reddedilirse
  (yanlış adres ya da port) hemen hata verir; yarım indirmede sunucu yeniden başlıyor olabileceği için yeniden dener.
- Genel ve indirme başına hız sınırı (token bucket).
- İsteğe bağlı SHA-256 / SHA-1 / MD5 doğrulaması.
- HTTP Basic ve Digest kimlik doğrulama; HTTP, SOCKS4/4a/5 vekil sunucu.
- `Content-Disposition` (RFC 6266/5987) ile dosya adı; Windows için güvenli ad, aynı ad varsa `ad (1).uzantı`.

**FTP**
- EPSV / PASV, `REST` ile çok bağlantılı indirme ve devam.
- `MDTM` ile sunucudaki dosyanın değiştiğini anlama.
- Veri bağlantısı her zaman kontrol bağlantısının adresine açılır; kötü niyetli bir sunucu istemciyi başka bir makineye yönlendiremez.
- FTPS: `ftps://` (örtük TLS) ve `ftp://` için açık TLS (`AUTH TLS`); Tercihler > Bağlantı'dan kapalı, "varsa" ya da
  "zorunlu" seçilir. Sunucu sertifikası her zaman işletim sisteminin doğrulayıcısıyla denetlenir; veri bağlantıları
  kontrol bağlantısının TLS oturumunu sürdürür (vsftpd, ProFTPD ve FileZilla Server bunu ister).

**BitTorrent ve magnet**
- Magnet bağlantıları (izleyicisiz olanlar DHT ile), `.torrent` dosyaları (bilgisayardan ya da web adresinden).
- Ekleme penceresinde dosya listesi: istenen dosyalar seçilir, yalnız onlar iner.
- Metadata bir kez alınınca saklanır; uygulama kapanıp açılınca var olan veri parça parça doğrulanır ve indirme
  kaldığı yerden sürer.
- **Web seed (BEP 19):** Eşlerden 20 saniye veri gelmezse torrentin HTTP kaynağına geçilir (archive.org gibi). İnen
  veri torrentteki parça özetleriyle doğrulanır; bozuk parça kabul edilmez.
- Paylaşma oranı (varsayılan 1.0, 0: paylaşma yok), gönderme sınırı, dinleme portu, DHT, yerel ağ araması; UPnP
  isteğe bağlı ve varsayılan kapalı. Paylaşma bitince dosyalar serbest bırakılır.
- Torrent oturumu ilk torrent başlatılana kadar açılmaz; torrent kullanmayan birinin bilgisayarında port dinlenmez.
- Windows'ta güvenli olmayan dosya yolu içeren torrentler (`C:` gibi sürücü önekli ya da `:` içeren bileşen)
  reddedilir.
- Pano, tarayıcı uzantısı (sağ tık ve "tüm bağlantılar") ve komut satırı magnet bağlantılarını tanır. Tercihler >
  BitTorrent'ten magnet ve `.torrent` için varsayılan uygulama olunabilir (yalnız kullanıcı isteyince).

**Akışlar (HLS / MPEG-DASH)**
- Kalite ve ses dili seçimi.
- AES-128 şifreli HLS.
- `SegmentTemplate`, `SegmentTimeline`, `SegmentList`, `SegmentBase` (DASH).
- Paralel segment indirme; durdurup devam edince inmiş segmentler atlanır.
- [ffmpeg](https://ffmpeg.org) varsa tek MP4'e birleştirir (yeniden kodlama yapılmaz). Yoksa `.ts` ya da ayrı ses/görüntü dosyaları olarak bırakır.

**Kuyruklar ve zamanlayıcı**
- Birden çok kuyruk; kuyruk başına aynı anda indirilecek dosya sayısı.
- Gün ve saat seçimli başlatma/durdurma, "yalnızca bir kez".
- Başarısız dosyayı üstel bekleme ile yeniden dener (30 sn, 1 dk, 2 dk…).
- Kuyruk bitince: uygulamadan çık, uyku, hazırda bekletme ya da bilgisayarı kapat. Bilgisayarı etkileyen eylemlerden
  önce 60 saniyelik, iptal edilebilir geri sayım gösterilir.
- **İndirme kotası:** "son H saatte en çok X MB"; aşılınca indirmeler durur, pencere kaydıkça kendiliğinden devam eder.

**Tarayıcı entegrasyonu** (Chrome, Edge, Firefox)
- İndirmeleri tarayıcıdan devralır; çerez, referer ve kullanıcı ajanı birlikte gelir.
- Sağ tık menüsü: "Karadelik ile indir", "Tüm bağlantıları Karadelik ile indir".
- Sayfadaki HLS/DASH ve medya akışlarını tespit eder, uzantı simgesinde listeler.
- Uygulamayla yerel bir WebSocket üzerinden konuşur. Bağlantı her açılışta değişen bir anahtarla doğrulanır ve yalnızca
  uzantının kendi origin'inden kabul edilir; sıradan web sayfaları indirme enjekte edemez.

**Diğer**
- Kategoriler (Sıkıştırılmış, Belgeler, Müzik, Programlar, Video): dosyalar uzantısına göre ilgili klasöre.
- **Site Grabber:** siteyi derinlik, dosya türü, yol ve boyut kurallarıyla tarar; `robots.txt` kurallarına uyar.
- **ZIP önizleme:** arşivin içeriğini indirmeden görür, yalnızca seçtiğiniz dosyaları indirip çıkarır.
- Pano izleme, sürükle-bırak, yapıştır (Ctrl+V).
- İndirme sonrası antivirüs taraması (varsayılan Windows Defender).
- Bildirim alanı simgesi, Türkçe ve İngilizce arayüz.
- SHA-256 doğrulamalı otomatik güncelleme.

## Gizlilik

Karadelik DM hiçbir kullanım verisi göndermez. İndirdiğiniz adresler, çerezler ve dosya adları yalnızca bu
bilgisayarda kalır. Ağa çıkan tek kendi isteği, isteğe bağlı güncelleme denetimidir (GitHub'daki `latest.json`).
BitTorrent'in doğası gereği, torrent indirirken IP adresiniz bağlandığınız eşlere ve izleyicilere görünür.

## Kurulum

1. [Sürümler](https://github.com/enesbsafak/karadelikdm/releases/latest) sayfasından `KaradelikDM-<sürüm>-setup.exe`'yi indirip çalıştırın.
   Kurulum kullanıcı başınadır, yönetici izni gerekmez.
2. Tarayıcı uzantısı:
   - **Chrome / Edge:** [Chrome Web Store](https://chromewebstore.google.com/detail/karadelik-download-manage/ajflkdfogcikaobkhalblmmahlpdbmee) ya da uygulamada Tercihler > Tarayıcı > "Mağazadan ekle". Edge, aynı
     sayfadan "Diğer mağazalardan uzantılara izin ver" dedikten sonra kurar.
   - **Firefox:** mağaza onayı bekleniyor. O zamana kadar yalnız geçici olarak: `about:debugging` → "Geçici eklenti yükle" →
     kaynakta `node extension/build.mjs` ile üretilen `extension/firefox/manifest.json`.
   - Önceki bir sürümden paketlenmemiş kopyayı elle kurduysanız, mağazadan kurduktan sonra onu kaldırın.

   Uzantı, uygulama ilk açıldığında tarayıcılara kaydedilen yardımcı programla (`karadelikdm-nmh.exe`) uygulamayı bulur.

## Komut satırı

```text
karadelikdm <adres> [adres...] [--dir KLASÖR] [--name AD] [--queue KUYRUK] [--start | --later]
```

Adres `http(s)://`, `ftp(s)://`, `magnet:?xt=urn:btih:…` ya da bir `.torrent` dosyasının yolu olabilir.

`--start` ve `--later` verilmezse ekleme penceresi açılır. Uygulama zaten açıksa adresler çalışan örneğe iletilir.
Eski betikler için `/d ADRES /p KLASÖR /f AD /n` (hemen başlat) ve `/a` (kuyruğa ekle) de kabul edilir.

## Klasörler

| Ne | Nerede |
| --- | --- |
| Ayarlar, indirme listesi, günlük | `%APPDATA%\KaradelikDM` (`settings.json`, `downloads.json`, `kdm.log`) |
| İndirilen dosyalar | Tercihler > Kayıt (varsayılan: İndirilenler, kategori alt klasörleri) |
| Yarım indirme | Hedef klasörde `<ad>.kdm` |
| Torrent metadata'sı, DHT durumu | `%APPDATA%\KaradelikDM\torrents` |

`downloads.json` her kayıttan önce `.bak` olarak yedeklenir; ana dosya bozulursa yedekten okunur.

## Bilinen sınırlamalar

- Aktif mod FTP (PORT) ve FTPS istemci sertifikası yok; FTP vekil sunucu kullanmaz. "Varsa" kipinde sertifikası
  doğrulanamayan (öz imzalı, FTP'de yaygın) sunucuda indirme sessizce düz FTP'ye düşmez, hata verir; bu yüzden açık
  TLS varsayılan olarak kapalıdır.
- DRM korumalı ve SAMPLE-AES akışlar, canlı yayınlar ve `blob:` / MSE ile oynatılan videolar (ör. YouTube) indirilemez.
- POST formuyla başlayan tarayıcı indirmeleri GET olarak devredilir ve başarısız olabilir.
- UTF-8 bayrağı olmayan ZIP arşivlerindeki adlar CP437 olarak okunur. Türkçe Windows'ta oluşturulmuş eski arşivlerde
  ş, ğ, ı harfleri yanlış görünebilir.
- Site Grabber sayfaları sırayla getirir; JavaScript ile oluşturulan bağlantıları ve CSS `url()` bağlantılarını görmez.
- Aynı bilgisayarda IDM kuruluysa, IDM'in sürücüsü kendi listesindeki dosya türlerini tarayıcıdan önce yakalar ve
  uzantımız devreye giremez. İkisi birlikte kullanılacaksa IDM'in tarayıcı entegrasyonunu kapatın.
- BitTorrent: yalnız BitTorrent v1 (`btih`) desteklenir; v2'ye özgü (`btmh`) magnet bağlantıları açılmaz. Protokol
  şifrelemesi (MSE/PE) yok. Torrentlere indirme başına hız sınırı uygulanmaz (genel sınır uygulanır). Paylaşma
  yalnız uygulama açıkken sürer. Web seed dosya bazlıdır ve yalnız eşlerden ilerleme olmayınca devreye girer.
  macOS'ta uygulama magnet ve `.torrent` için "birlikte aç" seçeneği olarak kayıtlıdır, varsayılan olmaz.
- macOS: derleme, clippy ve bütün testler CI'da (`.github/workflows/ci.yml`) her itmede macOS'ta çalışır ve geçer;
  uygulama penceresi, tepsi ve tarayıcı uzantısı bir Mac'te elle denenmedi. Bu Windows makinede çapraz derleme mümkün
  değil (C bağımlılıkları Apple SDK başlıklarını ister).

## Kaynaktan derleme

Gerekenler: [Rust](https://rustup.rs) (stable), [Node.js](https://nodejs.org) 20+, WebView2 (Windows 10/11'de yüklü gelir).

```powershell
npm install
cargo test --workspace                     # 220+ test, ağ erişimi gerekmez
cargo build -p kdm-nmh
copy target\debug\karadelikdm-nmh.exe src-tauri\binaries\karadelikdm-nmh-x86_64-pc-windows-msvc.exe
npm run dev                                # geliştirme
powershell -ExecutionPolicy Bypass -File scripts\release.ps1   # kurulum programı + latest.json
```

### Mimari

| Crate | Görev |
| --- | --- |
| `crates/kdm-core` | İndirme motoru, kuyruk/zamanlayıcı, kota, kategoriler |
| `crates/kdm-ftp` | FTP istemcisi |
| `crates/kdm-torrent` | BitTorrent / magnet ([librqbit](https://github.com/ikatson/rqbit) üstünde), web seed adresleri, yol denetimi |
| `crates/kdm-media` | HLS/DASH ayrıştırma, segment indirme, ffmpeg ile birleştirme |
| `crates/kdm-grabber` | Site tarayıcı, `robots.txt` |
| `crates/kdm-zip` | Uzak ZIP listeleme ve seçmeli çıkarma |
| `crates/kdm-bridge`, `crates/kdm-nmh` | Tarayıcı köprüsü ve native messaging yardımcısı |
| `src-tauri` | Tauri uygulaması: pencereler, tepsi, komutlar |
| `ui` | Arayüz (HTML/CSS/JS) |
| `extension` | Chrome/Edge/Firefox uzantısı |

Tasarım kararları ve gerekçeleri: [`docs/tasarim.md`](docs/tasarim.md).

## Lisans

[MIT](LICENSE) © 2026 enesbsafak

BitTorrent protokolü için [librqbit](https://github.com/ikatson/rqbit) (Apache-2.0) kullanılır.
