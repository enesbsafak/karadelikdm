//! Native messaging host. Tek işi keşif: uzantıya köprünün `{port, token}` bilgisini verir; istenirse ve
//! uygulama kapalıysa yanındaki `karadelikdm`'yi başlatıp köprü açılana kadar bekler.
//!
//! İndirme verisi bu süreçten geçmez (veri kanalı WebSocket'tir); host kısa ömürlüdür ve durumsuzdur.
//!
//! Çerçeveleme (Chrome/Firefox native messaging): 4 bayt uzunluk + UTF-8 JSON. Uzunluk tarayıcının yerel
//! bayt sırasındadır; desteklenen tüm hedefler (x86_64, aarch64) little-endian olduğundan LE yazılır.
//! stdout yalnız protokol içindir; tanılama stderr'e gider (tarayıcı günlüğüne düşer).
//!
//! İstek:  `{"type":"discover","launch":true}`
//! Yanıt:  `{"ok":true,"port":49152,"token":"…"}` ya da `{"ok":false,"error":"not_running|launch_failed|timeout|bad_request"}`

use anyhow::{bail, Context, Result};
use kdm_bridge::BridgeInfo;
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Uzantıdan gelen mesajlar birkaç baytlıktır; büyük uzunluk bozuk ya da kötü niyetli girdi demektir.
const MAX_IN: usize = 64 * 1024;
/// Chrome'un host'tan kabul ettiği en büyük mesaj.
const MAX_OUT: usize = 1024 * 1024;
const LAUNCH_WAIT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Bir mesaj okur. `Ok(None)`: tarayıcı bağlantıyı düzgün kapattı (mesaj sınırında EOF).
fn read_message(r: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => bail!("uzunluk öneki yarım kaldı"),
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_IN {
        bail!("mesaj çok büyük: {n} bayt");
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).context("mesaj gövdesi yarım kaldı")?;
    Ok(Some(body))
}

fn write_message(w: &mut impl Write, v: &Value) -> Result<()> {
    let body = serde_json::to_vec(v)?;
    if body.len() > MAX_OUT {
        bail!("yanıt çok büyük: {} bayt", body.len());
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

/// `bridge.json` okunabiliyor ve porta bağlanılabiliyorsa uygulama çalışıyordur. Çöken uygulamanın
/// geride bıraktığı eski dosya bağlantı denemesinde elenir.
fn live_bridge(data_dir: &Path) -> Option<BridgeInfo> {
    let info = kdm_bridge::read_bridge_file(data_dir).ok()?;
    TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, info.port)), PROBE_TIMEOUT).ok()?;
    Some(info)
}

fn found(info: BridgeInfo) -> Value {
    json!({"ok": true, "port": info.port, "token": info.token})
}

fn failure(error: &str) -> Value {
    json!({"ok": false, "error": error})
}

fn discover(data_dir: &Path, launch: bool, start_app: impl FnOnce() -> Result<()>, wait: Duration) -> Value {
    if let Some(info) = live_bridge(data_dir) {
        return found(info);
    }
    // Arka plandaki yeniden bağlanma döngüsü `launch:false` gönderir: kullanıcı uygulamayı kapattıysa
    // uzantı onu kendiliğinden geri açmasın. Yalnız kullanıcı eylemi (indirme, menü) başlatır.
    if !launch {
        return failure("not_running");
    }
    if let Err(e) = start_app() {
        eprintln!("karadelikdm-nmh: uygulama başlatılamadı: {e:#}");
        return failure("launch_failed");
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(info) = live_bridge(data_dir) {
            return found(info);
        }
    }
    failure("timeout")
}

fn handle(msg: &[u8], data_dir: &Path, start_app: impl FnOnce() -> Result<()>, wait: Duration) -> Value {
    let Ok(v) = serde_json::from_slice::<Value>(msg) else { return failure("bad_request") };
    match v.get("type").and_then(Value::as_str) {
        Some("discover") => {
            let launch = v.get("launch").and_then(Value::as_bool).unwrap_or(false);
            discover(data_dir, launch, start_app, wait)
        }
        _ => failure("bad_request"),
    }
}

/// Uygulamanınkiyle aynı kural (`src-tauri/src/paths.rs`): `KDM_DATA`, yoksa `<veri>/KaradelikDM`.
fn data_dir() -> PathBuf {
    std::env::var_os("KDM_DATA")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("KaradelikDM")))
        .unwrap_or_else(|| std::env::temp_dir().join("KaradelikDM"))
}

fn app_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("host klasörü bulunamadı")?;
    Ok(dir.join(if cfg!(windows) { "karadelikdm.exe" } else { "karadelikdm" }))
}

/// Uygulamayı host'tan bağımsız başlatır. Tarayıcı host'u kapatırken alt süreçleri de öldürür
/// (Windows'ta job nesnesi, Unix'te süreç grubu); bu yüzden Windows'ta job'dan ayrılma, Unix'te yeni grup.
/// Standart akışlar kapatılır: alt süreç protokol borusunu miras alıp bozmamalı.
fn launch_app() -> Result<()> {
    use std::process::{Command, Stdio};
    let app = app_path()?;
    if !app.exists() {
        bail!("{} yok", app.display());
    }
    let cmd = || {
        let mut c = Command::new(&app);
        c.arg("--minimized").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(dir) = app.parent() {
            c.current_dir(dir);
        }
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // Job ayrılmaya izin vermiyorsa erişim hatası döner; o durumda ayrılmadan dene
        if cmd().creation_flags(base | CREATE_BREAKAWAY_FROM_JOB).spawn().is_ok() {
            return Ok(());
        }
        cmd().creation_flags(base).spawn().context("başlatılamadı")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd().process_group(0).spawn().context("başlatılamadı")?;
    }
    Ok(())
}

