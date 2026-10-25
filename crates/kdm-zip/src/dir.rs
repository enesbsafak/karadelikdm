//! Arşiv sonu (EOCD, ZIP64 locator ve record) ve merkezi dizin ayrıştırma (APPNOTE 4.3.12-4.3.16).

use crate::{le16, le32, le64, name, read_exact, Entry, RangeSource};
use anyhow::{anyhow, bail, Result};
use std::borrow::Cow;

const CEN_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const LOC64_SIG: u32 = 0x0706_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const CEN_LEN: usize = 46;
const EOCD_LEN: usize = 22;
const LOC64_LEN: usize = 20;
const EOCD64_LEN: usize = 56;
/// EOCD + en uzun yorum (65535) + hemen önündeki ZIP64 locator. Tek okumada ikisi de gelir.
pub(crate) const TAIL_LEN: u64 = (EOCD_LEN + 0xFFFF + LOC64_LEN) as u64;
/// Bozuk ya da kötü niyetli bir EOCD'nin dev bir ayırma yaptırmaması için üst sınır. 256 MiB, yaklaşık
/// üç milyon girdiye yeter.
const MAX_CD: u64 = 256 << 20;

/// Merkezi dizinin yeri. Klasik EOCD'den ya da ZIP64 record'dan gelir.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Directory {
    pub entries: u64,
    pub size: u64,
    pub offset: u64,
    /// Diskler: çok parçalı arşivler desteklenmez
    pub disk: u32,
    pub cd_disk: u32,
}

/// Sondaki EOCD imzasının `tail` içindeki yeri. Yorumun içinde de "PK\5\6" geçebileceği için önce yorum
/// uzunluğu dosya sonuna tam oturan aday aranır; yoksa (sonda çöp bayt varsa) sona en yakın geçerli aday.
pub(crate) fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < EOCD_LEN {
        return None;
    }
    let mut loose = None;
    for i in (0..=tail.len() - EOCD_LEN).rev() {
        if le32(tail, i) != EOCD_SIG {
            continue;
        }
        let end = i + EOCD_LEN + le16(tail, i + 20) as usize;
        if end == tail.len() {
            return Some(i);
        }
        if end < tail.len() && loose.is_none() {
            loose = Some(i);
        }
    }
    loose
}

/// Klasik EOCD. 0xFFFF / 0xFFFFFFFF değerleri "ZIP64 record'a bak" demektir.
pub(crate) fn parse_eocd(b: &[u8]) -> Directory {
    Directory {
        disk: le16(b, 4) as u32,
        cd_disk: le16(b, 6) as u32,
        entries: le16(b, 10) as u64,
        size: le32(b, 12) as u64,
        offset: le32(b, 16) as u64,
    }
}

fn needs_zip64(d: &Directory) -> bool {
    d.entries == 0xFFFF || d.size == 0xFFFF_FFFF || d.offset == 0xFFFF_FFFF || d.disk == 0xFFFF
}

/// ZIP64 locator: record'un konumu ve toplam disk sayısı.
pub(crate) fn parse_locator(b: &[u8]) -> Option<(u64, u32)> {
    (b.len() >= LOC64_LEN && le32(b, 0) == LOC64_SIG).then(|| (le64(b, 8), le32(b, 16)))
}

/// ZIP64 EOCD record'unun sabit 56 baytı. Sondaki "extensible data" kullanılmaz.
pub(crate) fn parse_eocd64(b: &[u8]) -> Result<Directory> {
    if b.len() < EOCD64_LEN || le32(b, 0) != EOCD64_SIG {
        bail!("ZIP64 merkezi dizin sonu kaydı bulunamadı");
    }
    Ok(Directory {
        disk: le32(b, 16),
        cd_disk: le32(b, 20),
        entries: le64(b, 32),
        size: le64(b, 40),
        offset: le64(b, 48),
    })
}

/// Sondan okunan baytlar ve onların dosyadaki başlangıcı. Başka bir aralık zaten buradaysa yeniden okunmaz.
struct Tail {
    start: u64,
    buf: Vec<u8>,
}

