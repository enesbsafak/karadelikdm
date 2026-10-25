//! Karadelik Download Manager - Windows ve macOS için indirme yöneticisi.

mod actions;
mod bridge;
mod cli;
mod clipboard;
mod commands;
mod i18n;
mod kcore;
mod paths;
mod settings;
mod system;
mod tools;
mod torrent;
mod tray;
mod updater;
mod windows;

use std::time::Duration;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WindowEvent};

/// Çıkış: indirmeler duraklatılır, ilerleme ve kuyruk durumu kaydedilir.
pub fn exit_app() {
    bridge::stop();
    kcore::persist_runtime_state();
    kcore::engine().shutdown(Duration::from_secs(5));
    if let Some(app) = kcore::app() {
        app.exit(0);
    } else {
        std::process::exit(0);
    }
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    kcore::init(paths::Paths::detect());
    let start_args = args.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, argv, _cwd| {
            // İkinci örnek: adresleri çalışan örneğe ilet, yoksa pencereyi öne getir
            if !cli::handle(&argv) {
                windows::show_main();
            }
        }))
        .plugin(tauri_plugin_autostart::Builder::new().args(["--autostart"]).app_name("Karadelik Download Manager").build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .on_menu_event(|_app, ev| {
            let id = ev.id().as_ref().to_string();
            std::thread::spawn(move || actions::dispatch(&id));
        })
        .invoke_handler(tauri::generate_handler![
            commands::i18n_table,
            commands::get_state,
            commands::run_action,
            commands::dl_action,
            commands::reorder,
            commands::add_get,
            commands::add_submit,
            commands::probe_url,
            commands::suggest_dir,
            commands::find_url,
            commands::media_variants,
            commands::add_downloads_cmd,
            commands::page_spec,
            tools::grab_start,
            tools::grab_stop,
            tools::zip_list,
            tools::zip_extract,
            commands::settings_get,
            commands::settings_save,
            commands::queues_get,
            commands::queues_save,
            commands::pick_folder,
            commands::dialog_get,
            commands::dialog_submit,
            commands::dialog_close,
            commands::confirm_remove,
            commands::open_add_url,
            torrent::torrent_inspect,
            torrent::torrent_inspect_cancel,
            torrent::pick_torrent_file,
            torrent::torrent_register_handlers,
        ])
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    if kcore::settings().close_to_tray {
                        api.prevent_close();
                        let _ = window.hide();
                        static ONCE: std::sync::Once = std::sync::Once::new();
                        ONCE.call_once(|| kcore::notify(&i18n::t("msg.minimized_here")));
                    } else {
                        api.prevent_close();
                        std::thread::spawn(exit_app);
                    }
                }
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let _ = kcore::core().app.set(handle.clone());
            kcore::log(&format!("Karadelik Download Manager {} başlatıldı", env!("CARGO_PKG_VERSION")));

            let menu = tray::build(&handle)?;
            TrayIconBuilder::with_id("main")
                .icon(tauri::image::Image::from_bytes(tray::icon(false))?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Karadelik Download Manager")
                .menu(&menu)
                .show_menu_on_left_click(cfg!(target_os = "macos"))
                .on_tray_icon_event(|_tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        windows::show_main();
                    }
                })
                .build(app)?;

            let s = kcore::settings();
            {
                use tauri_plugin_autostart::ManagerExt;
                if s.run_at_startup {
                    let _ = app.autolaunch().enable();
                }
            }
            let a = cli::parse(&start_args);
            if !(a.minimized || s.start_minimized) {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                }
            }
            clipboard::start();
            updater::start_background();
            std::thread::spawn(bridge::start);
            if !a.urls.is_empty() {
                let argv = start_args.clone();
                std::thread::spawn(move || {
                    // Ana pencere hazır olsun
                    std::thread::sleep(Duration::from_millis(300));
                    cli::handle(&argv);
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Karadelik Download Manager başlatılamadı");

    app.run(|_app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            windows::show_main();
        }
        if let RunEvent::Exit = event {
            kcore::persist_runtime_state();
            kcore::engine().shutdown(Duration::from_secs(3));
        }
    });
}
