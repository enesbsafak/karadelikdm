//! Motoru yerel bir test HTTP sunucusuna karşı uçtan uca sınar.

use kdm_core::engine::*;
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------- test sunucusu ----------

#[derive(Default)]
struct ServerCfg {
    data: Mutex<Arc<Vec<u8>>>,
    etag: Mutex<String>,
    ranges: AtomicBool,
    accept_ranges_header: AtomicBool,
    /// bağlantı başına bayt/sn, 0: sınırsız
    throttle: AtomicU64,
    no_length: AtomicBool,
    content_disposition: Mutex<String>,
    status: AtomicU64,
    /// İlk N yanıt gövdenin yarısında kesilir
    cut_first: AtomicUsize,
    // ölçümler
    requests: AtomicUsize,
    range_requests: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    bytes_sent: AtomicU64,
}

struct Server {
    cfg: Arc<ServerCfg>,
    url: String,
}

fn server(data: Vec<u8>) -> Server {
    let cfg = Arc::new(ServerCfg::default());
    *cfg.data.lock() = Arc::new(data);
    *cfg.etag.lock() = "\"v1\"".into();
    cfg.ranges.store(true, Ordering::Relaxed);
    cfg.accept_ranges_header.store(true, Ordering::Relaxed);
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/files/test.bin", l.local_addr().unwrap());
    let c = cfg.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let c = c.clone();
            std::thread::spawn(move || {
                let _ = handle(&c, s);
            });
        }
    });
    Server { cfg, url }
}

fn handle(c: &ServerCfg, mut s: TcpStream) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut range = None;
    let mut if_range = None;
    let mut line = String::new();
    r.read_line(&mut line)?;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" {
            break;
        }
        let (k, v) = h.split_once(':').unwrap_or(("", ""));
        let v = v.trim().to_string();
        match k.to_ascii_lowercase().as_str() {
            "range" => range = Some(v),
            "if-range" => if_range = Some(v),
            _ => {}
        }
    }
    c.requests.fetch_add(1, Ordering::Relaxed);
    let n = c.active.fetch_add(1, Ordering::Relaxed) + 1;
    c.max_active.fetch_max(n, Ordering::Relaxed);
    let res = respond(c, &mut s, range, if_range);
    c.active.fetch_sub(1, Ordering::Relaxed);
    res
}