impl Tail {
    fn get<'a>(&'a self, src: &dyn RangeSource, offset: u64, len: u64) -> Result<Cow<'a, [u8]>> {
        let end = offset.checked_add(len).ok_or_else(|| anyhow!("geçersiz aralık"))?;
        if offset >= self.start && end <= self.start + self.buf.len() as u64 {
            let a = (offset - self.start) as usize;
            return Ok(Cow::Borrowed(&self.buf[a..a + len as usize]));
        }
        if end > src.len() {
            bail!("arşiv kesik: {offset}+{len} dosya sonunu aşıyor");
        }
        Ok(Cow::Owned(read_exact(src, offset, len)?))
    }
}

pub(crate) fn list(src: &dyn RangeSource) -> Result<Vec<Entry>> {
    let len = src.len();
    if len < EOCD_LEN as u64 {
        bail!("ZIP değil: dosya çok kısa");
    }
    let tail_len = len.min(TAIL_LEN);
    let tail = Tail { start: len - tail_len, buf: read_exact(src, len - tail_len, tail_len)? };
    let at = find_eocd(&tail.buf).ok_or_else(|| anyhow!("ZIP değil: merkezi dizin sonu (EOCD) bulunamadı"))?;
    let eocd_pos = tail.start + at as u64;
    let mut dir = parse_eocd(&tail.buf[at..]);
    // Merkezi dizinin hemen ardından gelen yapının gerçek konumu: önek kaymasını bulmak için
    let mut dir_end = eocd_pos;

    let locator = at.checked_sub(LOC64_LEN).and_then(|l| parse_locator(&tail.buf[l..at]));
    if let Some((rec_off, disks)) = locator {
        if disks > 1 {
            bail!("çok parçalı (split) arşivler desteklenmiyor");
        }
        let loc_pos = eocd_pos - LOC64_LEN as u64;
        // Önüne veri eklenmiş arşivde (SFX) locator'daki konum kayar; record genelde locator'ın hemen önündedir
        let mut found = None;
        for cand in [rec_off, loc_pos.saturating_sub(EOCD64_LEN as u64)] {
            if cand + EOCD64_LEN as u64 > loc_pos {
                continue;
            }
            let Ok(b) = tail.get(src, cand, EOCD64_LEN as u64) else { continue };
            if let Ok(d) = parse_eocd64(&b) {
                found = Some((cand, d));
                break;
            }
        }
        let (pos, d) = found.ok_or_else(|| anyhow!("ZIP64 merkezi dizin sonu kaydı bulunamadı"))?;
        dir = d;
        dir_end = pos;
    } else if needs_zip64(&dir) {
        bail!("arşiv ZIP64 gerektiriyor ama ZIP64 locator yok");
    }
    if dir.disk != 0 || dir.cd_disk != 0 {
        bail!("çok parçalı (split) arşivler desteklenmiyor");
    }

    // Önek kayması: arşivin önüne veri eklenmişse (SFX) kayıtlı ofsetler gerçek konumdan küçük kalır
    let recorded_end = dir.offset.checked_add(dir.size).ok_or_else(|| anyhow!("merkezi dizin boyutu geçersiz"))?;
    let bias = dir_end
        .checked_sub(recorded_end)
        .ok_or_else(|| anyhow!("merkezi dizin konumu tutarsız (arşiv kesik olabilir)"))?;
    if dir.size > MAX_CD {
        bail!("merkezi dizin çok büyük ({} bayt)", dir.size);
    }
    let cd = tail.get(src, dir.offset + bias, dir.size)?;
    let entries = parse_central(&cd, bias)?;

    let (got, want) = (entries.len() as u64, dir.entries);
    // ZIP64 kullanmayan bazı yazıcılar 65535'i aşan sayıyı 16 bitte taşırır; alt 16 bit tutuyorsa kabul
    if got != want && (got & 0xFFFF) != (want & 0xFFFF) {
        bail!("merkezi dizin bozuk: {want} girdi bekleniyordu, {got} okundu");
    }
    Ok(entries)
}

