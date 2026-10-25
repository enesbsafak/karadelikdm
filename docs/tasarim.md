# Karadelik Download Manager: Tasarım

Bu belge Karadelik Download Manager'ın (KDM) mimarisini ve her önemli kararın gerekçesini anlatır. Kod sıfırdan,
bu depoda yazılmıştır; başka bir ürünün kodundan, tablolarından ya da sabitlerinden hiçbir şey alınmamıştır.

## Yığın

KaradelikDev ile aynı: **Tauri 2 + Rust** arka uç, **saf HTML/CSS/JS** arayüz, aynı renk ailesi (koyu tema,
turuncu vurgu, çerçevesiz pencere), aynı i18n düzeni (`ui/i18n/tr.json`, `en.json`), NSIS kurulum ve
SHA-256 doğrulamalı güncelleyici.

## 1. İndirme motoru (`crates/kdm-core/src/engine`)

### 1.1 Parçalı indirme

- İndirme tek bağlantıyla başlar. Bağlantı sayısı yarım saniyede bir ikiye katlanarak üst sınıra çıkar. Hız sınırı
  açıksa sınırın %85'ine ulaşınca artış durur.
- Yeni bağlantı, kalan aralığı en büyük olan parçanın üst yarısını alır.
- Bölme eşiği sabit bayt değil, **süre** ile ölçülür: kurban bağlantının kalan işi mevcut hızıyla 1 saniyeden
  kısa sürede bitecekse bölünmez. Yeni bağlantının maliyeti tek bir kurulumdur (TCP + TLS, tipik olarak
  100–300 ms); 1 saniyeden uzun sürecek işi ikiye bölmek bu maliyetten fazlasını kazandırır. Sabit bir bayt eşiği
  hızlı hatlarda gereksiz bölmeye, yavaş hatlarda geç bölmeye yol açardı. Alt sınır olarak 256 KiB uygulanır. (İlk
  denemede 3 saniye seçilmişti; testlerde 6 MB'lık bir dosyada iki bağlantıdan sonra bölmeyi durdurduğu görülünce
  1 saniyeye indirildi.)
- Ek bağlantı 200 yanıtı alırsa sunucu Range desteklemiyor sayılır ve tek akışa dönülür.
- Takılan bağlantı (belirli süre veri gelmezse) kapatılıp kaldığı yerden yeniden istenir. Yavaş bir
  bağlantının parçası, "en büyük kalan" kuralıyla doğal olarak başka bağlantılara bölünür.

### 1.2 Disk düzeni

- Tek bir `.kdm` dosyası açılır. Windows'ta seyrek (sparse) olarak işaretlenir, böylece dosyanın sonuna
  yazmak öncesini sıfırla doldurmaz.
- Her bağlantı kendi konumuna doğrudan yazar. Parça dosyası ve indirme sonunda birleştirme aşaması yoktur; bu,
  iki kat disk alanını ve ayrı bir birleştirme süresini önler.
- Bitince dosya yalnızca yeniden adlandırılır.
- İsteğe bağlı olarak SHA-256 / SHA-1 / MD5 özeti doğrulanır.

### 1.3 Devam ettirme (resume) ve bütünlük

- HTTP'nin kendi doğrulayıcıları kullanılır: `ETag` / `Last-Modified` saklanır ve devamda `If-Range` gönderilir.
  Sunucudaki dosya değiştiyse 200 gelir ve indirme kullanıcıya bildirilerek baştan başlar.
- `Accept-Ranges: none` destek yok demektir.
- `Content-Range` başlangıcı istenenle karşılaştırılır; uyuşmazsa bağlantı reddedilir.
- Parça durumu birkaç saniyede bir **önce dosya diske yazılıp (fsync) sonra** kaydedilir. Çökme durumunda en
  fazla son birkaç saniye yeniden indirilir.

### 1.4 Hız sınırı ve kota

- Hız sınırı **token bucket** ile yapılır: genel ve indirme başına iki kova vardır. Her yazmadan önce jeton
  alınır, bekleme kesilebilir. Uyku tahminine dayalı bir yöntemden daha düzgün bir hız eğrisi verir.
- Kota "H saatte X MB" biçimindedir ve dakikalık sayaçlardan oluşan bir halkayla ölçülür. Aşılınca indirmeler
  duraklar ve pencere kayınca otomatik devam eder.

### 1.5 Dosya adı

