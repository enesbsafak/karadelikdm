//! Motorun torrent yolu: aynı süreçte bir librqbit oturumu paylaşır, motor yalnız 127.0.0.1 üzerinden indirir.
//! DHT ve yerel ağ araması kapalı; eş adresi test ayarıyla verilir.

use kdm_core::engine::*;
use librqbit::{CreateTorrentOptions, ListenerOptions, Session, SessionOptions};
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

struct Seeder {
    _rt: tokio::runtime::Runtime,
    _session: Arc<Session>,
    addr: SocketAddr,
    torrent: Vec<u8>,
    hash: String,
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kdm-te-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn data(n: usize, seed: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 29 + seed * 11 + i / 3001) % 249) as u8).collect()
}

fn seed(path: &Path, name: &str) -> Seeder {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let opts = SessionOptions {
        dht: None,
        disable_trackers: true,
        disable_local_service_discovery: true,
        listen: Some(ListenerOptions { listen_addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() }),
        ..Default::default()
    };
    let session = rt.block_on(Session::new_with_opts(path.parent().unwrap().to_path_buf(), opts)).unwrap();
    let (res, _h) = rt
        .block_on(session.create_and_serve_torrent(
            path,
            CreateTorrentOptions { name: Some(name), trackers: vec![], piece_length: Some(32 * 1024) },
        ))
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    let port = session.listen_addr().unwrap().port();
    Seeder {
        torrent: res.as_bytes().unwrap().to_vec(),
        hash: res.info_hash().as_string(),
        _rt: rt,
        _session: session,
        addr: SocketAddr::from(([127, 0, 0, 1], port)),
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
    data: PathBuf,
    out: PathBuf,
    engine: Engine,
    rx: mpsc::Receiver<(u64, Status, String)>,
}

fn env(name: &str, peers: Vec<SocketAddr>, seed_after: bool) -> Env {
    let data = temp(&format!("{name}-data"));
    let out = temp(&format!("{name}-out"));
    let (tx, rx) = mpsc::channel();
    let engine = Engine::open(data.join("downloads.json"), Arc::new(TestSink { tx: Mutex::new(tx) }));
    engine.set_settings(EngineSettings {
        torrent: TorrentSettings {
            listen_port: 0,
            dht: false,
            upnp: false,
            lsd: false,
            seed_after,
            seed_ratio: 0.0,
            upload_limit: 0,
            initial_peers: peers,
            web_seed_now: false,
        },
        ..Default::default()
    });
    Env { data, out, engine, rx }
}

fn wait(e: &Env, id: u64) -> (Status, String) {
    loop {
        let (got, st, msg) = e.rx.recv_timeout(Duration::from_secs(60)).expect("indirme zamanında bitmedi");
        if got == id {
            return (st, msg);
        }
    }
}

#[test]
fn magnet_download_through_engine() {
    let src = temp("mag-src");
    let file = src.join("video.mkv");
    let d = data(2 * 1024 * 1024 + 77, 3);
    std::fs::write(&file, &d).unwrap();
    let s = seed(&file, "video.mkv");
    let e = env("mag", vec![s.addr], false);
    let magnet = format!("magnet:?xt=urn:btih:{}&dn=video.mkv", s.hash.to_uppercase());
    let id = e.engine.add(NewDownload { url: magnet, start: true, ..Default::default() }, e.out.clone(), "").unwrap();
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    let rec = e.engine.record(id).unwrap();
    assert_eq!(rec.file_name, "video.mkv");
    assert_eq!(rec.size, Some(d.len() as u64));
    assert_eq!(std::fs::read(rec.target_path()).unwrap(), d);
    let t = rec.torrent.as_ref().unwrap();
    assert_eq!(t.info_hash, s.hash);
    assert_eq!(t.done, d.len() as u64);
    // Metadata önbelleğe yazıldı
    assert!(e.data.join("torrents").join(format!("{}.torrent", s.hash)).exists());
    let info = e.engine.info(id).unwrap();
    assert_eq!(info.done, d.len() as u64);
    assert!(info.torrent.is_some());
    // Aynı torrent ikinci kez eklenemez
    assert!(e.engine.add(NewDownload { url: format!("magnet:?xt=urn:btih:{}", s.hash), ..Default::default() }, e.out.clone(), "").is_err());
    // Silme (dosyalarla)
    e.engine.remove(id, true).unwrap();
    assert!(!rec.target_path().exists());
    assert!(!e.data.join("torrents").join(format!("{}.torrent", s.hash)).exists());
    e.engine.shutdown(Duration::from_secs(5));
}

#[test]
fn torrent_file_with_selection_existing_name_and_resume() {
    let src = temp("sel-src");
    let album = src.join("album");
    std::fs::create_dir_all(album.join("ek")).unwrap();
    let a = data(1024 * 1024 + 5, 1);
    let b = data(300 * 1024 + 1, 2);
    std::fs::write(album.join("a.bin"), &a).unwrap();
    std::fs::write(album.join("ek").join("b.bin"), &b).unwrap();
    let s = seed(&album, "album");
    let e = env("sel", vec![s.addr], false);
    // Hedefte aynı adlı (başka) bir klasör var: üstüne yazılmaz
    std::fs::create_dir_all(e.out.join("album")).unwrap();
    std::fs::write(e.out.join("album").join("a.bin"), b"kullanicinin dosyasi").unwrap();

    let meta = e.engine.torrent_metadata(kdm_torrent::Source::File(&s.torrent), &|| false).unwrap();
    let idx = meta.files.iter().position(|f| f.path == "ek/b.bin").unwrap();
    let nd = NewDownload {
        url: format!("magnet:?xt=urn:btih:{}", meta.info_hash),
        torrent: Some(TorrentSpec { info_hash: meta.info_hash.clone(), files: Some(vec![idx]), ..Default::default() }),
        start: true,
        ..Default::default()
    };
    let id = e.engine.add(nd, e.out.clone(), "").unwrap();
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    let rec = e.engine.record(id).unwrap();
    assert_ne!(rec.file_name, "album", "var olan klasörün üstüne yazıldı");
    assert_eq!(rec.size, Some(b.len() as u64), "boyut yalnız seçili dosya");
    assert_eq!(std::fs::read(rec.target_path().join("ek").join("b.bin")).unwrap(), b);
    assert_eq!(std::fs::read(e.out.join("album").join("a.bin")).unwrap(), b"kullanicinin dosyasi");

    // Baştan indir: dosyalar silinir, yeniden iner
    e.engine.restart(id).unwrap();
    assert!(!rec.target_path().join("ek").join("b.bin").exists());
    assert!(e.engine.start(id));
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(std::fs::read(rec.target_path().join("ek").join("b.bin")).unwrap(), b);
    e.engine.shutdown(Duration::from_secs(5));
}

#[test]
fn stopped_magnet_without_peers_returns_quickly() {
    // Bağlantıyı kabul edip hiç yanıt vermeyen bir eş: metadata hiç gelmez, indirme "bilgiler alınıyor" durumunda
    // bekler. (Kapalı port olmaz: macOS reddi anında bildirir, eş listesi tükenir ve librqbit hemen vazgeçer.)
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let silent_addr = silent.local_addr().unwrap();
    let held = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let held = held.clone();
        std::thread::spawn(move || {
            for s in silent.incoming().flatten() {
                held.lock().push(s);
            }
        });
    }
    let e = env("stop", vec![silent_addr], false);
    let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=yok";
    let id = e.engine.add(NewDownload { url: magnet.into(), start: true, ..Default::default() }, e.out.clone(), "").unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let info = e.engine.info(id).unwrap();
    assert!(info.torrent.as_ref().unwrap().resolving, "{info:?}");
    e.engine.stop(id);
    let (st, _) = wait(&e, id);
    assert_eq!(st, Status::Stopped);
    assert_eq!(e.engine.record(id).unwrap().file_name, "yok");
    // Geçersiz magnet
    assert!(e.engine.add(NewDownload { url: "magnet:?dn=x".into(), ..Default::default() }, e.out.clone(), "").is_err());
    e.engine.shutdown(Duration::from_secs(5));
}

