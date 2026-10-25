# Mağaza başvuruları: Chrome Web Store, Edge Eklentileri, Firefox (AMO)

Paketler: `node extension/build.mjs --store` → `extension/store/karadelikdm-chromium-<sürüm>.zip` (Chrome ve Edge),
`extension/store/karadelikdm-firefox-<sürüm>.zip` (Firefox). Chromium paketinde manifestteki `key` yoktur; mağazalar
kendi kimliğini verir. **Yayınlandıktan sonra** Chrome ve Edge'in verdiği kimlikler
`crates/kdm-bridge/src/lib.rs` içindeki `STORE_CHROME_IDS` listesine eklenip uygulamanın yeni sürümü çıkarılmalıdır;
aksi halde mağaza uzantısı uygulamaya bağlanamaz. Firefox kimliği (`karadelik-dm@enesbsafak`) aynı kalır.

Kod küçültülmemiş ve paketlenmemiştir (dosyalar `extension/src/` ile aynıdır); AMO için ayrıca kaynak kodu
yüklemek gerekmez.

## Ortak alanlar

- **Ad:** Karadelik Download Manager
- **Kategori:** Chrome: Productivity (Workflow & Planning) · Edge: Productivity · Firefox: Download Management
- **Web sitesi:** https://github.com/enesbsafak/karadelikdm
- **Destek:** https://github.com/enesbsafak/karadelikdm/issues
- **Gizlilik politikası URL'si:** https://github.com/enesbsafak/karadelikdm/blob/master/PRIVACY.md
- **Lisans:** MIT
- **Ekran görüntüleri:** `docs/store/screenshot-1.png`, `screenshot-2.png` (1280×800)
- **Simge:** `extension/src/icons/logo.png` (128×128)

## Kısa açıklama (en çok 132 karakter)

- **EN:** Send browser downloads, magnet links and video streams to the Karadelik Download Manager desktop app.
- **TR:** Tarayıcı indirmelerini, magnet bağlantılarını ve video akışlarını Karadelik Download Manager'a gönderir.

## Ayrıntılı açıklama

### English

Karadelik Download Manager is a free, open-source download manager for Windows and macOS. This extension connects
your browser to the desktop app (install it first from https://github.com/enesbsafak/karadelikdm/releases).

- Downloads of the file types you choose are handed to the app instead of the browser: multi-connection,
  resumable, with queues, a scheduler and speed limits.
- "Download with Karadelik" in the right-click menu for links, images, video and magnet links; "Download all links"
  for the whole page.
- Finds HLS and MPEG-DASH video streams on the page and sends them to the app with quality selection.
- Pause capture for the session or exclude a site with one click.

Everything stays on your computer: the extension talks only to the app on 127.0.0.1, protected by a secret
token. No analytics, no telemetry, no account.

### Türkçe

Karadelik Download Manager, Windows ve macOS için ücretsiz ve açık kaynaklı bir indirme yöneticisidir. Bu uzantı
tarayıcınızı masaüstü uygulamasına bağlar (önce uygulamayı https://github.com/enesbsafak/karadelikdm/releases
adresinden kurun).

- Seçtiğiniz dosya türlerinin indirmeleri tarayıcı yerine uygulamaya geçer: çok bağlantılı, kaldığı yerden devam
  eden, kuyruk, zamanlayıcı ve hız sınırıyla.
- Sağ tık menüsünde bağlantı, resim, video ve magnet bağlantıları için "Karadelik ile indir"; tüm sayfa için "Tüm
  bağlantıları indir".
- Sayfadaki HLS ve MPEG-DASH video akışlarını bulur, kalite seçimiyle uygulamaya gönderir.
- Tek tıkla yakalamayı oturum boyunca duraklatın ya da bir siteyi hariç tutun.

Her şey bilgisayarınızda kalır: uzantı yalnız 127.0.0.1 üzerindeki uygulamayla, gizli bir anahtarla korunan
bağlantı üzerinden konuşur. Analitik, telemetri ya da hesap yoktur.

## Tek amaç (Chrome "single purpose")

Send downloads, magnet links and media streams from the browser to the Karadelik Download Manager desktop app
installed on the user's computer.

## İzin gerekçeleri (Chrome "permission justification", Edge ve AMO incelemesi)

| İzin | Gerekçe (EN, mağazaya bu yazılır) |
| --- | --- |
| `downloads` | Detects a download the user started and, if its file type is on the user's capture list, cancels it in the browser after the desktop app has accepted it. |
| `nativeMessaging` | Finds the locally installed Karadelik Download Manager app (its port and secret token) and starts it when the user sends it a download. |
| `cookies` | Reads the cookies for the address being downloaded and passes them to the local app, so downloads that require the user to be signed in still work. Cookies are never sent anywhere else. |
| `contextMenus` | Adds "Download with Karadelik" and "Download all links with Karadelik" to the right-click menu. |
| `scripting` | When the user chooses "Download all links", reads the link addresses on the current page. Runs only on that user action. |
| `webRequest` | Observes (does not block or modify) response headers to detect HLS/DASH playlists and media files, so the extension window can list them. |
| `storage` | Keeps the extension's own settings: excluded sites, paused state, the last capture settings received from the app. |
| `activeTab` | Acts on the tab the user is interacting with from the extension window and the context menu. |
| Host permission `<all_urls>` | Downloads and media can come from any site; reading the download's cookies and observing media responses requires access to that site. |

**Uzak kod:** Yok. Bütün kod pakettedir; uzantı yalnız 127.0.0.1 üzerindeki yerel uygulamaya bağlanır.

## Veri kullanımı beyanı (Chrome "Privacy practices")

- Toplanan veri türleri: **Yok** işaretlenir. (Uzantı bilgiyi yalnız kullanıcının kendi bilgisayarındaki uygulamaya
  iletir; geliştiriciye ya da üçüncü tarafa aktarmaz.)
- Onaylanacak üç beyan: veriler satılmaz; ana işlev dışında kullanılmaz; kredi değerlendirmesi için kullanılmaz.

Firefox paketinin manifestinde `data_collection_permissions: { required: ["none"] }` beyanı vardır.

## İnceleyici notu (AMO ve Edge "notes for certification")

This extension is the browser half of a desktop download manager. To test it end to end, install the app from
https://github.com/enesbsafak/karadelikdm/releases/latest (Windows installer; the app registers the native
messaging host on first start). Then start a download of a `.zip` or `.deb` file: it is handed to the app. Without
the app, the extension shows "App closed" and leaves downloads to the browser. Source code:
https://github.com/enesbsafak/karadelikdm (folder `extension/`); files are not minified or bundled.
