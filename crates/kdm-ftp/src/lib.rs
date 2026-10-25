//! FTP istemcisi: RFC 959, RFC 2428 (EPSV), RFC 3659 (SIZE, MDTM, REST STREAM, MLSD), RFC 4217 (FTPS).
//!
//! Engellemeli std soketleriyle çalışır. Motor her parça için ayrı iş parçacığında `open` çağırır;
//! her çağrı kendi kontrol bağlantısını açar, böylece paralel parçalar birbirinin oturumunu bozmaz.

mod control;
mod listing;
mod tls;

pub use tls::TlsMode;

use anyhow::{anyhow, bail, Result};
use control::{io_err, Control, HostAddr, Reply};
use percent_encoding::percent_decode_str;
use std::fmt;
use std::io::{self, Read};
use std::time::Duration;
use tls::Stream;
use url::{Host, Url};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Dizin listesi için üst sınır: sonsuz akan bir liste belleği doldurmasın.
const MAX_LISTING: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct FtpOptions {
    /// Boşsa URL'deki kullanıcı, o da yoksa `anonymous`
    pub user: String,
    pub password: String,
    /// Bağlantı, okuma ve yazma için; sıfırsa 30 sn
    pub timeout: Duration,
    /// `ftp://` adreslerinde açık TLS; `ftps://` her zaman örtük TLS'tir
    pub tls: TlsMode,
    /// Yalnız testler için: boş değilse sunucu sertifikası işletim sistemi deposu yerine yalnız bu köklere
    /// (DER) göre doğrulanır. Doğrulamayı kapatmaz; kapatan bir seçenek bilerek yoktur.
    #[doc(hidden)]
    pub test_roots: Vec<Vec<u8>>,
}

impl Default for FtpOptions {
    fn default() -> Self {
        Self {
            user: String::new(),
            password: String::new(),
            timeout: DEFAULT_TIMEOUT,
            tls: TlsMode::Off,
            test_roots: Vec::new(),
        }
    }
}

// Parola günlüklere düşmesin
impl fmt::Debug for FtpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FtpOptions")
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "***" })
            .field("timeout", &self.timeout)
            .field("tls", &self.tls)
            .field("test_roots", &self.test_roots.len())
            .finish()
    }
}

/// Yoklamadan motorun ihtiyaç duyduğu bilgiler.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FtpMeta {
    /// `SIZE` (TYPE I altında bayt)
    pub size: Option<u64>,
    /// `MDTM` ham değeri, UTC `YYYYMMDDHHMMSS[.sss]`; değişiklik denetimi için
    pub mdtm: Option<String>,
    /// Sunucu `REST` komutunu kabul etti (350)
    pub resumable: bool,
    /// Yolun son parçası
    pub file_name: String,
    /// Oturum TLS'li: kontrol bağlantısı şifreli ve veri bağlantıları PROT P ile korunuyor
    pub tls: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Dizinlerde `None`
    pub size: Option<u64>,
    pub is_dir: bool,
}

/// Motorun `anyhow::Error::downcast_ref` ile ayırt edebileceği hatalar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FtpError {
    /// Kullanıcı adı veya parola reddedildi (530)
    Login { code: u16, message: String },
    /// `REST` reddedildi: devam edilemez, motor baştan başlatmalı
    RestUnsupported,
    /// Beklenmeyen yanıt; 4xx geçici, 5xx kalıcıdır
    Reply { command: String, code: u16, message: String },
    /// TLS istenirken sunucu `AUTH TLS`, `PBSZ` ya da `PROT P` komutunu 5xx ile reddetti (kalıcı)
    TlsRefused { command: String, code: u16, message: String },
    /// Sunucu sertifikası doğrulanamadı: güvenilmeyen kök, ad uyuşmazlığı, süresi geçmiş vb. (kalıcı)
    Certificate { message: String },
}

