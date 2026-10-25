//! Ağa çıkmadan uçtan uca: aynı süreçte bir librqbit oturumu dosyaları paylaşır (seeder), istemcimiz
//! yalnız 127.0.0.1 üzerinden ondan indirir. DHT, izleyici ve yerel ağ araması kapalıdır.

use kdm_torrent::*;
use librqbit::{CreateTorrentOptions, ListenerOptions, Session, SessionOptions};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Seeder {
    rt: tokio::runtime::Runtime,
    _session: Arc<Session>,
    addr: SocketAddr,
    torrent: Vec<u8>,
    magnet: String,
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kdm-torrent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn data(n: usize, seed: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 31 + seed * 7 + i / 4093) % 251) as u8).collect()
}

/// `src` altındaki `album` klasörünü paylaşır.
fn seed(src: &Path) -> Seeder {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let opts = SessionOptions {
        dht: None,
        disable_trackers: true,
        disable_local_service_discovery: true,
        listen: Some(ListenerOptions { listen_addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() }),
        ..Default::default()
    };
    let session = rt.block_on(Session::new_with_opts(src.to_path_buf(), opts)).unwrap();
    let (res, _h) = rt
        .block_on(session.create_and_serve_torrent(
            &src.join("album"),
            CreateTorrentOptions { name: Some("album"), trackers: vec![], piece_length: Some(64 * 1024) },
        ))
        .map_err(|e| format!("{e:?}"))
        .unwrap();
    let torrent = res.as_bytes().unwrap().to_vec();
    let magnet = format!("magnet:?xt=urn:btih:{}&dn=album", res.info_hash().as_string());
    let port = session.listen_addr().unwrap().port();
    Seeder { rt, _session: session, addr: SocketAddr::from(([127, 0, 0, 1], port)), torrent, magnet }
}

fn source(name: &str) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let src = temp(&format!("{name}-src"));
    let album = src.join("album");
    std::fs::create_dir_all(album.join("cd2")).unwrap();
    let a = data(3 * 1024 * 1024 + 123, 1);
    let b = data(700 * 1024 + 9, 2);
    std::fs::write(album.join("a.bin"), &a).unwrap();
    std::fs::write(album.join("cd2").join("b.bin"), &b).unwrap();
    (src, a, b)
}

fn client(state: &Path) -> Client {
    let opts = Options { listen_port: None, dht: false, upnp: false, lsd: false, trackers: false, state_dir: state.to_path_buf() };
    Client::start(opts, state.to_path_buf()).unwrap()
}

