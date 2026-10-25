//! Uçtan uca testler: sahte `Fetcher`, ffmpeg yok, gerçek dosya sistemi (geçici klasör).

use crate::crypto::{encrypt, seq_iv};
use crate::dash::make_sidx;
use crate::testutil::{Fake, TempDir};
use crate::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

fn job(url: &str, dir: &TempDir) -> Job {
    Job {
        url: url.into(),
        video: None,
        audio: None,
        out: dir.0.join("out").join("film.mp4"),
        tmp_dir: dir.0.join("tmp"),
        parallel: 4,
        ffmpeg: None,
    }
}

fn seg(i: usize, len: usize) -> Vec<u8> {
    (0..len).map(|j| (i * 31 + j) as u8).collect()
}

fn media_list(n: usize, prefix: &str) -> String {
    let mut s = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:4\n");
    for i in 0..n {
        s += &format!("#EXTINF:4.0,\n{prefix}{i}.ts\n");
    }
    s + "#EXT-X-ENDLIST\n"
}

fn no_progress(_: Progress) {}

#[test]
fn detect_by_body_type_and_extension() {
    assert_eq!(detect("http://a/x", "", b"\xef\xbb\xbf#EXTM3U\n"), Some(Kind::Hls));
    assert_eq!(detect("http://a/x", "", b"<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">"), Some(Kind::Dash));
    assert_eq!(detect("http://a/x", "application/vnd.apple.mpegurl; charset=utf-8", b""), Some(Kind::Hls));
    assert_eq!(detect("http://a/x", "application/dash+xml", b""), Some(Kind::Dash));
    assert_eq!(detect("http://a/x.M3U8?t=1", "", b""), Some(Kind::Hls));
    assert_eq!(detect("http://a/x.mpd#f", "text/plain", b""), Some(Kind::Dash));
    assert_eq!(detect("http://a/x.mp4", "video/mp4", b"\0\0\0\x18ftyp"), None);
}

#[test]
fn hls_variants_listing() {
    let master = "#EXTM3U\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Türkçe\",LANGUAGE=\"tr\",DEFAULT=YES,URI=\"tr.m3u8\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e\",AUDIO=\"a\"\nlow.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\nhigh.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\naudio-only.m3u8\n";
    let f = Fake::default().with("http://t/m.m3u8", master);
    let (kind, v) = variants(&f, "http://t/m.m3u8").unwrap();
    assert_eq!(kind, Kind::Hls);
    let ids: Vec<_> = v.iter().map(|v| (v.id.as_str(), v.kind)).collect();
    assert_eq!(
        ids,
        [("v1", TrackKind::Muxed), ("v0", TrackKind::Video), ("v2", TrackKind::Audio), ("a0", TrackKind::Audio)]
    );
    assert_eq!(v[0].label, "1920x1080 · 5.0 Mbps · avc1.640028,mp4a.40.2");
    assert_eq!((v[1].width, v[1].height, v[1].bandwidth), (640, 360, 800000));
    assert_eq!(v[3].label, "Türkçe (tr)");
}

#[test]
fn hls_ts_end_to_end_picks_best_variant() {
    let d = TempDir::new("hls-ts");
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=900\nhigh/index.m3u8\n";
    let mut f = Fake::default()
        .with("http://t/v/master.m3u8", master)
        .with("http://t/v/high/index.m3u8", media_list(5, "s"));
    let mut expect = Vec::new();
    for i in 0..5 {
        f = f.with(&format!("http://t/v/high/s{i}.ts"), seg(i, 100 + i));
        expect.extend(seg(i, 100 + i));
    }
    let j = job("http://t/v/master.m3u8", &d);
    let last = RefCell::new(Progress::default());
    let out = run(&f, &j, &AtomicBool::new(false), &|p| *last.borrow_mut() = p).unwrap();
    assert_eq!(out, d.0.join("out").join("film.ts"));
    assert_eq!(std::fs::read(&out).unwrap(), expect);
    assert_eq!(*last.borrow(), Progress { done_segments: 5, total_segments: 5, bytes: expect.len() as u64 });
    assert!(!j.tmp_dir.exists(), "geçici klasör silinmeli");
    assert_eq!(f.hits("http://t/v/low/index.m3u8"), 0);
}

#[test]
fn hls_aes128_explicit_and_derived_iv() {
    let d = TempDir::new("hls-aes");
    let (k1, k2) = ([0x11u8; 16], [0x22u8; 16]);
    let iv2 = seq_iv(0xabcdef);
    let list = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:3\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/k1\"\n\
        #EXTINF:4,\n0.ts\n#EXTINF:4,\n1.ts\n\
        #EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.test/k2\",IV=0x00000000000000000000000000abcdef\n#EXTINF:4,\n2.ts\n\
        #EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\n3.ts\n#EXT-X-ENDLIST\n";
    let plain: Vec<Vec<u8>> = (0..4).map(|i| seg(i, 50 + 7 * i)).collect();
    let f = Fake::default()
        .with("http://t/a/index.m3u8", list)
        .with("http://t/a/keys/k1", k1.to_vec())
        .with("https://keys.test/k2", k2.to_vec())
        // IV verilmediğinde medya sıra numarası: 3 ve 4
        .with("http://t/a/0.ts", encrypt(&k1, &seq_iv(3), &plain[0]))
        .with("http://t/a/1.ts", encrypt(&k1, &seq_iv(4), &plain[1]))
        .with("http://t/a/2.ts", encrypt(&k2, &iv2, &plain[2]))
        .with("http://t/a/3.ts", plain[3].clone());
    let out = run(&f, &job("http://t/a/index.m3u8", &d), &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), plain.concat());
    assert_eq!(f.hits("http://t/a/keys/k1"), 1, "anahtar bir kez alınmalı");
}

#[test]
fn hls_wrong_key_fails() {
    let d = TempDir::new("hls-badkey");
    let list = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\n0.ts\n#EXT-X-ENDLIST\n";
    let f = Fake::default()
        .with("http://t/index.m3u8", list)
        .with("http://t/k", vec![1u8; 16])
        .with("http://t/0.ts", encrypt(&[2u8; 16], &seq_iv(0), b"gizli veri"));
    assert!(run(&f, &job("http://t/index.m3u8", &d), &AtomicBool::new(false), &no_progress).is_err());
    let bad_len = Fake::default()
        .with("http://t/index.m3u8", list)
        .with("http://t/k", vec![1u8; 15])
        .with("http://t/0.ts", vec![0u8; 16]);
    let e = run(&bad_len, &job("http://t/index.m3u8", &d), &AtomicBool::new(false), &no_progress).unwrap_err();
    assert!(format!("{e:#}").contains("16 bayt"), "{e:#}");
}

#[test]
fn hls_fmp4_map_and_byterange_single_file() {
    let d = TempDir::new("hls-fmp4");
    let file: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
    let list = "#EXTM3U\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"100@0\"\n#EXTINF:4,\n#EXT-X-BYTERANGE:120@100\nmedia.mp4\n\
        #EXTINF:4,\n#EXT-X-BYTERANGE:80\nmedia.mp4\n#EXT-X-ENDLIST\n";
    let f = Fake::default().with("http://t/f/index.m3u8", list).with("http://t/f/media.mp4", file.clone());
    let mut j = job("http://t/f/index.m3u8", &d);
    j.out = d.0.join("film.mkv");
    let out = run(&f, &j, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(out, d.0.join("film.mp4"));
    assert_eq!(std::fs::read(out).unwrap(), file);
}

#[test]
fn hls_separate_audio_rendition_gives_two_files() {
    let d = TempDir::new("hls-audio");
    let master = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"g\",NAME=\"en\",URI=\"en/a.m3u8\"\n\
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"g\",NAME=\"tr\",DEFAULT=YES,URI=\"tr/a.m3u8\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"g\"\nv/v.m3u8\n";
    let alist = "#EXTM3U\n#EXTINF:4,\n0.aac\n#EXT-X-ENDLIST\n";
    let f = Fake::default()
        .with("http://t/m.m3u8", master)
        .with("http://t/v/v.m3u8", media_list(2, "s"))
        .with("http://t/v/s0.ts", b"V0".to_vec())
        .with("http://t/v/s1.ts", b"V1".to_vec())
        .with("http://t/tr/a.m3u8", alist)
        .with("http://t/tr/0.aac", b"TR".to_vec())
        .with("http://t/en/a.m3u8", alist)
        .with("http://t/en/0.aac", b"EN".to_vec());
    let outs = run_outputs(&f, &job("http://t/m.m3u8", &d), &AtomicBool::new(false), &no_progress).unwrap();
    let dir = d.0.join("out");
    assert_eq!(outs, [dir.join("film.video.ts"), dir.join("film.audio.aac")]);
    assert_eq!(std::fs::read(&outs[0]).unwrap(), b"V0V1");
    assert_eq!(std::fs::read(&outs[1]).unwrap(), b"TR", "varsayılan ses seçilmeli");

    // Açık seçim: a0 = en
    let d2 = TempDir::new("hls-audio2");
    let mut j = job("http://t/m.m3u8", &d2);
    j.audio = Some("a0".into());
    let outs = run_outputs(&f, &j, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(std::fs::read(&outs[1]).unwrap(), b"EN");
}

#[test]
fn hls_live_is_rejected_without_looping() {
    let d = TempDir::new("hls-live");
    let live = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n";
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlive.m3u8\n";
    let f = Fake::default().with("http://t/live.m3u8", live).with("http://t/m.m3u8", master);
    let e = run(&f, &job("http://t/m.m3u8", &d), &AtomicBool::new(false), &no_progress).unwrap_err();
    assert!(e.to_string().contains("canlı yayın"), "{e}");
    assert_eq!(f.hits("http://t/live.m3u8"), 1);
    assert!(variants(&f, "http://t/live.m3u8").unwrap_err().to_string().contains("canlı yayın"));
}

#[test]
fn unknown_variant_is_an_error() {
    let d = TempDir::new("hls-unknown");
    let f = Fake::default().with("http://t/m.m3u8", "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nx.m3u8\n");
    let mut j = job("http://t/m.m3u8", &d);
    j.video = Some("v9".into());
    assert!(run(&f, &j, &AtomicBool::new(false), &no_progress).unwrap_err().to_string().contains("v9"));
}

const MPD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1" duration="2" media="$RepresentationID$/$Number$.m4s" initialization="$RepresentationID$/init.mp4"/>
      <Representation id="v1" bandwidth="1000000" width="1280" height="720" codecs="avc1.64001f"/>
      <Representation id="v2" bandwidth="3000000" width="1920" height="1080" codecs="avc1.640028"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="tr">
      <SegmentTemplate timescale="48000" media="$RepresentationID$/$Time$.m4s" initialization="$RepresentationID$/init.mp4">
        <SegmentTimeline><S t="0" d="96000" r="-1"/></SegmentTimeline>
      </SegmentTemplate>
      <Representation id="a1" bandwidth="128000" codecs="mp4a.40.2"/>
    </AdaptationSet>
  </Period>
</MPD>"#;

fn dash_fake() -> (Fake, Vec<u8>, Vec<u8>) {
    let mut f = Fake::default().with("http://t/d/m.mpd", MPD);
    let (mut v, mut a) = (b"VINIT".to_vec(), b"AINIT".to_vec());
    f = f.with("http://t/d/v2/init.mp4", b"VINIT".to_vec()).with("http://t/d/a1/init.mp4", b"AINIT".to_vec());
    for n in 1..=3 {
        f = f.with(&format!("http://t/d/v2/{n}.m4s"), seg(n, 40));
        v.extend(seg(n, 40));
    }
    for t in [0, 96000, 192000] {
        f = f.with(&format!("http://t/d/a1/{t}.m4s"), seg(t / 1000, 20));
        a.extend(seg(t / 1000, 20));
    }
    (f, v, a)
}

#[test]
fn dash_variants_listing() {
    let (f, _, _) = dash_fake();
    let (kind, v) = variants(&f, "http://t/d/m.mpd").unwrap();
    assert_eq!(kind, Kind::Dash);
    let ids: Vec<_> = v.iter().map(|v| (v.id.as_str(), v.kind)).collect();
    assert_eq!(ids, [("v2", TrackKind::Video), ("v1", TrackKind::Video), ("a1", TrackKind::Audio)]);
    assert_eq!(v[0].label, "1920x1080 · 3.0 Mbps · avc1.640028");
    assert_eq!(v[2].label, "tr · 128 kbps · mp4a.40.2");
}

#[test]
fn dash_end_to_end_separate_files() {
    let d = TempDir::new("dash");
    let (f, v, a) = dash_fake();
    let j = job("http://t/d/m.mpd", &d);
    let outs = run_outputs(&f, &j, &AtomicBool::new(false), &no_progress).unwrap();
    let dir = d.0.join("out");
    assert_eq!(outs, [dir.join("film.video.mp4"), dir.join("film.audio.m4a")]);
    assert_eq!(std::fs::read(&outs[0]).unwrap(), v);
    assert_eq!(std::fs::read(&outs[1]).unwrap(), a);
    assert_eq!(f.hits("http://t/d/v1/init.mp4"), 0, "en yüksek bant genişliği seçilmeli");
    assert!(!j.tmp_dir.exists());
}

#[test]
fn dash_segment_base_end_to_end() {
    let d = TempDir::new("dash-base");
    let init = b"ftyp+moov".repeat(10);
    let sidx = make_sidx(&[25, 35, 15], 0);
    let idx_start = init.len() as u64;
    let idx_end = idx_start + sidx.len() as u64 - 1;
    let media: Vec<u8> = (0..75u8).collect();
    let file = [init.clone(), sidx, media.clone()].concat();
    let mpd = format!(
        r#"<MPD mediaPresentationDuration="PT6S"><Period><AdaptationSet mimeType="audio/mp4">
           <Representation id="a" codecs="mp4a.40.2"><BaseURL>audio.mp4</BaseURL>
           <SegmentBase indexRange="{idx_start}-{idx_end}"><Initialization range="0-{}"/></SegmentBase>
           </Representation></AdaptationSet></Period></MPD>"#,
        idx_start - 1
    );
    let f = Fake::default().with("http://t/b/m.mpd", mpd).with("http://t/b/audio.mp4", file);
    let last = RefCell::new(Progress::default());
    let out = run(&f, &job("http://t/b/m.mpd", &d), &AtomicBool::new(false), &|p| *last.borrow_mut() = p).unwrap();
    assert_eq!(out, d.0.join("out").join("film.m4a"));
    assert_eq!(std::fs::read(out).unwrap(), [init, media].concat());
    assert_eq!(last.borrow().total_segments, 4);
}

#[test]
fn dash_drm_is_rejected() {
    let drm = MPD.replace("<Representation id=\"a1\"", "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\"/><Representation id=\"a1\"");
    let f = Fake::default().with("http://t/m.mpd", drm);
    let e = variants(&f, "http://t/m.mpd").unwrap_err().to_string();
    assert!(e.contains("DRM korumalı"), "{e}");
}

#[test]
fn many_segments_in_parallel_keep_order() {
    let d = TempDir::new("parallel");
    let mut f = Fake::default().with("http://t/p/i.m3u8", media_list(60, "x"));
    let mut expect = Vec::new();
    for i in 0..60 {
        f = f.with(&format!("http://t/p/x{i}.ts"), seg(i, 10 + i % 7));
        expect.extend(seg(i, 10 + i % 7));
    }
    let mut j = job("http://t/p/i.m3u8", &d);
    j.parallel = 8;
    let calls = AtomicUsize::new(0);
    let out = run(&f, &j, &AtomicBool::new(false), &|_| {
        calls.fetch_add(1, Ordering::Relaxed);
    })
    .unwrap();
    assert_eq!(std::fs::read(out).unwrap(), expect);
    // başlangıç + segment başına bir
    assert_eq!(calls.load(Ordering::Relaxed), 61);
}

#[test]
fn stop_and_resume_skips_downloaded_segments() {
    let d = TempDir::new("resume");
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicUsize::new(0));
    let (s2, seen2) = (stop.clone(), seen.clone());
    let mut f = Fake::default().with("http://t/r/i.m3u8", media_list(8, "s")).hook(move |url| {
        // Üçüncü segment isteğinde kullanıcı "durdur"a basmış olsun
        if url.ends_with(".ts") && seen2.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
            s2.store(true, Ordering::SeqCst);
        }
    });
    let mut expect = Vec::new();
    for i in 0..8 {
        f = f.with(&format!("http://t/r/s{i}.ts"), seg(i, 64));
        expect.extend(seg(i, 64));
    }
    let mut j = job("http://t/r/i.m3u8", &d);
    j.parallel = 2;

    let e = run(&f, &j, &stop, &no_progress).unwrap_err();
    assert!(e.is::<Stopped>(), "{e:#}");
    let done: Vec<usize> = (0..8).filter(|i| j.tmp_dir.join("video").join(format!("{i}.seg")).exists()).collect();
    assert!(done.len() >= 3 && done.len() < 8, "{done:?}");
    assert!(!d.0.join("out").join("film.ts").exists());

    stop.store(false, Ordering::SeqCst);
    let first = RefCell::new(None);
    let out = run(&f, &j, &stop, &|p| {
        first.borrow_mut().get_or_insert(p);
    })
    .unwrap();
    assert_eq!(std::fs::read(out).unwrap(), expect);
    // İkinci çağrı inmiş segmentleri bilerek başlar ve hiçbir segment iki kez istenmez
    assert_eq!(first.borrow().unwrap().done_segments, done.len());
    for i in 0..8 {
        assert_eq!(f.hits(&format!("http://t/r/s{i}.ts")), 1, "s{i}.ts");
    }
}

#[test]
fn changed_plan_discards_stale_segments() {
    let d = TempDir::new("stale");
    let j = job("http://t/c/i.m3u8", &d);
    // Önceki oturumdan başka bir listeye ait kalıntı
    let vdir = j.tmp_dir.join("video");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("plan"), "http://t/c/eski.ts\n").unwrap();
    std::fs::write(vdir.join("0.seg"), b"ESKI").unwrap();
    let f = Fake::default().with("http://t/c/i.m3u8", media_list(1, "s")).with("http://t/c/s0.ts", b"YENI".to_vec());
    let out = run(&f, &j, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), b"YENI");
}

