//! kdm-media: HLS (RFC 8216) ve MPEG-DASH (ISO/IEC 23009-1) akışlarını indirir, gerekirse çözer, birleştirir.
//!
//! Ağa kendisi çıkmaz: tüm istekler çağıranın `Fetcher`'ı üzerinden yapılır. Böylece motorun ajanı, çerezleri ve
//! vekili aynen kullanılır, bu crate de `kdm-core`'a bağımlı olmaz.
//!
//! Akış: bildirim → iç plan (`plan::Track`, her biri sıralı parçalar) → paralel indirme (`download`) →
//! birleştirme (`mux`). Plan bildirimden her çağrıda yeniden çıkarılır; devam bilgisi yalnızca diskteki
//! tamamlanmış parçalardır.

mod crypto;
mod dash;
mod download;
mod hls;
mod mux;
mod plan;
mod xml;

#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tests;

pub use mux::find_ffmpeg;

use anyhow::{anyhow, bail, Context, Result};
use plan::{fetch, Piece, Track};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub trait Fetcher: Send + Sync {
    /// range: dahil (start, end)
    fn get(&self, url: &str, range: Option<(u64, u64)>) -> anyhow::Result<Vec<u8>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hls,
    Dash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    /// Görüntü ve ses aynı akışta
    Muxed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    /// `Job::video` / `Job::audio` için kimlik. HLS: `v<n>` / `a<n>`; DASH: Representation@id.
    pub id: String,
    pub bandwidth: u64,
    pub width: u32,
    pub height: u32,
    pub codecs: String,
    pub kind: TrackKind,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub url: String,
    /// Varyant kimliği; `None`: en yüksek bant genişlikli görüntü
    pub video: Option<String>,
    /// `None`: seçilen görüntü ayrı ses istiyorsa varsayılan / en iyi ses, istemiyorsa yok
    pub audio: Option<String>,
    /// Son dosya, ör. film.mp4. ffmpeg yoksa uzantı içeriğe göre değişir (bkz. `run`).
    pub out: PathBuf,
    pub tmp_dir: PathBuf,
    pub parallel: usize,
    /// `None`: ffmpeg kullanılmaz (bulmak için `find_ffmpeg`)
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub done_segments: usize,
    pub total_segments: usize,
    pub bytes: u64,
}

/// `run` durdurulunca dönen hata. `err.is::<Stopped>()` ile ayırt edilir; ilerleme `tmp_dir`'de korunur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("indirme durduruldu")
    }
}

impl std::error::Error for Stopped {}

/// Bildirim türü. Önce gövdeye (en güvenilir), sonra Content-Type'a, en son adres uzantısına bakılır.
pub fn detect(url: &str, content_type: &str, body_head: &[u8]) -> Option<Kind> {
    let head = &body_head[..body_head.len().min(2048)];
    let text = String::from_utf8_lossy(head);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("#EXTM3U") {
        return Some(Kind::Hls);
    }
    if t.starts_with('<') && t.contains("<MPD") {
        return Some(Kind::Dash);
    }
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match ct.as_str() {
        "application/vnd.apple.mpegurl" | "application/x-mpegurl" | "audio/mpegurl" | "audio/x-mpegurl" => {
            return Some(Kind::Hls)
        }
        "application/dash+xml" => return Some(Kind::Dash),
        _ => {}
    }
    let path = url.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    if path.ends_with(".m3u8") || path.ends_with(".m3u") {
        Some(Kind::Hls)
    } else if path.ends_with(".mpd") {
        Some(Kind::Dash)
    } else {
        None
    }
}

fn mbps(bw: u64) -> String {
    if bw >= 1_000_000 {
        format!("{:.1} Mbps", bw as f64 / 1e6)
    } else {
        format!("{} kbps", bw / 1000)
    }
}

