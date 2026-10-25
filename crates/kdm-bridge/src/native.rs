//! Native messaging host manifestleri ve kayıtları.
//!
//! Chromium (Chrome, Edge) ve Firefox aynı manifest biçimini kullanır, yalnız izin alanı farklıdır:
//! Chromium `allowed_origins` (`chrome-extension://<id>/`), Firefox `allowed_extensions` (eklenti kimliği).
//! Bu yüzden iki ayrı dosya yazılır. Kayıt yalnız kullanıcı düzeyindedir (HKCU / ev dizini); yönetici izni gerekmez.

use crate::HOST_NAME;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const DESCRIPTION: &str = "Karadelik Download Manager native messaging host";

/// Windows'ta `canonicalize` çıktısındaki `\\?\` önekini atar; tarayıcılar bu biçimi her zaman çözemiyor.
fn plain_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or_else(|| s.into_owned())
}

pub fn chrome_manifest(nmh_exe: &Path, chrome_ids: &[&str]) -> Value {
    json!({
        "name": HOST_NAME,
        "description": DESCRIPTION,
        "path": plain_path(nmh_exe),
        "type": "stdio",
        // Chrome burada sondaki `/`'ı zorunlu tutar ve joker kabul etmez
        "allowed_origins": chrome_ids.iter().map(|id| format!("chrome-extension://{id}/")).collect::<Vec<_>>(),
    })
}

pub fn firefox_manifest(nmh_exe: &Path, firefox_ids: &[&str]) -> Value {
    json!({
        "name": HOST_NAME,
        "description": DESCRIPTION,
        "path": plain_path(nmh_exe),
        "type": "stdio",
        "allowed_extensions": firefox_ids,
    })
}

/// İçerik aynıysa dokunmaz (her açılışta yazmak dosya izleyicilerini ve yedeklemeyi gereksiz tetikler).
/// `true`: dosya yazıldı.
pub(crate) fn write_if_changed(path: &Path, body: &[u8]) -> Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == body) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("{} oluşturulamadı", dir.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("{} yazılamadı", path.display()))?;
    Ok(true)
}

fn body(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec_pretty(v).unwrap_or_default();
    b.push(b'\n');
    b
}

/// `manifest_dir` altına `<HOST_NAME>.chrome.json` ve `<HOST_NAME>.firefox.json` yazar, yollarını döner.
pub fn write_manifests(
    nmh_exe: &Path,
    manifest_dir: &Path,
    chrome_ids: &[&str],
    firefox_ids: &[&str],
) -> Result<(PathBuf, PathBuf)> {
    let chrome = manifest_dir.join(format!("{HOST_NAME}.chrome.json"));
    let firefox = manifest_dir.join(format!("{HOST_NAME}.firefox.json"));
    write_if_changed(&chrome, &body(&chrome_manifest(nmh_exe, chrome_ids)))?;
    write_if_changed(&firefox, &body(&firefox_manifest(nmh_exe, firefox_ids)))?;
    Ok((chrome, firefox))
}

