//! Sahte site üzerinde çevrimdışı gezinme testleri.

use kdm_grabber::{crawl, Fetcher, Found, Progress, Rule};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Default)]
struct Site {
    /// adres → (content-type, gövde)
    pages: HashMap<String, (String, String)>,
    redirects: HashMap<String, String>,
    sizes: HashMap<String, u64>,
    gets: Mutex<Vec<String>>,
    size_calls: Mutex<Vec<String>>,
}

impl Site {
    fn html(mut self, url: &str, body: &str) -> Self {
        self.pages.insert(url.into(), ("text/html; charset=utf-8".into(), body.into()));
        self
    }
    fn file(mut self, url: &str, ctype: &str) -> Self {
        self.pages.insert(url.into(), (ctype.into(), "ikili".into()));
        self
    }
    fn robots(self, origin: &str, body: &str) -> Self {
        self.file(&format!("{origin}/robots.txt"), "text/plain").set_body(&format!("{origin}/robots.txt"), body)
    }
    fn set_body(mut self, url: &str, body: &str) -> Self {
        self.pages.get_mut(url).unwrap().1 = body.into();
        self
    }
    fn size(mut self, url: &str, n: u64) -> Self {
        self.sizes.insert(url.into(), n);
        self
    }
    fn redirect(mut self, from: &str, to: &str) -> Self {
        self.redirects.insert(from.into(), to.into());
        self
    }
    fn gets(&self) -> Vec<String> {
        self.gets.lock().unwrap().clone()
    }
    /// robots.txt dışındaki sayfa istekleri
    fn page_gets(&self) -> Vec<String> {
        self.gets().into_iter().filter(|u| !u.ends_with("/robots.txt")).collect()
    }
}

impl Fetcher for Site {
    fn get(&self, url: &str) -> anyhow::Result<(String, String, Vec<u8>)> {
        self.gets.lock().unwrap().push(url.to_owned());
        let target = self.redirects.get(url).cloned().unwrap_or_else(|| url.to_owned());
        let (ct, body) = self.pages.get(&target).ok_or_else(|| anyhow::anyhow!("HTTP 404"))?;
        Ok((target, ct.clone(), body.clone().into_bytes()))
    }
    fn size(&self, url: &str) -> Option<u64> {
        self.size_calls.lock().unwrap().push(url.to_owned());
        self.sizes.get(url).copied()
    }
}

fn rule(start: &str) -> Rule {
    Rule { start_url: start.into(), same_site_depth: 5, respect_robots: false, ..Rule::default() }
}

fn run(site: &Site, rule: &Rule) -> Vec<Found> {
    crawl(site, rule, &AtomicBool::new(false), &|_| {}).unwrap()
}

fn urls(found: &[Found]) -> Vec<&str> {
    found.iter().map(|f| f.url.as_str()).collect()
}

