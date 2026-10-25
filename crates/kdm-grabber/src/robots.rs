//! robots.txt (RFC 9309). Grup seçimi ürün belirteciyle, kural seçimi en uzun eşleşmeyle yapılır.

use crate::pattern::wildcard;
use crate::urls::norm_pct;

/// RFC 9309 2.5: en az 500 KiB ayrıştırılmalı; ötesi yok sayılabilir.
pub(crate) const MAX_BYTES: usize = 500 * 1024;

/// (ajan belirteçleri, kurallar)
type Group = (Vec<String>, Vec<(bool, String)>);

#[derive(Debug, Default)]
pub(crate) struct Robots {
    /// (izin mi, normalleştirilmiş desen)
    rules: Vec<(bool, String)>,
}

impl Robots {
    /// Kural yok: her şeye izin (robots.txt yoksa ya da alınamadıysa).
    pub(crate) fn allow_all() -> Self {
        Robots::default()
    }

    pub(crate) fn parse(body: &[u8], token: &str) -> Self {
        let body = String::from_utf8_lossy(&body[..body.len().min(MAX_BYTES)]);
        let token = product_token(token);
        // Ardışık user-agent satırları tek grup açar
        let mut groups: Vec<Group> = Vec::new();
        let mut in_rules = true;
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("");
            let Some((key, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "user-agent" => {
                    if in_rules {
                        groups.push((Vec::new(), Vec::new()));
                        in_rules = false;
                    }
                    groups.last_mut().unwrap().0.push(product_token(value));
                }
                k @ ("allow" | "disallow") => {
                    in_rules = true;
                    // Gruptan önceki kurallar sahipsizdir; boş değer kural değildir ("Disallow:" = her şeye izin)
                    if let (Some(g), false) = (groups.last_mut(), value.is_empty()) {
                        g.1.push((k == "allow", norm_pct(value)));
                    }
                }
                _ => {}
            }
        }
        // Belirtece uyan bütün gruplar birleştirilir; hiçbiri uymuyorsa `*` grupları
        let pick = |want: &str| -> Vec<(bool, String)> {
            groups.iter().filter(|g| g.0.iter().any(|a| a == want)).flat_map(|g| g.1.iter().cloned()).collect()
        };
        let own = !token.is_empty() && groups.iter().any(|g| g.0.contains(&token));
        Robots { rules: pick(if own { &token } else { "*" }) }
    }

    /// `path`: yol ve varsa `?sorgu`, yüzde kodlanmış hâliyle.
    pub(crate) fn allowed(&self, path: &str) -> bool {
        if path == "/robots.txt" {
            return true;
        }
        let path = norm_pct(path);
        let mut best: Option<(usize, bool)> = None;
        for (allow, pat) in &self.rules {
            if !matches(pat, &path) {
                continue;
            }
            // En uzun desen kazanır; eşitlikte izin (RFC 9309 2.2.2)
            let len = pat.len();
            if best.is_none_or(|(l, a)| len > l || (len == l && *allow && !a)) {
                best = Some((len, *allow));
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// Ürün belirteci: ilk boşluğa ya da `/`'a kadar, küçük harfle ("KaradelikDM/1.0" → "karadelikdm").
fn product_token(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase()
}

/// Desen yolun başından eşleşir; `*` herhangi bir dizi, sondaki `$` yolun sonu. Harf duyarlıdır.
fn matches(pat: &str, path: &str) -> bool {
    let (body, anchored) = match pat.strip_suffix('$') {
        Some(b) => (b, true),
        None => (pat, false),
    };
    // `?` robots.txt'de joker değildir; eşleştiricide joker olduğu için iki tarafta da 0'a çevrilir
    let mut p: Vec<u8> = body.bytes().map(|b| if b == b'?' { 0 } else { b }).collect();
    if !anchored {
        p.push(b'*');
    }
    let t: Vec<u8> = path.bytes().map(|b| if b == b'?' { 0 } else { b }).collect();
    wildcard(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXT: &str = "\
# örnek
User-agent: *
Disallow: /private/
Allow: /private/public
Disallow: /*.pdf$
Disallow: /tmp

User-agent: KaradelikDM
User-agent: other
Disallow: /only-us/
Allow: /

User-agent: karadelikdm
Disallow: /also-us/  # ikinci grup birleşir

Sitemap: https://example.com/sitemap.xml
";

    #[test]
    fn star_group_rules() {
        let r = Robots::parse(TXT.as_bytes(), "SomeBot/2.0");
        assert!(r.allowed("/"));
        assert!(!r.allowed("/private/x"));
        assert!(r.allowed("/private/public/x"));
        assert!(!r.allowed("/a/b.pdf"));
        assert!(r.allowed("/a/b.pdf?x=1"));
        assert!(!r.allowed("/tmp"));
        assert!(!r.allowed("/tmpfile"));
        assert!(r.allowed("/robots.txt"));
    }

    #[test]
    fn own_group_merged_and_star_ignored() {
        let r = Robots::parse(TXT.as_bytes(), "KaradelikDM/0.1");
        assert!(!r.allowed("/only-us/a"));
        assert!(!r.allowed("/also-us/a"));
        // kendi grubumuz varken `*` grubu uygulanmaz
        assert!(r.allowed("/private/x"));
    }

    #[test]
    fn longest_match_and_tie_goes_to_allow() {
        let r = Robots::parse(b"User-agent: *\nDisallow: /a\nAllow: /a\nDisallow: /a/b/\nAllow: /a/b/c", "x");
        assert!(r.allowed("/a"));
        assert!(!r.allowed("/a/b/x"));
        assert!(r.allowed("/a/b/cx"));
    }

    #[test]
    fn empty_disallow_percent_and_query() {
        let r = Robots::parse(b"User-agent: *\nDisallow:\n", "x");
        assert!(r.allowed("/anything"));
        let r = Robots::parse(b"user-agent: *\ndisallow: /%7euser/\ndisallow: /search?q=\ndisallow: /\xc3\xbc", "x");
        assert!(!r.allowed("/~user/a"));
        assert!(!r.allowed("/search?q=abc"));
        assert!(r.allowed("/search"));
        assert!(!r.allowed("/%C3%BCber"));
        assert!(Robots::parse(b"", "x").allowed("/x"));
        // Grubu olmayan kural sahipsizdir
        assert!(Robots::parse(b"Disallow: /\n", "x").allowed("/x"));
    }
}