impl fmt::Display for FtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Login { code, message } => {
                write!(f, "FTP girişi reddedildi: kullanıcı adı veya parola yanlış ({code} {message})")
            }
            Self::RestUnsupported => write!(f, "FTP sunucusu kaldığı yerden devam etmeyi (REST) desteklemiyor"),
            Self::Reply { command, code, message } => write!(f, "FTP sunucusu {command} komutunu reddetti: {code} {message}"),
            Self::TlsRefused { command, code, message } => {
                write!(f, "FTP sunucusu TLS şifrelemesini desteklemiyor ({command} reddedildi: {code} {message})")
            }
            Self::Certificate { message } => write!(f, "FTP sunucusunun TLS sertifikası doğrulanamadı: {message}"),
        }
    }
}

impl std::error::Error for FtpError {}

/// Çözümlenmiş URL ve kimlik bilgileri.
#[derive(Debug)]
struct Target {
    host: HostAddr,
    port: u16,
    user: String,
    pass: String,
    /// Yüzde kodlaması çözülmüş yol. Baştaki tek `/` atılır: RFC 1738'e göre yol giriş dizinine görelidir,
    /// mutlak yol için `%2F` ile başlanır.
    path: Vec<u8>,
    file_name: String,
    timeout: Duration,
    /// `ftps://`: TCP bağlantısının hemen ardından TLS
    implicit_tls: bool,
    tls: TlsMode,
    roots: Vec<Vec<u8>>,
}

fn decode(s: &str) -> Result<String> {
    Ok(percent_decode_str(s).decode_utf8().map_err(|_| anyhow!("FTP adresindeki kullanıcı bilgisi UTF-8 değil"))?.into_owned())
}

/// Satır sonu içeren argüman ikinci bir komut enjekte eder.
fn check_arg(what: &str, b: &[u8]) -> Result<()> {
    if b.iter().any(|c| matches!(c, b'\r' | b'\n' | 0)) {
        bail!("FTP {what} geçersiz karakter içeriyor");
    }
    Ok(())
}

impl Target {
    fn parse(raw: &str, opts: &FtpOptions) -> Result<Self> {
        let raw = raw.trim();
        let orig = Url::parse(raw).map_err(|e| anyhow!("FTP adresi geçersiz: {e}"))?;
        let implicit_tls = match orig.scheme() {
            "ftp" => false,
            "ftps" => true,
            s => bail!("yalnız ftp:// ve ftps:// adresleri desteklenir: {s}"),
        };
        // `ftps` url crate'i için "özel" şema değildir: sunucu adı IDNA'dan geçmez, IPv4 tanınmaz, yol farklı
        // ayrıştırılır. Bu yüzden adres `ftp://` olarak yeniden ayrıştırılır; port ise özgün adresten alınır,
        // çünkü `ftp://` ayrıştırması 21'i varsayılan sayıp siler (`ftps://h:21` 990'a dönerdi).
        let u = if implicit_tls {
            Url::parse(&format!("ftp{}", &raw[4..])).map_err(|e| anyhow!("FTP adresi geçersiz: {e}"))?
        } else {
            orig.clone()
        };
        let host = match u.host() {
            Some(Host::Domain(d)) if !d.is_empty() => HostAddr::Name(d.to_string()),
            Some(Host::Ipv4(a)) => HostAddr::Ip(a.into()),
            Some(Host::Ipv6(a)) => HostAddr::Ip(a.into()),
            _ => bail!("FTP adresinde sunucu yok"),
        };
        let port = orig.port().unwrap_or(if implicit_tls { 990 } else { 21 });

        let url_user = decode(u.username())?;
        let (user, pass) = if !opts.user.is_empty() {
            (opts.user.clone(), opts.password.clone())
        } else if !url_user.is_empty() {
            let p = match u.password() {
                Some(p) => decode(p)?,
                None => opts.password.clone(),
            };
            (url_user, p)
        } else {
            let p = if opts.password.is_empty() { "anonymous@".to_string() } else { opts.password.clone() };
            ("anonymous".to_string(), p)
        };
        check_arg("kullanıcı adı", user.as_bytes())?;
        check_arg("parolası", pass.as_bytes())?;

        let mut raw_path = u.path();
        // RFC 1738 `;type=a|i|d` eki: aktarım türünü biz seçeriz (her zaman I)
        if let Some(i) = raw_path.rfind(";type=") {
            if raw_path.len() - i == 7 {
                raw_path = &raw_path[..i];
            }
        }
        let rel = raw_path.strip_prefix('/').unwrap_or(raw_path);
        let path: Vec<u8> = percent_decode_str(rel).collect();
        check_arg("yolu", &path)?;

        let last = path.split(|b| *b == b'/').rfind(|s| !s.is_empty());
        let file_name = match last {
            Some(s) => String::from_utf8_lossy(s).into_owned(),
            None => u.host_str().unwrap_or_default().to_string(),
        };
        let timeout = if opts.timeout.is_zero() { DEFAULT_TIMEOUT } else { opts.timeout };
        Ok(Self {
            host,
            port,
            user,
            pass,
            path,
            file_name,
            timeout,
            implicit_tls,
            tls: opts.tls,
            roots: opts.test_roots.clone(),
        })
    }

