//! Transliteration and historical spelling: thin wrappers over the
//! `transliteratio` crate, which holds every table (the ISO tables
//! per script, the national and library systems, the inverses, the
//! Latin-to-ASCII folds, the spelling tables). The engine enables
//! every table the crate has; the crate's typed errors become the
//! refusal texts here.

/// The readings library the lexicon tables need: Chinese characters
/// under pinyin and wade, kanji under hepburn and iso3602, Thai
/// under rtgs. Until it is mounted those texts are refused naming it.
pub const READINGS_LIBRARY: &str = "anagnosis";

/// Every table name the crate answers to.
pub fn schemes() -> Vec<&'static str> {
    transliteratio::TABLES.iter().map(|t| t.code).collect()
}

/// Transliterate `text` under `scheme` (`""` or `"iso"` for the
/// script's own ISO table). An unknown table, a table left out of
/// the build, or a lexicon table over text that needs the lexicon
/// is an error in the engine's refusal words.
pub fn translit(text: &str, scheme: &str) -> Result<String, String> {
    transliteratio::transliterate(text, scheme).map_err(|e| match e {
        transliteratio::Error::UnknownTable(t) => format!(
            "translit: unknown table '{t}' (one of {})",
            schemes().join(", ")
        ),
        transliteratio::Error::NeedsLexicon { table, text } => format!(
            "translit: {table} over {text} needs the readings library ({READINGS_LIBRARY}), \
             which is not mounted"
        ),
        transliteratio::Error::FeatureOff { table, feature } => {
            format!("translit: table '{table}' is not in this build (feature '{feature}')")
        }
        other => format!("translit: {other}"),
    })
}

/// A historical spelling folded to the modern one: `ru-1918` (the
/// pre-reform Russian alphabet, the final ъ dropped) and `long-s`
/// (the long s and the typographic ligatures).
pub fn modernize(text: &str, table: &str) -> Result<String, String> {
    transliteratio::modernize(text, table).map_err(|e| match e {
        transliteratio::Error::UnknownSpelling(t) => format!(
            "modernize: unknown spelling table '{t}' ({})",
            transliteratio::SPELLINGS.join(", ")
        ),
        other => format!("modernize: {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{modernize, translit};

    #[test]
    fn the_wrapper_speaks_the_engines_refusals() {
        assert_eq!(translit("Щука", "iso9").unwrap(), "Ŝuka");
        assert_eq!(translit("Щука", "").unwrap(), "Ŝuka");
        assert_eq!(translit("Љубљана", "sr-latn").unwrap(), "Ljubljana");
        let e = translit("x", "klingon").unwrap_err();
        assert!(
            e.starts_with("translit: unknown table 'klingon' (one of iso, iso9,"),
            "{e}"
        );
        let e = translit("東京", "hepburn").unwrap_err();
        assert!(e.contains("kanji") && e.contains("anagnosis"), "{e}");
        assert_eq!(modernize("міръ", "ru-1918").unwrap(), "мир");
        assert!(modernize("x", "klingon").unwrap_err().contains("ru-1918"));
    }
}
