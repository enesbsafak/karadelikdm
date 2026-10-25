//! MPEG-DASH MPD (ISO/IEC 23009-1): Period / AdaptationSet / Representation, BaseURL zinciri ve üç segment
//! adresleme biçimi (SegmentTemplate, SegmentList, SegmentBase).
//!
//! Ayrıştırma ağsızdır; yalnızca SegmentBase'in `sidx` dizini `segments` sırasında indirilir. Böylece ekleme
//! penceresi varyantları listelerken her Representation için istek atılmaz.

use crate::plan::{fetch, resolve, Piece, MAX_SEGMENTS};
use crate::xml::{self, Element};
use crate::{Fetcher, TrackKind};
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug)]
pub(crate) struct Mpd {
    pub periods: Vec<Period>,
}

#[derive(Clone, Debug)]
pub(crate) struct Period {
    /// sn; bilinmiyorsa `duration` tabanlı şablon hesaplanamaz
    pub duration: Option<f64>,
    pub reps: Vec<Rep>,
}

#[derive(Clone, Debug)]
pub(crate) struct Rep {
    pub id: String,
    pub bandwidth: u64,
    pub width: u32,
    pub height: u32,
    pub codecs: String,
    pub mime: String,
    pub lang: String,
    pub kind: TrackKind,
    /// Çözümlenmiş BaseURL zinciri
    pub base: String,
    pub addr: Addr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct S {
    pub t: Option<u64>,
    pub d: u64,
    pub r: i64,
}

#[derive(Clone, Debug)]
pub(crate) enum Addr {
    /// Öznitelikler Period → AdaptationSet → Representation sırasıyla birleştirilmiş (özel olan ezer)
    Template { attrs: HashMap<String, String>, timeline: Option<Vec<S>>, init: Option<String> },
    List { init: Option<(String, Option<(u64, u64)>)>, segs: Vec<(String, Option<(u64, u64)>)> },
    Base { init: Option<(u64, u64)>, index: Option<(u64, u64)> },
    /// Adresleme yok: BaseURL tek dosyadır
    Single,
}

impl Rep {
    pub fn ext(&self) -> &'static str {
        match (self.mime.as_str(), self.kind) {
            ("video/webm" | "audio/webm", _) => "webm",
            ("video/mp2t", _) => "ts",
            (_, TrackKind::Audio) => "m4a",
            _ => "mp4",
        }
    }
}

/// ISO 8601 süre (`PT1H2M3.5S`, `P1DT2H`) → saniye. Yıl ve ay yaklaşık alınır; MPD'lerde pratikte görülmez.
pub(crate) fn duration(s: &str) -> Result<f64> {
    let bad = || anyhow!("geçersiz süre: {s}");
    let body = s.trim().strip_prefix('P').ok_or_else(bad)?;
    let (mut secs, mut in_time, mut num) = (0.0f64, false, String::new());
    for c in body.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' | '.' => num.push(c),
            u => {
                let v: f64 = num.parse().map_err(|_| bad())?;
                num.clear();
                let mul = match (u, in_time) {
                    ('Y', false) => 365.0 * 86400.0,
                    ('M', false) => 30.0 * 86400.0,
                    ('W', false) => 7.0 * 86400.0,
                    ('D', false) => 86400.0,
                    ('H', true) => 3600.0,
                    ('M', true) => 60.0,
                    ('S', true) => 1.0,
                    _ => return Err(bad()),
                };
                secs += v * mul;
            }
        }
    }
    if !num.is_empty() {
        return Err(bad());
    }
    Ok(secs)
}

/// `a-b` (dahil)
fn byte_range(s: &str) -> Result<(u64, u64)> {
    let bad = || anyhow!("geçersiz bayt aralığı: {s}");
    let (a, b) = s.trim().split_once('-').ok_or_else(bad)?;
    let (a, b): (u64, u64) = (a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?);
    if b < a {
        return Err(bad());
    }
    Ok((a, b))
}

fn join_base(base: &str, el: &Element) -> Result<String> {
    match el.child("BaseURL").map(|b| b.text.trim()).filter(|t| !t.is_empty()) {
        Some(t) => resolve(base, t),
        None => Ok(base.to_string()),
    }
}

