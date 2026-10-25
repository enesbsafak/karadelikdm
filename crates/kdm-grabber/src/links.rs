//! HTML'den bağlantı toplama. Ağaç kurulmaz: html5ever'in WHATWG uyumlu tokenizer'ı yeter, çünkü yalnız
//! başlangıç etiketlerinin öznitelikleri gerekir. Tokenizer'ın `<script>`, `<style>` gibi ham metin
//! durumlarına geçişini ağaç kurucunun yerine biz söyleriz; yoksa betik içindeki `"<a href=...>"` dizgileri
//! bağlantı sanılırdı.

use crate::urls;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use std::cell::RefCell;
use url::Url;

/// Belgeden çıkan mutlak ve normalleştirilmiş bağlantı.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    /// Küçük harfle etiket adı (`a`, `img`, `meta`...)
    pub tag: String,
    /// Bağlantının geldiği öznitelik (`href`, `src`, `srcset`, `data`, `content`...)
    pub attr: String,
}

/// (etiket adı, (öznitelik, değer) listesi)
type RawTag = (String, Vec<(String, String)>);

/// Yalnız ilgilendiğimiz başlangıç etiketlerini biriktirir.
#[derive(Default)]
struct Sink {
    tags: RefCell<Vec<RawTag>>,
}

const WANTED: &[&str] = &[
    "a", "area", "base", "link", "img", "script", "iframe", "frame", "source", "video", "audio", "embed", "object",
    "meta",
];

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let Token::TagToken(tag) = token else { return TokenSinkResult::Continue };
        if tag.kind != TagKind::StartTag {
            return TokenSinkResult::Continue;
        }
        let name: &str = &tag.name;
        if WANTED.contains(&name) {
            let attrs = tag.attrs.iter().map(|a| (a.name.local.to_string(), a.value.to_string())).collect();
            self.tags.borrow_mut().push((name.to_owned(), attrs));
        }
        // Ağaç kurucunun yaptığı durum geçişleri (HTML 13.2.6.4.4 "in head" ve "in body" kuralları)
        match name {
            "script" => TokenSinkResult::RawData(RawKind::ScriptData),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => TokenSinkResult::RawData(RawKind::Rawtext),
            "textarea" | "title" => TokenSinkResult::RawData(RawKind::Rcdata),
            "plaintext" => TokenSinkResult::Plaintext,
            _ => TokenSinkResult::Continue,
        }
    }
}

/// Bağlantı taşıyan (etiket, öznitelik) çiftleri. `srcset` ayrıca ayrıştırılır.
fn link_attrs(tag: &str) -> &'static [&'static str] {
    match tag {
        "a" | "area" | "link" => &["href"],
        "img" | "source" => &["src", "srcset"],
        "script" | "iframe" | "frame" | "audio" | "embed" => &["src"],
        "video" => &["src", "poster"],
        "object" => &["data"],
        _ => &[],
    }
}

