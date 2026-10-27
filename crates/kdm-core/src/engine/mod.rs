//! İndirme motoru: indirme kayıtları, başlat/durdur, kalıcılık ve arayüze ilerleme bildirimi.
//! Kuyruk ve zamanlayıcı kararları motorun üstündeki katmanda (`queue`) verilir.

pub mod filename;
pub mod http;
pub mod limiter;
pub mod model;
pub mod partfile;
pub mod split;
pub mod media;
mod store;
mod task;
pub mod torrent;
pub mod transport;

pub use model::*;
pub use task::hash_file;
pub use torrent::TorrentSettings;

use anyhow::{bail, Result};
use limiter::Limiter;
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use task::Task;

/// İndirmeden önce adres hakkında bilgi (ekleme penceresi).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Probe {
    pub file_name: String,
    pub size: Option<u64>,
    pub content_type: String,
    pub resumable: bool,
    pub final_url: String,
}

/// `Range: bytes=0-0` ile yoklar; gövde okunmaz. FTP'de SIZE/MDTM/REST ile.
pub fn probe(net: &http::NetConfig, spec: &http::RequestSpec) -> Result<Probe> {
    if transport::is_ftp(&spec.url) {
        return transport::probe_ftp(spec, std::time::Duration::from_secs(if net.timeout == 0 { 30 } else { net.timeout }), net.ftp_tls);
    }
    let agent = http::agent(net)?;
    let r = http::get(&agent, spec, Some(http::ByteRange { start: 0, end: Some(0) }), "", &http::DigestState::default())?;
    let m = r.meta;
    let (size, resumable) = match m.status {
        206 => (m.content_range.and_then(|c| c.2), true),
        200 => (m.total_size(), m.accept_ranges.split(',').any(|v| v.trim() == "bytes")),
        s => bail!("HTTP {s}"),
    };
    let name = filename::from_content_disposition(&m.content_disposition)
        .or_else(|| filename::from_url(&m.final_url))
        .or_else(|| filename::from_url(&spec.url))
        .unwrap_or_else(|| "indirme".into());
    Ok(Probe {
        file_name: filename::with_type_extension(name, &m.content_type),
        size,
        content_type: m.content_type,
        resumable,
        final_url: m.final_url,
    })
}

/// İlerleme ve sonuç bildirimleri (Tauri tarafında arayüze olay olarak gider).
pub trait Sink: Send + Sync {
    fn progress(&self, infos: Vec<Info>);
    fn finished(&self, id: u64, status: Status, message: &str);
}

#[derive(Clone, Debug)]
pub struct EngineSettings {
    /// İndirme başına varsayılan en çok bağlantı
    pub max_connections: u32,
    /// HLS/DASH: eşzamanlı segment
    pub media_parallel: u32,
    /// ffmpeg yolu ya da klasörü (boş: PATH)
    pub ffmpeg: String,
    pub torrent: TorrentSettings,
}

impl Default for EngineSettings {
    fn default() -> Self {
        EngineSettings { max_connections: 8, media_parallel: 4, ffmpeg: String::new(), torrent: TorrentSettings::default() }
    }
}

pub(crate) struct Inner {
    tasks: RwLock<BTreeMap<u64, Arc<Task>>>,
    pub(crate) global_limit: Limiter,
    pub(crate) settings: RwLock<EngineSettings>,
    net: RwLock<http::NetConfig>,
    agent: Mutex<Option<ureq::Agent>>,
    store: PathBuf,
    next_id: AtomicU64,
    /// Tüm indirmelerin alınan baytı (kota ölçümü)
    pub(crate) total_received: AtomicU64,
    changed: AtomicBool,
    last_save: Mutex<Option<Instant>>,
    save_lock: Mutex<()>,
    pub(crate) sink: Arc<dyn Sink>,
    /// İlk torrent başlatılınca açılır (öncesinde port dinlenmez)
    torrent: Mutex<Option<Arc<kdm_torrent::Client>>>,
    /// Bitmiş ve paylaşılan torrentler: kayıt kimliği -> (özet, boyut)
    seeding: Mutex<BTreeMap<u64, (String, u64)>>,
}

