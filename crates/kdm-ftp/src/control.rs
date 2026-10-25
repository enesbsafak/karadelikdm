//! Kontrol bağlantısı: komut gönderme, RFC 959 §4.2 yanıt okuma ve pasif veri bağlantısı (EPSV / PASV).

use crate::tls::Stream;
use crate::FtpError;
use anyhow::{anyhow, bail, Result};
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// Tek yanıt satırı için üst sınır: kötü niyetli sunucu belleği şişiremesin.
const MAX_LINE: u64 = 8 * 1024;
const MAX_LINES: usize = 2000;
/// Drop sırasında QUIT / ABOR yazarken beklenecek en uzun süre.
const CLOSE_WAIT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostAddr {
    Name(String),
    Ip(IpAddr),
}

/// Sunucu yanıtı: üç haneli kod ve ham satırlar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub code: u16,
    pub lines: Vec<String>,
}

impl Reply {
    pub fn class(&self) -> u16 {
        self.code / 100
    }

    /// Kod önekleri atılmış metin (çok satırlıysa satırlar boşlukla birleşir).
    pub fn message(&self) -> String {
        let code = self.code.to_string();
        let parts: Vec<&str> = self
            .lines
            .iter()
            .map(|l| {
                let rest = l.strip_prefix(code.as_str()).filter(|r| r.is_empty() || r.starts_with([' ', '-']));
                rest.map(|r| r.get(1..).unwrap_or("")).unwrap_or(l).trim()
            })
            .filter(|s| !s.is_empty())
            .collect();
        parts.join(" ")
    }

    pub fn error(&self, command: &str) -> FtpError {
        FtpError::Reply { command: command.to_string(), code: self.code, message: self.message() }
    }
}

/// `xyz-` çok satırlı, `xyz ` ya da yalnız `xyz` tek satırlı başlangıçtır.
fn parse_head(line: &str) -> Option<(u16, bool)> {
    let b = line.as_bytes();
    if b.len() < 3 || !b[..3].iter().all(u8::is_ascii_digit) || !(b'1'..=b'5').contains(&b[0]) {
        return None;
    }
    let code = line[..3].parse().ok()?;
    match b.get(3) {
        None | Some(b' ') => Some((code, false)),
        Some(b'-') => Some((code, true)),
        _ => None,
    }
}

fn read_line<R: BufRead>(r: &mut R) -> Result<String> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf).map_err(io_err)?;
    if n == 0 {
        bail!("FTP sunucusu bağlantıyı kapattı");
    }
    if !buf.ends_with(b"\n") && n as u64 == MAX_LINE {
        bail!("FTP sunucusunun yanıt satırı çok uzun");
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// RFC 959 §4.2: çok satırlı yanıt, aynı kod ve ardından boşluk taşıyan satırla biter;
/// aradaki satırlar başka rakamlarla başlayabilir ve bitiş sayılmaz.
pub(crate) fn read_reply<R: BufRead>(r: &mut R) -> Result<Reply> {
    let first = read_line(r)?;
    let (code, multi) = parse_head(&first).ok_or_else(|| anyhow!("FTP sunucusundan geçersiz yanıt: {first:?}"))?;
    let mut lines = vec![first];
    if multi {
        let prefix = format!("{code:03}");
        loop {
            let l = read_line(r)?;
            let end = l.starts_with(&prefix) && matches!(l.as_bytes().get(3), None | Some(b' '));
            lines.push(l);
            if end {
                break;
            }
            if lines.len() > MAX_LINES {
                bail!("FTP sunucusunun çok satırlı yanıtı bitmiyor");
            }
        }
    }
    Ok(Reply { code, lines })
}

/// Zaman aşımı hatalarını okunur bir iletiye çevirir (Windows `TimedOut`, Unix `WouldBlock` verir).
pub(crate) fn io_err(e: io::Error) -> anyhow::Error {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => anyhow!("FTP sunucusu yanıt vermedi (zaman aşımı)"),
        // Tür korunur: motor "bağlantı reddedildi"yi ayırt eder
        _ => anyhow::Error::new(e).context("FTP bağlantı hatası"),
    }
}

fn connect_addr(addr: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    connect_addr_with(addr, timeout, timeout)
}

/// `connect`: bağlantı kurma süresi; `io`: okuma/yazma zaman aşımı.
fn connect_addr_with(addr: SocketAddr, connect: Duration, io: Duration) -> io::Result<TcpStream> {
    let s = TcpStream::connect_timeout(&addr, connect)?;
    s.set_read_timeout(Some(io))?;
    s.set_write_timeout(Some(io))?;
    let _ = s.set_nodelay(true);
    Ok(s)
}

