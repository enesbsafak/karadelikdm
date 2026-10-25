//! Bağlantı ekleme ve parça bölme kararları. Saf fonksiyonlar; zamanlama ve G/Ç `task.rs`'de.
//!
//! Kural: kalan aralığı en büyük olan etkin parça ikiye bölünür, üst yarısı yeni bağlantıya verilir.
//! Bölme eşiği sabit bayt değil süredir: kurban bağlantı kalanını mevcut hızıyla `min_secs` saniyeden kısa
//! sürede bitirecekse bölmek yeni bağlantının kurulum maliyetine değmez.

/// Bölme kararı için bir parçanın görünümü.
#[derive(Clone, Copy, Debug)]
pub struct SegView {
    /// Yazılmış ya da yazılmak üzere ayrılmış son baytın bir sonrası
    pub pos: u64,
    /// Dahil; `None` ise boyut bilinmiyor (bölünemez)
    pub end: Option<u64>,
    /// Bir bağlantı bu parçada çalışıyor mu
    pub active: bool,
    /// bayt/sn, ölçülmediyse 0
    pub speed: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct SplitRule {
    /// Yeni parçanın en küçük boyutu
    pub min_bytes: u64,
    /// Kurbanın kalan süresi bundan kısaysa bölme
    pub min_secs: u64,
}

impl Default for SplitRule {
    /// Yeni bağlantının maliyeti bir bağlantı kurulumu (TCP + TLS, tipik 100-300 ms). Kurban kalanını 1 sn'den
    /// uzun sürede bitirecekse ikiye bölmek kurulum maliyetinden fazlasını kazandırır.
    fn default() -> Self {
        SplitRule { min_bytes: 256 * 1024, min_secs: 1 }
    }
}

/// Bölünecek parçanın indeksi ve yeni parçanın başlangıcı.
pub fn pick_split(segs: &[SegView], rule: SplitRule) -> Option<(usize, u64)> {
    let mut best: Option<(usize, u64)> = None;
    for (i, s) in segs.iter().enumerate() {
        let (Some(end), true) = (s.end, s.active) else { continue };
        let remaining = (end + 1).saturating_sub(s.pos);
        if remaining < rule.min_bytes.saturating_mul(2) {
            continue;
        }
        if s.speed > 0 && remaining < s.speed.saturating_mul(rule.min_secs) {
            continue;
        }
        if best.is_none_or(|(_, r)| remaining > r) {
            best = Some((i, remaining));
        }
    }
    best.map(|(i, remaining)| (i, segs[i].pos + remaining / 2))
}

/// Etkin bağlantı sınırının bir sonraki değeri. Saniyede bir çağrılır: sınır 1'den başlayıp ikiye katlanarak
/// `max`'a çıkar. Hız sınırı varken toplam hız sınırın %85'ine ulaştıysa artış durur (daha fazla bağlantı
/// yalnızca sırayla bekleyen istekler üretir).
pub fn next_allowed(current: u32, max: u32, limit: u64, speed: u64) -> u32 {
    let max = max.max(1);
    if limit > 0 && speed.saturating_mul(100) >= limit.saturating_mul(85) {
        return current.clamp(1, max);
    }
    current.max(1).saturating_mul(2).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(pos: u64, end: u64, speed: u64) -> SegView {
        SegView { pos, end: Some(end), active: true, speed }
    }

    #[test]
    fn splits_largest_remaining_upper_half() {
        let segs = [seg(0, 9_999_999, 0), seg(10_000_000, 49_999_999, 0)];
        let (i, at) = pick_split(&segs, SplitRule::default()).unwrap();
        assert_eq!(i, 1);
        assert_eq!(at, 10_000_000 + 40_000_000 / 2);
    }

    #[test]
    fn respects_min_bytes() {
        let rule = SplitRule { min_bytes: 1000, min_secs: 3 };
        assert!(pick_split(&[seg(0, 1998, 0)], rule).is_none());
        assert_eq!(pick_split(&[seg(0, 1999, 0)], rule), Some((0, 1000)));
    }

    #[test]
    fn skips_segments_finishing_soon() {
        // 10 MB kaldı, 20 MB/s: yarım saniyede biter, bölünmez
        let rule = SplitRule::default();
        assert!(pick_split(&[seg(0, 9_999_999, 20_000_000)], rule).is_none());
        // aynı parça 1 MB/s ile 10 s sürer, bölünür
        assert!(pick_split(&[seg(0, 9_999_999, 1_000_000)], rule).is_some());
    }

    #[test]
    fn ignores_inactive_and_unknown_size() {
        let mut a = seg(0, 99_999_999, 0);
        a.active = false;
        let b = SegView { pos: 0, end: None, active: true, speed: 0 };
        assert!(pick_split(&[a, b], SplitRule::default()).is_none());
    }

    #[test]
    fn ramp_doubles_and_caps() {
        assert_eq!(next_allowed(1, 8, 0, 0), 2);
        assert_eq!(next_allowed(4, 8, 0, 0), 8);
        assert_eq!(next_allowed(8, 8, 0, 0), 8);
        assert_eq!(next_allowed(0, 0, 0, 0), 1);
    }

    #[test]
    fn ramp_stops_near_speed_limit() {
        assert_eq!(next_allowed(2, 8, 1000, 850), 2);
        assert_eq!(next_allowed(2, 8, 1000, 500), 4);
    }
}
