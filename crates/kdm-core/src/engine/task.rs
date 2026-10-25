//! Bir indirmenin yürütülmesi: bir denetleyici iş parçacığı ve bağlantı başına bir işçi iş parçacığı.
//!
//! Eşzamanlılık kuralı: işçi yazmadan önce yazacağı aralığı kilit altında `reserved` ile ayırır, G/Ç'yi kilit
//! dışında yapar, sonra `pos`'u ilerletir. Bölme kararı `max(pos, reserved)`'ı alt sınır kabul ettiği için
//! iki bağlantı asla aynı bölgeye yazmaz. Sunucu Range'i yok sayınca indirme baştan başlar; eski işçiler
//! `generation` değişince kendiliğinden çekilir.

use super::filename;
use super::http::{self, ByteRange, DigestState, RequestSpec};
use super::transport;
use super::limiter::Limiter;
use super::model::{HashAlgo, RangeSupport, Record, Segment, Status};
use super::partfile::PartFile;
use super::split::{self, SegView, SplitRule};
use super::Inner;
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(200);
const CHECKPOINT: Duration = Duration::from_secs(3);
/// Bağlantı sınırı bu aralıkla ikiye katlanır
const RAMP: Duration = Duration::from_millis(500);
const BUF: usize = 64 * 1024;
/// Bir parçada art arda bu kadar başarısız denemeden sonra parça bırakılır
const MAX_SEG_FAILS: u32 = 6;

#[derive(Clone, Debug, Default)]
pub(crate) struct SegRt {
    pub active: bool,
    pub reserved: u64,
    /// Bu oturumda alınan bayt (hız ölçümü)
    pub recv: u64,
    pub last_recv: u64,
    pub speed: u64,
    pub fails: u32,
    /// Bu parça hangi parçadan bölündü (Range desteklenmezse geri katılır)
    pub parent: Option<usize>,
}

pub(crate) struct State {
    pub rec: Record,
    pub rt: Vec<SegRt>,
    pub file: Option<Arc<PartFile>>,
    pub generation: u64,
    /// Ölümcül hata (404, disk dolu...): tüm indirme durur
    pub fatal: Option<String>,
    pub notice: Option<String>,
}

pub struct Task {
    pub id: u64,
    pub(crate) state: Mutex<State>,
    pub stop: AtomicBool,
    pub running: AtomicBool,
    pub limiter: Limiter,
    /// Bu oturumda alınan toplam bayt
    pub received: AtomicU64,
    pub speed: AtomicU64,
    pub connections: AtomicU64,
    pub(crate) digest: DigestState,
    pub media_done: AtomicU64,
    pub media_total: AtomicU64,
    pub media_bytes: AtomicU64,
    pub(crate) torrent: Mutex<super::torrent::Live>,
}

impl Task {
    pub(crate) fn new(rec: Record) -> Self {
        let limit = rec.speed_limit;
        let rt = vec![SegRt::default(); rec.segments.len()];
        Task {
            id: rec.id,
            state: Mutex::new(State { rec, rt, file: None, generation: 0, fatal: None, notice: None }),
            stop: AtomicBool::new(false),
            running: AtomicBool::new(false),
            limiter: Limiter::new(limit),
            received: AtomicU64::new(0),
            speed: AtomicU64::new(0),
            connections: AtomicU64::new(0),
            digest: DigestState::default(),
            media_done: AtomicU64::new(0),
            media_total: AtomicU64::new(0),
            media_bytes: AtomicU64::new(0),
            torrent: Mutex::new(Default::default()),
        }
    }

