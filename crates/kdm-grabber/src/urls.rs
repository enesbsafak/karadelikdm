//! URL normalleştirme ve sınıflandırma. Aynı kaynağın farklı yazılışları (`%7e`/`~`, `#bölüm`, boş sorgu)
//! tek anahtara iner; böylece tekrarlar ve döngüler yakalanır.

use url::Url;

/// Takip edilen şemalar. `ftp` yalnız dosya olarak toplanır, sayfa olarak açılmaz.
pub(crate) fn supported(u: &Url) -> bool {
    matches!(u.scheme(), "http" | "https" | "ftp" | "ftps") && u.host_str().is_some()
}

/// Ana makine ve şema `url` crate'inde zaten küçük harfe iner, varsayılan port düşer, `.`/`..` çözülür.
/// Ek olarak parça atılır, boş sorgu silinir ve yüzde kodlaması RFC 3986 6.2.2'ye göre düzenlenir.
pub(crate) fn normalize(mut u: Url) -> Option<String> {
    if !supported(&u) {
        return None;
    }
    u.set_fragment(None);
    let path = norm_pct(u.path());
    u.set_path(&path);
    match u.query().map(norm_pct) {
        Some(q) if q.is_empty() => u.set_query(None),
        Some(q) => u.set_query(Some(&q)),
        None => {}
    }
    Some(u.into())
}

/// `%xx` onaltılık harfleri büyütür, ayrılmamış karakterleri (harf, rakam, `-._~`) çözer.
pub(crate) fn norm_pct(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (b.get(i + 1).and_then(hex), b.get(i + 2).and_then(hex)) {
                let v = h << 4 | l;
                if v.is_ascii_alphanumeric() || matches!(v, b'-' | b'.' | b'_' | b'~') {
                    out.push(v as char);
                } else {
                    out.push('%');
                    out.push(HEX[(v >> 4) as usize] as char);
                    out.push(HEX[(v & 15) as usize] as char);
                }
                i += 3;
                continue;
            }
        }
        // Girdi `url` crate'inden geliyorsa ASCII'dir; değilse (robots.txt) çok baytlı karakterler kodlanır
        let c = s[i..].chars().next().unwrap();
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 15) as usize] as char);
            }
        }
        i += c.len_utf8();
    }
    out
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn hex(c: &u8) -> Option<u8> {
    (*c as char).to_digit(16).map(|d| d as u8)
}

/// Yüzde kodlamasını çözer (eşleştirme için; geçersiz UTF-8 yerine U+FFFD).
pub(crate) fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (b.get(i + 1).and_then(hex), b.get(i + 2).and_then(hex)) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "Aynı site": `www.` öneki dışında aynı ana makine. Kamu soneki listesi (PSL) olmadan alt alan adlarını
/// birleştirmek yanlış eşleşmeye yol açar (`a.github.io` ile `b.github.io` farklı sitelerdir).
pub(crate) fn site_key(u: &Url) -> String {
    let host = u.host_str().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

/// Yolun son parçası, çözülmüş hâliyle.
pub(crate) fn file_name(u: &Url) -> String {
    decode(u.path().rsplit('/').next().unwrap_or(""))
}

const PAGE_EXTS: &[&str] = &[
    "htm", "html", "xhtml", "shtml", "shtm", "php", "php3", "php4", "php5", "phtml", "asp", "aspx", "jsp", "jspx",
    "cfm", "cgi", "pl", "do", "action",
];

/// Uzantısız ya da sunucu tarafı betik uzantılı adresler sayfa olabilir; diğerleri (zip, pdf, jpg...) dosyadır
/// ve indirilmeden toplanır. Gerçek tür sayfa getirilince Content-Type ile kesinleşir.
pub(crate) fn looks_like_page(u: &Url) -> bool {
    let name = file_name(u);
    match name.rsplit_once('.') {
        None => true,
        Some((_, ext)) => PAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Option<String> {
        normalize(Url::parse(s).unwrap())
    }

    #[test]
    fn normalizes_equivalent_forms() {
        let want = Some("http://example.com/a/~b/c%2F?x=%C3%BC".to_owned());
        assert_eq!(n("HTTP://Example.COM:80/a/./x/../%7eb/c%2f?x=%c3%bc#frag"), want);
        assert_eq!(n("http://example.com/a/~b/c%2F?x=%C3%BC"), want);
        assert_eq!(n("https://example.com/?"), Some("https://example.com/".to_owned()));
        assert_eq!(n("https://example.com"), Some("https://example.com/".to_owned()));
        assert_eq!(n("mailto:a@b.c"), None);
        assert_eq!(n("data:text/plain,x"), None);
        assert_eq!(n("ftp://example.com/f.zip"), Some("ftp://example.com/f.zip".to_owned()));
    }

    #[test]
    fn classifies_pages_and_files() {
        let u = |s| Url::parse(s).unwrap();
        assert!(looks_like_page(&u("http://a.com/")));
        assert!(looks_like_page(&u("http://a.com/docs")));
        assert!(looks_like_page(&u("http://a.com/index.PHP?id=3")));
        assert!(!looks_like_page(&u("http://a.com/f.zip")));
        assert!(!looks_like_page(&u("http://a.com/foto.JPG")));
        assert_eq!(file_name(&u("http://a.com/d/%C3%BC%20x.pdf")), "ü x.pdf");
        assert_eq!(site_key(&u("http://www.a.com/")), site_key(&u("https://a.com/x")));
        assert_ne!(site_key(&u("http://a.com/")), site_key(&u("http://b.a.com/")));
    }

    #[test]
    fn pct_normalization_encodes_non_ascii() {
        assert_eq!(norm_pct("/ü%7e%zz"), "/%C3%BC~%zz");
    }
}