- RFC 6266 / 5987 `filename*` önceliklidir, ardından `filename`, ardından URL'nin son parçası (yüzde kodlaması çözülerek).
- Windows'ta geçersiz karakterler temizlenir, ayrılmış adlar (CON, NUL…) engellenir.
- Aynı adda dosya varsa `ad (1).uzantı` biçimi kullanılır.

### 1.6 Kategoriler

Sıkıştırılmış, Belgeler, Müzik, Programlar, Video ve Genel. Her birinin uzantı listesi ve klasörü vardır; klasörler
yerelleştirilir ve kullanıcı düzenleyebilir.

## 2. Kuyruk ve zamanlayıcı (`queue.rs`)

- Kuyruk başına eşzamanlı dosya sayısı, gün seçimi, başlatmadan bağımsız durdurma saati.
- Başarısız dosya üstel geri çekilmeyle yeniden denenir (30 s, 60 s, 120 s…).
- Bitiş eylemleri: hiçbiri / uygulamadan çık / uyku / hazırda bekletme / kapat. Kuyruk elle durdurulduysa
  çalışmazlar. Bilgisayarı etkileyen eylemlerden önce 60 saniyelik, iptal edilebilir bir geri sayım gösterilir.

## 3. Tarayıcı entegrasyonu (`crates/kdm-bridge`, `extension/`)

- **Kanal:** `127.0.0.1` üzerinde rastgele porta bağlanan bir WebSocket sunucusu kurulur. Port ve **paylaşılan
  gizli anahtar** native messaging üzerinden uzantıya verilir. Sabit portlu ve kimlik doğrulamasız bir yerel soket,
  yerel kötü amaçlı bir sayfanın veya sürecin indirme enjekte etmesine açık olurdu; KDM'de her mesaj anahtarla
  doğrulanır, `Origin` ve `Host` başlıkları denetlenir.
- **Protokol:** Sürümlü JSON mesajlar.
- **Yakalama:** Manifest V3 uyumlu `downloads.onDeterminingFilename` / `onCreated` ile tarayıcının kendi
  indirmesi iptal edilip uygulamaya devredilir.
- **Medya:** m3u8 / mpd / medya yanıtları engellemesiz webRequest ile tespit edilip açılır pencerede listelenir.
- **Uygulamayı başlatma:** Native host uygulamayı, kullanıcı bir şey yaptığında başlatır.

## 4. Medya akışları (`crates/kdm-media`)

- HLS ana/medya listesi ayrıştırılır, varyant seçilir.
- Segmentler paralel indirilir; **AES-128-CBC** çözme yapılır; `#EXT-X-MAP` ve `BYTERANGE` desteklenir.
- DASH için `SegmentTemplate` (`$Number$`, `$Time$`), `SegmentTimeline`, `SegmentList` ve `SegmentBase` desteklenir.
- Birleştirme ve kap dönüştürme için kullanıcının sisteminde bulunan veya Tercihler'den gösterilen **ffmpeg**
  kullanılır (`-c copy`, yeniden kodlama yok). ffmpeg yoksa HLS `.ts` olarak birleştirilir, DASH ise ayrı
  video ve ses dosyaları olarak teslim edilir.

## 5. FTP (`crates/kdm-ftp`)

- `TYPE I`, EPSV öncelikli, PASV yedek.
- `REST` ile parçalı / çok bağlantılı indirme yapılır.
- `MDTM` ve `SIZE` ile değişiklik denetlenir.
- FTPS: `ftps://` örtük TLS (990), `ftp://` için RFC 4217 `AUTH TLS` + `PBSZ 0` + `PROT P`. Bkz. §9.

## 6. Site Grabber (`crates/kdm-grabber`)

- Derinlik aynı site ve diğer siteler için ayrı tutulur ve yalnızca HTML sayfalarına uygulanır.
- Dosyalar tür deseni, boyut ve dahil/hariç yol filtrelerinden geçer.
- `robots.txt`'ye saygı (kapatılabilir), eşzamanlı istek sınırı ve istek arası gecikme vardır.

## 7. ZIP önizleme (`crates/kdm-zip`)

- İndirme başlamadan HTTP Range ile ZIP'in sonu okunur ve içerik listelenir.
- Kullanıcı **yalnızca seçtiği dosyaları** indirebilir: her giriş kendi aralığından çekilip açılır.

## 8. Güvenlik ve gizlilik

