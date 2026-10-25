//! `downloads.json`: atomik yazma (geçici dosya + yeniden adlandırma) ve bir önceki sürümün `.bak` yedeği.
//! Ana dosya bozuksa ya da yoksa yedekten okunur.

use super::model::Record;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

fn with_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

pub fn load(path: &Path) -> Vec<Record> {
    for p in [path.to_path_buf(), with_ext(path, ".bak")] {
        if let Ok(text) = std::fs::read_to_string(&p) {
            match serde_json::from_str::<Vec<Record>>(&text) {
                Ok(v) => return v,
                Err(e) => log::error!("{} okunamadı: {e}", p.display()),
            }
        }
    }
    Vec::new()
}

pub fn save(path: &Path, records: &[Record]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = with_ext(path, ".tmp");
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| tmp.display().to_string())?;
        f.write_all(serde_json::to_string_pretty(records)?.as_bytes())?;
        f.sync_all()?;
    }
    if path.exists() {
        let _ = std::fs::rename(path, with_ext(path, ".bak"));
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
