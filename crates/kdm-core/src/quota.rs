//! İndirme kotası: "son H saatte en çok X MB". Dakika başına bayt sayan 24 saatlik bir halka tutulur;
//! kaydedilip açılışta okunur, bayatlamış dakikalar sıfırlanır.

use serde::{Deserialize, Serialize};

const MINUTES: usize = 24 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotaCfg {
    pub enabled: bool,
    pub megabytes: u64,
    /// 1-24
    pub hours: u32,
}

impl Default for QuotaCfg {
    fn default() -> Self {
        QuotaCfg { enabled: false, megabytes: 500, hours: 4 }
    }
}

impl QuotaCfg {
    fn limit(&self) -> u64 {
        self.megabytes.saturating_mul(1024 * 1024)
    }

    fn window(&self) -> usize {
        (self.hours.clamp(1, 24) as usize) * 60
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tracker {
    buckets: Vec<u64>,
    /// Son yazılan dakika (unix dakika)
    last: i64,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker { buckets: vec![0; MINUTES], last: 0 }
    }
}

impl Tracker {
    fn idx(minute: i64) -> usize {
        minute.rem_euclid(MINUTES as i64) as usize
    }

    /// Halkayı `now_secs` dakikasına getirir; aradaki dakikalar sıfırlanır.
    fn advance(&mut self, now_secs: i64) {
        if self.buckets.len() != MINUTES {
            self.buckets = vec![0; MINUTES];
        }
        let now = now_secs.div_euclid(60);
        if now <= self.last {
            return;
        }
        let gap = (now - self.last).min(MINUTES as i64);
        for m in (now - gap + 1)..=now {
            self.buckets[Self::idx(m)] = 0;
        }
        self.last = now;
    }

    pub fn add(&mut self, bytes: u64, now_secs: i64) {
        self.advance(now_secs);
        let i = Self::idx(self.last);
        self.buckets[i] = self.buckets[i].saturating_add(bytes);
    }

    /// Son `window` dakikada (içinde bulunulan dahil) alınan bayt.
    fn used_in(&mut self, window: usize, now_secs: i64) -> u64 {
        self.advance(now_secs);
        (0..window as i64).map(|k| self.buckets[Self::idx(self.last - k)]).sum()
    }

    pub fn used(&mut self, cfg: &QuotaCfg, now_secs: i64) -> u64 {
        self.used_in(cfg.window(), now_secs)
    }

    pub fn exceeded(&mut self, cfg: &QuotaCfg, now_secs: i64) -> bool {
        cfg.enabled && self.used(cfg, now_secs) >= cfg.limit()
    }

    /// Kota aşıldıysa yeniden indirme yapılabilecek en erken zaman (unix sn). Pencere kaydıkça en eski
    /// dakikalar düşer; toplam sınırın altına inen ilk dakika aranır.
    pub fn resume_at(&mut self, cfg: &QuotaCfg, now_secs: i64) -> Option<i64> {
        if !self.exceeded(cfg, now_secs) {
            return None;
        }
        let window = cfg.window() as i64;
        let mut used = self.used(cfg, now_secs);
        for shift in 1..=window {
            // `shift` dakika sonra pencereden çıkan dakika
            used -= self.buckets[Self::idx(self.last - window + shift)];
            if used < cfg.limit() {
                return Some((self.last + shift) * 60);
            }
        }
        Some((self.last + window) * 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn window_and_exceed() {
        let cfg = QuotaCfg { enabled: true, megabytes: 100, hours: 1 };
        let mut t = Tracker::default();
        let t0 = 1_700_000_000;
        t.add(60 * MB, t0);
        t.add(30 * MB, t0 + 30 * 60);
        assert!(!t.exceeded(&cfg, t0 + 30 * 60));
        t.add(20 * MB, t0 + 40 * 60);
        assert!(t.exceeded(&cfg, t0 + 40 * 60));
        // İlk 60 MB pencereden çıkınca kota açılır
        assert!(!t.exceeded(&cfg, t0 + 61 * 60));
    }

    #[test]
    fn resume_time() {
        let cfg = QuotaCfg { enabled: true, megabytes: 100, hours: 1 };
        let mut t = Tracker::default();
        let t0 = 1_700_000_000 / 60 * 60;
        t.add(80 * MB, t0);
        t.add(30 * MB, t0 + 10 * 60);
        let at = t.resume_at(&cfg, t0 + 20 * 60).unwrap();
        // 80 MB'lık dakika 60 dk sonra pencereden çıkar
        assert_eq!(at, t0 + 60 * 60);
    }

    #[test]
    fn stale_data_is_dropped() {
        let cfg = QuotaCfg { enabled: true, megabytes: 10, hours: 24 };
        let mut t = Tracker::default();
        t.add(50 * MB, 1_700_000_000);
        assert!(t.exceeded(&cfg, 1_700_000_000));
        // İki gün sonra
        assert!(!t.exceeded(&cfg, 1_700_000_000 + 2 * 86_400));
    }

    #[test]
    fn disabled_never_exceeds() {
        let cfg = QuotaCfg { enabled: false, megabytes: 1, hours: 1 };
        let mut t = Tracker::default();
        t.add(100 * MB, 1_700_000_000);
        assert!(!t.exceeded(&cfg, 1_700_000_000));
    }
}
