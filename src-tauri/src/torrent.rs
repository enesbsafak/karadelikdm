//! Torrent ekleme penceresi ve sistem kaydı: `.torrent` / magnet bilgisini okuma, dosya seçimi için liste,
//! magnet bağlantıları ve `.torrent` dosyaları için varsayılan uygulama olma (yalnız kullanıcı isteyince).

use crate::kcore::{engine, settings};
use kdm_core::engine::http::{self, DigestState, RequestSpec};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

type Res<T> = Result<T, String>;

/// `.torrent` dosyaları birkaç MB'ı geçmez; sınırsız okumak bellek tüketimine açık kapı olur
const MAX_TORRENT: u64 = 16 * 1024 * 1024;

/// Pencere başına süren magnet çözümü (pencere kapanınca ya da adres değişince iptal edilir)
static PENDING: Mutex<Option<HashMap<String, Arc<AtomicBool>>>> = Mutex::new(None);

#[derive(Serialize)]
pub struct FileView {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

#[derive(Serialize)]
pub struct InfoView {
    pub info_hash: String,
    pub name: String,
    pub total: u64,
    pub multi_file: bool,
    pub private: bool,
    pub files: Vec<FileView>,
    /// Kayda yazılacak adres
    pub magnet: String,
}

pub fn is_torrent_path(s: &str) -> bool {
    s.to_ascii_lowercase().ends_with(".torrent") && !s.contains("://")
}

fn cancel_flag(label: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Some(old) = PENDING.lock().get_or_insert_with(HashMap::new).insert(label.to_string(), flag.clone()) {
        old.store(true, Ordering::Relaxed);
    }
    flag
}

fn fetch(url: &str, referer: &str, cookies: &str, user_agent: &str) -> anyhow::Result<Vec<u8>> {
    let agent = http::agent(&settings().net)?;
    let spec = RequestSpec { url: url.into(), referer: referer.into(), cookies: cookies.into(), user_agent: user_agent.into(), ..Default::default() };
    let r = http::get(&agent, &spec, None, "", &DigestState::default())?;
    if r.meta.status != 200 {
        anyhow::bail!("HTTP {}", r.meta.status);
    }
    let mut b = Vec::new();
    r.body.into_reader().take(MAX_TORRENT + 1).read_to_end(&mut b)?;
    if b.len() as u64 > MAX_TORRENT {
        anyhow::bail!("torrent dosyası çok büyük");
    }
    Ok(b)
}

fn read_local(path: &str) -> anyhow::Result<Vec<u8>> {
    let p = Path::new(path);
    if std::fs::metadata(p)?.len() > MAX_TORRENT {
        anyhow::bail!("torrent dosyası çok büyük");
    }
    Ok(std::fs::read(p)?)
}

fn pct(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

pub fn inspect(source: &str, referer: &str, cookies: &str, user_agent: &str, cancel: &AtomicBool) -> anyhow::Result<InfoView> {
    let src = source.trim();
    let m = if kdm_torrent::is_magnet(src) {
        engine().torrent_metadata(kdm_torrent::Source::Magnet(src), &|| cancel.load(Ordering::Relaxed))?
    } else {
        let bytes = if src.starts_with("http://") || src.starts_with("https://") {
            fetch(src, referer, cookies, user_agent)?
        } else {
            read_local(src)?
        };
        engine().torrent_metadata(kdm_torrent::Source::File(&bytes), &|| false)?
    };
    let magnet = if kdm_torrent::is_magnet(src) { src.to_string() } else { format!("magnet:?xt=urn:btih:{}&dn={}", m.info_hash, pct(&m.name)) };
    Ok(InfoView {
        info_hash: m.info_hash,
        name: m.name,
        total: m.total,
        multi_file: m.multi_file,
        private: m.private,
        files: m
            .files
            .into_iter()
            .enumerate()
            .filter(|(_, f)| !f.padding)
            .map(|(index, f)| FileView { index, path: f.path, size: f.size })
            .collect(),
        magnet,
    })
}

#[tauri::command]
pub async fn torrent_inspect(window: tauri::WebviewWindow, source: String, referer: String, cookies: String, user_agent: String) -> Res<InfoView> {
    let flag = cancel_flag(window.label());
    let label = window.label().to_string();
    let r = tauri::async_runtime::spawn_blocking(move || {
        let r = inspect(&source, &referer, &cookies, &user_agent, &flag);
        if flag.load(Ordering::Relaxed) {
            return Err("iptal edildi".to_string());
        }
        r.map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(m) = PENDING.lock().as_mut() {
        if m.get(&label).is_some_and(|f| !f.load(Ordering::Relaxed)) {
            m.remove(&label);
        }
    }
    r
}

#[tauri::command]
pub fn torrent_inspect_cancel(window: tauri::WebviewWindow) {
    cancel(window.label());
}

/// Ekleme penceresi kapanınca süren çözüm bırakılır.
pub fn cancel(label: &str) {
    if let Some(f) = PENDING.lock().as_mut().and_then(|m| m.remove(label)) {
        f.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
pub async fn pick_torrent_file() -> Option<String> {
    let app = crate::kcore::app()?.clone();
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog().file().add_filter("Torrent", &["torrent"]).blocking_pick_file().map(|p| p.to_string())
    })
    .await
    .ok()
    .flatten()
}

/// Magnet bağlantıları ve `.torrent` dosyaları için bu uygulamayı kaydeder (HKCU; yönetici izni gerekmez).
/// Kaldırıcı aynı anahtarları siler (`windows/hooks.nsh`).
#[tauri::command]
pub fn torrent_register_handlers() -> Res<()> {
    register().map_err(|e| format!("{e:#}"))
}

#[cfg(windows)]
fn register() -> anyhow::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    let exe = std::env::current_exe()?.to_string_lossy().replace(r"\\?\", "");
    let cmd = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let classes = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(r"Software\Classes")?.0;
    let (m, _) = classes.create_subkey("magnet")?;
    m.set_value("", &"URL:Magnet")?;
    m.set_value("URL Protocol", &"")?;
    m.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
    m.create_subkey(r"shell\open\command")?.0.set_value("", &cmd)?;
    let (p, _) = classes.create_subkey(PROG_ID)?;
    p.set_value("", &"Torrent")?;
    p.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
    p.create_subkey(r"shell\open\command")?.0.set_value("", &cmd)?;
    let (t, _) = classes.create_subkey(".torrent")?;
    t.set_value("", &PROG_ID)?;
    t.set_value("Content Type", &"application/x-bittorrent")?;
    t.create_subkey("OpenWithProgids")?.0.set_value(PROG_ID, &"")?;
    // Gezgin'in önbelleği yenilensin
    unsafe {
        windows_sys::Win32::UI::Shell::SHChangeNotify(
            windows_sys::Win32::UI::Shell::SHCNE_ASSOCCHANGED as i32,
            windows_sys::Win32::UI::Shell::SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
    Ok(())
}

#[cfg(windows)]
const PROG_ID: &str = "KaradelikDM.torrent";

#[cfg(not(windows))]
fn register() -> anyhow::Result<()> {
    // macOS'ta ilişkilendirme paket bilgisinden (Info.plist) gelir
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_torrent_path_detection() {
        assert!(is_torrent_path(r"C:\Users\a b\film.TORRENT"));
        assert!(!is_torrent_path("https://x/a.torrent"));
        assert!(!is_torrent_path("magnet:?xt=urn:btih:abc"));
    }
}
