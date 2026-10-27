//! Uygulama genelinde paylaşılan durum: yollar, ayarlar, indirme motoru, kuyruklar, olay yayını.

use crate::i18n;
use crate::paths::Paths;
use crate::settings::Settings;
use kdm_core::engine::{Engine, EngineSettings, Info, Sink, Status};
use kdm_core::queue::{AfterAction, Hooks, Queue, Scheduler};
use kdm_core::quota::Tracker;
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

pub struct Core {
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    pub app: OnceCell<AppHandle>,
    pub engine: Engine,
    pub sched: OnceCell<Arc<Scheduler>>,
    /// Kota aşıldıysa devam zamanı (unix sn)
    pub quota_until: RwLock<Option<i64>>,
    pub message: RwLock<String>,
}

static CORE: OnceCell<Core> = OnceCell::new();

struct AppSink;

impl Sink for AppSink {
    fn progress(&self, infos: Vec<Info>) {
        crate::tray::update_tooltip(&infos);
        emit("downloads", infos);
    }

    fn finished(&self, id: u64, status: Status, message: &str) {
        let Some(c) = CORE.get() else { return };
        if let Some(s) = c.sched.get() {
            s.on_finished(id, status);
        }
        let Some(rec) = c.engine.record(id) else { return };
        emit("finished", serde_json::json!({ "id": id, "status": status, "message": message }));
        crate::progress::finished(id, status);
        let s = settings();
        match status {
            Status::Completed => {
                log(&format!("Tamamlandı: {}", rec.target_path().display()));
                if s.notify_complete {
                    notify(&i18n::tf("msg.completed", &[&rec.file_name]));
                }
                if s.av_enabled && !s.av_command.trim().is_empty() {
                    let (cmd, path) = (s.av_command.clone(), rec.target_path());
                    std::thread::spawn(move || av_scan(&cmd, &path));
                }
            }
            Status::Error => {
                log(&format!("Hata: {} ({})", rec.url, message));
                // Kuyruk yeniden deneyecekse bildirim gürültü olur
                if c.engine.record(id).is_some_and(|r| r.status == Status::Error) && s.notify_complete {
                    notify(&i18n::tf("msg.failed", &[&rec.file_name, message]));
                }
            }
            _ => {}
        }
    }
}

struct AppHooks;

impl Hooks for AppHooks {
    fn queue_finished(&self, q: &Queue) {
        let name = queue_name(q);
        log(&format!("Kuyruk bitti: {name}"));
        match q.after {
            AfterAction::None => notify(&i18n::tf("msg.queue_done", &[&name])),
            action => crate::system::after_queue(action, &name),
        }
    }

    fn quota_changed(&self, paused: bool, resume_at: Option<i64>) {
        *core().quota_until.write() = if paused { resume_at.or(Some(0)) } else { None };
        if paused {
            let at = resume_at
                .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                .unwrap_or_default();
            notify(&i18n::tf("msg.quota_paused", &[&at]));
        } else {
            notify(&i18n::t("msg.quota_resumed"));
        }
        emit("state", ());
    }
}

pub fn queue_name(q: &Queue) -> String {
    if q.name.trim().is_empty() {
        i18n::t("queue.main")
    } else {
        q.name.clone()
    }
}

pub fn init(paths: Paths) -> &'static Core {
    let settings = Settings::load(&paths);
    let engine = Engine::open(paths.downloads(), Arc::new(AppSink));
    let core = CORE.get_or_init(|| Core {
        paths,
        settings: RwLock::new(settings),
        app: OnceCell::new(),
        engine,
        sched: OnceCell::new(),
        quota_until: RwLock::new(None),
        message: RwLock::new(String::new()),
    });
    let s = core.settings.read().clone();
    let tracker: Tracker = std::fs::read_to_string(core.paths.quota())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let sched = Scheduler::new(core.engine.clone(), s.queues.clone(), s.quota.clone(), tracker, Arc::new(AppHooks));
    let _ = core.sched.set(sched);
    apply_settings(&s);
    core
}