    pub fn record(&self) -> Record {
        self.state.lock().rec.clone()
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Güçlü ETag ya da Last-Modified: `If-Range` için. Zayıf ETag (`W/`) If-Range'de kullanılamaz.
fn validator(rec: &Record) -> String {
    if !rec.etag.is_empty() && !rec.etag.starts_with("W/") {
        rec.etag.clone()
    } else {
        rec.last_modified.clone()
    }
}

/// Denetleyici: indirme bitene, durdurulana ya da hata verene kadar çalışır.
pub(crate) fn supervise(inner: Arc<Inner>, task: Arc<Task>) {
    let (media, torrent) = {
        let st = task.state.lock();
        (st.rec.media.clone(), st.rec.torrent.clone())
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match (media, torrent) {
        (_, Some(t)) => super::torrent::run(&inner, &task, t),
        (Some(m), _) => super::media::run(&inner, &task, m),
        _ => run(&inner, &task),
    }));
    let (status, msg) = match result {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => (Status::Error, format!("{e:#}")),
        Err(_) => (Status::Error, "iç hata".to_string()),
    };
    {
        let mut st = task.state.lock();
        st.rec.status = status;
        if status == Status::Error {
            st.rec.error = msg.clone();
        }
        if status == Status::Completed {
            st.rec.finished = now();
        }
        st.file = None;
        for r in st.rt.iter_mut() {
            r.active = false;
        }
    }
    task.speed.store(0, Ordering::Relaxed);
    task.connections.store(0, Ordering::Relaxed);
    task.running.store(false, Ordering::Release);
    // Durum değişikliği kısılmamalı: son kontrol noktasından 1 sn içinde bitse bile kaydedilir
    inner.save_now();
    inner.mark_changed();
    inner.sink.finished(task.id, status, &msg);
}

fn run(inner: &Arc<Inner>, task: &Arc<Task>) -> Result<(Status, String)> {
    {
        let mut st = task.state.lock();
        st.generation += 1;
        st.fatal = None;
        st.notice = None;
        st.rec.status = Status::Downloading;
        st.rec.error.clear();
        prepare(&mut st);
    }
    task.stop.store(false, Ordering::Relaxed);
    task.received.store(0, Ordering::Relaxed);
    inner.mark_changed();

    let max_conns = {
        let st = task.state.lock();
        if st.rec.max_connections > 0 { st.rec.max_connections } else { inner.settings.read().max_connections }
    }
    .clamp(1, 32)
    .min(if transport::is_ftp(&task.state.lock().rec.url) { transport::FTP_MAX_CONNECTIONS } else { 32 });
    let mut allowed: u32 = {
        // Devam ederken yarım kalmış parçalar hemen paralel başlasın
        let st = task.state.lock();
        let open = st.rec.segments.iter().filter(|s| !s.is_done()).count() as u32;
        if st.rec.range == RangeSupport::Yes { open.clamp(1, max_conns) } else { 1 }
    };
    let rule = SplitRule::default();
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut last_ramp = Instant::now();
    let mut last_seg_speed = Instant::now();
    let mut last_checkpoint = Instant::now();
    let mut samples: std::collections::VecDeque<(Instant, u64)> = std::collections::VecDeque::new();

    let outcome = loop {
        std::thread::sleep(TICK);
        workers.retain(|h| !h.is_finished());

        if task.stop.load(Ordering::Relaxed) {
            break Ok((Status::Stopped, String::new()));
        }
        if let Some(f) = task.state.lock().fatal.clone() {
            task.stop.store(true, Ordering::Relaxed);
            break Err(anyhow!(f));
        }

        // Hız (son 5 sn)
        let total = task.received.load(Ordering::Relaxed);
        let t = Instant::now();
        samples.push_back((t, total));
        while samples.len() > 2 && t.duration_since(samples[0].0) > Duration::from_secs(5) {
            samples.pop_front();
        }
        let speed = match (samples.front(), samples.back()) {
            (Some(a), Some(b)) if b.0 > a.0 => ((b.1 - a.1) as f64 / b.0.duration_since(a.0).as_secs_f64()) as u64,
            _ => 0,
        };
        task.speed.store(speed, Ordering::Relaxed);

        let limit = effective_limit(task.limiter.rate(), inner.global_limit.rate());
        if last_seg_speed.elapsed() >= Duration::from_secs(1) {
            let secs = last_seg_speed.elapsed().as_secs_f64();
            let mut st = task.state.lock();
            for r in st.rt.iter_mut() {
                r.speed = ((r.recv - r.last_recv) as f64 / secs) as u64;
                r.last_recv = r.recv;
            }
            drop(st);
            last_seg_speed = Instant::now();
        }
        if last_ramp.elapsed() >= RAMP {
            allowed = split::next_allowed(allowed, max_conns, limit, speed);
            last_ramp = Instant::now();
        }

        // Yeni bağlantılar
        let spawn: (Vec<usize>, u64) = {
            let mut st = task.state.lock();
            let mut list = Vec::new();
            let active = st.rt.iter().filter(|r| r.active).count() as u32;
            let mut slots = allowed.saturating_sub(active);
            let range = st.rec.range;
            let parallel_ok = matches!(range, RangeSupport::Yes | RangeSupport::Probable);
            // Henüz ilk yanıt gelmediyse ya da Range desteklenmiyorsa yalnız tek bağlantı
            let first_pending = range == RangeSupport::Unknown && active > 0;
            if !first_pending {
                // 1) Yarım kalmış, sahipsiz parçalar
                for i in 0..st.rec.segments.len() {
                    if slots == 0 || (!parallel_ok && active + (list.len() as u32) >= 1) {
                        break;
                    }
                    let seg = &st.rec.segments[i];
                    let r = &st.rt[i];
                    if !r.active && !seg.is_done() && r.fails < MAX_SEG_FAILS {
                        list.push(i);
                        slots -= 1;
                    }
                }
                // 2) Bölme (Probable iken doğrulanana kadar yalnız bir ek bağlantı)
                let probing = range == RangeSupport::Probable && st.rt.iter().skip(1).any(|r| r.active);
                if parallel_ok && !probing && st.rec.size.is_some() {
                    while slots > 0 {
                        let views: Vec<SegView> = st
                            .rec
                            .segments
                            .iter()
                            .zip(&st.rt)
                            .map(|(s, r)| SegView { pos: s.pos.max(r.reserved), end: s.end, active: r.active, speed: r.speed })
                            .collect();
                        let Some((i, at)) = split::pick_split(&views, rule) else { break };
                        let old_end = st.rec.segments[i].end;
                        st.rec.segments[i].end = Some(at - 1);
                        st.rec.segments.push(Segment { start: at, end: old_end, pos: at });
                        st.rt.push(SegRt { parent: Some(i), ..Default::default() });
                        list.push(st.rec.segments.len() - 1);
                        slots -= 1;
                        if range == RangeSupport::Probable {
                            break;
                        }
                    }
                }
            }
            for &i in &list {
                st.rt[i].active = true;
            }
            // Nesil, sunucu Range'i yok sayıp indirme baştan başladığında değişir
            (list, st.generation)
        };
        let (spawn, generation) = spawn;
        for i in spawn {
            let (inner2, task2) = (inner.clone(), task.clone());
            workers.push(std::thread::spawn(move || worker(inner2, task2, i, generation)));
        }

        let (active, done, stuck) = {
            let st = task.state.lock();
            let active = st.rt.iter().filter(|r| r.active).count();
            let done = st.rec.size == Some(0)
                || (!st.rec.segments.is_empty() && st.rec.segments.iter().all(|s| s.is_done()));
            let stuck = active == 0
                && st.rec.segments.iter().zip(&st.rt).any(|(s, r)| !s.is_done() && r.fails >= MAX_SEG_FAILS);
            (active, done, stuck)
        };
        task.connections.store(active as u64, Ordering::Relaxed);
        inner.mark_changed();

        if done && active == 0 {
            break finalize(task);
        }
        if stuck {
            let msg = task.state.lock().notice.clone().unwrap_or_else(|| "bağlantı kurulamadı".into());
            break Err(anyhow!(msg));
        }
        if last_checkpoint.elapsed() >= CHECKPOINT {
            checkpoint(inner, task);
            last_checkpoint = Instant::now();
        }
    };

    // İşçilerin çekilmesini bekle (okuma zaman aşımına kadar sürebilir; durdurma bayrağı her okumada denetlenir)
    task.stop.store(true, Ordering::Relaxed);
    for h in workers {
        let _ = h.join();
    }
    checkpoint(inner, task);
    outcome
}

fn effective_limit(own: u64, global: u64) -> u64 {
    match (own, global) {
        (0, g) => g,
        (o, 0) => o,
        (o, g) => o.min(g),
    }
}

/// Başlamadan önce: kayıtlı ilerleme diskteki dosyayla tutarlı mı, parça listesi hazır mı.
fn prepare(st: &mut State) {
    let part = st.rec.part_path();
    if st.rec.downloaded() > 0 && !part.exists() {
        // Kullanıcı yarım dosyayı silmiş: baştan
        st.rec.segments.clear();
    }
    // Boş (geri katılmış) parçaları at
    st.rec.segments.retain(|s| !(s.is_done() && s.pos == s.start));
    if st.rec.segments.is_empty() {
        let end = st.rec.size.and_then(|s| s.checked_sub(1));
        st.rec.segments.push(Segment { start: 0, end, pos: 0 });
    }
    st.rec.segments.sort_by_key(|s| s.start);
    st.rt = vec![SegRt::default(); st.rec.segments.len()];
    for (s, r) in st.rec.segments.iter().zip(st.rt.iter_mut()) {
        r.reserved = s.pos;
    }
}

fn checkpoint(inner: &Arc<Inner>, task: &Arc<Task>) {
    let file = task.state.lock().file.clone();
    // Önce veri diske, sonra ilerleme kaydı: çökmede kayıt diskteki veriden ileride olamaz
    if let Some(f) = file {
        let _ = f.sync();
    }
    inner.save();
}

fn finalize(task: &Arc<Task>) -> Result<(Status, String)> {
    let (part, file, checksum) = {
        let mut st = task.state.lock();
        let file = st.file.take();
        let size = st.rec.segments.iter().filter_map(|s| s.end).max().map(|e| e + 1).unwrap_or(0);
        if st.rec.size.is_none() {
            st.rec.size = Some(size);
        }
        (st.rec.part_path(), file, st.rec.checksum.clone())
    };
    if let Some(f) = &file {
        f.sync()?;
        let size = task.state.lock().rec.size.unwrap_or(0);
        f.set_len(size)?;
    } else if !part.exists() {
        // Sıfır baytlık dosya: hiç yazma olmadı
        PartFile::open(&part)?;
    }
    drop(file);

    let target = {
        let mut st = task.state.lock();
        let dir = st.rec.dir.clone();
        // İndirme sürerken aynı adda bir dosya oluşmuş olabilir
        let name = filename::unique(&st.rec.file_name, &|n| dir.join(n).exists());
        st.rec.file_name = name;
        st.rec.target_path()
    };
    std::fs::rename(&part, &target).map_err(|e| anyhow!("dosya yeniden adlandırılamadı ({}): {e}", target.display()))?;

    if let Some(c) = checksum {
        let got = hash_file(&target, &c.algo)?;
        if !got.eq_ignore_ascii_case(c.hex.trim()) {
            return Ok((Status::Error, format!("özet uyuşmuyor: beklenen {}, bulunan {got}", c.hex.trim())));
        }
    }
    Ok((Status::Completed, String::new()))
}

pub fn hash_file(path: &std::path::Path, algo: &HashAlgo) -> Result<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    macro_rules! run {
        ($h:expr) => {{
            let mut h = $h;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }};
    }
    Ok(match algo {
        HashAlgo::Sha256 => run!(sha2::Sha256::new()),
        HashAlgo::Sha1 => run!(sha1::Sha1::new()),
        HashAlgo::Md5 => run!(md5::Md5::new()),
    })
}

