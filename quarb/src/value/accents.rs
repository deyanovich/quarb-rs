//! Accents and marks, thin wrappers over the `transliteratio`
//! crate: `deaccent(LANG)` removes the marks a language counts as
//! accents and keeps the ones that make letters; `strip` removes
//! every nonspacing mark.

/// `text` without the accents of `lang` (a language tag; `ru-RU`
/// is `ru`); a language without a table is an error naming it and
/// listing the languages.
pub fn deaccent(text: &str, lang: &str) -> Result<String, String> {
    transliteratio::deaccent(text, lang).map_err(|e| match e {
        transliteratio::Error::UnknownLanguage(l) => format!(
            "deaccent: no accent table for language '{l}' (one of {})",
            transliteratio::LANGUAGES.join(", ")
        ),
        other => format!("deaccent: {other}"),
    })
}

/// `text` without any nonspacing mark.
pub fn strip(text: &str) -> String {
    transliteratio::strip(text)
}

#[cfg(test)]
mod tests {
    use super::{deaccent, strip};

    #[test]
    fn accents_by_language_and_every_mark() {
        assert_eq!(deaccent("ёлка подъё\u{301}м", "ru").unwrap(), "ёлка подъём");
        assert_eq!(
            deaccent("élève œuvre garçon", "fr").unwrap(),
            "eleve œuvre garcon"
        );
        assert!(deaccent("x", "klingon").unwrap_err().contains("klingon"));
        assert_eq!(
            strip("Ἀχιλλεύς élève Łódź ёлка"),
            "Αχιλλευς eleve Łodz елка"
        );
    }
}
