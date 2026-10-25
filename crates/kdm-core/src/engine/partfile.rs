//! Tek parça dosyası (`<ad>.kdm`). Her bağlantı kendi konumuna yazar; birleştirme aşaması yoktur.
//! Windows'ta dosya seyrek (sparse) işaretlenir: aksi halde dosyanın sonlarına yazmak, NTFS'in aradaki
//! boşluğu sıfırla doldurmasını bekletir (çok GB'lık dosyada saniyeler).

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::path::Path;

pub struct PartFile {
    file: File,
}

impl PartFile {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("klasör oluşturulamadı: {}", dir.display()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("dosya açılamadı: {}", path.display()))?;
        set_sparse(&file);
        Ok(PartFile { file })
    }

    /// Boyut biliniyorsa dosyayı o boyuta getirir (seyrek dosyada yer ayırmaz).
    pub fn set_len(&self, len: u64) -> Result<()> {
        if self.file.metadata()?.len() != len {
            self.file.set_len(len)?;
        }
        Ok(())
    }

    pub fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
        while !buf.is_empty() {
            let n = write_at(&self.file, buf, offset)?;
            if n == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            buf = &buf[n..];
            offset += n as u64;
        }
        Ok(())
    }

    pub fn sync(&self) -> std::io::Result<()> {
        self.file.sync_data()
    }

    pub fn size(&self) -> u64 {
        self.file.metadata().map(|m| m.len()).unwrap_or(0)
    }
}

#[cfg(windows)]
fn write_at(f: &File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, buf, offset)
}

#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, buf, offset)
}

#[cfg(windows)]
fn set_sparse(f: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let mut returned = 0u32;
    // Başarısız olursa (FAT32, ağ sürücüsü) normal dosya olarak devam edilir
    unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn set_sparse(_: &File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn positional_writes() {
        let p = std::env::temp_dir().join(format!("kdm-part-{}.kdm", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let f = PartFile::open(&p).unwrap();
            f.set_len(10).unwrap();
            f.write_all_at(b"world", 5).unwrap();
            f.write_all_at(b"hello", 0).unwrap();
            f.sync().unwrap();
            assert_eq!(f.size(), 10);
        }
        let mut s = String::new();
        File::open(&p).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "helloworld");
        std::fs::remove_file(&p).unwrap();
    }
}
