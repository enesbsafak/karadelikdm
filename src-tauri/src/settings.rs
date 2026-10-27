//! `settings.json`: tüm kullanıcı ayarları. Bilinmeyen/eksik alanlar varsayılanla doldurulur.

use crate::paths::{default_download_dir, Paths};
use kdm_core::category::{self, Category};
use kdm_core::engine::http::NetConfig;
use kdm_core::engine::TorrentSettings;
use kdm_core::queue::Queue;
use kdm_core::quota::QuotaCfg;
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "" : sistem dili
    pub language: String,
    pub download_dir: String,
    pub use_categories: bool,
    pub categories: Vec<Category>,
    pub max_connections: u32,
    pub speed_limit_enabled: bool,
    /// bayt/sn
    pub speed_limit: u64,
    pub net: NetConfig,
    pub queues: Vec<Queue>,
    pub quota: QuotaCfg,
    pub run_at_startup: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub notify_complete: bool,
    pub clipboard_monitor: bool,
    /// Pano ve tarayıcıdan yakalanacak uzantılar
    pub capture_exts: String,
    pub browser_enabled: bool,
    /// Tarayıcıdan gelen indirmede ekleme penceresini göster (kapalıysa hemen başlar)
    pub browser_show_dialog: bool,
    /// Bu boyuttan küçük dosyaları tarayıcıya bırak (KB)
    pub browser_min_size_kb: u64,
    pub av_enabled: bool,
    /// `{file}` yer tutucusu dosya yolunu alır
    pub av_command: String,
    pub ffmpeg_path: String,
    /// BitTorrent: port, DHT, paylaşma oranı
    pub torrent: TorrentSettings,
    pub update_check: bool,
    /// Kullanıcının başlattığı indirmede ilerleme penceresi
    pub show_progress: bool,
    /// Bitince "İndirme tamamlandı" penceresi
    pub show_complete: bool,
    pub first_run: bool,
}

pub const DEFAULT_CAPTURE: &str = "zip rar 7z gz tgz bz2 xz tar iso img exe msi msix apk dmg pkg deb rpm appimage \
     mp4 mkv webm avi mov wmv flv m4v mpg mpeg 3gp mp3 m4a aac flac wav ogg opus pdf epub doc docx xls xlsx ppt pptx bin";

pub fn default_av_command() -> String {
    if cfg!(windows) {
        r#""C:\Program Files\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File "{file}" -DisableRemediation"#.into()
    } else {
        String::new()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: String::new(),
            download_dir: default_download_dir().to_string_lossy().into_owned(),
            use_categories: true,
            categories: category::defaults(),
            max_connections: 8,
            speed_limit_enabled: false,
            speed_limit: 1024 * 1024,
            net: NetConfig::default(),
            queues: vec![Queue::default()],
            quota: QuotaCfg::default(),
            run_at_startup: false,
            start_minimized: false,
            close_to_tray: true,
            notify_complete: true,
            clipboard_monitor: false,
            capture_exts: DEFAULT_CAPTURE.into(),
            browser_enabled: true,
            browser_show_dialog: true,
            browser_min_size_kb: 0,
            av_enabled: false,
            av_command: default_av_command(),
            ffmpeg_path: String::new(),
            torrent: TorrentSettings::default(),
            update_check: true,
            show_progress: true,
            show_complete: true,
            first_run: true,
        }
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Self {
        let mut s: Settings = std::fs::read_to_string(paths.settings())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // Yeni sürümde eklenen kategoriler eksikse tamamla
        for c in category::defaults() {
            if !s.categories.iter().any(|x| x.id == c.id) {
                s.categories.push(c);
            }
        }
        s
    }

    pub fn save(&self, paths: &Paths) -> anyhow::Result<()> {
        let tmp = paths.settings().with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(tmp, paths.settings())?;
        Ok(())
    }

    /// Etkin dil kodu: ayar boşsa sistem dili (Türkçe değilse İngilizce).
    pub fn lang(&self) -> &'static str {
        match self.language.as_str() {
            "tr" => "tr",
            "en" => "en",
            _ => {
                let sys = sys_locale::get_locale().unwrap_or_default().to_ascii_lowercase();
                if sys.starts_with("tr") {
                    "tr"
                } else {
                    "en"
                }
            }
        }
    }

    pub fn capture_ext_list(&self) -> Vec<String> {
        self.capture_exts.split([' ', ',', ';']).filter(|e| !e.is_empty()).map(|e| e.trim_start_matches('.').to_ascii_lowercase()).collect()
    }
}
