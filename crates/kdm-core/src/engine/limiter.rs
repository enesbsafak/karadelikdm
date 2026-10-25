//! Token bucket hız sınırlayıcı. Genel sınır ve indirme başına sınır ayrı kovalardır; bir yazma ikisinden de
//! jeton alır. Bekleme kısa adımlarla yapılır, durdurma isteğinde hemen biter.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Kova kapasitesi: 250 ms'lik trafik (en az 64 KiB). Küçük kova hızı düzgün tutar, çok küçük olursa
/// tek okuma tamponunu karşılayamaz.
const BURST: Duration = Duration::from_millis(250);
const MIN_BURST: u64 = 64 * 1024;

pub struct Limiter {
    /// bayt/sn, 0: sınırsız
    rate: AtomicU64,
    state: Mutex<(f64, Instant)>,
}

impl Limiter {
    pub fn new(rate: u64) -> Self {
        Limiter { rate: AtomicU64::new(rate), state: Mutex::new((0.0, Instant::now())) }
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn set_rate(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    fn capacity(rate: u64) -> f64 {
        ((rate as f64) * BURST.as_secs_f64()).max(MIN_BURST as f64)
    }

    /// `n` bayt için izin bekler. `false`: durdurma istendi.
    pub fn acquire(&self, n: u64, stop: &AtomicBool) -> bool {
        loop {
            let rate = self.rate();
            if rate == 0 {
                return !stop.load(Ordering::Relaxed);
            }
            let wait = {
                let mut st = self.state.lock();
                let now = Instant::now();
                let cap = Self::capacity(rate);
                st.0 = (st.0 + now.duration_since(st.1).as_secs_f64() * rate as f64).min(cap);
                st.1 = now;
                // Kapasiteden büyük istekler borçlanarak geçer; aksi halde hiç karşılanamazlardı
                let need = (n as f64).min(cap);
                if st.0 >= need {
                    st.0 -= n as f64;
                    return !stop.load(Ordering::Relaxed);
                }
                Duration::from_secs_f64((need - st.0) / rate as f64)
            };
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(wait.min(Duration::from_millis(50)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_waits() {
        let l = Limiter::new(0);
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        for _ in 0..1000 {
            assert!(l.acquire(1 << 20, &stop));
        }
        assert!(t.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn limits_throughput() {
        // 1 MB/s ile 1,5 MB: ilk kova boşken başladığı için ~1,5 s
        let l = Limiter::new(1_000_000);
        let stop = AtomicBool::new(false);
        let t = Instant::now();
        for _ in 0..24 {
            l.acquire(64 * 1024, &stop);
        }
        let s = t.elapsed().as_secs_f64();
        assert!((1.2..2.2).contains(&s), "{s}");
    }

    #[test]
    fn stop_interrupts_wait() {
        let l = Limiter::new(1);
        let stop = AtomicBool::new(true);
        let t = Instant::now();
        assert!(!l.acquire(1 << 20, &stop));
        assert!(t.elapsed() < Duration::from_millis(100));
    }
}
