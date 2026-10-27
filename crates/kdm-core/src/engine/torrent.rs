//! BitTorrent indirmesinin yürütülmesi. Protokol `kdm-torrent`'tedir; burada kayıt, ad, klasör, durdurma, ilerleme
//! ve paylaşma (seed) kararları verilir.
//!
//! Metadata bir kez alınınca `<veri>/torrents/<özet>.torrent` olarak saklanır; yeniden başlatmada eşlerden tekrar
//! istenmez. Var olan dosyalar librqbit tarafından parça parça doğrulanır, böylece indirme kaldığı yerden sürer.

use super::model::{Status, TorrentSpec};
use super::task::Task;
use super::{filename, Inner};
use anyhow::{anyhow, bail, Result};
use kdm_torrent::{AddOptions, Client, Source, State};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(500);
const CHECKPOINT: Duration = Duration::from_secs(3);
/// Eşlerden bu kadar süre hiç veri gelmezse torrentteki HTTP kaynağına (web seed) geçilir
const WEB_SEED_AFTER: Duration = Duration::from_secs(20);

/// Torrent satırının oturumluk durumu (arayüz için).
#[derive(Clone, Debug, Default)]
pub(crate) struct Live {
    pub peers: u32,
    pub seen: u32,
    pub upload_speed: u64,
    pub checking: bool,
    pub resolving: bool,
}

/// Paylaşma ayarları ve oturum seçenekleri (Tercihler > BitTorrent).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TorrentSettings {
    /// 0: her açılışta işletim sistemi seçer
    pub listen_port: u16,
    pub dht: bool,
    pub upnp: bool,
    /// Yerel ağda eş arama
    pub lsd: bool,
    /// Bitince kendiliğinden paylaşılsın mı. Kapalıyken torrent bitince oturumdan çıkar; kullanıcı isterse
    /// satırdan "Paylaş" ile başlatır. Paylaşılırken dosyalar açık tutulur (kurulum programı çalıştırılamaz).
    pub seed_after: bool,
    /// Paylaşma, gönderilen / boyut bu orana ulaşınca durur; 0: kullanıcı durdurana kadar
    pub seed_ratio: f64,
    /// bayt/sn, 0: sınırsız
    pub upload_limit: u64,
    /// Testler için bilinen eşler (kaydedilmez)
    #[doc(hidden)]
    #[serde(skip)]
    pub initial_peers: Vec<std::net::SocketAddr>,
    /// Testler: eşleri beklemeden web seed'e geç (kaydedilmez)
    #[doc(hidden)]
    #[serde(skip)]
    pub web_seed_now: bool,
}

impl Default for TorrentSettings {
    fn default() -> Self {
        TorrentSettings { listen_port: 0, dht: true, upnp: false, lsd: true, seed_after: false, seed_ratio: 1.0, upload_limit: 0, initial_peers: Vec::new(), web_seed_now: false }
    }
}

pub(crate) fn cache_path(inner: &Inner, info_hash: &str) -> PathBuf {
    inner.torrent_dir().join(format!("{info_hash}.torrent"))
}

/// Kayıt kaldırılırken ya da baştan indirilirken: oturumdan çıkarır, istenirse dosyaları siler.
/// Oturumda değilse ve dosyalar silinecekse önce duraklatılmış olarak eklenir (librqbit yalnız torrentin kendi
/// dosyalarını siler; klasörü elle silmekten güvenlidir).
pub(crate) fn forget(inner: &Inner, spec: &TorrentSpec, out_dir: &std::path::Path, delete_files: bool) -> Result<()> {
    let cache = cache_path(inner, &spec.info_hash);
    let bytes = std::fs::read(&cache).ok();
    if !delete_files && !inner.torrent_started() {
        return Ok(());
    }
    let client = inner.torrent_client()?;
    if delete_files {
        if let Some(b) = &bytes {
            client.add(b, out_dir, AddOptions { only_files: spec.files.clone(), paused: true, ..Default::default() })?;
        }
    }
    client.remove(&spec.info_hash, delete_files)
}

/// İçeriğin yazılacağı klasör.
pub(crate) fn out_dir(dir: &std::path::Path, file_name: &str, spec: &TorrentSpec) -> PathBuf {
    if spec.multi_file {
        dir.join(file_name)
    } else {
        dir.to_path_buf()
    }
}

