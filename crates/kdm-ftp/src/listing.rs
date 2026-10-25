//! Metin ayrıştırıcılar: PASV / EPSV yanıtları ve dizin listeleri (MLSD, EPLF, UNIX `ls -l`, MS-DOS / IIS).

use crate::Entry;
use std::net::Ipv4Addr;

/// RFC 2428: `229 Entering Extended Passive Mode (|||6446|)`; ayraç `|` olmak zorunda değil.
pub(crate) fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = open + text[open..].find(')')?;
    let inner = &text[open + 1..close];
    let d = inner.chars().next().filter(|c| c.is_ascii_graphic())?;
    let parts: Vec<&str> = inner.split(d).collect();
    if parts.len() != 5 {
        return None;
    }
    parts[3].parse::<u16>().ok().filter(|p| *p != 0)
}

/// PASV yanıtındaki altı sayı; parantez, `=` ya da düz metin biçiminden bağımsız.
/// Önce virgülle ayrılmış altılı aranır, yoksa metindeki ilk altı sayı alınır.
pub(crate) fn parse_pasv(text: &str) -> Option<(Ipv4Addr, u16)> {
    // (değer, başlangıç, bitiş)
    let mut nums: Vec<(u32, usize, usize)> = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let s = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            nums.push((text[s..i].parse().unwrap_or(u32::MAX), s, i));
        } else {
            i += 1;
        }
    }
    let ok = |w: &[(u32, usize, usize)]| w.iter().all(|n| n.0 <= 255);
    let comma = |w: &[(u32, usize, usize)]| w.windows(2).all(|p| text[p[0].2..p[1].1].trim() == ",");
    let six = nums
        .windows(6)
        .find(|w| ok(w) && comma(w))
        .or_else(|| nums.get(..6).filter(|w| ok(w)))?;
    let ip = Ipv4Addr::new(six[0].0 as u8, six[1].0 as u8, six[2].0 as u8, six[3].0 as u8);
    let port = (six[4].0 * 256 + six[5].0) as u16;
    (port != 0).then_some((ip, port))
}

fn keep(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".."
}

/// RFC 3659 §7: `fact=değer;fact=değer; ad`. Ad boşluk içerebilir; gerçekler bitince tek boşluk gelir.
pub(crate) fn parse_mlsd(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((facts, name)) = line.split_once(' ') else { continue };
        let mut kind = String::new();
        let mut size = None;
        for f in facts.split(';') {
            if let Some((k, v)) = f.split_once('=') {
                match k.to_ascii_lowercase().as_str() {
                    "type" => kind = v.to_ascii_lowercase(),
                    "size" => size = v.parse().ok(),
                    _ => {}
                }
            }
        }
        // cdir / pdir: bulunulan ve üst dizin
        if kind == "cdir" || kind == "pdir" || !keep(name) {
            continue;
        }
        let is_dir = kind == "dir";
        out.push(Entry { name: name.to_string(), size: if is_dir { None } else { size }, is_dir });
    }
    out
}

/// LIST çıktısı: satır satır EPLF, UNIX ve MS-DOS denenir; tanınmayan satırlar (`total 12` vb.) atlanır.
pub(crate) fn parse_list(text: &str) -> Vec<Entry> {
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter_map(|l| parse_eplf(l).or_else(|| parse_unix(l)).or_else(|| parse_dos(l)))
        .filter(|e| keep(&e.name))
        .collect()
}