fn is_video_codec(c: &str) -> bool {
    ["avc", "hvc", "hev", "vp0", "vp8", "vp9", "av01", "dvh", "mp4v"].iter().any(|p| c.starts_with(p))
}

fn is_audio_codec(c: &str) -> bool {
    ["mp4a", "ac-3", "ec-3", "ac-4", "opus", "flac", "vorbis", "dts", "mp3"].iter().any(|p| c.starts_with(p))
}

/// Altyazı ve küçük resim izleri (text, image) `None`: birleştirilmez.
fn classify(content_type: &str, mime: &str, codecs: &str) -> Option<TrackKind> {
    let codecs: Vec<String> = codecs.split(',').map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty()).collect();
    let has_v = codecs.iter().any(|c| is_video_codec(c));
    let has_a = codecs.iter().any(|c| is_audio_codec(c));
    let ct = if content_type.is_empty() { mime.split('/').next().unwrap_or("") } else { content_type };
    match ct {
        "video" if has_a => Some(TrackKind::Muxed),
        "video" => Some(TrackKind::Video),
        "audio" => Some(TrackKind::Audio),
        "text" | "image" => None,
        _ => match (has_v, has_a) {
            (true, true) => Some(TrackKind::Muxed),
            (true, false) => Some(TrackKind::Video),
            (false, true) => Some(TrackKind::Audio),
            _ => None,
        },
    }
}

pub(crate) fn parse(text: &str, url: &str) -> Result<Mpd> {
    let root = xml::parse(text)?;
    if root.name != "MPD" {
        bail!("MPD belgesi değil (kök öğe: {})", root.name);
    }
    if root.attr("type") == Some("dynamic") {
        bail!("canlı DASH yayını (type=dynamic) desteklenmiyor: yayın bittikten sonra tekrar deneyin");
    }
    let total = root.attr("mediaPresentationDuration").map(duration).transpose()?;
    let base = join_base(url, &root)?;
    let pels: Vec<&Element> = root.children_named("Period").collect();
    if pels.is_empty() {
        bail!("MPD'de Period yok");
    }
    let starts = pels.iter().map(|p| p.attr("start").map(duration).transpose()).collect::<Result<Vec<_>>>()?;
    let durs = pels.iter().map(|p| p.attr("duration").map(duration).transpose()).collect::<Result<Vec<_>>>()?;
    let mut periods = Vec::new();
    let mut prev_end = Some(0.0);
    for (i, p) in pels.iter().enumerate() {
        let start = starts[i].or(prev_end).unwrap_or(0.0);
        // Süre: kendi özniteliği, yoksa sonraki Period'un başlangıcı, son Period'da toplam süre
        let dur = durs[i]
            .or_else(|| starts.get(i + 1).copied().flatten().map(|n| n - start))
            .or_else(|| if i + 1 == pels.len() { total.map(|t| t - start) } else { None })
            .filter(|d| *d > 0.0);
        prev_end = dur.map(|d| start + d);
        periods.push(period(p, &base, dur)?);
    }
    Ok(Mpd { periods })
}

fn period(p: &Element, base: &str, duration: Option<f64>) -> Result<Period> {
    let base = join_base(base, p)?;
    let mut reps = Vec::new();
    for (ai, a) in p.children_named("AdaptationSet").enumerate() {
        let a_base = join_base(&base, a)?;
        for (ri, r) in a.children_named("Representation").enumerate() {
            let pick = |k: &str| r.attr(k).or_else(|| a.attr(k)).unwrap_or("").to_string();
            let (mime, codecs) = (pick("mimeType"), pick("codecs"));
            let Some(kind) = classify(&pick("contentType"), &mime, &codecs) else { continue };
            if a.child("ContentProtection").is_some() || r.child("ContentProtection").is_some() {
                bail!("DRM korumalı DASH akışı (ContentProtection), desteklenmiyor");
            }
            let num = |k: &str| pick(k).trim().parse::<u64>().unwrap_or(0);
            let rbase = join_base(&a_base, r)?;
            reps.push(Rep {
                id: r.attr("id").map(str::to_string).unwrap_or_else(|| format!("a{ai}r{ri}")),
                bandwidth: num("bandwidth"),
                width: num("width") as u32,
                height: num("height") as u32,
                lang: pick("lang"),
                kind,
                addr: addressing([p, a, r], &rbase)?,
                base: rbase,
                mime,
                codecs,
            });
        }
    }
    Ok(Period { duration, reps })
}

