//! Tarayıcı uzantısıyla köprü. Yerel WebSocket sunucusu ve güvenlik denetimleri `kdm-bridge`'de; burada uzantıdan
//! gelen istekler ekleme penceresine ya da doğrudan indirmeye dönüştürülür.
//!
//! `handle` hemen dönmelidir: uzantı birkaç saniye içinde yanıt alamazsa indirmeyi tarayıcıya bırakır ve dosya iki
//! kez iner. Pencere açma ve indirme ekleme ayrı iş parçacığında yapılır.

use crate::commands::{add_downloads, AddForm};
use crate::kcore::{self, log, settings};
use crate::windows::{self, AddSpec};
use kdm_bridge::{CaptureConfig, Handler, Request, Server};
use parking_lot::Mutex;
use std::sync::Arc;

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

struct AppHandler;

fn valid_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "ftp" | "ftps")) || kdm_torrent::parse_magnet(url).is_ok()
}

impl Handler for AppHandler {
    fn config(&self) -> CaptureConfig {
        let s = settings();
        CaptureConfig { enabled: s.browser_enabled, exts: s.capture_ext_list(), min_size_kb: s.browser_min_size_kb }
    }

    fn handle(&self, req: Request) -> Result<(), String> {
        let s = settings();
        if !s.browser_enabled {
            return Err("browser integration disabled".into());
        }
        match req {
            Request::Download { url, referer, cookies, user_agent, file_name, size, .. } => {
                if !valid_url(&url) {
                    return Err("unsupported address".into());
                }
                if s.browser_show_dialog {
                    let spec = AddSpec { url, file_name, referer, cookies, user_agent, size, ..Default::default() };
                    std::thread::spawn(move || windows::open_add(spec));
                } else {
                    let form = AddForm { urls: vec![url], file_name, referer, cookies, user_agent, mode: "now".into(), ..Default::default() };
                    std::thread::spawn(move || {
                        if let Err(e) = add_downloads(form) {
                            kcore::notify(&e);
                        }
                    });
                }
                Ok(())
            }
            Request::Links { urls, referer, cookies, user_agent } => {
                let urls: Vec<String> = urls.into_iter().filter(|u| valid_url(u)).collect();
                if urls.is_empty() {
                    return Err("no supported links".into());
                }
                let spec = AddSpec { url: urls[0].clone(), urls, referer, cookies, user_agent, ..Default::default() };
                std::thread::spawn(move || windows::open_add(spec));
                Ok(())
            }
            Request::Media { url, kind, title, referer, cookies, user_agent } => {
                if !valid_url(&url) {
                    return Err("unsupported address".into());
                }
                let stream = kind == "hls" || kind == "dash";
                let file_name = kdm_core::engine::filename::sanitize(&title);
                let file_name = match (stream, file_name.is_empty()) {
                    (_, true) => String::new(),
                    (true, false) => format!("{file_name}.mp4"),
                    (false, false) => file_name,
                };
                let spec = AddSpec { url, file_name, referer, cookies, user_agent, media: stream, ..Default::default() };
                std::thread::spawn(move || windows::open_add(spec));
                Ok(())
            }
            Request::Hello { .. } | Request::Ping => Ok(()),
        }
    }
}

/// Açılışta: sunucuyu başlatır, native messaging host'unu tarayıcılara kaydeder.
pub fn start() {
    let data = kcore::core().paths.data.clone();
    match kdm_bridge::start(&data, kdm_bridge::default_allowed_origins(), Arc::new(AppHandler)) {
        Ok(srv) => {
            log(&format!("Tarayıcı köprüsü 127.0.0.1:{}", srv.port));
            *SERVER.lock() = Some(srv);
        }
        Err(e) => log(&format!("Tarayıcı köprüsü başlatılamadı: {e:#}")),
    }
    // Host, kurulumda ana exe'nin yanına konur (tauri.conf.json > bundle.externalBin)
    let nmh = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(nmh_name())));
    match nmh {
        Some(p) if p.is_file() => {
            if let Err(e) = kdm_bridge::register_native_hosts(&p, &data, &kdm_bridge::chrome_ids(), &[kdm_bridge::FIREFOX_EXTENSION_ID]) {
                log(&format!("Native messaging kaydı yapılamadı: {e:#}"));
            }
        }
        _ => log("karadelikdm-nmh bulunamadı; tarayıcı uzantısı uygulamayı bulamayacak"),
    }
}

fn nmh_name() -> &'static str {
    if cfg!(windows) {
        "karadelikdm-nmh.exe"
    } else {
        "karadelikdm-nmh"
    }
}

/// En az bir tarayıcı uzantısı şu an bağlı mı.
pub fn connected() -> bool {
    SERVER.lock().as_ref().is_some_and(|s| s.client_count() > 0)
}

/// Tarayıcı ayarları değişince uzantılara yeni yakalama ayarını gönderir.
pub fn broadcast() {
    if let Some(s) = SERVER.lock().as_ref() {
        s.broadcast_config();
    }
}

pub fn stop() {
    if let Some(s) = SERVER.lock().take() {
        s.stop();
    }
}