/// Paylaşılırken librqbit dosyayı yazma kipinde açık tutar: Windows'ta paylaşımsız açılamaz (kurulum programı
/// "dosya kullanılıyor" der). Paylaşma bitince serbest kalmalı.
fn locked(p: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(p).is_err()
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        false
    }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = std::time::Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(30), "zaman aşımı: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn completed_torrent_is_not_seeded_or_locked_by_default() {
    let src = temp("noseed-src");
    let file = src.join("setup.exe");
    std::fs::write(&file, data(300 * 1024, 4)).unwrap();
    let s = seed(&file, "setup.exe");
    let e = env("noseed", vec![s.addr], false);
    let id = e
        .engine
        .add(NewDownload { url: format!("magnet:?xt=urn:btih:{}", s.hash), start: true, ..Default::default() }, e.out.clone(), "")
        .unwrap();
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert!(!e.engine.info(id).unwrap().torrent.unwrap().seeding);
    assert!(!locked(&e.engine.record(id).unwrap().target_path()), "bitmiş dosya kilitli");
    e.engine.shutdown(Duration::from_secs(5));
}

#[test]
fn seeding_can_be_stopped_and_restarted() {
    let src = temp("seed-src");
    let file = src.join("f.bin");
    let d = data(256 * 1024, 9);
    std::fs::write(&file, &d).unwrap();
    let s = seed(&file, "f.bin");
    let e = env("seed", vec![s.addr], true);
    let id = e
        .engine
        .add(NewDownload { url: format!("magnet:?xt=urn:btih:{}", s.hash), start: true, ..Default::default() }, e.out.clone(), "")
        .unwrap();
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    let target = e.engine.record(id).unwrap().target_path();
    let seeding = |e: &Env| e.engine.info(id).unwrap().torrent.unwrap().seeding;
    assert!(seeding(&e));
    if cfg!(windows) {
        assert!(locked(&target), "paylaşılırken dosyanın açık tutulması beklenirdi (sınamanın varsayımı)");
    }
    // Durdur: dosya serbest
    e.engine.stop(id);
    assert!(!seeding(&e));
    assert!(!locked(&target), "paylaşma durunca dosya serbest kalmalı");
    assert_eq!(e.engine.record(id).unwrap().status, Status::Completed);
    // Yeniden paylaş: diskteki veri doğrulanır, indirme olmaz
    e.engine.seed(id).unwrap();
    wait_until("yeniden paylaşma", || seeding(&e));
    assert_eq!(std::fs::read(&target).unwrap(), d);
    e.engine.stop(id);
    // Değişmiş dosya paylaşılmaz ve yeniden indirilmez
    std::fs::write(&target, b"degisti").unwrap();
    e.engine.seed(id).unwrap();
    wait_until("hata bildirimi", || !e.engine.info(id).unwrap().error.is_empty());
    assert!(!seeding(&e));
    assert!(e.engine.info(id).unwrap().error.contains("Paylaşılamadı"));
    assert_eq!(std::fs::read(&target).unwrap(), b"degisti");
    // Aynı boyutta ama bozuk içerik: librqbit doğrulaması yakalar
    let mut bad = d.clone();
    bad[1000] ^= 0xff;
    std::fs::write(&target, &bad).unwrap();
    e.engine.seed(id).unwrap();
    wait_until("doğrulama", || {
        let i = e.engine.info(id).unwrap();
        let t = i.torrent.unwrap();
        t.seeding || (!t.checking && !i.error.is_empty())
    });
    assert!(!seeding(&e), "bozuk dosya paylaşılmamalı");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(std::fs::read(&target).unwrap(), bad, "bozuk dosya sessizce yeniden indirilmemeli");
    e.engine.shutdown(Duration::from_secs(5));
}

