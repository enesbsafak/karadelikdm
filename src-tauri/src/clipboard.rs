//! Pano izleme: açıksa, kopyalanan ve yakalanacak uzantılardan biriyle biten adres için ekleme penceresi açılır.

use crate::kcore::{app, settings};
use crate::windows::{self, AddSpec};

fn read() -> Option<String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app()?.clipboard().read_text().ok()
}

/// Metinden indirilebilir adresler (satır satır).
pub fn urls_in(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>'))
        .filter(|w| {
            ((w.starts_with("http://") || w.starts_with("https://") || w.starts_with("ftp://") || w.starts_with("ftps://")) && url::Url::parse(w).is_ok())
                || kdm_torrent::parse_magnet(w).is_ok()
        })
        .map(String::from)
        .collect()
}

fn wanted(url: &str, exts: &[String]) -> bool {
    // Magnet bağlantısının uzantısı yoktur; kopyalanması indirme isteğidir
    if kdm_torrent::is_magnet(url) {
        return true;
    }
    let name = kdm_core::engine::filename::from_url(url).unwrap_or_default();
    let ext = kdm_core::engine::filename::extension(&name);
    !ext.is_empty() && exts.contains(&ext)
}

pub fn start() {
    std::thread::spawn(|| {
        let mut last = read().unwrap_or_default();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(800));
            let s = settings();
            if !s.clipboard_monitor {
                last = read().unwrap_or_default();
                continue;
            }
            let Some(text) = read() else { continue };
            if text == last {
                continue;
            }
            last = text.clone();
            let exts = s.capture_ext_list();
            let urls: Vec<String> = urls_in(&text).into_iter().filter(|u| wanted(u, &exts)).collect();
            if !urls.is_empty() {
                windows::open_add(AddSpec { url: urls[0].clone(), urls, ..Default::default() });
            }
        }
    });
}

/// Tepsi > Ekle: panoda adres varsa onunla açılır.
pub fn open_from_clipboard() {
    let urls = read().map(|t| urls_in(&t)).unwrap_or_default();
    windows::open_add(AddSpec { url: urls.first().cloned().unwrap_or_default(), urls, ..Default::default() });
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_urls() {
        let u = super::urls_in("indir: https://a.com/x.zip ve \"http://b.org/y.iso\" ftp://c/d ftps://e/f nope mailto:x@y");
        assert_eq!(u, vec!["https://a.com/x.zip", "http://b.org/y.iso", "ftp://c/d", "ftps://e/f"]);
        assert!(super::wanted("https://a.com/x.ZIP?a=1", &["zip".into()]));
        let m = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x";
        assert_eq!(super::urls_in(&format!("bak {m} magnet:?dn=bozuk")), vec![m]);
        assert!(super::wanted(m, &[]));
        assert!(!super::wanted("https://a.com/page", &["zip".into()]));
    }
}
