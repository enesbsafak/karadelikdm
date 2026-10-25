//! Birleştirme: parçaları sırayla eklemek ve ffmpeg ile kap birleştirme (`-c copy`, yeniden kodlama yok).

use crate::download::seg_path;
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// İzin parçalarını `out`'a ekler. Önce `.part` adına yazılır: yarım kalan birleştirme son dosya sanılmasın.
pub(crate) fn concat(dir: &Path, count: usize, out: &Path) -> Result<()> {
    let part = part_name(out);
    {
        let mut w = BufWriter::with_capacity(1 << 20, fs::File::create(&part).with_context(|| format!("yazılamadı: {}", part.display()))?);
        for i in 0..count {
            let p = seg_path(dir, i);
            let mut r = fs::File::open(&p).with_context(|| format!("segment eksik: {}", p.display()))?;
            std::io::copy(&mut r, &mut w)?;
        }
        w.flush()?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    fs::rename(&part, out).with_context(|| format!("taşınamadı: {}", out.display()))?;
    Ok(())
}

fn part_name(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// ffmpeg uzantıdan kap seçer; geçici çıktı adı son uzantıyı korumalı (`film.kdm-part.mp4`).
fn ffmpeg_tmp(out: &Path) -> PathBuf {
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "cikti".into());
    let name = match out.extension() {
        Some(e) => format!("{stem}.kdm-part.{}", e.to_string_lossy()),
        None => format!("{stem}.kdm-part"),
    };
    out.with_file_name(name)
}

pub(crate) fn ffmpeg_args(inputs: &[PathBuf], out: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-y"].map(OsString::from).to_vec();
    for i in inputs {
        a.push("-i".into());
        a.push(i.into());
    }
    if inputs.len() > 1 {
        // Ayrı ses izi eklenince varsayılan seçim yalnızca bir ses alır; ID3 gibi veri akışları ise mp4'e
        // kopyalanamaz. Bu yüzden yalnızca görüntü ve ses açıkça eşlenir.
        for m in ["0:v?", "0:a?", "1:a?"] {
            a.push("-map".into());
            a.push(m.into());
        }
    }
    a.extend(["-c", "copy"].map(OsString::from));
    a.push(out.into());
    a
}

pub(crate) fn run_ffmpeg(exe: &Path, inputs: &[PathBuf], out: &Path) -> Result<()> {
    let tmp = ffmpeg_tmp(out);
    let mut cmd = Command::new(exe);
    cmd.args(ffmpeg_args(inputs, &tmp)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: GUI uygulamasından başlatılınca konsol penceresi açılmasın
        cmd.creation_flags(0x0800_0000);
    }
    let o = cmd.output().with_context(|| format!("ffmpeg çalıştırılamadı: {}", exe.display()))?;
    if !o.status.success() {
        let _ = fs::remove_file(&tmp);
        let err = String::from_utf8_lossy(&o.stderr);
        let tail: Vec<&str> = err.lines().rev().take(5).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("ffmpeg birleştirme başarısız ({}): {}", o.status, tail.join(" | "));
    }
    fs::rename(&tmp, out).with_context(|| format!("taşınamadı: {}", out.display()))?;
    Ok(())
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// ffmpeg'i bulur: önce `hint` (dosya ya da klasör), sonra PATH.
pub fn find_ffmpeg(hint: &str) -> Option<PathBuf> {
    let hint = hint.trim().trim_matches('"');
    if !hint.is_empty() {
        let p = PathBuf::from(hint);
        if p.is_file() {
            return Some(p);
        }
        let inner = p.join(exe_name());
        if inner.is_file() {
            return Some(inner);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe_name())).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_single_and_dual_input() {
        let out = Path::new("C:/x/film.mp4");
        let one: Vec<String> = ffmpeg_args(&[PathBuf::from("a.ts")], out).iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(one, ["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i", "a.ts", "-c", "copy", "C:/x/film.mp4"]);
        let two: Vec<String> = ffmpeg_args(&[PathBuf::from("v.mp4"), PathBuf::from("a.m4a")], out)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let s = two.join(" ");
        assert!(s.contains("-i v.mp4 -i a.m4a -map 0:v? -map 0:a? -map 1:a? -c copy C:/x/film.mp4"), "{s}");
    }

    #[test]
    fn tmp_name_keeps_extension() {
        assert_eq!(ffmpeg_tmp(Path::new("d/film.mkv")), Path::new("d/film.kdm-part.mkv"));
    }

    #[test]
    fn find_ffmpeg_does_not_panic() {
        let _ = find_ffmpeg("");
        let _ = find_ffmpeg("   ");
        // Var olmayan ipucu PATH'e düşer, hata vermez
        let _ = find_ffmpeg("Z:/kesinlikle/yok/ffmpeg.exe");
    }

    #[test]
    fn find_ffmpeg_hint_file_and_dir() {
        let dir = std::env::temp_dir().join(format!("kdm-media-ff-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(exe_name());
        fs::write(&exe, b"").unwrap();
        assert_eq!(find_ffmpeg(&exe.to_string_lossy()), Some(exe.clone()));
        assert_eq!(find_ffmpeg(&dir.to_string_lossy()), Some(exe));
        fs::remove_dir_all(&dir).unwrap();
    }
}
