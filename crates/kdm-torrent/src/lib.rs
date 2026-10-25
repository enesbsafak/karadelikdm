//! BitTorrent ve magnet indirmeleri. Protokol işi (DHT, izleyiciler, eş bağlantıları, parça doğrulama) librqbit'tedir;
//! bu crate onu motorun iş parçacığı tabanlı dünyasına eşzamanlı bir API ile bağlar ve librqbit'in denetlemediği
//! şeyleri denetler (Windows'ta sürücü önekli ya da `:` içeren dosya yolları).
//!
//! İstemci kendi tokio çalışma zamanını taşır; ilk torrent başlatılana kadar oluşturulmaz, böylece torrent
//! kullanmayan birinin bilgisayarında port açılmaz.

use anyhow::{anyhow, bail, Context, Result};
use librqbit::api::TorrentIdOrHash;
use librqbit::dht::{DhtPersistenceConfig, Id20};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerOptions, Magnet, Session, SessionOptions,
    TorrentStatsState, ValidatedTorrentMetaV1Info,
};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// Oturum ayarları. Port ve DHT yalnız oturum açılırken uygulanır.
#[derive(Clone, Debug)]
pub struct Options {
    /// Gelen bağlantılar için TCP portu; 0: işletim sistemi seçer, `None`: dinleme yok (yalnız giden bağlantı)
    pub listen_port: Option<u16>,
    pub dht: bool,
    /// Yönlendiricide port açma (UPnP). Ağdaki bir cihazın kendiliğinden port açması güvenlik riski olduğu için
    /// varsayılan kapalıdır.
    pub upnp: bool,
    /// Yerel ağda eş arama (BEP 14)
    pub lsd: bool,
    pub trackers: bool,
    /// DHT durum dosyası buraya yazılır
    pub state_dir: PathBuf,
}

impl Options {
    pub fn new(state_dir: PathBuf) -> Self {
        Options { listen_port: Some(0), dht: true, upnp: false, lsd: true, trackers: true, state_dir }
    }
}

