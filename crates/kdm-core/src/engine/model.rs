//! İndirme kayıtları ve arayüze giden anlık görüntüler.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Eklendi, başlatılmayı bekliyor (kuyrukta ya da duraklatılmış)
    #[default]
    Stopped,
    /// Kuyruk sırası bekliyor
    Queued,
    Downloading,
    Completed,
    Error,
}

/// Sunucunun Range desteği.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RangeSupport {
    /// Henüz yanıt alınmadı
    #[default]
    Unknown,
    /// `Accept-Ranges: bytes` dendi ama ek bağlantıyla henüz doğrulanmadı
    Probable,
    /// 206 + doğru `Content-Range` alındı
    Yes,
    No,
}

/// Dosyanın kesintisiz bir aralığı. `pos` bir sonraki yazılacak bayt; `end` dahil.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    /// `None`: boyut bilinmiyor, akış sonuna kadar
    pub end: Option<u64>,
    pub pos: u64,
}

impl Segment {
    pub fn is_done(&self) -> bool {
        matches!(self.end, Some(e) if self.pos > e)
    }

    pub fn remaining(&self) -> Option<u64> {
        self.end.map(|e| (e + 1).saturating_sub(self.pos))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub user: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgo {
    Sha256,
    Sha1,
    Md5,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algo: HashAlgo,
    pub hex: String,
}

/// HLS/DASH indirmesinde seçilen varyantlar (`None`: en iyisi).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSpec {
    pub video: Option<String>,
    pub audio: Option<String>,
}

/// BitTorrent indirmesi. Metadata `<veri>/torrents/<özet>.torrent` dosyasında saklanır.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentSpec {
    /// 40 karakter küçük harf onaltılık
    pub info_hash: String,
    /// İndirilecek dosyaların sırası; `None`: tümü
    pub files: Option<Vec<usize>>,
    /// Seçili dosyalardan inmiş bayt (kalıcı; yeniden açılışta liste doğru görünsün)
    pub done: u64,
    /// Toplam gönderilen bayt
    pub uploaded: u64,
    /// Çok dosyalı: içerik `dir/file_name` klasöründedir
    pub multi_file: bool,
}

/// Yeni indirme isteği (arayüz, tarayıcı uzantısı, CLI veya kuyruk içe aktarımından).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewDownload {
    pub url: String,
    /// Boşsa sunucudan / URL'den belirlenir
    pub file_name: String,
    /// Boşsa kategoriye göre seçilir
    pub dir: String,
    pub referer: String,
    pub cookies: String,
    pub user_agent: String,
    pub headers: BTreeMap<String, String>,
    pub auth: Option<Auth>,
    pub checksum: Option<Checksum>,
    /// 0: genel ayar
    pub max_connections: u32,
    /// bayt/sn, 0: sınırsız
    pub speed_limit: u64,
    pub queue: String,
    pub start: bool,
    pub description: String,
    /// HLS/DASH akışı
    pub media: Option<MediaSpec>,
    /// BitTorrent (adres magnet bağlantısıdır)
    pub torrent: Option<TorrentSpec>,
}

/// Kalıcı indirme kaydı (`downloads.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub final_url: String,
    pub file_name: String,
    /// Kullanıcı adı verdiyse sunucunun önerisi yok sayılır
    #[serde(default)]
    pub name_locked: bool,
    pub dir: PathBuf,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub range: RangeSupport,
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub last_modified: String,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub referer: String,
    #[serde(default)]
    pub cookies: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub auth: Option<Auth>,
    #[serde(default)]
    pub checksum: Option<Checksum>,
    #[serde(default)]
    pub max_connections: u32,
    #[serde(default)]
    pub speed_limit: u64,
    #[serde(default)]
    pub queue: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub error: String,
    pub added: i64,
    #[serde(default)]
    pub finished: i64,
    /// Kuyruk sıralaması
    #[serde(default)]
    pub order: u64,
    /// Kuyrukta art arda başarısız deneme sayısı
    #[serde(default)]
    pub retries: u32,
    /// Kuyruk bu zamandan (unix sn) önce yeniden denemez
    #[serde(default)]
    pub retry_at: i64,
    /// HLS/DASH akışıysa seçilen varyantlar
    #[serde(default)]
    pub media: Option<MediaSpec>,
    #[serde(default)]
    pub torrent: Option<TorrentSpec>,
}

impl Record {
    pub fn downloaded(&self) -> u64 {
        self.segments.iter().map(|s| s.pos - s.start).sum()
    }

    pub fn target_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }

    pub fn part_path(&self) -> PathBuf {
        self.dir.join(format!("{}.kdm", self.file_name))
    }
}

/// Arayüze giden anlık durum.
#[derive(Clone, Debug, Serialize)]
pub struct Info {
    pub id: u64,
    pub url: String,
    pub file_name: String,
    pub dir: String,
    pub size: Option<u64>,
    pub done: u64,
    pub status: Status,
    pub range: RangeSupport,
    /// bayt/sn
    pub speed: u64,
    /// saniye
    pub eta: Option<u64>,
    pub connections: u32,
    /// Görselleştirme için [başlangıç, bitiş, indirilen] (en çok 64 parça)
    pub segments: Vec<[u64; 3]>,
    pub error: String,
    pub category: String,
    pub queue: String,
    pub description: String,
    pub added: i64,
    pub finished: i64,
    pub order: u64,
    /// HLS/DASH: [inen segment, toplam segment]; yalnız sürerken (sayaçlar oturumluktur)
    pub parts: Option<[u64; 2]>,
    /// HLS/DASH akışı
    pub media: bool,
    pub torrent: Option<TorrentView>,
}

/// Torrent satırının ek bilgileri.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TorrentView {
    pub info_hash: String,
    /// Bağlı / bilinen eş
    pub peers: u32,
    pub seen: u32,
    /// bayt/sn
    pub upload_speed: u64,
    pub uploaded: u64,
    /// Var olan veri doğrulanıyor
    pub checking: bool,
    /// Metadata eşlerden bekleniyor
    pub resolving: bool,
    /// Bitti ve paylaşılıyor
    pub seeding: bool,
}