/// Merkezi dizin kayıtları. `bias` önek kayması: yerel başlık ofsetlerine eklenir.
pub(crate) fn parse_central(cd: &[u8], bias: u64) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut p = 0;
    // Sayıya değil bayta göre yürünür: sondaki dijital imza (0x05054b50) gibi kayıtlarda durur
    while p + CEN_LEN <= cd.len() && le32(cd, p) == CEN_SIG {
        let flags = le16(cd, p + 8);
        let method = le16(cd, p + 10);
        let crc32 = le32(cd, p + 16);
        let mut compressed = le32(cd, p + 20) as u64;
        let mut size = le32(cd, p + 24) as u64;
        let nlen = le16(cd, p + 28) as usize;
        let xlen = le16(cd, p + 30) as usize;
        let clen = le16(cd, p + 32) as usize;
        let disk = le16(cd, p + 34) as u32;
        let ext_attr = le32(cd, p + 38);
        let mut local = le32(cd, p + 42) as u64;
        let name_at = p + CEN_LEN;
        let next = name_at + nlen + xlen + clen;
        if next > cd.len() {
            bail!("merkezi dizin kaydı kesik (girdi {})", out.len());
        }
        let raw_name = &cd[name_at..name_at + nlen];
        let extra = &cd[name_at + nlen..name_at + nlen + xlen];

        let m = u32::MAX as u64;
        let z64 = zip64_extra(extra, size == m, compressed == m, local == m, disk == 0xFFFF)
            .ok_or_else(|| anyhow!("ZIP64 ek alanı eksik ya da kesik (girdi {})", out.len()))?;
        size = z64.0.unwrap_or(size);
        compressed = z64.1.unwrap_or(compressed);
        local = z64.2.unwrap_or(local);

        let name = if flags & 0x0800 != 0 {
            String::from_utf8_lossy(raw_name).into_owned()
        } else {
            unicode_path(extra, raw_name).unwrap_or_else(|| name::decode_cp437(raw_name))
        };
        // Dizinler '/' ile biter; bazı Windows araçları yalnız MS-DOS dizin özniteliğini (0x10) koyar
        let is_dir = name.ends_with('/') || name.ends_with('\\') || (ext_attr & 0x10 != 0 && size == 0);
        out.push(Entry {
            index: out.len(),
            name,
            size,
            compressed,
            method,
            crc32,
            local_offset: local.checked_add(bias).ok_or_else(|| anyhow!("yerel başlık ofseti geçersiz"))?,
            is_dir,
            // bit 0: geleneksel/AES şifreleme, bit 6: güçlü şifreleme
            encrypted: flags & 0x0041 != 0,
        });
        p = next;
    }
    Ok(out)
}

/// Ek alan blokları: (kimlik, veri).
fn extra_fields(extra: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut p = 0;
    std::iter::from_fn(move || {
        if p + 4 > extra.len() {
            return None;
        }
        let id = le16(extra, p);
        let n = le16(extra, p + 2) as usize;
        let data = extra.get(p + 4..p + 4 + n)?;
        p += 4 + n;
        Some((id, data))
    })
}

type Zip64 = (Option<u64>, Option<u64>, Option<u64>);

/// ZIP64 genişletilmiş bilgi (0x0001). Alanlar yalnız klasik değeri 0xFFFFFFFF olanlar için ve sabit sırada
/// bulunur (APPNOTE 4.5.3). Gerekli alan eksikse `None`.
fn zip64_extra(extra: &[u8], need_size: bool, need_comp: bool, need_local: bool, need_disk: bool) -> Option<Zip64> {
    if !(need_size || need_comp || need_local || need_disk) {
        return Some((None, None, None));
    }
    let (_, data) = extra_fields(extra).find(|(id, _)| *id == 0x0001)?;
    let mut p = 0;
    let mut take = |need: bool| -> Option<Option<u64>> {
        if !need {
            return Some(None);
        }
        let v = data.get(p..p + 8).map(|b| le64(b, 0))?;
        p += 8;
        Some(Some(v))
    };
    let size = take(need_size)?;
    let comp = take(need_comp)?;
    let local = take(need_local)?;
    Some((size, comp, local))
}

