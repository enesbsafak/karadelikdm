//! Arayüz metinleri. Aynı JSON dosyaları hem Rust (tepsi menüsü, bildirimler) hem web arayüzünde kullanılır.

use once_cell::sync::Lazy;
use std::collections::HashMap;

static TR: Lazy<HashMap<String, String>> =
    Lazy::new(|| serde_json::from_str(include_str!("../../ui/i18n/tr.json")).unwrap_or_default());
static EN: Lazy<HashMap<String, String>> =
    Lazy::new(|| serde_json::from_str(include_str!("../../ui/i18n/en.json")).unwrap_or_default());

pub const LANGUAGES: &[(&str, &str)] = &[("tr", "Türkçe"), ("en", "English")];

pub fn t(key: &str) -> String {
    let lang = crate::kcore::settings().lang();
    let primary = if lang == "tr" { &*TR } else { &*EN };
    primary.get(key).or_else(|| EN.get(key)).cloned().unwrap_or_else(|| key.to_string())
}

/// `{0}`, `{1}` yer tutucularını doldurur.
pub fn tf(key: &str, args: &[&str]) -> String {
    let mut s = t(key);
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

pub fn table(lang: &str) -> HashMap<String, String> {
    let mut m = EN.clone();
    if lang == "tr" {
        m.extend(TR.clone());
    }
    m
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_languages_have_same_keys() {
        let mut missing: Vec<String> = super::EN.keys().filter(|k| !super::TR.contains_key(*k)).cloned().collect();
        missing.extend(super::TR.keys().filter(|k| !super::EN.contains_key(*k)).map(|k| format!("en: {k}")));
        missing.sort();
        assert!(missing.is_empty(), "eksik çeviriler: {missing:?}");
    }
}
