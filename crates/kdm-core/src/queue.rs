//! Kuyruklar, zamanlayıcı ve kota uygulaması. Kararlar saf fonksiyonlarda (`plan`, `due`, `retry_delay`);
//! `Scheduler` bunları saniyede bir motora uygular.

use crate::engine::{Engine, Record, Status};
use crate::quota::{QuotaCfg, Tracker};
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub const MAIN: &str = "main";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AfterAction {
    #[default]
    None,
    Exit,
    Sleep,
    Hibernate,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// "HH:MM", boş: yok
    pub start: String,
    pub stop: String,
    /// bit 0 = Pazartesi ... bit 6 = Pazar
    pub days: u8,
    /// Bir kez çalıştıktan sonra kapanır
    pub once: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule { enabled: false, start: "02:00".into(), stop: String::new(), days: 0x7f, once: false }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub id: String,
    pub name: String,
    pub max_parallel: u32,
    pub schedule: Schedule,
    pub after: AfterAction,
    /// Başarısız indirme en çok bu kadar yeniden denenir
    pub retry_limit: u32,
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            id: MAIN.into(),
            name: String::new(),
            max_parallel: 3,
            schedule: Schedule::default(),
            after: AfterAction::None,
            retry_limit: 5,
        }
    }
}

/// Kuyruk olayları (uygulama: bildirim, bilgisayarı kapatma geri sayımı vb.).
pub trait Hooks: Send + Sync {
    /// Kuyruk boşaldı; `after` eylemi uygulanmalı (kullanıcı elle durdurduysa çağrılmaz)
    fn queue_finished(&self, queue: &Queue);
    /// Kota durumu değişti; `resume_at` unix sn
    fn quota_changed(&self, paused: bool, resume_at: Option<i64>);
}

/// `HH:MM` -> dakika
fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Bu dakikada zamanlanmış başlatma / durdurma var mı.
pub fn due(s: &Schedule, now: &DateTime<impl TimeZone>) -> (bool, bool) {
    if !s.enabled {
        return (false, false);
    }
    let day_bit = 1u8 << now.weekday().num_days_from_monday();
    if s.days & day_bit == 0 {
        return (false, false);
    }
    let minute = now.hour() * 60 + now.minute();
    (parse_hm(&s.start) == Some(minute), parse_hm(&s.stop) == Some(minute))
}

/// Kuyruktan şimdi başlatılacak indirmeler: sırayla, eşzamanlılık sınırına kadar, yeniden deneme zamanı gelmiş olanlar.
pub fn plan(q: &Queue, records: &[Record], running: &[u64], now: i64) -> Vec<u64> {
    let busy = records.iter().filter(|r| r.queue == q.id && running.contains(&r.id)).count();
    let free = (q.max_parallel.max(1) as usize).saturating_sub(busy);
    let mut pending: Vec<&Record> = records
        .iter()
        .filter(|r| r.queue == q.id && r.status == Status::Queued && !running.contains(&r.id) && r.retry_at <= now)
        .collect();
    pending.sort_by_key(|r| r.order);
    pending.into_iter().take(free).map(|r| r.id).collect()
}

/// Kuyrukta bekleyen ya da çalışan iş kaldı mı.
pub fn has_work(q: &Queue, records: &[Record], running: &[u64]) -> bool {
    records.iter().any(|r| r.queue == q.id && (r.status == Status::Queued || running.contains(&r.id)))
}

/// n. başarısızlıktan sonraki bekleme: 30 s, 60 s, 120 s... (en çok 30 dk)
pub fn retry_delay(n: u32) -> i64 {
    (30i64 << n.saturating_sub(1).min(6)).min(1800)
}

struct Run {
    queue_id: String,
}

pub struct Scheduler {
    engine: Engine,
    queues: RwLock<Vec<Queue>>,
    active: Mutex<Vec<Run>>,
    quota: RwLock<QuotaCfg>,
    tracker: Mutex<Tracker>,
    quota_paused: Mutex<Option<Vec<u64>>>,
    last_total: Mutex<u64>,
    last_minute: Mutex<i64>,
    hooks: Arc<dyn Hooks>,
}

