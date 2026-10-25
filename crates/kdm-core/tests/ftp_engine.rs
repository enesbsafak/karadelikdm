//! Motorun FTP yolu: aynı süreçte çalışan küçük bir FTP sunucusuna karşı uçtan uca.

use kdm_core::engine::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Cfg {
    files: Mutex<HashMap<String, Vec<u8>>>,
    mdtm: Mutex<String>,
    rest: AtomicBool,
    /// RETR veri bağlantısı başına bayt/sn, 0: sınırsız
    throttle: AtomicU64,
    sessions: AtomicUsize,
    max_sessions: AtomicUsize,
    rest_commands: AtomicUsize,
}

struct Server {
    addr: SocketAddr,
    cfg: Arc<Cfg>,
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("ftp://{}{}", self.addr, path)
    }
}

fn server(name: &str, data: Vec<u8>) -> Server {
    let cfg = Arc::new(Cfg::default());
    cfg.files.lock().insert(name.into(), data);
    *cfg.mdtm.lock() = "20260101120000".into();
    cfg.rest.store(true, Ordering::Relaxed);
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c = cfg.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let c = c.clone();
            std::thread::spawn(move || {
                let n = c.sessions.fetch_add(1, Ordering::Relaxed) + 1;
                c.max_sessions.fetch_max(n, Ordering::Relaxed);
                session(s, &c);
                c.sessions.fetch_sub(1, Ordering::Relaxed);
            });
        }
    });
    Server { addr, cfg }
}

fn accept(l: &TcpListener) -> Option<TcpStream> {
    l.set_nonblocking(true).ok()?;
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Ok((s, _)) = l.accept() {
            s.set_nonblocking(false).ok()?;
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn session(s: TcpStream, cfg: &Cfg) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    macro_rules! say {
        ($($t:tt)*) => {{ let _ = write!(w, $($t)*); let _ = w.write_all(b"\r\n"); }};
    }
    say!("220 hazır");
    let mut rest = 0usize;
    let mut data: Option<TcpListener> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end().to_string();
        let (verb, arg) = line.split_once(' ').unwrap_or((&line, ""));
        match verb.to_ascii_uppercase().as_str() {
            "USER" => say!("331 parola"),
            "PASS" => say!("230 tamam"),
            "TYPE" | "OPTS" => say!("200 tamam"),
            "SIZE" => match cfg.files.lock().get(arg) {
                Some(f) => say!("213 {}", f.len()),
                None => say!("550 yok"),
            },
            "MDTM" => say!("213 {}", cfg.mdtm.lock()),
            "EPSV" => {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                say!("229 Extended Passive Mode (|||{}|)", l.local_addr().unwrap().port());
                data = Some(l);
            }
            "REST" if cfg.rest.load(Ordering::Relaxed) => {
                cfg.rest_commands.fetch_add(1, Ordering::Relaxed);
                rest = arg.parse().unwrap_or(0);
                say!("350 tamam");
            }
            "RETR" => {
                let Some(f) = cfg.files.lock().get(arg).cloned() else {
                    say!("550 dosya yok");
                    continue;
                };
                let Some(mut d) = data.take().and_then(|l| accept(&l)) else {
                    say!("425 veri bağlantısı yok");
                    continue;
                };
                say!("150 gönderiliyor");
                let body = &f[rest.min(f.len())..];
                rest = 0;
                let rate = cfg.throttle.load(Ordering::Relaxed);
                let t = Instant::now();
                let mut sent = 0usize;
                let mut ok = true;
                for part in body.chunks(16 * 1024) {
                    if d.write_all(part).is_err() {
                        ok = false;
                        break;
                    }
                    sent += part.len();
                    if rate > 0 {
                        let due = Duration::from_secs_f64(sent as f64 / rate as f64);
                        if let Some(w) = due.checked_sub(t.elapsed()) {
                            std::thread::sleep(w);
                        }
                    }
                }
                drop(d);
                if ok {
                    say!("226 bitti");
                } else {
                    say!("426 kesildi");
                }
            }
            "ABOR" => say!("226 iptal"),
            "QUIT" => {
                say!("221 güle güle");
                return;
            }
            _ => say!("500 bilinmeyen komut"),
        }
    }
}

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

fn open_env(dir: PathBuf) -> Env {
    let (tx, rx) = mpsc::channel();
    let engine = Engine::open(dir.join("downloads.json"), Arc::new(TestSink { tx: Mutex::new(tx) }));
    Env { dir, engine, rx }
}