/// Ayarları motora ve kuyruklara uygular.
pub fn apply_settings(s: &Settings) {
    let c = core();
    c.engine.set_settings(EngineSettings {
        max_connections: s.max_connections.clamp(1, 32),
        media_parallel: 4,
        ffmpeg: s.ffmpeg_path.clone(),
        torrent: s.torrent.clone(),
    });
    c.engine.set_net(s.net.clone());
    c.engine.set_global_limit(if s.speed_limit_enabled { s.speed_limit } else { 0 });
    if let Some(sched) = c.sched.get() {
        sched.set_queues(s.queues.clone());
        sched.set_quota(s.quota.clone());
    }
}

pub fn core() -> &'static Core {
    CORE.get().expect("çekirdek başlatılmadı")
}

pub fn sched() -> &'static Arc<Scheduler> {
    core().sched.get().expect("zamanlayıcı başlatılmadı")
}

pub fn engine() -> &'static Engine {
    &core().engine
}

pub fn settings() -> Settings {
    core().settings.read().clone()
}

pub fn update_settings(f: impl FnOnce(&mut Settings)) {
    let c = core();
    let snapshot = {
        let mut s = c.settings.write();
        f(&mut s);
        if let Err(e) = s.save(&c.paths) {
            log(&format!("Ayarlar kaydedilemedi: {e}"));
        }
        s.clone()
    };
    apply_settings(&snapshot);
}

/// Kuyruk listesi zamanlayıcıda değişebilir ("bir kez" seçeneği); kapanışta ve kaydederken ayarlara yazılır.
pub fn persist_runtime_state() {
    let c = core();
    if let Some(s) = c.sched.get() {
        let queues = s.queues();
        let mut st = c.settings.write();
        st.queues = queues;
        let _ = st.save(&c.paths);
        if let Ok(t) = serde_json::to_string(&s.tracker()) {
            let _ = std::fs::write(c.paths.quota(), t);
        }
    }
}

pub fn log(msg: &str) {
    let line = format!("{}: {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    if let Some(c) = CORE.get() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(c.paths.log()) {
            let _ = f.write_all(line.as_bytes());
        }
    }
    #[cfg(debug_assertions)]
    eprint!("{line}");
}

pub fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = app() {
        let _ = app.emit(event, payload);
    }
}

pub fn status(msg: &str) {
    *core().message.write() = msg.to_string();
    emit("status", msg.to_string());
}

pub fn notify(msg: &str) {
    status(msg);
    if let Some(app) = app() {
        use tauri_plugin_notification::NotificationExt;
        let _ = app.notification().builder().title("Karadelik Download Manager").body(msg).show();
    }
}

pub fn app() -> Option<&'static AppHandle> {
    CORE.get().and_then(|c| c.app.get())
}

/// Komut satırını kabuk kullanmadan bölümlere ayırır (çift tırnak destekli).
pub fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for c in cmd.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Antivirüs taraması. Dosya yolu tek bir argüman olarak verilir; kabuk kullanılmadığı için dosya adındaki
/// özel karakterler komut olarak yorumlanamaz.
fn av_scan(command: &str, file: &Path) {
    let parts: Vec<String> = split_command(command).into_iter().map(|p| p.replace("{file}", &file.to_string_lossy())).collect();
    let Some((exe, args)) = parts.split_first() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match cmd.output() {
        Ok(o) if o.status.success() => log(&format!("Antivirüs: temiz: {}", file.display())),
        Ok(o) => {
            log(&format!("Antivirüs uyarısı ({}): {}", o.status, String::from_utf8_lossy(&o.stdout).trim()));
            notify(&i18n::tf("msg.av_warning", &[&file.file_name().unwrap_or_default().to_string_lossy()]));
        }
        Err(e) => log(&format!("Antivirüs çalıştırılamadı: {e}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_splitting() {
        assert_eq!(
            super::split_command(r#""C:\Program Files\x.exe" -Scan -File "{file}""#),
            vec![r"C:\Program Files\x.exe", "-Scan", "-File", "{file}"]
        );
        assert_eq!(super::split_command("  a  b "), vec!["a", "b"]);
        assert_eq!(super::split_command(r#"a "" b"#), vec!["a", "", "b"]);
    }
}
