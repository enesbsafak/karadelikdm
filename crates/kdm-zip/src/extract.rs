//! Tek girdinin çıkarılması: yerel başlık, veri aralığını parça parça okuma, açma ve CRC32 doğrulama.

use crate::{le16, le32, read_exact, Entry, RangeSource};
use anyhow::{bail, Context, Result};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

const LOC_SIG: u32 = 0x0403_4b50;
const LOC_LEN: u64 = 30;
/// HTTP'de her parça bir istektir: 1 MiB, istek sayısı ile bellek arasında makul bir denge.
const CHUNK: u64 = 1 << 20;

pub(crate) fn extract(src: &dyn RangeSource, entry: &Entry, out: &Path) -> Result<()> {
    if entry.is_dir {
        std::fs::create_dir_all(out).with_context(|| format!("klasör oluşturulamadı: {}", out.display()))?;
        return Ok(());
    }
    if entry.encrypted {
        bail!("'{}' şifreli; şifreli girdiler desteklenmiyor", entry.name);
    }
    if entry.method != 0 && entry.method != 8 {
        bail!("'{}' desteklenmeyen sıkıştırma yöntemi: {} ({})", entry.name, method_name(entry.method), entry.method);
    }
    if entry.method == 0 && entry.compressed != entry.size {
        bail!("'{}' bozuk: sıkıştırılmamış girdinin boyutları uyuşmuyor", entry.name);
    }

    // Yerel başlıktaki ad ve ek alan uzunlukları merkezi dizindekinden farklı olabilir; veri ancak bunlar
    // okununca bulunur
    let hdr = read_exact(src, entry.local_offset, LOC_LEN)?;
    if le32(&hdr, 0) != LOC_SIG {
        bail!("'{}' için yerel başlık bulunamadı (arşiv bozuk)", entry.name);
    }
    let data_start = entry.local_offset + LOC_LEN + le16(&hdr, 26) as u64 + le16(&hdr, 28) as u64;
    if data_start.checked_add(entry.compressed).is_none_or(|end| end > src.len()) {
        bail!("'{}' arşiv sonunu aşıyor (arşiv kesik)", entry.name);
    }

    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("klasör oluşturulamadı: {}", dir.display()))?;
    }
    let part = part_path(out);
    let res = write_entry(src, entry, data_start, &part).and_then(|()| {
        std::fs::rename(&part, out).with_context(|| format!("dosya yeniden adlandırılamadı: {}", out.display()))
    });
    if res.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    res
}

fn part_path(out: &Path) -> PathBuf {
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    out.with_file_name(name)
}

fn write_entry(src: &dyn RangeSource, entry: &Entry, data_start: u64, part: &Path) -> Result<()> {
    let file = std::fs::File::create(part).with_context(|| format!("dosya açılamadı: {}", part.display()))?;
    let mut w = io::BufWriter::new(file);
    let reader = RangeReader { src, pos: data_start, end: data_start + entry.compressed, buf: Vec::new(), at: 0 };
    let mut hasher = crc32fast::Hasher::new();
    // Bildirilen boyuttan bir bayt fazlası okunur: merkezi dizin boyutu küçük gösteren bir arşiv (zip bombası)
    // diski doldurmadan yakalanır
    let limit = entry.size.saturating_add(1);
    let written = if entry.method == 0 {
        copy(reader.take(limit), &mut w, &mut hasher)
    } else {
        copy(flate2::bufread::DeflateDecoder::new(reader).take(limit), &mut w, &mut hasher)
    }
    .with_context(|| format!("'{}' açılamadı", entry.name))?;
    if written != entry.size {
        bail!("'{}' bozuk: {} bayt bekleniyordu, {written} bayt çıktı", entry.name, entry.size);
    }
    let crc = hasher.finalize();
    if crc != entry.crc32 {
        bail!("'{}' CRC32 uyuşmuyor: beklenen {:08x}, hesaplanan {crc:08x}", entry.name, entry.crc32);
    }
    w.flush()?;
    w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

fn copy(mut r: impl Read, w: &mut impl Write, hasher: &mut crc32fast::Hasher) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        w.write_all(&buf[..n])?;
        total += n as u64;
    }
}

/// Sıkıştırılmış aralığı `CHUNK`'lık `read_at` çağrılarıyla akıtır. `BufRead` olduğu için deflate çözücü
/// aralığın dışına taşmaz.
struct RangeReader<'a> {
    src: &'a dyn RangeSource,
    pos: u64,
    end: u64,
    buf: Vec<u8>,
    at: usize,
}

impl BufRead for RangeReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.at >= self.buf.len() && self.pos < self.end {
            let n = (self.end - self.pos).min(CHUNK);
            self.buf = read_exact(self.src, self.pos, n).map_err(io::Error::other)?;
            self.pos += n;
            self.at = 0;
        }
        Ok(&self.buf[self.at.min(self.buf.len())..])
    }

    fn consume(&mut self, amt: usize) {
        self.at += amt;
    }
}

impl Read for RangeReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let avail = self.fill_buf()?;
        let n = avail.len().min(out.len());
        out[..n].copy_from_slice(&avail[..n]);
        self.consume(n);
        Ok(n)
    }
}

fn method_name(m: u16) -> &'static str {
    match m {
        1 => "Shrink",
        2..=5 => "Reduce",
        6 => "Implode",
        9 => "Deflate64",
        12 => "BZIP2",
        14 => "LZMA",
        93 => "Zstandard",
        95 => "XZ",
        96 => "JPEG",
        97 => "WavPack",
        98 => "PPMd",
        99 => "AES",
        _ => "bilinmiyor",
    }
}