fn label(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

fn manifest(f: &dyn Fetcher, url: &str, stop: &AtomicBool) -> Result<(Kind, String)> {
    let body = fetch(f, url, None, stop).context("akış bildirimi alınamadı")?;
    let kind = detect(url, "", &body).ok_or_else(|| anyhow!("HLS ya da DASH bildirimi tanınmadı: {url}"))?;
    let text = String::from_utf8(body).map_err(|_| anyhow!("bildirim UTF-8 değil: {url}"))?;
    Ok((kind, text))
}

/// Ana oynatma listesi / MPD -> seçilebilir varyantlar (ekleme penceresinde gösterilir)
pub fn variants(f: &dyn Fetcher, url: &str) -> anyhow::Result<(Kind, Vec<Variant>)> {
    let stop = AtomicBool::new(false);
    let (kind, text) = manifest(f, url, &stop)?;
    let mut out = match kind {
        Kind::Hls => hls_variants(&text, url)?,
        Kind::Dash => dash_variants(&text, url)?,
    };
    // Görüntü önce (yüksekten düşüğe), ses sonra
    out.sort_by_key(|v| (v.kind == TrackKind::Audio, std::cmp::Reverse(v.bandwidth)));
    Ok((kind, out))
}

fn hls_kind(s: &hls::StreamInf, m: &hls::Master) -> TrackKind {
    let codecs: Vec<&str> = s.codecs.split(',').map(str::trim).filter(|c| !c.is_empty()).collect();
    if !codecs.is_empty() && codecs.iter().all(|c| c.starts_with("mp4a") || c.starts_with("ac-3") || c.starts_with("ec-3")) {
        return TrackKind::Audio;
    }
    if audio_for(s, m, None).ok().flatten().is_some() {
        TrackKind::Video
    } else {
        TrackKind::Muxed
    }
}

fn hls_variants(text: &str, url: &str) -> Result<Vec<Variant>> {
    if !hls::is_master(text) {
        // Doğrudan medya listesi: tek akış. Canlı yayın / DRM hatası ekleme penceresinde hemen görünsün.
        hls::parse_media(text, url)?;
        return Ok(vec![Variant {
            id: "v0".into(),
            bandwidth: 0,
            width: 0,
            height: 0,
            codecs: String::new(),
            kind: TrackKind::Muxed,
            label: "Tek akış".into(),
        }]);
    }
    let m = hls::parse_master(text, url)?;
    let mut out: Vec<Variant> = m
        .streams
        .iter()
        .enumerate()
        .map(|(i, s)| Variant {
            id: format!("v{i}"),
            bandwidth: s.bandwidth,
            width: s.width,
            height: s.height,
            codecs: s.codecs.clone(),
            kind: hls_kind(s, &m),
            label: label(&[
                if s.height > 0 { format!("{}x{}", s.width, s.height) } else { String::new() },
                if s.bandwidth > 0 { mbps(s.bandwidth) } else { String::new() },
                s.codecs.clone(),
            ]),
        })
        .collect();
    for (i, a) in m.audios.iter().enumerate() {
        if a.uri.is_none() {
            continue;
        }
        out.push(Variant {
            id: format!("a{i}"),
            bandwidth: 0,
            width: 0,
            height: 0,
            codecs: String::new(),
            kind: TrackKind::Audio,
            label: if a.language.is_empty() { a.name.clone() } else { format!("{} ({})", a.name, a.language) },
        });
    }
    Ok(out)
}

fn dash_variants(text: &str, url: &str) -> Result<Vec<Variant>> {
    let mpd = dash::parse(text, url)?;
    let reps = &mpd.periods[0].reps;
    if reps.is_empty() {
        bail!("MPD'de görüntü ya da ses izi yok");
    }
    Ok(reps
        .iter()
        .map(|r| Variant {
            id: r.id.clone(),
            bandwidth: r.bandwidth,
            width: r.width,
            height: r.height,
            codecs: r.codecs.clone(),
            kind: r.kind,
            label: label(&[
                if r.height > 0 { format!("{}x{}", r.width, r.height) } else { String::new() },
                if r.kind == TrackKind::Audio { r.lang.clone() } else { String::new() },
                if r.bandwidth > 0 { mbps(r.bandwidth) } else { String::new() },
                r.codecs.clone(),
            ]),
        })
        .collect())
}

/// HLS: varyantın ses grubundaki yorum. `id` verilmişse o, yoksa grubun varsayılanı ya da ilki.
/// Grup URI'siz yorumlardan oluşuyorsa ses varyantın içindedir.
fn audio_for<'a>(s: &hls::StreamInf, m: &'a hls::Master, id: Option<&str>) -> Result<Option<&'a hls::Rendition>> {
    if let Some(id) = id {
        let i: usize = id.strip_prefix('a').and_then(|n| n.parse().ok()).ok_or_else(|| anyhow!("ses varyantı bulunamadı: {id}"))?;
        let r = m.audios.get(i).filter(|r| r.uri.is_some()).ok_or_else(|| anyhow!("ses varyantı bulunamadı: {id}"))?;
        return Ok(Some(r));
    }
    let Some(g) = &s.audio else { return Ok(None) };
    let group: Vec<&hls::Rendition> = m.audios.iter().filter(|r| &r.group == g && r.uri.is_some()).collect();
    Ok(group.iter().find(|r| r.default).or(group.first()).copied())
}

