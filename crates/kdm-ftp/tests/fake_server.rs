//! Aynı süreçte çalışan sahte FTP sunucusuyla uçtan uca testler (ağ erişimi gerekmez).
//! TLS testleri `rcgen` ile `localhost` için üretilen kendinden imzalı sertifikayı kullanır.

use kdm_ftp::{list, open, stat, Entry, FtpError, FtpOptions, TlsMode};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::ServerSessionMemoryCache;
use rustls::{HandshakeKind, ServerConfig, ServerConnection, SupportedProtocolVersion};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum Pasv {
    /// `227 Entering Passive Mode (a,b,c,d,p1,p2)` bildirilen IP ile
    Paren([u8; 4]),
    /// `227 =a,b,c,d,p1,p2`
    Equals([u8; 4]),
}

#[derive(Clone)]
struct Cfg {
    greeting: Vec<String>,
    user: String,
    pass: String,
    anonymous: bool,
    files: HashMap<String, Vec<u8>>,
    epsv: bool,
    pasv: Pasv,
    rest: bool,
    mdtm: Option<String>,
    mlsd: Option<String>,
    list: String,
    /// RETR'de bu kadar bayttan sonra iki bağlantıyı da kopar
    cut_after: Option<usize>,
    /// Veri bitince kontrol bağlantısında yanıt verme
    silent_after_data: bool,
    /// TLS sertifikası ve ayarları; `None` ise AUTH TLS bilinmez
    tls: Option<Arc<ServerConfig>>,
    /// Veri bağlantıları için ayrı yapılandırma (oturum deposu farklıysa devam ettirme imkânsızdır)
    data_tls: Option<Arc<ServerConfig>>,
    /// Örtük TLS: karşılamadan önce el sıkışma
    implicit: bool,
    auth_tls: bool,
    /// AUTH TLS yanıtı (araya düz metin sokma testi için değiştirilebilir)
    auth_reply: String,
    /// vsftpd `require_ssl_reuse`: veri TLS'i kontrol oturumunu devam ettirmiyorsa 522
    require_reuse: bool,
    /// Veri bitince TLS close_notify gönder
    data_close_notify: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            greeting: vec!["220 sahte sunucu hazır".into()],
            user: "bob".into(),
            pass: "secret".into(),
            anonymous: true,
            files: HashMap::new(),
            epsv: true,
            pasv: Pasv::Paren([127, 0, 0, 1]),
            rest: true,
            mdtm: None,
            mlsd: None,
            list: String::new(),
            cut_after: None,
            silent_after_data: false,
            tls: None,
            data_tls: None,
            implicit: false,
            auth_tls: true,
            auth_reply: "234 Proceed with negotiation.".into(),
            require_reuse: false,
            data_close_notify: true,
        }
    }
}

/// Sunucu tarafı bağlantı: düz TCP ya da üstünde rustls; AUTH TLS'te yerinde açılır.
struct Conn {
    tcp: TcpStream,
    tls: Option<ServerConnection>,
}

impl Conn {
    fn plain(tcp: TcpStream) -> Self {
        Self { tcp, tls: None }
    }

    fn start_tls(&mut self, cfg: &Arc<ServerConfig>) -> bool {
        let mut c = ServerConnection::new(cfg.clone()).unwrap();
        while c.is_handshaking() {
            if c.complete_io(&mut self.tcp).is_err() {
                return false;
            }
        }
        self.tls = Some(c);
        true
    }

    fn resumed(&self) -> bool {
        self.tls.as_ref().and_then(|c| c.handshake_kind()) == Some(HandshakeKind::Resumed)
    }