- **Güncelleme:** SHA-256 doğrulamalı indirilir (KaradelikDev ile aynı).
- **Telemetri yok.** İndirilen adresler, çerezler ve dosya adları hiçbir sunucuya gönderilmez.
- **Gömülü kimlik bilgisi yok.**
- **TLS:** rustls ve işletim sistemi sertifika doğrulayıcısı; sertifika denetimini kapatan bir ayar yoktur.
- **Antivirüs taraması:** İndirme sonrası isteğe bağlıdır. Windows'ta varsayılan komut Defender'dır
  (`MpCmdRun.exe -Scan -ScanType 3 -File`).

### Kapsam dışı (bilinçli)

| Özellik | Neden yok |
| --- | --- |
| Çekirdek sürücüsüyle ağ trafiği yakalama | Sürücü imzası ve EV sertifikası gerekir, hata mavi ekran demektir; tarayıcı uzantısı aynı işi görür |
| Eski tarayıcı eklenti modelleri (IE BHO, XPCOM) | Bu tarayıcılar artık yok |
| MMS, RTMP, çevirmeli ağ | Ölü veya kullanım dışı teknolojiler |
| Lisans, seri numarası, donanım parmak izi | Açık kaynak (MIT), gerekmez |
| Kullanım ya da adres raporlama | Gizlilik ihlali |

## 9. Uygulamada verilen ek kararlar

Belgenin ilk hali yazılırken öngörülmeyen, uygulama ve testlerde ortaya çıkan kararlar:

- **FTP, HTTP modeline çevrilir** (`engine/transport.rs`): REST ile ortadan başlayan aktarım "206 + Content-Range",
  baştan başlayan "200", MDTM ise If-Range karşılığıdır. Böylece parça bölme, geri katma ve "dosya değişti, baştan"
  mantığı FTP için ayrıca yazılmadı. FTP'de en çok 4 bağlantı açılır; sunucular IP başına bağlantıyı çoğunlukla sınırlar.
- **PASV adresi yok sayılır:** veri bağlantısı her zaman kontrol bağlantısının IP'sine açılır (curl varsayılanı).
  NAT arkasındaki sunucuların iç adres bildirmesi sorunu da böylece çözülür ve FTP bounce / SSRF önlenir.
- **FTP bağlantı zaman aşımı toplam bütçedir:** birçok adrese çözülen ve 21. portu süzen bir sunucu (gerçek bir
  denemede ftp.debian.org, 4 adres) adres başına 30 sn beklemeyle tek denemeyi 2 dakikaya, yeniden denemelerle
  12 dakikanın üstüne çıkarıyordu.
- **FTPS** (`kdm-ftp/src/tls.rs`): rustls + işletim sistemi doğrulayıcısı; sertifika denetimini kapatan bir ayar
  yoktur. Her FTP oturumunun kendi TLS oturum önbelleği vardır ve veri bağlantıları kontrol bağlantısının oturumunu
  sürdürür; paylaşılan önbellek paralel parçaların birbirinin oturumunu sürdürmesine yol açar ve aynı oturumu isteyen
  sunucular (vsftpd `require_ssl_reuse`) bunu reddeder. AUTH TLS, USER'dan önce gönderilir; 234 ile aynı pakette gelen
  düz metin reddedilir (STARTTLS enjeksiyonu). Sertifika ve TLS reddi kalıcı hatadır, yeniden denenmez. Açık TLS
  varsayılan olarak kapalıdır: "varsa" kipi öz imzalı sertifikalı sunucularda düz FTP'ye düşmek yerine hata verir.
  Gerçek denemede (2026-10-25) ftp.icm.edu.pl (vsftpd) üzerinden 24 MB, TLS zorunlu ve 4 bağlantıyla indi, SHA-256
  ftp.gnu.org'un HTTPS kopyasıyla aynı; TLS'siz ftp.halifax.rwth-aachen.de "varsa" kipinde düz FTP'ye düştü, "zorunlu"
  kipinde parola gönderilmeden durdu.
- **Medya indirmeleri motorun HTTP ajanını kullanır:** `kdm-media` ağa kendisi çıkmaz. Segmentler motorun çerez,
  referer, vekil ve hız sınırı ayarlarıyla iner; ilerleme segment sayısı olarak gösterilir. ffmpeg ayarı `none` ise
  ffmpeg bilerek kullanılmaz.
- **Tarayıcı köprüsünde uygulama yalnız kullanıcı bir şey yapınca başlatılır:** uzantının arka plandaki yeniden
  bağlanması uygulamayı açmaz; aksi halde kullanıcı uygulamayı kapattıktan saniyeler sonra uzantı yeniden açardı.