fn respond(c: &ServerCfg, s: &mut TcpStream, range: Option<String>, if_range: Option<String>) -> std::io::Result<()> {
    let data = c.data.lock().clone();
    let etag = c.etag.lock().clone();
    let status = c.status.load(Ordering::Relaxed);
    if status != 0 {
        write!(s, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    let total = data.len() as u64;
    let mut code = 200;
    let (mut a, mut b) = (0u64, total.saturating_sub(1));
    let range_ok = if_range.as_ref().is_none_or(|v| *v == etag);
    if let (Some(rg), true, true) = (&range, c.ranges.load(Ordering::Relaxed), range_ok) {
        c.range_requests.fetch_add(1, Ordering::Relaxed);
        let spec = rg.trim_start_matches("bytes=");
        let (x, y) = spec.split_once('-').unwrap();
        a = x.parse().unwrap();
        if !y.is_empty() {
            b = y.parse::<u64>().unwrap().min(total.saturating_sub(1));
        }
        if a >= total && total > 0 {
            write!(s, "HTTP/1.1 416 X\r\nContent-Range: bytes */{total}\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        }
        code = 206;
    }
    let body: &[u8] = if total == 0 { &[] } else { &data[a as usize..=b as usize] };
    let mut head = format!("HTTP/1.1 {code} {}\r\nETag: {etag}\r\nConnection: close\r\n", if code == 206 { "Partial" } else { "OK" });
    if code == 206 {
        head += &format!("Content-Range: bytes {a}-{b}/{total}\r\n");
    }
    if c.accept_ranges_header.load(Ordering::Relaxed) {
        head += "Accept-Ranges: bytes\r\n";
    }
    if !c.no_length.load(Ordering::Relaxed) {
        head += &format!("Content-Length: {}\r\n", body.len());
    }
    let cd = c.content_disposition.lock().clone();
    if !cd.is_empty() {
        head += &format!("Content-Disposition: {cd}\r\n");
    }
    head += "\r\n";
    s.write_all(head.as_bytes())?;
    let cut = c.cut_first.load(Ordering::Relaxed) > 0 && c.cut_first.fetch_sub(1, Ordering::Relaxed) > 0;
    let body = if cut { &body[..body.len() / 2] } else { body };
    let rate = c.throttle.load(Ordering::Relaxed);
    let chunk = 16 * 1024;
    let start = Instant::now();
    let mut sent = 0u64;
    for part in body.chunks(chunk) {
        s.write_all(part)?;
        sent += part.len() as u64;
        c.bytes_sent.fetch_add(part.len() as u64, Ordering::Relaxed);
        if rate > 0 {
            let due = Duration::from_secs_f64(sent as f64 / rate as f64);
            if let Some(w) = due.checked_sub(start.elapsed()) {
                std::thread::sleep(w);
            }
        }
    }
    s.flush()?;
    if cut {
        let _ = s.shutdown(std::net::Shutdown::Both);
    }
    Ok(())
}

// ---------- yardımcılar ----------

struct TestSink {
    tx: Mutex<mpsc::Sender<(u64, Status, String)>>,
}

impl Sink for TestSink {
    fn progress(&self, _: Vec<Info>) {}
    fn finished(&self, id: u64, status: Status, message: &str) {
        let _ = self.tx.lock().send((id, status, message.to_string()));
    }
}

struct Env {
    dir: PathBuf,
    engine: Engine,
    rx: mpsc::Receiver<(u64, Status, String)>,
}

fn env(name: &str) -> Env {
    let dir = std::env::temp_dir().join(format!("kdm-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    open_env(dir)
}

fn open_env(dir: PathBuf) -> Env {
    let (tx, rx) = mpsc::channel();
    let engine = Engine::open(dir.join("downloads.json"), Arc::new(TestSink { tx: Mutex::new(tx) }));
    Env { dir, engine, rx }
}

fn data(n: usize) -> Vec<u8> {
    // Konuma bağlı desen: yanlış yere yazılan bayt hemen fark edilir
    (0..n).map(|i| ((i * 31 + i / 7919) % 251) as u8).collect()
}

fn add(e: &Env, url: &str) -> u64 {
    e.engine.add(NewDownload { url: url.into(), start: true, ..Default::default() }, e.dir.clone(), "").unwrap()
}

fn wait(e: &Env, id: u64, secs: u64) -> (Status, String) {
    loop {
        let (got, status, msg) = e.rx.recv_timeout(Duration::from_secs(secs)).expect("indirme zamanında bitmedi");
        if got == id {
            return (status, msg);
        }
    }
}

fn content(e: &Env, id: u64) -> Vec<u8> {
    std::fs::read(e.engine.record(id).unwrap().target_path()).unwrap()
}

// ---------- testler ----------

#[test]
fn multi_connection_download_is_byte_exact() {
    let d = data(6 * 1024 * 1024 + 123);
    let s = server(d.clone());
    s.cfg.throttle.store(1024 * 1024, Ordering::Relaxed);
    let e = env("multi");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 60);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    assert!(s.cfg.max_active.load(Ordering::Relaxed) >= 3, "en az 3 eşzamanlı bağlantı beklenirdi");
    let rec = e.engine.record(id).unwrap();
    assert_eq!(rec.range, RangeSupport::Yes);
    assert_eq!(rec.file_name, "test.bin");
    assert!(!rec.part_path().exists());
    // Gereksiz yeniden indirme olmamalı (bölünen bağlantıların fazladan okuduğu kısa kuyruklar hariç)
    let sent = s.cfg.bytes_sent.load(Ordering::Relaxed);
    assert!(sent < d.len() as u64 * 13 / 10, "gönderilen {sent}");
}

#[test]
fn server_without_range_support_uses_one_connection() {
    let d = data(3 * 1024 * 1024);
    let s = server(d.clone());
    s.cfg.ranges.store(false, Ordering::Relaxed);
    s.cfg.accept_ranges_header.store(false, Ordering::Relaxed);
    s.cfg.throttle.store(2 * 1024 * 1024, Ordering::Relaxed);
    let e = env("norange");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 60);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    assert_eq!(s.cfg.max_active.load(Ordering::Relaxed), 1);
    assert_eq!(e.engine.record(id).unwrap().range, RangeSupport::No);
}

#[test]
fn accept_ranges_but_ignores_range_falls_back() {
    // Accept-Ranges: bytes diyor ama Range'i yok sayıyor: ek bağlantı 200 alır, tek akışa dönülür
    let d = data(4 * 1024 * 1024);
    let s = server(d.clone());
    s.cfg.ranges.store(false, Ordering::Relaxed);
    s.cfg.throttle.store(1024 * 1024, Ordering::Relaxed);
    let e = env("liar");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 60);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    assert_eq!(e.engine.record(id).unwrap().range, RangeSupport::No);
}

#[test]
fn stop_and_resume_across_restart() {
    let d = data(5 * 1024 * 1024);
    let s = server(d.clone());
    s.cfg.throttle.store(512 * 1024, Ordering::Relaxed);
    let e = env("resume");
    let id = add(&e, &s.url);
    std::thread::sleep(Duration::from_millis(2500));
    e.engine.stop(id);
    let (st, _) = wait(&e, id, 30);
    assert_eq!(st, Status::Stopped);
    let rec = e.engine.record(id).unwrap();
    let done = rec.downloaded();
    assert!(done > 0 && done < d.len() as u64, "ilerleme {done}");
    assert!(rec.part_path().exists());
    let dir = e.dir.clone();
    e.engine.shutdown(Duration::from_secs(5));
    drop(e);

    // Uygulama yeniden açılmış gibi
    let sent_before = s.cfg.bytes_sent.load(Ordering::Relaxed);
    s.cfg.throttle.store(0, Ordering::Relaxed);
    let e = open_env(dir);
    assert_eq!(e.engine.record(id).unwrap().downloaded(), done);
    assert!(e.engine.start(id));
    let (st, msg) = wait(&e, id, 60);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    let resent = s.cfg.bytes_sent.load(Ordering::Relaxed) - sent_before;
    assert!(resent <= d.len() as u64 - done + 512 * 1024, "devamda fazla veri: {resent}");
}

#[test]
fn changed_file_on_server_restarts_from_scratch() {
    let d1 = data(4 * 1024 * 1024);
    let s = server(d1.clone());
    s.cfg.throttle.store(512 * 1024, Ordering::Relaxed);
    let e = env("ifrange");
    let id = add(&e, &s.url);
    std::thread::sleep(Duration::from_millis(1500));
    e.engine.stop(id);
    wait(&e, id, 30);
    // Sunucudaki dosya değişti
    let d2: Vec<u8> = data(3 * 1024 * 1024).into_iter().map(|b| b ^ 0x5a).collect();
    *s.cfg.data.lock() = Arc::new(d2.clone());
    *s.cfg.etag.lock() = "\"v2\"".into();
    s.cfg.throttle.store(0, Ordering::Relaxed);
    e.engine.start(id);
    let (st, msg) = wait(&e, id, 60);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d2);
}

#[test]
fn unknown_size_stream() {
    let d = data(700_000);
    let s = server(d.clone());
    s.cfg.no_length.store(true, Ordering::Relaxed);
    s.cfg.ranges.store(false, Ordering::Relaxed);
    s.cfg.accept_ranges_header.store(false, Ordering::Relaxed);
    let e = env("nolen");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 30);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    assert_eq!(e.engine.record(id).unwrap().size, Some(d.len() as u64));
}

#[test]
fn empty_file() {
    let s = server(Vec::new());
    s.cfg.ranges.store(false, Ordering::Relaxed);
    let e = env("empty");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 30);
    assert_eq!(st, Status::Completed, "{msg}");
    assert!(content(&e, id).is_empty());
}

