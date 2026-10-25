//! Dosya adı: `Content-Disposition` (RFC 6266, `filename*` için RFC 5987), yoksa URL'nin son parçası.
//! Windows'ta geçersiz karakterler ve ayrılmış adlar temizlenir.

use percent_encoding::percent_decode_str;
use std::path::Path;

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `Content-Disposition` başlığından dosya adı. `filename*` varsa o kullanılır.
pub fn from_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for part in split_params(value).into_iter().skip(1) {
        let Some((k, v)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        if k == "filename*" {
            extended = decode_ext_value(v);
        } else if k == "filename" {
            plain = Some(unquote(v));
        }
    }
    extended.or(plain).map(|n| sanitize(&n)).filter(|n| !n.is_empty())
}

/// `;` ile ayırır ama tırnak içindekileri bölmez.
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut escape = false;
    for c in s.chars() {
        if escape {
            cur.push(c);
            escape = false;
        } else if quoted && c == '\\' {
            cur.push(c);
            escape = true;
        } else if c == '"' {
            quoted = !quoted;
            cur.push(c);
        } else if c == ';' && !quoted {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        // Bazı sunucular RFC'ye aykırı olarak filename içinde yüzde kodlaması kullanır
        if out.contains('%') {
            if let Ok(d) = percent_decode_str(&out).decode_utf8() {
                return d.into_owned();
            }
        }
        out
    } else {
        percent_decode_str(v).decode_utf8_lossy().into_owned()
    }
}

/// RFC 5987: `charset'dil'yüzde-kodlu`
fn decode_ext_value(v: &str) -> Option<String> {
    let v = unquote_simple(v);
    let mut it = v.splitn(3, '\'');
    let charset = it.next()?.to_ascii_lowercase();
    let _lang = it.next()?;
    let enc = it.next()?;
    let bytes: Vec<u8> = percent_decode_str(enc).collect();
    match charset.as_str() {
        "utf-8" | "" => String::from_utf8(bytes).ok(),
        // ISO-8859-1: her bayt aynı kod noktası
        "iso-8859-1" | "latin1" => Some(bytes.iter().map(|&b| b as char).collect()),
        _ => None,
    }
}

fn unquote_simple(v: &str) -> &str {
    v.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(v)
}

/// URL yolunun son parçası (sorgu ve parça hariç).
pub fn from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.rev().find(|s| !s.is_empty())?;
    let name = sanitize(&percent_decode_str(last).decode_utf8_lossy());
    (!name.is_empty()).then_some(name)
}

/// Dosya sistemi için güvenli ad. Yol ayırıcıları da temizlendiği için sunucu `../` ile dışarı yazamaz.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect();
    // Windows sondaki nokta ve boşluğu kabul etmez
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    let s = s.trim_start().to_string();
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let s = if RESERVED.contains(&stem.as_str()) { format!("_{s}") } else { s };
    if s.chars().all(|c| c == '.' || c == '_') && !s.is_empty() {
        return String::new();
    }
    truncate_utf8(&s, 200)
}

fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Uzantıyı koru
    let (stem, ext) = match s.rfind('.') {
        Some(i) if s.len() - i <= 16 => (&s[..i], &s[i..]),
        _ => (s, ""),
    };
    let mut end = max.saturating_sub(ext.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &stem[..end], ext)
}

/// Kullanılmayan bir ad: `ad.zip` doluysa `ad (1).zip`, `ad (2).zip`... Neyin dolu sayılacağına çağıran
/// karar verir (diskteki dosyalar, yarım `.kdm` dosyaları, henüz başlamamış diğer indirmeler).
pub fn unique(name: &str, busy: &dyn Fn(&str) -> bool) -> String {
    if !busy(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|n| !busy(n))
        .unwrap_or_else(|| name.to_string())
}

pub fn extension(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// URL'de uzantı yoksa (ör. `/download?id=5`) yaygın türler için uzantı ekler.
pub fn with_type_extension(name: String, content_type: &str) -> String {
    if !extension(&name).is_empty() {
        return name;
    }
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let ext = match mime.as_str() {
        "application/zip" => "zip",
        "application/pdf" => "pdf",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" | "application/x-rar-compressed" => "rar",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" => "exe",
        "application/x-msi" => "msi",
        "application/vnd.android.package-archive" => "apk",
        "application/x-iso9660-image" => "iso",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        _ => return name,
    };
    format!("{name}.{ext}")
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_variants() {
        assert_eq!(from_content_disposition("attachment; filename=\"a b.zip\"").unwrap(), "a b.zip");
        assert_eq!(from_content_disposition("attachment; filename=plain.txt").unwrap(), "plain.txt");
        assert_eq!(
            from_content_disposition("attachment; filename=\"fallback.txt\"; filename*=UTF-8''%C3%A7i%C3%A7ek.txt").unwrap(),
            "çiçek.txt"
        );
        assert_eq!(from_content_disposition("attachment; filename*=iso-8859-1'en'%E9t%E9.txt").unwrap(), "été.txt");
        assert_eq!(from_content_disposition("inline; filename=\"semi;colon.txt\"").unwrap(), "semi;colon.txt");
        assert_eq!(from_content_disposition("attachment; filename=\"%C5%9Fema.pdf\"").unwrap(), "şema.pdf");
        assert!(from_content_disposition("attachment").is_none());
    }

    #[test]
    fn path_traversal_is_neutralised() {
        assert_eq!(from_content_disposition("attachment; filename=\"../../evil.exe\"").unwrap(), ".._.._evil.exe");
        assert_eq!(sanitize("a/b\\c"), "a_b_c");
    }

    #[test]
    fn url_names() {
        assert_eq!(from_url("https://x.com/dir/file%20name.iso?x=1#f").unwrap(), "file name.iso");
        assert_eq!(from_url("https://x.com/dir/").unwrap(), "dir");
        assert!(from_url("https://x.com/").is_none());
    }

    #[test]
    fn windows_rules() {
        assert_eq!(sanitize("con.txt"), "_con.txt");
        assert_eq!(sanitize("name. . "), "name");
        assert_eq!(sanitize("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize(".."), "");
    }

    #[test]
    fn long_names_keep_extension() {
        let long = format!("{}.mp4", "ş".repeat(300));
        let s = sanitize(&long);
        assert!(s.len() <= 200 && s.ends_with(".mp4"));
    }

    #[test]
    fn unique_names() {
        let taken = ["a.zip", "a (1).zip"];
        assert_eq!(unique("a.zip", &|n| taken.contains(&n)), "a (2).zip");
        assert_eq!(unique("b.zip", &|n| taken.contains(&n)), "b.zip");
        assert_eq!(unique("noext", &|n| n == "noext"), "noext (1)");
        assert_eq!(unique(".hidden", &|n| n == ".hidden"), ".hidden (1)");
    }
}
