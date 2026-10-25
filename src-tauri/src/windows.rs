//! Pencereler: ana pencere, indirme ekleme, tercihler, kuyruklar ve küçük diyaloglar.

use crate::kcore::app;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Field {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DialogSpec {
    /// input | confirm | info | countdown
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    /// Onaylanınca çalışacak eylem kimliği; alan değerleri eklenir
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub danger: bool,
    /// countdown: saniye
    #[serde(default)]
    pub seconds: u32,
}

/// İndirme ekleme penceresinin ön doldurması (tarayıcı, pano, CLI ya da elle).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AddSpec {
    pub url: String,
    pub file_name: String,
    pub referer: String,
    pub cookies: String,
    pub user_agent: String,
    pub size: Option<u64>,
    pub dir: String,
    pub queue: String,
    /// Birden çok bağlantı (toplu ekleme)
    pub urls: Vec<String>,
    /// Tarayıcı uzantısı akış olarak bildirdi (HLS/DASH)
    pub media: bool,
}

static N: AtomicU32 = AtomicU32::new(0);
static DIALOGS: once_cell::sync::Lazy<Mutex<HashMap<String, DialogSpec>>> = once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));
static ADDS: once_cell::sync::Lazy<Mutex<HashMap<String, AddSpec>>> = once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));
/// Araç pencerelerinin açılış verisi (ZIP önizleme adresi vb.)
static PAGES: once_cell::sync::Lazy<Mutex<HashMap<String, serde_json::Value>>> = once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

fn themed<'a>(b: WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle>) -> WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle> {
    if cfg!(target_os = "macos") {
        b.theme(Some(tauri::Theme::Dark))
    } else {
        b
    }
}

/// Windows'ta pencere ana iş parçacığındaki bir olay işleyicisinden oluşturulursa WebView2 kilitlenir
/// (Tauri: WebviewWindowBuilder::build belgesi); bu yüzden pencereler ayrı iş parçacığından oluşturulur.
fn spawn_window(label: String, page: &str, title: String, w: f64, h: f64, resizable: bool, on_top: bool) {
    let Some(app) = app() else { return };
    let app = app.clone();
    let page = page.to_string();
    std::thread::spawn(move || {
        let b = themed(WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(page.into())))
            .title(title)
            .inner_size(w, h)
            .resizable(resizable)
            .minimizable(resizable)
            .maximizable(resizable)
            .always_on_top(on_top)
            .center();
        let b = if resizable { b.min_inner_size(w * 0.7, h * 0.7) } else { b };
        if let Ok(win) = b.build() {
            let _ = win.set_focus();
        }
    });
}

fn focus_existing(label: &str) -> bool {
    if let Some(w) = app().and_then(|a| a.get_webview_window(label)) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        true
    } else {
        false
    }
}

pub fn dialog_spec(label: &str) -> Option<DialogSpec> {
    DIALOGS.lock().get(label).cloned()
}

pub fn open_dialog(spec: DialogSpec) {
    let label = format!("dialog-{}", N.fetch_add(1, Ordering::Relaxed));
    // İleti satırlarına göre yükseklik (pencere 440 px; satır başına yaklaşık 52 karakter sığar)
    let lines: f64 = spec.message.lines().map(|l| (l.chars().count() as f64 / 52.0).ceil().max(1.0)).sum();
    let height = match spec.kind.as_str() {
        "info" if !spec.text.is_empty() => 420.0,
        "input" => 170.0 + 64.0 * spec.fields.len() as f64,
        "countdown" => 230.0 + 20.0 * lines,
        _ => (150.0 + 20.0 * lines).clamp(210.0, 520.0),
    };
    let title = spec.title.clone();
    DIALOGS.lock().insert(label.clone(), spec);
    spawn_window(label, "dialog.html", title, 440.0, height, false, true);
}

pub fn confirm(title: &str, message: &str, action: &str, danger: bool) {
    open_dialog(DialogSpec { kind: "confirm".into(), title: title.into(), message: message.into(), action: action.into(), danger, ..Default::default() });
}

pub fn info(title: &str, message: &str, text: &str) {
    open_dialog(DialogSpec { kind: "info".into(), title: title.into(), message: message.into(), text: text.into(), ..Default::default() });
}

pub fn input(title: &str, label: &str, action: &str, value: &str) {
    open_dialog(DialogSpec {
        kind: "input".into(),
        title: title.into(),
        fields: vec![Field { name: "value".into(), label: label.into(), kind: "text".into(), value: value.into() }],
        action: action.into(),
        ..Default::default()
    });
}

/// Süre dolunca `action` çalışır; kullanıcı vazgeçebilir.
pub fn countdown(title: &str, message: &str, seconds: u32, action: &str) {
    open_dialog(DialogSpec { kind: "countdown".into(), title: title.into(), message: message.into(), action: action.into(), seconds, danger: true, ..Default::default() });
}

pub fn close_dialog(label: &str) {
    crate::torrent::cancel(label);
    DIALOGS.lock().remove(label);
    ADDS.lock().remove(label);
    if let Some(w) = app().and_then(|a| a.get_webview_window(label)) {
        let _ = w.close();
    }
}

pub fn show_main() {
    focus_existing("main");
}

pub fn add_spec(label: &str) -> Option<AddSpec> {
    ADDS.lock().get(label).cloned()
}

pub fn open_add(spec: AddSpec) {
    let label = format!("add-{}", N.fetch_add(1, Ordering::Relaxed));
    let h = if spec.urls.len() > 1 { 560.0 } else { 470.0 };
    ADDS.lock().insert(label.clone(), spec);
    spawn_window(label, "add.html", crate::i18n::t("add.title"), 620.0, h, false, true);
}

pub fn open_prefs() {
    if !focus_existing("prefs") {
        spawn_window("prefs".into(), "prefs.html", crate::i18n::t("prefs.title"), 680.0, 680.0, false, false);
    }
}

pub fn page_spec(label: &str) -> serde_json::Value {
    PAGES.lock().get(label).cloned().unwrap_or(serde_json::Value::Null)
}

pub fn open_grabber() {
    if !focus_existing("grabber") {
        spawn_window("grabber".into(), "grabber.html", crate::i18n::t("grab.title"), 720.0, 640.0, true, false);
    }
}

/// ZIP önizleme: `spec` = { url, referer, cookies, dir }
pub fn open_zip(spec: serde_json::Value) {
    let label = format!("zip-{}", N.fetch_add(1, Ordering::Relaxed));
    PAGES.lock().insert(label.clone(), spec);
    spawn_window(label, "zip.html", crate::i18n::t("zip.title"), 640.0, 560.0, true, false);
}

pub fn open_queues() {
    if !focus_existing("queues") {
        spawn_window("queues".into(), "queues.html", crate::i18n::t("queues.title"), 760.0, 660.0, true, false);
    }
}