fn wait_finished(c: &Client, hash: &str) -> Progress {
    let t = Instant::now();
    loop {
        let p = c.progress(hash).expect("torrent oturumda yok");
        if p.finished && p.state == State::Live || p.finished && p.state == State::Paused {
            return p;
        }
        assert!(p.state != State::Error, "{:?}", p.error);
        assert!(t.elapsed() < Duration::from_secs(60), "zamanında bitmedi: {p:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn downloads_full_torrent_byte_exact() {
    let (src, a, b) = source("full");
    let s = seed(&src);
    let meta = parse_torrent(&s.torrent).unwrap();
    assert_eq!(meta.name, "album");
    assert!(meta.multi_file);
    assert_eq!(meta.total, (a.len() + b.len()) as u64);
    let mut paths: Vec<_> = meta.files.iter().filter(|f| !f.padding).map(|f| f.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, ["a.bin", "cd2/b.bin"]);

    let state = temp("full-state");
    let out = temp("full-out").join("album");
    let c = client(&state);
    let hash = c.add(&s.torrent, &out, AddOptions { initial_peers: vec![s.addr], ..Default::default() }).unwrap();
    assert_eq!(hash, meta.info_hash);
    let p = wait_finished(&c, &hash);
    assert_eq!(p.done, meta.total);
    assert_eq!(std::fs::read(out.join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(out.join("cd2").join("b.bin")).unwrap(), b);
    c.shutdown();
    drop(s.rt);
}

#[test]
fn only_selected_files_are_downloaded() {
    let (src, _a, b) = source("sel");
    let s = seed(&src);
    let meta = parse_torrent(&s.torrent).unwrap();
    let idx = meta.files.iter().position(|f| f.path == "cd2/b.bin").unwrap();
    let state = temp("sel-state");
    let out = temp("sel-out");
    let c = client(&state);
    let hash = c
        .add(&s.torrent, &out, AddOptions { only_files: Some(vec![idx]), initial_peers: vec![s.addr], ..Default::default() })
        .unwrap();
    let p = wait_finished(&c, &hash);
    assert_eq!(p.total, b.len() as u64, "toplam yalnız seçili dosya");
    assert_eq!(std::fs::read(out.join("cd2").join("b.bin")).unwrap(), b);
    // Seçilmeyen dosya ya hiç yok ya da tam değil (komşu parça paylaşıldığı için kısmen yazılmış olabilir)
    let a_len = std::fs::metadata(out.join("a.bin")).map(|m| m.len()).unwrap_or(0);
    let a_have = std::fs::read(out.join("a.bin")).unwrap_or_default();
    assert!(a_len == 0 || a_have.iter().filter(|&&x| x != 0).count() < a_have.len() / 2, "seçilmeyen dosya inmiş");
    c.shutdown();
    drop(s.rt);
}

#[test]
fn magnet_metadata_from_peer_and_cancel() {
    let (src, a, b) = source("mag");
    let s = seed(&src);
    let state = temp("mag-state");
    let c = client(&state);
    let (hash, name) = parse_magnet(&s.magnet).unwrap();
    assert_eq!(name.as_deref(), Some("album"));
    let meta = c.resolve(Source::Magnet(&s.magnet), &[s.addr], &|| false).unwrap();
    assert_eq!(meta.info_hash, hash);
    assert_eq!(meta.total, (a.len() + b.len()) as u64);
    // Eşten gelen metadata kaydedilip yerel olarak aynı biçimde ayrıştırılabilir
    let again = parse_torrent(&meta.torrent).unwrap();
    assert_eq!(again.info_hash, hash);
    assert_eq!(again.files, meta.files);

    // Eşi olmayan bir magnet: iptal hemen döner
    let lonely = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
    let t = Instant::now();
    let r = c.resolve(Source::Magnet(lonely), &[], &|| t.elapsed() > Duration::from_millis(300));
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(5));
    c.shutdown();
    drop(s.rt);
}

#[test]
fn resumes_after_restart_and_pause() {
    let (src, a, b) = source("res");
    let s = seed(&src);
    let state = temp("res-state");
    let out = temp("res-out").join("album");
    {
        let c = client(&state);
        c.set_limits(512 * 1024, 0);
        let hash = c.add(&s.torrent, &out, AddOptions { initial_peers: vec![s.addr], ..Default::default() }).unwrap();
        let t = Instant::now();
        while c.progress(&hash).unwrap().done < 1024 * 1024 {
            assert!(t.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(50));
        }
        c.pause(&hash).unwrap();
        let p = c.progress(&hash).unwrap();
        assert_eq!(p.state, State::Paused);
        assert!(!p.finished, "hız sınırı uygulanmadı: {:.1} sn'de bitti", t.elapsed().as_secs_f64());
        c.shutdown();
    }
    // Yeni oturum: var olan veri doğrulanır, kalan kısım iner
    let c = client(&state);
    let hash = c.add(&s.torrent, &out, AddOptions { initial_peers: vec![s.addr], ..Default::default() }).unwrap();
    wait_finished(&c, &hash);
    assert_eq!(std::fs::read(out.join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(out.join("cd2").join("b.bin")).unwrap(), b);
    // Silme dosyaları da kaldırır
    c.remove(&hash, true).unwrap();
    assert!(c.progress(&hash).is_none());
    assert!(!out.join("a.bin").exists());
    c.shutdown();
    drop(s.rt);
}

/// Bencode ile elle yazılmış, dosya yolu `C:` bileşeni içeren torrent.
#[test]
fn drive_prefixed_path_is_rejected() {
    fn bstr(s: &str) -> String {
        format!("{}:{}", s.len(), s)
    }
    let pieces = "x".repeat(20);
    let info = format!(
        "d5:filesld6:lengthi1e4:pathl{}{}eee4:name{}12:piece lengthi16384e6:pieces{}e",
        bstr("C:"),
        bstr("evil.exe"),
        bstr("ok"),
        bstr(&pieces)
    );
    let t = format!("d4:info{info}e");
    let e = parse_torrent(t.as_bytes()).unwrap_err().to_string();
    assert!(e.contains("güvensiz"), "{e}");
    // Aynı yapı güvenli adla kabul edilir (yapının kendisi geçerli)
    let ok = t.replace(&bstr("C:"), &bstr("dir"));
    assert!(parse_torrent(ok.as_bytes()).is_ok());
}
