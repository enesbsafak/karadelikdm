//! Dosya kategorileri: uzantıya göre sınıflandırma ve her kategorinin indirme klasörü.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Sabit kimlik: compressed, documents, music, programs, video, general
    pub id: String,
    /// Boşluk ya da virgülle ayrılmış uzantılar
    pub exts: String,
    /// Mutlak yol ya da temel indirme klasörüne göre alt klasör
    pub dir: String,
}

pub const GENERAL: &str = "general";

pub fn defaults() -> Vec<Category> {
    let c = |id: &str, exts: &str, dir: &str| Category { id: id.into(), exts: exts.into(), dir: dir.into() };
    vec![
        c("compressed", "zip rar 7z gz tgz bz2 tbz2 xz txz zst tar iso img cab lz lzma", "Compressed"),
        c("documents", "pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt csv epub mobi djvu md", "Documents"),
        c("music", "mp3 m4a aac flac wav wma ogg oga opus aiff aif ape alac mid midi", "Music"),
        c("programs", "exe msi msix msixbundle appx appxbundle apk xapk dmg pkg deb rpm appimage bat cmd ps1 jar", "Programs"),
        c("video", "mp4 m4v mkv webm avi mov wmv flv mpg mpeg m2ts mts ts 3gp ogv vob", "Video"),
        c(GENERAL, "", ""),
    ]
}

fn exts(c: &Category) -> impl Iterator<Item = String> + '_ {
    c.exts.split([' ', ',', ';']).filter(|e| !e.is_empty()).map(|e| e.trim_start_matches('.').to_ascii_lowercase())
}

/// Dosya adına uyan kategori; hiçbiri uymazsa `general`.
pub fn classify<'a>(name: &str, cats: &'a [Category]) -> Option<&'a Category> {
    let ext = crate::engine::filename::extension(name);
    if !ext.is_empty() {
        if let Some(c) = cats.iter().find(|c| c.id != GENERAL && exts(c).any(|e| e == ext)) {
            return Some(c);
        }
    }
    cats.iter().find(|c| c.id == GENERAL)
}

/// Kategorinin klasörü: mutlak yol olduğu gibi, göreli yol temel klasörün altında.
pub fn dir_for(base: &Path, cat: Option<&Category>) -> PathBuf {
    match cat {
        Some(c) if !c.dir.trim().is_empty() => {
            let d = Path::new(c.dir.trim());
            if d.is_absolute() {
                d.to_path_buf()
            } else {
                base.join(d)
            }
        }
        _ => base.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let cats = defaults();
        assert_eq!(classify("a.ZIP", &cats).unwrap().id, "compressed");
        assert_eq!(classify("film.mkv", &cats).unwrap().id, "video");
        assert_eq!(classify("setup.exe", &cats).unwrap().id, "programs");
        assert_eq!(classify("noext", &cats).unwrap().id, "general");
        assert_eq!(classify("x.unknown", &cats).unwrap().id, "general");
    }

    #[test]
    fn dirs() {
        let base = Path::new(if cfg!(windows) { r"C:\dl" } else { "/dl" });
        let cats = defaults();
        assert_eq!(dir_for(base, classify("a.zip", &cats)), base.join("Compressed"));
        assert_eq!(dir_for(base, classify("a", &cats)), base);
        let abs = Category { id: "x".into(), exts: "".into(), dir: if cfg!(windows) { r"D:\v".into() } else { "/v".into() } };
        assert_eq!(dir_for(base, Some(&abs)), PathBuf::from(&abs.dir));
    }
}