impl Scheduler {
    pub fn new(engine: Engine, queues: Vec<Queue>, quota: QuotaCfg, tracker: Tracker, hooks: Arc<dyn Hooks>) -> Arc<Self> {
        let mut queues = queues;
        if !queues.iter().any(|q| q.id == MAIN) {
            queues.insert(0, Queue::default());
        }
        let s = Arc::new(Scheduler {
            last_total: Mutex::new(engine.total_received()),
            engine,
            queues: RwLock::new(queues),
            active: Mutex::new(Vec::new()),
            quota: RwLock::new(quota),
            tracker: Mutex::new(tracker),
            quota_paused: Mutex::new(None),
            last_minute: Mutex::new(0),
            hooks,
        });
        let weak = Arc::downgrade(&s);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(s) = weak.upgrade() else { break };
            s.tick(Local::now());
        });
        s
    }

    pub fn queues(&self) -> Vec<Queue> {
        self.queues.read().clone()
    }

    pub fn set_queues(&self, mut list: Vec<Queue>) {
        if !list.iter().any(|q| q.id == MAIN) {
            list.insert(0, Queue::default());
        }
        let ids: Vec<String> = list.iter().map(|q| q.id.clone()).collect();
        // Silinen kuyrukların indirmeleri ana kuyruğa geçer
        for r in self.engine.records() {
            if !r.queue.is_empty() && !ids.contains(&r.queue) {
                let _ = self.engine.update(r.id, |r| r.queue = MAIN.into());
            }
        }
        self.active.lock().retain(|run| ids.contains(&run.queue_id));
        *self.queues.write() = list;
    }

    pub fn set_quota(&self, cfg: QuotaCfg) {
        *self.quota.write() = cfg;
    }

    pub fn tracker(&self) -> Tracker {
        self.tracker.lock().clone()
    }

    pub fn is_active(&self, queue_id: &str) -> bool {
        self.active.lock().iter().any(|r| r.queue_id == queue_id)
    }

    pub fn active_ids(&self) -> Vec<String> {
        self.active.lock().iter().map(|r| r.queue_id.clone()).collect()
    }

    /// Kuyruğu başlatır: bitmemiş tüm indirmeleri sıraya alır.
    pub fn start_queue(&self, queue_id: &str) {
        let running = self.engine.running_ids();
        for r in self.engine.records() {
            if r.queue == queue_id && r.status != Status::Completed && !running.contains(&r.id) {
                let _ = self.engine.update(r.id, |r| {
                    r.status = Status::Queued;
                    r.retries = 0;
                    r.retry_at = 0;
                });
            }
        }
        let mut a = self.active.lock();
        if !a.iter().any(|r| r.queue_id == queue_id) {
            a.push(Run { queue_id: queue_id.to_string() });
        }
    }

    /// Kuyruğu durdurur; çalışanlar duraklatılır, bitiş eylemi uygulanmaz.
    pub fn stop_queue(&self, queue_id: &str) {
        self.active.lock().retain(|r| r.queue_id != queue_id);
        for r in self.engine.records() {
            if r.queue == queue_id {
                if self.engine.is_running(r.id) {
                    self.engine.stop(r.id);
                } else if r.status == Status::Queued {
                    self.engine.set_status(r.id, Status::Stopped);
                }
            }
        }
    }

    /// İndirmeyi kuyruğa ekler (kuyruk çalışıyorsa sırası gelince başlar).
    pub fn enqueue(&self, id: u64, queue_id: &str) {
        let order = self.engine.records().iter().map(|r| r.order).max().unwrap_or(0) + 1;
        let _ = self.engine.update(id, |r| {
            r.queue = queue_id.to_string();
            r.status = Status::Queued;
            r.order = order;
            r.retries = 0;
            r.retry_at = 0;
        });
    }

    /// Motor bir indirmeyi bitirdiğinde (uygulamanın `Sink`'i çağırır).
    pub fn on_finished(&self, id: u64, status: Status) {
        let Some(rec) = self.engine.record(id) else { return };
        if !self.is_active(&rec.queue) {
            return;
        }
        let limit = self.queues.read().iter().find(|q| q.id == rec.queue).map(|q| q.retry_limit).unwrap_or(5);
        match status {
            Status::Error if rec.retries < limit => {
                let order = self.engine.records().iter().map(|r| r.order).max().unwrap_or(0) + 1;
                let now = chrono::Utc::now().timestamp();
                let _ = self.engine.update(id, |r| {
                    r.retries += 1;
                    r.retry_at = now + retry_delay(r.retries);
                    r.status = Status::Queued;
                    r.order = order;
                });
            }
            Status::Completed => {
                let _ = self.engine.update(id, |r| r.retries = 0);
            }
            _ => {}
        }
    }

    pub fn tick(&self, now: DateTime<Local>) {
        let ts = now.timestamp();
        self.tick_quota(ts);

        let minute = ts.div_euclid(60);
        let new_minute = {
            let mut last = self.last_minute.lock();
            let changed = *last != minute;
            *last = minute;
            changed
        };
        if new_minute {
            let queues = self.queues();
            for q in &queues {
                let (start, stop) = due(&q.schedule, &now);
                if start {
                    self.start_queue(&q.id);
                    if q.schedule.once {
                        self.queues.write().iter_mut().filter(|x| x.id == q.id).for_each(|x| x.schedule.enabled = false);
                    }
                }
                if stop {
                    self.stop_queue(&q.id);
                }
            }
        }

        if self.quota_paused.lock().is_some() {
            return;
        }
        let records = self.engine.records();
        let running = self.engine.running_ids();
        let active: Vec<String> = self.active_ids();
        for qid in active {
            let Some(q) = self.queues.read().iter().find(|q| q.id == qid).cloned() else { continue };
            for id in plan(&q, &records, &running, ts) {
                self.engine.start(id);
            }
            if !has_work(&q, &records, &running) {
                self.active.lock().retain(|r| r.queue_id != qid);
                self.hooks.queue_finished(&q);
            }
        }
    }

    fn tick_quota(&self, ts: i64) {
        let total = self.engine.total_received();
        let delta = {
            let mut last = self.last_total.lock();
            let d = total.saturating_sub(*last);
            *last = total;
            d
        };
        let cfg = self.quota.read().clone();
        let mut tr = self.tracker.lock();
        tr.add(delta, ts);
        let exceeded = tr.exceeded(&cfg, ts);
        let mut paused = self.quota_paused.lock();
        match (exceeded, paused.is_some()) {
            (true, false) => {
                let ids = self.engine.running_ids();
                for &id in &ids {
                    self.engine.stop(id);
                }
                *paused = Some(ids);
                let at = tr.resume_at(&cfg, ts);
                drop(tr);
                self.hooks.quota_changed(true, at);
            }
            (false, true) => {
                let ids = paused.take().unwrap_or_default();
                drop(paused);
                drop(tr);
                // Durdurulan indirmelerin çekilmesi bitmiş olmalı (kota en az bir dakika sürer)
                for id in ids {
                    self.engine.start(id);
                }
                self.hooks.quota_changed(false, None);
            }
            _ => {}
        }
    }
}