/// Range destekli küçük HTTP sunucusu: `/dl/<yol>` -> `root/<yol>`. `corrupt` verilirse o dosyanın bir baytı bozulur.
fn http_server(root: PathBuf, corrupt: Option<&'static str>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let root = root.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                let rel = percent_encoding::percent_decode_str(path.trim_start_matches("/dl/")).decode_utf8_lossy().into_owned();
                let mut w = s;
                let Ok(mut body) = std::fs::read(root.join(&rel)) else {
                    let _ = w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    return;
                };
                if corrupt.is_some_and(|c| rel.ends_with(c)) {
                    let mid = body.len() / 2;
                    body[mid] ^= 0xff;
                }
                let total = body.len();
                let (status, from) = match range {
                    Some(a) if a < total => (format!("206 Partial Content\r\nContent-Range: bytes {a}-{}/{total}", total - 1), a),
                    _ => ("200 OK".to_string(), 0),
                };
                let _ = write!(w, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n", total - from);
                let _ = w.write_all(&body[from..]);
            });
        }
    });
    format!("http://{addr}/dl/")
}

/// `.torrent` sonuna `url-list` ekler (anahtar sıralaması: "info" < "url-list")
fn with_web_seed(torrent: &[u8], url: &str) -> Vec<u8> {
    let mut t = torrent[..torrent.len() - 1].to_vec();
    t.extend_from_slice(format!("8:url-list{}:{}e", url.len(), url).as_bytes());
    t
}