/// Zincir: index → a1 → a2 → a3, her sayfada bir zip.
fn chain() -> Site {
    Site::default()
        .html("http://site.com/", r#"<a href="a1.html">1</a><a href="f0.zip">z</a>"#)
        .html("http://site.com/a1.html", r#"<a href="a2.html">2</a><a href="f1.zip">z</a>"#)
        .html("http://site.com/a2.html", r#"<a href="a3.html">3</a><a href="f2.zip">z</a>"#)
        .html("http://site.com/a3.html", r#"<a href="f3.zip">z</a>"#)
}

#[test]
fn same_site_depth_limits_pages_not_files() {
    let site = chain();
    for (depth, want) in [(0, 1), (1, 2), (2, 3), (3, 4), (9, 4)] {
        let site = chain();
        let r = Rule { same_site_depth: depth, ..rule("http://site.com/") };
        let found = run(&site, &r);
        let expect: Vec<String> = (0..want).map(|i| format!("http://site.com/f{i}.zip")).collect();
        assert_eq!(urls(&found), expect, "derinlik {depth}");
        // Getirilen sayfa sayısı derinlik+1; dosyalar hiç getirilmez
        assert_eq!(site.page_gets().len(), want.min(depth as usize + 1));
    }
    // Referer, dosyanın bulunduğu sayfadır
    let found = run(&site, &rule("http://site.com/"));
    let want = Found { url: "http://site.com/f2.zip".into(), referer: "http://site.com/a2.html".into(), size: None };
    assert_eq!(found[2], want);
    assert_eq!(found[0].referer, "http://site.com/");
}

fn multi_site() -> Site {
    Site::default()
        .html(
            "http://site.com/",
            r#"<a href="http://other.com/p.html">o</a><a href="http://cdn.com/direct.zip">cdn</a>
               <a href="http://www.site.com/same.html">www</a>"#,
        )
        .html("http://site.com/same.html", r#"<a href="s.zip">s</a>"#)
        .html(
            "http://other.com/p.html",
            r#"<a href="o.zip">o</a><a href="http://third.com/q.html">q</a><a href="http://site.com/geri.html">g</a>"#,
        )
        .html("http://third.com/q.html", r#"<a href="t.zip">t</a>"#)
        .html("http://site.com/geri.html", r#"<a href="g.zip">g</a>"#)
        .redirect("http://www.site.com/same.html", "http://site.com/same.html")
}

#[test]
fn other_site_depth() {
    // 0: başka siteye gidilmez ama başka sitedeki dosya bağlantısı yine toplanır; www. aynı sitedir
    let site = multi_site();
    let found = run(&site, &Rule { other_site_depth: 0, ..rule("http://site.com/") });
    assert_eq!(urls(&found), ["http://cdn.com/direct.zip", "http://site.com/s.zip"]);
    assert!(!site.gets().iter().any(|u| u.contains("other.com")));

    let site = multi_site();
    let found = run(&site, &Rule { other_site_depth: 1, ..rule("http://site.com/") });
    assert_eq!(
        urls(&found),
        ["http://cdn.com/direct.zip", "http://other.com/o.zip", "http://site.com/s.zip", "http://site.com/g.zip"]
    );
    assert!(!site.gets().iter().any(|u| u.contains("third.com")));

    let site = multi_site();
    let found = run(&site, &Rule { other_site_depth: 2, ..rule("http://site.com/") });
    assert!(urls(&found).contains(&"http://third.com/t.zip"));

    // Aynı site derinliği 0 iken başka siteden geri dönülen sayfa (derinlik 2) gezilmez
    let site = multi_site();
    let found = run(&site, &Rule { same_site_depth: 0, other_site_depth: 1, ..rule("http://site.com/") });
    assert_eq!(urls(&found), ["http://cdn.com/direct.zip", "http://other.com/o.zip"]);
}

fn files_site() -> Site {
    Site::default()
        .html(
            "http://site.com/",
            r#"<a href="a.zip">1</a><a href="/d/B.ZIP">2</a><a href="c.pdf">3</a><a href="d.exe">4</a>
               <a href="setup-beta.zip">5</a><a href="/arsiv/eski.zip">6</a><img src="resim.png">"#,
        )
        .size("http://site.com/a.zip", 5_000)
        .size("http://site.com/d/B.ZIP", 50)
        .size("http://site.com/c.pdf", 2_000_000)
}

#[test]
fn patterns_and_excludes() {
    let site = files_site();
    let r = Rule {
        file_patterns: vec!["*.zip".into(), "*.PDF".into()],
        exclude_patterns: vec!["*beta*".into()],
        ..rule("http://site.com/")
    };
    let found = run(&site, &r);
    assert_eq!(
        urls(&found),
        ["http://site.com/a.zip", "http://site.com/d/B.ZIP", "http://site.com/c.pdf", "http://site.com/arsiv/eski.zip"]
    );
    assert_eq!(found[0].size, Some(5_000));
    assert_eq!(found[3].size, None);

    // Boyut: bilinmeyen boyut filtreden geçer
    let r = Rule { file_patterns: vec!["*.zip".into(), "*.pdf".into()], min_size: 100, max_size: 1_000_000, ..r };
    assert_eq!(urls(&run(&files_site(), &r)), ["http://site.com/a.zip", "http://site.com/arsiv/eski.zip"]);

    // "*": her kaynak; yol filtreleri
    let r = Rule { exclude_paths: vec!["/arsiv/".into()], ..rule("http://site.com/") };
    let found = run(&files_site(), &r);
    assert_eq!(found.len(), 6);
    assert!(!urls(&found).contains(&"http://site.com/arsiv/eski.zip"));
    let r = Rule { include_paths: vec!["/d/*".into()], ..rule("http://site.com/") };
    assert_eq!(urls(&run(&files_site(), &r)), ["http://site.com/d/B.ZIP"]);
    // `/` içeren desen yolun tamamına uygulanır
    let r = Rule { file_patterns: vec!["/arsiv/*.zip".into()], ..rule("http://site.com/") };
    assert_eq!(urls(&run(&files_site(), &r)), ["http://site.com/arsiv/eski.zip"]);
}

#[test]
fn html_pages_only_reported_when_explicitly_requested() {
    let site = || {
        Site::default()
            .html("http://site.com/", r#"<a href="sayfa.html">s</a><a href="uzak.html">u</a>"#)
            .html("http://site.com/sayfa.html", "<p>merhaba</p>")
    };
    let found = run(&site(), &rule("http://site.com/"));
    assert!(found.is_empty(), "{found:?}");
    // "*.html" ile sayfalar sonuçtur; derinlik dışındakiler getirilmeden eklenir
    let s = site();
    let r = Rule { file_patterns: vec!["*.html".into()], same_site_depth: 0, ..rule("http://site.com/") };
    assert_eq!(urls(&run(&s, &r)), ["http://site.com/sayfa.html", "http://site.com/uzak.html"]);
    assert_eq!(s.page_gets(), ["http://site.com/"]);
}

#[test]
fn page_like_link_that_is_a_file() {
    let site = Site::default()
        .html("http://site.com/", r#"<a href="/indir?id=7">indir</a><a href="/hakkinda">h</a>"#)
        .file("http://site.com/indir?id=7", "application/zip")
        .html("http://site.com/hakkinda", "")
        .size("http://site.com/indir?id=7", 1234);
    let found = run(&site, &rule("http://site.com/"));
    let want = Found { url: "http://site.com/indir?id=7".into(), referer: "http://site.com/".into(), size: Some(1234) };
    assert_eq!(found, [want]);
}

fn robots_site() -> Site {
    Site::default()
        .robots(
            "http://site.com",
            "User-agent: *\nDisallow: /gizli/\nDisallow: /*.exe$\n\nUser-agent: BaskaBot\nDisallow: /\n",
        )
        .html("http://site.com/", r#"<a href="/gizli/sayfa.html">g</a><a href="kur.exe">e</a><a href="a.zip">a</a>"#)
        .html("http://site.com/gizli/sayfa.html", r#"<a href="gizli.zip">z</a>"#)
}

#[test]
fn robots_txt_respected() {
    let site = robots_site();
    let r = Rule { respect_robots: true, ..rule("http://site.com/") };
    assert_eq!(urls(&run(&site, &r)), ["http://site.com/a.zip"]);
    assert!(!site.gets().contains(&"http://site.com/gizli/sayfa.html".to_owned()));
    // köken başına tek robots.txt isteği
    assert_eq!(site.gets().iter().filter(|u| u.ends_with("robots.txt")).count(), 1);

    let site = robots_site();
    let found = run(&site, &rule("http://site.com/"));
    assert_eq!(urls(&found), ["http://site.com/kur.exe", "http://site.com/a.zip", "http://site.com/gizli/gizli.zip"]);
    assert!(!site.gets().iter().any(|u| u.ends_with("robots.txt")));

    // Kendi grubumuzdaki yasak başlangıç sayfasını da kapsar
    let site = robots_site();
    let r = Rule { respect_robots: true, user_agent_token: "BaskaBot".into(), ..rule("http://site.com/") };
    let err = crawl(&site, &r, &AtomicBool::new(false), &|_| {}).unwrap_err();
    assert!(err.to_string().contains("robots.txt"), "{err}");
    assert_eq!(site.page_gets().len(), 0);

    // robots.txt yoksa (404) her şeye izin
    let site = Site::default().html("http://x.com/", r#"<a href="a.zip">a</a>"#);
    assert_eq!(urls(&run(&site, &Rule { respect_robots: true, ..rule("http://x.com/") })), ["http://x.com/a.zip"]);
}

#[test]
fn base_href_used_for_resolution() {
    let site = Site::default()
        .html(
            "http://site.com/docs/index.html",
            r#"<base href="http://site.com/files/"><a href="x.zip">x</a><a href="alt.html">a</a>"#,
        )
        .html("http://site.com/files/alt.html", r#"<a href="y.zip">y</a>"#);
    let found = run(&site, &rule("http://site.com/docs/index.html"));
    assert_eq!(urls(&found), ["http://site.com/files/x.zip", "http://site.com/files/y.zip"]);
}

#[test]
fn cyclic_and_duplicate_links_fetched_once() {
    let site = Site::default()
        .html("http://site.com/", r#"<a href="a.html">a</a><a href="b.html#x">b</a><a href="./a.html?">a2</a>"#)
        .html("http://site.com/a.html", r#"<a href="b.html">b</a><a href="/">kök</a><a href="ortak.zip">z</a>"#)
        .html(
            "http://site.com/b.html",
            r#"<a href="a.html">a</a><a href="http://SITE.com:80/">kök</a><a href="ortak.zip">z</a>"#,
        );
    let found = run(&site, &Rule { same_site_depth: 100, ..rule("http://site.com/") });
    assert_eq!(urls(&found), ["http://site.com/ortak.zip"]);
    assert_eq!(site.page_gets(), ["http://site.com/", "http://site.com/a.html", "http://site.com/b.html"]);
    assert_eq!(site.size_calls.lock().unwrap().len(), 1);
}

#[test]
fn redirect_target_used_as_base_and_deduplicated() {
    let site = Site::default()
        .html("https://yeni.com/ana/", r#"<a href="dosya.zip">d</a><a href="https://yeni.com/ana/alt.html">alt</a>"#)
        .html("https://yeni.com/ana/alt.html", r#"<a href="/">geri</a><a href="z.zip">z</a>"#)
        .redirect("http://eski.com/", "https://yeni.com/ana/");
    let found = run(&site, &rule("http://eski.com/"));
    // Yönlendirilen alan da başlangıç sitesi sayılır
    assert_eq!(urls(&found), ["https://yeni.com/ana/dosya.zip", "https://yeni.com/ana/z.zip"]);
}

/// p0 → p1 → ... zinciri, her sayfada bir zip.
fn numbered(n: usize) -> Site {
    (0..n).fold(Site::default(), |site, i| {
        let body = format!(r#"<a href="p{}.html">n</a><a href="f{i}.zip">f</a>"#, i + 1);
        site.html(&format!("http://site.com/p{i}.html"), &body)
    })
}

#[test]
fn max_pages_limit() {
    let site = numbered(10);
    let found = run(&site, &Rule { max_pages: 3, same_site_depth: 100, ..rule("http://site.com/p0.html") });
    assert_eq!(site.page_gets().len(), 3);
    assert_eq!(found.len(), 3);
}

#[test]
fn stop_flag_honoured() {
    let site = numbered(10);
    let r = Rule { same_site_depth: 100, ..rule("http://site.com/p0.html") };
    let stop = AtomicBool::new(false);
    let last = Mutex::new(None::<Progress>);
    let found = crawl(&site, &r, &stop, &|p: &Progress| {
        *last.lock().unwrap() = Some(p.clone());
        if p.pages >= 2 {
            stop.store(true, Ordering::SeqCst);
        }
    })
    .unwrap();
    // İkinci sayfa bitince durdurulur: üçüncü sayfa istenmez, ikincinin dosyası sonuçtadır
    assert_eq!(site.page_gets().len(), 2);
    assert_eq!(urls(&found), ["http://site.com/f0.zip", "http://site.com/f1.zip"]);
    let p = last.lock().unwrap().clone().unwrap();
    assert_eq!((p.pages, p.found, p.current.as_str()), (2, 2, "http://site.com/p1.html"));

    // Baştan durdurulmuşsa hiç istek yapılmaz
    let site = chain();
    let found = crawl(&site, &rule("http://site.com/"), &AtomicBool::new(true), &|_| {}).unwrap();
    assert!(found.is_empty() && site.gets().is_empty());
}

#[test]
fn srcset_and_other_tags_collected() {
    let site = Site::default().html(
        "http://site.com/galeri/",
        r#"<img src="k.jpg" srcset="k-2x.jpg 2x, /b/k-3x.jpg 3x">
           <picture><source srcset="p.webp 1x, p2.jpg 2x"></picture>
           <video poster="v.jpg" src="v.mp4"></video><script src="app.js"></script>"#,
    );
    let r = Rule { file_patterns: vec!["*.jpg".into()], ..rule("http://site.com/galeri/") };
    assert_eq!(
        urls(&run(&site, &r)),
        [
            "http://site.com/galeri/k.jpg",
            "http://site.com/galeri/k-2x.jpg",
            "http://site.com/b/k-3x.jpg",
            "http://site.com/galeri/p2.jpg",
            "http://site.com/galeri/v.jpg",
        ]
    );
}

#[test]
fn delay_between_requests() {
    let site = chain();
    let r = Rule { same_site_depth: 1, delay_ms: 40, file_patterns: vec!["*.none".into()], ..rule("http://site.com/") };
    let t = std::time::Instant::now();
    run(&site, &r);
    assert_eq!(site.gets().len(), 2);
    assert!(t.elapsed() >= std::time::Duration::from_millis(40));
}

#[test]
fn start_errors() {
    let site = Site::default();
    let stop = AtomicBool::new(false);
    assert!(crawl(&site, &rule("bozuk adres"), &stop, &|_| {}).is_err());
    assert!(crawl(&site, &rule("ftp://site.com/"), &stop, &|_| {}).is_err());
    let err = crawl(&site, &rule("http://yok.com/"), &stop, &|_| {}).unwrap_err();
    assert!(format!("{err:#}").contains("başlangıç sayfası"), "{err:#}");
}
