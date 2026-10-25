//! FTPS: örtük TLS (`ftps://`, 990) ve açık TLS (RFC 4217, `AUTH TLS`). Kontrol ve veri bağlantıları aynı
//! `Stream` türünü kullanır; TLS bağlantı kurulduktan sonra yerinde açılır (AUTH TLS'te önce düz konuşulur).

use crate::control::{io_err, HostAddr};
use crate::FtpError;
use anyhow::{anyhow, Result};
use rustls::client::Resumption;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, OnceLock};

/// Oturum başına önbellek: kontrol bağlantısı ve birkaç veri bağlantısı için yeter.
const SESSION_CACHE: usize = 8;

/// `ftp://` adreslerinde açık TLS (RFC 4217). `ftps://` her zaman örtük TLS kullanır, bu ayar yok sayılır.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TlsMode {
    /// Düz FTP
    #[default]
    Off,
    /// `AUTH TLS` denenir; sunucu 5xx ile reddederse düz FTP ile devam edilir
    IfAvailable,
    /// `AUTH TLS` reddedilirse `FtpError::TlsRefused`
    Required,
}

/// Süreçte tek sağlayıcı: ureq ile aynı (ring). Süreç varsayılanına güvenilmez; başka bir crate ikinci bir
/// sağlayıcı açarsa `ClientConfig::builder()` hangisini seçeceğini bilemez.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// İşletim sistemi kök sertifikalarıyla doğrulayan temel yapılandırma bir kez kurulur: bazı platformlarda
/// doğrulayıcı kök deposunu ilk kullanımda diskten yükler, her parça için yeniden yüklemek boşa olur.
fn platform_config() -> Result<Arc<ClientConfig>> {
    static BASE: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(c) = BASE.get() {
        return Ok(c.clone());
    }
    let c = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .and_then(|b| b.with_platform_verifier())
        .map_err(|e| anyhow!("TLS yapılandırması kurulamadı: {e}"))?
        .with_no_client_auth();
    Ok(BASE.get_or_init(|| Arc::new(c)).clone())
}

/// Yalnız verilen köklere güvenen yapılandırma (testler). İşletim sistemi deposu karışmaz; sonuç makineden
/// bağımsızdır. Doğrulama yine tamdır: ad, süre ve imza denetlenir.
fn roots_config(roots: &[Vec<u8>]) -> Result<Arc<ClientConfig>> {
    let mut store = RootCertStore::empty();
    for der in roots {
        store.add(CertificateDer::from(der.clone())).map_err(|e| anyhow!("TLS kök sertifikası geçersiz: {e}"))?;
    }
    let c = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow!("TLS yapılandırması kurulamadı: {e}"))?
        .with_root_certificates(store)
        .with_no_client_auth();
    Ok(Arc::new(c))
}

/// Tek FTP oturumunun (kontrol + veri bağlantıları) yapılandırması.
///
/// Oturum önbelleği (`resumption`) bilerek her oturuma ayrı verilir. Sunucuların çoğu (vsftpd
/// `require_ssl_reuse`, ProFTPD, FileZilla Server) veri bağlantısının TLS oturumunun kontrol bağlantısınınkinin
/// devamı olmasını ister: böylece veri portuna bağlanan üçüncü biri aktarımı çalamaz. Veri bağlantısı aynı
/// `ClientConfig`'i ve aynı `ServerName`'i kullandığında rustls kontrol bağlantısından kalan oturumu (TLS 1.2
/// oturum kimliği / bileti, TLS 1.3 PSK bileti) önbellekte bulur ve sunar. Klonlanan `ClientConfig` önbelleği
/// `Arc` ile paylaştığı için yeni bir depo şarttır; yoksa paralel parçaların veri bağlantıları birbirlerinin
/// oturumunu devam ettirmeye çalışır ve aynı oturumu şart koşan sunucular bağlantıyı reddeder.
pub(crate) fn session_config(roots: &[Vec<u8>]) -> Result<Arc<ClientConfig>> {
    let base = if roots.is_empty() { platform_config()? } else { roots_config(roots)? };
    let mut c = (*base).clone();
    c.resumption = Resumption::in_memory_sessions(SESSION_CACHE);
    Ok(Arc::new(c))
}

