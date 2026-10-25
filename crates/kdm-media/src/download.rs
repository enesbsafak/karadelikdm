//! Segmentlerin paralel indirilmesi ve devam ettirme.
//!
//! Disk düzeni: `tmp_dir/<iz>/plan` parça listesi, `tmp_dir/<iz>/<sıra>.seg` tamamlanmış parça. Parça önce
//! `<sıra>.seg.part` adıyla yazılır, diske işlenir ve ancak sonra yeniden adlandırılır: `.seg` dosyasının varlığı
//! parçanın eksiksiz olduğunun kanıtıdır, devam ederken içeriğe bakmadan atlanabilir.
//!
//! `on_progress` `Sync` değildir; işçiler yalnızca kanala yazar, geri çağırma çağıranın iş parçacığında yapılır.

use crate::crypto;
use crate::plan::{fetch, plan_line, Piece, Track};
use crate::{Fetcher, Progress, Stopped};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};

pub(crate) fn seg_path(dir: &Path, i: usize) -> PathBuf {
    dir.join(format!("{i}.seg"))
}

/// Plan değiştiyse (başka varyant seçildi, bildirim güncellendi) eski parçalar başka segmentlere aittir: silinir.
fn prepare_dir(dir: &Path, t: &Track) -> Result<()> {
    let plan: String = t.pieces.iter().map(|p| plan_line(p) + "\n").collect();
    let file = dir.join("plan");
    if fs::read_to_string(&file).ok().as_deref() != Some(plan.as_str()) {
        if dir.exists() {
            fs::remove_dir_all(dir).with_context(|| format!("eski geçici klasör silinemedi: {}", dir.display()))?;
        }
        fs::create_dir_all(dir).with_context(|| format!("geçici klasör oluşturulamadı: {}", dir.display()))?;
        fs::write(&file, plan)?;
    }
    Ok(())
}

/// Anahtar adresi başına bir istek. Kilit istek boyunca tutulur: aynı anahtarı bekleyen işçiler
/// ikinci kez istemesin.
#[derive(Default)]
struct Keys(Mutex<HashMap<String, [u8; 16]>>);

impl Keys {
    fn get(&self, f: &dyn Fetcher, url: &str, stop: &AtomicBool) -> Result<[u8; 16]> {
        let mut m = self.0.lock().map_err(|_| anyhow!("iç hata: anahtar kilidi"))?;
        if let Some(k) = m.get(url) {
            return Ok(*k);
        }
        let d = fetch(f, url, None, stop).context("şifre çözme anahtarı alınamadı")?;
        let k = crypto::key_bytes(&d, url)?;
        m.insert(url.to_string(), k);
        Ok(k)
    }
}

fn piece(f: &dyn Fetcher, p: &Piece, keys: &Keys, path: &Path, stop: &AtomicBool) -> Result<u64> {
    let mut data = fetch(f, &p.url, p.range, stop)?;
    if let Some(k) = &p.key {
        let key = keys.get(f, &k.url, stop)?;
        data = crypto::decrypt(&key, &k.iv, data).with_context(|| p.url.clone())?;
    }
    let part = path.with_extension("seg.part");
    let mut w = fs::File::create(&part).with_context(|| format!("yazılamadı: {}", part.display()))?;
    w.write_all(&data)?;
    w.sync_data()?;
    drop(w);
    fs::rename(&part, path)?;
    Ok(data.len() as u64)
}

pub(crate) fn download(
    f: &dyn Fetcher,
    tracks: &[Track],
    tmp: &Path,
    parallel: usize,
    stop: &AtomicBool,
    on_progress: &dyn Fn(Progress),
) -> Result<()> {
    let mut todo: Vec<(usize, usize)> = Vec::new();
    let mut pr = Progress { done_segments: 0, total_segments: 0, bytes: 0 };
    for (ti, t) in tracks.iter().enumerate() {
        let dir = tmp.join(t.name);
        prepare_dir(&dir, t)?;
        pr.total_segments += t.pieces.len();
        for pi in 0..t.pieces.len() {
            match fs::metadata(seg_path(&dir, pi)) {
                Ok(m) => {
                    pr.done_segments += 1;
                    pr.bytes += m.len();
                }
                Err(_) => todo.push((ti, pi)),
            }
        }
    }
    on_progress(pr);

    let next = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let failure: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    let keys = Keys::default();
    let (tx, rx) = mpsc::channel::<u64>();
    let workers = parallel.clamp(1, 32).min(todo.len());
    std::thread::scope(|s| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (todo, next, abort, failure, keys) = (&todo, &next, &abort, &failure, &keys);
            s.spawn(move || loop {
                // Durdurma yalnızca parçalar arasında denetlenir: inen parça yarıda bırakılmaz, diske yazılır
                if stop.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(ti, pi)) = todo.get(i) else { break };
                let t = &tracks[ti];
                match piece(f, &t.pieces[pi], keys, &seg_path(&tmp.join(t.name), pi), stop) {
                    Ok(n) => {
                        let _ = tx.send(n);
                    }
                    Err(e) => {
                        if !e.is::<Stopped>() {
                            abort.store(true, Ordering::Relaxed);
                            if let Ok(mut slot) = failure.lock() {
                                slot.get_or_insert(e);
                            }
                        }
                        break;
                    }
                }
            });
        }
        drop(tx);
        for n in rx {
            pr.done_segments += 1;
            pr.bytes += n;
            on_progress(pr);
        }
    });

    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    if stop.load(Ordering::Relaxed) {
        return Err(Stopped.into());
    }
    Ok(())
}