impl Inner {
    /// Ağ ayarı değişene kadar aynı ajan kullanılır (bağlantı havuzu paylaşılır).
    pub(crate) fn agent(&self) -> Result<ureq::Agent> {
        let mut a = self.agent.lock();
        if let Some(agent) = a.as_ref() {
            return Ok(agent.clone());
        }
        let agent = http::agent(&self.net.read())?;
        *a = Some(agent.clone());
        Ok(agent)
    }

    pub(crate) fn timeout(&self) -> std::time::Duration {
        let t = self.net.read().timeout;
        std::time::Duration::from_secs(if t == 0 { 30 } else { t })
    }

    pub(crate) fn ftp_tls(&self) -> http::FtpTls {
        self.net.read().ftp_tls
    }

    pub(crate) fn torrent_dir(&self) -> PathBuf {
        self.store.parent().map(|p| p.join("torrents")).unwrap_or_else(|| PathBuf::from("torrents"))
    }

    pub(crate) fn torrent_started(&self) -> bool {
        self.torrent.lock().is_some()
    }

    pub(crate) fn torrent_client(&self) -> Result<Arc<kdm_torrent::Client>> {
        let mut g = self.torrent.lock();
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let ts = self.settings.read().torrent.clone();
        let dir = self.torrent_dir();
        let opts = kdm_torrent::Options {
            listen_port: Some(ts.listen_port),
            dht: ts.dht,
            upnp: ts.upnp,
            lsd: ts.lsd,
            trackers: true,
            state_dir: dir.clone(),
        };
        let c = Arc::new(kdm_torrent::Client::start(opts, dir)?);
        self.apply_torrent_limits(&c);
        *g = Some(c.clone());
        Ok(c)
    }

    pub(crate) fn apply_torrent_limits(&self, c: &kdm_torrent::Client) {
        c.set_limits(self.global_limit.rate(), self.settings.read().torrent.upload_limit);
    }

    pub(crate) fn seed(&self, id: u64, hash: String, total: u64) {
        self.seeding.lock().insert(id, (hash, total));
    }

    pub(crate) fn unseed(&self, id: u64) {
        self.seeding.lock().remove(&id);
    }

    /// Paylaşmayı bitirir: torrent oturumdan çıkar, dosyalar serbest kalır.
    pub(crate) fn stop_seeding(&self, id: u64) -> bool {
        let Some((hash, _)) = self.seeding.lock().remove(&id) else { return false };
        if let Some(c) = self.torrent.lock().clone() {
            let _ = c.remove(&hash, false);
        }
        if let Some(t) = self.tasks.read().get(&id) {
            *t.torrent.lock() = torrent::Live::default();
        }
        self.save();
        self.mark_changed();
        true
    }

    /// Paylaşılan torrentler: gönderilen baytı kayda işler, oran dolunca durdurur.
    fn tick_seeding(&self) {
        let list: Vec<(u64, String, u64)> = self.seeding.lock().iter().map(|(id, (h, t))| (*id, h.clone(), *t)).collect();
        if list.is_empty() {
            return;
        }
        let Some(c) = self.torrent.lock().clone() else { return };
        let ratio = self.settings.read().torrent.seed_ratio;
        for (id, hash, total) in list {
            let task = self.tasks.read().get(&id).cloned();
            let Some(p) = c.progress(&hash) else {
                self.unseed(id);
                continue;
            };
            if let Some(t) = &task {
                if let Some(ts) = t.state.lock().rec.torrent.as_mut() {
                    ts.uploaded = p.uploaded;
                }
                let mut l = t.torrent.lock();
                l.upload_speed = p.up_speed;
                l.peers = p.peers;
                l.seen = p.seen;
            }
            if ratio > 0.0 && p.uploaded as f64 >= ratio * total.max(1) as f64 {
                self.stop_seeding(id);
            }
            self.mark_changed();
        }
    }

    pub(crate) fn mark_changed(&self) {
        self.changed.store(true, Ordering::Relaxed);
    }

    /// Başka bir bitmemiş indirme aynı klasörde bu adı kullanıyor mu.
    pub(crate) fn name_taken(&self, id: u64, dir: &Path, name: &str) -> bool {
        self.tasks.read().values().any(|t| {
            if t.id == id {
                return false;
            }
            let st = t.state.lock();
            st.rec.status != Status::Completed && st.rec.dir == dir && st.rec.file_name.eq_ignore_ascii_case(name)
        })
    }

