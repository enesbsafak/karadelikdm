//! Yerel WebSocket sunucusu. Her bağlantıya bir iş parçacığı; uzantı tek ya da birkaç bağlantı açar.
//!
//! Savunma katmanları, sırasıyla:
//! - Yalnız `127.0.0.1`, rastgele port.
//! - El sıkışmada `Host` (DNS rebinding) ve `Origin` (sıradan web sayfaları) denetimi.
//! - İlk mesaj sabit zamanlı karşılaştırılan token taşıyan `hello`; değilse bağlantı kapanır.
//! - Mesaj boyutu sınırı, bağlantı başına hız sınırı, eşzamanlı bağlantı sınırı, boşta kalma süresi.

use crate::{write_bridge_file, BridgeInfo, CaptureConfig, Handler, Request, PROTOCOL_VERSION};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tungstenite::handshake::server::{ErrorResponse, Request as HsRequest, Response as HsResponse};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::frame::CloseFrame;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Message, WebSocket};

pub(crate) const MAX_MESSAGE: usize = 1 << 20;
const MAX_CONNECTIONS: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Uzantı 20 sn'de bir `ping` gönderir; üç tanesi kaçarsa bağlantı ölü sayılır.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Okuma bu aralıkla zaman aşımına uğrar; giden kuyruk ve durdurma isteği arada denetlenir.
const POLL: Duration = Duration::from_millis(100);
/// Hız sınırı: 20 mesajlık patlama, saniyede 10 mesaj. "Tüm bağlantıları indir" tek mesajdır,
/// meşru kullanım bunun çok altında kalır.
const RATE_BURST: f64 = 20.0;
const RATE_PER_SEC: f64 = 10.0;

pub struct Server {
    pub port: u16,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    bridge_file: PathBuf,
}

struct Shared {
    port: u16,
    token: String,
    origins: Vec<String>,
    handler: Arc<dyn Handler>,
    stop: AtomicBool,
    active: AtomicUsize,
    next_id: AtomicU64,
    /// Kimliği doğrulanmış bağlantıların giden kuyrukları (yapılandırma yayını için)
    clients: Mutex<HashMap<u64, Sender<String>>>,
    conns: Mutex<Vec<JoinHandle<()>>>,
}

/// Sunucudan uzantıya giden mesajlar.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Out {
    Hello { ok: bool, protocol: u32, app_version: &'static str, config: CaptureConfig },
    Config { config: CaptureConfig },
    Result {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Pong {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
}

impl Out {
    fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Sunucuyu başlatır ve `data_dir/bridge.json`'a `{port, token, pid}` yazar.
pub fn start(data_dir: &Path, allowed_origins: Vec<String>, handler: Arc<dyn Handler>) -> Result<Server> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("köprü soketi açılamadı")?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    let bridge_file =
        write_bridge_file(data_dir, &BridgeInfo { port, token: token.clone(), pid: std::process::id() })?;
    let shared = Arc::new(Shared {
        port,
        token,
        origins: allowed_origins,
        handler,
        stop: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        next_id: AtomicU64::new(1),
        clients: Mutex::new(HashMap::new()),
        conns: Mutex::new(Vec::new()),
    });
    let sh = shared.clone();
    let accept = std::thread::Builder::new().name("kdm-bridge".into()).spawn(move || accept_loop(listener, sh))?;
    Ok(Server { port, shared, accept: Some(accept), bridge_file })
}

impl Server {
    /// Ayarlar değişince çağrılır; bağlı uzantılar yeni yakalama yapılandırmasını hemen alır.
    pub fn broadcast_config(&self) {
        let msg = Out::Config { config: self.shared.handler.config() }.json();
        self.shared.clients.lock().unwrap().retain(|_, tx| tx.send(msg.clone()).is_ok());
    }

    /// Şu an bağlı ve kimliği doğrulanmış uzantı sayısı.
    pub fn client_count(&self) -> usize {
        self.shared.clients.lock().unwrap().len()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let Some(accept) = self.accept.take() else { return };
        self.shared.stop.store(true, Ordering::SeqCst);
        // Bloklayan accept()'i uyandır
        let _ = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)), Duration::from_secs(1));
        let _ = accept.join();
        let conns = std::mem::take(&mut *self.shared.conns.lock().unwrap());
        for c in conns {
            let _ = c.join();
        }
        // Yalnız kendi dosyamızı sil; bu arada başka bir örnek yazmış olabilir
        if crate::read_bridge_file(self.bridge_file.parent().unwrap_or(Path::new("."))).is_ok_and(|i| i.token == self.shared.token) {
            let _ = std::fs::remove_file(&self.bridge_file);
        }
    }

    #[cfg(test)]
    pub(crate) fn token(&self) -> &str {
        &self.shared.token
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Sabit zamanlı karşılaştırma: ilk farklı baytta erken dönmez, böylece süre ölçülerek token tahmin edilemez.
/// Uzunluk gizli değil (her zaman 64).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// `*` ile biten giriş önek eşleşmesidir (`moz-extension://*`). Sondaki `/` yok sayılır.
pub(crate) fn origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    let Some(origin) = origin else { return false };
    let origin = origin.trim_end_matches('/');
    allowed.iter().any(|a| match a.strip_suffix('*') {
        Some(prefix) => origin.len() > prefix.len() && origin.starts_with(prefix),
        None => origin.eq_ignore_ascii_case(a.trim_end_matches('/')),
    })
}