fn env(name: &str) -> Env {
    let dir = std::env::temp_dir().join(format!("kdm-ftp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    open_env(dir)
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 37 + i / 6007) % 253) as u8).collect()
}

fn add(e: &Env, url: &str) -> u64 {
    e.engine.add(NewDownload { url: url.into(), start: true, ..Default::default() }, e.dir.clone(), "").unwrap()
}

fn wait(e: &Env, id: u64) -> (Status, String) {
    loop {
        let (got, status, msg) = e.rx.recv_timeout(Duration::from_secs(60)).expect("indirme zamanında bitmedi");
        if got == id {
            return (status, msg);
        }
    }
}

fn content(e: &Env, id: u64) -> Vec<u8> {
    std::fs::read(e.engine.record(id).unwrap().target_path()).unwrap()
}

#[test]
fn ftp_multi_connection_is_byte_exact_and_capped() {
    let d = data(4 * 1024 * 1024 + 77);
    let s = server("f.bin", d.clone());
    s.cfg.throttle.store(700 * 1024, Ordering::Relaxed);
    let e = env("multi");
    let id = add(&e, &s.url("/f.bin"));
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    let max = s.cfg.max_sessions.load(Ordering::Relaxed);
    assert!((2..=4).contains(&max), "eşzamanlı FTP oturumu: {max}");
    let rec = e.engine.record(id).unwrap();
    assert_eq!(rec.range, RangeSupport::Yes);
    assert_eq!(rec.file_name, "f.bin");
    assert_eq!(rec.last_modified, "20260101120000");
}

#[test]
fn ftp_resume_across_restart_uses_rest() {
    let d = data(3 * 1024 * 1024);
    let s = server("r.bin", d.clone());
    s.cfg.throttle.store(400 * 1024, Ordering::Relaxed);
    let e = env("resume");
    let id = add(&e, &s.url("/r.bin"));
    std::thread::sleep(Duration::from_millis(1500));
    e.engine.stop(id);
    assert_eq!(wait(&e, id).0, Status::Stopped);
    let done = e.engine.record(id).unwrap().downloaded();
    assert!(done > 0 && done < d.len() as u64);
    let dir = e.dir.clone();
    e.engine.shutdown(Duration::from_secs(5));
    drop(e);
    s.cfg.throttle.store(0, Ordering::Relaxed);
    let e = open_env(dir);
    e.engine.start(id);
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
}

#[test]
fn ftp_without_rest_uses_single_connection() {
    let d = data(1_500_000);
    let s = server("n.bin", d.clone());
    s.cfg.rest.store(false, Ordering::Relaxed);
    s.cfg.throttle.store(1024 * 1024, Ordering::Relaxed);
    let e = env("norest");
    let id = add(&e, &s.url("/n.bin"));
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d);
    assert_eq!(e.engine.record(id).unwrap().range, RangeSupport::No);
}

#[test]
fn ftp_missing_file_is_fatal() {
    let s = server("var.bin", data(10));
    let e = env("missing");
    let id = add(&e, &s.url("/yok.bin"));
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Error);
    assert!(msg.contains("550"), "{msg}");
}

#[test]
fn ftp_changed_file_restarts() {
    let d1 = data(2 * 1024 * 1024);
    let s = server("c.bin", d1);
    s.cfg.throttle.store(300 * 1024, Ordering::Relaxed);
    let e = env("changed");
    let id = add(&e, &s.url("/c.bin"));
    std::thread::sleep(Duration::from_millis(1200));
    e.engine.stop(id);
    wait(&e, id);
    let d2: Vec<u8> = data(1_000_000).into_iter().map(|b| b ^ 0x33).collect();
    s.cfg.files.lock().insert("c.bin".into(), d2.clone());
    *s.cfg.mdtm.lock() = "20260202120000".into();
    s.cfg.throttle.store(0, Ordering::Relaxed);
    e.engine.start(id);
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(content(&e, id), d2);
}

#[test]
fn ftp_probe() {
    let s = server("p.iso", data(123_456));
    let p = probe(&http::NetConfig::default(), &http::RequestSpec { url: s.url("/p.iso"), ..Default::default() }).unwrap();
    assert_eq!(p.file_name, "p.iso");
    assert_eq!(p.size, Some(123_456));
    assert!(p.resumable);
}
