//! Menü, tepsi ve diyaloglardan gelen eylem kimlikleri (`id` ya da `id:parametre`).

use crate::i18n::{t, tf};
use crate::kcore::{self, engine, sched};
use crate::windows;

pub fn dispatch(id: &str) {
    dispatch_values(id, &[]);
}

/// Diyalog onayından gelen alan değerleriyle birlikte.
pub fn dispatch_values(id: &str, values: &[(String, String)]) {
    let value = |name: &str| values.iter().find(|(k, _)| k == name).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
    let (head, arg) = id.split_once(':').unwrap_or((id, ""));
    match head {
        "show" => windows::show_main(),
        "add" => windows::open_add(windows::AddSpec::default()),
        "add_clipboard" => crate::clipboard::open_from_clipboard(),
        "prefs" => windows::open_prefs(),
        "queues" => windows::open_queues(),
        "grabber" => windows::open_grabber(),
        "zip_url" => {
            let s = crate::kcore::settings();
            windows::open_zip(serde_json::json!({ "url": arg, "dir": s.download_dir }));
        }
        "pause_all" => {
            for q in sched().active_ids() {
                sched().stop_queue(&q);
            }
            for id in engine().running_ids() {
                engine().stop(id);
            }
        }
        "queue_start" => sched().start_queue(if arg.is_empty() { kdm_core::queue::MAIN } else { arg }),
        "queue_stop" => sched().stop_queue(if arg.is_empty() { kdm_core::queue::MAIN } else { arg }),
        "limit" => {
            // limit:0 sınırı kapatır; limit:<bayt/sn> açar
            let bps: u64 = arg.parse().unwrap_or(0);
            kcore::update_settings(|s| {
                s.speed_limit_enabled = bps > 0;
                if bps > 0 {
                    s.speed_limit = bps;
                }
            });
            kcore::emit("state", ());
            crate::tray::refresh();
        }
        "limit_toggle" => {
            kcore::update_settings(|s| s.speed_limit_enabled = !s.speed_limit_enabled);
            kcore::emit("state", ());
            crate::tray::refresh();
        }
        "limit_custom" => {
            let kb = kcore::settings().speed_limit / 1024;
            windows::input(&t("limit.title"), &t("limit.kbps"), "limit_set", &kb.to_string());
        }
        "limit_set" => {
            if let Ok(kb) = value("value").parse::<u64>() {
                dispatch(&format!("limit:{}", kb * 1024));
            }
        }
        "clipboard_toggle" => {
            kcore::update_settings(|s| s.clipboard_monitor = !s.clipboard_monitor);
            kcore::emit("state", ());
            crate::tray::refresh();
        }
        "refresh_url" => {
            // İndirme adresinin süresi dolduysa yeni adresle devam (aynı dosya olmalı)
            if let Ok(id) = arg.parse::<u64>() {
                let url = value("value");
                if url.starts_with("http") {
                    if let Err(e) = engine().update(id, |r| r.url = url.clone()) {
                        kcore::status(&e.to_string());
                    }
                }
            }
        }
        "remove_files" => {
            let ids: Vec<u64> = arg.split(',').filter_map(|s| s.parse().ok()).collect();
            std::thread::spawn(move || {
                for id in ids {
                    let _ = engine().remove(id, true);
                }
            });
        }
        "power" => crate::system::power(arg),
        "app_update" => {
            if let Some(v) = crate::updater::available() {
                windows::confirm(&t("update.title"), &tf("update.confirm", &[&v, env!("CARGO_PKG_VERSION")]), "app_update_go", false);
            }
        }
        "app_update_go" => crate::updater::install(),
        "update_check" => {
            std::thread::spawn(crate::updater::check_now_and_notify);
        }
        "about" => windows::info(
            "Karadelik Download Manager",
            &tf("about.text", &[env!("CARGO_PKG_VERSION")]),
            "",
        ),
        "exit" => crate::exit_app(),
        _ => kcore::log(&format!("Bilinmeyen eylem: {id}")),
    }
}