    /// Dizin yolu: sondaki `/` atılır (kök hariç).
    fn dir_path(&self) -> &[u8] {
        let mut p = self.path.as_slice();
        while p.len() > 1 && p.ends_with(b"/") {
            p = &p[..p.len() - 1];
        }
        p
    }
}

/// Bağlanır, gerekiyorsa TLS'e geçer, karşılamayı okur, oturum açar ve ikili kipe geçer.
fn session(t: &Target) -> Result<Control> {
    let mut c = Control::connect(&t.host, t.port, t.timeout)?;
    let tls = if t.implicit_tls || t.tls != TlsMode::Off {
        Some((tls::session_config(&t.roots)?, tls::server_name(&t.host)?))
    } else {
        None
    };
    if let (true, Some((cfg, name))) = (t.implicit_tls, &tls) {
        c.start_tls(cfg.clone(), name.clone())?;
    }
    let mut r = c.read_reply()?;
    // 120: "birazdan hazır", ardından 220 gelir
    if r.code == 120 {
        r = c.read_reply()?;
    }
    if r.code != 220 {
        return Err(r.error("bağlantı").into());
    }
    if let (false, Some((cfg, name))) = (t.implicit_tls, &tls) {
        // RFC 4217 §4: giriş bilgileri şifresiz gitmesin diye AUTH TLS, USER'dan önce
        let r = c.cmd("AUTH", b"TLS")?;
        match r.code {
            234 => c.start_tls(cfg.clone(), name.clone())?,
            _ if r.class() == 5 && t.tls == TlsMode::IfAvailable => {}
            _ => return Err(tls_refused(&r, "AUTH TLS")),
        }
    }
    let mut r = c.cmd("USER", t.user.as_bytes())?;
    if r.code == 331 {
        r = c.cmd("PASS", t.pass.as_bytes())?;
    }
    match r.code {
        230 | 202 => {}
        332 => bail!("FTP sunucusu hesap bilgisi (ACCT) istiyor; desteklenmiyor"),
        530 => return Err(FtpError::Login { code: r.code, message: r.message() }.into()),
        // 421 (çok fazla bağlantı) vb. geçicidir; oturum hatası sayılmaz
        _ if r.class() == 4 => return Err(r.error("USER/PASS").into()),
        _ => return Err(FtpError::Login { code: r.code, message: r.message() }.into()),
    }
    if let (true, Some((cfg, name))) = (c.is_tls(), tls) {
        // RFC 4217 §8-9: PBSZ, PROT'tan önce zorunludur ve TLS için her zaman 0'dır. PROT P reddedilirse düz veri
        // bağlantısına (PROT C) düşülmez: şifreli oturum kurulmuşken dosya içeriği açık gitmiş olurdu.
        let r = c.cmd("PBSZ", b"0")?;
        if r.class() != 2 {
            return Err(tls_refused(&r, "PBSZ"));
        }
        let r = c.cmd("PROT", b"P")?;
        if r.class() != 2 {
            return Err(tls_refused(&r, "PROT P"));
        }
        c.protect_data(cfg, name);
    }
    // RFC 2640: eski sunucularda UTF-8 dosya adlarını açar; yanıt önemsiz
    let _ = c.cmd("OPTS", b"UTF8 ON")?;
    let r = c.cmd("TYPE", b"I")?;
    if r.class() != 2 {
        return Err(r.error("TYPE I").into());
    }
    Ok(c)
}

