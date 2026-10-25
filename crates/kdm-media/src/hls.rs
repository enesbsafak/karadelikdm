//! HLS oynatma listeleri (RFC 8216): ana liste (varyantlar, ses yorumları) ve medya listesi (segmentler).

use crate::crypto::{parse_iv, seq_iv};
use crate::plan::{resolve, Key, Piece, MAX_SEGMENTS};
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StreamInf {
    pub uri: String,
    pub bandwidth: u64,
    pub width: u32,
    pub height: u32,
    pub codecs: String,
    /// `AUDIO` grup kimliği
    pub audio: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Rendition {
    /// Yoksa ses varyantın içindedir
    pub uri: Option<String>,
    pub group: String,
    pub name: String,
    pub language: String,
    pub default: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Master {
    pub streams: Vec<StreamInf>,
    pub audios: Vec<Rendition>,
}

#[derive(Clone, Debug)]
pub(crate) struct Media {
    pub pieces: Vec<Piece>,
    /// fMP4 (`#EXT-X-MAP`) mi, yoksa MPEG-TS / paketli ses mi
    pub has_map: bool,
}

impl Media {
    /// Birleşik dosyanın uzantısı. fMP4 değilse ilk segmentin uzantısına bakılır (paketli ses: .aac, .mp3).
    pub fn ext(&self, audio: bool) -> &'static str {
        if self.has_map {
            return if audio { "m4a" } else { "mp4" };
        }
        let first = self.pieces.first().map(|p| p.url.as_str()).unwrap_or("");
        let path = first.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
        if path.ends_with(".aac") {
            "aac"
        } else if path.ends_with(".mp3") {
            "mp3"
        } else {
            "ts"
        }
    }
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.trim_start_matches('\u{feff}').lines().map(str::trim).filter(|l| !l.is_empty())
}

pub(crate) fn is_master(text: &str) -> bool {
    lines(text).any(|l| l.starts_with("#EXT-X-STREAM-INF"))
}

/// Öznitelik listesi (RFC 8216 §4.2): `AD=değer,AD="tırnaklı, virgüllü"`.
pub(crate) fn attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_ascii_uppercase();
        rest = &rest[eq + 1..];
        let val;
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            val = r[..end].to_string();
            rest = &r[(end + 1).min(r.len())..];
            rest = rest.find(',').map_or("", |c| &rest[c + 1..]);
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            val = rest[..end].trim().to_string();
            rest = rest.get(end + 1..).unwrap_or("");
        }
        out.push((key, val));
        rest = rest.trim_start();
    }
    out
}

fn get<'a>(a: &'a [(String, String)], k: &str) -> Option<&'a str> {
    a.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

pub(crate) fn parse_master(text: &str, base: &str) -> Result<Master> {
    let mut m = Master::default();
    let mut pending: Option<StreamInf> = None;
    for line in lines(text) {
        if let Some(v) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(v);
            let (width, height) = get(&a, "RESOLUTION")
                .and_then(|r| r.split_once(['x', 'X']))
                .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                .unwrap_or((0, 0));
            let bandwidth = get(&a, "BANDWIDTH").or(get(&a, "AVERAGE-BANDWIDTH")).and_then(|b| b.parse().ok()).unwrap_or(0);
            pending = Some(StreamInf {
                uri: String::new(),
                bandwidth,
                width,
                height,
                codecs: get(&a, "CODECS").unwrap_or("").to_string(),
                audio: get(&a, "AUDIO").map(str::to_string),
            });
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(v);
            if get(&a, "TYPE") != Some("AUDIO") {
                continue;
            }
            m.audios.push(Rendition {
                uri: get(&a, "URI").map(|u| resolve(base, u)).transpose()?,
                group: get(&a, "GROUP-ID").unwrap_or("").to_string(),
                name: get(&a, "NAME").unwrap_or("").to_string(),
                language: get(&a, "LANGUAGE").unwrap_or("").to_string(),
                default: get(&a, "DEFAULT") == Some("YES"),
            });
        } else if line.starts_with('#') {
            continue;
        } else if let Some(mut s) = pending.take() {
            s.uri = resolve(base, line)?;
            m.streams.push(s);
        }
    }
    if m.streams.is_empty() {
        bail!("ana oynatma listesinde varyant yok");
    }
    Ok(m)
}

