# Karadelik Download Manager tarayıcı uzantısı

Chromium (Chrome, Edge) ve Firefox için tek kaynaklı Manifest V3 uzantısı. Tarayıcı indirmelerini, bağlam
menüsünden seçilen bağlantıları ve sayfadaki medya akışlarını Karadelik Download Manager'a devreder.

## Yapı

```
extension/
  build.mjs            src/ → chromium/ ve firefox/ (paketlenmemiş yüklenebilir klasörler)
  src/
    manifest.base.json ortak manifest
    chromium.json      Chromium farkları: service_worker, sabit "key", minimum_chrome_version
    firefox.json       Firefox farkları: background.scripts, gecko kimliği
    background.js      bağlantı, yakalama, bağlam menüsü, medya tespiti
    lib.js             saf yardımcılar (testler doğrudan kullanır)
    popup.*            açılır pencere
    _locales/{en,tr}   arayüz metinleri
  test/                Node testleri
```

`chromium/` ve `firefox/` üretilen klasörlerdir, depoya girmez.

```sh
node extension/build.mjs                         # paketleri üret
node --test "extension/test/*.test.mjs"          # testler (çevrimdışı)
```

## Geliştirme kurulumu

- **Chrome / Edge:** `chrome://extensions` (Edge'de `edge://extensions`) → Geliştirici modu → "Paketlenmemiş öğe yükle"
  → `extension/chromium`. Manifestteki `key` sayesinde kimlik her makinede `kkifckmmeinedmmdokgacefiojpfbpfl` olur;
  native host manifesti bu kimliğe izin verir.
- **Firefox:** `about:debugging#/runtime/this-firefox` → "Geçici eklenti yükle" → `extension/firefox/manifest.json`.
  Kimlik `karadelik-dm@enesbsafak`. Kalıcı kurulum için eklentinin AMO'da imzalanması gerekir.

Uygulama ilk açılışta native host'u kaydeder (`kdm_bridge::register_native_hosts`). Kayıt yoksa uzantı
"Uygulama kapalı" gösterir ve indirmeler tarayıcıda kalır.

## Nasıl çalışır

1. **Keşif:** `runtime.connectNative("dev.karadelik.dm")` → `karadelikdm-nmh`. İstek
   `{"type":"discover","launch":bool}`, yanıt `{"ok":true,"port":…,"token":…}`. `launch:true` yalnız kullanıcı
   eyleminde (yakalanan indirme, bağlam menüsü, açılır penceredeki "Başlat") gönderilir. Arka plandaki yeniden
   bağlanma `launch:false` kullanır; kullanıcı uygulamayı kapattıysa uzantı onu kendiliğinden geri açmaz.
2. **Bağlantı:** `ws://127.0.0.1:<port>`, ilk mesaj `{"type":"hello","token":…,"version":…}`. Yanıt
   `{"type":"hello","ok":true,"protocol":1,"app_version":…,"config":{enabled,exts,min_size_kb}}`.
   Sonraki her istek bir `id` taşır, yanıt `{"type":"result","id":…,"ok":…,"error"?:…}`'dur. Uygulama ayarları
   değişince `{"type":"config",…}` iter. Kopan bağlantı üstel geri çekilmeyle (2 sn → 60 sn) yeniden denenir.
3. **Uyku:** Chromium'da service worker 30 sn boşta kalınca durur. Bağlıyken 20 sn'de bir `ping` gönderilir
   (Chrome 116+ WebSocket trafiğini etkinlik sayar). Geri çekilme durumu ve medya listeleri `storage.session`'da
   tutulur; uyanınca kalınan yerden devam edilir. Token hiçbir yere yazılmaz, her uyanışta host'tan yeniden alınır.

### Yakalama

- Chromium: `downloads.onDeterminingFilename`. Bu noktada dosya adı ve boyut bellidir ve dosya henüz hedefe
  yazılmamıştır; karar verilene kadar indirme bekletilir.
- Firefox: `onDeterminingFilename` yok, `downloads.onCreated` kullanılır.
- Koşullar: uygulamada yakalama açık, uzantı listede, boyut ≥ en küçük boyut (bilinmiyorsa yakalanır),
  adres http(s), gizli pencere değil, site hariç tutulmamış, yakalama duraklatılmamış.
- Uygulama isteği onaylarsa tarayıcı indirmesi `cancel` + `erase` ile kaldırılır. Uygulamaya ulaşılamazsa ya da
  hata dönerse indirme tarayıcıda kalır.
- Çerezler `cookies.getAll({url})` ile `Cookie` başlığına çevrilir; referer ve user agent eklenir.

### Bağlam menüsü

- "Karadelik ile indir": bağlantı, resim, video, ses.
- "Tüm bağlantıları Karadelik ile indir": `scripting.executeScript` ile tüm çerçevelerdeki `a[href]` toplanır,
  http(s) olmayanlar ve tekrarlar atılır. `Links` mesajı tek bir çerez alanı taşıdığından çerezler yalnız tüm
  bağlantılar aynı ana makinedeyse gönderilir. Aksi halde bir sitenin çerezleri başka sitelere sızardı.

### Medya

`webRequest.onHeadersReceived` (engellemesiz, yalnız okur) ile `.m3u8` / `.mpd` adresleri ve HLS, DASH, `video/*`,
`audio/*` içerik türleri sekme başına listelenir. Akış parçaları (`.ts`, `.m4s`, `video/mp2t`…) ve 512 KB'tan
küçük tek dosyalar atlanır. Araç çubuğu rozeti sayıyı gösterir. Açılır pencerede bir öğeye tıklanınca `Media`
gönderilir.

## İzinler ve gerekçeleri

| İzin | Neden |
| --- | --- |
| `downloads` | Tarayıcı indirmesini görmek, iptal etmek ve geçmişten silmek. |
| `contextMenus` | "Karadelik ile indir" ve "Tüm bağlantıları indir" menüleri. |
| `cookies` | Oturum gerektiren indirmelerin uygulamada da çalışması için ilgili adresin çerezlerini iletmek. |
| `nativeMessaging` | Köprünün port ve token bilgisini almak, kapalıysa uygulamayı başlatmak. |
| `storage` | Hariç tutulan siteler, son yapılandırma (`local`); duraklatma, medya listeleri, geri çekilme (`session`). |
| `scripting` | "Tüm bağlantıları indir"de sayfadaki bağlantıları okumak. Sayfaya kalıcı betik eklenmez. |
| `webRequest` | Medya yanıtlarını başlıklarından tanımak. Engelleme yapılmaz, istek değiştirilmez. |
| `activeTab` | Kullanıcı menüye tıkladığında o sekmeye erişim; `<all_urls>` iptal edilirse de menü çalışsın. |
| `<all_urls>` (host) | Her sitedeki medya yanıtlarını görmek, çerezleri okumak ve sekme adresini/başlığını referer olarak kullanmak. |

İçerik betiği (`content_scripts`), uzak kod ve `webRequestBlocking` yoktur. Hiçbir veri internete gönderilmez;
tüm trafik `127.0.0.1` üzerindeki uygulamaya gider (Firefox manifestinde `data_collection_permissions: none`).

## Güvenlik notları

- Yerel soket yalnız `127.0.0.1`'e bağlanır ve rastgele porttadır. El sıkışmada `Origin` (uzantı origin'leri) ve
  `Host` (DNS rebinding) denetlenir. İlk mesaj token'lı `hello` değilse bağlantı kapanır. Böylece sıradan bir web
  sayfası sokete bağlanıp indirme enjekte edemez.
- Firefox her kurulumda rastgele bir `moz-extension://<uuid>` atadığı için orada origin öneki kabul edilir. Token'ı
  ise yalnız native host manifestindeki `allowed_extensions` listesinde olan uzantı alabilir.
- Açılır pencere mesajları yalnız uzantının kendi sayfalarından kabul edilir (`sender.id`, sekmesiz).