/// DNS rebinding'e karşı: tarayıcı `Host`'u adres çubuğundaki ada göre koyar.
pub(crate) fn host_allowed(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    host == format!("127.0.0.1:{port}") || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
}

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(4 * MAX_MESSAGE)
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        if shared.active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            log::warn!("köprü: bağlantı sınırı aşıldı");
            continue;
        }
        shared.active.fetch_add(1, Ordering::SeqCst);
        let sh = shared.clone();
        let spawned = std::thread::Builder::new().name("kdm-bridge-conn".into()).spawn(move || {
            let _guard = ActiveGuard(&sh.active);
            if let Err(e) = serve(&sh, stream) {
                log::debug!("köprü bağlantısı kapandı: {e:#}");
            }
        });
        match spawned {
            Ok(h) => {
                let mut conns = shared.conns.lock().unwrap();
                conns.retain(|c| !c.is_finished());
                conns.push(h);
            }
            Err(_) => {
                shared.active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

struct ActiveGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn forbidden(why: &str) -> ErrorResponse {
    let mut r = ErrorResponse::new(Some(why.to_string()));
    *r.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
    r
}

enum Event {
    Text(String),
    /// Okuma zaman aşımı; kuyruk ve durdurma denetlenir
    Tick,
    Closed,
}

fn read_event(ws: &mut WebSocket<TcpStream>) -> Event {
    match ws.read() {
        Ok(Message::Text(t)) => Event::Text(t.as_str().to_owned()),
        // Ping'e tungstenite kendisi yanıt verir; ikili veri protokolde yok
        Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Event::Tick,
        Ok(Message::Binary(_) | Message::Close(_)) => Event::Closed,
        Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Event::Tick,
        Err(e) => {
            log::debug!("köprü okuma: {e}");
            Event::Closed
        }
    }
}

fn close(ws: &mut WebSocket<TcpStream>, code: CloseCode, reason: &'static str) {
    let _ = ws.close(Some(CloseFrame { code, reason: reason.into() }));
    // Kapanış çerçevesini gönder; karşı tarafın yanıtını kısa süre bekle
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match ws.read() {
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
            Ok(_) => {}
        }
    }
}

fn serve(shared: &Arc<Shared>, stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let _ = stream.set_nodelay(true);
    let port = shared.port;
    let origins = shared.origins.clone();
    // Hata türü tungstenite'in `Callback` imzasından gelir
    #[allow(clippy::result_large_err)]
    let check = move |req: &HsRequest, resp: HsResponse| {
        let header = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok());
        if !host_allowed(header("host"), port) {
            log::warn!("köprü: geçersiz Host reddedildi: {:?}", header("host"));
            return Err(forbidden("host"));
        }
        if !origin_allowed(header("origin"), &origins) {
            log::warn!("köprü: izinsiz Origin reddedildi: {:?}", header("origin"));
            return Err(forbidden("origin"));
        }
        Ok(resp)
    };
    let mut ws = tungstenite::accept_hdr_with_config(stream, check, Some(ws_config()))
        .map_err(|e| anyhow::anyhow!("el sıkışma: {e}"))?;
    ws.get_ref().set_read_timeout(Some(POLL))?;

    // 1. aşama: hello
    let deadline = Instant::now() + HELLO_TIMEOUT;
    let first = loop {
        match read_event(&mut ws) {
            Event::Text(t) => break t,
            Event::Tick if Instant::now() < deadline && !shared.stop.load(Ordering::SeqCst) => {}
            Event::Tick | Event::Closed => {
                close(&mut ws, CloseCode::Policy, "hello timeout");
                anyhow::bail!("hello gelmedi");
            }
        }
    };
    let Ok((_, Request::Hello { token, version })) = parse(&first) else {
        close(&mut ws, CloseCode::Policy, "hello expected");
        anyhow::bail!("ilk mesaj hello değil");
    };
    if !ct_eq(token.as_bytes(), shared.token.as_bytes()) {
        log::warn!("köprü: yanlış token");
        close(&mut ws, CloseCode::Policy, "unauthorized");
        anyhow::bail!("yanlış token");
    }
    log::info!("köprü: uzantı bağlandı (sürüm {version})");
    // Yayın listesine hello yanıtından önce girilir: araya giren bir `broadcast_config` kaybolmasın
    // (en kötü ihtimalle aynı yapılandırma iki kez gelir)
    let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    shared.clients.lock().unwrap().insert(id, tx);
    let hello = Out::Hello {
        ok: true,
        protocol: PROTOCOL_VERSION,
        app_version: env!("CARGO_PKG_VERSION"),
        config: shared.handler.config(),
    };
    let r = ws
        .send(Message::text(hello.json()))
        .map_err(anyhow::Error::from)
        .and_then(|_| session(shared, &mut ws, rx));
    shared.clients.lock().unwrap().remove(&id);
    r
}