#[test]
fn segment_retries_three_times() {
    let d = TempDir::new("retry");
    let base = Fake::default().with("http://t/x/i.m3u8", media_list(2, "s")).with("http://t/x/s0.ts", b"A".to_vec());
    let ok = base.with("http://t/x/s1.ts", b"B".to_vec()).failing("http://t/x/s1.ts", 3);
    let out = run(&ok, &job("http://t/x/i.m3u8", &d), &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), b"AB");
    assert_eq!(ok.hits("http://t/x/s1.ts"), 4);

    let d2 = TempDir::new("retry-fail");
    let bad = Fake::default()
        .with("http://t/x/i.m3u8", media_list(2, "s"))
        .with("http://t/x/s0.ts", b"A".to_vec())
        .with("http://t/x/s1.ts", b"B".to_vec())
        .failing("http://t/x/s1.ts", 4);
    let mut j = job("http://t/x/i.m3u8", &d2);
    j.parallel = 1;
    let e = run(&bad, &j, &AtomicBool::new(false), &no_progress).unwrap_err();
    assert!(format!("{e:#}").contains("http://t/x/s1.ts"), "{e:#}");
    assert_eq!(bad.hits("http://t/x/s1.ts"), 4);
    // Başarılı segment devam için korunur
    assert!(j.tmp_dir.join("video").join("0.seg").exists());
}

#[test]
fn missing_ffmpeg_binary_keeps_progress() {
    let d = TempDir::new("noff");
    let f = Fake::default().with("http://t/n/i.m3u8", media_list(1, "s")).with("http://t/n/s0.ts", b"A".to_vec());
    let mut j = job("http://t/n/i.m3u8", &d);
    j.ffmpeg = Some(d.0.join("yok").join("ffmpeg.exe"));
    let e = run(&f, &j, &AtomicBool::new(false), &no_progress).unwrap_err();
    assert!(format!("{e:#}").contains("ffmpeg"), "{e:#}");
    assert!(j.tmp_dir.join("video").join("0.seg").exists());
}
