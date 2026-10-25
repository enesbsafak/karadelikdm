//! HTTP(S) istekleri: ajan yapılandırması, Range istekleri, yanıt meta verisi, Basic ve Digest kimlik doğrulama.

use super::model::Auth;
use anyhow::{anyhow, Result};
use base64::Engine as _;
use md5::{Digest as _, Md5};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::time::Duration;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::{Agent, Body, ResponseExt};

pub const DEFAULT_UA: &str = concat!("KaradelikDM/", env!("CARGO_PKG_VERSION"));

/// Ağ ayarları (Tercihler > Bağlantı).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NetConfig {
    /// `http://host:port`, `socks5://user:pass@host:port`; boş: doğrudan
    pub proxy: String,
    /// Boşsa `DEFAULT_UA`
    pub user_agent: String,
    /// sn, 0: varsayılan
    pub timeout: u64,
    /// `ftp://` adreslerinde açık TLS (AUTH TLS); `ftps://` her zaman örtük TLS'tir
    pub ftp_tls: FtpTls,
}

/// Kapalı varsayılandır: "varsa" kipinde öz imzalı sertifikalı sunucular (FTP'de yaygın) düz FTP'ye düşmek
/// yerine hata verir; kullanıcı bilerek açar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FtpTls {
    #[default]
    Off,
    IfAvailable,
    Required,
}

pub fn agent(cfg: &NetConfig) -> Result<Agent> {
    let timeout = Duration::from_secs(if cfg.timeout == 0 { 30 } else { cfg.timeout });
    let ua = if cfg.user_agent.is_empty() { DEFAULT_UA } else { &cfg.user_agent };
    let mut b = Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(10)
        .timeout_connect(Some(timeout))
        .timeout_recv_response(Some(timeout))
        // Takılan bağlantıyı yakalar: bu süre boyunca hiç veri gelmezse okuma hata verir ve parça yeniden istenir
        .timeout_per_read(Some(timeout))
        .user_agent(ua)
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build());
    if !cfg.proxy.trim().is_empty() {
        let p = ureq::Proxy::new(cfg.proxy.trim()).map_err(|e| anyhow!("vekil sunucu adresi geçersiz: {e}"))?;
        b = b.proxy(Some(p));
    }
    Ok(b.build().new_agent())
}

/// Bir isteğin ortak başlıkları.
#[derive(Clone, Debug, Default)]
pub struct RequestSpec {
    pub url: String,
    pub referer: String,
    pub cookies: String,
    pub user_agent: String,
    pub headers: BTreeMap<String, String>,
    pub auth: Option<Auth>,
}

/// Range isteği: `start-` ya da `start-end` (dahil).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: Option<u64>,
}

impl ByteRange {
    fn header(&self) -> String {
        match self.end {
            Some(e) => format!("bytes={}-{}", self.start, e),
            None => format!("bytes={}-", self.start),
        }
    }
}

/// Yanıttan motorun ihtiyaç duyduğu bilgiler.
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub status: u16,
    pub final_url: String,
    pub content_length: Option<u64>,
    /// (başlangıç, bitiş, toplam)
    pub content_range: Option<(u64, u64, Option<u64>)>,
    pub accept_ranges: String,
    pub etag: String,
    pub last_modified: String,
    pub content_disposition: String,
    pub content_type: String,
    pub content_encoding: String,
}

impl Meta {
    /// Bilinen toplam boyut.
    pub fn total_size(&self) -> Option<u64> {
        match (self.status, self.content_range) {
            (206, Some((_, _, t))) => t,
            (200, _) if self.content_encoding.is_empty() || self.content_encoding == "identity" => self.content_length,
            _ => None,
        }
    }
}

/// Digest kimlik doğrulama durumu (bir indirmenin bağlantıları paylaşır; `nc` her istekte artar).
#[derive(Default)]
pub struct DigestState {
    inner: Mutex<Option<Challenge>>,
}