enum Step {
    /// Parça bitti ya da işçi artık gerekli değil
    Exit,
    /// Bağlantı koptu / geçici hata: aynı parçayı yeniden dene
    Retry(String),
}

fn worker(inner: Arc<Inner>, task: Arc<Task>, idx: usize, generation: u64) {
    let mut backoff = Duration::from_millis(500);
    loop {
        let step = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attempt(&inner, &task, idx, generation))) {
            Ok(s) => s,
            Err(_) => Step::Retry("iç hata".into()),
        };
        let mut st = task.state.lock();
        if st.generation != generation {
            return;
        }
        match step {
            Step::Exit => {
                if let Some(r) = st.rt.get_mut(idx) {
                    r.active = false;
                }
                return;
            }
            Step::Retry(msg) => {
                let r = &mut st.rt[idx];
                r.fails += 1;
                let give_up = r.fails >= MAX_SEG_FAILS;
                st.notice = Some(msg);
                if give_up || task.stop.load(Ordering::Relaxed) {
                    st.rt[idx].active = false;
                    return;
                }
            }
        }
        drop(st);
        // Durdurma isteğine hızlı yanıt için kısa adımlarla bekle
        let until = Instant::now() + backoff;
        while Instant::now() < until {
            if task.stop.load(Ordering::Relaxed) {
                task.state.lock().rt[idx].active = false;
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        backoff = (backoff * 2).min(Duration::from_secs(16));
    }
}

