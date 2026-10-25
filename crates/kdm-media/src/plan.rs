//! Ortak iç model: bir indirme planı izlerden, iz de sırayla birleştirilecek parçalardan oluşur.
//!
//! HLS ve DASH ayrıştırıcıları bildirimi bu modele çevirir; indirici ve birleştirici yalnızca bunu bilir.
//! Başlatma bölümü (`#EXT-X-MAP`, `Initialization`) de sıradan bir parçadır: izin çıktısı parçaların
//! sırayla eklenmesidir, böylece akış ortasında değişen başlatma bölümü de doğru yere girer.

use crate::{Fetcher, Stopped};
use anyhow::{anyhow, bail, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use url::Url;

/// Bir parça hatasında ilk denemeden sonraki deneme sayısı
pub(crate) const RETRIES: u32 = 3;
/// Denemeler arası bekleme (deneme sayısıyla artar). Testlerde beklemeye gerek yok.
const BACKOFF: Duration = Duration::from_millis(if cfg!(test) { 2 } else { 500 });
/// Bozuk ya da kötü niyetli bildirimin bellek tüketmesini önler (2 sn'lik segmentle ~11 gün)
pub(crate) const MAX_SEGMENTS: usize = 500_000;

/// AES-128-CBC anahtarı: anahtar `url`'den alınır.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub url: String,
    pub iv: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    pub url: String,
    /// Dahil
    pub range: Option<(u64, u64)>,
    pub key: Option<Key>,
}

impl Piece {
    pub fn plain(url: String, range: Option<(u64, u64)>) -> Self {
        Piece { url, range, key: None }
    }
}

/// Ayrı bir dosyaya birleşen akış (video, ses ya da ikisi bir arada).
#[derive(Clone, Debug)]
pub(crate) struct Track {
    /// `tmp_dir` altındaki klasör adı ve ayrı dosya son eki
    pub name: &'static str,
    /// Birleşik dosyanın uzantısı
    pub ext: &'static str,
    pub pieces: Vec<Piece>,
}

/// `rel`'i `base`'e göre çözümler (RFC 3986). Mutlak `rel` olduğu gibi döner.
pub(crate) fn resolve(base: &str, rel: &str) -> Result<String> {
    let b = Url::parse(base).map_err(|e| anyhow!("geçersiz adres `{base}`: {e}"))?;
    let u = b.join(rel.trim()).map_err(|e| anyhow!("adres çözümlenemedi `{rel}`: {e}"))?;
    Ok(u.to_string())
}

/// Bekleme sırasında da durdurmaya bakar: kullanıcı beklemeyi beklemek zorunda kalmasın.
fn backoff(attempt: u32, stop: &AtomicBool) -> Result<()> {
    let until = std::time::Instant::now() + BACKOFF * attempt;
    while std::time::Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            return Err(Stopped.into());
        }
        std::thread::sleep(Duration::from_millis(20).min(BACKOFF));
    }
    Ok(())
}

/// İsteği `RETRIES` kez yeniden dener. Range verildiyse yanıtın boyu denetlenir: kısa yanıt (kopan bağlantı)
/// yeniden denenir; Range'i yok sayıp tüm dosyayı dönen sunucuda istenen dilim kesilir.
pub(crate) fn fetch(f: &dyn Fetcher, url: &str, range: Option<(u64, u64)>, stop: &AtomicBool) -> Result<Vec<u8>> {
    let mut last = None;
    for attempt in 0..=RETRIES {
        if attempt > 0 {
            backoff(attempt, stop)?;
        }
        match f.get(url, range).and_then(|d| fit_range(d, range)) {
            Ok(d) => return Ok(d),
            Err(e) => last = Some(e),
        }
    }
    let e = last.unwrap_or_else(|| anyhow!("bilinmeyen hata"));
    Err(e.context(format!("{} denemede indirilemedi: {url}", RETRIES + 1)))
}

fn fit_range(mut d: Vec<u8>, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
    let Some((s, e)) = range else { return Ok(d) };
    if e < s {
        bail!("geçersiz bayt aralığı {s}-{e}");
    }
    let want = e - s + 1;
    let got = d.len() as u64;
    if got == want {
        return Ok(d);
    }
    if got > e {
        // Sunucu Range'i yok saydı (200 + tüm dosya)
        d.truncate(e as usize + 1);
        d.drain(..s as usize);
        return Ok(d);
    }
    bail!("eksik yanıt: {want} bayt beklenirken {got} bayt geldi")
}

/// Plan dosyasında sorgu dizgisi yok sayılır: imzalı adreslerin belirteci oturumdan oturuma değişir ama
/// segment aynıdır; devam ederken bu yüzden inmiş segmentler çöpe gitmemeli.
pub(crate) fn plan_line(p: &Piece) -> String {
    let path = p.url.split(['?', '#']).next().unwrap_or("");
    match p.range {
        Some((s, e)) => format!("{path} {s}-{e}"),
        None => path.to_string(),
    }
}
