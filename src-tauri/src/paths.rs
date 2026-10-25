//! Veri klasörü: Windows'ta `%APPDATA%\KaradelikDM`, macOS'ta `~/Library/Application Support/KaradelikDM`.
//! `KDM_DATA` ortam değişkeni (testler, taşınabilir kullanım) bunu geçersiz kılar.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
}

impl Paths {
    pub fn detect() -> Self {
        let data = std::env::var_os("KDM_DATA")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("KaradelikDM")))
            .unwrap_or_else(|| std::env::temp_dir().join("KaradelikDM"));
        let _ = std::fs::create_dir_all(&data);
        Paths { data }
    }

    pub fn settings(&self) -> PathBuf {
        self.data.join("settings.json")
    }

    pub fn downloads(&self) -> PathBuf {
        self.data.join("downloads.json")
    }

    pub fn quota(&self) -> PathBuf {
        self.data.join("quota.json")
    }

    pub fn log(&self) -> PathBuf {
        self.data.join("kdm.log")
    }
}

pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
}