/// Adresleri sırayla dener. Zaman aşımı adres başına değil toplam bütçedir: birçok adrese çözülen ve 21. portu
/// süzen bir sunucuda (ör. 4 adres) adres başına 30 sn beklemek tek denemeyi dakikalara uzatırdı. Kalan süre kalan
/// adreslere bölünür. `Err(None)`: adres yok.
fn connect_any(addrs: &[SocketAddr], timeout: Duration) -> Result<TcpStream, Option<io::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = None;
    for (i, a) in addrs.iter().enumerate() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(Some(last.unwrap_or_else(|| io::Error::from(io::ErrorKind::TimedOut))));
        }
        let share = (left / (addrs.len() - i) as u32).max(Duration::from_millis(500)).min(left);
        match connect_addr_with(*a, share, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last)
}

/// PROT P sonrası veri bağlantılarının TLS ayarları: kontrol bağlantısıyla aynı yapılandırma ve ad, böylece
/// oturum yeniden kullanılır (bkz. `tls::session_config`).
struct DataTls {
    cfg: Arc<ClientConfig>,
    name: ServerName<'static>,
}

pub(crate) struct Control {
    reader: BufReader<Stream>,
    timeout: Duration,
    peer: IpAddr,
    /// EPSV bir kez başarısız olursa bu oturumda bir daha denenmez.
    epsv: bool,
    /// `None`: veri bağlantıları düz (PROT C ya da TLS yok)
    data_tls: Option<DataTls>,
    quit_sent: bool,
}

impl Control {
    pub fn connect(host: &HostAddr, port: u16, timeout: Duration) -> Result<Self> {
        let addrs: Vec<SocketAddr> = match host {
            HostAddr::Ip(ip) => vec![SocketAddr::new(*ip, port)],
            HostAddr::Name(n) => (n.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| anyhow!("FTP sunucusu bulunamadı ({n}): {e}"))?
                .collect(),
        };
        let s = connect_any(&addrs, timeout).map_err(|e| match e {
            Some(e) => io_err(e).context("FTP sunucusuna bağlanılamadı"),
            None => anyhow!("FTP sunucusunun adresi çözülemedi"),
        })?;
        let peer = s.peer_addr().map(|p| p.ip()).unwrap_or(addrs[0].ip());
        Ok(Self { reader: BufReader::new(Stream::plain(s)), timeout, peer, epsv: true, data_tls: None, quit_sent: false })
    }

    /// Kontrol bağlantısında TLS'e geçer (örtük TLS'te karşılamadan önce, açık TLS'te 234'ten sonra).
    pub fn start_tls(&mut self, cfg: Arc<ClientConfig>, name: ServerName<'static>) -> Result<()> {
        // 234'ten sonra tamponda kalan düz metin, saldırganın araya soktuğu ve TLS'ten sonra gelmiş gibi işlenecek
        // bir yanıt olabilir (STARTTLS komut enjeksiyonu, CVE-2011-0411 türü); kabul edilmez.
        if !self.reader.buffer().is_empty() {
            bail!("FTP sunucusu TLS el sıkışmasından önce beklenmeyen veri gönderdi");
        }
        self.reader.get_mut().upgrade(cfg, name)
    }

    pub fn is_tls(&self) -> bool {
        self.reader.get_ref().is_tls()
    }

    /// PROT P kabul edildi: bundan sonraki veri bağlantıları kontrol bağlantısının oturumunu devam ettirir.
    pub fn protect_data(&mut self, cfg: Arc<ClientConfig>, name: ServerName<'static>) {
        self.data_tls = Some(DataTls { cfg, name });
    }

    pub fn read_reply(&mut self) -> Result<Reply> {
        read_reply(&mut self.reader)
    }

    fn send(&mut self, line: &[u8]) -> io::Result<()> {
        let s = self.reader.get_mut();
        s.write_all(line)?;
        s.flush()
    }

    /// Komut gönderir ve yanıtı okur. Argüman ham bayttır: sunucu dosya adları UTF-8 olmayabilir.
    pub fn cmd(&mut self, verb: &str, arg: &[u8]) -> Result<Reply> {
        let mut line = Vec::with_capacity(verb.len() + arg.len() + 3);
        line.extend_from_slice(verb.as_bytes());
        if !arg.is_empty() {
            line.push(b' ');
            line.extend_from_slice(arg);
        }
        line.extend_from_slice(b"\r\n");
        self.send(&line).map_err(io_err)?;
        self.read_reply()
    }