fn web_seed_env(name: &str) -> (Env, Vec<u8>, PathBuf, Vec<u8>, Vec<u8>) {
    let src = temp(&format!("{name}-src"));
    let album = src.join("album");
    std::fs::create_dir_all(album.join("ek")).unwrap();
    let a = data(900 * 1024 + 3, 5);
    let b = data(200 * 1024 + 7, 6);
    std::fs::write(album.join("a.bin"), &a).unwrap();
    std::fs::write(album.join("ek").join("b c.bin"), &b).unwrap();
    // Paylaşıcı yalnız .torrent üretmek için; adresi motora verilmez (eş yok)
    let s = seed(&album, "album");
    let e = env(name, vec![], false);
    e.engine.set_settings(EngineSettings {
        torrent: TorrentSettings { dht: false, lsd: false, web_seed_now: true, ..Default::default() },
        ..Default::default()
    });
    (e, s.torrent, src, a, b)
}

#[test]
fn web_seed_completes_without_peers() {
    let (e, torrent, src, a, b) = web_seed_env("ws");
    let t = with_web_seed(&torrent, &http_server(src, None));
    let meta = e.engine.torrent_metadata(kdm_torrent::Source::File(&t), &|| false).unwrap();
    assert_eq!(meta.web_seeds.len(), 1);
    let nd = NewDownload {
        url: format!("magnet:?xt=urn:btih:{}", meta.info_hash),
        torrent: Some(TorrentSpec { info_hash: meta.info_hash.clone(), ..Default::default() }),
        start: true,
        ..Default::default()
    };
    let id = e.engine.add(nd, e.out.clone(), "").unwrap();
    let (st, msg) = wait(&e, id);
    assert_eq!(st, Status::Completed, "{msg}");
    let rec = e.engine.record(id).unwrap();
    assert_eq!(std::fs::read(rec.target_path().join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(rec.target_path().join("ek").join("b c.bin")).unwrap(), b);
    assert!(!rec.target_path().join("a.bin.kdmws").exists());
    // Bitti ve paylaşma kapalı: dosya kilitli değil, silinebilir/taşınabilir
    let moved = rec.target_path().join("a.taşındı");
    std::fs::rename(rec.target_path().join("a.bin"), &moved).unwrap();
    e.engine.shutdown(Duration::from_secs(5));
}

#[test]
fn corrupt_web_seed_is_not_accepted() {
    let (e, torrent, src, _a, _b) = web_seed_env("wsbad");
    let t = with_web_seed(&torrent, &http_server(src, Some("a.bin")));
    let meta = e.engine.torrent_metadata(kdm_torrent::Source::File(&t), &|| false).unwrap();
    let nd = NewDownload {
        url: format!("magnet:?xt=urn:btih:{}", meta.info_hash),
        torrent: Some(TorrentSpec { info_hash: meta.info_hash.clone(), ..Default::default() }),
        start: true,
        ..Default::default()
    };
    let id = e.engine.add(nd, e.out.clone(), "").unwrap();
    // Bozuk parça özet denetiminden geçemez; eş olmadığı için indirme bitmemeli
    assert!(e.rx.recv_timeout(Duration::from_secs(8)).is_err(), "bozuk veriyle tamamlandı");
    let info = e.engine.info(id).unwrap();
    assert_eq!(info.status, Status::Downloading);
    assert!(info.done < meta.total, "{} / {}", info.done, meta.total);
    e.engine.stop(id);
    assert_eq!(wait(&e, id).0, Status::Stopped);
    e.engine.shutdown(Duration::from_secs(5));
}