/// Etkin `#EXT-X-KEY` durumu.
#[derive(Clone, Debug)]
enum KeyState {
    Clear,
    Aes { uri: String, iv: Option<[u8; 16]> },
    /// SAMPLE-AES gibi bu sürümün çözemediği yöntem
    Unsupported(String),
    /// Başka bir anahtar sistemi (FairPlay, Widevine, PlayReady)
    Drm(String),
}

impl KeyState {
    fn key(&self, seq: u64) -> Result<Option<Key>> {
        match self {
            KeyState::Clear => Ok(None),
            KeyState::Aes { uri, iv } => Ok(Some(Key { url: uri.clone(), iv: iv.unwrap_or_else(|| seq_iv(seq)) })),
            KeyState::Unsupported(m) => bail!("{m} şifrelemesi desteklenmiyor (yalnızca AES-128 çözülebilir)"),
            KeyState::Drm(f) => bail!("DRM korumalı akış ({f}), desteklenmiyor"),
        }
    }
}

/// `#EXT-X-MAP`: adres, aralık ve bildirildiği andaki anahtar
type MapDecl = (String, Option<(u64, u64)>, KeyState);

/// `n[@o]`
fn byterange(s: &str) -> Result<(u64, Option<u64>)> {
    let bad = || anyhow!("geçersiz BYTERANGE: {s}");
    let (n, o) = match s.trim().split_once('@') {
        Some((n, o)) => (n, Some(o.trim().parse::<u64>().map_err(|_| bad())?)),
        None => (s.trim(), None),
    };
    let n: u64 = n.trim().parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    Ok((n, o))
}

/// Ofsetsiz alt aralık aynı kaynaktaki bir önceki alt aralığın hemen ardından başlar (RFC 8216 §4.3.2.2).
fn place(ends: &mut HashMap<String, u64>, url: &str, (n, o): (u64, Option<u64>)) -> (u64, u64) {
    let start = o.unwrap_or_else(|| ends.get(url).copied().unwrap_or(0));
    let end = start + n - 1;
    ends.insert(url.to_string(), end + 1);
    (start, end)
}