fn hls_plan(f: &dyn Fetcher, job: &Job, text: &str, stop: &AtomicBool) -> Result<Vec<Track>> {
    let media_track = |url: &str, text: &str, audio: bool| -> Result<Track> {
        let m = hls::parse_media(text, url)?;
        Ok(Track { name: if audio { "audio" } else { "video" }, ext: m.ext(audio), pieces: m.pieces })
    };
    if !hls::is_master(text) {
        if let Some(v) = job.video.as_deref().filter(|v| *v != "v0") {
            bail!("varyant bulunamadı: {v}");
        }
        return Ok(vec![media_track(&job.url, text, false)?]);
    }
    let m = hls::parse_master(text, &job.url)?;
    let si = match job.video.as_deref() {
        Some(id) => id
            .strip_prefix('v')
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|i| *i < m.streams.len())
            .ok_or_else(|| anyhow!("varyant bulunamadı: {id}"))?,
        None => (0..m.streams.len()).max_by_key(|&i| m.streams[i].bandwidth).unwrap_or(0),
    };
    let s = &m.streams[si];
    let mut tracks = Vec::new();
    let body = fetch(f, &s.uri, None, stop).context("medya oynatma listesi alınamadı")?;
    let only_audio = hls_kind(s, &m) == TrackKind::Audio;
    tracks.push(media_track(&s.uri, &String::from_utf8_lossy(&body), only_audio)?);
    if let Some(r) = audio_for(s, &m, job.audio.as_deref())? {
        let uri = r.uri.as_deref().unwrap_or_default();
        if uri != s.uri {
            let body = fetch(f, uri, None, stop).context("ses oynatma listesi alınamadı")?;
            tracks.push(media_track(uri, &String::from_utf8_lossy(&body), true)?);
        }
    }
    Ok(tracks)
}

fn dash_plan(f: &dyn Fetcher, job: &Job, text: &str, stop: &AtomicBool) -> Result<Vec<Track>> {
    let mpd = dash::parse(text, &job.url)?;
    let first = &mpd.periods[0].reps;
    let find = |id: &str| first.iter().find(|r| r.id == id).ok_or_else(|| anyhow!("varyant bulunamadı: {id}"));
    let best = |pred: &dyn Fn(TrackKind) -> bool| first.iter().filter(|r| pred(r.kind)).max_by_key(|r| r.bandwidth);
    let video = match job.video.as_deref() {
        Some(id) => Some(find(id)?),
        None => best(&|k| k != TrackKind::Audio),
    };
    let audio = match job.audio.as_deref() {
        Some(id) => Some(find(id)?),
        None if video.is_none_or(|v| v.kind == TrackKind::Video) => best(&|k| k == TrackKind::Audio),
        None => None,
    };
    let mut tracks = Vec::new();
    for (name, sel) in [("video", video), ("audio", audio)] {
        let Some(sel) = sel else { continue };
        let mut pieces: Vec<Piece> = Vec::new();
        let mut last_init: Option<Piece> = None;
        for p in &mpd.periods {
            // Sonraki Period'larda aynı kimlik, yoksa aynı türün en yakın bant genişliği
            let rep = p.reps.iter().find(|r| r.id == sel.id).or_else(|| {
                p.reps.iter().filter(|r| r.kind == sel.kind).min_by_key(|r| r.bandwidth.abs_diff(sel.bandwidth))
            });
            let Some(rep) = rep else { continue };
            let (init, segs) = dash::segments(f, rep, p.duration, stop)?;
            if let Some(i) = init {
                if last_init.as_ref() != Some(&i) {
                    pieces.push(i.clone());
                    last_init = Some(i);
                }
            }
            pieces.extend(segs);
        }
        tracks.push(Track { name, ext: sel.ext(), pieces });
    }
    if tracks.is_empty() {
        bail!("MPD'de görüntü ya da ses izi yok");
    }
    Ok(tracks)
}