#[derive(Clone, Debug, Default)]
struct Challenge {
    realm: String,
    nonce: String,
    opaque: String,
    qop: String,
    algorithm: String,
    nc: u32,
}

pub struct Response {
    pub meta: Meta,
    pub body: Body,
}

/// İsteği gönderir. 401 + Digest/Basic meydan okumasında kimlik bilgisi varsa bir kez yeniden dener.
pub fn get(agent: &Agent, spec: &RequestSpec, range: Option<ByteRange>, if_range: &str, digest: &DigestState) -> Result<Response> {
    let mut retried = false;
    loop {
        let mut req = agent.get(&spec.url).header("Accept", "*/*").header("Accept-Encoding", "identity");
        if !spec.user_agent.is_empty() {
            req = req.header("User-Agent", &spec.user_agent);
        }
        if !spec.referer.is_empty() {
            req = req.header("Referer", &spec.referer);
        }
        if !spec.cookies.is_empty() {
            req = req.header("Cookie", &spec.cookies);
        }
        for (k, v) in &spec.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(r) = range {
            req = req.header("Range", r.header());
            if !if_range.is_empty() {
                req = req.header("If-Range", if_range);
            }
        }
        if let Some(a) = &spec.auth {
            if let Some(h) = digest_header(digest, a, &spec.url) {
                req = req.header("Authorization", h);
            } else if !a.user.is_empty() {
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", a.user, a.password));
                req = req.header("Authorization", format!("Basic {token}"));
            }
        }
        // Tür korunur: motor "bağlantı reddedildi"yi ayırt eder
        let resp = req.call().map_err(anyhow::Error::new)?;
        let meta = meta_of(&resp);
        if meta.status == 401 && !retried && spec.auth.is_some() {
            let www = resp.headers().get_all("www-authenticate").iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>();
            if let Some(ch) = www.iter().find_map(|v| parse_digest(v)) {
                *digest.inner.lock() = Some(ch);
                retried = true;
                continue;
            }
        }
        return Ok(Response { meta, body: resp.into_body() });
    }
}

fn header(resp: &ureq::http::Response<Body>, name: &str) -> String {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_string()
}

fn meta_of(resp: &ureq::http::Response<Body>) -> Meta {
    Meta {
        status: resp.status().as_u16(),
        final_url: resp.get_uri().to_string(),
        content_length: header(resp, "content-length").parse().ok(),
        content_range: parse_content_range(&header(resp, "content-range")),
        accept_ranges: header(resp, "accept-ranges").to_ascii_lowercase(),
        etag: header(resp, "etag"),
        last_modified: header(resp, "last-modified"),
        content_disposition: header(resp, "content-disposition"),
        content_type: header(resp, "content-type"),
        content_encoding: header(resp, "content-encoding").to_ascii_lowercase(),
    }
}

/// `bytes 0-499/1234`, `bytes 0-499/*`
pub fn parse_content_range(v: &str) -> Option<(u64, u64, Option<u64>)> {
    let v = v.trim();
    let rest = v.strip_prefix("bytes")?.trim_start();
    let (span, total) = rest.split_once('/')?;
    let (a, b) = span.trim().split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?, total))
}

fn parse_digest(v: &str) -> Option<Challenge> {
    let rest = v.trim().strip_prefix("Digest").or_else(|| v.trim().strip_prefix("digest"))?;
    let mut c = Challenge::default();
    for (k, val) in parse_auth_params(rest) {
        match k.as_str() {
            "realm" => c.realm = val,
            "nonce" => c.nonce = val,
            "opaque" => c.opaque = val,
            // "auth,auth-int": yalnız gövdesiz GET yaptığımız için auth yeterli
            "qop" => c.qop = if val.split(',').any(|q| q.trim() == "auth") { "auth".into() } else { String::new() },
            "algorithm" => c.algorithm = val,
            _ => {}
        }
    }
    (!c.nonce.is_empty()).then_some(c)
}