fn session(shared: &Arc<Shared>, ws: &mut WebSocket<TcpStream>, rx: Receiver<String>) -> Result<()> {
    let mut bucket = (RATE_BURST, Instant::now());
    let mut last = Instant::now();
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            close(ws, CloseCode::Away, "shutdown");
            return Ok(());
        }
        loop {
            match rx.try_recv() {
                Ok(msg) => ws.send(Message::text(msg))?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        let text = match read_event(ws) {
            Event::Text(t) => t,
            Event::Tick if last.elapsed() < IDLE_TIMEOUT => continue,
            Event::Tick => {
                close(ws, CloseCode::Normal, "idle");
                return Ok(());
            }
            Event::Closed => return Ok(()),
        };
        last = Instant::now();

        // Basit kova: bitince mesaj işlenmez, uzantı hata alır
        let now = Instant::now();
        bucket.0 = (bucket.0 + now.duration_since(bucket.1).as_secs_f64() * RATE_PER_SEC).min(RATE_BURST);
        bucket.1 = now;
        let reply = if bucket.0 < 1.0 {
            Out::Result { id: peek_id(&text), ok: false, error: Some("rate_limited".into()) }
        } else {
            bucket.0 -= 1.0;
            dispatch(shared, &text)
        };
        ws.send(Message::text(reply.json()))?;
    }
}

fn peek_id(text: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(text).ok()?.get("id")?.as_u64()
}

/// `id` alanını ayırır; geri kalan `Request` olarak çözülür.
fn parse(text: &str) -> std::result::Result<(Option<u64>, Request), (Option<u64>, String)> {
    let mut v: serde_json::Value = serde_json::from_str(text).map_err(|e| (None, e.to_string()))?;
    let id = v.as_object_mut().and_then(|o| o.remove("id")).and_then(|i| i.as_u64());
    serde_json::from_value(v).map(|r| (id, r)).map_err(|e| (id, e.to_string()))
}