- **Köprü isteği beklemez:** uzantı birkaç saniye içinde yanıt alamazsa indirmeyi tarayıcıya bırakır ve dosya iki kez
  iner. Pencere açma ve indirme ekleme ayrı iş parçacığında yapılır.
- **Durum değişikliği kaydı kısılmaz:** ilerleme en çok saniyede bir kaydedilir. İlk sürümde bu kısma bitiş durumunu
  da yutuyordu; gerçek bir indirmede dosya tamamlandığı halde listede "indiriliyor" kalıyordu.
- **Bağlantı reddi:** Kapalı porta yeni bir indirme 6 yeniden deneme ve katlanan beklemeyle ~30 sn sonra hata
  veriyordu. Hiç veri almamış indirmede ret artık kalıcı hatadır (yanlış adres/port); yarım indirmede sunucu yeniden
  başlıyor olabileceği için yeniden denenir. Ayrım için HTTP ve FTP hata türleri zincirde korunur (`is_refused`).
- **BitTorrent** (`crates/kdm-torrent`, `engine/torrent.rs`): protokol (DHT, izleyiciler, eş bağlantısı, parça
  doğrulama) sıfırdan yazılmadı, olgun ve Apache-2.0 lisanslı librqbit kullanıldı; hatalı bir protokol uygulaması
  hem güvenlik hem veri bütünlüğü riskidir. Kararlar:
  - librqbit'in TLS özelliği reqwest 0.13 üzerinden rustls'a ikinci bir kripto sağlayıcısı (aws-lc-rs) ekliyordu;
    ureq ve kdm-ftp ring kullandığından aynı süreçte iki sağlayıcı olurdu. İzleyici/`.torrent` HTTPS'i için
    işletim sisteminin TLS'i (SChannel), SHA-1 için işletim sisteminin kripto API'si seçildi.
  - librqbit `..` ve ayırıcıları reddeder ama `C:` gibi sürücü önekli bileşeni ve `:` (NTFS alternatif akışı)
    denetlemez; `PathBuf::push("C:")` hedef klasörü değiştirir. Bu yollar her platformda reddedilir (test var).
  - librqbit'in ilerleme ve toplamı parça bazlıdır: seçili dosyayla parça paylaşan seçilmemiş dosyanın baytları da
    sayılır. Arayüze dosya bazlı değer gösterilir.
  - librqbit hız sınırlayıcısı bir isteği (16 KiB) tek seferde geçirebilmeli; daha düşük sınırda her istek hata
    verirdi. Torrentte en düşük sınır 16 KiB/sn'dir.
  - Bitmiş torrentin dosyaları librqbit'te yazma kipinde açık kalıyordu; gerçek denemede Gezgin ve `Get-FileHash`
    dosyayı açamadı. Paylaşma yoksa ya da oran dolunca torrent oturumdan çıkarılır, dosyalar serbest kalır.
  - Var olan aynı adlı klasör/dosyanın üstüne yazılmaz (librqbit var olan veriyi "devam" sanıp bozuk parçaları
    yeniden yazardı): çok dosyalıda klasör adı, tek dosyalıda ayrı alt klasör seçilir.
  - librqbit web seed (BEP 19) desteklemiyor; archive.org torrentleri bu yüzden hiç inmedi. Eşlerden 20 sn ilerleme
    yoksa eksik dosyalar HTTP ile `<ad>.kdmws`'e indirilir, torrent yeniden eklenince her parça özetle doğrulanır.
    Bozuk web seed verisinin kabul edilmediği test edildi.
  - Torrent oturumu tembel açılır; UPnP varsayılan kapalı; magnet/.torrent ilişkilendirmesi Windows'ta yalnız
    kullanıcı isteyince yazılır ve kaldırıcı yalnız bu kuruluma işaret eden kaydı siler. macOS'ta uygulama
    "Birlikte Aç" seçeneği olarak kaydolur, varsayılan olmaz.
- **Arayüz** (2026-10-25): Renkler ve `app.css` (KaradelikDev ile ortak dosya) korunarak `dm.css` ve ana pencere
  Apple'ın masaüstü diliyle yeniden yazıldı: tek parça başlık/araç çubuğu ve simge düğmeler, kategori ve kuyrukları
  gösteren kenar çubuğu, liste başlığında durum için bölmeli denetim ve sıralama, tablo yerine iki satırlı ve satır
  içi ilerlemeli liste, sağda ilerleme halkalı ayrıntı paneli, durum çubuğunda hız sınırı ve pano rozetleri. Diğer
  pencereler ortak bileşenlerin yeni hâlini alır. Karadelik DM'e özel ikon (olay ufkuna inen ok) aile ürünlerinden
  ayırt edilmek için eklendi.