fn s_entry(e: &Element) -> Result<S> {
    let n = |k: &str| e.attr(k).map(|v| v.trim().parse::<i64>().map_err(|_| anyhow!("SegmentTimeline: S@{k} geçersiz: {v}"))).transpose();
    let d = n("d")?.filter(|d| *d > 0).ok_or_else(|| anyhow!("SegmentTimeline: S@d eksik ya da sıfır"))?;
    let t = n("t")?.map(|t| t.max(0) as u64);
    Ok(S { t, d: d as u64, r: n("r")?.unwrap_or(0) })
}

/// Adresleme biçimini en özel düzey belirler; öznitelikler genelden özele kalıtılır.
fn addressing(levels: [&Element; 3], base: &str) -> Result<Addr> {
    const MODES: [&str; 3] = ["SegmentTemplate", "SegmentList", "SegmentBase"];
    let Some(mode) = levels.iter().rev().find_map(|l| MODES.into_iter().find(|m| l.child(m).is_some())) else {
        return Ok(Addr::Single);
    };
    let found: Vec<&Element> = levels.iter().filter_map(|l| l.child(mode)).collect();
    let specific = |name: &str| found.iter().rev().find_map(|e| e.child(name));
    Ok(match mode {
        "SegmentTemplate" => {
            let mut attrs = HashMap::new();
            for e in &found {
                for (k, v) in &e.attrs {
                    attrs.insert(k.clone(), v.clone());
                }
            }
            let timeline =
                specific("SegmentTimeline").map(|t| t.children_named("S").map(s_entry).collect::<Result<Vec<_>>>()).transpose()?;
            let init = specific("Initialization").and_then(|i| i.attr("sourceURL")).map(str::to_string);
            Addr::Template { attrs, timeline, init }
        }
        "SegmentList" => {
            let init = specific("Initialization")
                .map(|i| -> Result<_> {
                    let url = i.attr("sourceURL").map(|u| resolve(base, u)).transpose()?.unwrap_or_else(|| base.to_string());
                    Ok((url, i.attr("range").map(byte_range).transpose()?))
                })
                .transpose()?;
            let segs = match found.iter().rev().find(|e| e.child("SegmentURL").is_some()) {
                Some(e) => e
                    .children_named("SegmentURL")
                    .map(|s| -> Result<_> {
                        let url = s.attr("media").map(|m| resolve(base, m)).transpose()?.unwrap_or_else(|| base.to_string());
                        Ok((url, s.attr("mediaRange").map(byte_range).transpose()?))
                    })
                    .collect::<Result<Vec<_>>>()?,
                None => Vec::new(),
            };
            Addr::List { init, segs }
        }
        _ => {
            let init = specific("Initialization").and_then(|i| i.attr("range")).map(byte_range).transpose()?;
            let index = found.iter().rev().find_map(|e| e.attr("indexRange")).map(byte_range).transpose()?;
            Addr::Base { init, index }
        }
    })
}

/// `$Number$`, `$Time$`, `$Bandwidth$`, `$RepresentationID$`, `$$`; sayılarda `%0Nd` biçimi.
pub(crate) fn fill(tpl: &str, id: &str, bw: u64, number: u64, time: u64) -> Result<String> {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut rest = tpl;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let j = after.find('$').ok_or_else(|| anyhow!("SegmentTemplate: kapanmamış `$`: {tpl}"))?;
        let tag = &after[..j];
        rest = &after[j + 1..];
        if tag.is_empty() {
            out.push('$');
            continue;
        }
        let (name, fmt) = match tag.find('%') {
            Some(p) => (&tag[..p], Some(&tag[p + 1..])),
            None => (tag, None),
        };
        let v = match name {
            "RepresentationID" if fmt.is_none() => {
                out.push_str(id);
                continue;
            }
            "Number" => number,
            "Time" => time,
            "Bandwidth" => bw,
            _ => bail!("SegmentTemplate: desteklenmeyen tanımlayıcı `${tag}$`"),
        };
        out.push_str(&format_num(v, fmt).ok_or_else(|| anyhow!("SegmentTemplate: geçersiz biçim `${tag}$`"))?);
    }
    out.push_str(rest);
    Ok(out)
}