    fn records(&self) -> Vec<Record> {
        self.tasks.read().values().map(|t| t.record()).collect()
    }

    /// En çok saniyede bir kaydeder.
    pub(crate) fn save(&self) {
        {
            let mut last = self.last_save.lock();
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.save_now();
    }

    pub(crate) fn save_now(&self) {
        let _g = self.save_lock.lock();
        if let Err(e) = store::save(&self.store, &self.records()) {
            log::error!("indirme listesi kaydedilemedi: {e:#}");
        }
    }

    fn info(&self, t: &Task) -> Info {
        let st = t.state.lock();
        let r = &st.rec;
        let done = r.downloaded();
        let speed = t.speed.load(Ordering::Relaxed);
        let eta = match (r.size, speed) {
            (Some(size), s) if s > 0 => Some(size.saturating_sub(done) / s),
            _ => None,
        };
        let parts = r
            .media
            .as_ref()
            .filter(|_| r.status != Status::Completed)
            .map(|_| [t.media_done.load(Ordering::Relaxed), t.media_total.load(Ordering::Relaxed)]);
        // Bitmiş indirmede indirilen = dosya boyutu (medya kayıtlarında parça listesi yoktur)
        let done = match (r.status, &r.media, &r.torrent) {
            (Status::Completed, _, _) => r.size.unwrap_or(done),
            (_, Some(_), _) => t.media_bytes.load(Ordering::Relaxed),
            (_, _, Some(ts)) => ts.done,
            _ => done,
        };
        let eta = match (parts, speed) {
            // Segment başına ortalama bayt ile kalan süre tahmini
            (Some([d, n]), s) if s > 0 && d > 0 && n > d => Some(done / d * (n - d) / s),
            (Some(_), _) => None,
            _ => eta,
        };
        let mut segs: Vec<[u64; 3]> = r.segments.iter().map(|s| [s.start, s.end.unwrap_or(s.pos), s.pos - s.start]).collect();
        segs.sort_by_key(|s| s[0]);
        segs.truncate(64);
        Info {
            id: r.id,
            url: r.url.clone(),
            file_name: r.file_name.clone(),
            dir: r.dir.to_string_lossy().into_owned(),
            size: r.size,
            done,
            status: r.status,
            range: r.range,
            speed,
            eta,
            connections: t.connections.load(Ordering::Relaxed) as u32,
            segments: segs,
            error: if r.error.is_empty() { st.notice.clone().unwrap_or_default() } else { r.error.clone() },
            category: r.category.clone(),
            queue: r.queue.clone(),
            description: r.description.clone(),
            added: r.added,
            finished: r.finished,
            order: r.order,
            parts,
            media: r.media.is_some(),
            torrent: r.torrent.as_ref().map(|ts| {
                let l = t.torrent.lock().clone();
                TorrentView {
                    info_hash: ts.info_hash.clone(),
                    peers: l.peers,
                    seen: l.seen,
                    upload_speed: l.upload_speed,
                    uploaded: ts.uploaded,
                    checking: l.checking,
                    resolving: l.resolving,
                    seeding: self.seeding.lock().contains_key(&r.id),
                }
            }),
        }
    }
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    /// `store`: indirme listesi dosyası (`downloads.json`).
    pub fn open(store: PathBuf, sink: Arc<dyn Sink>) -> Engine {
        let records = store::load(&store);
        let next = records.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        let tasks = records
            .into_iter()
            .map(|mut r| {
                // Önceki oturum kapanırken sürenler duraklatılmış sayılır
                if r.status == Status::Downloading {
                    r.status = Status::Stopped;
                }
                (r.id, Arc::new(Task::new(r)))
            })
            .collect();
        let inner = Arc::new(Inner {
            tasks: RwLock::new(tasks),
            global_limit: Limiter::new(0),
            settings: RwLock::new(EngineSettings::default()),
            net: RwLock::new(http::NetConfig::default()),
            agent: Mutex::new(None),
            store,
            next_id: AtomicU64::new(next),
            total_received: AtomicU64::new(0),
            changed: AtomicBool::new(true),
            last_save: Mutex::new(None),
            save_lock: Mutex::new(()),
            sink,
            torrent: Mutex::new(None),
            seeding: Mutex::new(BTreeMap::new()),
        });
        let weak = Arc::downgrade(&inner);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            let Some(inner) = weak.upgrade() else { break };
            inner.tick_seeding();
            let running = inner.tasks.read().values().any(|t| t.running.load(Ordering::Relaxed));
            if inner.changed.swap(false, Ordering::Relaxed) || running {
                let infos = inner.tasks.read().values().map(|t| inner.info(t)).collect();
                inner.sink.progress(infos);
            }
        });
        Engine { inner }
    }

    /// Torrent portu, DHT, UPnP ve yerel arama yalnız oturum açılırken uygulanır (uygulama yeniden başlayınca).
    pub fn set_settings(&self, s: EngineSettings) {
        *self.inner.settings.write() = s;
        if let Some(c) = self.inner.torrent.lock().clone() {
            self.inner.apply_torrent_limits(&c);
        }
    }

    pub fn set_net(&self, cfg: http::NetConfig) {
        let mut net = self.inner.net.write();
        if *net != cfg {
            *net = cfg;
            *self.inner.agent.lock() = None;
        }
    }

    /// Ağ ayarlarını denetler (vekil adresi geçerli mi).
    pub fn check_net(cfg: &http::NetConfig) -> Result<()> {
        http::agent(cfg).map(|_| ())
    }

    /// Genel hız sınırı (bayt/sn, 0: sınırsız).
    pub fn set_global_limit(&self, bps: u64) {
        self.inner.global_limit.set_rate(bps);
        if let Some(c) = self.inner.torrent.lock().clone() {
            self.inner.apply_torrent_limits(&c);
        }
    }

    /// Torrent oturumu açıksa dinlenen port.
    pub fn torrent_port(&self) -> Option<u16> {
        self.inner.torrent.lock().as_ref().and_then(|c| c.listen_port())
    }

    /// Ekleme penceresi için: `.torrent` içeriğini ayrıştırır ya da magnet metadata'sını eşlerden alır ve
    /// önbelleğe yazar (indirme başlayınca yeniden istenmez). `cancel` true olunca vazgeçilir.
    pub fn torrent_metadata(&self, src: kdm_torrent::Source<'_>, cancel: &dyn Fn() -> bool) -> Result<kdm_torrent::Metadata> {
        let m = match src {
            kdm_torrent::Source::File(b) => kdm_torrent::parse_torrent(b)?,
            magnet => {
                let peers = self.inner.settings.read().torrent.initial_peers.clone();
                self.inner.torrent_client()?.resolve(magnet, &peers, cancel)?
            }
        };
        std::fs::create_dir_all(self.inner.torrent_dir())?;
        std::fs::write(torrent::cache_path(&self.inner, &m.info_hash), &m.torrent)?;
        Ok(m)
    }

    pub fn global_limit(&self) -> u64 {
        self.inner.global_limit.rate()
    }

    /// Uygulama açıldığından beri tüm indirmelerde alınan bayt.
    pub fn total_received(&self) -> u64 {
        self.inner.total_received.load(Ordering::Relaxed)
    }

    /// Yeni indirme ekler. `dir` boşsa çağıran (kategori) belirlemiş olmalı.
    pub fn add(&self, nd: NewDownload, dir: PathBuf, category: &str) -> Result<u64> {
        let url = nd.url.trim().to_string();
        if kdm_torrent::is_magnet(&url) || nd.torrent.is_some() {
            return self.add_torrent(nd, dir, category);
        }
        let parsed = url::Url::parse(&url).map_err(|e| anyhow::anyhow!("geçersiz adres: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https" | "ftp" | "ftps") {
            bail!("desteklenmeyen adres türü: {}", parsed.scheme());
        }
        // Adres içindeki kullanıcı bilgisi kimlik doğrulamaya taşınır
        let mut auth = nd.auth.clone();
        let mut clean = parsed.clone();
        if !parsed.username().is_empty() && auth.is_none() {
            let dec = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
            auth = Some(Auth { user: dec(parsed.username()), password: dec(parsed.password().unwrap_or("")) });
            let _ = clean.set_username("");
            let _ = clean.set_password(None);
        }
        let given = filename::sanitize(&nd.file_name);
        let name_locked = !given.is_empty();
        let mut base = if name_locked { given } else { filename::from_url(clean.as_str()).unwrap_or_else(|| "indirme".into()) };
        if nd.media.is_some() {
            // Akış adresinin adı (master.m3u8, manifest.mpd) dosya adı olamaz; birleşik çıktı mp4
            let stem = match base.rsplit_once('.') {
                Some((s, e)) if matches!(e.to_ascii_lowercase().as_str(), "m3u8" | "m3u" | "mpd" | "mp4" | "ts") => s.to_string(),
                _ => base.clone(),
            };
            base = format!("{stem}.mp4");
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let file_name = filename::unique(&base, &|n| {
            dir.join(n).exists() || dir.join(format!("{n}.kdm")).exists() || self.inner.name_taken(id, &dir, n)
        });
        let order = self.inner.tasks.read().values().map(|t| t.state.lock().rec.order).max().unwrap_or(0) + 1;
        let rec = Record {
            id,
            url: clean.to_string(),
            final_url: String::new(),
            file_name,
            name_locked,
            dir,
            size: None,
            segments: Vec::new(),
            status: Status::Stopped,
            range: RangeSupport::Unknown,
            etag: String::new(),
            last_modified: String::new(),
            content_type: String::new(),
            referer: nd.referer,
            cookies: nd.cookies,
            user_agent: nd.user_agent,
            headers: nd.headers,
            auth,
            checksum: nd.checksum,
            max_connections: nd.max_connections,
            speed_limit: nd.speed_limit,
            queue: nd.queue,
            category: category.to_string(),
            description: nd.description,
            error: String::new(),
            added: chrono::Utc::now().timestamp(),
            finished: 0,
            order,
            retries: 0,
            retry_at: 0,
            media: nd.media,
            torrent: None,
        };
        self.insert(rec, nd.start)
    }

    fn insert(&self, rec: Record, start: bool) -> Result<u64> {
        let id = rec.id;
        self.inner.tasks.write().insert(id, Arc::new(Task::new(rec)));
        self.inner.save_now();
        self.inner.mark_changed();
        if start {
            self.start(id);
        }
        Ok(id)
    }

    /// Magnet bağlantısı ya da (`nd.torrent` ile) önceden ayrıştırılmış `.torrent`. Adres her durumda magnet
    /// bağlantısı olarak saklanır; metadata önbellekteyse ağdan istenmez.
    fn add_torrent(&self, nd: NewDownload, dir: PathBuf, category: &str) -> Result<u64> {
        let url = nd.url.trim().to_string();
        let mut spec = nd.torrent.clone().unwrap_or_default();
        let mut dn = None;
        if kdm_torrent::is_magnet(&url) {
            let (h, n) = kdm_torrent::parse_magnet(&url)?;
            if !spec.info_hash.is_empty() && !spec.info_hash.eq_ignore_ascii_case(&h) {
                bail!("magnet bağlantısı ile torrent bilgisi uyuşmuyor");
            }
            spec.info_hash = h;
            dn = n;
        } else if spec.info_hash.len() != 40 || !spec.info_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("geçersiz torrent özeti");
        }
        spec.info_hash.make_ascii_lowercase();
        spec.done = 0;
        spec.uploaded = 0;
        let dup = self.inner.tasks.read().values().find(|t| {
            t.state.lock().rec.torrent.as_ref().is_some_and(|x| x.info_hash == spec.info_hash)
        }).map(|t| t.id);
        if let Some(dup) = dup {
            bail!("bu torrent listede zaten var (#{dup})");
        }
        // Önbellekte metadata varsa ad ve klasör yapısı şimdiden bilinir
        let cached = std::fs::read(torrent::cache_path(&self.inner, &spec.info_hash))
            .ok()
            .and_then(|b| kdm_torrent::parse_torrent(&b).ok());
        if let Some(m) = &cached {
            spec.multi_file = m.multi_file;
        }
        let given = filename::sanitize(&nd.file_name);
        let name_locked = !given.is_empty();
        let base = if name_locked {
            given
        } else {
            let n = cached.as_ref().map(|m| m.name.clone()).or(dn).unwrap_or_default();
            let n = filename::sanitize(&n);
            if n.is_empty() { format!("torrent-{}", &spec.info_hash[..8]) } else { n }
        };
        let url = if kdm_torrent::is_magnet(&url) { url } else { format!("magnet:?xt=urn:btih:{}", spec.info_hash) };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let order = self.inner.tasks.read().values().map(|t| t.state.lock().rec.order).max().unwrap_or(0) + 1;
        let rec = Record {
            id,
            url,
            final_url: String::new(),
            file_name: base,
            name_locked,
            dir,
            size: None,
            segments: Vec::new(),
            status: Status::Stopped,
            range: RangeSupport::Unknown,
            etag: String::new(),
            last_modified: String::new(),
            content_type: String::new(),
            referer: String::new(),
            cookies: String::new(),
            user_agent: String::new(),
            headers: BTreeMap::new(),
            auth: None,
            checksum: None,
            max_connections: 0,
            speed_limit: 0,
            queue: nd.queue,
            category: category.to_string(),
            description: nd.description,
            error: String::new(),
            added: chrono::Utc::now().timestamp(),
            finished: 0,
            order,
            retries: 0,
            retry_at: 0,
            media: None,
            torrent: Some(spec),
        };
        self.insert(rec, nd.start)
    }

    fn task(&self, id: u64) -> Option<Arc<Task>> {
        self.inner.tasks.read().get(&id).cloned()
    }

    pub fn start(&self, id: u64) -> bool {
        let Some(t) = self.task(id) else { return false };
        if t.state.lock().rec.status == Status::Completed {
            return false;
        }
        if t.running.swap(true, Ordering::AcqRel) {
            return true;
        }
        let inner = self.inner.clone();
        std::thread::spawn(move || task::supervise(inner, t));
        true
    }

    pub fn stop(&self, id: u64) {
        if self.inner.stop_seeding(id) {
            return;
        }
        if let Some(t) = self.task(id) {
            if t.running.load(Ordering::Acquire) {
                t.stop.store(true, Ordering::Relaxed);
            } else {
                let mut st = t.state.lock();
                if st.rec.status == Status::Queued {
                    st.rec.status = Status::Stopped;
                }
                drop(st);
                self.inner.mark_changed();
            }
        }
    }

    /// Bitmiş torrenti paylaşmaya başlar (arka planda; dosyalar önce doğrulanır). Sonuç satırda görünür.
    pub fn seed(&self, id: u64) -> Result<()> {
        let Some(t) = self.task(id) else { bail!("indirme bulunamadı") };
        {
            let st = t.state.lock();
            if st.rec.torrent.is_none() || st.rec.status != Status::Completed {
                bail!("yalnız bitmiş torrent paylaşılabilir");
            }
        }
        if self.inner.seeding.lock().contains_key(&id) || t.running.load(Ordering::Acquire) {
            return Ok(());
        }
        {
            let mut l = t.torrent.lock();
            if l.checking {
                return Ok(());
            }
            l.checking = true;
        }
        t.state.lock().notice = None;
        self.inner.mark_changed();
        let inner = self.inner.clone();
        std::thread::spawn(move || {
            let r = torrent::reseed(&inner, &t);
            t.torrent.lock().checking = false;
            if let Err(e) = r {
                t.state.lock().notice = Some(format!("Paylaşılamadı: {e:#}"));
            }
            inner.mark_changed();
        });
        Ok(())
    }

    pub fn is_running(&self, id: u64) -> bool {
        self.task(id).is_some_and(|t| t.running.load(Ordering::Acquire))
    }

    pub fn running_ids(&self) -> Vec<u64> {
        self.inner.tasks.read().values().filter(|t| t.running.load(Ordering::Acquire)).map(|t| t.id).collect()
    }

    /// Kayıt üzerinde değişiklik (yalnız indirme çalışmıyorken; ör. kuyruk, açıklama, bağlantı yenileme).
    pub fn update(&self, id: u64, f: impl FnOnce(&mut Record)) -> Result<()> {
        let Some(t) = self.task(id) else { bail!("indirme bulunamadı") };
        if t.running.load(Ordering::Acquire) {
            bail!("indirme sürerken değiştirilemez");
        }
        f(&mut t.state.lock().rec);
        t.limiter.set_rate(t.state.lock().rec.speed_limit);
        self.inner.save_now();
        self.inner.mark_changed();
        Ok(())
    }

    /// Çalışırken de değiştirilebilen alanlar.
    pub fn set_speed_limit(&self, id: u64, bps: u64) {
        if let Some(t) = self.task(id) {
            t.state.lock().rec.speed_limit = bps;
            t.limiter.set_rate(bps);
            self.inner.mark_changed();
        }
    }

    pub fn set_status(&self, id: u64, status: Status) {
        if let Some(t) = self.task(id) {
            if !t.running.load(Ordering::Acquire) {
                t.state.lock().rec.status = status;
                self.inner.mark_changed();
            }
        }
    }

    /// Siler; `delete_files` ise yarım ve bitmiş dosyaları da.
    pub fn remove(&self, id: u64, delete_files: bool) -> Result<()> {
        let Some(t) = self.task(id) else { return Ok(()) };
        t.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(10);
        while t.running.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let rec = t.record();
        self.inner.tasks.write().remove(&id);
        if let Some(ts) = &rec.torrent {
            self.inner.unseed(id);
            let out = torrent::out_dir(&rec.dir, &rec.file_name, ts);
            if let Err(e) = torrent::forget(&self.inner, ts, &out, delete_files) {
                log::warn!("torrent kaldırılamadı: {e:#}");
            }
            let _ = std::fs::remove_file(torrent::cache_path(&self.inner, &ts.info_hash));
            self.inner.save_now();
            self.inner.mark_changed();
            return Ok(());
        }
        let _ = std::fs::remove_file(rec.part_path());
        if delete_files && rec.status == Status::Completed {
            let _ = std::fs::remove_file(rec.target_path());
        }
        self.inner.save_now();
        self.inner.mark_changed();
        Ok(())
    }

    /// Baştan indirir (yarım dosya silinir).
    pub fn restart(&self, id: u64) -> Result<()> {
        let Some(t) = self.task(id) else { bail!("indirme bulunamadı") };
        if t.running.load(Ordering::Acquire) {
            bail!("önce durdurun");
        }
        let r = t.record();
        if let Some(ts) = &r.torrent {
            self.inner.unseed(id);
            torrent::forget(&self.inner, ts, &torrent::out_dir(&r.dir, &r.file_name, ts), true)?;
            let mut st = t.state.lock();
            if let Some(x) = st.rec.torrent.as_mut() {
                x.done = 0;
                x.uploaded = 0;
            }
            st.rec.status = Status::Stopped;
            st.rec.error.clear();
            st.rec.finished = 0;
            drop(st);
            self.inner.save_now();
            self.inner.mark_changed();
            return Ok(());
        }
        {
            let mut st = t.state.lock();
            let _ = std::fs::remove_file(st.rec.part_path());
            let r = &mut st.rec;
            r.segments.clear();
            r.size = None;
            r.range = RangeSupport::Unknown;
            r.etag.clear();
            r.last_modified.clear();
            r.status = Status::Stopped;
            r.error.clear();
            r.finished = 0;
        }
        self.inner.save_now();
        self.inner.mark_changed();
        Ok(())
    }

    pub fn record(&self, id: u64) -> Option<Record> {
        self.task(id).map(|t| t.record())
    }

    pub fn records(&self) -> Vec<Record> {
        self.inner.records()
    }

    pub fn info(&self, id: u64) -> Option<Info> {
        self.task(id).map(|t| self.inner.info(&t))
    }

    pub fn infos(&self) -> Vec<Info> {
        self.inner.tasks.read().values().map(|t| self.inner.info(t)).collect()
    }

    /// Tümünü durdurur, en çok `wait` kadar bekler ve kaydeder.
    pub fn shutdown(&self, wait: Duration) {
        let tasks: Vec<Arc<Task>> = self.inner.tasks.read().values().cloned().collect();
        for t in &tasks {
            t.stop.store(true, Ordering::Relaxed);
        }
        let deadline = Instant::now() + wait;
        while tasks.iter().any(|t| t.running.load(Ordering::Acquire)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(c) = self.inner.torrent.lock().take() {
            c.shutdown();
        }
        self.inner.save_now();
    }
}
