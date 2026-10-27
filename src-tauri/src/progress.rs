//! İndirme ilerleme penceresi ve "İndirme tamamlandı" penceresi. Kullanıcının tek tek başlattığı indirmeler
//! (ekleme penceresinde "Şimdi indir", tarayıcıdan doğrudan başlayan, listede elle başlatılan) için açılır;
//! kuyruk ve toplu eklemeler sessiz kalır. Aynı pencere indirme bitince "tamamlandı" görünümüne geçer.

use crate::kcore::{app, engine, settings};
use kdm_core::engine::Status;
use parking_lot::Mutex;
use std::collections::HashSet;
use tauri::Manager;

/// Bitince "tamamlandı" penceresi gösterilecek indirmeler
static WATCHED: once_cell::sync::Lazy<Mutex<HashSet<u64>>> = once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));

pub fn label(id: u64) -> String {
    format!("progress-{id}")
}

/// Pencere etiketinden indirme kimliği.
pub fn id_of(label: &str) -> Option<u64> {
    label.strip_prefix("progress-")?.parse().ok()
}

fn window(id: u64) -> Option<tauri::WebviewWindow> {
    app()?.get_webview_window(&label(id))
}

/// Kullanıcı bir indirmeyi başlattı.
pub fn started(id: u64) {
    let s = settings();
    if !s.show_progress && !s.show_complete {
        return;
    }
    WATCHED.lock().insert(id);
    if s.show_progress {
        open(id);
    }
}

/// Pencereyi açar ya da öne getirir (listede çift tık, sağ tık menüsü).
pub fn open(id: u64) {
    let Some(rec) = engine().record(id) else { return };
    crate::windows::open_progress(&label(id), &rec.file_name);
}

pub fn finished(id: u64, status: Status) {
    let watched = if status == Status::Completed { WATCHED.lock().remove(&id) } else { WATCHED.lock().contains(&id) };
    if status != Status::Completed {
        // Hata ya da durdurma: açık pencere durumu gösterir
        return;
    }
    let show = watched && settings().show_complete;
    match (window(id), show) {
        (Some(w), true) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        (Some(w), false) => {
            let _ = w.close();
        }
        (None, true) => open(id),
        (None, false) => {}
    }
}

/// İndirme listeden kaldırıldı.
pub fn forget(id: u64) {
    WATCHED.lock().remove(&id);
    if let Some(w) = window(id) {
        let _ = w.close();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn label_round_trip() {
        assert_eq!(super::id_of(&super::label(42)), Some(42));
        assert_eq!(super::id_of("dialog-3"), None);
        assert_eq!(super::id_of("progress-x"), None);
    }
}