pub(crate) fn run(inner: &Arc<Inner>, task: &Arc<Task>, spec: TorrentSpec) -> Result<(Status, String)> {
    {
        let mut st = task.state.lock();
        st.rec.status = Status::Downloading;
        st.rec.error.clear();
        st.fatal = None;
        st.notice = None;
    }
    task.stop.store(false, Ordering::Relaxed);
    task.received.store(0, Ordering::Relaxed);
    *task.torrent.lock() = Live::default();
    inner.unseed(task.id);
    inner.mark_changed();
    let client = inner.torrent_client()?;
    let stopped = || task.stop.load(Ordering::Relaxed);

    // 1) Metadata: önbellekten ya da eşlerden
    let cache = cache_path(inner, &spec.info_hash);
    let bytes = match std::fs::read(&cache) {
        Ok(b) => b,
        Err(_) => {
            task.torrent.lock().resolving = true;
            task.state.lock().notice = Some("Torrent bilgileri eşlerden alınıyor".into());
            inner.mark_changed();
            let url = task.state.lock().rec.url.clone();
            let peers = inner.settings.read().torrent.initial_peers.clone();
            let r = client.resolve(Source::Magnet(&url), &peers, &stopped);
            task.torrent.lock().resolving = false;
            task.state.lock().notice = None;
            let m = match r {
                Ok(m) => m,
                Err(_) if stopped() => return Ok((Status::Stopped, String::new())),
                Err(e) => return Err(e),
            };
            std::fs::create_dir_all(inner.torrent_dir())?;
            std::fs::write(&cache, &m.torrent)?;
            m.torrent
        }
    };
    let meta = kdm_torrent::parse_torrent(&bytes)?;
    if meta.info_hash != spec.info_hash {
        bail!("torrent dosyası bu indirmeye ait değil");
    }
    if let Some(sel) = &spec.files {
        if sel.is_empty() || sel.iter().any(|&i| i >= meta.files.len()) {
            bail!("geçersiz dosya seçimi");
        }
    }
    let selected: u64 = meta
        .files
        .iter()
        .enumerate()
        .filter(|(i, f)| !f.padding && spec.files.as_ref().is_none_or(|s| s.contains(i)))
        .map(|(_, f)| f.size)
        .sum();

    // 2) Ad ve klasör: ilk kez metadata gelince belirlenir; var olan başka bir dosyanın üstüne yazılmaz
    let out = {
        let mut st = task.state.lock();
        let fresh = st.rec.size.is_none();
        let r = &mut st.rec;
        let mut sp = r.torrent.clone().unwrap_or_default();
        sp.multi_file = meta.multi_file;
        if fresh {
            let content = if meta.multi_file {
                let n = filename::sanitize(&meta.name);
                if n.is_empty() { r.file_name.clone() } else { n }
            } else {
                meta.files[0].path.clone()
            };
            if meta.multi_file {
                let base = if r.name_locked { r.file_name.clone() } else { content };
                let dir = r.dir.clone();
                r.file_name = filename::unique(&base, &|n| dir.join(n).exists());
            } else {
                // Tek dosyada ad torrentten gelir; aynı adlı bir dosya varsa ayrı klasöre konur
                if r.dir.join(&content).exists() {
                    let stem = content.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(content.clone());
                    let dir = r.dir.clone();
                    r.dir = dir.join(filename::unique(&stem, &|n| dir.join(n).exists()));
                }
                r.file_name = content;
            }
        }
        r.size = Some(selected);
        r.torrent = Some(sp.clone());
        out_dir(&r.dir, &r.file_name, &sp)
    };
    inner.save_now();

    // 3) İndir
    inner.apply_torrent_limits(&client);
    let initial_peers = inner.settings.read().torrent.initial_peers.clone();
    client.add(&bytes, &out, AddOptions { only_files: spec.files.clone(), initial_peers, ..Default::default() })?;
    let hash = spec.info_hash.clone();
    let mut last_save = Instant::now();
    let mut last_done: Option<u64> = None;
    // Web seed: eşlerden ilerleme yoksa bir kez denenir
    let mut stall = Instant::now();
    let mut stall_done = 0u64;
    let mut web_seed_tried = false;
    loop {
        if stopped() {
            client.pause(&hash)?;
            sync(task, &client, &hash);
            return Ok((Status::Stopped, String::new()));
        }
        let Some(p) = client.progress(&hash) else { bail!("torrent oturumdan düştü") };
        // Kota ve oturum sayacı: yalnız doğrulanmış yeni veri sayılır (doğrulama sırasındaki artış değil)
        if p.state == State::Live {
            if let Some(prev) = last_done {
                let d = p.done.saturating_sub(prev);
                task.received.fetch_add(d, Ordering::Relaxed);
                inner.total_received.fetch_add(d, Ordering::Relaxed);
            }
            last_done = Some(p.done);
        }
        task.speed.store(p.down_speed, Ordering::Relaxed);
        task.connections.store(p.peers as u64, Ordering::Relaxed);
        *task.torrent.lock() = Live {
            peers: p.peers,
            seen: p.seen,
            upload_speed: p.up_speed,
            checking: p.state == State::Checking,
            resolving: false,
        };
        {
            let mut st = task.state.lock();
            if let Some(t) = st.rec.torrent.as_mut() {
                t.done = p.done;
                t.uploaded = p.uploaded;
            }
            // Doğrulama başlamadan librqbit toplamı 0 bildirebilir
            st.rec.size = Some(if p.total > 0 { p.total } else { selected });
        }
        inner.mark_changed();
        if p.state == State::Error {
            return Err(anyhow!(p.error.unwrap_or_else(|| "torrent hatası".into())));
        }
        if p.finished && p.state == State::Live {
            if inner.settings.read().torrent.seed_after {
                inner.seed(task.id, hash.clone(), p.total);
            } else {
                // Oturumdan çıkarılır: librqbit dosyaları yazma kipinde açık tutar, Gezgin kopyalayamaz
                client.remove(&hash, false)?;
            }
            return Ok((Status::Completed, String::new()));
        }
        if p.state != State::Live || p.done > stall_done {
            stall = Instant::now();
            stall_done = p.done;
        }
        let now = inner.settings.read().torrent.web_seed_now;
        if !web_seed_tried && !meta.web_seeds.is_empty() && p.state == State::Live && (now || stall.elapsed() >= WEB_SEED_AFTER) {
            web_seed_tried = true;
            task.state.lock().notice = Some("Eşlerden veri gelmiyor; torrentin HTTP kaynağından (web seed) indiriliyor".into());
            inner.mark_changed();
            // Dosyalar librqbit'te açıkken üzerine yazılamaz; çıkarılır, HTTP ile indirilir, yeniden eklenince
            // her parça torrentteki özetle doğrulanır (bozuk ya da eksik parça eşlerden istenir)
            client.remove(&hash, false)?;
            let r = web_seed(inner, task, &meta, &spec, &out, &p.files);
            task.state.lock().notice = None;
            match r {
                Err(_) if stopped() => return Ok((Status::Stopped, String::new())),
                Err(e) => task.state.lock().notice = Some(format!("Web seed başarısız: {e:#}")),
                Ok(()) => {}
            }
            let initial_peers = inner.settings.read().torrent.initial_peers.clone();
            client.add(&bytes, &out, AddOptions { only_files: spec.files.clone(), initial_peers, ..Default::default() })?;
            last_done = None;
            stall = Instant::now();
            continue;
        }
        if last_save.elapsed() >= CHECKPOINT {
            inner.save();
            last_save = Instant::now();
        }
        std::thread::sleep(TICK);
    }
}