/// 5xx: TLS bu sunucuda yok, kalıcı. 4xx (431 vb.) geçicidir, yeniden denenir.
fn tls_refused(r: &Reply, command: &str) -> anyhow::Error {
    if r.class() == 5 {
        FtpError::TlsRefused { command: command.to_string(), code: r.code, message: r.message() }.into()
    } else {
        r.error(command).into()
    }
}

/// SIZE ve MDTM; ikisi de isteğe bağlıdır (RFC 3659), reddedilirse `None`.
fn probe(c: &mut Control, t: &Target) -> Result<FtpMeta> {
    let r = c.cmd("SIZE", &t.path)?;
    let size = if r.code == 213 { r.message().trim().parse().ok() } else { None };
    let r = c.cmd("MDTM", &t.path)?;
    let mdtm = if r.code == 213 {
        let v = r.message();
        let v = v.split_whitespace().next().unwrap_or("").to_string();
        (v.len() >= 14 && v.as_bytes()[..14].iter().all(u8::is_ascii_digit)).then_some(v)
    } else {
        None
    };
    Ok(FtpMeta { size, mdtm, resumable: false, file_name: t.file_name.clone(), tls: c.is_tls() })
}

/// Dosya boyutu, değişiklik zamanı ve devam desteğini yoklar; aktarım yapmaz.
pub fn stat(url: &str, opts: &FtpOptions) -> Result<FtpMeta> {
    let t = Target::parse(url, opts)?;
    let mut c = session(&t)?;
    let mut meta = probe(&mut c, &t)?;
    // REST 0 zararsızdır: yalnız bir sonraki aktarım komutunu etkiler, ardından QUIT gelir
    meta.resumable = c.cmd("REST", b"0")?.code == 350;
    Ok(meta)
}

/// Dosyayı `start` ofsetinden okumaya başlar.
///
/// `REST` her zaman gönderilir (`start == 0` iken `REST 0`), böylece `resumable` her çağrıda bilinir.
/// `start > 0` iken REST reddedilirse `FtpError::RestUnsupported` döner.
pub fn open(url: &str, opts: &FtpOptions, start: u64) -> Result<(FtpMeta, FtpReader)> {
    let t = Target::parse(url, opts)?;
    let mut c = session(&t)?;
    let mut meta = probe(&mut c, &t)?;
    // Veri bağlantısı REST'ten önce açılır: REST'ten hemen sonra aktarım komutu gelmeli (RFC 959 §4.1.3)
    let mut data = c.open_data()?;
    let r = c.cmd("REST", start.to_string().as_bytes())?;
    meta.resumable = r.code == 350;
    if !meta.resumable && start > 0 {
        return Err(FtpError::RestUnsupported.into());
    }
    let r = c.cmd("RETR", &t.path)?;
    let final_pending = match r.class() {
        1 => true,
        // Bazı sunucular küçük dosyada 1xx göndermeden doğrudan 226 yazar
        2 => false,
        _ => return Err(r.error("RETR").into()),
    };
    // Veri TLS'i ancak şimdi: sunucu el sıkışmaya aktarım komutunu aldıktan sonra başlar
    data.handshake("veri bağlantısında")?;
    let expected = meta.size.map(|s| s.saturating_sub(start));
    let reader =
        FtpReader { data: Some(data), ctrl: Some(c), final_pending, expected, received: 0, state: State::Reading };
    Ok((meta, reader))
}

