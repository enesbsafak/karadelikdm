//! Arayüzün çağırdığı Tauri komutları.

use crate::i18n;
use crate::kcore::{self, engine, sched, settings};
use crate::settings::Settings;
use crate::windows::{self, AddSpec};
use kdm_core::category;
use kdm_core::engine::http::RequestSpec;
use kdm_core::engine::{Auth, Checksum, HashAlgo, Info, MediaSpec, NewDownload, Probe, Status, TorrentSpec};
use kdm_core::queue::Queue;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)
}

#[tauri::command]
pub fn i18n_table() -> HashMap<String, String> {
    i18n::table(settings().lang())
}

#[derive(Serialize)]
pub struct QueueView {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub max_parallel: u32,
}

#[derive(Serialize)]
pub struct CategoryView {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct State {
    pub downloads: Vec<Info>,
    pub queues: Vec<QueueView>,
    pub categories: Vec<CategoryView>,
    pub speed_limit_enabled: bool,
    pub speed_limit: u64,
    pub quota_until: Option<i64>,
    pub clipboard_monitor: bool,
    pub message: String,
    pub version: &'static str,
    /// Yeni sürüm varsa numarası
    pub update: Option<String>,
}

fn queue_views() -> Vec<QueueView> {
    let active = sched().active_ids();
    sched()
        .queues()
        .iter()
        .map(|q| QueueView { id: q.id.clone(), name: kcore::queue_name(q), active: active.contains(&q.id), max_parallel: q.max_parallel })
        .collect()
}

#[tauri::command]
pub fn get_state() -> State {
    let s = settings();
    State {
        downloads: engine().infos(),
        queues: queue_views(),
        categories: s.categories.iter().map(|c| CategoryView { id: c.id.clone(), name: i18n::t(&format!("cat.{}", c.id)) }).collect(),
        speed_limit_enabled: s.speed_limit_enabled,
        speed_limit: s.speed_limit,
        quota_until: *kcore::core().quota_until.read(),
        clipboard_monitor: s.clipboard_monitor,
        message: kcore::core().message.read().clone(),
        version: env!("CARGO_PKG_VERSION"),
        update: crate::updater::available(),
    }
}

#[tauri::command]
pub fn run_action(id: String) {
    // Pencere açma vb. ana iş parçacığını bekletmesin
    std::thread::spawn(move || crate::actions::dispatch(&id));
}

/// Seçili indirmeler üzerinde eylem.
#[tauri::command]
pub async fn dl_action(ids: Vec<u64>, action: String) -> Res<()> {
    blocking(move || dl_action_sync(&ids, &action)).await?
}

fn dl_action_sync(ids: &[u64], action: &str) -> Res<()> {
    let e = engine();
    let (head, arg) = action.split_once(':').unwrap_or((action, ""));
    for &id in ids {
        match head {
            "start" => {
                e.start(id);
            }
            "stop" => e.stop(id),
            "remove" => e.remove(id, false).map_err(err)?,
            "restart" => {
                e.stop(id);
                wait_stopped(id);
                e.restart(id).map_err(err)?;
                e.start(id);
            }
            "queue" => sched().enqueue(id, arg),
            "limit" => e.set_speed_limit(id, arg.parse().unwrap_or(0)),
            "open" | "folder" => {
                let rec = e.record(id).ok_or("?")?;
                let path = if rec.status == Status::Completed { rec.target_path() } else { rec.part_path() };
                use tauri_plugin_opener::OpenerExt;
                let app = kcore::app().ok_or("?")?;
                if head == "open" && rec.status == Status::Completed {
                    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(err)?;
                } else if path.exists() {
                    app.opener().reveal_item_in_dir(&path).map_err(err)?;
                } else {
                    app.opener().open_path(rec.dir.to_string_lossy(), None::<&str>).map_err(err)?;
                }
            }
            "copy_url" => {
                let rec = e.record(id).ok_or("?")?;
                use tauri_plugin_clipboard_manager::ClipboardExt;
                if let Some(app) = kcore::app() {
                    let _ = app.clipboard().write_text(rec.url);
                }
            }
            "zip" => {
                let rec = e.record(id).ok_or("?")?;
                windows::open_zip(serde_json::json!({ "url": rec.url, "referer": rec.referer, "cookies": rec.cookies, "dir": rec.dir }));
            }
            "refresh_url" => {
                let rec = e.record(id).ok_or("?")?;
                windows::input(&i18n::t("refresh.title"), &i18n::t("refresh.label"), &format!("refresh_url:{id}"), &rec.url);
            }
            _ => return Err(format!("bilinmeyen eylem: {action}")),
        }
    }
    Ok(())
}

fn wait_stopped(id: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while engine().is_running(id) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Kuyruk içi sıralama: verilen kimlikler bu sırayla en üste dizilir.
#[tauri::command]
pub fn reorder(ids: Vec<u64>) -> Res<()> {
    for (i, id) in ids.iter().enumerate() {
        let _ = engine().update(*id, |r| r.order = i as u64 + 1);
    }
    Ok(())
}

// ---------- indirme ekleme ----------

#[derive(Serialize)]
pub struct AddView {
    pub spec: AddSpec,
    pub queues: Vec<QueueView>,
    pub download_dir: String,
    pub use_categories: bool,
}

#[tauri::command]
pub fn add_get(window: tauri::WebviewWindow) -> AddView {
    let s = settings();
    AddView {
        spec: windows::add_spec(window.label()).unwrap_or_default(),
        queues: queue_views(),
        download_dir: s.download_dir.clone(),
        use_categories: s.use_categories,
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct AddForm {
    pub urls: Vec<String>,
    pub file_name: String,
    /// Boş: kategoriye göre
    pub dir: String,
    pub queue: String,
    /// now | queue | later
    pub mode: String,
    pub checksum_algo: String,
    pub checksum: String,
    pub user: String,
    pub password: String,
    pub referer: String,
    pub cookies: String,
    pub user_agent: String,
    pub description: String,
    pub connections: u32,
    /// HLS/DASH akışı olarak indir
    pub media: bool,
    pub media_video: String,
    pub media_audio: String,
    /// Ekleme penceresinde okunmuş torrent (adres magnet bağlantısıdır)
    pub torrent_hash: String,
    /// Seçili dosyalar; `None`: tümü
    pub torrent_files: Option<Vec<usize>>,
    /// Torrentin adı / tek dosyalıysa dosya adı (kategori tahmini için)
    pub torrent_name: String,
}

/// Kategoriye göre klasör (dosya adı henüz kesin değilse URL'den tahmin).
pub fn resolve_dir(s: &Settings, dir: &str, name_hint: &str) -> (PathBuf, String) {
    if !dir.trim().is_empty() {
        let cat = category::classify(name_hint, &s.categories).map(|c| c.id.clone()).unwrap_or_default();
        return (PathBuf::from(dir.trim()), cat);
    }
    let base = PathBuf::from(&s.download_dir);
    let cat = category::classify(name_hint, &s.categories);
    let d = if s.use_categories { category::dir_for(&base, cat) } else { base };
    (d, cat.map(|c| c.id.clone()).unwrap_or_default())
}

#[tauri::command]
pub async fn add_submit(window: tauri::WebviewWindow, form: AddForm) -> Res<Vec<u64>> {
    let label = window.label().to_string();
    let ids = blocking(move || add_downloads(form)).await??;
    windows::close_dialog(&label);
    Ok(ids)
}

pub fn add_downloads(form: AddForm) -> Res<Vec<u64>> {
    let s = settings();
    let urls: Vec<String> = form.urls.iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
    if urls.is_empty() {
        return Err(i18n::t("add.no_url"));
    }
    let checksum = (!form.checksum.trim().is_empty()).then(|| Checksum {
        algo: match form.checksum_algo.as_str() {
            "md5" => HashAlgo::Md5,
            "sha1" => HashAlgo::Sha1,
            _ => HashAlgo::Sha256,
        },
        hex: form.checksum.trim().to_string(),
    });
    let auth = (!form.user.is_empty()).then(|| Auth { user: form.user.clone(), password: form.password.clone() });
    let queue = if form.queue.is_empty() { kdm_core::queue::MAIN.to_string() } else { form.queue.clone() };
    let mut ids = Vec::new();
    for url in &urls {
        // Tek adreste kullanıcının verdiği ad; toplu eklemede her dosya kendi adını alır
        let name = if urls.len() == 1 { form.file_name.clone() } else { String::new() };
        let hint = if name.is_empty() { kdm_core::engine::filename::from_url(url).unwrap_or_default() } else { name.clone() };
        // Akışların çıktısı videodur; adres adı (master.m3u8) kategoriyi yanıltmasın
        let hint = if form.media { format!("{hint}.mp4") } else { hint };
        let torrent = (urls.len() == 1 && !form.torrent_hash.is_empty())
            .then(|| TorrentSpec { info_hash: form.torrent_hash.clone(), files: form.torrent_files.clone(), ..Default::default() });
        let hint = if torrent.is_some() && !form.torrent_name.is_empty() && name.is_empty() { form.torrent_name.clone() } else { hint };
        let (dir, cat) = resolve_dir(&s, &form.dir, &hint);
        let nd = NewDownload {
            url: url.clone(),
            file_name: name,
            dir: String::new(),
            referer: form.referer.clone(),
            cookies: form.cookies.clone(),
            user_agent: form.user_agent.clone(),
            auth: auth.clone(),
            checksum: if urls.len() == 1 { checksum.clone() } else { None },
            max_connections: form.connections,
            queue: queue.clone(),
            start: form.mode == "now",
            description: form.description.clone(),
            media: form.media.then(|| MediaSpec {
                video: (!form.media_video.is_empty()).then(|| form.media_video.clone()),
                audio: (!form.media_audio.is_empty()).then(|| form.media_audio.clone()),
            }),
            torrent,
            ..Default::default()
        };
        let id = engine().add(nd, dir, &cat).map_err(err)?;
        if form.mode == "queue" {
            sched().enqueue(id, &queue);
        }
        ids.push(id);
    }
    kcore::emit("state", ());
    Ok(ids)
}

#[tauri::command]
pub async fn probe_url(url: String, referer: String, cookies: String, user_agent: String) -> Res<Probe> {
    let net = settings().net;
    blocking(move || {
        let spec = RequestSpec { url, referer, cookies, user_agent, ..Default::default() };
        kdm_core::engine::probe(&net, &spec).map_err(|e| format!("{e:#}"))
    })
    .await?
}

/// HLS/DASH ise türü ve kalite seçenekleri; değilse `None`.
#[tauri::command]
pub async fn media_variants(url: String, referer: String, cookies: String, user_agent: String) -> Res<Option<serde_json::Value>> {
    let net = settings().net;
    blocking(move || {
        let spec = RequestSpec { url, referer, cookies, user_agent, ..Default::default() };
        kdm_core::engine::media::variants(&net, &spec)
            .map(|o| o.map(|(kind, list)| serde_json::json!({ "kind": kind, "variants": list })))
            .map_err(|e| format!("{e:#}"))
    })
    .await?
}

/// Aynı adres listede var mı (ekleme penceresi uyarısı).
#[tauri::command]
pub fn find_url(url: String) -> Option<Info> {
    let url = url.trim();
    engine().infos().into_iter().find(|i| i.url == url)
}

/// Kategoriye göre önerilen klasör (ekleme penceresinde ad değişince).
#[tauri::command]
pub fn suggest_dir(name: String) -> String {
    resolve_dir(&settings(), "", &name).0.to_string_lossy().into_owned()
}

// ---------- ayarlar ve kuyruklar ----------

#[derive(Serialize)]
pub struct SettingsView {
    pub settings: Settings,
    pub languages: Vec<(&'static str, &'static str)>,
    pub default_av: String,
    pub default_capture: &'static str,
    pub data_dir: String,
    pub version: &'static str,
}

#[derive(Serialize)]
pub struct UpdateView {
    current: &'static str,
    /// Daha yeni sürüm varsa numarası
    latest: Option<String>,
}

/// Tercihler > Genel > "Şimdi denetle".
#[tauri::command]
pub async fn update_check_now() -> Res<UpdateView> {
    let r = blocking(crate::updater::check).await?;
    let latest = r.map_err(|e| format!("{e:#}"))?;
    if latest.is_some() {
        kcore::emit("state", ());
    }
    Ok(UpdateView { current: env!("CARGO_PKG_VERSION"), latest })
}

#[tauri::command]
pub fn settings_get() -> SettingsView {
    SettingsView {
        settings: settings(),
        languages: i18n::LANGUAGES.to_vec(),
        default_av: crate::settings::default_av_command(),
        default_capture: crate::settings::DEFAULT_CAPTURE,
        data_dir: kcore::core().paths.data.to_string_lossy().into_owned(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
pub fn settings_save(mut s: Settings) -> Res<()> {
    kdm_core::engine::Engine::check_net(&s.net).map_err(|e| format!("{e:#}"))?;
    // Kuyruklar ayrı pencereden yönetilir; buradan gelen liste eskimiş olabilir
    s.queues = sched().queues();
    s.first_run = false;
    let lang_changed = s.lang() != settings().lang();
    let autostart = s.run_at_startup;
    kcore::update_settings(|cur| *cur = s);
    if let Some(app) = kcore::app() {
        use tauri_plugin_autostart::ManagerExt;
        let al = app.autolaunch();
        let _ = if autostart { al.enable() } else { al.disable() };
    }
    crate::bridge::broadcast();
    if lang_changed {
        kcore::emit("i18n", i18n::table(settings().lang()));
        crate::tray::refresh();
    }
    kcore::emit("state", ());
    Ok(())
}

#[tauri::command]
pub fn queues_get() -> serde_json::Value {
    serde_json::json!({
        "queues": sched().queues(),
        "active": sched().active_ids(),
        "downloads": engine().infos(),
        "main_name": i18n::t("queue.main"),
    })
}

#[tauri::command]
pub fn queues_save(queues: Vec<Queue>) -> Res<()> {
    sched().set_queues(queues.clone());
    kcore::update_settings(|s| s.queues = queues);
    kcore::emit("state", ());
    Ok(())
}

#[tauri::command]
pub async fn pick_folder(current: String) -> Option<String> {
    let app = kcore::app()?.clone();
    blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let mut d = app.dialog().file();
        if !current.is_empty() {
            d = d.set_directory(current);
        }
        d.blocking_pick_folder().map(|p| p.to_string())
    })
    .await
    .ok()
    .flatten()
}

// ---------- diyaloglar ----------

#[tauri::command]
pub fn dialog_get(window: tauri::WebviewWindow) -> Option<windows::DialogSpec> {
    windows::dialog_spec(window.label())
}

#[tauri::command]
pub fn dialog_submit(window: tauri::WebviewWindow, values: Vec<(String, String)>) {
    let label = window.label().to_string();
    if let Some(spec) = windows::dialog_spec(&label) {
        windows::close_dialog(&label);
        if !spec.action.is_empty() {
            std::thread::spawn(move || crate::actions::dispatch_values(&spec.action, &values));
        }
    } else {
        windows::close_dialog(&label);
    }
}

#[tauri::command]
pub fn dialog_close(window: tauri::WebviewWindow) {
    windows::close_dialog(window.label());
}

/// Grabber'dan doğrudan ekleme (pencere kapanmaz; çağıran kapatır).
#[tauri::command]
pub async fn add_downloads_cmd(form: AddForm) -> Res<Vec<u64>> {
    blocking(move || add_downloads(form)).await?
}

#[tauri::command]
pub fn page_spec(window: tauri::WebviewWindow) -> serde_json::Value {
    windows::page_spec(window.label())
}

/// Sürükle-bırak ya da yapıştırılan adreslerle ekleme penceresi.
#[tauri::command]
pub fn open_add_url(urls: Vec<String>) {
    let urls: Vec<String> = urls.iter().flat_map(|u| crate::clipboard::urls_in(u)).collect();
    if urls.is_empty() {
        return;
    }
    std::thread::spawn(move || windows::open_add(AddSpec { url: urls[0].clone(), urls, ..Default::default() }));
}

#[tauri::command]
pub fn confirm_remove(ids: Vec<u64>) {
    let names: Vec<String> = ids.iter().filter_map(|id| engine().record(*id)).map(|r| r.file_name).collect();
    windows::confirm(
        &i18n::t("remove.title"),
        &i18n::tf("remove.files", &[&names.join("\n")]),
        &format!("remove_files:{}", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")),
        true,
    );
}