## 10. Doğrulama

- `cargo test --workspace`: 250'yi aşkın test, ağ erişimi gerektirmez. Motor, yerel test sunucularına karşı çok
  bağlantılı indirme, durdur/devam (uygulama yeniden açılarak), Range'i yok sayan sunucu, sunucuda değişen dosya,
  boyutu bilinmeyen akış, boş dosya, kopan bağlantı, bağlantı reddi, hız sınırı ve özet doğrulamasıyla sınanır. Aynı
  senaryolar FTP ve HLS için de vardır. CI (`.github/workflows/ci.yml`) her itmede Windows ve macOS'ta derler,
  clippy'yi uyarıları hata sayarak ve bütün testleri çalıştırır.
- Uzantı tarafı `extension/test` altında Node testleriyle sınanır. `background.test.mjs`, `background.js`'i sahte
  tarayıcı API'siyle yükleyip yakalama ve sağ tık yolunu çalıştırır (yalnız `lib.js`'i sınayan testlerin göremediği,
  yakalamayı tamamen bozan bir hata bu testle yakalandı).
- Gerçek ağda denenenler: HTTPS (10 MB ve 1 MB dosyalar), FTP (ftp.gnu.org, SHA-256 curl ile aynı), FTPS (bkz. §9),
  tek örnek üzerinden komut satırı iletimi.
- **BitTorrent, gerçek ağda (2026-10-25):** yalnız bilgi özetinden oluşan izleyicisiz magnet (Debian 13.7 netinst)
  DHT ile çözüldü; 756 MB yaklaşık 40 sn'de, 1100'ü aşkın bilinen eşten indi; SHA-256 Debian'ın resmi `SHA256SUMS`
  değeriyle aynı. archive.org'daki bir torrent (10 dosya, eş vermiyor) web seed'e geçerek tamamlandı; her dosya
  archive.org'un HTTPS kopyasıyla bayt bayt aynı. İkisi de uygulamanın ekleme penceresi üzerinden yapıldı.
- **Kurulum programı ve gerçek tarayıcı:** NSIS paketi sessizce kuruldu. Uzantı gerçek Edge'e DevTools protokolünün
  `Extensions.loadUnpacked` komutuyla yüklendi ve kurulu native host üzerinden uygulamaya bağlandı. Tarayıcıda
  başlatılan indirmeler (HTTP ve HTTPS; `.deb`, `.zip`, `.bin`) yakalandı, uygulamaya devredildi ve tamamlandı
  (SHA-256 kaynakla aynı); tarayıcının kendi kopyası silindi. Kaldırıcı native host kayıtlarını temizledi.
  Betik: `extension/test/edge-e2e.mjs` (elle çalıştırılır).
- **Medya ve açılır pencere yolu, gerçek Edge'de** (`extension/test/edge-paths.mjs`): sayfanın yüklediği HLS listesi
  `webRequest` ile tespit edilip rozette gösterildi; açılır pencere akışı uygulamaya gönderdi, ekleme penceresi
  kaliteleri listeledi ve akış indirildi. "Tüm bağlantılar" isteği toplu ekleme penceresini doğru adreslerle açtı.
- **Sağ tık menüsü, gerçek Edge'de** (`extension/test/edge-menu.mjs` + `uia-click.ps1`): Edge'in yerel menüsündeki
  öğe Windows UI Automation ile çağrıldı (kullanıcı tıklamasıyla aynı yol).
- **Pencere pencere tasarım denetimi:** Uygulamanın bütün pencerelerinin ve 880 px'lik en dar ana pencerenin gerçek
  görüntüsü alındı; bulunan kusurlar (kesilen diyalog metni, pencereye sığmayan sekmeler, boş görünen parça çubuğu,
  kırpılan saat alanı) düzeltildi.
- **Başka bir indirme yöneticisiyle birlikte:** IDM kuruluyken, IDM'in listesindeki türlerde (ör. `.bin`) düz HTTP
  indirmesi tarayıcıda hiç başlamadı; uzantımız devreye giremedi. HTTPS'te ve IDM kaldırılınca aynı indirmeler
  yakalandı. Kullanıcı iki indirme yöneticisini birlikte kullanacaksa diğerinin tarayıcı entegrasyonunu kapatmalıdır.