#[test]
fn content_disposition_name_and_dedupe() {
    let d = data(10_000);
    let s = server(d.clone());
    *s.cfg.content_disposition.lock() = "attachment; filename*=UTF-8''%C3%A7%C4%B1kt%C4%B1.bin".into();
    let e = env("cd");
    std::fs::write(e.dir.join("çıktı.bin"), b"var olan").unwrap();
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 30);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(e.engine.record(id).unwrap().file_name, "çıktı (1).bin");
    assert_eq!(content(&e, id), d);
    assert_eq!(std::fs::read(e.dir.join("çıktı.bin")).unwrap(), b"var olan");
}

#[test]
fn not_found_is_an_error() {
    let s = server(data(10));
    s.cfg.status.store(404, Ordering::Relaxed);
    let e = env("404");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 30);
    assert_eq!(st, Status::Error);
    assert!(msg.contains("404"), "{msg}");
}

#[test]
fn dropped_connections_are_retried() {
    let d = data(2 * 1024 * 1024);
    let s = server(d.clone());
    s.cfg.cut_first.store(3, Ordering::Relaxed);
    let e = env("cut");
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 60);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
}

#[test]
fn speed_limit_is_respected() {
    let d = data(1_500_000);
    let s = server(d.clone());
    let e = env("limit");
    e.engine.set_global_limit(600_000);
    let t = Instant::now();
    let id = add(&e, &s.url);
    let (st, msg) = wait(&e, id, 30);
    assert_eq!(st, Status::Completed, "{msg}");
    let secs = t.elapsed().as_secs_f64();
    assert!(secs >= 2.0, "1,5 MB 600 KB/s ile {secs:.2} sn sürdü");
    assert_eq!(content(&e, id), d);
}