fn main() {
    let dir = data_dir();
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    loop {
        let msg = match read_message(&mut stdin) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                // Çerçeve bozuldu; akış senkronunu yitirdik, devam etmek anlamsız
                eprintln!("karadelikdm-nmh: {e:#}");
                std::process::exit(1);
            }
        };
        let reply = handle(&msg, &dir, launch_app, LAUNCH_WAIT);
        if let Err(e) = write_message(&mut stdout, &reply) {
            eprintln!("karadelikdm-nmh: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kdm-nmh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn length_prefix_is_little_endian() {
        let mut out = Vec::new();
        write_message(&mut out, &json!({"ok": true})).unwrap();
        let body = br#"{"ok":true}"#;
        assert_eq!(&out[..4], &(body.len() as u32).to_le_bytes());
        assert_eq!(&out[..4], &[11, 0, 0, 0]);
        assert_eq!(&out[4..], body);
    }

    #[test]
    fn roundtrip_several_messages() {
        let mut input = frame(br#"{"type":"discover"}"#);
        input.extend(frame(b"{}"));
        let mut r = Cursor::new(input);
        assert_eq!(read_message(&mut r).unwrap().unwrap(), br#"{"type":"discover"}"#);
        assert_eq!(read_message(&mut r).unwrap().unwrap(), b"{}");
        assert!(read_message(&mut r).unwrap().is_none());
    }

    #[test]
    fn empty_input_is_clean_eof() {
        assert!(read_message(&mut Cursor::new(Vec::new())).unwrap().is_none());
    }

    #[test]
    fn truncated_prefix_is_error() {
        assert!(read_message(&mut Cursor::new(vec![5, 0])).is_err());
    }

    #[test]
    fn truncated_body_is_error() {
        let mut v = frame(b"{\"type\":\"discover\"}");
        v.truncate(v.len() - 3);
        assert!(read_message(&mut Cursor::new(v)).is_err());
    }

    #[test]
    fn oversized_length_is_error_without_allocating() {
        let v = u32::MAX.to_le_bytes().to_vec();
        assert!(read_message(&mut Cursor::new(v)).is_err());
    }

    #[test]
    fn oversized_reply_is_refused() {
        let mut out = Vec::new();
        assert!(write_message(&mut out, &json!({"x": "a".repeat(MAX_OUT)})).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn malformed_json_gets_error_reply() {
        let dir = temp_dir("bad");
        let never = || -> Result<()> { panic!("başlatılmamalı") };
        assert_eq!(handle(b"{not json", &dir, never, Duration::ZERO), failure("bad_request"));
        assert_eq!(handle(br#"{"type":"download"}"#, &dir, never, Duration::ZERO), failure("bad_request"));
    }

    #[test]
    fn discover_running_app() {
        let dir = temp_dir("live");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        kdm_bridge::write_bridge_file(&dir, &BridgeInfo { port, token: "t".repeat(64), pid: 1 }).unwrap();
        let r = handle(br#"{"type":"discover","launch":false}"#, &dir, || panic!("başlatılmamalı"), Duration::ZERO);
        assert_eq!(r, json!({"ok": true, "port": port, "token": "t".repeat(64)}));
        // pid uzantıya gitmez
        assert!(r.get("pid").is_none());
    }

    #[test]
    fn stale_bridge_file_is_not_running() {
        let dir = temp_dir("stale");
        // Kapatılmış bir dinleyicinin portu: bağlantı reddedilir
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        kdm_bridge::write_bridge_file(&dir, &BridgeInfo { port, token: "x".into(), pid: 1 }).unwrap();
        let r = handle(br#"{"type":"discover"}"#, &dir, || panic!("launch:false başlatmamalı"), Duration::ZERO);
        assert_eq!(r, failure("not_running"));
    }

    #[test]
    fn launch_waits_for_bridge() {
        let dir = temp_dir("launch");
        let d2 = dir.clone();
        // "Uygulama" 300 ms sonra köprüyü açar
        let start = move || -> Result<()> {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
                let port = l.local_addr().unwrap().port();
                kdm_bridge::write_bridge_file(&d2, &BridgeInfo { port, token: "k".into(), pid: 2 }).unwrap();
                std::thread::sleep(Duration::from_secs(3));
                drop(l);
            });
            Ok(())
        };
        let r = handle(br#"{"type":"discover","launch":true}"#, &dir, start, Duration::from_secs(5));
        assert_eq!(r["ok"], true);
        assert_eq!(r["token"], "k");
    }

    #[test]
    fn launch_failure_and_timeout() {
        let dir = temp_dir("fail");
        let r = handle(br#"{"type":"discover","launch":true}"#, &dir, || bail!("yok"), Duration::from_secs(1));
        assert_eq!(r, failure("launch_failed"));
        let r = handle(br#"{"type":"discover","launch":true}"#, &dir, || Ok(()), Duration::from_millis(300));
        assert_eq!(r, failure("timeout"));
    }
}
