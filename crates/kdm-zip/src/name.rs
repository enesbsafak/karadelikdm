//! Girdi adları: CP437 çözme ve çıkarma yolunun güvenli kurulması (Zip Slip).

use std::path::{Path, PathBuf};

/// IBM CP437'nin 0x80-0xFF yarısı. Alt yarı ASCII ile aynıdır (0x00-0x1F'nin grafik karşılıkları ad
/// içinde anlamsız olduğundan ASCII kontrol karakteri olarak bırakılır; `safe_join` onları reddeder).
const CP437_HIGH: &str = concat!(
    "ÇüéâäàåçêëèïîìÄÅ",
    "ÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
    "áíóúñÑªº¿⌐¬½¼¡«»",
    "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
    "└┴┬├─┼╞╟╚╔╩╦╠═╬╧",
    "╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
    "αßΓπΣσµτΦΘΩδ∞φε∩",
    "≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
);

/// UTF-8 bayrağı (bit 11) olmayan adlar APPNOTE'a göre CP437'dir.
pub fn decode_cp437(raw: &[u8]) -> String {
    raw.iter()
        .map(|&b| if b < 0x80 { b as char } else { CP437_HIGH.chars().nth((b - 0x80) as usize).unwrap_or('?') })
        .collect()
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `base` altına girdi adını ekler. Klasör dışına çıkabilecek ya da Windows'ta başka bir şeye dönüşebilecek
/// her ad reddedilir: `..`, mutlak/UNC yol, sürücü harfi, `:` (alternatif veri akışı), ayrılmış aygıt adları,
/// yalnız nokta ve boşluktan oluşan parçalar (Windows sondaki nokta/boşluğu sildiği için `.. ` → `..`).
/// `/` ve `\` ikisi de ayırıcı sayılır. Boş ve `.` parçaları atlanır.
pub fn safe_join(base: &Path, name: &str) -> Option<PathBuf> {
    if name.starts_with(['/', '\\']) {
        return None;
    }
    let mut out = base.to_path_buf();
    let mut any = false;
    for part in name.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if !valid_component(part) {
            return None;
        }
        out.push(part);
        any = true;
    }
    any.then_some(out)
}

fn valid_component(part: &str) -> bool {
    let trimmed = part.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return false;
    }
    if part.chars().any(|c| c < ' ' || c == '\u{7f}' || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
        return false;
    }
    // Aygıt adları uzantılı da ayrılmıştır ("nul.txt")
    let stem = trimmed.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    !RESERVED.contains(&stem.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437_table_complete_and_decodes() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(decode_cp437(b"caf\x82 \x81ber.txt"), "café über.txt");
        assert_eq!(decode_cp437(&[0x80, 0x9E, 0xE1, 0xFF]), "Ç₧ß\u{a0}");
    }

    #[test]
    fn safe_join_accepts_normal_names() {
        let base = Path::new("hedef");
        assert_eq!(safe_join(base, "a/b.txt"), Some(base.join("a").join("b.txt")));
        assert_eq!(safe_join(base, "a\\b.txt"), Some(base.join("a").join("b.txt")));
        assert_eq!(safe_join(base, "./a//b/"), Some(base.join("a").join("b")));
        assert_eq!(safe_join(base, "türkçe ğüşiöç.txt"), Some(base.join("türkçe ğüşiöç.txt")));
        assert_eq!(safe_join(base, "..a/b..c"), Some(base.join("..a").join("b..c")));
        assert_eq!(safe_join(base, "console.txt"), Some(base.join("console.txt")));
    }

    #[test]
    fn safe_join_rejects_bad_names() {
        let base = Path::new("hedef");
        for bad in [
            "../evil", "a/../../evil", "..\\evil", "/etc/passwd", "\\windows\\x", "\\\\server\\share\\x",
            "C:\\x", "C:x", "a/C:/x", "dosya.txt:gizli", "nul", "NUL.txt", "a/com1.log", "LPT9", ".. ", "...",
            "a/ ./b", "", "/", ".", "./", "a\0b", "a\nb", "a*b", "a?b", "a|b",
        ] {
            assert_eq!(safe_join(base, bad), None, "{bad:?} kabul edilmemeliydi");
        }
    }
}