/// printf alt kümesi: `0Nd`, `Nd`, `d`, `x`, `X`, `o` (ISO/IEC 23009-1 §5.3.9.4.4)
fn format_num(v: u64, fmt: Option<&str>) -> Option<String> {
    let Some(f) = fmt else { return Some(v.to_string()) };
    let conv = f.chars().last()?;
    let w = &f[..f.len() - conv.len_utf8()];
    let w: usize = if w.is_empty() { 0 } else { w.parse().ok()? };
    Some(match conv {
        'd' | 'i' | 'u' => format!("{v:0w$}"),
        'x' => format!("{v:0w$x}"),
        'X' => format!("{v:0w$X}"),
        'o' => format!("{v:0w$o}"),
        _ => return None,
    })
}

/// Bir Representation'ın bir Period'daki parçaları: (başlatma bölümü, medya segmentleri).
pub(crate) fn segments(f: &dyn Fetcher, rep: &Rep, dur: Option<f64>, stop: &AtomicBool) -> Result<(Option<Piece>, Vec<Piece>)> {
    let (init, segs) = match &rep.addr {
        Addr::Single => (None, vec![Piece::plain(rep.base.clone(), None)]),
        Addr::List { init, segs } => {
            (init.clone().map(|(u, r)| Piece::plain(u, r)), segs.iter().map(|(u, r)| Piece::plain(u.clone(), *r)).collect())
        }
        Addr::Base { init, index } => base_segments(f, rep, *init, *index, stop)?,
        Addr::Template { attrs, timeline, init } => template_segments(rep, attrs, timeline.as_deref(), init.as_deref(), dur)?,
    };
    if segs.is_empty() {
        bail!("Representation `{}` için segment bulunamadı", rep.id);
    }
    Ok((init, segs))
}

fn template_segments(
    rep: &Rep,
    attrs: &HashMap<String, String>,
    timeline: Option<&[S]>,
    init_child: Option<&str>,
    dur: Option<f64>,
) -> Result<(Option<Piece>, Vec<Piece>)> {
    let num = |k: &str, d: u64| -> Result<u64> {
        match attrs.get(k) {
            Some(v) => v.trim().parse().map_err(|_| anyhow!("SegmentTemplate@{k} geçersiz: {v}")),
            None => Ok(d),
        }
    };
    let timescale = num("timescale", 1)?.max(1);
    let start = num("startNumber", 1)?;
    let pto = num("presentationTimeOffset", 0)?;
    let media = attrs.get("media").ok_or_else(|| anyhow!("SegmentTemplate@media yok"))?;
    let init = match attrs.get("initialization").map(String::as_str).or(init_child) {
        Some(t) => Some(Piece::plain(resolve(&rep.base, &fill(t, &rep.id, rep.bandwidth, start, 0)?)?, None)),
        None => None,
    };

    // (numara, zaman)
    let mut list: Vec<(u64, u64)> = Vec::new();
    let mut push = |n: u64, t: u64| -> Result<()> {
        if list.len() >= MAX_SEGMENTS {
            bail!("MPD'de çok fazla segment var");
        }
        list.push((n, t));
        Ok(())
    };
    if let Some(tl) = timeline {
        // r=-1: sonraki S@t'ye ya da Period sonuna kadar tekrarla
        let period_end = dur.map(|d| pto + (d * timescale as f64).round() as u64);
        let (mut t, mut n) = (0u64, start);
        for (i, s) in tl.iter().enumerate() {
            if let Some(st) = s.t {
                t = st;
            }
            let count = if s.r >= 0 {
                s.r as u64 + 1
            } else {
                let limit = tl
                    .get(i + 1)
                    .and_then(|x| x.t)
                    .or(period_end)
                    .ok_or_else(|| anyhow!("SegmentTimeline: r=-1 var ama sunum süresi bilinmiyor"))?;
                limit.saturating_sub(t).div_ceil(s.d)
            };
            for _ in 0..count {
                push(n, t)?;
                t += s.d;
                n += 1;
            }
        }
    } else if let Some(d) = attrs.get("duration") {
        let d: u64 = d.trim().parse().ok().filter(|d| *d > 0).ok_or_else(|| anyhow!("SegmentTemplate@duration geçersiz: {d}"))?;
        let pd = dur.ok_or_else(|| anyhow!("segment sayısı hesaplanamadı: Period süresi bilinmiyor"))?;
        // Kayan nokta hatası tam bölünen süreye fazladan segment eklemesin
        let mut count = (pd * timescale as f64 / d as f64 - 1e-6).ceil().max(0.0) as u64;
        if let Some(e) = attrs.get("endNumber").and_then(|e| e.trim().parse::<u64>().ok()) {
            count = count.min((e + 1).saturating_sub(start));
        }
        for i in 0..count {
            push(start + i, pto + i * d)?;
        }
    } else {
        bail!("SegmentTemplate'te ne duration ne SegmentTimeline var");
    }

    let segs = list
        .into_iter()
        .map(|(n, t)| Ok(Piece::plain(resolve(&rep.base, &fill(media, &rep.id, rep.bandwidth, n, t)?)?, None)))
        .collect::<Result<Vec<_>>>()?;
    Ok((init, segs))
}

