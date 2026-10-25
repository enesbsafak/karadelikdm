//! Kuyruk bitiş eylemleri: uygulamadan çık, uyku, hazırda bekletme, bilgisayarı kapat.
//! Bilgisayarı etkileyen eylemlerden önce 60 saniyelik, iptal edilebilir bir geri sayım gösterilir.

use crate::i18n;
use crate::kcore::{self, log};
use kdm_core::queue::AfterAction;

pub fn after_queue(action: AfterAction, queue: &str) {
    match action {
        AfterAction::None => {}
        AfterAction::Exit => {
            kcore::notify(&i18n::tf("msg.queue_done", &[queue]));
            crate::exit_app();
        }
        a => crate::windows::countdown(&i18n::tf("msg.queue_done", &[queue]), &i18n::t(label(a)), 60, &format!("power:{}", id(a))),
    }
}

fn id(a: AfterAction) -> &'static str {
    match a {
        AfterAction::Sleep => "sleep",
        AfterAction::Hibernate => "hibernate",
        AfterAction::Shutdown => "shutdown",
        _ => "",
    }
}

fn label(a: AfterAction) -> &'static str {
    match a {
        AfterAction::Sleep => "power.sleep_in",
        AfterAction::Hibernate => "power.hibernate_in",
        _ => "power.shutdown_in",
    }
}

/// Geri sayım bitince çalışır (`power:<eylem>`).
pub fn power(what: &str) {
    log(&format!("Güç eylemi: {what}"));
    // Kapanmadan önce ilerleme kaydedilsin
    kcore::persist_runtime_state();
    kcore::engine().shutdown(std::time::Duration::from_secs(5));
    if let Err(e) = run(what) {
        log(&format!("Güç eylemi başarısız: {e}"));
        kcore::notify(&format!("{what}: {e}"));
    }
}

#[cfg(windows)]
fn run(what: &str) -> anyhow::Result<()> {
    use windows_sys::Win32::System::Power::SetSuspendState;
    match what {
        "shutdown" => {
            use std::os::windows::process::CommandExt;
            std::process::Command::new("shutdown").args(["/s", "/t", "0"]).creation_flags(0x0800_0000).spawn()?;
        }
        "sleep" | "hibernate" => {
            let ok = unsafe { SetSuspendState(what == "hibernate", false, false) };
            if !ok {
                anyhow::bail!("{}", std::io::Error::last_os_error());
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(not(windows))]
fn run(what: &str) -> anyhow::Result<()> {
    match what {
        "shutdown" => {
            std::process::Command::new("osascript").args(["-e", "tell application \"System Events\" to shut down"]).spawn()?;
        }
        "sleep" | "hibernate" => {
            std::process::Command::new("pmset").arg("sleepnow").spawn()?;
        }
        _ => {}
    }
    Ok(())
}