/// Seçili ve eksik dosyaları web seed'den indirir. Her dosya `<ad>.kdmws` geçici dosyasına iner (kesilirse
/// oradan Range ile sürer), boyutu doğruysa asıl adın yerine konur.
fn web_seed(
    inner: &Arc<Inner>,
    task: &Arc<Task>,
    meta: &kdm_torrent::Metadata,
    spec: &TorrentSpec,
    out: &std::path::Path,
    have: &[u64],
) -> Result<()> {
    use super::http::{self, ByteRange, DigestState, RequestSpec};
    use std::io::{Read, Write};
    let agent = inner.agent()?;
    let wanted: Vec<usize> = (0..meta.files.len())
        .filter(|&i| !meta.files[i].padding && spec.files.as_ref().is_none_or(|s| s.contains(&i)))
        .collect();
    let have_of = |i: usize| have.get(i).copied().unwrap_or(0).min(meta.files[i].size);
    let mut done: u64 = wanted.iter().map(|&i| have_of(i)).sum();
    let mut last_err = None;
    let mut window = (Instant::now(), task.received.load(Ordering::Relaxed));
    for &i in &wanted {
        let f = &meta.files[i];
        if have_of(i) >= f.size {
            continue;
        }
        done -= have_of(i);
        let rel: std::path::PathBuf = f.path.split('/').collect();
        let target = out.join(rel);
        if let Some(d) = target.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = target.with_file_name(format!("{}.kdmws", target.file_name().and_then(|n| n.to_str()).unwrap_or("dosya")));
        let mut ok = false;
        for base in &meta.web_seeds {
            let url = kdm_torrent::web_seed_url(base, meta, f);
            let start = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0).min(f.size);
            let req = RequestSpec { url: url.clone(), ..Default::default() };
            let range = (start > 0).then_some(ByteRange { start, end: None });
            let r = match http::get(&agent, &req, range, "", &DigestState::default()) {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(format!("{url}: {e:#}"));
                    continue;
                }
            };
            let resumed = r.meta.status == 206 && start > 0;
            if r.meta.status != 200 && !resumed {
                last_err = Some(format!("{url}: HTTP {}", r.meta.status));
                continue;
            }
            let mut file = if resumed { std::fs::OpenOptions::new().append(true).open(&tmp)? } else { std::fs::File::create(&tmp)? };
            let mut got = if resumed { start } else { 0 };
            done += got;
            let mut body = r.body.into_reader();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                if task.stop.load(Ordering::Relaxed) {
                    bail!("durduruldu");
                }
                let n = body.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if got + n as u64 > f.size {
                    bail!("{url}: sunucu torrentteki boyuttan fazla veri gönderdi");
                }
                if !inner.global_limit.acquire(n as u64, &task.stop) {
                    bail!("durduruldu");
                }
                file.write_all(&buf[..n])?;
                got += n as u64;
                done += n as u64;
                task.received.fetch_add(n as u64, Ordering::Relaxed);
                inner.total_received.fetch_add(n as u64, Ordering::Relaxed);
                if window.0.elapsed() >= Duration::from_secs(1) {
                    let rx = task.received.load(Ordering::Relaxed);
                    task.speed.store((rx - window.1) * 1000 / (window.0.elapsed().as_millis().max(1) as u64), Ordering::Relaxed);
                    window = (Instant::now(), rx);
                    if let Some(t) = task.state.lock().rec.torrent.as_mut() {
                        t.done = done;
                    }
                    inner.mark_changed();
                }
            }
            drop(file);
            if got == f.size {
                // Yarım ya da eski dosyanın yerine konur (Windows'ta rename hedefin üstüne yazar)
                std::fs::rename(&tmp, &target)?;
                ok = true;
                break;
            }
            done -= got;
            last_err = Some(format!("{url}: bağlantı erken kapandı ({got}/{} bayt)", f.size));
        }
        if !ok {
            bail!("{}", last_err.unwrap_or_else(|| "web seed yok".into()));
        }
    }
    task.speed.store(0, Ordering::Relaxed);
    Ok(())
}