/// Tek dosya + Range. `sidx` okunabilirse dosya alt bölümlere ayrılır (paralel indirme ve devam için);
/// okunamazsa dosya tek parça olarak indirilir (başlatma bölümü zaten içindedir).
fn base_segments(
    f: &dyn Fetcher,
    rep: &Rep,
    init: Option<(u64, u64)>,
    index: Option<(u64, u64)>,
    stop: &AtomicBool,
) -> Result<(Option<Piece>, Vec<Piece>)> {
    let whole = || (None, vec![Piece::plain(rep.base.clone(), None)]);
    let Some(ix) = index else { return Ok(whole()) };
    let data = fetch(f, &rep.base, Some(ix), stop)?;
    let refs = match sidx(&data, ix.0) {
        Ok(r) if !r.is_empty() => r,
        _ => return Ok(whole()),
    };
    // Başlatma bölümü sidx'i de kapsıyorsa sidx dışarıda kalsın: alt bölümler zaten ayrı parça
    let init = match init {
        Some((s, e)) if s < ix.0 => (s, e.min(ix.0 - 1)),
        Some(r) => r,
        None if ix.0 > 0 => (0, ix.0 - 1),
        None => return Ok(whole()),
    };
    let segs = refs.into_iter().map(|r| Piece::plain(rep.base.clone(), Some(r))).collect();
    Ok((Some(Piece::plain(rep.base.clone(), Some(init))), segs))
}

fn be(d: &[u8], p: usize, n: usize) -> Result<u64> {
    let b = d.get(p..p + n).ok_or_else(|| anyhow!("sidx kutusu kısa"))?;
    Ok(b.iter().fold(0u64, |a, &x| (a << 8) | x as u64))
}

/// ISO/IEC 14496-12 `sidx`: alt bölümlerin dosyadaki mutlak aralıkları. `at`: `data`'nın dosyadaki ofseti.
pub(crate) fn sidx(data: &[u8], at: u64) -> Result<Vec<(u64, u64)>> {
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let (hdr, size): (usize, u64) = match be(data, off, 4)? {
            1 => (16, be(data, off + 8, 8)?),
            0 => (8, (data.len() - off) as u64),
            s => (8, s),
        };
        if size < hdr as u64 {
            bail!("bozuk kutu boyutu");
        }
        if &data[off + 4..off + 8] == b"sidx" {
            let b = &data[off..];
            let version = be(b, hdr, 1)?;
            // sürüm+bayraklar, reference_ID, timescale
            let mut p = hdr + 12;
            let first_offset = if version == 0 {
                p += 4;
                let v = be(b, p, 4)?;
                p += 4;
                v
            } else {
                p += 8;
                let v = be(b, p, 8)?;
                p += 8;
                v
            };
            p += 2;
            let count = be(b, p, 2)?;
            p += 2;
            let mut pos = at + off as u64 + size + first_offset;
            let mut out = Vec::new();
            for _ in 0..count {
                let v = be(b, p, 4)?;
                p += 12;
                if v & 0x8000_0000 != 0 {
                    bail!("iç içe sidx desteklenmiyor");
                }
                let len = v & 0x7fff_ffff;
                if len == 0 {
                    bail!("boş sidx başvurusu");
                }
                out.push((pos, pos + len - 1));
                pos += len;
            }
            return Ok(out);
        }
        off = off.checked_add(size as usize).ok_or_else(|| anyhow!("bozuk kutu boyutu"))?;
    }
    bail!("sidx kutusu bulunamadı")
}

