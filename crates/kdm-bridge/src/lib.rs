//! Tarayıcı köprüsü. Uzantı ile uygulama arasında iki kanal var:
//!
//! 1. **Keşif:** Uzantı native messaging ile `karadelikdm-nmh`'yi çalıştırır; host `bridge.json`'daki
//!    `{port, token}`'ı verir. Bu kanal yalnız uzantının kendi kimliğine açıktır (manifestteki izin listesi).
//! 2. **Veri:** `ws://127.0.0.1:<port>` üzerinde JSON mesajlar. İlk mesaj token taşıyan `hello`'dur.
//!
//! Neden iki kanal: native messaging her mesajda yeni süreç ya da uzun ömürlü bir host gerektirir ve
//! uygulama kapalıyken host'un onu başlatması gerekir. WebSocket ise uygulamanın kendi sürecinde yaşar,
//! yapılandırma değişikliğini (ör. yakalama kapatıldı) uzantıya anında itebilir.

mod native;
mod server;

pub use native::{chrome_manifest, firefox_manifest, register_native_hosts, write_manifests};
pub use server::{start, Server};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Native messaging host adı (uzantıdaki `connectNative` ile aynı olmalı).
pub const HOST_NAME: &str = "dev.karadelik.dm";
/// Paketlenmemiş kurulumda da sabit kalsın diye manifestteki `key`'den türetilir (`extension/src/chromium.json`).
pub const CHROME_EXTENSION_ID: &str = "kkifckmmeinedmmdokgacefiojpfbpfl";
pub const FIREFOX_EXTENSION_ID: &str = "karadelik-dm@enesbsafak";
/// Protokol sürümü; uyumsuz değişiklikte artar. `hello` yanıtında uzantıya gider.
pub const PROTOCOL_VERSION: u32 = 1;
/// Uygulamanın veri klasöründeki keşif dosyası.
pub const BRIDGE_FILE: &str = "bridge.json";

/// Uzantıdan gelen mesajlar. Uzantı isteğe bağlı bir `id` alanı da gönderebilir; yanıt bu `id`'yi taşır.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        token: String,
        #[serde(default)]
        version: String,
    },
    Download {
        url: String,
        #[serde(default)]
        referer: String,
        #[serde(default)]
        cookies: String,
        #[serde(default)]
        user_agent: String,
        #[serde(default)]
        file_name: String,
        #[serde(default)]
        size: Option<u64>,
        #[serde(default)]
        mime: String,
        #[serde(default)]
        page_url: String,
    },
    Links {
        urls: Vec<String>,
        #[serde(default)]
        referer: String,
        #[serde(default)]
        cookies: String,
        #[serde(default)]
        user_agent: String,
    },
    Media {
        url: String,
        /// hls | dash | file
        kind: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        referer: String,
        #[serde(default)]
        cookies: String,
        #[serde(default)]
        user_agent: String,
    },
    Ping,
}

/// Uzantının yakalama kararı için gereken ayarlar (Tercihler > Tarayıcı).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub enabled: bool,
    /// Noktasız, küçük harf
    pub exts: Vec<String>,
    pub min_size_kb: u64,
}

pub trait Handler: Send + Sync {
    /// `hello` yanıtında ve `broadcast_config`'te uzantıya gider.
    fn config(&self) -> CaptureConfig;
    /// Download / Links / Media. Hata metni uzantıya iletilir; tarayıcı indirmeyi kendisi sürdürür.
    fn handle(&self, req: Request) -> Result<(), String>;
}

/// `bridge.json` içeriği.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub fn bridge_path(data_dir: &Path) -> PathBuf {
    data_dir.join(BRIDGE_FILE)
}

/// Geçici dosyaya yazıp yeniden adlandırır; host yarım yazılmış bir dosya okumasın.
pub fn write_bridge_file(data_dir: &Path, info: &BridgeInfo) -> Result<PathBuf> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("{} oluşturulamadı", data_dir.display()))?;
    let path = bridge_path(data_dir);
    let tmp = data_dir.join(format!("{BRIDGE_FILE}.{}.tmp", std::process::id()));
    let body = serde_json::to_vec_pretty(info)?;
    write_private(&tmp, &body)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(path)
}

pub fn read_bridge_file(data_dir: &Path) -> Result<BridgeInfo> {
    let path = bridge_path(data_dir);
    let body = std::fs::read(&path).with_context(|| format!("{} okunamadı", path.display()))?;
    Ok(serde_json::from_slice(&body)?)
}

/// Token içerdiği için Unix'te yalnız sahibi okuyabilir. Windows'ta `%APPDATA%` zaten kullanıcıya özeldir.
fn write_private(path: &Path, body: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| format!("{} yazılamadı", path.display()))?;
    f.write_all(body)?;
    f.sync_all()?;
    Ok(())
}

/// Uygulamanın `start`'a vereceği varsayılan origin listesi. Firefox her kurulumda rastgele bir
/// `moz-extension://<uuid>` atadığından orada önek eşleşmesi kullanılır; asıl koruma yine token'dır ve
/// token'ı yalnız `allowed_extensions` listesindeki uzantı native host'tan alabilir.
pub fn default_allowed_origins() -> Vec<String> {
    vec![format!("chrome-extension://{CHROME_EXTENSION_ID}"), "moz-extension://*".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kdm-bridge-{tag}-{}-{}", std::process::id(), rand::random::<u32>()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bridge_file_roundtrip() {
        let dir = temp_dir("bf");
        let info = BridgeInfo { port: 4242, token: "ab".repeat(32), pid: 7 };
        let path = write_bridge_file(&dir.join("alt"), &info).unwrap();
        assert!(path.ends_with(BRIDGE_FILE));
        assert_eq!(read_bridge_file(&dir.join("alt")).unwrap(), info);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["port"], 4242);
        assert_eq!(v["pid"], 7);
        // Geçici dosya kalmamalı
        assert_eq!(std::fs::read_dir(dir.join("alt")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn request_json_shape() {
        let r: Request = serde_json::from_str(r#"{"type":"download","url":"https://a/b.zip","size":null}"#).unwrap();
        assert!(matches!(r, Request::Download { ref url, size: None, .. } if url == "https://a/b.zip"));
        let r: Request = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert_eq!(r, Request::Ping);
        let r: Request = serde_json::from_str(r#"{"type":"media","url":"u","kind":"hls"}"#).unwrap();
        assert!(matches!(r, Request::Media { ref kind, .. } if kind == "hls"));
        assert!(serde_json::from_str::<Request>(r#"{"type":"download"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"type":"rm_rf"}"#).is_err());
    }

    /// Rust sabiti ile uzantı manifestindeki anahtar ve Firefox kimliği aynı kalmalı.
    #[test]
    fn extension_ids_match_manifests() {
        use base64::Engine as _;
        use sha2::Digest as _;
        let chromium: serde_json::Value = serde_json::from_str(include_str!("../../../extension/src/chromium.json")).unwrap();
        let der = base64::engine::general_purpose::STANDARD.decode(chromium["key"].as_str().unwrap()).unwrap();
        let hash = sha2::Sha256::digest(&der);
        let id: String = hash[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect();
        assert_eq!(id, CHROME_EXTENSION_ID);
        let firefox: serde_json::Value = serde_json::from_str(include_str!("../../../extension/src/firefox.json")).unwrap();
        assert_eq!(firefox["browser_specific_settings"]["gecko"]["id"], FIREFOX_EXTENSION_ID);
    }
}