/// Native host manifestlerini yazar ve tarayıcılara kaydeder. Manifest yalnız değiştiyse yazılır.
///
/// - Windows: manifestler `manifest_dir`'e yazılır; `HKCU\Software\{Google\Chrome,Microsoft\Edge,Mozilla}\
///   NativeMessagingHosts\<name>` anahtarının varsayılan değeri dosya yoludur.
/// - macOS / Linux: tarayıcılar manifesti sabit klasörlerde arar, oraya `<name>.json` yazılır;
///   `manifest_dir` kullanılmaz. Tarayıcı sonradan kurulsa da çalışsın diye klasör yoksa oluşturulur.
pub fn register_native_hosts(
    nmh_exe: &Path,
    manifest_dir: &Path,
    chrome_ids: &[&str],
    firefox_ids: &[&str],
) -> Result<()> {
    #[cfg(windows)]
    {
        let (chrome, firefox) = write_manifests(nmh_exe, manifest_dir, chrome_ids, firefox_ids)?;
        for (vendor, file) in [(r"Google\Chrome", &chrome), (r"Microsoft\Edge", &chrome), ("Mozilla", &firefox)] {
            set_registry(&format!(r"Software\{vendor}\NativeMessagingHosts\{HOST_NAME}"), file)?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = manifest_dir;
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME tanımsız")?);
        let file = format!("{HOST_NAME}.json");
        let chrome = body(&chrome_manifest(nmh_exe, chrome_ids));
        let firefox = body(&firefox_manifest(nmh_exe, firefox_ids));
        for (dir, b) in unix_host_dirs(&home).into_iter().map(|(d, ff)| (d, if ff { &firefox } else { &chrome })) {
            write_if_changed(&dir.join(&file), b)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
fn set_registry(key: &str, manifest: &Path) -> Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    let want = plain_path(manifest);
    let (k, _) = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(key)
        .with_context(|| format!("HKCU\\{key} oluşturulamadı"))?;
    if k.get_value::<String, _>("").ok().as_deref() != Some(want.as_str()) {
        k.set_value("", &want).with_context(|| format!("HKCU\\{key} yazılamadı"))?;
    }
    Ok(())
}

/// (klasör, Firefox mu)
#[cfg(not(windows))]
fn unix_host_dirs(home: &Path) -> Vec<(PathBuf, bool)> {
    if cfg!(target_os = "macos") {
        let base = home.join("Library/Application Support");
        vec![
            (base.join("Google/Chrome/NativeMessagingHosts"), false),
            (base.join("Microsoft Edge/NativeMessagingHosts"), false),
            (base.join("Mozilla/NativeMessagingHosts"), true),
        ]
    } else {
        vec![
            (home.join(".config/google-chrome/NativeMessagingHosts"), false),
            (home.join(".config/chromium/NativeMessagingHosts"), false),
            (home.join(".config/microsoft-edge/NativeMessagingHosts"), false),
            (home.join(".mozilla/native-messaging-hosts"), true),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::temp_dir;
    use crate::{CHROME_EXTENSION_ID, FIREFOX_EXTENSION_ID};

    fn exe() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"\\?\C:\Program Files\Karadelik DM\karadelikdm-nmh.exe")
        } else {
            PathBuf::from("/Applications/Karadelik DM.app/Contents/MacOS/karadelikdm-nmh")
        }
    }

    #[test]
    fn chrome_manifest_content() {
        let m = chrome_manifest(&exe(), &[CHROME_EXTENSION_ID]);
        assert_eq!(m["name"], HOST_NAME);
        assert_eq!(m["type"], "stdio");
        assert_eq!(m["allowed_origins"], json!([format!("chrome-extension://{CHROME_EXTENSION_ID}/")]));
        assert!(m.get("allowed_extensions").is_none());
        let path = m["path"].as_str().unwrap();
        assert!(!path.starts_with(r"\\?\"));
        assert!(path.ends_with("karadelikdm-nmh.exe") || path.ends_with("karadelikdm-nmh"));
    }

    #[test]
    fn firefox_manifest_content() {
        let m = firefox_manifest(&exe(), &[FIREFOX_EXTENSION_ID]);
        assert_eq!(m["name"], HOST_NAME);
        assert_eq!(m["type"], "stdio");
        assert_eq!(m["allowed_extensions"], json!([FIREFOX_EXTENSION_ID]));
        assert!(m.get("allowed_origins").is_none());
    }

    #[test]
    fn host_name_is_valid() {
        // Chrome kuralı: küçük harf, rakam, `_` ve `.`; noktayla başlayıp bitemez, `..` olamaz
        assert!(HOST_NAME.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'));
        assert!(!HOST_NAME.starts_with('.') && !HOST_NAME.ends_with('.') && !HOST_NAME.contains(".."));
    }

    #[test]
    fn manifests_written_only_when_changed() {
        let dir = temp_dir("nm");
        let (c, f) = write_manifests(&exe(), &dir, &[CHROME_EXTENSION_ID], &[FIREFOX_EXTENSION_ID]).unwrap();
        let cv: Value = serde_json::from_slice(&std::fs::read(&c).unwrap()).unwrap();
        let fv: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(cv, chrome_manifest(&exe(), &[CHROME_EXTENSION_ID]));
        assert_eq!(fv, firefox_manifest(&exe(), &[FIREFOX_EXTENSION_ID]));
        assert!(!write_if_changed(&c, &std::fs::read(&c).unwrap()).unwrap());
        assert!(write_if_changed(&c, b"{}").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