/// Boşlukla ayrılmış parçalar ve satırdaki başlangıç konumları (ad boşluk içerebildiği için konum gerekir).
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.push((s, &line[s..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &line[s..]));
    }
    out
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// EPLF: `+i8388621.48594,m825718503,r,s280,\tdjb.html`
fn parse_eplf(line: &str) -> Option<Entry> {
    let (facts, name) = line.strip_prefix('+')?.split_once('\t')?;
    let mut is_dir = false;
    let mut size = None;
    for f in facts.split(',') {
        match f.as_bytes().first() {
            Some(b'/') => is_dir = true,
            Some(b's') => size = f[1..].parse().ok(),
            _ => {}
        }
    }
    Some(Entry { name: name.to_string(), size: if is_dir { None } else { size }, is_dir })
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn is_month(s: &str) -> bool {
    MONTHS.contains(&s.to_ascii_lowercase().as_str())
}

fn is_hhmm(s: &str) -> bool {
    matches!(s.split_once(':'), Some((h, m)) if digits(h) && digits(m) && h.len() <= 2 && m.len() == 2)
}

/// `2026-10-25` (ls --time-style=long-iso)
fn is_iso_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    p.len() == 3 && p[0].len() == 4 && p.iter().all(|x| digits(x))
}

/// UNIX `ls -l`: izinler, bağlantı sayısı, sahip, [grup], boyut, tarih, ad.
/// Sütun sayısı sunucudan sunucuya değiştiği için boyut, tarihin hemen önündeki sayı olarak bulunur.
fn parse_unix(line: &str) -> Option<Entry> {
    let t = tokens(line);
    let perms = t.first()?.1;
    let kind = perms.chars().next()?;
    if perms.len() < 10 || !"-dlbcps".contains(kind) {
        return None;
    }
    let mut found = None;
    for i in 2..t.len() {
        if !digits(t[i - 1].1) {
            continue;
        }
        // `Oct 25 14:15 ad` ya da `Oct 25 2025 ad`
        if is_month(t[i].1)
            && i + 3 < t.len()
            && digits(t[i + 1].1)
            && t[i + 1].1.len() <= 2
            && (is_hhmm(t[i + 2].1) || (digits(t[i + 2].1) && t[i + 2].1.len() == 4))
        {
            found = Some((i - 1, i + 3));
            break;
        }
        // `2026-10-25 14:15 ad`
        if is_iso_date(t[i].1) && i + 2 < t.len() && is_hhmm(t[i + 1].1) {
            found = Some((i - 1, i + 2));
            break;
        }
    }
    let (size_idx, name_idx) = found?;
    let mut name = &line[t[name_idx].0..];
    if kind == 'l' {
        if let Some((n, _)) = name.split_once(" -> ") {
            name = n;
        }
    }
    let is_dir = kind == 'd';
    let size = if is_dir { None } else { t[size_idx].1.parse().ok() };
    Some(Entry { name: name.to_string(), size, is_dir })
}