#[test]
fn checksum_mismatch_is_reported() {
    let d = data(50_000);
    let s = server(d.clone());
    let e = env("sum");
    let good = e
        .engine
        .add(
            NewDownload {
                url: s.url.clone(),
                file_name: "good.bin".into(),
                checksum: Some(Checksum { algo: HashAlgo::Sha256, hex: sha256(&d) }),
                start: true,
                ..Default::default()
            },
            e.dir.clone(),
            "",
        )
        .unwrap();
    assert_eq!(wait(&e, good, 30).0, Status::Completed);
    let bad = e
        .engine
        .add(
            NewDownload {
                url: s.url.clone(),
                file_name: "bad.bin".into(),
                checksum: Some(Checksum { algo: HashAlgo::Md5, hex: "00".repeat(16) }),
                start: true,
                ..Default::default()
            },
            e.dir.clone(),
            "",
        )
        .unwrap();
    let (st, msg) = wait(&e, bad, 30);
    assert_eq!(st, Status::Error);
    assert!(msg.contains("özet"), "{msg}");
}

fn sha256(d: &[u8]) -> String {
    let p = std::env::temp_dir().join(format!("kdm-sha-{}", std::process::id()));
    std::fs::write(&p, d).unwrap();
    let h = hash_file(&p, &HashAlgo::Sha256).unwrap();
    std::fs::remove_file(p).unwrap();
    h
}

#[allow(dead_code)]
fn read_all(mut r: impl Read) -> Vec<u8> {
    let mut v = Vec::new();
    r.read_to_end(&mut v).unwrap();
    v
}


#[test]
fn probe_reports_name_size_and_resume() {
    let s = server(data(123_456));
    *s.cfg.content_disposition.lock() = "attachment; filename=\"rapor.pdf\"".into();
    let p = probe(&http::NetConfig::default(), &http::RequestSpec { url: s.url.clone(), ..Default::default() }).unwrap();
    assert_eq!(p.file_name, "rapor.pdf");
    assert_eq!(p.size, Some(123_456));
    assert!(p.resumable);
    // gövde okunmadı: yalnızca 1 bayt gönderilmiş olmalı
    std::thread::sleep(Duration::from_millis(100));
    assert!(s.cfg.bytes_sent.load(Ordering::Relaxed) <= 1);
}

#[test]
fn final_status_is_persisted_immediately() {
    // Kontrol noktasından hemen sonra biten indirmenin durumu da diske yazılmalı
    let s = server(data(200_000));
    let e = env("persist");
    let id = add(&e, &s.url);
    assert_eq!(wait(&e, id, 30).0, Status::Completed);
    let text = std::fs::read_to_string(e.dir.join("downloads.json")).unwrap();
    let recs: Vec<Record> = serde_json::from_str(&text).unwrap();
    assert_eq!(recs.iter().find(|r| r.id == id).unwrap().status, Status::Completed);
}

fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[test]
fn refused_connection_fails_fast_on_new_download() {
    let e = env("refused");
    for scheme in ["http", "ftp", "ftps"] {
        let t = Instant::now();
        let id = add(&e, &format!("{scheme}://127.0.0.1:{}/a.bin", closed_port()));
        let (st, msg) = wait(&e, id, 20);
        assert_eq!(st, Status::Error, "{scheme}: {msg}");
        // Eskiden 6 yeniden deneme x katlanan bekleme ~30 sn sürüyordu
        assert!(t.elapsed() < Duration::from_secs(8), "{scheme}: {:?}", t.elapsed());
    }
}

#[test]
fn refused_connection_is_retried_when_download_has_data() {
    let d = data(4 * 1024 * 1024);
    let s = server(d);
    s.cfg.throttle.store(512 * 1024, Ordering::Relaxed);
    let e = env("refused-resume");
    let id = add(&e, &s.url);
    std::thread::sleep(Duration::from_millis(1500));
    e.engine.stop(id);
    assert_eq!(wait(&e, id, 30).0, Status::Stopped);
    assert!(e.engine.record(id).unwrap().downloaded() > 0);
    // Sunucu kapandı (yeniden başlıyor olabilir): yarım indirme hemen hataya düşmez, yeniden dener
    let port = closed_port();
    e.engine.update(id, |r| r.url = format!("http://127.0.0.1:{port}/files/test.bin")).unwrap();
    assert!(e.engine.start(id));
    assert!(e.rx.recv_timeout(Duration::from_secs(4)).is_err(), "yarım indirme bağlantı reddinde hemen bitti");
    assert_eq!(e.engine.info(id).unwrap().status, Status::Downloading);
    e.engine.stop(id);
    assert_eq!(wait(&e, id, 30).0, Status::Stopped);
}