fn plan_job(f: &dyn Fetcher, job: &Job, stop: &AtomicBool) -> Result<Vec<Track>> {
    let (kind, text) = manifest(f, &job.url, stop)?;
    match kind {
        Kind::Hls => hls_plan(f, job, &text, stop),
        Kind::Dash => dash_plan(f, job, &text, stop),
    }
}

/// Dosyayı aynı birimdeyse taşır, değilse kopyalar.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).with_context(|| format!("kopyalanamadı: {}", to.display()))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

fn finish(job: &Job, tracks: &[Track]) -> Result<Vec<PathBuf>> {
    if let Some(dir) = job.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("klasör oluşturulamadı: {}", dir.display()))?;
    }
    let dir = |t: &Track| job.tmp_dir.join(t.name);
    if let Some(exe) = &job.ffmpeg {
        let mut inputs = Vec::new();
        for t in tracks {
            let p = job.tmp_dir.join(format!("{}.{}", t.name, t.ext));
            mux::concat(&dir(t), t.pieces.len(), &p)?;
            inputs.push(p);
        }
        mux::run_ffmpeg(exe, &inputs, &job.out)?;
        return Ok(vec![job.out.clone()]);
    }
    // ffmpeg yok: tek iz uzantısı içeriğe göre (`.ts`, fMP4 ise `.mp4`), iki iz ayrı dosya
    let mut out = Vec::new();
    for t in tracks {
        let target = if tracks.len() == 1 {
            job.out.with_extension(t.ext)
        } else {
            let stem = job.out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "cikti".into());
            job.out.with_file_name(format!("{stem}.{}.{}", t.name, t.ext))
        };
        let tmp = job.tmp_dir.join(format!("{}.{}", t.name, t.ext));
        mux::concat(&dir(t), t.pieces.len(), &tmp)?;
        move_file(&tmp, &target)?;
        out.push(target);
    }
    Ok(out)
}

/// `run` ile aynı; ffmpeg yokken ayrı kalan tüm dosyaları döner (önce görüntü, sonra ses).
pub fn run_outputs(
    f: &dyn Fetcher,
    job: &Job,
    stop: &AtomicBool,
    on_progress: &dyn Fn(Progress),
) -> anyhow::Result<Vec<PathBuf>> {
    let tracks = plan_job(f, job, stop)?;
    std::fs::create_dir_all(&job.tmp_dir).with_context(|| format!("geçici klasör oluşturulamadı: {}", job.tmp_dir.display()))?;
    download::download(f, &tracks, &job.tmp_dir, job.parallel, stop, on_progress)?;
    let out = finish(job, &tracks)?;
    // Başarıdan sonra geçici dosyalar gereksiz; silinemezse (açık tutan bir program) indirme yine başarılıdır
    let _ = std::fs::remove_dir_all(&job.tmp_dir);
    Ok(out)
}

/// Segmentleri indirir, gerekirse çözer, birleştirir. stop true olunca kısa sürede çıkar ve
/// tmp_dir'deki ilerlemeyi korur: aynı Job ile tekrar çağrılınca inmiş segmentleri atlar (devam).
///
/// Dönen yol: ffmpeg varsa `job.out`. Yoksa tek iz `job.out` uzantısı içeriğe göre değişmiş dosya
/// (`film.ts`, fMP4 için `film.mp4`); iki iz varsa görüntü dosyası (`film.video.mp4`), ses yanındadır
/// (`film.audio.m4a`). Tüm dosyalar için `run_outputs`. Durdurulunca hata `Stopped`'dur.
pub fn run(
    f: &dyn Fetcher,
    job: &Job,
    stop: &std::sync::atomic::AtomicBool,
    on_progress: &dyn Fn(Progress),
) -> anyhow::Result<std::path::PathBuf> {
    let mut v = run_outputs(f, job, stop, on_progress)?;
    if v.is_empty() {
        bail!("çıktı üretilmedi");
    }
    Ok(v.swap_remove(0))
}
