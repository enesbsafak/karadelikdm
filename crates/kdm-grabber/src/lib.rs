//! Site Grabber: bir siteyi kurallarla gezip indirilecek dosyaları toplar. Ağa kendisi çıkmaz; sayfalar ve
//! boyutlar çağıranın `Fetcher`'ından gelir. Böylece vekil, çerez ve TLS ayarları indirme motoruyla aynı kalır
//! ve testler çevrimdışı çalışır.
//!
//! Model (tasarım §6): derinlik aynı site ve diğer siteler için ayrı tutulur ve yalnız HTML sayfalarına
//! uygulanır. Dosyalar derinlikten bağımsız toplanır, desen/boyut/yol filtrelerinden geçer.

mod crawl;
mod links;
mod pattern;
mod robots;
mod urls;

pub use crawl::crawl;
pub use links::{extract_links, Link};

/// Ağ erişimi. Uygulama yönlendirmeleri izlemeli ve büyük gövdeleri sınırlamalıdır.
pub trait Fetcher: Send + Sync {
    /// Sayfayı indirir: (son URL, content-type, gövde). Büyük gövdeleri çağıran sınırlar.
    fn get(&self, url: &str) -> anyhow::Result<(String, String, Vec<u8>)>;
    /// Dosya boyutunu öğrenmek için (HEAD ya da Range 0-0); bilinmiyorsa None
    fn size(&self, url: &str) -> Option<u64>;
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub start_url: String,
    /// Başlangıç sitesinde izlenecek sayfa derinliği (0: yalnız başlangıç sayfası)
    pub same_site_depth: u32,
    /// Başlangıç sitesinden çıkıldıktan sonra arka arkaya kaç sayfa izlenir (0: başka sitelere gitme)
    pub other_site_depth: u32,
    /// "*.zip", "*.pdf", "*"; büyük/küçük harf duyarsız. `/` içeren desen yolun tamamına, diğerleri dosya adına
    /// uygulanır. Boş liste: her şey
    pub file_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    /// URL yolu desenleri. Jokersiz desen önektir. Başlangıç sayfası hariç sayfalara ve dosyalara uygulanır
    pub include_paths: Vec<String>,
    pub exclude_paths: Vec<String>,
    /// 0: sınırsız; yalnız boyut öğrenilebilirse uygulanır
    pub min_size: u64,
    pub max_size: u64,
    /// Getirilecek en çok sayfa (0: sınırsız)
    pub max_pages: usize,
    pub respect_robots: bool,
    /// Ardışık istekler arasındaki en kısa süre
    pub delay_ms: u64,
    /// robots.txt eşleşmesi için ürün belirteci
    pub user_agent_token: String,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            start_url: String::new(),
            same_site_depth: 2,
            other_site_depth: 0,
            file_patterns: vec!["*".into()],
            exclude_patterns: Vec::new(),
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            min_size: 0,
            max_size: 0,
            max_pages: 1000,
            respect_robots: true,
            delay_ms: 0,
            user_agent_token: "KaradelikDM".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub url: String,
    /// Dosyanın bulunduğu sayfa; indirmede Referer olarak gönderilir
    pub referer: String,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Getirilen sayfa sayısı
    pub pages: usize,
    /// Kuyrukta bekleyen sayfa sayısı
    pub queued: usize,
    pub found: usize,
    /// Şu an getirilen ya da son işlenen sayfa
    pub current: String,
}