/// Info-ZIP Unicode Path (0x7075, APPNOTE 4.6.9): UTF-8 bayrağı olmadan yazılmış adın UTF-8 karşılığı.
/// Yalnız içindeki CRC başlıktaki ada uyuyorsa geçerlidir (ad sonradan değiştirildiyse eskimiştir).
fn unicode_path(extra: &[u8], raw_name: &[u8]) -> Option<String> {
    let (_, data) = extra_fields(extra).find(|(id, _)| *id == 0x7075)?;
    if data.len() < 5 || data[0] != 1 || le32(data, 1) != crc32fast::hash(raw_name) {
        return None;
    }
    std::str::from_utf8(&data[5..]).ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eocd(entries: u16, size: u32, offset: u32, comment: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&EOCD_SIG.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&entries.to_le_bytes());
        b.extend_from_slice(&entries.to_le_bytes());
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&offset.to_le_bytes());
        b.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        b.extend_from_slice(comment);
        b
    }

    #[test]
    fn eocd_found_behind_comment_with_fake_signature() {
        let mut tail = vec![0u8; 100];
        let mut comment = b"yorum ".to_vec();
        comment.extend_from_slice(&EOCD_SIG.to_le_bytes());
        comment.extend_from_slice(&[0u8; 30]);
        tail.extend(eocd(3, 150, 7, &comment));
        assert_eq!(find_eocd(&tail), Some(100));
        let d = parse_eocd(&tail[100..]);
        assert_eq!((d.entries, d.size, d.offset), (3, 150, 7));
    }

    #[test]
    fn eocd_with_trailing_garbage_still_found() {
        let mut tail = eocd(1, 46, 0, b"");
        tail.extend_from_slice(b"cop");
        assert_eq!(find_eocd(&tail), Some(0));
        assert_eq!(find_eocd(&tail[..10]), None);
    }

    #[test]
    fn eocd64_record_parsed() {
        let mut b = Vec::new();
        b.extend_from_slice(&EOCD64_SIG.to_le_bytes());
        b.extend_from_slice(&44u64.to_le_bytes()); // kaydın kalan boyu
        b.extend_from_slice(&45u16.to_le_bytes());
        b.extend_from_slice(&45u16.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&70_000u64.to_le_bytes());
        b.extend_from_slice(&70_000u64.to_le_bytes());
        b.extend_from_slice(&5_000_000_000u64.to_le_bytes());
        b.extend_from_slice(&6_000_000_000u64.to_le_bytes());
        assert_eq!(b.len(), EOCD64_LEN);
        let d = parse_eocd64(&b).unwrap();
        assert_eq!((d.entries, d.size, d.offset, d.disk, d.cd_disk), (70_000, 5_000_000_000, 6_000_000_000, 0, 0));
        b[0] = 0;
        assert!(parse_eocd64(&b).is_err());

        let mut loc = Vec::new();
        loc.extend_from_slice(&LOC64_SIG.to_le_bytes());
        loc.extend_from_slice(&0u32.to_le_bytes());
        loc.extend_from_slice(&11_000_000_000u64.to_le_bytes());
        loc.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(parse_locator(&loc), Some((11_000_000_000, 1)));
    }

    #[test]
    fn zip64_extra_reads_only_marked_fields_in_order() {
        let mut x = Vec::new();
        x.extend_from_slice(&0x5455u16.to_le_bytes()); // önce ilgisiz bir blok (zaman damgası)
        x.extend_from_slice(&1u16.to_le_bytes());
        x.push(0);
        x.extend_from_slice(&1u16.to_le_bytes());
        x.extend_from_slice(&16u16.to_le_bytes());
        x.extend_from_slice(&5_000_000_000u64.to_le_bytes());
        x.extend_from_slice(&7_000_000_000u64.to_le_bytes());
        // boyut klasik alanda geçerli, sıkıştırılmış boyut ve ofset ZIP64'te
        assert_eq!(zip64_extra(&x, false, true, true, false), Some((None, Some(5_000_000_000), Some(7_000_000_000))));
        // üç alan isteniyor ama blokta iki tane var
        assert_eq!(zip64_extra(&x, true, true, true, false), None);
        assert_eq!(zip64_extra(&[], false, false, false, false), Some((None, None, None)));
    }
}
