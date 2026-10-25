# Privacy Policy / Gizlilik Politikası

_Last updated / Son güncelleme: 2026-10-25_

## English

Karadelik Download Manager (the desktop app) and its browser extension do not collect, store on any server, sell or
share personal data. There is no analytics, telemetry or advertising.

**What the extension handles, and where it goes**

- When a download you start matches the file types you chose, the extension passes its address, file name, size,
  referring page, your browser's user agent and the cookies for that address to the Karadelik Download Manager app
  **running on your own computer**. Cookies are needed so downloads that require you to be signed in keep working.
- Media playlist addresses (HLS / DASH) seen on a page are kept in memory for that tab only, so the extension's
  window can list them.
- The extension stores its own settings (sites you excluded, whether capture is paused) in the browser's local
  extension storage.

All of this travels only between the extension and the desktop app over a local connection (`127.0.0.1`),
authenticated with a secret token. Nothing is sent to us or to any third party.

**What the desktop app sends over the network**

Only the downloads you ask for (to the servers hosting them), BitTorrent traffic for torrents you add (your IP
address is visible to peers and trackers, as with any BitTorrent client), and an optional update check that reads
`latest.json` from this project's GitHub releases.

**Contact:** open an issue at https://github.com/enesbsafak/karadelikdm/issues

## Türkçe

Karadelik Download Manager (masaüstü uygulaması) ve tarayıcı uzantısı kişisel veri toplamaz, hiçbir sunucuda
saklamaz, satmaz ya da paylaşmaz. Analitik, telemetri ya da reklam yoktur.

**Uzantının işlediği bilgiler ve gittiği yer**

- Başlattığınız bir indirme seçtiğiniz dosya türlerinden biriyse uzantı; adresini, dosya adını, boyutunu, geldiği
  sayfayı, tarayıcınızın kimlik bilgisini (user agent) ve o adresin çerezlerini **kendi bilgisayarınızda çalışan**
  Karadelik Download Manager uygulamasına iletir. Çerezler, oturum açmayı gerektiren indirmelerin çalışması içindir.
- Bir sayfada görülen medya listesi adresleri (HLS / DASH) yalnız o sekme için bellekte tutulur ve uzantı
  penceresinde listelenir.
- Uzantı kendi ayarlarını (hariç tuttuğunuz siteler, yakalamanın duraklatılıp duraklatılmadığı) tarayıcının yerel
  uzantı deposunda saklar.

Bunların tümü yalnız uzantı ile masaüstü uygulaması arasında, gizli bir anahtarla doğrulanan yerel bir bağlantı
(`127.0.0.1`) üzerinden gider. Bize ya da herhangi bir üçüncü tarafa hiçbir şey gönderilmez.

**Masaüstü uygulamasının ağa gönderdikleri**

Yalnız istediğiniz indirmeler (dosyayı barındıran sunuculara), eklediğiniz torrentlerin BitTorrent trafiği (her
BitTorrent istemcisinde olduğu gibi IP adresiniz eşlere ve izleyicilere görünür) ve bu projenin GitHub sürümlerinden
`latest.json` okuyan isteğe bağlı güncelleme denetimi.

**İletişim:** https://github.com/enesbsafak/karadelikdm/issues adresinde bir kayıt açın.