/// MS-DOS / IIS: `10-25-26  02:15PM  <DIR>  ad` ya da `10-25-26  02:15PM  12345 ad`.
fn parse_dos(line: &str) -> Option<Entry> {
    let t = tokens(line);
    if t.len() < 4 {
        return None;
    }
    let date: Vec<&str> = t[0].1.split(['-', '/']).collect();
    if date.len() != 3 || !date.iter().all(|x| digits(x)) {
        return None;
    }
    let time = t[1].1.to_ascii_uppercase();
    let clock = time.trim_end_matches("AM").trim_end_matches("PM");
    if !is_hhmm(clock) {
        return None;
    }
    // `02:15 PM` biçimi: AM/PM ayrı parça
    let mut k = 2;
    if matches!(t[k].1.to_ascii_uppercase().as_str(), "AM" | "PM") {
        k += 1;
    }
    if k + 1 >= t.len() {
        return None;
    }
    let name = &line[t[k + 1].0..];
    if t[k].1.eq_ignore_ascii_case("<DIR>") {
        return Some(Entry { name: name.to_string(), size: None, is_dir: true });
    }
    // Bazı sunucular binlik ayraç koyar: `12,345`
    let size: String = t[k].1.chars().filter(|c| *c != ',' && *c != '.').collect();
    if !digits(&size) {
        return None;
    }
    Some(Entry { name: name.to_string(), size: size.parse().ok(), is_dir: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, size: Option<u64>, is_dir: bool) -> Entry {
        Entry { name: name.into(), size, is_dir }
    }

    #[test]
    fn epsv_formats() {
        assert_eq!(parse_epsv("Entering Extended Passive Mode (|||6446|)"), Some(6446));
        assert_eq!(parse_epsv("ok (!!!21000!)"), Some(21000));
        assert_eq!(parse_epsv("Entering Extended Passive Mode"), None);
        assert_eq!(parse_epsv("(|||0|)"), None);
        assert_eq!(parse_epsv("(|||99999|)"), None);
    }

    #[test]
    fn pasv_formats() {
        let want = Some((Ipv4Addr::new(192, 168, 1, 2), 4 * 256 + 1));
        assert_eq!(parse_pasv("Entering Passive Mode (192,168,1,2,4,1)."), want);
        assert_eq!(parse_pasv("Entering Passive Mode 192,168,1,2,4,1"), want);
        assert_eq!(parse_pasv("=192,168,1,2,4,1"), want);
        assert_eq!(parse_pasv("Entering Passive Mode (192, 168, 1, 2, 4, 1)"), want);
        // Önünde başka sayı olan metin: virgüllü altılı tercih edilir
        assert_eq!(parse_pasv("Mode 2 ok (192,168,1,2,4,1) 30s"), want);
        assert_eq!(parse_pasv("(1,2,3,4,5)"), None);
        assert_eq!(parse_pasv("(300,2,3,4,5,6)"), None);
    }

    #[test]
    fn mlsd() {
        let text = "type=cdir;perm=el; .\r\n\
                    type=pdir;perm=el; ..\r\n\
                    type=dir;modify=20261025141500;perm=fle; pub\r\n\
                    Type=File;Size=12345;modify=20261025141500; dosya adı.iso\r\n\
                    type=OS.unix=slink:/x;size=7; link\r\n\
                    bozuk-satir\r\n";
        assert_eq!(
            parse_mlsd(text),
            vec![e("pub", None, true), e("dosya adı.iso", Some(12345), false), e("link", Some(7), false)]
        );
    }

    #[test]
    fn unix_list() {
        let text = "total 24\n\
            drwxr-xr-x    2 ftp      ftp          4096 Oct 25 14:15 pub\n\
            -rw-r--r--    1 ftp      ftp      10485760 Jan  1  2025 big file.bin\n\
            -rw-r--r--    1 owner           42 Mar 03 09:07 nogroup.txt\n\
            lrwxrwxrwx    1 0        0             7 Oct 25 14:15 latest -> pub/x\n\
            -rw-r--r-- 1 user group 99 2026-10-25 14:15 iso.txt\n\
            drwxr-xr-x    2 ftp      ftp          4096 Oct 25 14:15 .\n";
        assert_eq!(
            parse_list(text),
            vec![
                e("pub", None, true),
                e("big file.bin", Some(10485760), false),
                e("nogroup.txt", Some(42), false),
                e("latest", Some(7), false),
                e("iso.txt", Some(99), false),
            ]
        );
    }

    #[test]
    fn dos_list() {
        let text = "10-25-26  02:15PM       <DIR>          aspnet client\r\n\
                    10-25-26  02:15PM                12345 rapor 2026.pdf\r\n\
                    01-02-2026  14:05            1,234,567 buyuk.zip\r\n\
                    10-25-26  09:00 AM               7 ayri.txt\r\n";
        assert_eq!(
            parse_list(text),
            vec![
                e("aspnet client", None, true),
                e("rapor 2026.pdf", Some(12345), false),
                e("buyuk.zip", Some(1234567), false),
                e("ayri.txt", Some(7), false),
            ]
        );
    }

    #[test]
    fn eplf_list() {
        let text = "+i8388621.29609,m824255902,/,\tdev\r\n+i8388621.44468,m839956783,r,s10376,\tRFC959\r\n";
        assert_eq!(parse_list(text), vec![e("dev", None, true), e("RFC959", Some(10376), false)]);
    }
}
