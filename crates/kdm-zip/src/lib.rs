//! Uzak ZIP önizleme ve seçmeli çıkarma. Ağa kendisi çıkmaz: baytlar çağıranın `RangeSource`'undan gelir
//! (HTTP Range, yerel dosya ya da bellek).
//!
//! Başlıklar APPNOTE.TXT'ye göre elle ayrıştırılır. Hazır ZIP okuyucuları arşive `Seek` ile dilediği gibi
//! erişir; bizde her erişim bir HTTP isteği olduğundan okuma sayısını kendimiz denetlemeliyiz: listeleme
//! sondan tek bir 64 KiB okuma ve merkezi dizin için bir okumayla biter.

mod dir;
mod extract;
mod name;

pub use name::{decode_cp437, safe_join};

/// Rastgele erişimli bayt kaynağı. HTTP'de her `read_at` bir Range isteğidir.
pub trait RangeSource: Send + Sync {
    fn len(&self) -> u64;
    /// `offset`'ten `len` bayt. Daha az bayt dönerse çağrı hata sayılır.
    fn read_at(&self, offset: u64, len: u64) -> anyhow::Result<Vec<u8>>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Merkezi dizindeki bir girdi. Boyutlar ve CRC merkezi dizinden gelir: yerel başlıktakiler veri
/// tanımlayıcısı (bit 3) kullanıldığında sıfırdır.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Merkezi dizindeki sıra
    pub index: usize,
    pub name: String,
    /// Açılmış boyut
    pub size: u64,
    pub compressed: u64,
    /// 0: stored, 8: deflate; diğerleri listelenir ama çıkarılamaz
    pub method: u16,
    pub crc32: u32,
    /// Yerel başlığın dosyadaki konumu (arşivin önüne eklenmiş veri varsa düzeltilmiş)
    pub local_offset: u64,
    pub is_dir: bool,
    pub encrypted: bool,
}

/// Merkezi dizini okur. Sondan EOCD'yi, gerekiyorsa ZIP64 locator ve record'u bulur.
pub fn list(src: &dyn RangeSource) -> anyhow::Result<Vec<Entry>> {
    dir::list(src)
}

/// Tek girdiyi `out`'a çıkarır. Önce `out.part`'a yazar, CRC ve boyut tutarsa yerine taşır; böylece yarım
/// ya da bozuk dosya hedef adla kalmaz. `out` güvenli olmalı: çağıran [`safe_join`] kullanmalı.
pub fn extract(src: &dyn RangeSource, entry: &Entry, out: &std::path::Path) -> anyhow::Result<()> {
    extract::extract(src, entry, out)
}

/// `read_at` sonucunu doğrular: eksik veri sessizce yanlış ayrıştırmaya yol açmasın.
fn read_exact(src: &dyn RangeSource, offset: u64, len: u64) -> anyhow::Result<Vec<u8>> {
    let buf = src.read_at(offset, len)?;
    if buf.len() as u64 != len {
        anyhow::bail!("kaynak eksik veri döndürdü ({offset} konumunda {len} yerine {} bayt)", buf.len());
    }
    Ok(buf)
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
