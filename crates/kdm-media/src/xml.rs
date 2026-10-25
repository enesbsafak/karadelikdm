//! MPD için küçük bir XML ağacı. MPD birkaç yüz KB'ı geçmez; ağaç üzerinde kalıtım (Period → AdaptationSet →
//! Representation) çözmek akış ayrıştırmasından çok daha basit.
//!
//! Ad alanı önekleri atılır (`cenc:pssh` → `pssh`): MPD öğeleri tek ad alanındadır, önek seçimi üreticiye kalır.

use anyhow::{anyhow, bail, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

#[derive(Clone, Debug, Default)]
pub(crate) struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn element(e: &BytesStart) -> Result<Element> {
    let mut el = Element { name: local(e.name().as_ref()).to_string(), ..Default::default() };
    for a in e.attributes() {
        let a = a.map_err(|e| anyhow!("MPD öznitelik hatası: {e}"))?;
        let v = a.normalized_value(XmlVersion::Implicit1_0).map_err(|e| anyhow!("MPD öznitelik hatası: {e}"))?;
        el.attrs.push((local(a.key.as_ref()).to_string(), v.into_owned()));
    }
    Ok(el)
}

fn attach(stack: &mut [Element], root: &mut Option<Element>, el: Element) {
    if let Some(p) = stack.last_mut() {
        p.children.push(el);
    } else if root.is_none() {
        *root = Some(el);
    }
}

pub(crate) fn parse(s: &str) -> Result<Element> {
    let mut reader = Reader::from_str(s.trim_start_matches('\u{feff}'));
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    loop {
        let ev = reader.read_event().map_err(|e| anyhow!("MPD XML hatası ({}): {e}", reader.error_position()))?;
        match ev {
            Event::Start(e) => stack.push(element(&e)?),
            Event::Empty(e) => {
                let el = element(&e)?;
                attach(&mut stack, &mut root, el);
            }
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| anyhow!("MPD XML hatası: eşleşmeyen kapanış etiketi"))?;
                attach(&mut stack, &mut root, el);
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml_content(XmlVersion::Implicit1_0));
                }
            }
            // `&amp;` gibi başvurular metinden ayrı olay olarak gelir (ör. BaseURL'deki sorgu dizgisi)
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    let c = match r.resolve_char_ref().ok().flatten() {
                        Some(c) => c,
                        None => match &*r {
                            "amp" => '&',
                            "lt" => '<',
                            "gt" => '>',
                            "quot" => '"',
                            "apos" => '\'',
                            other => bail!("MPD XML hatası: tanınmayan varlık &{other};"),
                        },
                    };
                    top.text.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.ok_or_else(|| anyhow!("MPD XML hatası: kök öğe yok"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_entities_and_prefixes() {
        let x = r#"<?xml version="1.0"?><MPD xmlns:cenc="urn:x"><BaseURL>http://a/b?x=1&amp;y=2</BaseURL>
            <Period id="p&amp;1"><cenc:pssh>abc</cenc:pssh><AdaptationSet/></Period></MPD>"#;
        let r = parse(x).unwrap();
        assert_eq!(r.name, "MPD");
        assert_eq!(r.child("BaseURL").unwrap().text, "http://a/b?x=1&y=2");
        let p = r.child("Period").unwrap();
        assert_eq!(p.attr("id"), Some("p&1"));
        assert_eq!(p.child("pssh").unwrap().text, "abc");
        assert_eq!(p.children_named("AdaptationSet").count(), 1);
    }

    #[test]
    fn rejects_broken_xml() {
        assert!(parse("<MPD><Period></MPD>").is_err());
    }
}
