//! Site Grabber ve ZIP önizleme. Tarama ve ZIP ayrıştırma `kdm-grabber` / `kdm-zip`'te; burada motorun HTTP ajanı
//! (vekil, kullanıcı ajanı, çerez) bu crate'lerin `Fetcher` / `RangeSource` trait'lerine bağlanır.

use crate::kcore::{self, settings};
use kdm_core::engine::http::{self, ByteRange, DigestState, RequestSpec};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Taranan sayfa gövdesinin üst sınırı: dev bir dosyayı sayfa sanıp belleğe almayalım
const PAGE_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Default)]
struct Http {
    agent: Option<kdm_core::ureq::Agent>,
    spec: RequestSpec,
}

impl Http {
    fn new(referer: &str, cookies: &str) -> Result<Self, String> {
        Self::with_net(&settings().net, referer, cookies)
    }

    fn with_net(net: &http::NetConfig, referer: &str, cookies: &str) -> Result<Self, String> {
        let agent = http::agent(net).map_err(|e| format!("{e:#}"))?;
        Ok(Http { agent: Some(agent), spec: RequestSpec { referer: referer.into(), cookies: cookies.into(), ..Default::default() } })
    }

    fn get(&self, url: &str, range: Option<ByteRange>) -> anyhow::Result<http::Response> {
        let spec = RequestSpec { url: url.to_string(), ..self.spec.clone() };
        http::get(self.agent.as_ref().expect("ajan"), &spec, range, "", &DigestState::default())
    }
}

impl kdm_grabber::Fetcher for Http {
    fn get(&self, url: &str) -> anyhow::Result<(String, String, Vec<u8>)> {
        let r = Http::get(self, url, None)?;
        if r.meta.status != 200 {
            anyhow::bail!("HTTP {}", r.meta.status);
        }
        let mut body = Vec::new();
        r.body.into_reader().take(PAGE_LIMIT).read_to_end(&mut body)?;
        Ok((r.meta.final_url, r.meta.content_type, body))
    }

    fn size(&self, url: &str) -> Option<u64> {
        let r = Http::get(self, url, Some(ByteRange { start: 0, end: Some(0) })).ok()?;
        r.meta.total_size().or(r.meta.content_range.and_then(|c| c.2))
    }
}

// ---------- Grabber ----------

#[derive(Deserialize)]
#[serde(default)]
pub struct GrabForm {
    pub start_url: String,
    pub same_site_depth: u32,
    pub other_site_depth: u32,
    pub file_patterns: String,
    pub exclude_patterns: String,
    pub include_paths: String,
    pub exclude_paths: String,
    pub min_size_kb: u64,
    pub max_size_kb: u64,
    pub max_pages: usize,
    pub respect_robots: bool,
    pub delay_ms: u64,
    pub cookies: String,
}

impl Default for GrabForm {
    fn default() -> Self {
        GrabForm {
            start_url: String::new(),
            same_site_depth: 2,
            other_site_depth: 0,
            file_patterns: "*.zip *.pdf".into(),
            exclude_patterns: String::new(),
            include_paths: String::new(),
            exclude_paths: String::new(),
            min_size_kb: 0,
            max_size_kb: 0,
            max_pages: 200,
            respect_robots: true,
            delay_ms: 200,
            cookies: String::new(),
        }
    }
}

fn words(s: &str) -> Vec<String> {
    s.split([' ', ',', ';', '\n']).map(str::trim).filter(|w| !w.is_empty()).map(String::from).collect()
}

#[derive(Clone, Serialize)]
pub struct FoundView {
    pub url: String,
    pub referer: String,
    pub size: Option<u64>,
}

static GRAB_STOP: once_cell::sync::Lazy<Mutex<Option<Arc<AtomicBool>>>> = once_cell::sync::Lazy::new(|| Mutex::new(None));

