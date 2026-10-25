//! Kendini güncelleme. GitHub'daki son sürümün `latest.json` dosyası okunur; kurulum programı indirilip SHA-256 ile
//! doğrulanır. Uygulama indirmeleri duraklatıp kapanır; gizli bir PowerShell kurulumu sessizce (`/S`) çalıştırır ve
//! yeni sürümü açar. macOS'ta sürüm sayfası açılır.

use crate::kcore::{self, log};
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const LATEST_URL: &str = "https://github.com/enesbsafak/karadelikdm/releases/latest/download/latest.json";
const RELEASES_PREFIX: &str = "https://github.com/enesbsafak/karadelikdm/releases/download/";
pub const RELEASES_PAGE: &str = "https://github.com/enesbsafak/karadelikdm/releases/latest";

#[derive(Clone, Debug, Deserialize)]
pub struct Latest {
    pub version: String,
    /// "windows-x86_64", "macos-aarch64", "macos-x86_64"
    #[serde(default)]
    pub platforms: BTreeMap<String, Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

pub fn platform_key() -> &'static str {
    if cfg!(windows) {
        "windows-x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "macos-aarch64"
    } else {
        "macos-x86_64"
    }
}

static AVAILABLE: Mutex<Option<Latest>> = Mutex::new(None);

pub fn available() -> Option<String> {
    AVAILABLE.lock().as_ref().map(|l| l.version.clone())
}

/// "1.10.0" > "1.9.2"; yalnız sayısal parçalar karşılaştırılır ("v" öneki yok sayılır).
pub fn is_newer(remote: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.trim().trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect() };
    let (r, c) = (parts(remote), parts(current));
    !r.is_empty() && r > c
}

fn agent() -> Result<kdm_core::ureq::Agent> {
    kdm_core::engine::http::agent(&kcore::settings().net)
}

fn fetch_text(url: &str) -> Result<String> {
    let mut r = agent()?.get(url).call().map_err(|e| anyhow::anyhow!("{e}"))?;
    if r.status().as_u16() != 200 {
        bail!("HTTP {}", r.status());
    }
    r.body_mut().read_to_string().map_err(|e| anyhow::anyhow!("{e}"))
}

/// latest.json içeriğini denetler: bu platformun paketi bizim sürüm yolumuzdan mı, özeti var mı.
pub fn validate(l: &Latest) -> Result<Asset> {
    let a = l.platforms.get(platform_key()).cloned().ok_or_else(|| anyhow::anyhow!("latest.json: {} paketi yok", platform_key()))?;
    if !a.url.starts_with(RELEASES_PREFIX) {
        bail!("latest.json: beklenmeyen indirme adresi {}", a.url);
    }
    if a.sha256.len() != 64 || !a.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("latest.json: SHA-256 geçersiz");
    }
    Ok(a)
}

pub fn check() -> Result<Option<String>> {
    let l: Latest = serde_json::from_str(&fetch_text(LATEST_URL)?).context("latest.json")?;
    validate(&l)?;
    let newer = is_newer(&l.version, env!("CARGO_PKG_VERSION"));
    let v = newer.then(|| l.version.clone());
    *AVAILABLE.lock() = newer.then_some(l);
    Ok(v)
}

/// Açılıştan kısa süre sonra ve günde bir.
pub fn start_background() {
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let mut notified = String::new();
        loop {
            if kcore::settings().update_check {
                match check() {
                    Ok(Some(v)) if v != notified => {
                        kcore::notify(&crate::i18n::tf("msg.update_available", &[&v]));
                        kcore::emit("state", ());
                        notified = v;
                    }
                    Ok(_) => {}
                    Err(e) => log(&format!("Güncelleme denetimi: {e:#}")),
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(24 * 3600));
        }
    });
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

fn download(a: &Asset, version: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("KaradelikDM-update");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("KaradelikDM-{version}-setup.exe"));
    let mut r = agent()?.get(&a.url).call().map_err(|e| anyhow::anyhow!("{e}"))?;
    if r.status().as_u16() != 200 {
        bail!("HTTP {}", r.status());
    }
    let total: Option<u64> = r.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut reader = r.body_mut().as_reader();
    let mut out = std::fs::File::create(&file)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(t) = total {
            kcore::status(&crate::i18n::tf("msg.update_downloading", &[&format!("{}%", done * 100 / t.max(1))]));
        }
    }
    out.sync_all()?;
    drop(out);
    if !sha256_file(&file)?.eq_ignore_ascii_case(&a.sha256) {
        let _ = std::fs::remove_file(&file);
        bail!(crate::i18n::t("msg.update_bad_hash"));
    }
    Ok(file)
}

/// Tercihler/ana pencere "Güncelle": indir, doğrula, kur ve yeniden aç.
pub fn install() {
    let Some(l) = AVAILABLE.lock().clone() else { return };
    if !cfg!(windows) {
        if let Some(app) = kcore::app() {
            use tauri_plugin_opener::OpenerExt;
            let _ = app.opener().open_url(RELEASES_PAGE, None::<&str>);
        }
        return;
    }
    let result = validate(&l).and_then(|a| download(&a, &l.version)).and_then(|setup| relaunch(&setup));
    if let Err(e) = result {
        log(&format!("Güncelleme başarısız: {e:#}"));
        kcore::notify(&format!("{e:#}"));
    }
}

#[cfg(windows)]
fn relaunch(setup: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    log(&format!("Güncelleme kuruluyor: {}", setup.display()));
    kcore::persist_runtime_state();
    kcore::engine().shutdown(std::time::Duration::from_secs(5));
    let exe = std::env::current_exe()?;
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let script = format!(
        "Wait-Process -Id {} -Timeout 30 -ErrorAction SilentlyContinue; Start-Process -FilePath {} -ArgumentList '/S' -Wait; Start-Process -FilePath {}",
        std::process::id(),
        q(setup),
        q(&exe)
    );
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000)
        .spawn()
        .context("powershell")?;
    if let Some(a) = kcore::app() {
        a.exit(0);
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    std::process::exit(0);
}

#[cfg(not(windows))]
fn relaunch(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("", "0.1.0"));
    }

    #[test]
    fn rejects_foreign_urls_and_bad_hashes() {
        let ok = format!(r#"{{"version":"1.0.0","platforms":{{"{}":{{"url":"{RELEASES_PREFIX}v1.0.0/a.exe","sha256":"{}"}}}}}}"#, platform_key(), "a".repeat(64));
        assert!(validate(&serde_json::from_str(&ok).unwrap()).is_ok());
        let evil = ok.replace(RELEASES_PREFIX, "https://evil.example/");
        assert!(validate(&serde_json::from_str(&evil).unwrap()).is_err());
        let short = ok.replace(&"a".repeat(64), "abc");
        assert!(validate(&serde_json::from_str(&short).unwrap()).is_err());
    }
}