    fn close_notify(&mut self) {
        if let Some(c) = self.tls.as_mut() {
            c.send_close_notify();
            let _ = c.complete_io(&mut self.tcp);
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.tls.as_mut() {
            None => self.tcp.read(buf),
            Some(c) => rustls::Stream::new(c, &mut self.tcp).read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.tls.as_mut() {
            None => self.tcp.write(buf),
            Some(c) => rustls::Stream::new(c, &mut self.tcp).write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.tls.as_mut() {
            None => self.tcp.flush(),
            Some(c) => rustls::Stream::new(c, &mut self.tcp).flush(),
        }
    }
}

/// `localhost` için kendinden imzalı sertifikalı sunucu yapılandırması ve istemcinin güveneceği kök (DER).
fn tls_server(versions: &[&'static SupportedProtocolVersion]) -> (Arc<ServerConfig>, Vec<u8>) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
    let cfg = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();
    // Varsayılanlar: oturum kimliği deposu (TLS 1.2) ve durumlu TLS 1.3 biletleri; devam ettirme açık
    (Arc::new(cfg), cert.to_vec())
}

struct Server {
    addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("ftp://{}{}", self.addr, path)
    }

    /// Sertifikadaki ad: istemci yine kontrol bağlantısının IP'sine (127.0.0.1) bağlanır
    fn url_as(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://localhost:{}{}", self.addr.port(), path)
    }

    fn commands(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Sunucu iş parçacığı komutu kaydedene dek bekler.
    fn wait_for(&self, verb: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if self.commands().iter().any(|c| c.starts_with(verb)) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

fn start(cfg: Cfg) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let cfg = Arc::new(cfg);
    // `localhost` çoğu sistemde önce ::1'e çözülür; aynı portu orada da dinlemek, Windows'ta kapalı porta
    // bağlanmanın ~1 sn'lik bekleyişini önler. IPv6 yoksa yalnız IPv4.
    let mut listeners = vec![l];
    listeners.extend(TcpListener::bind(("::1", addr.port())).ok());
    for l in listeners {
        let (cfg, log2) = (cfg.clone(), log.clone());
        thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (cfg, log) = (cfg.clone(), log2.clone());
                thread::spawn(move || session(s, &cfg, &log));
            }
        });
    }
    Server { addr, log }
}

fn accept(l: &TcpListener) -> Option<TcpStream> {
    l.set_nonblocking(true).ok()?;
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Ok((s, _)) = l.accept() {
            s.set_nonblocking(false).ok()?;
            return Some(s);
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn session(s: TcpStream, cfg: &Cfg, log: &Mutex<Vec<String>>) {
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    // Veri portu kontrol bağlantısının geldiği adreste açılır (istemci oraya bağlanır)
    let local = s.local_addr().unwrap().ip();
    let mut r = BufReader::new(Conn::plain(s));
    macro_rules! say {
        ($($t:tt)*) => {{
            let w = r.get_mut();
            let _ = write!(w, $($t)*);
            let _ = w.write_all(b"\r\n");
            let _ = w.flush();
        }};
    }
    let note = |m: &str| log.lock().unwrap().push(m.to_string());
    if cfg.implicit && !r.get_mut().start_tls(cfg.tls.as_ref().unwrap()) {
        note("TLS-FAIL");
        return;
    }
    let data_cfg = cfg.data_tls.clone().or_else(|| cfg.tls.clone());
    let mut prot_p = false;
    for g in &cfg.greeting {
        say!("{g}");
    }
    let mut user = String::new();
    let mut logged = false;
    let mut rest = 0usize;
    let mut data: Option<TcpListener> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end().to_string();
        log.lock().unwrap().push(line.clone());
        let (verb, arg) = line.split_once(' ').unwrap_or((&line, ""));
        let verb = verb.to_ascii_uppercase();
        if !logged && !matches!(verb.as_str(), "USER" | "PASS" | "QUIT" | "AUTH") {
            say!("530 Please login with USER and PASS.");
            continue;
        }
        match verb.as_str() {
            "USER" => {
                user = arg.to_string();
                say!("331 Please specify the password.");
            }
            "PASS" => {
                if (user == cfg.user && arg == cfg.pass) || (cfg.anonymous && user == "anonymous") {
                    logged = true;
                    say!("230-Login successful.\r\n230 Hoş geldiniz.");
                } else {
                    say!("530 Login incorrect.");
                }
            }
            "AUTH" if cfg.tls.is_some() && cfg.auth_tls && arg.eq_ignore_ascii_case("TLS") => {
                say!("{}", cfg.auth_reply);
                if !r.get_mut().start_tls(cfg.tls.as_ref().unwrap()) {
                    note("TLS-FAIL");
                    return;
                }
            }
            "PBSZ" if r.get_ref().tls.is_some() => say!("200 PBSZ=0"),
            "PROT" if r.get_ref().tls.is_some() => {
                prot_p = arg == "P";
                say!("200 Protection level set.");
            }
            "TYPE" => say!("200 Switching to Binary mode."),
            "SIZE" => match cfg.files.get(arg) {
                Some(f) => say!("213 {}", f.len()),
                None => say!("550 Could not get file size."),
            },
            "MDTM" => match (&cfg.mdtm, cfg.files.contains_key(arg)) {
                (Some(m), true) => say!("213 {m}"),
                _ => say!("550 Could not get file modification time."),
            },
            "EPSV" if cfg.epsv => {
                let l = TcpListener::bind((local, 0)).unwrap();
                say!("229 Entering Extended Passive Mode (|||{}|)", l.local_addr().unwrap().port());
                data = Some(l);
            }
            "PASV" => {
                let l = TcpListener::bind((local, 0)).unwrap();
                let p = l.local_addr().unwrap().port();
                match cfg.pasv {
                    Pasv::Paren([a, b, c, d]) => {
                        say!("227 Entering Passive Mode ({a},{b},{c},{d},{},{}).", p >> 8, p & 0xff)
                    }
                    Pasv::Equals([a, b, c, d]) => say!("227 ={a},{b},{c},{d},{},{}", p >> 8, p & 0xff),
                }
                data = Some(l);
            }
            "REST" if cfg.rest => {
                rest = arg.parse().unwrap_or(0);
                say!("350 Restart position accepted ({rest}).");
            }
            "RETR" => {
                let Some(f) = cfg.files.get(arg) else {
                    say!("550 Failed to open file.");
                    continue;
                };
                let Some(d) = data.take().and_then(|l| accept(&l)) else {
                    say!("425 Use PORT or PASV first.");
                    continue;
                };
                let mut d = Conn::plain(d);
                say!("150 Opening BINARY mode data connection.");
                if prot_p && !data_tls(&mut d, data_cfg.as_ref().unwrap(), cfg, &note) {
                    say!("522 SSL connection failed: session reuse required");
                    continue;
                }
                let body = &f[rest.min(f.len())..];
                rest = 0;
                if let Some(n) = cfg.cut_after {
                    let _ = d.write_all(&body[..n.min(body.len())]);
                    // Her iki bağlantıyı da kopar: sunucu çöktü
                    return;
                }
                let ok = d.write_all(body).is_ok();
                if cfg.data_close_notify {
                    d.close_notify();
                }
                drop(d);
                if cfg.silent_after_data {
                    continue;
                }
                if ok {
                    say!("226 Transfer complete.");
                } else {
                    say!("426 Failure writing network stream.");
                }
            }
            "MLSD" | "LIST" => {
                let text = if verb == "MLSD" { cfg.mlsd.clone() } else { Some(cfg.list.clone()) };
                let Some(text) = text else {
                    say!("500 Unknown command.");
                    continue;
                };
                let Some(d) = data.take().and_then(|l| accept(&l)) else {
                    say!("425 Use PORT or PASV first.");
                    continue;
                };
                let mut d = Conn::plain(d);
                say!("150 Here comes the directory listing.");
                if prot_p && !data_tls(&mut d, data_cfg.as_ref().unwrap(), cfg, &note) {
                    say!("522 SSL connection failed: session reuse required");
                    continue;
                }
                let _ = d.write_all(text.as_bytes());
                d.close_notify();
                drop(d);
                say!("226 Directory send OK.");
            }
            "ABOR" => say!("226 ABOR successful."),
            "QUIT" => {
                say!("221 Goodbye.");
                return;
            }
            _ => say!("500 Unknown command."),
        }
    }
}

/// Veri bağlantısında TLS el sıkışması; devam ettirilip ettirilmediği günlüğe yazılır.
fn data_tls(d: &mut Conn, tls: &Arc<ServerConfig>, cfg: &Cfg, note: &dyn Fn(&str)) -> bool {
    if !d.start_tls(tls) {
        note("DATA-TLS-FAIL");
        return false;
    }
    let resumed = d.resumed();
    note(if resumed { "DATA-TLS resumed" } else { "DATA-TLS full" });
    resumed || !cfg.require_reuse
}

fn body(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
}

fn opts() -> FtpOptions {
    FtpOptions { timeout: Duration::from_secs(2), ..Default::default() }
}

fn with_file(name: &str, data: Vec<u8>) -> Cfg {
    let mut c = Cfg::default();
    c.files.insert(name.into(), data);
    c
}

fn read_all(url: &str, o: &FtpOptions, start: u64) -> Vec<u8> {
    let (_, mut r) = open(url, o, start).unwrap();
    let mut v = Vec::new();
    r.read_to_end(&mut v).unwrap();
    v
}

#[test]
fn full_download_is_byte_exact() {
    let data = body(300_000);
    let s = start(with_file("pub/dosya.bin", data.clone()));
    let (meta, mut r) = open(&s.url("/pub/dosya.bin"), &opts(), 0).unwrap();
    assert_eq!(meta.size, Some(300_000));
    assert_eq!(meta.file_name, "dosya.bin");
    assert!(meta.resumable);
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert_eq!(got, data);
    // Akış bittikten sonra tekrar okuma da 0 döner
    assert_eq!(r.read(&mut [0u8; 16]).unwrap(), 0);
    let cmds = s.commands();
    assert!(cmds.contains(&"USER anonymous".to_string()));
    assert!(cmds.contains(&"PASS anonymous@".to_string()));
    assert!(cmds.contains(&"TYPE I".to_string()));
    assert!(cmds.contains(&"EPSV".to_string()));
    assert!(!cmds.contains(&"PASV".to_string()));
    // REST hemen RETR'den önce gelir
    let i = cmds.iter().position(|c| c == "RETR pub/dosya.bin").unwrap();
    assert_eq!(cmds[i - 1], "REST 0");
}

#[test]
fn rest_resumes_from_middle() {
    let data = body(100_000);
    let s = start(with_file("f.bin", data.clone()));
    assert_eq!(read_all(&s.url("/f.bin"), &opts(), 40_000), data[40_000..]);
    assert!(s.commands().contains(&"REST 40000".to_string()));
}

#[test]
fn rest_rejected_is_typed_error() {
    let mut c = with_file("f.bin", body(1000));
    c.rest = false;
    let s = start(c);
    let err = open(&s.url("/f.bin"), &opts(), 500).err().unwrap();
    assert_eq!(err.downcast_ref::<FtpError>(), Some(&FtpError::RestUnsupported));
    // Baştan indirme yine çalışır, ama resumable=false bildirilir
    let (meta, mut r) = open(&s.url("/f.bin"), &opts(), 0).unwrap();
    assert!(!meta.resumable);
    let mut v = Vec::new();
    r.read_to_end(&mut v).unwrap();
    assert_eq!(v, body(1000));
    assert!(!stat(&s.url("/f.bin"), &opts()).unwrap().resumable);
}

#[test]
fn falls_back_to_pasv_without_epsv() {
    let mut c = with_file("f.bin", body(5000));
    c.epsv = false;
    let s = start(c);
    assert_eq!(read_all(&s.url("/f.bin"), &opts(), 0), body(5000));
    let cmds = s.commands();
    let e = cmds.iter().position(|c| c == "EPSV").unwrap();
    assert_eq!(cmds[e + 1], "PASV");
}

#[test]
fn pasv_ip_is_ignored_and_control_ip_used() {
    // İç adres, 0.0.0.0 ya da başka bir genel adres: istemci her zaman kontrol bağlantısının IP'sine (127.0.0.1)
    // bağlanmalı; aksi halde kötü niyetli sunucu istemciyi başka bir makineye yönlendirebilir
    for pasv in [Pasv::Paren([10, 0, 0, 99]), Pasv::Equals([0, 0, 0, 0]), Pasv::Paren([203, 0, 113, 7])] {
        let mut c = with_file("f.bin", body(2000));
        c.epsv = false;
        c.pasv = pasv;
        let s = start(c);
        assert_eq!(read_all(&s.url("/f.bin"), &opts(), 0), body(2000));
    }
}

#[test]
fn multi_line_greeting() {
    let mut c = with_file("f.bin", body(10));
    c.greeting = vec![
        "220-Karadelik test sunucusu".into(),
        "220-Kurallar:".into(),
        "  221 bu satır bitiş değil".into(),
        "123 bu da değil".into(),
        "220 Hazır.".into(),
    ];
    let s = start(c);
    assert_eq!(read_all(&s.url("/f.bin"), &opts(), 0), body(10));
}

#[test]
fn wrong_password_is_clear_error() {
    let mut c = with_file("f.bin", body(10));
    c.anonymous = false;
    let s = start(c);
    let o = FtpOptions { user: "bob".into(), password: "yanlis".into(), ..opts() };
    let err = open(&s.url("/f.bin"), &o, 0).err().unwrap();
    assert!(matches!(err.downcast_ref::<FtpError>(), Some(FtpError::Login { code: 530, .. })));
    let msg = err.to_string();
    assert!(msg.contains("parola"), "{msg}");
    // Doğru parola: opts öncelikli, URL'den gelen de çalışır
    let ok = FtpOptions { user: "bob".into(), password: "secret".into(), ..opts() };
    assert!(stat(&s.url("/f.bin"), &ok).is_ok());
    let url = format!("ftp://bob:secret@{}/f.bin", s.addr);
    assert!(stat(&url, &opts()).is_ok());
}

#[test]
fn size_and_mdtm() {
    let mut c = with_file("dir/a b.txt", body(1234));
    c.mdtm = Some("20261025141500".into());
    let s = start(c);
    let m = stat(&s.url("/dir/a%20b.txt"), &opts()).unwrap();
    assert_eq!(m.size, Some(1234));
    assert_eq!(m.mdtm.as_deref(), Some("20261025141500"));
    assert!(m.resumable);
    assert_eq!(m.file_name, "a b.txt");
    // Desteklenmeyen MDTM ve bilinmeyen dosya: None, hata değil
    let mut c = with_file("x", body(1));
    c.mdtm = None;
    let s = start(c);
    let m = stat(&s.url("/x"), &opts()).unwrap();
    assert_eq!((m.size, m.mdtm), (Some(1), None));
    let m = stat(&s.url("/yok"), &opts()).unwrap();
    assert_eq!(m.size, None);
    let err = open(&s.url("/yok"), &opts(), 0).err().unwrap();
    assert!(matches!(err.downcast_ref::<FtpError>(), Some(FtpError::Reply { code: 550, .. })));
}

#[test]
fn server_dies_mid_read_returns_error() {
    let mut c = with_file("f.bin", body(200_000));
    c.cut_after = Some(50_000);
    let s = start(c);
    let t0 = Instant::now();
    let (_, mut r) = open(&s.url("/f.bin"), &opts(), 0).unwrap();
    let mut v = Vec::new();
    assert!(r.read_to_end(&mut v).is_err());
    assert!(v.len() <= 50_000);
    // Hata tekrar okunduğunda da sürer, sessiz EOF'a dönmez
    assert!(r.read(&mut [0u8; 8]).is_err());
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
}

#[test]
fn silent_server_after_data_times_out() {
    let mut c = with_file("f.bin", body(1000));
    c.silent_after_data = true;
    let s = start(c);
    let o = FtpOptions { timeout: Duration::from_millis(500), ..opts() };
    let t0 = Instant::now();
    let (_, mut r) = open(&s.url("/f.bin"), &o, 0).unwrap();
    let mut v = Vec::new();
    let err = r.read_to_end(&mut v).unwrap_err();
    assert!(err.to_string().contains("zaman aşımı"), "{err}");
    assert!(t0.elapsed() < Duration::from_secs(3));
}

#[test]
fn drop_mid_transfer_sends_abor_without_hanging() {
    let s = start(with_file("big.bin", body(16 * 1024 * 1024)));
    let (_, mut r) = open(&s.url("/big.bin"), &opts(), 0).unwrap();
    let mut buf = vec![0u8; 10_000];
    r.read_exact(&mut buf).unwrap();
    assert_eq!(buf, body(10_000));
    let t0 = Instant::now();
    drop(r);
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(s.wait_for("ABOR"));
    assert!(s.wait_for("QUIT"));
}

#[test]
fn parallel_opens_use_separate_sessions() {
    let data = Arc::new(body(400_000));
    let s = Arc::new(start(with_file("f.bin", (*data).clone())));
    let handles: Vec<_> = (0..4u64)
        .map(|i| {
            let (s, data) = (s.clone(), data.clone());
            thread::spawn(move || {
                let from = i * 100_000;
                let (_, mut r) = open(&s.url("/f.bin"), &opts(), from).unwrap();
                let mut part = vec![0u8; 100_000];
                r.read_exact(&mut part).unwrap();
                assert_eq!(part, data[from as usize..from as usize + 100_000]);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let users = s.commands().iter().filter(|c| c.starts_with("USER")).count();
    assert_eq!(users, 4);
}

#[test]
fn list_prefers_mlsd() {
    let s = start(Cfg {
        mlsd: Some("type=cdir; .\r\ntype=dir; alt\r\ntype=file;size=42; a.txt\r\n".into()),
        list: "-rw-r--r-- 1 u g 1 Oct 25 14:15 yanlis\r\n".into(),
        ..Default::default()
    });
    let v = list(&s.url("/pub/"), &opts()).unwrap();
    assert_eq!(
        v,
        vec![
            Entry { name: "alt".into(), size: None, is_dir: true },
            Entry { name: "a.txt".into(), size: Some(42), is_dir: false }
        ]
    );
    assert!(s.commands().contains(&"MLSD pub".to_string()));
}

#[test]
fn list_falls_back_to_list() {
    let s = start(Cfg {
        list: "10-25-26  02:15PM       <DIR>          alt dizin\r\n10-25-26  02:15PM                12345 a.bin\r\n".into(),
        ..Default::default()
    });
    let v = list(&s.url("/"), &opts()).unwrap();
    assert_eq!(
        v,
        vec![
            Entry { name: "alt dizin".into(), size: None, is_dir: true },
            Entry { name: "a.bin".into(), size: Some(12345), is_dir: false }
        ]
    );
    let cmds = s.commands();
    assert!(cmds.contains(&"MLSD".to_string()) && cmds.contains(&"LIST".to_string()));
}

// --- FTPS ---

fn tls_cfg(versions: &[&'static SupportedProtocolVersion]) -> (Cfg, Vec<u8>) {
    let (server, root) = tls_server(versions);
    let mut c = with_file("pub/f.bin", body(300_000));
    c.tls = Some(server);
    c.require_reuse = true;
    (c, root)
}

fn tls_opts(root: &[u8], mode: TlsMode) -> FtpOptions {
    FtpOptions { tls: mode, test_roots: vec![root.to_vec()], ..opts() }
}

fn pos(cmds: &[String], c: &str) -> usize {
    cmds.iter().position(|x| x == c).unwrap_or_else(|| panic!("{c} yok: {cmds:?}"))
}

#[test]
fn ftps_implicit_full_download() {
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.implicit = true;
    let s = start(c);
    let (meta, mut r) = open(&s.url_as("ftps", "/pub/f.bin"), &tls_opts(&root, TlsMode::Off), 0).unwrap();
    assert!(meta.tls);
    assert_eq!(meta.size, Some(300_000));
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert_eq!(got, body(300_000));
    let cmds = s.commands();
    assert!(!cmds.iter().any(|c| c.starts_with("AUTH")), "{cmds:?}");
    assert!(pos(&cmds, "PASS anonymous@") < pos(&cmds, "PBSZ 0"));
    assert!(pos(&cmds, "PBSZ 0") < pos(&cmds, "PROT P"));
    assert!(cmds.contains(&"DATA-TLS resumed".to_string()), "{cmds:?}");
    // stat da TLS'li oturum bildirir
    assert!(stat(&s.url_as("ftps", "/pub/f.bin"), &tls_opts(&root, TlsMode::Off)).unwrap().tls);
}

#[test]
fn explicit_tls_required_order_download_and_resume() {
    let (c, root) = tls_cfg(rustls::ALL_VERSIONS);
    let s = start(c);
    let o = tls_opts(&root, TlsMode::Required);
    let (meta, mut r) = open(&s.url_as("ftp", "/pub/f.bin"), &o, 0).unwrap();
    assert!(meta.tls);
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert_eq!(got, body(300_000));
    let cmds = s.commands();
    // AUTH TLS girişten önce, PBSZ ve PROT P girişten sonra ve bu sırayla
    let (auth, user, pass) = (pos(&cmds, "AUTH TLS"), pos(&cmds, "USER anonymous"), pos(&cmds, "PASS anonymous@"));
    let (pbsz, prot, retr) = (pos(&cmds, "PBSZ 0"), pos(&cmds, "PROT P"), pos(&cmds, "RETR pub/f.bin"));
    assert!(auth < user && user < pass && pass < pbsz && pbsz < prot && prot < retr, "{cmds:?}");
    assert_eq!(cmds[retr - 1], "REST 0");

    assert_eq!(read_all(&s.url_as("ftp", "/pub/f.bin"), &o, 123_456), body(300_000)[123_456..]);
    assert!(s.commands().contains(&"REST 123456".to_string()));
}

#[test]
fn explicit_tls_listing_uses_protected_data() {
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.mlsd = Some("type=file;size=42; a.txt\r\n".into());
    let s = start(c);
    let v = list(&s.url_as("ftp", "/pub/"), &tls_opts(&root, TlsMode::Required)).unwrap();
    assert_eq!(v, vec![Entry { name: "a.txt".into(), size: Some(42), is_dir: false }]);
    assert!(s.commands().contains(&"DATA-TLS resumed".to_string()));
}

#[test]
fn if_available_falls_back_to_plain() {
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.auth_tls = false;
    let s = start(c);
    let (meta, mut r) = open(&s.url_as("ftp", "/pub/f.bin"), &tls_opts(&root, TlsMode::IfAvailable), 0).unwrap();
    assert!(!meta.tls);
    let mut got = Vec::new();
    r.read_to_end(&mut got).unwrap();
    assert_eq!(got, body(300_000));
    let cmds = s.commands();
    assert_eq!(cmds[pos(&cmds, "AUTH TLS") + 1], "USER anonymous");
    assert!(!cmds.iter().any(|c| c.starts_with("PBSZ") || c.starts_with("PROT")), "{cmds:?}");
}

#[test]
fn required_without_auth_is_clear_error() {
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.auth_tls = false;
    let s = start(c);
    let err = open(&s.url_as("ftp", "/pub/f.bin"), &tls_opts(&root, TlsMode::Required), 0).err().unwrap();
    assert!(
        matches!(err.downcast_ref::<FtpError>(), Some(FtpError::TlsRefused { code: 500, command, .. }) if command == "AUTH TLS"),
        "{err:#}"
    );
    assert!(err.to_string().contains("TLS"), "{err}");
    // Parola şifresiz gönderilmedi
    assert!(!s.commands().iter().any(|c| c.starts_with("USER") || c.starts_with("PASS")));
}

#[test]
fn untrusted_certificate_is_rejected() {
    let (mut c, _root) = tls_cfg(rustls::ALL_VERSIONS);
    c.implicit = true;
    let s = start(c);
    // Test kökü verilmedi: işletim sistemi deposu bu sertifikayı tanımaz
    let err = open(&s.url_as("ftps", "/pub/f.bin"), &opts(), 0).err().unwrap();
    assert!(matches!(err.downcast_ref::<FtpError>(), Some(FtpError::Certificate { .. })), "{err:#}");
    assert!(s.wait_for("TLS-FAIL"));
    assert!(!s.commands().iter().any(|c| c.starts_with("USER")));
}

#[test]
fn certificate_name_is_checked_even_with_test_root() {
    // Kök güvenilir ama sertifika `localhost` için; IP adresiyle bağlanınca ad uyuşmaz
    let (c, root) = tls_cfg(rustls::ALL_VERSIONS);
    let s = start(c);
    let url = format!("ftp://{}/pub/f.bin", s.addr);
    let err = open(&url, &tls_opts(&root, TlsMode::Required), 0).err().unwrap();
    assert!(matches!(err.downcast_ref::<FtpError>(), Some(FtpError::Certificate { .. })), "{err:#}");
}

#[test]
fn data_connection_resumes_control_session() {
    // TLS 1.3 (durumlu bilet) ve TLS 1.2 (oturum kimliği) ayrı ayrı
    for versions in [&[&rustls::version::TLS13][..], &[&rustls::version::TLS12][..]] {
        let (c, root) = tls_cfg(versions);
        let s = start(c);
        let o = tls_opts(&root, TlsMode::Required);
        assert_eq!(read_all(&s.url_as("ftp", "/pub/f.bin"), &o, 0), body(300_000));
        let cmds = s.commands();
        assert!(cmds.contains(&"DATA-TLS resumed".to_string()), "{cmds:?}");
        assert!(!cmds.contains(&"DATA-TLS full".to_string()), "{cmds:?}");
    }
}

#[test]
fn server_rejects_data_without_session_reuse() {
    // Sunucunun denetimi gerçekten çalışıyor: veri bağlantısı ayrı oturum deposu kullanırsa devam ettirme
    // imkânsızdır, sunucu 522 verir ve indirme hata ile biter
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    let mut other = (**c.tls.as_ref().unwrap()).clone();
    other.session_storage = ServerSessionMemoryCache::new(16);
    c.data_tls = Some(Arc::new(other));
    let s = start(c);
    let (_, mut r) = open(&s.url_as("ftp", "/pub/f.bin"), &tls_opts(&root, TlsMode::Required), 0).unwrap();
    let mut v = Vec::new();
    let err = r.read_to_end(&mut v).unwrap_err();
    assert!(err.to_string().contains("522"), "{err}");
    assert!(s.commands().contains(&"DATA-TLS full".to_string()));
}

#[test]
fn missing_close_notify_with_full_size_is_success() {
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.data_close_notify = false;
    let s = start(c);
    assert_eq!(read_all(&s.url_as("ftp", "/pub/f.bin"), &tls_opts(&root, TlsMode::Required), 0), body(300_000));
}

#[test]
fn tls_transfer_cut_mid_way_is_error() {
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.cut_after = Some(50_000);
    let s = start(c);
    let (_, mut r) = open(&s.url_as("ftp", "/pub/f.bin"), &tls_opts(&root, TlsMode::Required), 0).unwrap();
    let mut v = Vec::new();
    assert!(r.read_to_end(&mut v).is_err());
    assert!(v.len() <= 50_000);
}

#[test]
fn plaintext_after_auth_reply_is_rejected() {
    // 234 ile aynı pakette gelen düz metin TLS'ten sonra gelmiş gibi işlenmemeli (STARTTLS enjeksiyonu)
    let (mut c, root) = tls_cfg(rustls::ALL_VERSIONS);
    c.auth_reply = "234 Proceed.\r\n230 araya sokulmus".into();
    let s = start(c);
    let err = open(&s.url_as("ftp", "/pub/f.bin"), &tls_opts(&root, TlsMode::Required), 0).err().unwrap();
    assert!(err.to_string().contains("beklenmeyen veri"), "{err:#}");
}
