//! Joker karakterli desenler: `*` herhangi bir dizi (boş da olur), `?` tek karakter.

/// Büyük/küçük harf duyarsız tam eşleşme.
pub(crate) fn wildcard_ci(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    wildcard(&p, &t)
}

/// Doğrusal geri izlemeli eşleştirme: son `*`'a dönülür, üstel patlama olmaz.
pub(crate) fn wildcard<T: PartialEq + Copy + From<u8>>(p: &[T], t: &[T]) -> bool {
    let star = T::from(b'*');
    let any = T::from(b'?');
    let (mut pi, mut ti) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == star {
            back = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && (p[pi] == any || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((bp, bt)) = back {
            pi = bp + 1;
            ti = bt + 1;
            back = Some((bp, bt + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == star)
}

/// URL yolu deseni. Joker içermeyen desen önek sayılır (`/docs/` → `/docs/` altındaki her şey); jokerli
/// desen yolun tamamına uymalıdır.
pub(crate) fn path_matches(pat: &str, path: &str) -> bool {
    if pat.contains(['*', '?']) {
        wildcard_ci(pat, path)
    } else {
        path.to_lowercase().starts_with(&pat.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_ci("*.zip", "Setup.ZIP"));
        assert!(wildcard_ci("*", ""));
        assert!(wildcard_ci("a*b*c", "aXXbYYc"));
        assert!(!wildcard_ci("a*b*c", "aXXbYY"));
        assert!(wildcard_ci("file?.pdf", "file1.pdf"));
        assert!(!wildcard_ci("file?.pdf", "file10.pdf"));
        assert!(wildcard_ci("*.tar.gz", "x.tar.gz"));
        assert!(!wildcard_ci("*.zip", "x.zip.exe"));
        assert!(wildcard_ci("İ*", "i̇stanbul"));
    }

    #[test]
    fn path_prefix_and_wildcard() {
        assert!(path_matches("/docs/", "/Docs/a/b.pdf"));
        assert!(!path_matches("/docs/", "/blog/docs/"));
        assert!(path_matches("*/private/*", "/a/private/x"));
        assert!(!path_matches("/a/*.pdf", "/a/b.zip"));
    }
}