fn attempt(inner: &Arc<Inner>, task: &Arc<Task>, idx: usize, generation: u64) -> Step {
    let (spec, range, if_range) = {
        let st = task.state.lock();
        if st.generation != generation {
            return Step::Exit;
        }
        let seg = &st.rec.segments[idx];
        if seg.is_done() {
            return Step::Exit;
        }
        let rec = &st.rec;
        let spec = RequestSpec {
            url: rec.url.clone(),
            referer: rec.referer.clone(),
            cookies: rec.cookies.clone(),
            user_agent: rec.user_agent.clone(),
            headers: rec.headers.clone(),
            auth: rec.auth.clone(),
        };
        // Her zaman Range gönderilir: ilk yanıtın 206 olması desteği hemen gösterir
        let range = ByteRange { start: seg.pos, end: seg.end };
        let if_range = if seg.pos > 0 { validator(rec) } else { String::new() };
        (spec, range, if_range)
    };

    let opened = if transport::is_ftp(&spec.url) {
        transport::open_ftp(&spec, range, &if_range, inner.timeout(), inner.ftp_tls())
    } else {
        match inner.agent() {
            Ok(agent) => transport::open_http(&agent, &spec, range, &if_range, &task.digest),
            Err(e) => Err(transport::Failure::Fatal(format!("{e:#}"))),
        }
    };
    let (meta, body) = match opened {
        Ok(x) => x,
        Err(transport::Failure::Retry(m)) => return Step::Retry(m),
        Err(transport::Failure::Fatal(m)) => return fatal(task, m),
        Err(transport::Failure::Refused(m)) => {
            let fresh = task.received.load(Ordering::Relaxed) == 0 && task.state.lock().rec.downloaded() == 0;
            return if fresh { fatal(task, m) } else { Step::Retry(m) };
        }
    };
    let mut start = range.start;

    match meta.status {
        206 => {
            let Some((cr_start, _, total)) = meta.content_range else {
                return Step::Retry("206 yanıtında Content-Range yok".into());
            };
            if cr_start != range.start {
                return Step::Retry(format!("sunucu istenen aralığı göndermedi ({cr_start} != {})", range.start));
            }
            let mut st = task.state.lock();
            if st.generation != generation {
                return Step::Exit;
            }
            st.rec.range = RangeSupport::Yes;
            if st.rec.size.is_none() {
                st.rec.size = total;
                if let (Some(t), None) = (total, st.rec.segments[idx].end) {
                    st.rec.segments[idx].end = t.checked_sub(1);
                }
            }
            on_first_response(inner, &mut st, &meta);
        }
        200 => {
            let mut st = task.state.lock();
            if st.generation != generation {
                return Step::Exit;
            }
            if range.start == 0 && idx == 0 && st.rec.segments.len() == 1 {
                // İlk ve tek bağlantı: tam gövde geliyor
                st.rec.range = if meta.accept_ranges.split(',').any(|v| v.trim() == "bytes") && meta.total_size().is_some() {
                    RangeSupport::Probable
                } else {
                    RangeSupport::No
                };
                st.rec.size = meta.total_size();
                st.rec.segments[0].end = st.rec.size.and_then(|s| s.checked_sub(1));
                on_first_response(inner, &mut st, &meta);
                if st.rec.size == Some(0) {
                    // Boş dosya: yazılacak parça yok, denetleyici boyut 0'ı tamamlanmış sayar
                    st.rec.segments.clear();
                    return Step::Exit;
                }
            } else if let Some(parent) = st.rt[idx].parent.filter(|_| st.rec.range != RangeSupport::Yes) {
                // Ek bağlantı tam dosyayı aldı: sunucu Range desteklemiyor. Parçayı ayrıldığı parçaya geri kat
                st.rec.range = RangeSupport::No;
                let end = st.rec.segments[idx].end;
                st.rec.segments[parent].end = end;
                let s = st.rec.segments[idx].start;
                st.rec.segments[idx].end = Some(s.saturating_sub(1));
                st.rec.segments[idx].pos = s;
                return Step::Exit;
            } else {
                // Devam isteğine tam dosya geldi: sunucu Range'i yok saydı ya da dosya sunucuda değişti
                // (If-Range uyuşmadı). Eski parçalar geçersiz: baştan. Nesil değiştiği için diğer işçiler çekilir,
                // denetleyici yeni neslin ilk işçisini başlatır.
                st.generation += 1;
                st.rec.range = RangeSupport::Unknown;
                st.rec.size = None;
                st.rec.etag.clear();
                st.rec.last_modified.clear();
                st.rec.segments = vec![Segment { start: 0, end: None, pos: 0 }];
                st.rt = vec![SegRt::default()];
                st.notice = Some("sunucu kaldığı yerden devam etmeyi kabul etmedi ya da dosya değişti; baştan indiriliyor".into());
                if let Some(f) = &st.file {
                    let _ = f.set_len(0);
                }
                return Step::Exit;
            }
        }
        401 | 403 | 404 | 410 => {
            return fatal(task, format!("HTTP {}: {}", meta.status, status_text(meta.status)));
        }
        s => return Step::Retry(format!("HTTP {s}: {}", status_text(s))),
    }

    // İçerik sıkıştırılmış geliyorsa (Accept-Encoding: identity'ye rağmen) aralıklar baytlarla örtüşmez
    if !meta.content_encoding.is_empty() && meta.content_encoding != "identity" {
        let mut st = task.state.lock();
        st.rec.range = RangeSupport::No;
        if idx != 0 || range.start != 0 {
            return Step::Retry("sunucu sıkıştırılmış içerik gönderdi".into());
        }
        st.rec.size = None;
        st.rec.segments[0].end = None;
        start = 0;
    }
    stream(inner, task, idx, generation, start, body)
}