#[cfg(test)]
pub(crate) fn make_sidx(sizes: &[u32], first_offset: u32) -> Vec<u8> {
    let mut b: Vec<u8> = Vec::new();
    let size = 32 + 12 * sizes.len() as u32;
    b.extend_from_slice(&size.to_be_bytes());
    b.extend_from_slice(b"sidx");
    b.extend([0, 0, 0, 0]); // sürüm 0, bayraklar
    b.extend(1u32.to_be_bytes()); // reference_ID
    b.extend(1000u32.to_be_bytes()); // timescale
    b.extend(0u32.to_be_bytes()); // earliest_presentation_time
    b.extend_from_slice(&first_offset.to_be_bytes());
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&(sizes.len() as u16).to_be_bytes());
    for s in sizes {
        b.extend_from_slice(&s.to_be_bytes());
        b.extend_from_slice(&2000u32.to_be_bytes());
        b.extend_from_slice(&0x9000_0000u32.to_be_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fake;

    const URL: &str = "http://cdn.test/dash/film/manifest.mpd";

    fn pieces(text: &str) -> Vec<(Option<Piece>, Vec<Piece>)> {
        let m = parse(text, URL).unwrap();
        let stop = AtomicBool::new(false);
        let f = Fake::default();
        m.periods[0].reps.iter().map(|r| segments(&f, r, m.periods[0].duration, &stop).unwrap()).collect()
    }

    fn urls(p: &[Piece]) -> Vec<&str> {
        p.iter().map(|p| p.url.as_str()).collect()
    }

    #[test]
    fn iso_durations() {
        assert_eq!(duration("PT1H2M3.5S").unwrap(), 3723.5);
        assert_eq!(duration("P1DT1S").unwrap(), 86401.0);
        assert_eq!(duration("PT0S").unwrap(), 0.0);
        assert!(duration("1H").is_err());
        assert!(duration("PT5").is_err());
    }

    #[test]
    fn template_identifiers_and_formats() {
        assert_eq!(fill("$RepresentationID$/s-$Number%05d$.m4s", "v1", 0, 42, 0).unwrap(), "v1/s-00042.m4s");
        assert_eq!(fill("$Bandwidth$_$Time$_$$_$Number$", "x", 800, 3, 9000).unwrap(), "800_9000_$_3");
        assert_eq!(fill("$Number%x$", "x", 0, 255, 0).unwrap(), "ff");
        assert!(fill("$Foo$", "x", 0, 0, 0).is_err());
        assert!(fill("$Number", "x", 0, 0, 0).is_err());
    }

    #[test]
    fn template_number_with_duration() {
        // 9.5 sn / 2 sn → 5 segment; startNumber=3; Representation özniteliği AdaptationSet'i ezer
        let x = r#"<MPD mediaPresentationDuration="PT9.5S"><Period>
            <AdaptationSet contentType="video" mimeType="video/mp4">
              <SegmentTemplate timescale="1000" duration="2000" startNumber="3"
                 media="$RepresentationID$/$Bandwidth$/seg-$Number%03d$.m4s" initialization="$RepresentationID$/init.mp4"/>
              <Representation id="v720" bandwidth="2000000" width="1280" height="720" codecs="avc1.64001f"/>
            </AdaptationSet></Period></MPD>"#;
        let p = pieces(x);
        let (init, segs) = &p[0];
        assert_eq!(init.as_ref().unwrap().url, "http://cdn.test/dash/film/v720/init.mp4");
        assert_eq!(segs.len(), 5);
        assert_eq!(segs[0].url, "http://cdn.test/dash/film/v720/2000000/seg-003.m4s");
        assert_eq!(segs[4].url, "http://cdn.test/dash/film/v720/2000000/seg-007.m4s");
        // Tam bölünen süre fazladan segment üretmez
        let exact = x.replace("PT9.5S", "PT10S");
        assert_eq!(pieces(&exact)[0].1.len(), 5);
    }

    #[test]
    fn template_timeline_with_repeat_and_open_end() {
        let x = r#"<MPD mediaPresentationDuration="PT20S"><Period><AdaptationSet mimeType="audio/mp4" lang="tr">
            <SegmentTemplate timescale="10" media="a/$Time$.m4s" initialization="a/init.mp4" startNumber="0">
              <SegmentTimeline><S t="0" d="40" r="1"/><S d="20"/><S t="100" d="30" r="-1"/></SegmentTimeline>
            </SegmentTemplate>
            <Representation id="aud" bandwidth="128000" codecs="mp4a.40.2"/></AdaptationSet></Period></MPD>"#;
        let p = pieces(x);
        let names: Vec<_> = p[0].1.iter().map(|s| s.url.rsplit('/').next().unwrap().to_string()).collect();
        // 0,40 (r=1), 80, sonra 100'den 200'e (20 sn * 10) 30'luk adımlarla: 100,130,160,190
        assert_eq!(names, ["0.m4s", "40.m4s", "80.m4s", "100.m4s", "130.m4s", "160.m4s", "190.m4s"]);
    }

    #[test]
    fn timeline_number_template() {
        let x = r#"<MPD mediaPresentationDuration="PT6S"><Period><AdaptationSet contentType="video">
            <Representation id="v" bandwidth="1" codecs="avc1"><SegmentTemplate media="n$Number$.m4s" startNumber="10">
              <SegmentTimeline><S t="5" d="2" r="2"/></SegmentTimeline></SegmentTemplate></Representation>
            </AdaptationSet></Period></MPD>"#;
        let p = pieces(x);
        assert!(p[0].0.is_none());
        assert_eq!(urls(&p[0].1).iter().map(|u| u.rsplit('/').next().unwrap()).collect::<Vec<_>>(), ["n10.m4s", "n11.m4s", "n12.m4s"]);
    }

    #[test]
    fn segment_list_with_ranges() {
        let x = r#"<MPD mediaPresentationDuration="PT4S"><Period><AdaptationSet mimeType="video/mp4">
            <Representation id="v" bandwidth="1" codecs="avc1"><BaseURL>media/v.mp4</BaseURL>
              <SegmentList duration="2"><Initialization range="0-99"/>
                <SegmentURL mediaRange="100-199"/><SegmentURL media="other.m4s"/><SegmentURL media="/x.m4s" mediaRange="5-9"/>
              </SegmentList></Representation></AdaptationSet></Period></MPD>"#;
        let p = pieces(x);
        let (init, segs) = &p[0];
        assert_eq!(init.as_ref().unwrap(), &Piece::plain("http://cdn.test/dash/film/media/v.mp4".into(), Some((0, 99))));
        assert_eq!(segs[0], Piece::plain("http://cdn.test/dash/film/media/v.mp4".into(), Some((100, 199))));
        assert_eq!(segs[1], Piece::plain("http://cdn.test/dash/film/media/other.m4s".into(), None));
        assert_eq!(segs[2], Piece::plain("http://cdn.test/x.m4s".into(), Some((5, 9))));
    }

    #[test]
    fn segment_base_with_sidx() {
        let mut file = vec![b'I'; 100];
        let sidx_box = make_sidx(&[30, 20], 0);
        let sidx_end = 100 + sidx_box.len() as u64;
        file.extend(&sidx_box);
        file.extend([b'A'; 30]);
        file.extend([b'B'; 20]);
        let x = format!(
            r#"<MPD><Period><AdaptationSet mimeType="video/mp4"><Representation id="v" codecs="avc1"><BaseURL>v.mp4</BaseURL>
              <SegmentBase indexRange="100-{}"><Initialization range="0-99"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#,
            sidx_end - 1
        );
        let m = parse(&x, URL).unwrap();
        let f = Fake::default().with("http://cdn.test/dash/film/v.mp4", file);
        let (init, segs) = segments(&f, &m.periods[0].reps[0], None, &AtomicBool::new(false)).unwrap();
        assert_eq!(init.unwrap().range, Some((0, 99)));
        let r: Vec<_> = segs.iter().map(|s| s.range).collect();
        assert_eq!(r, [Some((sidx_end, sidx_end + 29)), Some((sidx_end + 30, sidx_end + 49))]);
    }

    #[test]
    fn segment_base_without_index_is_one_file() {
        let x = r#"<MPD><Period><AdaptationSet mimeType="audio/mp4"><Representation id="a"><BaseURL>a.m4a</BaseURL>
            <SegmentBase><Initialization range="0-99"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#;
        let p = pieces(x);
        assert!(p[0].0.is_none());
        assert_eq!(p[0].1, [Piece::plain("http://cdn.test/dash/film/a.m4a".into(), None)]);
    }

    #[test]
    fn base_url_chain() {
        let x = r#"<MPD mediaPresentationDuration="PT2S"><BaseURL>http://media.test/root/</BaseURL>
            <Period><BaseURL>p1/</BaseURL><AdaptationSet mimeType="video/mp4"><BaseURL>as/</BaseURL>
              <SegmentTemplate media="$Number$.m4s" duration="2"/>
              <Representation id="r" codecs="avc1"><BaseURL>r/</BaseURL></Representation>
              <Representation id="abs" codecs="avc1"><BaseURL>https://other.test/z/</BaseURL></Representation>
            </AdaptationSet></Period></MPD>"#;
        let p = pieces(x);
        assert_eq!(urls(&p[0].1), ["http://media.test/root/p1/as/r/1.m4s"]);
        assert_eq!(urls(&p[1].1), ["https://other.test/z/1.m4s"]);
    }

    #[test]
    fn kinds_from_content_type_and_mime() {
        let x = r#"<MPD mediaPresentationDuration="PT2S"><Period>
            <AdaptationSet contentType="video"><Representation id="v" mimeType="video/mp4" codecs="avc1.4d401f" width="640" height="360" bandwidth="500"/></AdaptationSet>
            <AdaptationSet mimeType="audio/mp4" lang="en"><Representation id="a" codecs="mp4a.40.2" bandwidth="64"/></AdaptationSet>
            <AdaptationSet mimeType="video/mp4"><Representation id="m" codecs="avc1.4d401f,mp4a.40.2"/></AdaptationSet>
            <AdaptationSet contentType="text" mimeType="application/mp4"><Representation id="sub" codecs="stpp"/></AdaptationSet>
            <AdaptationSet><Representation id="c" codecs="ec-3"/></AdaptationSet>
            </Period></MPD>"#;
        let m = parse(x, URL).unwrap();
        let k: Vec<_> = m.periods[0].reps.iter().map(|r| (r.id.as_str(), r.kind)).collect();
        assert_eq!(
            k,
            [("v", TrackKind::Video), ("a", TrackKind::Audio), ("m", TrackKind::Muxed), ("c", TrackKind::Audio)]
        );
        let a = &m.periods[0].reps[1];
        assert_eq!((a.lang.as_str(), a.ext()), ("en", "m4a"));
        assert_eq!(m.periods[0].reps[0].ext(), "mp4");
        assert_eq!((m.periods[0].reps[0].width, m.periods[0].reps[0].height), (640, 360));
    }

    #[test]
    fn drm_and_live_errors() {
        let drm = r#"<MPD mediaPresentationDuration="PT2S"><Period><AdaptationSet mimeType="video/mp4">
            <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
            <Representation id="v" codecs="avc1"/></AdaptationSet></Period></MPD>"#;
        let e = parse(drm, URL).unwrap_err().to_string();
        assert!(e.contains("DRM"), "{e}");
        let live = r#"<MPD type="dynamic"><Period/></MPD>"#;
        assert!(parse(live, URL).unwrap_err().to_string().contains("canlı"));
    }

    #[test]
    fn period_durations() {
        let x = r#"<MPD mediaPresentationDuration="PT10S">
            <Period id="1"><AdaptationSet mimeType="video/mp4"><SegmentTemplate media="a$Number$" duration="2"/><Representation id="v" codecs="avc1"/></AdaptationSet></Period>
            <Period id="2" start="PT4S"><AdaptationSet mimeType="video/mp4"><SegmentTemplate media="b$Number$" duration="2"/><Representation id="v" codecs="avc1"/></AdaptationSet></Period>
            </MPD>"#;
        let m = parse(x, URL).unwrap();
        assert_eq!(m.periods[0].duration, Some(4.0));
        assert_eq!(m.periods[1].duration, Some(6.0));
    }

    #[test]
    fn sidx_rejects_garbage() {
        assert!(sidx(b"\0\0\0\x08free", 0).is_err());
        assert!(sidx(&[0, 0, 0, 40, b's', b'i', b'd', b'x', 0], 0).is_err());
    }
}
