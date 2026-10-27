//! Tarayıcı uzantısını kurmayı kısaltır. Tarayıcılar uzantının sessizce kurulmasına izin vermez; kullanıcı bir kez
//! onaylar. Mağazada yayınlanmış tarayıcıda mağaza sayfası o tarayıcıda açılır ("Ekle" tek tık). Yayınlanmamışta
//! (Chrome/Edge) uygulamayla gelen paketlenmemiş uzantı veri klasörüne çıkarılır; arayüz yolu ve adımları gösterir.
//! Uzantılar sayfası (`chrome://extensions`) komut satırından açılamaz: tarayıcılar bu adresleri yok sayar.

use crate::kcore::{app, core};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::Manager;

type Res<T> = Result<T, String>;

/// Mağaza sayfaları; yayınlanınca doldurulur. `None`: elle (paketlenmemiş) kurulum.
const CHROME_STORE: Option<&str> = Some(CWS_PAGE);
/// Edge, Chrome Web Store'dan da kurar ("Diğer mağazalardan uzantılara izin ver" tek tık); kimlik aynıdır.
/// Edge Eklentileri onaylanınca kendi sayfası yazılır.
const EDGE_STORE: Option<&str> = Some(CWS_PAGE);
const CWS_PAGE: &str = "https://chromewebstore.google.com/detail/karadelik-download-manage/ajflkdfogcikaobkhalblmmahlpdbmee";
const FIREFOX_STORE: Option<&str> = None;

struct Browser {
    id: &'static str,
    name: &'static str,
    /// Windows'ta App Paths kaydındaki çalıştırılabilir ad
    #[cfg_attr(not(windows), allow(dead_code))]
    exe: &'static str,
    /// macOS uygulama paketi
    #[cfg_attr(windows, allow(dead_code))]
    mac_app: &'static str,
    store: Option<&'static str>,
    /// Elle kurulumda uzantılar sayfası
    extensions_page: &'static str,
    chromium: bool,
}

const BROWSERS: &[Browser] = &[
    Browser { id: "chrome", name: "Google Chrome", exe: "chrome.exe", mac_app: "Google Chrome.app", store: CHROME_STORE, extensions_page: "chrome://extensions", chromium: true },
    Browser { id: "edge", name: "Microsoft Edge", exe: "msedge.exe", mac_app: "Microsoft Edge.app", store: EDGE_STORE, extensions_page: "edge://extensions", chromium: true },
    Browser { id: "firefox", name: "Mozilla Firefox", exe: "firefox.exe", mac_app: "Firefox.app", store: FIREFOX_STORE, extensions_page: "about:addons", chromium: false },
];

#[derive(Serialize)]
pub struct BrowserView {
    id: &'static str,
    name: &'static str,
    found: bool,
    /// "store": mağazadan tek tık; "manual": paketlenmemiş kurulum; "unavailable": henüz kurulamaz (Firefox, mağaza öncesi)
    mode: &'static str,
}

#[derive(Serialize)]
pub struct ExtStatus {
    browsers: Vec<BrowserView>,
    connected: bool,
}

#[derive(Serialize)]
pub struct InstallResult {
    mode: &'static str,
    /// Elle kurulumda: çıkarılan uzantı klasörü ve tarayıcının uzantılar sayfası
    path: String,
    page: String,
}

fn mode(b: &Browser) -> &'static str {
    match (b.store, b.chromium) {
        (Some(_), _) => "store",
        (None, true) => "manual",
        (None, false) => "unavailable",
    }
}

#[cfg(windows)]
fn find(b: &Browser) -> Option<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let key = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}", b.exe);
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter().find_map(|root| {
        let v: String = winreg::RegKey::predef(root).open_subkey(&key).ok()?.get_value("").ok()?;
        let p = PathBuf::from(v.trim_matches('"'));
        p.exists().then_some(p)
    })
}

#[cfg(not(windows))]
fn find(b: &Browser) -> Option<PathBuf> {
    let p = Path::new("/Applications").join(b.mac_app);
    p.exists().then_some(p)
}

fn open_in(exe: &Path, url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new(exe).arg(url).spawn()?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("open").arg("-a").arg(exe).arg(url).spawn()?;
    }
    Ok(())
}

/// Uygulamayla gelen paketlenmemiş Chromium uzantısını veri klasörüne kopyalar (her seferinde günceller, böylece
/// uygulama güncellenince tarayıcıdaki "Yeniden yükle" yeni sürümü alır).
fn extract_chromium() -> Res<PathBuf> {
    let src = app()
        .ok_or("uygulama hazır değil")?
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("extension")
        .join("chromium");
    if !src.join("manifest.json").exists() {
        return Err(format!("uzantı paketi bulunamadı: {}", src.display()));
    }
    let dst = core().paths.data.join("extension").join("chromium");
    copy_dir(&src, &dst).map_err(|e| format!("uzantı kopyalanamadı: {e}"))?;
    Ok(dst)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn ext_status() -> ExtStatus {
    ExtStatus {
        browsers: BROWSERS.iter().map(|b| BrowserView { id: b.id, name: b.name, found: find(b).is_some(), mode: mode(b) }).collect(),
        connected: crate::bridge::connected(),
    }
}

#[tauri::command]
pub fn ext_install(browser: String) -> Res<InstallResult> {
    let b = BROWSERS.iter().find(|b| b.id == browser).ok_or("bilinmeyen tarayıcı")?;
    match mode(b) {
        "store" => {
            let exe = find(b).ok_or("tarayıcı bulunamadı")?;
            let url = b.store.unwrap_or_default();
            open_in(&exe, url).map_err(|e| e.to_string())?;
            Ok(InstallResult { mode: "store", path: String::new(), page: url.to_string() })
        }
        "manual" => {
            let dir = extract_chromium()?;
            Ok(InstallResult { mode: "manual", path: dir.to_string_lossy().into_owned(), page: b.extensions_page.to_string() })
        }
        _ => Err("Bu tarayıcı için uzantı mağaza onayından sonra kurulabilecek.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_follow_store_availability() {
        for b in BROWSERS {
            let m = mode(b);
            assert!(m == "store" || (b.chromium && m == "manual") || (!b.chromium && m == "unavailable"), "{} {m}", b.id);
        }
    }

    #[test]
    fn copy_dir_copies_tree() {
        let base = std::env::temp_dir().join(format!("kdm-ext-copy-{}", std::process::id()));
        let src = base.join("src");
        std::fs::create_dir_all(src.join("icons")).unwrap();
        std::fs::write(src.join("manifest.json"), b"{}").unwrap();
        std::fs::write(src.join("icons").join("a.png"), b"x").unwrap();
        copy_dir(&src, &base.join("dst")).unwrap();
        // ikinci kopya üzerine yazar
        copy_dir(&src, &base.join("dst")).unwrap();
        assert_eq!(std::fs::read(base.join("dst").join("icons").join("a.png")).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&base);
    }
}