/// Bilinen bir yerel saatle test/ön izleme için.
pub fn local_at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
    Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().expect("geçerli tarih")
}

/// Kuyruk adlarının görünen hali (ana kuyruğun adı boşsa çeviriden gelir).
pub fn names(queues: &[Queue]) -> BTreeMap<String, String> {
    queues.iter().map(|q| (q.id.clone(), q.name.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::RangeSupport;
    use std::path::PathBuf;

    fn rec(id: u64, queue: &str, status: Status, order: u64, retry_at: i64) -> Record {
        Record {
            id,
            url: String::new(),
            final_url: String::new(),
            file_name: format!("{id}"),
            name_locked: false,
            dir: PathBuf::new(),
            size: None,
            segments: vec![],
            status,
            range: RangeSupport::Unknown,
            etag: String::new(),
            last_modified: String::new(),
            content_type: String::new(),
            referer: String::new(),
            cookies: String::new(),
            user_agent: String::new(),
            headers: Default::default(),
            auth: None,
            checksum: None,
            max_connections: 0,
            speed_limit: 0,
            queue: queue.into(),
            category: String::new(),
            description: String::new(),
            error: String::new(),
            added: 0,
            finished: 0,
            order,
            retries: 0,
            retry_at,
            media: None,
            torrent: None,
        }
    }

    #[test]
    fn plan_respects_order_limit_and_retry_time() {
        let q = Queue { max_parallel: 2, ..Default::default() };
        let recs = vec![
            rec(1, MAIN, Status::Queued, 30, 0),
            rec(2, MAIN, Status::Queued, 10, 0),
            rec(3, MAIN, Status::Queued, 20, 500), // yeniden deneme zamanı gelmedi
            rec(4, "other", Status::Queued, 1, 0),
            rec(5, MAIN, Status::Completed, 5, 0),
            rec(6, MAIN, Status::Queued, 40, 0),
        ];
        assert_eq!(plan(&q, &recs, &[], 100), vec![2, 1]);
        // biri zaten çalışıyor: tek boş yer
        assert_eq!(plan(&q, &recs, &[2], 100), vec![1]);
        assert_eq!(plan(&q, &recs, &[2], 600), vec![3]);
        assert!(has_work(&q, &recs, &[]));
        assert!(!has_work(&q, &[rec(5, MAIN, Status::Completed, 1, 0)], &[]));
    }

    #[test]
    fn schedule_matches_day_and_minute() {
        let s = Schedule { enabled: true, start: "02:00".into(), stop: "07:30".into(), days: 0b0000001, once: false };
        // 2026-10-26 Pazartesi
        assert_eq!(due(&s, &local_at(2026, 10, 26, 2, 0)), (true, false));
        assert_eq!(due(&s, &local_at(2026, 10, 26, 7, 30)), (false, true));
        assert_eq!(due(&s, &local_at(2026, 10, 26, 2, 1)), (false, false));
        // Salı seçili değil
        assert_eq!(due(&s, &local_at(2026, 10, 27, 2, 0)), (false, false));
        let off = Schedule { enabled: false, ..s };
        assert_eq!(due(&off, &local_at(2026, 10, 26, 2, 0)), (false, false));
    }

    #[test]
    fn retry_backoff() {
        assert_eq!(retry_delay(1), 30);
        assert_eq!(retry_delay(2), 60);
        assert_eq!(retry_delay(3), 120);
        assert_eq!(retry_delay(20), 1800);
    }

    #[test]
    fn hm_parsing() {
        assert_eq!(parse_hm("00:00"), Some(0));
        assert_eq!(parse_hm(" 23:59 "), Some(1439));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm(""), None);
    }
}
