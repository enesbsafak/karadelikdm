//! Motorun HLS yolu: statik dosya sunan küçük bir HTTP sunucusuna karşı uçtan uca.

use kdm_core::engine::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
struct Site {
    files: Mutex<HashMap<String, (String, Vec<u8>)>>,
    hits: Mutex<HashMap<String, usize>>,
    /// Her segment yanıtından önce bekleme (ms)
    delay_ms: AtomicU64,
}

fn serve(site: Arc<Site>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let site = site.clone();
            std::thread::spawn(move || handle(&site, s));
        }
    });
    base
}

fn handle(site: &Site, mut s: TcpStream) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
    }
    *site.hits.lock().entry(path.clone()).or_default() += 1;
    if path.ends_with(".ts") {
        std::thread::sleep(Duration::from_millis(site.delay_ms.load(Ordering::Relaxed)));
    }
    let file = site.files.lock().get(&path).cloned();
    let _ = match file {
        Some((ct, body)) => {
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            s.write_all(&body)
        }
        None => s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
}

/// 20 segmentlik bir HLS akışı; birleşik beklenen çıktı döner.
fn hls_site(site: &Site) -> Vec<u8> {
    let mut f = site.files.lock();
    f.insert(
        "/live/master.m3u8".into(),
        (
            "application/vnd.apple.mpegurl".into(),
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720\nhigh/index.m3u8\n".to_vec(),
        ),
    );
    let mut expected = Vec::new();
    for q in ["low", "high"] {
        let mut pl = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n");
        for i in 0..20 {
            pl.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
            // TS paket senkron baytıyla başlayan sahte segment
            let body: Vec<u8> = std::iter::once(0x47u8).chain((0..30_000).map(|k| ((k * 13 + i * 7) % 251) as u8)).collect();
            if q == "high" {
                expected.extend_from_slice(&body);
            }
            f.insert(format!("/live/{q}/seg{i}.ts"), ("video/mp2t".into(), body));
        }
        pl.push_str("#EXT-X-ENDLIST\n");
        f.insert(format!("/live/{q}/index.m3u8"), ("application/vnd.apple.mpegurl".into(), pl.into_bytes()));
    }
    expected
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

fn env(name: &str) -> (PathBuf, Engine, mpsc::Receiver<(u64, Status, String)>) {
    let dir = std::env::temp_dir().join(format!("kdm-media-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (tx, rx) = mpsc::channel();
    let engine = Engine::open(dir.join("downloads.json"), Arc::new(TestSink { tx: Mutex::new(tx) }));
    // Testte ffmpeg kullanılmaz: makinede kurulu olsa bile sahte TS verisi birleştirilemez
    engine.set_settings(EngineSettings { ffmpeg: "none".into(), ..Default::default() });
    (dir, engine, rx)
}

fn wait(rx: &mpsc::Receiver<(u64, Status, String)>, id: u64) -> (Status, String) {
    loop {
        let (got, st, msg) = rx.recv_timeout(Duration::from_secs(60)).expect("zamanında bitmedi");
        if got == id {
            return (st, msg);
        }
    }
}

#[test]
fn hls_download_picks_best_variant_and_concatenates() {
    let site = Arc::new(Site::default());
    let expected = hls_site(&site);
    let base = serve(site.clone());
    let (dir, engine, rx) = env("best");
    let id = engine
        .add(NewDownload { url: format!("{base}/live/master.m3u8"), media: Some(MediaSpec::default()), start: true, ..Default::default() }, dir.clone(), "video")
        .unwrap();
    let (st, msg) = wait(&rx, id);
    assert_eq!(st, Status::Completed, "{msg}");
    let rec = engine.record(id).unwrap();
    // ffmpeg yok: çıktı .ts olarak kalır
    assert!(rec.file_name.ends_with(".ts"), "{}", rec.file_name);
    assert_eq!(std::fs::read(rec.target_path()).unwrap(), expected);
    assert_eq!(rec.size, Some(expected.len() as u64));
    // Düşük kalitenin segmentleri hiç istenmemeli
    assert!(!site.hits.lock().keys().any(|k| k.starts_with("/live/low/seg")));
}

#[test]
fn hls_stop_and_resume_skips_finished_segments() {
    let site = Arc::new(Site::default());
    let expected = hls_site(&site);
    site.delay_ms.store(400, Ordering::Relaxed);
    let base = serve(site.clone());
    let (dir, engine, rx) = env("resume");
    let id = engine
        .add(NewDownload { url: format!("{base}/live/master.m3u8"), media: Some(MediaSpec::default()), start: true, ..Default::default() }, dir.clone(), "video")
        .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    engine.stop(id);
    assert_eq!(wait(&rx, id).0, Status::Stopped);
    let before: usize = site.hits.lock().iter().filter(|(k, _)| k.starts_with("/live/high/seg")).map(|(_, v)| v).sum();
    assert!(before > 0 && before < 20, "durdurmadan önce {before} segment");
    site.delay_ms.store(0, Ordering::Relaxed);
    engine.start(id);
    let (st, msg) = wait(&rx, id);
    assert_eq!(st, Status::Completed, "{msg}");
    assert_eq!(std::fs::read(engine.record(id).unwrap().target_path()).unwrap(), expected);
    // Her segment toplamda en çok iki kez istenmiş olmalı (durdurma anında yarım kalanlar)
    let hits = site.hits.lock();
    let total: usize = hits.iter().filter(|(k, _)| k.starts_with("/live/high/seg")).map(|(_, v)| v).sum();
    assert!(total <= 20 + 4, "toplam segment isteği {total}");
}

#[test]
fn media_variants_are_listed() {
    let site = Arc::new(Site::default());
    hls_site(&site);
    let base = serve(site);
    let (kind, list) = media::variants(&http::NetConfig::default(), &http::RequestSpec { url: format!("{base}/live/master.m3u8"), ..Default::default() })
        .unwrap()
        .unwrap();
    assert_eq!(kind, "hls");
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|v| v.height == 720));
}

#[test]
fn plain_file_is_not_a_stream() {
    let site = Arc::new(Site::default());
    site.files.lock().insert("/a.zip".into(), ("application/zip".into(), b"PK\x03\x04data".to_vec()));
    let base = serve(site);
    let r = media::variants(&http::NetConfig::default(), &http::RequestSpec { url: format!("{base}/a.zip"), ..Default::default() }).unwrap();
    assert!(r.is_none());
}

#[test]
fn completed_media_reports_full_size_as_done() {
    let site = Arc::new(Site::default());
    let expected = hls_site(&site);
    let base = serve(site);
    let (dir, engine, rx) = env("done");
    let id = engine
        .add(NewDownload { url: format!("{base}/live/master.m3u8"), media: Some(MediaSpec::default()), start: true, ..Default::default() }, dir.clone(), "video")
        .unwrap();
    assert_eq!(wait(&rx, id).0, Status::Completed);
    let info = engine.info(id).unwrap();
    assert_eq!(info.done, expected.len() as u64);
    assert_eq!(info.size, Some(expected.len() as u64));
    assert!(info.media && info.parts.is_none());
}