/// Taramayı arka planda başlatır; ilerleme `grabber` olayı, sonuç `grabber_done` olayıyla gelir.
#[tauri::command]
pub fn grab_start(form: GrabForm) -> Result<(), String> {
    let url = url::Url::parse(form.start_url.trim()).map_err(|e| e.to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("http/https".into());
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut g = GRAB_STOP.lock();
        if g.is_some() {
            return Err(crate::i18n::t("grab.busy"));
        }
        *g = Some(stop.clone());
    }
    let fetcher = Http::new(form.start_url.trim(), &form.cookies)?;
    let rule = kdm_grabber::Rule {
        start_url: url.to_string(),
        same_site_depth: form.same_site_depth.min(20),
        other_site_depth: form.other_site_depth.min(5),
        file_patterns: words(&form.file_patterns),
        exclude_patterns: words(&form.exclude_patterns),
        include_paths: words(&form.include_paths),
        exclude_paths: words(&form.exclude_paths),
        min_size: form.min_size_kb * 1024,
        max_size: form.max_size_kb * 1024,
        max_pages: form.max_pages,
        respect_robots: form.respect_robots,
        delay_ms: form.delay_ms,
        user_agent_token: "KaradelikDM".into(),
    };
    std::thread::spawn(move || {
        let result = kdm_grabber::crawl(&fetcher, &rule, &stop, &|p| {
            kcore::emit("grabber", serde_json::json!({ "pages": p.pages, "queued": p.queued, "found": p.found, "current": p.current }));
        });
        *GRAB_STOP.lock() = None;
        match result {
            Ok(list) => {
                let list: Vec<FoundView> = list.into_iter().map(|f| FoundView { url: f.url, referer: f.referer, size: f.size }).collect();
                kcore::emit("grabber_done", serde_json::json!({ "ok": true, "found": list }));
            }
            Err(e) => kcore::emit("grabber_done", serde_json::json!({ "ok": false, "error": format!("{e:#}") })),
        }
    });
    Ok(())
}

#[tauri::command]
pub fn grab_stop() {
    if let Some(s) = GRAB_STOP.lock().as_ref() {
        s.store(true, Ordering::Relaxed);
    }
}

// ---------- ZIP önizleme ----------

struct RemoteZip {
    http: Http,
    url: String,
    len: u64,
}

impl kdm_zip::RangeSource for RemoteZip {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, len: u64) -> anyhow::Result<Vec<u8>> {
        let end = offset + len.max(1) - 1;
        let r = self.http.get(&self.url, Some(ByteRange { start: offset, end: Some(end) }))?;
        if r.meta.status != 206 || r.meta.content_range.map(|c| c.0) != Some(offset) {
            anyhow::bail!("{}", crate::i18n::t("zip.no_range"));
        }
        let mut v = Vec::with_capacity(len as usize);
        r.body.into_reader().take(len).read_to_end(&mut v)?;
        Ok(v)
    }
}

fn remote_zip(url: &str, referer: &str, cookies: &str) -> Result<RemoteZip, String> {
    open_remote_zip(Http::new(referer, cookies)?, url)
}

fn open_remote_zip(http: Http, url: &str) -> Result<RemoteZip, String> {
    let r = http.get(url, Some(ByteRange { start: 0, end: Some(0) })).map_err(|e| format!("{e:#}"))?;
    let len = match (r.meta.status, r.meta.content_range) {
        (206, Some((_, _, Some(total)))) => total,
        _ => return Err(crate::i18n::t("zip.no_range")),
    };
    Ok(RemoteZip { http, url: url.to_string(), len })
}

#[derive(Serialize)]
pub struct ZipEntryView {
    pub index: usize,
    pub name: String,
    pub size: u64,
    pub compressed: u64,
    pub is_dir: bool,
    pub encrypted: bool,
    pub supported: bool,
}

