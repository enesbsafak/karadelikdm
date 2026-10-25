//! Genişlik öncelikli gezinme. Sayfalar kuyruğa derinlik kurallarıyla girer; dosyalar bulundukları anda
//! filtrelenip sonuca eklenir ve hiç indirilmez (yalnız boyutu sorulur).

use crate::links::{extract_links, Link};
use crate::robots::Robots;
use crate::{pattern, urls, Fetcher, Found, Progress, Rule};
use anyhow::{anyhow, bail, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use url::Url;

struct Page {
    url: String,
    referer: String,
    /// Başlangıçtan bu yana sayfa adımı
    depth: u32,
    /// Başlangıç sitesi dışında arka arkaya atılan adım (başlangıç sitesindeyse 0)
    away: u32,
}

struct Crawler<'a> {
    f: &'a dyn Fetcher,
    rule: &'a Rule,
    stop: &'a AtomicBool,
    /// Başlangıç sitesi; başlangıç sayfası başka bir alana yönlendirirse o da eklenir
    homes: Vec<String>,
    queue: VecDeque<Page>,
    /// Kuyruğa giren sayfalar: döngüler ve tekrarlar burada kesilir
    queued: HashSet<String>,
    /// Getirilmiş sayfaların son adresleri (yönlendirmeyle aynı sayfaya ikinci kez varılırsa atlanır)
    done: HashSet<String>,
    /// Dosya olarak kabul edilmiş adresler
    offered: HashSet<String>,
    found: Vec<Found>,
    /// Köken başına robots.txt
    robots: HashMap<String, Robots>,
    pages: usize,
    last_request: Option<Instant>,
}

