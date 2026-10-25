//! Aktarım katmanı: HTTP(S) ve FTP aynı biçimde (yanıt meta verisi + gövde akışı) motora sunulur.
//! FTP yanıtı HTTP modeline çevrilir: REST ile ortadan başlayan aktarım "206 + Content-Range", baştan başlayan
//! "200"; MDTM, Last-Modified gibi If-Range yerine kullanılır. Böylece parça bölme, geri katma ve "dosya değişti,
//! baştan" mantığı FTP için ayrıca yazılmaz.

use super::http::{self, ByteRange, DigestState, Meta, RequestSpec};
use super::http::FtpTls;
use kdm_ftp::{FtpError, FtpOptions, TlsMode};
use std::io::Read;
use std::time::Duration;

pub(crate) type Body = Box<dyn Read + Send>;

pub(crate) enum Failure {
    /// Geçici: aynı parça yeniden denenir
    Retry(String),
    /// Kalıcı: indirme durur (404, giriş reddedildi...)
    Fatal(String),
    /// Sunucu bağlantıyı reddetti (port kapalı). Hiç veri alınmamış indirmede yanlış adres/port demektir ve
    /// beklemek işe yaramaz; daha önce veri alınmışsa sunucu yeniden başlıyor olabilir, yeniden denenir.
    Refused(String),
}

/// Hata zincirinde "bağlantı reddedildi" var mı (HTTP: ureq, FTP: io).
pub(crate) fn is_refused(e: &anyhow::Error) -> bool {
    let refused = |io: &std::io::Error| io.kind() == std::io::ErrorKind::ConnectionRefused;
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(refused) || matches!(c.downcast_ref::<ureq::Error>(), Some(ureq::Error::Io(io)) if refused(io))
    })
}

/// `ftp://` ve `ftps://`
pub fn is_ftp(url: &str) -> bool {
    url.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("ftp://")) || url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("ftps://"))
}

/// FTP sunucuları IP başına bağlantıyı çoğunlukla sınırlar (tipik 2-4)
pub const FTP_MAX_CONNECTIONS: u32 = 4;

pub(crate) fn open_http(
    agent: &ureq::Agent,
    spec: &RequestSpec,
    range: ByteRange,
    if_range: &str,
    digest: &DigestState,
) -> Result<(Meta, Body), Failure> {
    let r = http::get(agent, spec, Some(range), if_range, digest).map_err(|e| {
        if is_refused(&e) {
            Failure::Refused(format!("{e:#}"))
        } else {
            Failure::Retry(format!("{e:#}"))
        }
    })?;
    Ok((r.meta, Box::new(r.body.into_reader())))
}

fn ftp_options(spec: &RequestSpec, timeout: Duration, tls: FtpTls) -> FtpOptions {
    let (user, password) = spec.auth.as_ref().map(|a| (a.user.clone(), a.password.clone())).unwrap_or_default();
    let tls = match tls {
        FtpTls::Off => TlsMode::Off,
        FtpTls::IfAvailable => TlsMode::IfAvailable,
        FtpTls::Required => TlsMode::Required,
    };
    FtpOptions { user, password, timeout, tls, ..Default::default() }
}

fn ftp_failure(e: anyhow::Error) -> Failure {
    if is_refused(&e) {
        return Failure::Refused(format!("{e:#}"));
    }
    match e.downcast_ref::<FtpError>() {
        // Sertifika ya da reddedilen TLS yeniden denemeyle düzelmez
        Some(FtpError::Login { .. } | FtpError::Certificate { .. } | FtpError::TlsRefused { .. }) => Failure::Fatal(format!("{e:#}")),
        // 5xx kalıcıdır (550 dosya yok, 553 izin yok); 4xx geçici
        Some(FtpError::Reply { code, .. }) if *code >= 500 => Failure::Fatal(format!("{e:#}")),
        _ => Failure::Retry(format!("{e:#}")),
    }
}

/// `validator`: önceki MDTM; farklıysa dosya sunucuda değişmiştir (If-Range'in FTP karşılığı).
pub(crate) fn open_ftp(spec: &RequestSpec, range: ByteRange, validator: &str, timeout: Duration, tls: FtpTls) -> Result<(Meta, Body), Failure> {
    let opts = ftp_options(spec, timeout, tls);
    let (m, reader, start) = match kdm_ftp::open(&spec.url, &opts, range.start) {
        Ok((m, r)) => (m, r, range.start),
        Err(e) if matches!(e.downcast_ref::<FtpError>(), Some(FtpError::RestUnsupported)) => {
            // Sunucu REST desteklemiyor: baştan tam dosya (motor bunu "Range desteklenmiyor" olarak işler)
            let (m, r) = kdm_ftp::open(&spec.url, &opts, 0).map_err(ftp_failure)?;
            (m, r, 0)
        }
        Err(e) => return Err(ftp_failure(e)),
    };
    let mdtm = m.mdtm.clone().unwrap_or_default();
    let changed = start > 0 && !validator.is_empty() && !mdtm.is_empty() && validator != mdtm;
    let partial = start > 0 && !changed;
    let meta = Meta {
        status: if partial { 206 } else { 200 },
        final_url: spec.url.clone(),
        content_length: m.size.map(|s| s.saturating_sub(start)),
        content_range: partial.then(|| (start, m.size.map(|s| s.saturating_sub(1)).unwrap_or(start), m.size)),
        accept_ranges: if m.resumable { "bytes".into() } else { String::new() },
        etag: String::new(),
        last_modified: mdtm,
        content_disposition: String::new(),
        content_type: String::new(),
        content_encoding: String::new(),
    };
    Ok((meta, Box::new(reader)))
}

/// Yoklama (ekleme penceresi) için FTP karşılığı.
pub(crate) fn probe_ftp(spec: &RequestSpec, timeout: Duration, tls: FtpTls) -> anyhow::Result<super::Probe> {
    let m = kdm_ftp::stat(&spec.url, &ftp_options(spec, timeout, tls))?;
    Ok(super::Probe {
        file_name: if m.file_name.is_empty() { "indirme".into() } else { super::filename::sanitize(&m.file_name) },
        size: m.size,
        content_type: String::new(),
        resumable: m.resumable,
        final_url: spec.url.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ftp_and_ftps_schemes() {
        assert!(is_ftp("ftp://h/a") && is_ftp("FTP://h/a") && is_ftp("ftps://h/a") && is_ftp("FTPS://h/a"));
        assert!(!is_ftp("http://h/ftp://") && !is_ftp("ftpx://h") && !is_ftp("ftp:/h") && !is_ftp(""));
    }

    #[test]
    fn tls_errors_are_fatal() {
        let fatal = |e: FtpError| matches!(ftp_failure(e.into()), Failure::Fatal(_));
        assert!(fatal(FtpError::Certificate { message: "x".into() }));
        assert!(fatal(FtpError::TlsRefused { command: "AUTH TLS".into(), code: 534, message: "x".into() }));
    }

    #[test]
    fn tls_setting_reaches_ftp_options() {
        let spec = RequestSpec { url: "ftp://h/a".into(), ..Default::default() };
        let o = ftp_options(&spec, Duration::from_secs(5), FtpTls::Required);
        assert_eq!(o.tls, TlsMode::Required);
        assert_eq!(ftp_options(&spec, Duration::from_secs(5), FtpTls::Off).tls, TlsMode::Off);
    }
}