/// Bitmiş bir torrenti yeniden paylaşır: dosyalar diskte doğrulanır, eksik ya da değişmişse hiçbir şey
/// indirilmez (yoksa paylaşma sessizce yeniden indirmeye dönerdi).
pub(crate) fn reseed(inner: &Arc<Inner>, task: &Arc<Task>) -> Result<()> {
    let (spec, out) = {
        let st = task.state.lock();
        let spec = st.rec.torrent.clone().ok_or_else(|| anyhow!("torrent değil"))?;
        let out = out_dir(&st.rec.dir, &st.rec.file_name, &spec);
        (spec, out)
    };
    let bytes = std::fs::read(cache_path(inner, &spec.info_hash)).map_err(|_| anyhow!("torrent bilgisi bulunamadı"))?;
    let meta = kdm_torrent::parse_torrent(&bytes)?;
    // Önce boyut denetimi: dosya yoksa librqbit onu boş olarak oluşturup indirmeye başlardı
    for (i, f) in meta.files.iter().enumerate() {
        if f.padding || spec.files.as_ref().is_some_and(|s| !s.contains(&i)) {
            continue;
        }
        let p = if meta.multi_file { out.join(&f.path) } else { out.join(&meta.files[0].path) };
        if std::fs::metadata(&p).map(|m| m.len()).ok() != Some(f.size) {
            bail!("dosya eksik ya da değişmiş: {}", p.display());
        }
    }
    let client = inner.torrent_client()?;
    inner.apply_torrent_limits(&client);
    let hash = spec.info_hash.clone();
    let opts = |paused| AddOptions { only_files: spec.files.clone(), paused, ..Default::default() };
    client.add(&bytes, &out, opts(true))?;
    // Duraklatılmış eklenen torrent önce diskteki veriyi doğrular
    let p = loop {
        let Some(p) = client.progress(&hash) else { bail!("torrent oturumdan düştü") };
        if p.state != State::Checking {
            break p;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if p.state == State::Error || p.done < p.total {
        let _ = client.remove(&hash, false);
        bail!("{}", p.error.unwrap_or_else(|| "dosyalar torrentle uyuşmuyor; paylaşmak için yeniden indirin".into()));
    }
    client.add(&bytes, &out, opts(false))?;
    inner.seed(task.id, hash, p.total);
    Ok(())
}

fn sync(task: &Task, client: &Client, hash: &str) {
    if let Some(p) = client.progress(hash) {
        if let Some(t) = task.state.lock().rec.torrent.as_mut() {
            t.done = p.done;
            t.uploaded = p.uploaded;
        }
    }
}