/// Kurala göre siteyi gezer ve bulunan dosyaları keşif sırasıyla döndürür. `stop` kurulunca o ana kadar
/// bulunanlarla döner. Yalnız başlangıç adresi geçersizse, robots.txt onu yasaklıyorsa ya da başlangıç
/// sayfası alınamazsa hata verir; diğer sayfaların hataları atlanır.
pub fn crawl(f: &dyn Fetcher, rule: &Rule, stop: &AtomicBool, on_progress: &dyn Fn(&Progress)) -> Result<Vec<Found>> {
    let start = Url::parse(rule.start_url.trim()).map_err(|e| anyhow!("geçersiz başlangıç adresi: {e}"))?;
    if !matches!(start.scheme(), "http" | "https") {
        bail!("başlangıç adresi http ya da https olmalı");
    }
    let start_url = urls::normalize(start.clone()).ok_or_else(|| anyhow!("geçersiz başlangıç adresi"))?;
    let mut c = Crawler {
        f,
        rule,
        stop,
        homes: vec![urls::site_key(&start)],
        queue: VecDeque::new(),
        queued: HashSet::new(),
        done: HashSet::new(),
        offered: HashSet::new(),
        found: Vec::new(),
        robots: HashMap::new(),
        pages: 0,
        last_request: None,
    };
    c.queued.insert(start_url.clone());
    c.queue.push_back(Page { url: start_url, referer: String::new(), depth: 0, away: 0 });

    let mut first = true;
    while let Some(page) = c.queue.pop_front() {
        let is_start = std::mem::take(&mut first);
        if c.stopped() || (rule.max_pages > 0 && c.pages >= rule.max_pages) {
            break;
        }
        if c.done.contains(&page.url) {
            continue;
        }
        if !c.robots_allow(&page.url) {
            if is_start {
                bail!("robots.txt başlangıç sayfasına izin vermiyor");
            }
            continue;
        }
        on_progress(&c.progress(&page.url));
        if !c.wait() {
            break;
        }
        c.pages += 1;
        let (final_url, ctype, body) = match f.get(&page.url) {
            Ok(r) => r,
            Err(e) if is_start => return Err(e.context("başlangıç sayfası alınamadı")),
            Err(_) => continue,
        };
        // Yönlendirme sonrası adres bağlantıların tabanıdır; geçersizse istenen adres kullanılır
        let base = Url::parse(&final_url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .unwrap_or_else(|| Url::parse(&page.url).expect("kuyruktaki adres geçerli"));
        let base_norm = urls::normalize(base.clone()).unwrap_or_else(|| page.url.clone());
        c.done.insert(page.url.clone());
        if base_norm != page.url && !c.done.insert(base_norm.clone()) {
            continue;
        }
        c.queued.insert(base_norm.clone());
        if is_start {
            let home = urls::site_key(&base);
            if !c.homes.contains(&home) {
                c.homes.push(home);
            }
        }

        if !is_html(&ctype, &body) {
            // Sayfa sandığımız adres dosya çıktı (ör. uzantısız indirme bağlantısı)
            c.offer_file(&page.url, &page.referer, false);
            continue;
        }
        // HTML'in kendisi yalnız açıkça istenmişse (ör. "*.html") sonuçtur; "*" bütün sayfaları eklemesin
        c.offer_file(&page.url, &page.referer, true);
        let html = decode_body(&body);
        for link in extract_links(&html, base.as_str()) {
            if c.stopped() {
                break;
            }
            c.consider(link, &page, &base_norm);
        }
        on_progress(&c.progress(&base_norm));
    }
    Ok(c.found)
}

impl Crawler<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn progress(&self, current: &str) -> Progress {
        Progress { pages: self.pages, queued: self.queue.len(), found: self.found.len(), current: current.to_owned() }
    }

    /// İstekler arasında `delay_ms` bekler. Durdurma beklerken de fark edilir; durdurulduysa `false`.
    fn wait(&mut self) -> bool {
        if let (Some(last), true) = (self.last_request, self.rule.delay_ms > 0) {
            let due = last + Duration::from_millis(self.rule.delay_ms);
            loop {
                let now = Instant::now();
                if now >= due || self.stopped() {
                    break;
                }
                std::thread::sleep((due - now).min(Duration::from_millis(50)));
            }
        }
        self.last_request = Some(Instant::now());
        !self.stopped()
    }

    fn consider(&mut self, link: Link, parent: &Page, parent_url: &str) {
        let Ok(u) = Url::parse(&link.url) else { return };
        if !self.path_ok(&u) {
            return;
        }
        // Yalnız gezinme bağlantıları sayfa olabilir; img/script/link hedefleri her zaman kaynak dosyasıdır
        let nav = matches!(link.tag.as_str(), "a" | "area" | "iframe" | "frame" | "meta");
        if nav && matches!(u.scheme(), "http" | "https") && urls::looks_like_page(&u) {
            if self.queued.contains(&link.url) {
                return;
            }
            let same = self.homes.contains(&urls::site_key(&u));
            let depth = parent.depth + 1;
            let away = if same { 0 } else { parent.away + 1 };
            let ok = if same { depth <= self.rule.same_site_depth } else { away <= self.rule.other_site_depth };
            if ok {
                self.queued.insert(link.url.clone());
                self.queue.push_back(Page { url: link.url, referer: parent_url.to_owned(), depth, away });
            } else {
                // Derinlik dışı sayfa, açıkça istenen bir desene uyuyorsa (ör. "*.php") getirilmeden eklenir
                self.offer_file(&link.url, parent_url, true);
            }
            return;
        }
        self.offer_file(&link.url, parent_url, false);
    }

    /// Dosya adayını filtrelerden geçirip sonuca ekler. `specific`: yalnız "*" dışındaki desenler sayılır.
    fn offer_file(&mut self, url: &str, referer: &str, specific: bool) {
        if self.stopped() || self.offered.contains(url) {
            return;
        }
        let Ok(u) = Url::parse(url) else { return };
        if !self.file_ok(&u, specific) {
            return;
        }
        self.offered.insert(url.to_owned());
        if !self.robots_allow(url) || !self.wait() {
            return;
        }
        let size = self.f.size(url);
        let (min, max) = (self.rule.min_size, self.rule.max_size);
        if size.is_some_and(|s| (min > 0 && s < min) || (max > 0 && s > max)) {
            return;
        }
        self.found.push(Found { url: url.to_owned(), referer: referer.to_owned(), size });
    }

    fn file_ok(&self, u: &Url, specific: bool) -> bool {
        let name = urls::file_name(u);
        let path = urls::decode(u.path());
        let hit = |p: &&String| {
            let p = p.trim();
            !p.is_empty() && pattern::wildcard_ci(p, if p.contains('/') { &path } else { &name })
        };
        let pats: Vec<&String> = self.rule.file_patterns.iter().filter(|p| !p.trim().is_empty()).collect();
        let included = if specific {
            pats.iter().any(|p| !is_universal(p) && hit(p))
        } else {
            pats.is_empty() || pats.iter().any(hit)
        };
        included && !self.rule.exclude_patterns.iter().any(|p| hit(&p))
    }

    fn path_ok(&self, u: &Url) -> bool {
        let path = urls::decode(u.path());
        let inc: Vec<&str> = self.rule.include_paths.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
        let excluded =
            self.rule.exclude_paths.iter().map(|p| p.trim()).any(|p| !p.is_empty() && pattern::path_matches(p, &path));
        (inc.is_empty() || inc.iter().any(|p| pattern::path_matches(p, &path))) && !excluded
    }

    /// robots.txt köken başına bir kez alınır. Alınamazsa her şeye izin verilir: `Fetcher` durum kodu
    /// bildirmediği için 404 ile 5xx ayırt edilemez.
    fn robots_allow(&mut self, url: &str) -> bool {
        if !self.rule.respect_robots {
            return true;
        }
        let Ok(u) = Url::parse(url) else { return false };
        if !matches!(u.scheme(), "http" | "https") {
            return true;
        }
        let origin = u.origin().ascii_serialization();
        if !self.robots.contains_key(&origin) {
            let r = if self.wait() {
                match self.f.get(&format!("{origin}/robots.txt")) {
                    Ok((_, _, body)) => Robots::parse(&body, &self.rule.user_agent_token),
                    Err(_) => Robots::allow_all(),
                }
            } else {
                Robots::allow_all()
            };
            self.robots.insert(origin.clone(), r);
        }
        let mut path = u.path().to_owned();
        if let Some(q) = u.query() {
            path.push('?');
            path.push_str(q);
        }
        self.robots[&origin].allowed(&path)
    }
}

fn is_universal(p: &str) -> bool {
    let p = p.trim();
    p.chars().all(|c| c == '*') || p == "*.*"
}

/// Content-Type'a göre; yoksa gövdenin başına bakılır.
fn is_html(ctype: &str, body: &[u8]) -> bool {
    let mime = ctype.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if !mime.is_empty() {
        return mime == "text/html" || mime == "application/xhtml+xml";
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(512)]).trim_start().to_ascii_lowercase();
    ["<!doctype html", "<html", "<head", "<body"].iter().any(|p| head.starts_with(p))
}

/// UTF-8 değilse Latin-1 sayılır: bağlantılar çoğunlukla ASCII'dir ve hiçbir bayt kaybolmaz.
fn decode_body(body: &[u8]) -> String {
    match std::str::from_utf8(body) {
        Ok(s) => s.to_owned(),
        Err(_) => body.iter().map(|&b| b as char).collect(),
    }
}
