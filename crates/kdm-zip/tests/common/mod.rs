//! Test yardımcıları: bellekte ZIP yazıcı, okuma sayan kaynak, geçici klasör.

#![allow(dead_code)]

use kdm_zip::RangeSource;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub struct MemSource {
    pub data: Vec<u8>,
    pub calls: AtomicUsize,
    pub bytes: AtomicU64,
}

impl MemSource {
    pub fn new(data: Vec<u8>) -> Self {
        MemSource { data, calls: AtomicUsize::new(0), bytes: AtomicU64::new(0) }
    }
    pub fn reset(&self) {
        self.calls.store(0, Ordering::SeqCst);
        self.bytes.store(0, Ordering::SeqCst);
    }
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }
}

impl RangeSource for MemSource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&self, offset: u64, len: u64) -> anyhow::Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.bytes.fetch_add(len, Ordering::SeqCst);
        let end = (offset + len).min(self.data.len() as u64);
        anyhow::ensure!(offset <= end, "aralık dışı");
        Ok(self.data[offset as usize..end as usize].to_vec())
    }
}

pub struct Item {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub method: u16,
    pub flags: u16,
    pub crc_override: Option<u32>,
    /// Merkezi dizinde gösterilecek açılmış boyut (bomba testi için)
    pub size_override: Option<u64>,
}

impl Item {
    /// UTF-8 bayraklı ad
    pub fn new(name: &str, data: &[u8], method: u16) -> Self {
        Item { flags: 0x0800, ..Item::raw_name(name.as_bytes(), data, method) }
    }
    /// Bayraksız (CP437) ham ad
    pub fn raw_name(name: &[u8], data: &[u8], method: u16) -> Self {
        Item { name: name.to_vec(), data: data.to_vec(), method, flags: 0, crc_override: None, size_override: None }
    }
}

#[derive(Default)]
pub struct ZipBuilder {
    pub prefix: Vec<u8>,
    pub items: Vec<Item>,
    pub comment: Vec<u8>,
    /// Tüm boyut/ofsetleri ZIP64 ek alanına ve EOCD64 kaydına yazar
    pub zip64: bool,
}

fn compress(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

impl ZipBuilder {
    pub fn add(mut self, item: Item) -> Self {
        self.items.push(item);
        self
    }

    /// APPNOTE'a göre elle yazılır: okuyucunun bağımsız bir doğrulaması için ayrıca `zip` crate'i kullanılır.
    pub fn build(&self) -> Vec<u8> {
        let mut out = self.prefix.clone();
        let base = self.prefix.len() as u64;
        let mut cd = Vec::new();
        for it in &self.items {
            let body = if it.method == 8 { compress(&it.data) } else { it.data.clone() };
            let crc = it.crc_override.unwrap_or_else(|| crc32fast::hash(&it.data));
            let size = it.size_override.unwrap_or(it.data.len() as u64);
            let local = out.len() as u64 - base;
            // Yerel başlık: veri tanımlayıcısı gibi sıfır CRC/boyut, okuyucu merkezi dizine güvenmeli
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&it.flags.to_le_bytes());
            out.extend_from_slice(&it.method.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&[0; 12]);
            out.extend_from_slice(&(it.name.len() as u16).to_le_bytes());
            let local_extra = b"\x99\x99\x03\x00abc"; // bilinmeyen ek alan: atlanmalı
            out.extend_from_slice(&(local_extra.len() as u16).to_le_bytes());
            out.extend_from_slice(&it.name);
            out.extend_from_slice(local_extra);
            out.extend_from_slice(&body);

            let mut extra = Vec::new();
            let (c32, s32, l32) = if self.zip64 {
                extra.extend_from_slice(&1u16.to_le_bytes());
                extra.extend_from_slice(&24u16.to_le_bytes());
                extra.extend_from_slice(&size.to_le_bytes());
                extra.extend_from_slice(&(body.len() as u64).to_le_bytes());
                extra.extend_from_slice(&local.to_le_bytes());
                (u32::MAX, u32::MAX, u32::MAX)
            } else {
                (body.len() as u32, size as u32, local as u32)
            };
            let is_dir = it.name.ends_with(b"/");
            cd.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            cd.extend_from_slice(&45u16.to_le_bytes());
            cd.extend_from_slice(&45u16.to_le_bytes());
            cd.extend_from_slice(&it.flags.to_le_bytes());
            cd.extend_from_slice(&it.method.to_le_bytes());
            cd.extend_from_slice(&[0; 4]);
            cd.extend_from_slice(&crc.to_le_bytes());
            cd.extend_from_slice(&c32.to_le_bytes());
            cd.extend_from_slice(&s32.to_le_bytes());
            cd.extend_from_slice(&(it.name.len() as u16).to_le_bytes());
            cd.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&(if is_dir { 0x10u32 } else { 0x20 }).to_le_bytes());
            cd.extend_from_slice(&l32.to_le_bytes());
            cd.extend_from_slice(&it.name);
            cd.extend_from_slice(&extra);
        }
        let cd_off = out.len() as u64 - base;
        out.extend_from_slice(&cd);
        let n = self.items.len() as u64;
        if self.zip64 {
            let rec = out.len() as u64 - base;
            out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
            out.extend_from_slice(&44u64.to_le_bytes());
            out.extend_from_slice(&45u16.to_le_bytes());
            out.extend_from_slice(&45u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&(cd.len() as u64).to_le_bytes());
            out.extend_from_slice(&cd_off.to_le_bytes());
            out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&rec.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes());
        }
        let (n16, size32, off32) =
            if self.zip64 { (u16::MAX, u32::MAX, u32::MAX) } else { (n as u16, cd.len() as u32, cd_off as u32) };
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&n16.to_le_bytes());
        out.extend_from_slice(&n16.to_le_bytes());
        out.extend_from_slice(&size32.to_le_bytes());
        out.extend_from_slice(&off32.to_le_bytes());
        out.extend_from_slice(&(self.comment.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.comment);
        out
    }
}

/// Basit sözde rastgele (sıkışmayan) veri; dış bağımlılık gerektirmesin diye xorshift.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "kdm-zip-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