fn parse_auth_params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',')).collect();
        if key.trim().is_empty() {
            break;
        }
        if chars.next() != Some('=') {
            continue;
        }
        let val = if chars.peek() == Some(&'"') {
            chars.next();
            let mut v = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => v.extend(chars.next()),
                    '"' => break,
                    c => v.push(c),
                }
            }
            v
        } else {
            std::iter::from_fn(|| chars.next_if(|c| *c != ',')).collect::<String>().trim().to_string()
        };
        out.push((key.trim().to_ascii_lowercase(), val));
    }
    out
}

fn md5_hex(s: &str) -> String {
    hex::encode(Md5::digest(s.as_bytes()))
}

fn digest_header(state: &DigestState, auth: &Auth, url: &str) -> Option<String> {
    let mut guard = state.inner.lock();
    let c = guard.as_mut()?;
    c.nc += 1;
    let uri = url::Url::parse(url).ok().map(|u| {
        let mut p = u.path().to_string();
        if let Some(q) = u.query() {
            p.push('?');
            p.push_str(q);
        }
        p
    })?;
    let cnonce = format!("{:016x}", rand_u64());
    let nc = format!("{:08x}", c.nc);
    let mut ha1 = md5_hex(&format!("{}:{}:{}", auth.user, c.realm, auth.password));
    if c.algorithm.eq_ignore_ascii_case("MD5-sess") {
        ha1 = md5_hex(&format!("{ha1}:{}:{cnonce}", c.nonce));
    }
    let ha2 = md5_hex(&format!("GET:{uri}"));
    let response = if c.qop.is_empty() {
        md5_hex(&format!("{ha1}:{}:{ha2}", c.nonce))
    } else {
        md5_hex(&format!("{ha1}:{}:{nc}:{cnonce}:{}:{ha2}", c.nonce, c.qop))
    };
    let mut h = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{response}\"",
        auth.user, c.realm, c.nonce
    );
    if !c.algorithm.is_empty() {
        h.push_str(&format!(", algorithm={}", c.algorithm));
    }
    if !c.qop.is_empty() {
        h.push_str(&format!(", qop={}, nc={nc}, cnonce=\"{cnonce}\"", c.qop));
    }
    if !c.opaque.is_empty() {
        h.push_str(&format!(", opaque=\"{}\"", c.opaque));
    }
    Some(h)
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range() {
        assert_eq!(parse_content_range("bytes 0-499/1234"), Some((0, 499, Some(1234))));
        assert_eq!(parse_content_range("bytes 500-999/*"), Some((500, 999, None)));
        assert_eq!(parse_content_range("bytes */1234"), None);
        assert_eq!(parse_content_range(""), None);
    }

    #[test]
    fn digest_challenge() {
        let c = parse_digest(r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b", opaque="5ccc""#).unwrap();
        assert_eq!(c.realm, "testrealm@host.com");
        assert_eq!(c.qop, "auth");
        assert_eq!(c.nonce, "dcd98b");
        assert_eq!(c.opaque, "5ccc");
        assert!(parse_digest("Basic realm=\"x\"").is_none());
    }

    #[test]
    fn digest_rfc2617_example() {
        // RFC 2617 3.5 örneği (cnonce sabitlenemediği için yalnız qop'suz yolu doğruluyoruz)
        let state = DigestState::default();
        *state.inner.lock() = Some(Challenge { realm: "testrealm@host.com".into(), nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(), ..Default::default() });
        let h = digest_header(&state, &Auth { user: "Mufasa".into(), password: "Circle Of Life".into() }, "http://h/dir/index.html").unwrap();
        let ha1 = md5_hex("Mufasa:testrealm@host.com:Circle Of Life");
        let ha2 = md5_hex("GET:/dir/index.html");
        let expect = md5_hex(&format!("{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:{ha2}"));
        assert!(h.contains(&format!("response=\"{expect}\"")), "{h}");
    }
}