/// HTML'deki bağlantılar, belge sırasıyla. `base` sayfanın (yönlendirme sonrası) adresidir; belgede
/// `<base href>` varsa ilki geçerlidir. Parça atılır; yalnız http, https ve ftp adresleri döner.
pub fn extract_links(html: &str, base: &str) -> Vec<Link> {
    let tok = Tokenizer::new(Sink::default(), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    let _ = tok.feed(&input);
    tok.end();
    let tags = tok.sink.tags.take();

    let attr = |attrs: &[(String, String)], name: &str| attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let doc_url = Url::parse(base).ok();
    // HTML: belge taban adresi, href'i olan ilk <base>'dir ve konumundan bağımsız tüm belgeye uygulanır
    let base_url = tags
        .iter()
        .filter(|(t, _)| t == "base")
        .find_map(|(_, a)| attr(a, "href"))
        .and_then(|h| resolve(doc_url.as_ref(), &h))
        .or(doc_url);

    let mut out = Vec::new();
    let mut push = |raw: &str, tag: &str, at: &str| {
        let raw = raw.trim_matches(|c: char| c.is_ascii_whitespace());
        if raw.is_empty() {
            return;
        }
        if let Some(url) = resolve(base_url.as_ref(), raw).and_then(urls::normalize) {
            out.push(Link { url, tag: tag.to_owned(), attr: at.to_owned() });
        }
    };
    for (tag, attrs) in &tags {
        if tag == "meta" {
            let refresh = attr(attrs, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"));
            if let Some(u) = attr(attrs, "content").filter(|_| refresh).as_deref().and_then(refresh_url) {
                push(u, tag, "content");
            }
            continue;
        }
        for &name in link_attrs(tag) {
            let Some(v) = attr(attrs, name) else { continue };
            if name == "srcset" {
                for u in parse_srcset(&v) {
                    push(u, tag, name);
                }
            } else {
                push(&v, tag, name);
            }
        }
    }
    out
}

fn resolve(base: Option<&Url>, raw: &str) -> Option<Url> {
    // `javascript:`, `mailto:`, `data:` gibi şemalar normalize'da elenir; burada yalnız çözülür
    match base {
        Some(b) => b.join(raw).ok(),
        None => Url::parse(raw).ok(),
    }
}

/// `srcset` adayı adresleri (HTML "parse a srcset attribute"). Adres boşluğa kadar sürer; sonundaki virgüller
/// ayırıcıdır. Tanımlayıcılar (`2x`, `640w`) parantez içinde virgül taşıyabilir.
pub(crate) fn parse_srcset(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    let ws = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r');
    loop {
        while i < b.len() && (ws(b[i]) || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            return out;
        }
        let start = i;
        while i < b.len() && !ws(b[i]) {
            i += 1;
        }
        let mut url = &s[start..i];
        if url.ends_with(',') {
            // Adres virgülle bitiyorsa tanımlayıcı yoktur, aday burada biter
            url = url.trim_end_matches(',');
        } else {
            let mut depth = 0u32;
            while i < b.len() {
                match b[i] {
                    b'(' => depth += 1,
                    b')' => depth = depth.saturating_sub(1),
                    b',' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
        }
        if !url.is_empty() {
            out.push(url);
        }
    }
}

/// `<meta http-equiv=refresh content="5; url=...">` içindeki adres (HTML "shared declarative refresh steps").
pub(crate) fn refresh_url(content: &str) -> Option<&str> {
    let s = content.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let s = s.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let s = s.strip_prefix([';', ',']).unwrap_or(s).trim_start_matches(|c: char| c.is_ascii_whitespace());
    // "url=" isteğe bağlı; "url" ardından "=" gelmiyorsa adresin parçasıdır
    let s = match s.get(..3) {
        Some(p) if p.eq_ignore_ascii_case("url") => {
            let rest = s[3..].trim_start_matches(|c: char| c.is_ascii_whitespace());
            rest.strip_prefix('=').map(|r| r.trim_start_matches(|c: char| c.is_ascii_whitespace())).unwrap_or(s)
        }
        _ => s,
    };
    let s = match s.chars().next() {
        Some(q @ ('"' | '\'')) => s[1..].split(q).next().unwrap_or(""),
        _ => s,
    };
    let s = s.trim_end_matches(|c: char| c.is_ascii_whitespace());
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(html: &str, base: &str) -> Vec<String> {
        extract_links(html, base).into_iter().map(|l| l.url).collect()
    }

    #[test]
    fn collects_from_all_tags() {
        let html = r##"<html><head>
            <link rel=stylesheet href="s.css"><script src="/j.js"></script>
            <meta http-equiv="Refresh" content="3; URL='yeni.html'">
            </head><body>
            <a href="a.html#x">A</a><area href="b.html"><img src="i.png" srcset="i2.png 2x, i3.png 3x">
            <iframe src="f.html"></iframe><frame src="fr.html"><picture><source srcset="p.webp"></picture>
            <video src="v.mp4" poster="v.jpg"></video><audio src="s.mp3"></audio><embed src="e.swf">
            <object data="o.pdf"></object>
            <a href="javascript:void(0)">x</a><a href="mailto:a@b.c">m</a><img src="data:image/png;base64,AAAA">
            <a href="  ">boş</a><a href="#">üst</a>
            </body></html>"##;
        let got = extract_links(html, "http://site.com/d/index.html");
        let pairs: Vec<_> = got.iter().map(|l| (l.tag.as_str(), l.attr.as_str(), l.url.as_str())).collect();
        assert_eq!(
            pairs,
            [
                ("link", "href", "http://site.com/d/s.css"),
                ("script", "src", "http://site.com/j.js"),
                ("meta", "content", "http://site.com/d/yeni.html"),
                ("a", "href", "http://site.com/d/a.html"),
                ("area", "href", "http://site.com/d/b.html"),
                ("img", "src", "http://site.com/d/i.png"),
                ("img", "srcset", "http://site.com/d/i2.png"),
                ("img", "srcset", "http://site.com/d/i3.png"),
                ("iframe", "src", "http://site.com/d/f.html"),
                ("frame", "src", "http://site.com/d/fr.html"),
                ("source", "srcset", "http://site.com/d/p.webp"),
                ("video", "src", "http://site.com/d/v.mp4"),
                ("video", "poster", "http://site.com/d/v.jpg"),
                ("audio", "src", "http://site.com/d/s.mp3"),
                ("embed", "src", "http://site.com/d/e.swf"),
                ("object", "data", "http://site.com/d/o.pdf"),
                ("a", "href", "http://site.com/d/index.html"),
            ]
        );
    }

    #[test]
    fn base_href_applies_to_whole_document() {
        let html = r#"<a href="once.zip">x</a><base href="/files/"><base href="http://other/"><img src="sonra.png">"#;
        let want = ["http://site.com/files/once.zip", "http://site.com/files/sonra.png"];
        assert_eq!(urls(html, "http://site.com/a/b.html"), want);
        // base href yoksa sayfa adresi
        assert_eq!(urls(r#"<a href="x.zip">"#, "http://site.com/a/b.html"), ["http://site.com/a/x.zip"]);
        // göreli base sayfa adresine göre çözülür; geçersizse yok sayılır
        let rel = r#"<base href="../up/"><a href="x.zip">"#;
        assert_eq!(urls(rel, "http://site.com/a/b/c.html"), ["http://site.com/a/up/x.zip"]);
        let bad = r#"<base href="http://[bad"><a href="x.zip">"#;
        assert_eq!(urls(bad, "http://site.com/a/c.html"), ["http://site.com/a/x.zip"]);
    }

    #[test]
    fn ignores_markup_inside_script_style_and_comments() {
        let html = r#"<script>var s = "<a href='js.zip'>";</script><style>a{background:url(<a href=css.zip>)}</style>
            <!-- <a href="yorum.zip"> --><textarea><a href="ta.zip"></textarea><title><a href=t.zip></title>
            <a href="gercek.zip">"#;
        assert_eq!(urls(html, "http://s.com/"), ["http://s.com/gercek.zip"]);
    }

    #[test]
    fn decodes_entities_in_attributes() {
        assert_eq!(urls(r#"<a href="get?a=1&amp;b=2">"#, "http://s.com/"), ["http://s.com/get?a=1&b=2"]);
    }

    #[test]
    fn srcset_parsing() {
        assert_eq!(parse_srcset("a.png 1x, b.png 2x"), ["a.png", "b.png"]);
        // Adres boşluğa kadar sürer: boşluksuz virgül adresin parçasıdır (tarayıcılar da böyle yapar)
        assert_eq!(parse_srcset("  a.png,b.png,  c.png 100w  "), ["a.png,b.png", "c.png"]);
        assert_eq!(parse_srcset("a.png, b.png,, c.png 100w,"), ["a.png", "b.png", "c.png"]);
        assert_eq!(parse_srcset("a,b.png 2x"), ["a,b.png"]);
        assert_eq!(parse_srcset("x.png foo(1, 2), y.png"), ["x.png", "y.png"]);
        assert_eq!(parse_srcset("data:image/png;base64,QUJD 1x, z.png 2x"), ["data:image/png;base64,QUJD", "z.png"]);
        assert_eq!(parse_srcset(" , ,"), Vec::<&str>::new());
        assert_eq!(parse_srcset("tek.jpg"), ["tek.jpg"]);
    }

    #[test]
    fn refresh_content_forms() {
        assert_eq!(refresh_url("0; url=a.html"), Some("a.html"));
        assert_eq!(refresh_url("5;URL = 'b c.html' "), Some("b c.html"));
        assert_eq!(refresh_url("1, \"q.html\""), Some("q.html"));
        assert_eq!(refresh_url("2; urlx.html"), Some("urlx.html"));
        assert_eq!(refresh_url("3"), None);
    }
}
