//! HLS/DASH indirmeleri. Segment indirme ve birleştirme `kdm-media`'da; burada motorun HTTP ajanı (çerez, referer,
//! vekil, hız sınırı) `Fetcher` olarak verilir ve ilerleme indirme kaydına yansıtılır.

use super::http::{self, ByteRange, DigestState, RequestSpec};
use super::model::{MediaSpec, Status};
use super::task::Task;
use super::Inner;
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct HttpFetcher {
    inner: Arc<Inner>,
    task: Arc<Task>,
    agent: ureq::Agent,
    spec: RequestSpec,
}

impl kdm_media::Fetcher for HttpFetcher {
    fn get(&self, url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        let spec = RequestSpec { url: url.to_string(), ..self.spec.clone() };
        let r = http::get(&self.agent, &spec, range.map(|(a, b)| ByteRange { start: a, end: Some(b) }), "", &DigestState::default())?;
        if !matches!(r.meta.status, 200 | 206) {
            bail!("HTTP {}: {url}", r.meta.status);
        }
        let mut reader = r.body.into_reader();
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if !self.task.limiter.acquire(n as u64, &self.task.stop) || !self.inner.global_limit.acquire(n as u64, &self.task.stop) {
                bail!(kdm_media::Stopped);
            }
            out.extend_from_slice(&buf[..n]);
            self.task.received.fetch_add(n as u64, Ordering::Relaxed);
            self.inner.total_received.fetch_add(n as u64, Ordering::Relaxed);
        }
        // Range yok sayılıp tam gövde geldiyse istenen kısmı kes
        if let (Some((a, b)), 200) = (range, r.meta.status) {
            let (a, b) = (a as usize, (b as usize).saturating_add(1).min(out.len()));
            if a > b {
                bail!("sunucu istenen aralığı göndermedi: {url}");
            }
            out = out[a..b].to_vec();
        }
        Ok(out)
    }
}

/// Seçilebilir kalite/ses varyantı (ekleme penceresi).
#[derive(Clone, Debug, Serialize)]
pub struct VariantView {
    pub id: String,
    /// video | audio | muxed
    pub kind: &'static str,
    pub label: String,
    pub bandwidth: u64,
    pub height: u32,
}

/// Akış türü ve varyantları; `None`: adres HLS/DASH değil.
pub fn variants(net: &http::NetConfig, spec: &RequestSpec) -> Result<Option<(&'static str, Vec<VariantView>)>> {
    let agent = http::agent(net)?;
    let r = http::get(&agent, spec, Some(ByteRange { start: 0, end: Some(4095) }), "", &DigestState::default())?;
    let mut head = Vec::new();
    r.body.into_reader().take(4096).read_to_end(&mut head)?;
    let Some(_) = kdm_media::detect(&spec.url, &r.meta.content_type, &head) else { return Ok(None) };
    struct Plain(ureq::Agent, RequestSpec);
    impl kdm_media::Fetcher for Plain {
        fn get(&self, url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
            let spec = RequestSpec { url: url.to_string(), ..self.1.clone() };
            let r = http::get(&self.0, &spec, range.map(|(a, b)| ByteRange { start: a, end: Some(b) }), "", &DigestState::default())?;
            if !matches!(r.meta.status, 200 | 206) {
                bail!("HTTP {}", r.meta.status);
            }
            let mut v = Vec::new();
            r.body.into_reader().take(16 * 1024 * 1024).read_to_end(&mut v)?;
            Ok(v)
        }
    }
    let (kind, list) = kdm_media::variants(&Plain(agent, spec.clone()), &spec.url)?;
    let views = list
        .into_iter()
        .map(|v| VariantView {
            id: v.id,
            kind: match v.kind {
                kdm_media::TrackKind::Video => "video",
                kdm_media::TrackKind::Audio => "audio",
                kdm_media::TrackKind::Muxed => "muxed",
            },
            label: v.label,
            bandwidth: v.bandwidth,
            height: v.height,
        })
        .collect();
    Ok(Some((if kind == kdm_media::Kind::Hls { "hls" } else { "dash" }, views)))
}

/// Medya kaydını indirir. Durdurulursa ilerleme geçici klasörde kalır, devamda inmiş segmentler atlanır.
pub(crate) fn run(inner: &Arc<Inner>, task: &Arc<Task>, media: MediaSpec) -> Result<(Status, String)> {
    let (spec, out, tmp) = {
        let mut st = task.state.lock();
        st.rec.status = Status::Downloading;
        st.rec.error.clear();
        st.fatal = None;
        st.notice = None;
        let r = &st.rec;
        let spec = RequestSpec {
            url: r.url.clone(),
            referer: r.referer.clone(),
            cookies: r.cookies.clone(),
            user_agent: r.user_agent.clone(),
            headers: r.headers.clone(),
            auth: r.auth.clone(),
        };
        (spec, r.target_path(), r.dir.join(format!(".{}.kdmparts", r.file_name)))
    };
    task.stop.store(false, Ordering::Relaxed);
    task.received.store(0, Ordering::Relaxed);
    inner.mark_changed();
    let (parallel, ffmpeg_hint) = {
        let s = inner.settings.read();
        (s.media_parallel.clamp(1, 16) as usize, s.ffmpeg.clone())
    };
    let job = kdm_media::Job {
        url: spec.url.clone(),
        video: media.video.clone(),
        audio: media.audio.clone(),
        out,
        tmp_dir: tmp,
        parallel,
        // "none": ffmpeg bilerek kapatıldı (HLS .ts, DASH ayrı dosyalar olarak kalır)
        ffmpeg: if ffmpeg_hint.trim() == "none" { None } else { kdm_media::find_ffmpeg(&ffmpeg_hint) },
    };
    let fetcher = HttpFetcher { inner: inner.clone(), task: task.clone(), agent: inner.agent()?, spec };

    // Hız ölçümü: segmentler çağıranın iş parçacıklarında indiği için ayrı bir örnekleyici
    let done = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (task, done, inner) = (task.clone(), done.clone(), inner.clone());
        std::thread::spawn(move || {
            let mut last = task.received.load(Ordering::Relaxed);
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1000));
                let now = task.received.load(Ordering::Relaxed);
                task.speed.store(now - last, Ordering::Relaxed);
                last = now;
                inner.mark_changed();
            }
        })
    };
    let result = kdm_media::run(&fetcher, &job, &task.stop, &|p| {
        task.media_done.store(p.done_segments as u64, Ordering::Relaxed);
        task.media_total.store(p.total_segments as u64, Ordering::Relaxed);
        task.media_bytes.store(p.bytes, Ordering::Relaxed);
        task.connections.store(parallel.min(p.total_segments.saturating_sub(p.done_segments)) as u64, Ordering::Relaxed);
    });
    done.store(true, Ordering::Relaxed);
    let _ = sampler.join();
    match result {
        Ok(path) => {
            let mut st = task.state.lock();
            if let Some(name) = path.file_name() {
                st.rec.file_name = name.to_string_lossy().into_owned();
            }
            st.rec.size = std::fs::metadata(&path).ok().map(|m| m.len());
            Ok((Status::Completed, String::new()))
        }
        // Durdurma sırasında süren istek hata verebilir; o da durdurma sayılır
        Err(e) if e.is::<kdm_media::Stopped>() || task.stop.load(Ordering::Relaxed) => Ok((Status::Stopped, String::new())),
        Err(e) => Err(anyhow!("{e:#}")),
    }
}