/// SNI ve sertifika doğrulaması URL'deki adla yapılır (veri bağlantısı IP'ye açılsa da).
pub(crate) fn server_name(h: &HostAddr) -> Result<ServerName<'static>> {
    match h {
        HostAddr::Ip(ip) => Ok(ServerName::IpAddress((*ip).into())),
        HostAddr::Name(n) => ServerName::try_from(n.clone()).map_err(|_| anyhow!("FTP sunucu adı TLS için geçersiz: {n}")),
    }
}

/// El sıkışma hatası: sertifika sorunları kalıcıdır, ayrı türde döner; zaman aşımı okunur iletiye çevrilir.
fn handshake_err(e: io::Error, what: &str) -> anyhow::Error {
    if let Some(rustls::Error::InvalidCertificate(_)) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        return FtpError::Certificate { message: e.to_string() }.into();
    }
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::UnexpectedEof => {
            io_err(e).context(format!("FTP {what} TLS el sıkışması tamamlanmadı"))
        }
        _ => anyhow!("FTP {what} TLS el sıkışması başarısız: {e}"),
    }
}

/// TCP bağlantısı; TLS açıldıysa üstünde rustls istemci bağlantısı.
pub(crate) struct Stream {
    tcp: TcpStream,
    tls: Option<ClientConnection>,
}

impl Stream {
    pub fn plain(tcp: TcpStream) -> Self {
        Self { tcp, tls: None }
    }

    /// El sıkışma burada yapılmaz: veri bağlantısında sunucu el sıkışmaya ancak aktarım komutundan sonra başlar,
    /// önceden beklemek kilitlenir. Çağıran uygun anda `handshake` çağırır.
    pub fn tls(tcp: TcpStream, cfg: Arc<ClientConfig>, name: ServerName<'static>) -> Result<Self> {
        let conn = ClientConnection::new(cfg, name).map_err(|e| anyhow!("TLS bağlantısı başlatılamadı: {e}"))?;
        Ok(Self { tcp, tls: Some(conn) })
    }

    /// Düz bağlantıyı TLS'e çevirir (AUTH TLS sonrası) ve el sıkışır.
    pub fn upgrade(&mut self, cfg: Arc<ClientConfig>, name: ServerName<'static>) -> Result<()> {
        let conn = ClientConnection::new(cfg, name).map_err(|e| anyhow!("TLS bağlantısı başlatılamadı: {e}"))?;
        self.tls = Some(conn);
        self.handshake("kontrol bağlantısında")
    }

    pub fn handshake(&mut self, what: &str) -> Result<()> {
        let Some(c) = self.tls.as_mut() else { return Ok(()) };
        while c.is_handshaking() {
            c.complete_io(&mut self.tcp).map_err(|e| handshake_err(e, what))?;
        }
        Ok(())
    }

    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    pub fn tcp(&self) -> &TcpStream {
        &self.tcp
    }

    /// TLS kapanış uyarısı (close_notify); yanıt beklenmez.
    pub fn close_notify(&mut self) {
        if let Some(c) = self.tls.as_mut() {
            if !c.is_handshaking() {
                c.send_close_notify();
                while c.wants_write() {
                    if !matches!(c.write_tls(&mut self.tcp), Ok(n) if n > 0) {
                        break;
                    }
                }
            }
        }
    }

    pub fn shutdown(&self) {
        let _ = self.tcp.shutdown(Shutdown::Both);
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(c) = self.tls.as_mut() else { return self.tcp.read(buf) };
        let r = rustls::Stream::new(c, &mut self.tcp).read(buf);
        match r {
            // close_notify gelmeden kapanan bağlantı. Birçok sunucu veri bağlantısını uyarı göndermeden kapatır;
            // bunu hata saymak bu sunucularla hiç indirme yapamamak demektir. Kesilmeye karşı koruma yine sürer:
            // aktarım sonunda kontrol bağlantısından (TLS'li) gelen 226 ve SIZE ile bayt sayısı denetlenir.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && !c.is_handshaking() => Ok(0),
            r => r,
        }
    }
}

impl Write for Stream {
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