#[tauri::command]
pub async fn zip_list(url: String, referer: String, cookies: String) -> Result<Vec<ZipEntryView>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = remote_zip(&url, &referer, &cookies)?;
        let entries = kdm_zip::list(&src).map_err(|e| format!("{e:#}"))?;
        Ok(entries
            .into_iter()
            .map(|e| ZipEntryView {
                index: e.index,
                supported: !e.encrypted && matches!(e.method, 0 | 8),
                name: e.name,
                size: e.size,
                compressed: e.compressed,
                is_dir: e.is_dir,
                encrypted: e.encrypted,
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Seçilen girdileri `dir` altına çıkarır; ilerleme `zip_progress`, sonuç `zip_done` olayı.
#[tauri::command]
pub fn zip_extract(url: String, referer: String, cookies: String, indexes: Vec<usize>, dir: String) -> Result<(), String> {
    let base = PathBuf::from(dir.trim());
    if dir.trim().is_empty() {
        return Err(crate::i18n::t("add.no_dir"));
    }
    std::thread::spawn(move || {
        let run = || -> Result<usize, String> {
            let src = remote_zip(&url, &referer, &cookies)?;
            let entries = kdm_zip::list(&src).map_err(|e| format!("{e:#}"))?;
            let chosen: Vec<_> = entries.into_iter().filter(|e| indexes.contains(&e.index) && !e.is_dir).collect();
            for (i, e) in chosen.iter().enumerate() {
                // Zip Slip: "../" ya da mutlak yol içeren adlar reddedilir
                let out = kdm_zip::safe_join(&base, &e.name).ok_or_else(|| format!("{}: {}", crate::i18n::t("zip.bad_name"), e.name))?;
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(|x| x.to_string())?;
                }
                kcore::emit("zip_progress", serde_json::json!({ "done": i, "total": chosen.len(), "name": e.name }));
                kdm_zip::extract(&src, e, &out).map_err(|x| format!("{}: {x:#}", e.name))?;
            }
            Ok(chosen.len())
        };
        match run() {
            Ok(n) => {
                kcore::notify(&crate::i18n::tf("zip.done", &[&n.to_string()]));
                kcore::emit("zip_done", serde_json::json!({ "ok": true, "count": n }));
            }
            Err(e) => kcore::emit("zip_done", serde_json::json!({ "ok": false, "error": e })),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicU64;

    /// Range destekli küçük statik sunucu; gönderilen toplam baytı sayar
    fn serve(files: Vec<(&'static str, &'static str, Vec<u8>)>) -> (String, Arc<AtomicU64>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let sent = Arc::new(AtomicU64::new(0));
        let sent2 = sent.clone();
        let files = Arc::new(files);
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (files, sent) = (files.clone(), sent2.clone());
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut w = s;
                    let mut line = String::new();
                    let _ = r.read_line(&mut line);
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let mut range = None;
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                            let (a, b) = v.trim().split_once('-').unwrap();
                            range = Some((a.parse::<usize>().unwrap(), b.parse::<usize>().ok()));
                        }
                    }
                    let Some((_, ct, body)) = files.iter().find(|f| f.0 == path) else {
                        let _ = w.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    };
                    let (status, a, b) = match range {
                        Some((a, b)) => (206, a, b.unwrap_or(body.len() - 1).min(body.len() - 1)),
                        None => (200, 0, body.len() - 1),
                    };
                    let part = &body[a..=b];
                    let mut head = format!("HTTP/1.1 {status} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n", part.len());
                    if status == 206 {
                        head += &format!("Content-Range: bytes {a}-{b}/{}\r\n", body.len());
                    }
                    let _ = w.write_all(format!("{head}\r\n").as_bytes());
                    let _ = w.write_all(part);
                    sent.fetch_add(part.len() as u64, Ordering::Relaxed);
                });
            }
        });
        (base, sent)
    }

    fn sample_zip() -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        // Büyük, sıkışmaz bir dosya: önizlemenin tamamını indirmediğini göstermek için
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
        z.start_file("buyuk.bin", SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        z.write_all(&big).unwrap();
        z.start_file("belgeler/okubeni.txt", SimpleFileOptions::default()).unwrap();
        z.write_all("Karadelik ZIP önizleme testi\n".repeat(200).as_bytes()).unwrap();
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn remote_zip_lists_and_extracts_only_what_is_needed() {
        let data = sample_zip();
        let total = data.len() as u64;
        let (base, sent) = serve(vec![("/a.zip", "application/zip", data)]);
        let http = Http::with_net(&http::NetConfig::default(), "", "").unwrap();
        let src = open_remote_zip(http, &format!("{base}/a.zip")).unwrap();
        let entries = kdm_zip::list(&src).unwrap();
        let txt = entries.iter().find(|e| e.name == "belgeler/okubeni.txt").expect("girdi");
        let out_dir = std::env::temp_dir().join(format!("kdm-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        let out = kdm_zip::safe_join(&out_dir, &txt.name).unwrap();
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        kdm_zip::extract(&src, txt, &out).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "Karadelik ZIP önizleme testi\n".repeat(200));
        // Listeleme + küçük dosyanın çıkarılması arşivin küçük bir kısmını indirmeli
        let got = sent.load(Ordering::Relaxed);
        assert!(got < total / 10, "{got} / {total} bayt indirildi");
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn zip_preview_needs_range_support() {
        let (base, _) = serve(vec![("/x.zip", "application/zip", vec![1, 2, 3])]);
        // Range'i kabul eden ama zip olmayan dosya: anlaşılır hata, panik değil
        let http = Http::with_net(&http::NetConfig::default(), "", "").unwrap();
        let src = open_remote_zip(http, &format!("{base}/x.zip")).unwrap();
        assert!(kdm_zip::list(&src).is_err());
    }

    #[test]
    fn grabber_finds_files_through_our_http_layer() {
        let (base, _) = serve(vec![
            ("/", "text/html", br#"<a href="/docs/">belgeler</a> <a href="/a.zip">a</a> <a href="/b.exe">b</a>"#.to_vec()),
            ("/docs/", "text/html", br#"<a href="rapor.pdf">r</a><a href="/docs/derin/">d</a>"#.to_vec()),
            ("/docs/derin/", "text/html", br#"<a href="gizli.zip">g</a>"#.to_vec()),
            ("/a.zip", "application/zip", vec![0; 1000]),
            ("/b.exe", "application/octet-stream", vec![0; 10]),
            ("/docs/rapor.pdf", "application/pdf", vec![0; 2048]),
            ("/docs/derin/gizli.zip", "application/zip", vec![0; 10]),
        ]);
        let http = Http::with_net(&http::NetConfig::default(), "", "").unwrap();
        let rule = kdm_grabber::Rule {
            start_url: format!("{base}/"),
            same_site_depth: 1,
            file_patterns: vec!["*.zip".into(), "*.pdf".into()],
            respect_robots: false,
            ..Default::default()
        };
        let found = kdm_grabber::crawl(&http, &rule, &AtomicBool::new(false), &|_| {}).unwrap();
        let urls: Vec<&str> = found.iter().map(|f| f.url.as_str()).collect();
        assert!(urls.contains(&format!("{base}/a.zip").as_str()), "{urls:?}");
        assert!(urls.contains(&format!("{base}/docs/rapor.pdf").as_str()), "{urls:?}");
        // .exe deseni yok; derinlik 1 olduğu için /docs/derin/ sayfası taranmaz
        assert!(!urls.iter().any(|u| u.ends_with(".exe") || u.ends_with("gizli.zip")), "{urls:?}");
        let pdf = found.iter().find(|f| f.url.ends_with("rapor.pdf")).unwrap();
        assert_eq!(pdf.size, Some(2048));
        assert_eq!(pdf.referer, format!("{base}/docs/"));
    }
}