fn fatal(task: &Arc<Task>, msg: String) -> Step {
    let mut st = task.state.lock();
    st.fatal.get_or_insert(msg);
    Step::Exit
}

fn status_text(code: u16) -> &'static str {
    match code {
        401 => "kimlik doğrulama gerekli",
        403 => "erişim engellendi",
        404 => "dosya bulunamadı",
        410 => "dosya artık yok",
        429 => "çok fazla istek",
        500..=599 => "sunucu hatası",
        _ => "beklenmeyen yanıt",
    }
}

/// İlk başarılı yanıt: dosya adı, doğrulayıcılar, tür.
fn on_first_response(inner: &Arc<Inner>, st: &mut State, meta: &http::Meta) {
    if !meta.final_url.is_empty() {
        st.rec.final_url = meta.final_url.clone();
    }
    if !meta.etag.is_empty() {
        st.rec.etag = meta.etag.clone();
    }
    if !meta.last_modified.is_empty() {
        st.rec.last_modified = meta.last_modified.clone();
    }
    if !meta.content_type.is_empty() {
        st.rec.content_type = meta.content_type.clone();
    }
    // Ad yalnız henüz hiçbir şey yazılmadıysa değişebilir (parça dosyasının adı ona bağlı)
    if st.file.is_none() && st.rec.downloaded() == 0 && !st.rec.name_locked {
        let name = filename::from_content_disposition(&meta.content_disposition)
            .or_else(|| filename::from_url(&st.rec.final_url))
            .or_else(|| filename::from_url(&st.rec.url));
        if let Some(n) = name {
            let n = filename::with_type_extension(n, &st.rec.content_type);
            if n != st.rec.file_name {
                let dir = st.rec.dir.clone();
                let id = st.rec.id;
                st.rec.file_name = filename::unique(&n, &|x| {
                    dir.join(x).exists() || dir.join(format!("{x}.kdm")).exists() || inner.name_taken(id, &dir, x)
                });
            }
        }
        st.rec.name_locked = true;
    }
}