fn dispatch(shared: &Arc<Shared>, text: &str) -> Out {
    let (id, req) = match parse(text) {
        Ok(x) => x,
        Err((id, e)) => return Out::Result { id, ok: false, error: Some(format!("bad_request: {e}")) },
    };
    match req {
        Request::Ping => Out::Pong { id },
        Request::Hello { .. } => Out::Result { id, ok: false, error: Some("already_authenticated".into()) },
        req => {
            let handler = shared.handler.clone();
            // Uygulama tarafındaki bir panik yalnız bu isteği düşürsün, bağlantıyı değil
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.handle(req))) {
                Ok(Ok(())) => Out::Result { id, ok: true, error: None },
                Ok(Err(e)) => Out::Result { id, ok: false, error: Some(e) },
                Err(_) => Out::Result { id, ok: false, error: Some("internal_error".into()) },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::temp_dir;
    use tungstenite::client::IntoClientRequest;

    #[derive(Default)]
    struct Recorder {
        got: Mutex<Vec<Request>>,
    }

    impl Handler for Recorder {
        fn config(&self) -> CaptureConfig {
            CaptureConfig { enabled: true, exts: vec!["zip".into(), "iso".into()], min_size_kb: 100 }
        }
        fn handle(&self, req: Request) -> std::result::Result<(), String> {
            if let Request::Download { url, .. } = &req {
                if url.contains("fail") {
                    return Err("nope".into());
                }
            }
            self.got.lock().unwrap().push(req);
            Ok(())
        }
    }

    const ORIGIN: &str = "chrome-extension://kkifckmmeinedmmdokgacefiojpfbpfl";

    fn setup() -> (Server, Arc<Recorder>, PathBuf) {
        let dir = temp_dir("srv");
        let rec = Arc::new(Recorder::default());
        let srv = start(&dir, crate::default_allowed_origins(), rec.clone()).unwrap();
        (srv, rec, dir)
    }

    type Client = WebSocket<TcpStream>;

    fn connect(port: u16, origin: Option<&str>) -> std::result::Result<Client, tungstenite::Error> {
        let mut req = format!("ws://127.0.0.1:{port}").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("Origin", o.parse().unwrap());
        }
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        match tungstenite::client(req, stream) {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::HandshakeError::Failure(e)) => Err(e),
            Err(tungstenite::HandshakeError::Interrupted(_)) => panic!("el sıkışma yarıda kaldı"),
        }
    }

    fn send(ws: &mut Client, v: serde_json::Value) {
        ws.send(Message::text(v.to_string())).unwrap();
    }

    fn recv(ws: &mut Client) -> serde_json::Value {
        loop {
            match ws.read().expect("yanıt bekleniyordu") {
                Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("beklenmeyen mesaj: {other:?}"),
            }
        }
    }

    /// Sunucu bağlantıyı kapatmış olmalı: Close çerçevesi ya da bağlantı hatası.
    fn assert_closed(ws: &mut Client) -> Option<CloseCode> {
        loop {
            match ws.read() {
                Ok(Message::Close(f)) => return f.map(|f| f.code),
                Ok(Message::Text(t)) => panic!("kapanış yerine mesaj geldi: {t}"),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    fn hello(ws: &mut Client, token: &str) {
        send(ws, serde_json::json!({"type": "hello", "token": token, "version": "test"}));
    }

    fn authed(srv: &Server) -> Client {
        let mut ws = connect(srv.port, Some(ORIGIN)).unwrap();
        hello(&mut ws, srv.token());
        let r = recv(&mut ws);
        assert_eq!(r["type"], "hello");
        ws
    }

    #[test]
    fn writes_bridge_file_and_removes_on_stop() {
        let (srv, _, dir) = setup();
        let info = crate::read_bridge_file(&dir).unwrap();
        assert_eq!(info.port, srv.port);
        assert_eq!(info.token, srv.token());
        assert_eq!(info.token.len(), 64);
        assert_eq!(info.pid, std::process::id());
        srv.stop();
        assert!(!crate::bridge_path(&dir).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn random_port_on_loopback() {
        let (srv, _, _) = setup();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, srv.port));
        assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
        let (srv2, _, _) = setup();
        assert_ne!(srv.port, srv2.port);
    }

    #[test]
    fn correct_token_and_origin_accepted_with_config() {
        let (srv, _, _) = setup();
        let mut ws = connect(srv.port, Some(ORIGIN)).unwrap();
        hello(&mut ws, srv.token());
        let r = recv(&mut ws);
        assert_eq!(r["type"], "hello");
        assert_eq!(r["ok"], true);
        assert_eq!(r["protocol"], PROTOCOL_VERSION);
        assert_eq!(r["config"]["enabled"], true);
        assert_eq!(r["config"]["exts"], serde_json::json!(["zip", "iso"]));
        assert_eq!(r["config"]["min_size_kb"], 100);
        send(&mut ws, serde_json::json!({"type": "ping", "id": 9}));
        let r = recv(&mut ws);
        assert_eq!(r, serde_json::json!({"type": "pong", "id": 9}));
    }

    #[test]
    fn firefox_origin_accepted() {
        let (srv, _, _) = setup();
        let mut ws = connect(srv.port, Some("moz-extension://3f1c2a9e-1111-4b4b-9c9c-0123456789ab")).unwrap();
        hello(&mut ws, srv.token());
        assert_eq!(recv(&mut ws)["ok"], true);
    }

    #[test]
    fn wrong_token_rejected() {
        let (srv, rec, _) = setup();
        let mut ws = connect(srv.port, Some(ORIGIN)).unwrap();
        let mut bad = srv.token().to_string();
        bad.replace_range(0..1, if bad.starts_with('0') { "1" } else { "0" });
        hello(&mut ws, &bad);
        assert_eq!(assert_closed(&mut ws), Some(CloseCode::Policy));
        // Kapatılmış bağlantıdan indirme geçmemeli
        let _ = ws.send(Message::text(r#"{"type":"download","url":"https://x/y.zip"}"#));
        assert!(rec.got.lock().unwrap().is_empty());

        // Token'sız ilk mesaj da reddedilir
        let mut ws = connect(srv.port, Some(ORIGIN)).unwrap();
        send(&mut ws, serde_json::json!({"type": "download", "url": "https://x/y.zip"}));
        assert_eq!(assert_closed(&mut ws), Some(CloseCode::Policy));
        assert!(rec.got.lock().unwrap().is_empty());
    }

    #[test]
    fn wrong_or_missing_origin_rejected() {
        let (srv, _, _) = setup();
        for origin in [None, Some("https://evil.example"), Some("chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), Some("null")] {
            match connect(srv.port, origin) {
                Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 403, "{origin:?}"),
                Err(e) => panic!("{origin:?}: beklenmeyen hata {e}"),
                Ok(_) => panic!("{origin:?} kabul edilmemeliydi"),
            }
        }
    }

    #[test]
    fn foreign_host_rejected() {
        // DNS rebinding: sayfa kendi alan adını 127.0.0.1'e çözdürse de Host başlığı onu ele verir
        let (srv, _, _) = setup();
        let mut req = format!("ws://rebind.example:{}", srv.port).into_client_request().unwrap();
        req.headers_mut().insert("Origin", ORIGIN.parse().unwrap());
        let stream = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
        match tungstenite::client(req, stream) {
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(resp))) => assert_eq!(resp.status(), 403),
            Err(e) => panic!("beklenmeyen hata: {e}"),
            Ok(_) => panic!("yabancı Host kabul edilmemeliydi"),
        }
    }

    #[test]
    fn oversized_message_rejected() {
        let (srv, rec, _) = setup();
        let mut ws = authed(&srv);
        let big = format!(r#"{{"type":"download","url":"https://x/{}"}}"#, "a".repeat(MAX_MESSAGE + 10));
        // Sunucu okurken kapatabilir; gönderim hatası da kabul
        let _ = ws.send(Message::text(big));
        assert!(matches!(assert_closed(&mut ws), None | Some(CloseCode::Size)));
        assert!(rec.got.lock().unwrap().is_empty());
    }

    #[test]
    fn download_reaches_handler() {
        let (srv, rec, _) = setup();
        let mut ws = authed(&srv);
        send(
            &mut ws,
            serde_json::json!({
                "type": "download", "id": 1, "url": "https://example.com/a.zip", "referer": "https://example.com/",
                "cookies": "a=1; b=2", "user_agent": "UA", "file_name": "a.zip", "size": 123456,
                "mime": "application/zip", "page_url": "https://example.com/"
            }),
        );
        assert_eq!(recv(&mut ws), serde_json::json!({"type": "result", "id": 1, "ok": true}));
        let got = rec.got.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![Request::Download {
                url: "https://example.com/a.zip".into(),
                referer: "https://example.com/".into(),
                cookies: "a=1; b=2".into(),
                user_agent: "UA".into(),
                file_name: "a.zip".into(),
                size: Some(123456),
                mime: "application/zip".into(),
                page_url: "https://example.com/".into(),
            }]
        );
    }

    #[test]
    fn handler_errors_and_bad_requests_reported() {
        let (srv, _, _) = setup();
        let mut ws = authed(&srv);
        send(&mut ws, serde_json::json!({"type": "download", "id": 2, "url": "https://x/fail.zip"}));
        assert_eq!(recv(&mut ws), serde_json::json!({"type": "result", "id": 2, "ok": false, "error": "nope"}));
        send(&mut ws, serde_json::json!({"type": "explode", "id": 3}));
        let r = recv(&mut ws);
        assert_eq!((r["id"].as_u64(), r["ok"].as_bool()), (Some(3), Some(false)));
        send(&mut ws, serde_json::json!({"type": "hello", "id": 4, "token": srv.token()}));
        assert_eq!(recv(&mut ws)["error"], "already_authenticated");
    }

    #[test]
    fn links_and_media_reach_handler() {
        let (srv, rec, _) = setup();
        let mut ws = authed(&srv);
        send(&mut ws, serde_json::json!({"type": "links", "id": 1, "urls": ["https://a/1", "https://a/2"]}));
        assert_eq!(recv(&mut ws)["ok"], true);
        send(&mut ws, serde_json::json!({"type": "media", "id": 2, "url": "https://a/m.m3u8", "kind": "hls", "title": "T"}));
        assert_eq!(recv(&mut ws)["ok"], true);
        let got = rec.got.lock().unwrap();
        assert!(matches!(&got[0], Request::Links { urls, .. } if urls.len() == 2));
        assert!(matches!(&got[1], Request::Media { kind, title, .. } if kind == "hls" && title == "T"));
    }

    #[test]
    fn rate_limited() {
        let (srv, rec, _) = setup();
        let mut ws = authed(&srv);
        let n = 60;
        for i in 0..n {
            send(&mut ws, serde_json::json!({"type": "links", "id": i, "urls": ["https://a/1"]}));
        }
        let limited = (0..n).filter(|_| recv(&mut ws)["error"] == "rate_limited").count();
        assert!(limited > 0, "hız sınırı devreye girmedi");
        assert!(rec.got.lock().unwrap().len() < n as usize);
    }

    #[test]
    fn broadcast_config_reaches_clients() {
        let (srv, _, _) = setup();
        let mut ws = authed(&srv);
        srv.broadcast_config();
        let r = recv(&mut ws);
        assert_eq!(r["type"], "config");
        assert_eq!(r["config"]["min_size_kb"], 100);
    }

    #[test]
    fn client_count_follows_connections() {
        let (srv, _, _) = setup();
        assert_eq!(srv.client_count(), 0);
        let ws = authed(&srv);
        assert_eq!(srv.client_count(), 1);
        drop(ws);
        // Kopan bağlantı okuma döngüsü bitince listeden çıkar
        let t = std::time::Instant::now();
        while srv.client_count() != 0 && t.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(srv.client_count(), 0);
    }

    #[test]
    fn stop_closes_clients() {
        let (srv, _, _) = setup();
        let mut ws = authed(&srv);
        srv.stop();
        assert!(matches!(assert_closed(&mut ws), None | Some(CloseCode::Away)));
    }

    #[test]
    fn origin_and_host_rules() {
        let allowed = crate::default_allowed_origins();
        assert!(origin_allowed(Some(ORIGIN), &allowed));
        assert!(origin_allowed(Some(&format!("{ORIGIN}/")), &allowed));
        assert!(origin_allowed(Some("moz-extension://abc"), &allowed));
        assert!(!origin_allowed(Some("moz-extension://"), &allowed));
        assert!(!origin_allowed(Some("https://kkifckmmeinedmmdokgacefiojpfbpfl"), &allowed));
        assert!(!origin_allowed(Some("chrome-extension://kkifckmmeinedmmdokgacefiojpfbpflx"), &allowed));
        assert!(!origin_allowed(None, &allowed));
        assert!(host_allowed(Some("127.0.0.1:80"), 80));
        assert!(host_allowed(Some("localhost:80"), 80));
        assert!(!host_allowed(Some("evil.example:80"), 80));
        assert!(!host_allowed(Some("127.0.0.1:81"), 80));
        assert!(!host_allowed(None, 80));
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }
}