pub enum Source<'a> {
    Magnet(&'a str),
    /// `.torrent` dosyasının içeriği
    File(&'a [u8]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// `/` ile ayrılmış göreli yol (çok dosyalı torrentte kök klasör adı dahil değildir)
    pub path: String,
    pub size: u64,
    /// BEP 47 dolgu dosyası: parçaları hizalamak için vardır, kullanıcıya gösterilmez
    pub padding: bool,
}

#[derive(Clone, Debug)]
pub struct Metadata {
    /// 40 karakter küçük harf onaltılık (BTv1 bilgi özeti)
    pub info_hash: String,
    pub name: String,
    pub files: Vec<FileEntry>,
    pub total: u64,
    /// Çok dosyalı torrent: dosyalar `name` adlı klasöre yazılır
    pub multi_file: bool,
    pub private: bool,
    /// Kaydedilebilir `.torrent` içeriği; yeniden başlatmada metadata ağdan tekrar istenmez
    pub torrent: Vec<u8>,
    /// BEP 19 web seed adresleri (`url-list`, yalnız http/https)
    pub web_seeds: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Var olan veri doğrulanıyor
    Checking,
    Live,
    Paused,
    Error,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub state: State,
    /// Seçili dosyalardan inmiş ve doğrulanmış bayt
    pub done: u64,
    /// Seçili dosyaların toplamı
    pub total: u64,
    pub uploaded: u64,
    /// bayt/sn
    pub down_speed: u64,
    pub up_speed: u64,
    /// Bağlı eş
    pub peers: u32,
    /// Bilinen eş
    pub seen: u32,
    pub finished: bool,
    pub error: Option<String>,
    /// Dosya başına inmiş bayt (torrentteki sırayla; metadata yokken boş)
    pub files: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct AddOptions {
    /// `None`: tüm dosyalar
    pub only_files: Option<Vec<usize>>,
    pub paused: bool,
    /// Testler ve bilinen eşler için
    #[doc(hidden)]
    pub initial_peers: Vec<SocketAddr>,
}

/// Torrentte uygulanabilen en düşük hız sınırı (bayt/sn)
pub const MIN_LIMIT: u64 = 16 * 1024;

pub struct Client {
    rt: tokio::runtime::Runtime,
    session: Arc<Session>,
}

impl Client {
    /// Oturumu açar. `default_dir` yalnız librqbit'in zorunlu varsayılanıdır; her torrent kendi klasörünü verir.
    pub fn start(opts: Options, default_dir: PathBuf) -> Result<Client> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kdm-torrent")
            .enable_all()
            .build()
            .context("torrent çalışma zamanı başlatılamadı")?;
        std::fs::create_dir_all(&opts.state_dir).ok();
        let so = SessionOptions {
            dht: opts.dht.then(|| DhtSessionConfig {
                persistence: Some(DhtPersistenceConfig {
                    dump_interval: None,
                    config_filename: Some(opts.state_dir.join("dht.json")),
                }),
                ..Default::default()
            }),
            disable_trackers: !opts.trackers,
            disable_local_service_discovery: !opts.lsd,
            listen: opts.listen_port.map(|port| ListenerOptions {
                listen_addr: (std::net::Ipv6Addr::UNSPECIFIED, port).into(),
                enable_upnp_port_forwarding: opts.upnp,
                ..Default::default()
            }),
            client_name_and_version: Some(concat!("KaradelikDM ", env!("CARGO_PKG_VERSION")).into()),
            ..Default::default()
        };
        let session = rt.block_on(Session::new_with_opts(default_dir, so)).context("torrent oturumu açılamadı")?;
        Ok(Client { rt, session })
    }

    /// Dinlenen port (gelen bağlantılar)
    pub fn listen_port(&self) -> Option<u16> {
        self.session.listen_addr().map(|a| a.port())
    }

    /// Metadata'yı alır: `.torrent` için yerel ayrıştırma, magnet için eşlerden (DHT / izleyici) indirme.
    /// `cancel` true döndürünce vazgeçilir.
    pub fn resolve(&self, src: Source<'_>, initial_peers: &[SocketAddr], cancel: &dyn Fn() -> bool) -> Result<Metadata> {
        if let Source::File(b) = src {
            return parse_torrent(b);
        }
        let Source::Magnet(m) = src else { unreachable!() };
        parse_magnet(m)?;
        let session = self.session.clone();
        let add = AddTorrent::from_url(m.to_string());
        let opts = AddTorrentOptions {
            list_only: true,
            initial_peers: (!initial_peers.is_empty()).then(|| initial_peers.to_vec()),
            ..Default::default()
        };
        let job = self.rt.spawn(async move { session.add_torrent(add, Some(opts)).await });
        let res = self.wait(job, cancel)?;
        match res.map_err(|e| anyhow!("{e:#}"))? {
            AddTorrentResponse::ListOnly(l) => {
                let meta = metadata_from(&l.info, hex(&l.info_hash), l.torrent_bytes.to_vec())?;
                Ok(meta)
            }
            AddTorrentResponse::AlreadyManaged(_, h) | AddTorrentResponse::Added(_, h) => {
                // Zaten oturumdaysa metadata elde
                let r = h.with_metadata(|m| metadata_from(&m.info, hex(&h.info_hash()), m.torrent_bytes.to_vec()));
                r.map_err(|e| anyhow!("{e:#}"))?
            }
        }
    }

    /// Torrenti indirmeye başlar (ya da duraklatılmışsa sürdürür). Var olan dosyalar önce doğrulanır, böylece
    /// yarım kalan indirme kaldığı yerden devam eder. Dönen değer bilgi özeti.
    pub fn add(&self, torrent: &[u8], out_dir: &Path, opts: AddOptions) -> Result<String> {
        let meta = parse_torrent(torrent)?;
        if let Some(h) = self.handle(&meta.info_hash) {
            if !opts.paused && h.is_paused() {
                let s = self.session.clone();
                self.rt.block_on(async move { s.unpause(&h).await }).map_err(|e| anyhow!("{e:#}"))?;
            }
            return Ok(meta.info_hash);
        }
        if let Some(only) = &opts.only_files {
            if only.is_empty() || only.iter().any(|&i| i >= meta.files.len()) {
                bail!("geçersiz dosya seçimi");
            }
        }
        let session = self.session.clone();
        let add = AddTorrent::from_bytes(torrent.to_vec());
        let ao = AddTorrentOptions {
            paused: opts.paused,
            only_files: opts.only_files,
            output_folder: Some(out_dir.to_string_lossy().into_owned()),
            // Klasördeki aynı adlı dosya doğrulanır; uyan parçalar korunur (kaldığı yerden devam)
            overwrite: true,
            initial_peers: (!opts.initial_peers.is_empty()).then_some(opts.initial_peers),
            ..Default::default()
        };
        let res = self.rt.block_on(async move { session.add_torrent(add, Some(ao)).await });
        res.map_err(|e| anyhow!("{e:#}"))?;
        Ok(meta.info_hash)
    }

    pub fn progress(&self, info_hash: &str) -> Option<Progress> {
        let h = self.handle(info_hash)?;
        let s = h.stats();
        let (down, up, peers, seen) = match &s.live {
            Some(l) => (
                mib_to_bps(l.download_speed.mbps),
                mib_to_bps(l.upload_speed.mbps),
                l.snapshot.peer_stats.live,
                l.snapshot.peer_stats.seen,
            ),
            None => (0, 0, 0, 0),
        };
        // librqbit'in toplamı parça bazlıdır: seçili dosyayla parça paylaşan seçilmemiş dosyanın baytları da
        // sayılır. Kullanıcıya dosya bazlı değer gösterilir.
        let (mut done, mut total) = (s.progress_bytes, s.total_bytes);
        if !s.file_progress.is_empty() {
            // (uzunluk, dolgu dosyası mı)
            let lens: Vec<(u64, bool)> =
                h.with_metadata(|m| m.info.iter_file_details().map(|f| (f.len, f.attrs().padding)).collect()).unwrap_or_default();
            let only = h.only_files();
            let sel = |i: usize| !lens[i].1 && only.as_ref().is_none_or(|o| o.contains(&i));
            if lens.len() == s.file_progress.len() {
                total = lens.iter().enumerate().filter(|(i, _)| sel(*i)).map(|(_, l)| l.0).sum();
                done = s.file_progress.iter().enumerate().filter(|(i, _)| sel(*i)).map(|(_, d)| d).sum::<u64>().min(total);
            }
        }
        Some(Progress {
            state: match s.state {
                TorrentStatsState::Initializing { .. } => State::Checking,
                TorrentStatsState::Live => State::Live,
                TorrentStatsState::Paused => State::Paused,
                TorrentStatsState::Error => State::Error,
            },
            done,
            total,
            uploaded: s.uploaded_bytes,
            down_speed: down,
            up_speed: up,
            peers,
            seen,
            finished: s.finished,
            error: s.error,
            files: s.file_progress,
        })
    }

    pub fn pause(&self, info_hash: &str) -> Result<()> {
        let Some(h) = self.handle(info_hash) else { return Ok(()) };
        if h.is_paused() {
            return Ok(());
        }
        let s = self.session.clone();
        self.rt.block_on(async move { s.pause(&h).await }).map_err(|e| anyhow!("{e:#}"))
    }

    /// Oturumdan çıkarır; `delete_files` ise indirilen dosyaları da siler.
    pub fn remove(&self, info_hash: &str, delete_files: bool) -> Result<()> {
        let Some(id) = Id20::from_str(info_hash).ok() else { bail!("geçersiz bilgi özeti") };
        if self.session.get(TorrentIdOrHash::Hash(id)).is_none() {
            return Ok(());
        }
        let s = self.session.clone();
        self.rt.block_on(async move { s.delete(TorrentIdOrHash::Hash(id), delete_files).await }).map_err(|e| anyhow!("{e:#}"))
    }

    /// Tüm torrentler için hız sınırı (bayt/sn, 0: sınırsız). librqbit sınırlayıcısı bir isteği (16 KiB blok) tek
    /// seferde geçirebilmelidir; daha düşük sınır her isteği hataya düşürürdü, bu yüzden en az 16 KiB/sn uygulanır.
    pub fn set_limits(&self, down: u64, up: u64) {
        let nz = |v: u64| NonZeroU32::new(if v == 0 { 0 } else { v.clamp(MIN_LIMIT, u32::MAX as u64) as u32 });
        self.session.ratelimits.set_download_bps(nz(down));
        self.session.ratelimits.set_upload_bps(nz(up));
    }

    pub fn shutdown(&self) {
        let s = self.session.clone();
        self.rt.block_on(async move { s.stop().await });
    }

    fn handle(&self, info_hash: &str) -> Option<Arc<librqbit::ManagedTorrent>> {
        let id = Id20::from_str(info_hash).ok()?;
        self.session.get(TorrentIdOrHash::Hash(id))
    }

    fn wait<T: Send + 'static>(&self, job: tokio::task::JoinHandle<T>, cancel: &dyn Fn() -> bool) -> Result<T> {
        while !job.is_finished() {
            if cancel() {
                job.abort();
                bail!("iptal edildi");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.rt.block_on(job).map_err(|e| anyhow!("torrent işi çöktü: {e}"))
    }
}

fn mib_to_bps(mib: f64) -> u64 {
    (mib * 1024.0 * 1024.0).max(0.0) as u64
}

fn hex(id: &Id20) -> String {
    id.as_string()
}

/// Magnet bağlantısından bilgi özeti ve (varsa) görünen ad.
pub fn parse_magnet(url: &str) -> Result<(String, Option<String>)> {
    let m = Magnet::parse(url).map_err(|e| anyhow!("geçersiz magnet bağlantısı: {e:#}"))?;
    let id = m.as_id20().ok_or_else(|| anyhow!("magnet bağlantısında BitTorrent v1 özeti (btih) yok"))?;
    Ok((hex(&id), m.name.clone()))
}

pub fn is_magnet(url: &str) -> bool {
    url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("magnet:"))
}

/// `.torrent` içeriğini ağa çıkmadan ayrıştırır.
pub fn parse_torrent(bytes: &[u8]) -> Result<Metadata> {
    let t = librqbit::torrent_from_bytes(bytes).map_err(|e| anyhow!("geçersiz .torrent dosyası: {e:#}"))?;
    let info = t.info.data.clone().validate().map_err(|e| anyhow!("geçersiz .torrent dosyası: {e:#}"))?;
    metadata_from(&info, hex(&t.info_hash), bytes.to_vec())
}

fn metadata_from<B: AsRef<[u8]>>(info: &ValidatedTorrentMetaV1Info<B>, info_hash: String, torrent: Vec<u8>) -> Result<Metadata> {
    let mut files = Vec::new();
    for fd in info.iter_file_details() {
        let parts = fd.filename.to_vec();
        for p in &parts {
            check_component(p)?;
        }
        files.push(FileEntry { path: parts.join("/"), size: fd.len, padding: fd.attrs().padding });
    }
    let name = info.name().map(|n| n.into_owned()).unwrap_or_default();
    if !name.is_empty() {
        check_component(&name)?;
    }
    Ok(Metadata {
        info_hash,
        web_seeds: url_list(&torrent),
        multi_file: files.len() > 1 || info.info().files.is_some(),
        name,
        total: files.iter().filter(|f| !f.padding).map(|f| f.size).sum(),
        files,
        private: info.info().private,
        torrent,
    })
}

/// BEP 19: tek dosyalı torrentte `/` ile bitmeyen adres dosyanın kendisidir; aksi halde adrese torrent adı ve
/// (çok dosyalıysa) dosyanın yolu eklenir. Yol bileşenleri yüzde kodlanır.
pub fn web_seed_url(base: &str, meta: &Metadata, file: &FileEntry) -> String {
    let enc = |c: &str| percent_encoding::utf8_percent_encode(c, PATH_SET).to_string();
    if !meta.multi_file {
        return if base.ends_with('/') { format!("{base}{}", enc(&meta.name)) } else { base.to_string() };
    }
    let base = if base.ends_with('/') { base.to_string() } else { format!("{base}/") };
    let path: Vec<String> = file.path.split('/').map(enc).collect();
    format!("{base}{}/{}", enc(&meta.name), path.join("/"))
}

/// Yol bileşeninde kodlanacaklar: denetim karakterleri, boşluk ve URL'de anlamı olanlar
const PATH_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ').add(b'"').add(b'#').add(b'%').add(b'<').add(b'>').add(b'?').add(b'`').add(b'{').add(b'}')
    .add(b'/').add(b'\\').add(b'^').add(b'|').add(b'[').add(b']');

/// `.torrent` kök sözlüğünden `url-list` (tek dize ya da dize listesi). librqbit bu alanı okumaz.
fn url_list(bytes: &[u8]) -> Vec<String> {
    fn end(b: &[u8], i: usize, depth: u32) -> Option<usize> {
        if depth > 64 {
            return None;
        }
        match *b.get(i)? {
            b'i' => Some(i + b[i..].iter().position(|&c| c == b'e')? + 1),
            b'l' | b'd' => {
                let mut j = i + 1;
                while *b.get(j)? != b'e' {
                    j = end(b, j, depth + 1)?;
                }
                Some(j + 1)
            }
            b'0'..=b'9' => {
                let colon = i + b[i..].iter().position(|&c| c == b':')?;
                let n: usize = std::str::from_utf8(&b[i..colon]).ok()?.parse().ok()?;
                colon.checked_add(1)?.checked_add(n).filter(|&e| e <= b.len())
            }
            _ => None,
        }
    }
    fn string(b: &[u8], i: usize) -> Option<(&[u8], usize)> {
        let e = end(b, i, 0)?;
        let colon = i + b[i..].iter().position(|&c| c == b':')?;
        (b.get(i)?.is_ascii_digit()).then_some((&b[colon + 1..e], e))
    }
    let mut out = Vec::new();
    if bytes.first() != Some(&b'd') {
        return out;
    }
    let mut i = 1;
    while bytes.get(i).is_some_and(|&c| c != b'e') {
        let Some((key, v)) = string(bytes, i) else { return out };
        let Some(ve) = end(bytes, v, 0) else { return out };
        if key == b"url-list" {
            let mut push = |s: &[u8]| {
                if let Ok(u) = std::str::from_utf8(s) {
                    if u.starts_with("http://") || u.starts_with("https://") {
                        out.push(u.to_string());
                    }
                }
            };
            if bytes[v] == b'l' {
                let mut j = v + 1;
                while bytes.get(j).is_some_and(|&c| c != b'e') {
                    match string(bytes, j) {
                        Some((s, e)) => {
                            push(s);
                            j = e;
                        }
                        None => match end(bytes, j, 0) {
                            Some(e) => j = e,
                            None => break,
                        },
                    }
                }
            } else if let Some((s, _)) = string(bytes, v) {
                push(s);
            }
        }
        i = ve;
    }
    out
}

/// librqbit `..` ve ayırıcıları reddeder; Windows'ta `C:` gibi sürücü önekli bileşen hedef klasörü tamamen
/// değiştirir (`PathBuf::push`), `dosya:akış` ise alternatif veri akışına yazar. Bunlar her platformda reddedilir:
/// aynı torrent Windows'ta açılınca klasör dışına yazmasın.
fn check_component(c: &str) -> Result<()> {
    let bad = c.is_empty()
        || c == "."
        || c == ".."
        || c.contains([':', '/', '\\', '\0'])
        || Path::new(c).components().count() != 1
        || !matches!(Path::new(c).components().next(), Some(std::path::Component::Normal(_)));
    if bad {
        bail!("torrent güvensiz bir dosya yolu içeriyor: {c:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnet_parsing() {
        let (h, n) = parse_magnet("magnet:?xt=urn:btih:0123456789ABCDEF0123456789abcdef01234567&dn=deneme%20dosya").unwrap();
        assert_eq!(h, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(n.as_deref(), Some("deneme dosya"));
        assert!(parse_magnet("magnet:?dn=yok").is_err());
        assert!(parse_magnet("http://x/y").is_err());
        assert!(is_magnet("MAGNET:?xt=") && !is_magnet("magnetx"));
    }

    #[test]
    fn unsafe_components_rejected() {
        for bad in ["C:", "C:evil.exe", "a:b", "..", ".", "", "a/b", "a\\b"] {
            assert!(check_component(bad).is_err(), "{bad:?}");
        }
        for ok in ["dosya.bin", "klasör", "a b", "...x", "con.txt"] {
            assert!(check_component(ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn web_seed_urls() {
        let t = b"d8:announce3:x/y8:url-listl21:https://a.example/dl/12:ftp://nope/x14:http://b.ex/f1e4:infod4:name1:aee";
        assert_eq!(url_list(t), ["https://a.example/dl/", "http://b.ex/f1"]);
        assert_eq!(url_list(b"d8:url-list17:http://h/file.isoe"), ["http://h/file.iso"]);
        assert!(url_list(b"d8:url-listi5ee").is_empty() && url_list(b"garbage").is_empty() && url_list(b"d8:url-list99:x").is_empty());
        let f = |p: &str| FileEntry { path: p.into(), size: 1, padding: false };
        let mut m = Metadata {
            info_hash: String::new(),
            name: "Kitap Arşivi".into(),
            files: vec![f("bölüm 1/a#b.txt")],
            total: 1,
            multi_file: true,
            private: false,
            torrent: vec![],
            web_seeds: vec![],
        };
        assert_eq!(web_seed_url("https://a/dl", &m, &m.files[0].clone()), "https://a/dl/Kitap%20Ar%C5%9Fivi/b%C3%B6l%C3%BCm%201/a%23b.txt");
        m.multi_file = false;
        assert_eq!(web_seed_url("https://a/dl/", &m, &m.files[0].clone()), "https://a/dl/Kitap%20Ar%C5%9Fivi");
        assert_eq!(web_seed_url("https://a/x.iso", &m, &m.files[0].clone()), "https://a/x.iso");
    }

    #[test]
    fn garbage_torrent_is_error() {
        assert!(parse_torrent(b"not bencode").is_err());
        assert!(parse_torrent(b"d4:infod4:name1:aee").is_err());
    }
}