pub(crate) fn parse_media(text: &str, base: &str) -> Result<Media> {
    let mut seq: u64 = 0;
    let mut endlist = false;
    let mut vod = false;
    let mut key = KeyState::Clear;
    let mut prev_was_key = false;
    // (adres, aralık, bildirildiği andaki anahtar)
    let mut map: Option<MapDecl> = None;
    let mut last_map: Option<(String, Option<(u64, u64)>)> = None;
    let mut range: Option<(u64, Option<u64>)> = None;
    let mut ends: HashMap<String, u64> = HashMap::new();
    let mut pieces = Vec::new();
    let mut n: u64 = 0;
    let mut has_map = false;

    for line in lines(text) {
        let is_key = line.starts_with("#EXT-X-KEY:");
        if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.trim().parse().map_err(|_| anyhow!("geçersiz EXT-X-MEDIA-SEQUENCE: {v}"))?;
        } else if line == "#EXT-X-ENDLIST" {
            endlist = true;
        } else if let Some(v) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            // VOD listesi değişmez (RFC 8216 §4.3.3.5), ENDLIST unutulmuş olsa da tamdır
            vod = v.trim() == "VOD";
        } else if let Some(v) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(byterange(v)?);
        } else if let Some(v) = line.strip_prefix("#EXT-X-KEY:") {
            let a = attrs(v);
            let method = get(&a, "METHOD").unwrap_or("NONE");
            let format = get(&a, "KEYFORMAT").unwrap_or("identity");
            // Aynı segmentler için art arda birden çok anahtar sistemi bildirilebilir; çözebildiğimiz
            // identity anahtarı gruptaysa diğerleri onu ezmemeli
            let keep = prev_was_key && matches!(key, KeyState::Aes { .. });
            if format != "identity" {
                if !keep {
                    key = if method == "NONE" { KeyState::Clear } else { KeyState::Drm(format.to_string()) };
                }
            } else {
                key = match method {
                    "NONE" => KeyState::Clear,
                    "AES-128" => {
                        let uri = get(&a, "URI").ok_or_else(|| anyhow!("EXT-X-KEY: URI yok"))?;
                        KeyState::Aes { uri: resolve(base, uri)?, iv: get(&a, "IV").map(parse_iv).transpose()? }
                    }
                    m => KeyState::Unsupported(m.to_string()),
                };
            }
        } else if let Some(v) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attrs(v);
            let uri = resolve(base, get(&a, "URI").ok_or_else(|| anyhow!("EXT-X-MAP: URI yok"))?)?;
            let r = get(&a, "BYTERANGE").map(byterange).transpose()?;
            let r = r.map(|(n, o)| place(&mut ends, &uri, (n, Some(o.unwrap_or(0)))));
            map = Some((uri, r, key.clone()));
        } else if line.starts_with('#') {
            // EXTINF, DISCONTINUITY, PROGRAM-DATE-TIME...: birleştirme için gerekmez
        } else {
            let url = resolve(base, line)?;
            let s = seq + n;
            if let Some((mu, mr, mk)) = &map {
                let id = (mu.clone(), *mr);
                if last_map.as_ref() != Some(&id) {
                    pieces.push(Piece { url: mu.clone(), range: *mr, key: mk.key(s)? });
                    last_map = Some(id);
                    has_map = true;
                }
            }
            let r = range.take().map(|r| place(&mut ends, &url, r));
            pieces.push(Piece { url, range: r, key: key.key(s)? });
            n += 1;
            if pieces.len() > MAX_SEGMENTS {
                bail!("oynatma listesinde çok fazla segment var");
            }
        }
        prev_was_key = is_key;
    }
    if !endlist && !vod {
        bail!("canlı yayın (EXT-X-ENDLIST yok) desteklenmiyor: yayın bittikten sonra tekrar deneyin");
    }
    if n == 0 {
        bail!("medya oynatma listesinde segment yok");
    }
    Ok(Media { pieces, has_map })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://cdn.test/vod/film/index.m3u8";

    #[test]
    fn attribute_list() {
        let a = attrs(r#"BANDWIDTH=1280000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1280x720, NAME="a,b""#);
        assert_eq!(get(&a, "BANDWIDTH"), Some("1280000"));
        assert_eq!(get(&a, "CODECS"), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(get(&a, "RESOLUTION"), Some("1280x720"));
        assert_eq!(get(&a, "NAME"), Some("a,b"));
    }

    #[test]
    fn master_playlist() {
        let t = "#EXTM3U\r\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Türkçe\",LANGUAGE=\"tr\",DEFAULT=YES,URI=\"audio/tr.m3u8\"\r\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",LANGUAGE=\"en\",URI=\"/abs/en.m3u8\"\r\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"sub\",NAME=\"x\",URI=\"sub.m3u8\"\r\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"aud\"\r\n\
            360/index.m3u8\r\n\
            #EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=90000,URI=\"iframe.m3u8\"\r\n\
            #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080\r\n\
            https://other.test/1080.m3u8\r\n";
        assert!(is_master(t));
        let m = parse_master(t, BASE).unwrap();
        assert_eq!(m.streams.len(), 2);
        let s = &m.streams[0];
        assert_eq!(s.uri, "http://cdn.test/vod/film/360/index.m3u8");
        assert_eq!((s.bandwidth, s.width, s.height), (800000, 640, 360));
        assert_eq!(s.codecs, "avc1.4d401e,mp4a.40.2");
        assert_eq!(s.audio.as_deref(), Some("aud"));
        assert_eq!(m.streams[1].uri, "https://other.test/1080.m3u8");
        assert_eq!(m.streams[1].audio, None);
        assert_eq!(m.audios.len(), 2);
        assert_eq!(m.audios[0].uri.as_deref(), Some("http://cdn.test/vod/film/audio/tr.m3u8"));
        assert!(m.audios[0].default);
        assert_eq!(m.audios[1].uri.as_deref(), Some("http://cdn.test/abs/en.m3u8"));
        assert_eq!((m.audios[1].name.as_str(), m.audios[1].language.as_str()), ("English", "en"));
    }

    #[test]
    fn media_playlist_segments_and_relative_uris() {
        let t = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXT-X-MEDIA-SEQUENCE:7\n\
            #EXTINF:10.0,\nseg0.ts\n#EXTINF:10.0,\n../shared/seg1.ts?tok=1\n#EXTINF:4.5,\nhttp://x.test/seg2.ts\n#EXT-X-ENDLIST\n";
        let m = parse_media(t, BASE).unwrap();
        let urls: Vec<_> = m.pieces.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(
            urls,
            ["http://cdn.test/vod/film/seg0.ts", "http://cdn.test/vod/shared/seg1.ts?tok=1", "http://x.test/seg2.ts"]
        );
        assert!(m.pieces.iter().all(|p| p.range.is_none() && p.key.is_none()));
        assert!(!m.has_map);
        assert_eq!(m.ext(false), "ts");
    }

    #[test]
    fn byterange_with_and_without_offset() {
        let t = "#EXTM3U\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000@500\nall.ts\n#EXTINF:4,\n#EXT-X-BYTERANGE:200\nall.ts\n\
            #EXTINF:4,\n#EXT-X-BYTERANGE:300\nall.ts\n#EXTINF:4,\nother.ts\n#EXT-X-ENDLIST\n";
        let m = parse_media(t, BASE).unwrap();
        let r: Vec<_> = m.pieces.iter().map(|p| p.range).collect();
        assert_eq!(r, [Some((500, 1499)), Some((1500, 1699)), Some((1700, 1999)), None]);
        assert!(parse_media("#EXTM3U\n#EXT-X-BYTERANGE:0@1\na.ts\n#EXT-X-ENDLIST", BASE).is_err());
    }

    #[test]
    fn map_with_byterange_is_inserted_once() {
        let t = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-MAP:URI=\"main.mp4\",BYTERANGE=\"720@0\"\n\
            #EXTINF:4,\n#EXT-X-BYTERANGE:1000@720\nmain.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000\nmain.mp4\n\
            #EXT-X-MAP:URI=\"init2.mp4\"\n#EXTINF:4,\nseg3.m4s\n#EXT-X-ENDLIST\n";
        let m = parse_media(t, BASE).unwrap();
        assert!(m.has_map);
        assert_eq!(m.ext(false), "mp4");
        assert_eq!(m.ext(true), "m4a");
        let got: Vec<_> = m.pieces.iter().map(|p| (p.url.rsplit('/').next().unwrap(), p.range)).collect();
        assert_eq!(
            got,
            [
                ("main.mp4", Some((0, 719))),
                ("main.mp4", Some((720, 1719))),
                ("main.mp4", Some((1720, 2719))),
                ("init2.mp4", None),
                ("seg3.m4s", None),
            ]
        );
    }

    #[test]
    fn keys_explicit_and_derived_iv() {
        let t = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:5\n#EXT-X-KEY:METHOD=AES-128,URI=\"k1.bin\"\n#EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.test/k2\",IV=0x000000000000000000000000000000FF\n#EXTINF:4,\nc.ts\n\
            #EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\nd.ts\n#EXT-X-ENDLIST\n";
        let m = parse_media(t, BASE).unwrap();
        let k: Vec<_> = m.pieces.iter().map(|p| p.key.clone()).collect();
        assert_eq!(k[0], Some(Key { url: "http://cdn.test/vod/film/k1.bin".into(), iv: seq_iv(5) }));
        assert_eq!(k[1], Some(Key { url: "http://cdn.test/vod/film/k1.bin".into(), iv: seq_iv(6) }));
        assert_eq!(k[2], Some(Key { url: "https://keys.test/k2".into(), iv: seq_iv(0xff) }));
        assert_eq!(k[3], None);
    }

    #[test]
    fn identity_key_wins_over_other_key_systems() {
        let t = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"com.apple.streamingkeydelivery\",URI=\"skd://x\"\n\
            #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_media(t, BASE).unwrap().pieces[0].key.is_some());
    }

    #[test]
    fn unsupported_encryption_errors() {
        let sample = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        let e = parse_media(sample, BASE).unwrap_err().to_string();
        assert!(e.contains("SAMPLE-AES") && e.contains("desteklenmiyor"), "{e}");
        let drm = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"com.apple.streamingkeydelivery\",URI=\"skd://x\"\n#EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        let e = parse_media(drm, BASE).unwrap_err().to_string();
        assert!(e.contains("DRM"), "{e}");
    }

    #[test]
    fn live_playlist_is_rejected() {
        let t = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:100\n#EXTINF:6,\na.ts\n#EXTINF:6,\nb.ts\n";
        let e = parse_media(t, BASE).unwrap_err().to_string();
        assert!(e.contains("canlı yayın"), "{e}");
        let event = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:6,\na.ts\n";
        assert!(parse_media(event, BASE).is_err());
        // VOD türü ENDLIST olmadan da tamdır
        assert!(parse_media("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:6,\na.ts\n", BASE).is_ok());
    }

    #[test]
    fn packed_audio_extension() {
        let m = parse_media("#EXTM3U\n#EXTINF:6,\na.aac?x=1\n#EXT-X-ENDLIST", BASE).unwrap();
        assert_eq!(m.ext(true), "aac");
    }
}