/// Dizin listesi: önce MLSD (makinece okunur), desteklenmezse LIST.
pub fn list(url: &str, opts: &FtpOptions) -> Result<Vec<Entry>> {
    let t = Target::parse(url, opts)?;
    let mut c = session(&t)?;
    let dir = t.dir_path();
    match transfer_text(&mut c, "MLSD", dir) {
        Ok(text) => return Ok(listing::parse_mlsd(&text)),
        Err(e) => match e.downcast_ref::<FtpError>() {
            Some(FtpError::Reply { command, code, .. }) if command == "MLSD" && code / 100 == 5 => {}
            _ => return Err(e),
        },
    }
    let text = transfer_text(&mut c, "LIST", dir)?;
    Ok(listing::parse_list(&text))
}

fn transfer_text(c: &mut Control, verb: &str, arg: &[u8]) -> Result<String> {
    let mut data = c.open_data()?;
    let r = c.cmd(verb, arg)?;
    let pending = match r.class() {
        1 => true,
        2 => false,
        _ => return Err(r.error(verb).into()),
    };
    data.handshake("veri bağlantısında")?;
    let mut buf = Vec::new();
    (&mut data).take(MAX_LISTING + 1).read_to_end(&mut buf).map_err(io_err)?;
    drop(data);
    if buf.len() as u64 > MAX_LISTING {
        bail!("FTP dizin listesi çok büyük");
    }
    if pending {
        let r: Reply = c.read_reply()?;
        if r.class() != 2 {
            return Err(r.error(verb).into());
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

enum State {
    Reading,
    Done,
    Failed(String),
}

/// RETR veri akışı. Akış sonunda sunucunun bitiş yanıtını ve beklenen bayt sayısını denetler;
/// eksik ya da yarıda kesilen aktarım `Ok(0)` değil hata döner. TLS'li oturumda veri de TLS üstündendir.
pub struct FtpReader {
    data: Option<Stream>,
    ctrl: Option<Control>,
    final_pending: bool,
    expected: Option<u64>,
    received: u64,
    state: State,
}

impl FtpReader {
    fn finish(&mut self) -> Result<()> {
        if let Some(d) = self.data.take() {
            d.shutdown();
        }
        if self.final_pending {
            let c = self.ctrl.as_mut().ok_or_else(|| anyhow!("FTP kontrol bağlantısı yok"))?;
            let r = c.read_reply().map_err(|e| e.context("FTP aktarımı tamamlanmadı"))?;
            self.final_pending = false;
            if r.class() != 2 {
                return Err(r.error("RETR").into());
            }
        }
        if let Some(exp) = self.expected {
            if self.received < exp {
                bail!("FTP aktarımı eksik kaldı: {} / {} bayt", self.received, exp);
            }
        }
        // Drop QUIT gönderir
        self.ctrl = None;
        Ok(())
    }
}

impl Read for FtpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.state {
            State::Done => return Ok(0),
            State::Failed(m) => return Err(io::Error::other(m.clone())),
            State::Reading => {}
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(data) = self.data.as_mut() else { return Ok(0) };
        match data.read(buf) {
            Ok(0) => match self.finish() {
                Ok(()) => {
                    self.state = State::Done;
                    Ok(0)
                }
                Err(e) => {
                    let m = format!("{e:#}");
                    self.state = State::Failed(m.clone());
                    Err(io::Error::other(m))
                }
            },
            Ok(n) => {
                self.received += n as u64;
                Ok(n)
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(e),
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
                Err(io::Error::new(io::ErrorKind::TimedOut, "FTP veri bağlantısı zaman aşımına uğradı"))
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for FtpReader {
    fn drop(&mut self) {
        // Yarıda bırakılan aktarım: önce veri bağlantısı kapanır, sonra ABOR; Control'ün Drop'u QUIT yazar.
        // Yanıt beklenmez, böylece yanıt vermeyen sunucuda Drop takılmaz.
        if let Some(d) = self.data.take() {
            d.shutdown();
            if let Some(c) = self.ctrl.as_mut() {
                c.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(user: &str, pass: &str) -> FtpOptions {
        FtpOptions { user: user.into(), password: pass.into(), ..Default::default() }
    }

    #[test]
    fn target_anonymous_and_path() {
        let t = Target::parse("ftp://ftp.example.org/pub/dosya%20ad%C4%B1.iso", &opts("", "")).unwrap();
        assert_eq!(t.host, HostAddr::Name("ftp.example.org".into()));
        assert_eq!(t.port, 21);
        assert_eq!((t.user.as_str(), t.pass.as_str()), ("anonymous", "anonymous@"));
        assert_eq!(t.path, "pub/dosya adı.iso".as_bytes());
        assert_eq!(t.file_name, "dosya adı.iso");
    }

    #[test]
    fn target_credentials_priority() {
        let t = Target::parse("ftp://bob:s%40cret@10.0.0.1:2121/a.txt", &opts("", "")).unwrap();
        assert_eq!((t.user.as_str(), t.pass.as_str(), t.port), ("bob", "s@cret", 2121));
        let t = Target::parse("ftp://bob:s%40cret@10.0.0.1/a.txt", &opts("alice", "pw")).unwrap();
        assert_eq!((t.user.as_str(), t.pass.as_str()), ("alice", "pw"));
    }

    #[test]
    fn target_absolute_typecode_and_dir() {
        let t = Target::parse("ftp://h/%2Fetc/x.bin;type=i", &opts("", "")).unwrap();
        assert_eq!(t.path, b"/etc/x.bin");
        assert_eq!(t.file_name, "x.bin");
        let t = Target::parse("ftp://[::1]/pub/", &opts("", "")).unwrap();
        assert!(matches!(t.host, HostAddr::Ip(ip) if ip.is_ipv6()));
        assert_eq!(t.dir_path(), b"pub");
        assert_eq!(t.file_name, "pub");
        assert_eq!(Target::parse("ftp://h/", &opts("", "")).unwrap().file_name, "h");
    }

    #[test]
    fn target_ftps_ports_and_host() {
        let t = Target::parse("ftps://Ftp.Example.org/pub/a.bin", &opts("", "")).unwrap();
        assert!(t.implicit_tls);
        assert_eq!((t.host.clone(), t.port), (HostAddr::Name("ftp.example.org".into()), 990));
        assert_eq!(t.path, b"pub/a.bin");
        // Açık 21 korunur; ftp:// ayrıştırması onu varsayılan sayıp silerdi
        assert_eq!(Target::parse("ftps://h:21/a", &opts("", "")).unwrap().port, 21);
        assert_eq!(Target::parse("ftps://bob:pw@h:2990/a", &opts("", "")).unwrap().port, 2990);
        // Özel şema ayrıştırması: IPv4 tanınır, IDN punycode'a çevrilir
        assert!(matches!(Target::parse("ftps://127.0.0.1/a", &opts("", "")).unwrap().host, HostAddr::Ip(_)));
        let t = Target::parse("ftps://türkçe.example/a", &opts("", "")).unwrap();
        assert!(matches!(&t.host, HostAddr::Name(n) if n.starts_with("xn--") && n.ends_with(".example")), "{:?}", t.host);
        assert!(!Target::parse("ftp://h/a", &opts("", "")).unwrap().implicit_tls);
    }

    #[test]
    fn target_rejects_injection_and_scheme() {
        assert!(Target::parse("ftp://h/a%0D%0ADELE%20b", &opts("", "")).is_err());
        assert!(Target::parse("http://h/a", &opts("", "")).is_err());
        assert!(Target::parse("ftp://h/a", &opts("x\r\nDELE y", "")).is_err());
    }
}