fn open_file(st: &mut State) -> Result<Arc<PartFile>> {
    if let Some(f) = &st.file {
        return Ok(f.clone());
    }
    let f = Arc::new(PartFile::open(&st.rec.part_path())?);
    if let Some(size) = st.rec.size {
        f.set_len(size)?;
    }
    st.file = Some(f.clone());
    Ok(f)
}

/// Yanıt gövdesini parçanın konumuna yazar. Gövde aktarım katmanından bağımsızdır (HTTP, FTP).
fn stream(inner: &Arc<Inner>, task: &Arc<Task>, idx: usize, generation: u64, start: u64, mut reader: Box<dyn Read + Send>) -> Step {
    let file = {
        let mut st = task.state.lock();
        if st.generation != generation {
            return Step::Exit;
        }
        match open_file(&mut st) {
            Ok(f) => f,
            Err(e) => {
                st.fatal.get_or_insert(format!("{e:#}"));
                return Step::Exit;
            }
        }
    };
    let mut buf = vec![0u8; BUF];
    let mut pos = start;
    loop {
        if task.stop.load(Ordering::Relaxed) {
            return Step::Exit;
        }
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) => return Step::Retry(format!("bağlantı koptu: {e}")),
        };
        if n == 0 {
            let mut st = task.state.lock();
            if st.generation != generation {
                return Step::Exit;
            }
            let seg = &mut st.rec.segments[idx];
            return match seg.end {
                None => {
                    // Boyutu bilinmeyen akış bitti. Boş akışta parça kalmaz; boyut 0 tamamlanmış sayılır
                    seg.end = pos.checked_sub(1);
                    st.rec.size = Some(pos);
                    if pos == 0 {
                        st.rec.segments.clear();
                    }
                    Step::Exit
                }
                Some(_) if seg.is_done() => Step::Exit,
                Some(_) => Step::Retry("bağlantı erken kapandı".into()),
            };
        }
        if !task.limiter.acquire(n as u64, &task.stop) || !inner.global_limit.acquire(n as u64, &task.stop) {
            return Step::Exit;
        }
        let take = {
            let mut st = task.state.lock();
            if st.generation != generation {
                return Step::Exit;
            }
            let seg = &st.rec.segments[idx];
            if seg.pos != pos {
                // Parça başka biri tarafından değiştirildi (geri katma); bu işçi çekilir
                return Step::Exit;
            }
            let allowed = seg.end.map(|e| (e + 1).saturating_sub(pos)).unwrap_or(u64::MAX);
            let take = (n as u64).min(allowed) as usize;
            st.rt[idx].reserved = pos + take as u64;
            take
        };
        if take == 0 {
            return Step::Exit;
        }
        if let Err(e) = file.write_all_at(&buf[..take], pos) {
            let mut st = task.state.lock();
            st.fatal.get_or_insert(format!("diske yazılamadı: {e}"));
            return Step::Exit;
        }
        pos += take as u64;
        task.received.fetch_add(take as u64, Ordering::Relaxed);
        inner.total_received.fetch_add(take as u64, Ordering::Relaxed);
        let mut st = task.state.lock();
        if st.generation != generation {
            return Step::Exit;
        }
        st.rec.segments[idx].pos = pos;
        st.rt[idx].recv += take as u64;
        st.rt[idx].fails = 0;
        if st.rec.segments[idx].is_done() {
            return Step::Exit;
        }
    }
}
