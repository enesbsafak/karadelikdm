//! Bildirim alanı simgesi ve menüsü.

use crate::i18n::t;
use crate::kcore::{app, settings};
use kdm_core::engine::{Info, Status};
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Wry};

pub fn icon(active: bool) -> &'static [u8] {
    match (cfg!(target_os = "macos"), active) {
        (true, true) => include_bytes!("../icons/tray-mac-on.png"),
        (true, false) => include_bytes!("../icons/tray-mac-off.png"),
        (false, true) => include_bytes!("../icons/tray-on.png"),
        (false, false) => include_bytes!("../icons/tray-off.png"),
    }
}

const LIMITS: &[u64] = &[128, 256, 512, 1024, 2048, 5120, 10240];

fn kb_label(kb: u64) -> String {
    if kb >= 1024 && kb.is_multiple_of(1024) {
        format!("{} MB/s", kb / 1024)
    } else {
        format!("{kb} KB/s")
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let s = settings();
    let current = if s.speed_limit_enabled { s.speed_limit / 1024 } else { 0 };
    let mut limit = SubmenuBuilder::new(app, t("tray.speed_limit"))
        .item(&CheckMenuItemBuilder::with_id("limit:0", t("tray.unlimited")).checked(current == 0).build(app)?)
        .separator();
    for &kb in LIMITS {
        limit = limit.item(&CheckMenuItemBuilder::with_id(format!("limit:{}", kb * 1024), kb_label(kb)).checked(current == kb).build(app)?);
    }
    let custom_checked = current != 0 && !LIMITS.contains(&current);
    let custom_text = if custom_checked { format!("{} ({})", t("tray.custom"), kb_label(current)) } else { t("tray.custom") };
    let limit = limit.separator().item(&CheckMenuItemBuilder::with_id("limit_custom", custom_text).checked(custom_checked).build(app)?).build()?;

    MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id("show", t("tray.show")).build(app)?)
        .item(&MenuItemBuilder::with_id("add", t("tray.add")).build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("queue_start", t("tray.start_queue")).build(app)?)
        .item(&MenuItemBuilder::with_id("pause_all", t("tray.pause_all")).build(app)?)
        .item(&limit)
        .item(&CheckMenuItemBuilder::with_id("clipboard_toggle", t("tray.clipboard")).checked(s.clipboard_monitor).build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("update_check", t("tray.check_updates")).build(app)?)
        .item(&MenuItemBuilder::with_id("prefs", t("tray.prefs")).build(app)?)
        .item(&MenuItemBuilder::with_id("exit", t("tray.exit")).build(app)?)
        .build()
}

pub fn refresh() {
    let Some(app) = app() else { return };
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = app2.tray_by_id("main") {
            if let Ok(menu) = build(&app2) {
                let _ = tray.set_menu(Some(menu));
            }
        }
    });
}

fn human_speed(bps: u64) -> String {
    if bps >= 1024 * 1024 {
        format!("{:.1} MB/s", bps as f64 / 1048576.0)
    } else {
        format!("{} KB/s", bps / 1024)
    }
}

/// İpucu ve simge: etkin indirme varsa "açık" simgesi.
pub fn update_tooltip(infos: &[Info]) {
    let Some(app) = app() else { return };
    let active: Vec<&Info> = infos.iter().filter(|i| i.status == Status::Downloading).collect();
    let speed: u64 = active.iter().map(|i| i.speed).sum();
    let tip = if active.is_empty() {
        "Karadelik Download Manager".to_string()
    } else {
        format!("Karadelik DM · {} · {}", crate::i18n::tf("tray.active", &[&active.len().to_string()]), human_speed(speed))
    };
    let on = !active.is_empty();
    static LAST: parking_lot::Mutex<Option<(bool, String)>> = parking_lot::Mutex::new(None);
    {
        let mut last = LAST.lock();
        if last.as_ref().is_some_and(|(o, t)| *o == on && *t == tip) {
            return;
        }
        *last = Some((on, tip.clone()));
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = app2.tray_by_id("main") {
            let _ = tray.set_tooltip(Some(&tip));
            if let Ok(img) = tauri::image::Image::from_bytes(icon(on)) {
                let _ = tray.set_icon(Some(img));
                let _ = tray.set_icon_as_template(cfg!(target_os = "macos"));
            }
        }
    });
}
