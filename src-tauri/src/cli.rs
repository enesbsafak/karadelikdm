//! Komut satırı: `karadelikdm [adres...] [--dir KLASÖR] [--name AD] [--queue KUYRUK] [--start|--later]`.
//! Uygulama zaten açıksa argümanlar tek örnek eklentisiyle çalışan örneğe iletilir.
//! `--start` / `--later` verilmezse ekleme penceresi açılır.

use crate::commands::{add_downloads, AddForm};
use crate::kcore;
use crate::windows::{self, AddSpec};

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub urls: Vec<String>,
    pub dir: String,
    pub name: String,
    pub queue: String,
    /// "now" | "later" | "queue" | "" (pencere)
    pub mode: String,
    pub minimized: bool,
}

pub fn parse(argv: &[String]) -> Args {
    let mut a = Args::default();
    let mut it = argv.iter().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--dir" | "/p" => a.dir = it.next().cloned().unwrap_or_default(),
            "--name" | "/f" => a.name = it.next().cloned().unwrap_or_default(),
            "--queue" => {
                a.queue = it.next().cloned().unwrap_or_default();
                if a.mode.is_empty() {
                    a.mode = "queue".into();
                }
            }
            // IDM yardımında: /n sessiz mod (soru sormadan başlat), /a kuyruğa ekle ama başlatma
            "--start" | "/n" => a.mode = "now".into(),
            "--later" => a.mode = "later".into(),
            "/a" => a.mode = "queue".into(),
            "--autostart" | "--minimized" => a.minimized = true,
            "/d" => a.urls.extend(it.next().cloned()),
            s if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("ftp://") || s.starts_with("ftps://") => a.urls.push(s.to_string()),
            // Tarayıcıdan magnet bağlantısı ya da Gezgin'den .torrent dosyası (varsayılan uygulama olunca)
            s if kdm_torrent::is_magnet(s) || crate::torrent::is_torrent_path(s) => a.urls.push(s.to_string()),
            _ => {}
        }
    }
    a
}

/// macOS'un "bununla aç" olayındaki adresleri komut satırı biçimine çevirir: `file://` yol olur, magnet aynen kalır.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn opened_to_argv(urls: &[url::Url]) -> Vec<String> {
    let items = urls.iter().filter_map(|u| {
        if u.scheme() == "file" {
            u.to_file_path().ok().map(|p| p.to_string_lossy().into_owned())
        } else {
            Some(u.to_string())
        }
    });
    std::iter::once(String::new()).chain(items).collect()
}

/// Argümanlarda indirme varsa işler. `true`: bir şey yapıldı.
pub fn handle(argv: &[String]) -> bool {
    let a = parse(argv);
    if a.urls.is_empty() {
        return false;
    }
    if a.mode.is_empty() {
        windows::open_add(AddSpec { url: a.urls[0].clone(), file_name: a.name, dir: a.dir, queue: a.queue, urls: a.urls, ..Default::default() });
    } else {
        let form = AddForm { urls: a.urls, file_name: a.name, dir: a.dir, queue: a.queue, mode: a.mode, ..Default::default() };
        if let Err(e) = add_downloads(form) {
            kcore::notify(&e);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        std::iter::once("kdm").chain(s.iter().copied()).map(String::from).collect()
    }

    #[test]
    fn parses_modern_and_legacy_flags() {
        let a = parse(&v(&["https://x/a.zip", "--dir", "D:\\dl", "--start"]));
        assert_eq!(a.urls, vec!["https://x/a.zip"]);
        assert_eq!(a.dir, "D:\\dl");
        assert_eq!(a.mode, "now");
        let b = parse(&v(&["/d", "https://x/b.iso", "/p", "C:\\t", "/f", "b.iso", "/a"]));
        assert_eq!((b.urls[0].as_str(), b.dir.as_str(), b.name.as_str(), b.mode.as_str()), ("https://x/b.iso", "C:\\t", "b.iso", "queue"));
        assert_eq!(parse(&v(&["https://x/d", "--later"])).mode, "later");
        assert_eq!(parse(&v(&["--queue", "night", "https://x/c"])).mode, "queue");
        assert!(parse(&v(&["--autostart"])).minimized);
        let t = parse(&v(&["magnet:?xt=urn:btih:abc", r"C:\a b\x.torrent", "notes.txt"]));
        assert_eq!(t.urls, vec!["magnet:?xt=urn:btih:abc", r"C:\a b\x.torrent"]);
    }

    #[test]
    fn opened_urls_become_arguments() {
        let file = if cfg!(windows) { "file:///C:/Users/a%20b/film.torrent" } else { "file:///Users/a%20b/film.torrent" };
        let magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x";
        let urls: Vec<url::Url> = [file, magnet].iter().map(|u| url::Url::parse(u).unwrap()).collect();
        let a = parse(&opened_to_argv(&urls));
        let want_path = if cfg!(windows) { r"C:\Users\a b\film.torrent" } else { "/Users/a b/film.torrent" };
        assert_eq!(a.urls, vec![want_path.to_string(), magnet.to_string()]);
    }
}