    /// Pasif veri bağlantısı; PROT P iken TLS'li, el sıkışma çağırana bırakılır (`Stream::tls`).
    pub fn open_data(&mut self) -> Result<Stream> {
        let tcp = self.open_data_tcp()?;
        match &self.data_tls {
            None => Ok(Stream::plain(tcp)),
            Some(d) => Stream::tls(tcp, d.cfg.clone(), d.name.clone()),
        }
    }

    /// Önce EPSV (RFC 2428), olmazsa PASV.
    fn open_data_tcp(&mut self) -> Result<TcpStream> {
        if self.epsv {
            let r = self.cmd("EPSV", b"")?;
            if r.code == 421 {
                return Err(r.error("EPSV").into());
            }
            if r.code == 229 {
                if let Some(port) = crate::listing::parse_epsv(&r.message()) {
                    if let Ok(s) = connect_addr(SocketAddr::new(self.peer, port), self.timeout) {
                        return Ok(s);
                    }
                }
            }
            // 500/502 (desteklenmiyor), ayrıştırılamayan yanıt ya da bağlanılamayan port: PASV'ye düş
            self.epsv = false;
        }
        let r = self.cmd("PASV", b"")?;
        if r.code != 227 {
            return Err(r.error("PASV").into());
        }
        let (ip, port) =
            crate::listing::parse_pasv(&r.message()).ok_or_else(|| anyhow!("PASV yanıtı anlaşılamadı: {}", r.message()))?;
        // Bildirilen IP yok sayılır, her zaman kontrol bağlantısının adresi kullanılır (curl varsayılanı gibi).
        // NAT arkasındaki sunucunun iç adresi sorunu da çözülür; kötü niyetli bir sunucu da istemciyi yerel ağdaki
        // başka bir makineye bağlantı açmaya yönlendiremez (FTP bounce / SSRF).
        let _ = ip;
        let ip = self.peer;
        connect_addr(SocketAddr::new(ip, port), self.timeout)
            .map_err(|e| io_err(e).context(format!("FTP veri bağlantısı açılamadı ({ip}:{port})")))
    }

    /// Süren aktarımı iptal eder ve oturumu kapatır; yanıt beklenmez ki Drop takılmasın.
    /// İkisi tek yazımda gider: sunucu ABOR'a yanıt yazarken bağlantı kapanırsa ayrı gelen QUIT kaybolabilir.
    pub fn abort(&mut self) {
        self.close_with(b"ABOR\r\nQUIT\r\n");
    }

    fn close_with(&mut self, cmds: &[u8]) {
        if self.quit_sent {
            return;
        }
        self.quit_sent = true;
        let _ = self.reader.get_ref().tcp().set_write_timeout(Some(CLOSE_WAIT));
        let _ = self.send(cmds);
        self.reader.get_mut().close_notify();
        let _ = self.reader.get_ref().tcp().shutdown(Shutdown::Write);
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.close_with(b"QUIT\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn reply(s: &str) -> Result<Reply> {
        read_reply(&mut Cursor::new(s.as_bytes().to_vec()))
    }

    #[test]
    fn single_line() {
        let r = reply("220 hazır\r\n").unwrap();
        assert_eq!(r.code, 220);
        assert_eq!(r.message(), "hazır");
        assert_eq!(reply("230\r\n").unwrap().message(), "");
    }

    #[test]
    fn multi_line_ignores_other_codes() {
        let mut c = Cursor::new(b"220-Hos geldiniz\r\n 220 girintili\r\n123 baska kod\r\n220-devam\r\n220 son\r\n331 sonraki\r\n".to_vec());
        let r = read_reply(&mut c).unwrap();
        assert_eq!(r.code, 220);
        assert_eq!(r.lines.len(), 5);
        assert_eq!(read_reply(&mut c).unwrap().code, 331);
    }

    #[test]
    fn lf_only_and_invalid() {
        assert_eq!(reply("213 42\n").unwrap().message(), "42");
        assert!(reply("hello\r\n").is_err());
        assert!(reply("").is_err());
        assert!(reply("220-yarım\r\n").is_err());
    }

    #[test]
    fn connect_timeout_is_a_total_budget() {
        // Yönlendirilemeyen dört adres: eski davranışta 4 x 2 sn sürerdi
        let addrs: Vec<SocketAddr> = (1..=4).map(|i| format!("10.255.255.{i}:21").parse().unwrap()).collect();
        let t = std::time::Instant::now();
        assert!(connect_any(&addrs, Duration::from_secs(2)).is_err());
        let took = t.elapsed();
        assert!(took < Duration::from_millis(3500), "{took:?}");
        assert!(matches!(connect_any(&[], Duration::from_secs(1)), Err(None)));
    }
}
