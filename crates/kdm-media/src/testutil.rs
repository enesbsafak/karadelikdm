//! Çevrimdışı testler için sahte `Fetcher`: adres → bayt eşlemesi, istek sayaçları ve hata enjeksiyonu.

use crate::Fetcher;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

type Hook = Box<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Fake {
    files: HashMap<String, Vec<u8>>,
    hits: Mutex<HashMap<String, usize>>,
    /// Adres başına kalan başarısız yanıt sayısı
    fail: Mutex<HashMap<String, usize>>,
    /// Her istek yanıtlanmadan önce çağrılır
    hook: Option<Hook>,
}

impl Fake {
    pub fn with(mut self, url: &str, data: impl Into<Vec<u8>>) -> Self {
        self.files.insert(url.to_string(), data.into());
        self
    }

    pub fn failing(self, url: &str, times: usize) -> Self {
        self.fail.lock().unwrap().insert(url.to_string(), times);
        self
    }

    pub fn hook(mut self, h: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.hook = Some(Box::new(h));
        self
    }

    pub fn hits(&self, url: &str) -> usize {
        self.hits.lock().unwrap().get(url).copied().unwrap_or(0)
    }
}

impl Fetcher for Fake {
    fn get(&self, url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        *self.hits.lock().unwrap().entry(url.to_string()).or_default() += 1;
        if let Some(h) = &self.hook {
            h(url);
        }
        if let Some(n) = self.fail.lock().unwrap().get_mut(url) {
            if *n > 0 {
                *n -= 1;
                bail!("sahte ağ hatası");
            }
        }
        let d = self.files.get(url).ok_or_else(|| anyhow!("404: {url}"))?;
        match range {
            Some((s, e)) => Ok(d.get(s as usize..=e as usize).ok_or_else(|| anyhow!("416: {url} {s}-{e}"))?.to_vec()),
            None => Ok(d.clone()),
        }
    }
}

/// Test başına benzersiz, test sonunda silinen klasör.
pub(crate) struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "kdm-media-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
