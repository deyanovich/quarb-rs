//! The output locale (spec: The Output Locale): how a result is
//! *rendered*, never what it is. A tool decides the locale in
//! force by its layers (`--locale`, `QUARB_LOCALE`, its config,
//! the system's, else canonical) and renders what it prints
//! through [`OutputLocale`]: the decimal mark and grouping of
//! numbers, the printed form of instants, `@| csv`'s default
//! delimiter, `sort`'s default collation. A function that reads
//! text takes its locale from its argument alone, so a value
//! never depends on where the query ran; the interchange forms
//! (`@| json`, `@| jsonl`, `@| kaiv`) stay canonical. Locale `C`
//! (or `POSIX`, or none) is the canonical form: the decimal
//! point, no grouping, ISO-8601 instants, the comma, code-point
//! order — today's behaviour.

use crate::datenames::{self, Names};
use crate::value::Value;

/// A locale's rendering rules.
#[derive(Clone, Copy, Debug)]
pub struct OutputLocale {
    /// The BCP 47 tag as given (`ru-RU`).
    pub tag: &'static str,
    pub decimal: char,
    /// The grouping separator the locale writes, used only when
    /// `grouping` is asked for.
    pub group: Option<&'static str>,
    /// Whether digits are grouped. Off unless the tool is asked
    /// (`--grouping`): a year is not `1,876` and an id not
    /// `100,234`, and the separators stay out of legacy encodings.
    pub grouping: bool,
    /// CLDR's minimum grouping digits: the integer part groups
    /// only when it has this many digits beyond the first group
    /// (`1` groups 1234; `2` leaves 1234 alone and groups 12345).
    pub min_grouping: usize,
    /// `@| csv`'s delimiter under this locale (a semicolon where
    /// the decimal mark is the comma).
    pub csv_delimiter: u8,
    /// An instant's printed form, as a `strftime` format under the
    /// locale's names — CLDR's medium date and time patterns.
    pub instant: &'static str,
    pub names: &'static Names,
}

const SPACE: &str = "\u{a0}";
const NARROW: &str = "\u{202f}";

/// The rules for a tag, by its language; `None` for a tag no
/// table covers (the tool then reports it and stays canonical).
/// `C`, `POSIX` and the empty tag are canonical and answer `None`
/// too — the caller treats "no locale" and "canonical" alike.
/// The locales shipped with tables — rendering rules, month and
/// weekday names, time expressions — by size of language.
pub const SHIPPED: &[&str] = &[
    "en", "zh", "es", "ar", "pt", "ru", "ja", "de", "fr", "tr", "it",
];

/// The language of a BCP 47 tag (or a POSIX locale name): `ru-RU`,
/// `ru_RU.UTF-8`, `ru@petr1708` all answer `ru`, lower-cased. The
/// one tag resolution every table shares; an empty tag is the
/// empty language, which the tables read as English.
pub use quarb_time::language;

pub fn for_tag(tag: &str) -> Option<OutputLocale> {
    let trimmed = tag.trim();
    let lang = language(tag);
    let names = datenames::names(&lang)?;
    let (tag, decimal, group, min_grouping, instant): (
        &'static str,
        char,
        Option<&'static str>,
        usize,
        &'static str,
    ) = match lang.as_str() {
        "en" => ("en", '.', Some(","), 1, "%b %e, %Y, %I:%M:%S %p"),
        "zh" => ("zh", '.', Some(","), 1, "%Y年%m月%d日 %H:%M:%S"),
        "es" => ("es", ',', Some("."), 2, "%e %b %Y, %H:%M:%S"),
        "ar" => (
            "ar",
            '.',
            Some(","),
            1,
            "%d\u{200f}/%m\u{200f}/%Y, %H:%M:%S",
        ),
        "pt" => ("pt", ',', Some("."), 1, "%e de %b de %Y %H:%M:%S"),
        "ru" => ("ru", ',', Some(SPACE), 1, "%e %b %Y г., %H:%M:%S"),
        "ja" => ("ja", '.', Some(","), 1, "%Y/%m/%d %H:%M:%S"),
        "de" => ("de", ',', Some("."), 1, "%d.%m.%Y, %H:%M:%S"),
        "fr" => ("fr", ',', Some(NARROW), 1, "%e %b %Y %H:%M:%S"),
        "tr" => ("tr", ',', Some("."), 1, "%e %b %Y %H:%M:%S"),
        "it" => ("it", ',', Some("."), 1, "%e %b %Y, %H:%M:%S"),
        _ => return None,
    };
    // The regions that write numbers unlike their language do: one
    // table with `number(LOCALE)`, so what is rendered under a
    // locale reads back under it.
    let (decimal, group) = match crate::number::locale_convention(trimmed) {
        Some(crate::number::Decimal::Point) if decimal == ',' => (
            '.',
            if trimmed
                .to_ascii_lowercase()
                .replace('_', "-")
                .contains("-ch")
            {
                Some("\u{2019}")
            } else {
                Some(",")
            },
        ),
        Some(crate::number::Decimal::Comma) if decimal == '.' => (',', Some(SPACE)),
        _ => (decimal, group),
    };
    Some(OutputLocale {
        tag,
        decimal,
        group,
        grouping: false,
        min_grouping,
        csv_delimiter: if decimal == ',' { b';' } else { b',' },
        instant,
        names,
    })
}

/// Whether a tag names the canonical form (`C`, `POSIX`, empty).
pub fn is_canonical(tag: &str) -> bool {
    let t = tag.trim();
    t.is_empty() || t == "C" || t == "POSIX" || t.starts_with("C.") || t.starts_with("POSIX.")
}

impl OutputLocale {
    /// A number's digits under the locale: the canonical text
    /// (`-1234.5`, `1e21`) regrouped and remarked. An exponent form
    /// is left as it is.
    pub fn number(&self, canonical: &str) -> String {
        if canonical.contains(['e', 'E', 'N', 'i']) {
            return canonical.to_string();
        }
        let (sign, rest) = match canonical.strip_prefix('-') {
            Some(r) => ("-", r),
            None => ("", canonical),
        };
        let (int, frac) = match rest.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (rest, None),
        };
        let mut out = String::from(sign);
        match self.group {
            Some(sep) if self.grouping && int.len() > 3 + (self.min_grouping - 1) => {
                let first = int.len() % 3;
                if first > 0 {
                    out.push_str(&int[..first]);
                }
                for (i, chunk) in int[first..].as_bytes().chunks(3).enumerate() {
                    if first > 0 || i > 0 {
                        out.push_str(sep);
                    }
                    out.push_str(std::str::from_utf8(chunk).unwrap());
                }
            }
            _ => out.push_str(int),
        }
        if let Some(f) = frac {
            out.push(self.decimal);
            out.push_str(f);
        }
        out
    }

    /// An instant under the locale's medium date and time pattern,
    /// in the instant's own offset.
    pub fn instant(&self, secs: i64, nanos: u32, offset_min: Option<i16>) -> String {
        // CLDR's `d` is the day without padding; strftime's `%e`
        // pads with a space, so the day is spelled in first.
        let day = crate::temporal::strftime_in("%e", secs, nanos, offset_min, self.names);
        let fmt = self.instant.replace("%e", day.trim());
        // An instant is a date: the month takes its date form.
        crate::temporal::strftime_form(&fmt, secs, nanos, offset_min, self.names, true)
    }

    /// A scalar rendered under the locale: numbers remarked and
    /// grouped, instants in the locale's form, a quantity's
    /// magnitude likewise; text, booleans, durations as they are.
    pub fn scalar(&self, v: &Value) -> String {
        match v {
            Value::Int(_) | Value::Float(_) => self.number(&v.to_string()),
            Value::Instant {
                secs,
                nanos,
                offset_min,
            } => self.instant(*secs, *nanos, *offset_min),
            Value::Quantity { .. } => {
                let s = v.to_string();
                match s.split_once(' ') {
                    Some((mag, unit)) => format!("{} {unit}", self.number(mag)),
                    None => s,
                }
            }
            other => other.to_string(),
        }
    }

    /// The Quarb form under the locale: as [`Value::to_quarb`],
    /// scalars through [`Self::scalar`]. Text keeps its quoting.
    pub fn quarb(&self, v: &Value) -> String {
        match v {
            Value::List(l) => {
                let inner: Vec<String> = l.iter().map(|x| self.quarb(x)).collect();
                format!("@({})", inner.join("; "))
            }
            Value::Record(o) => {
                let inner: Vec<String> = o
                    .iter()
                    .map(|(k, x)| format!("{} = {}", crate::value::quarb_key(k), self.quarb(x)))
                    .collect();
                format!("%({})", inner.join("; "))
            }
            Value::Null | Value::Str(_) | Value::Bool(_) => v.to_quarb(),
            other => self.scalar(other),
        }
    }

    /// What a result line shows under the locale: a scalar bare, a
    /// record or list in its Quarb form (as [`Value::display_form`]).
    pub fn display(&self, v: &Value) -> String {
        match v {
            Value::List(_) | Value::Record(_) => self.quarb(v),
            Value::Str(s) => s.clone(),
            other => self.scalar(other),
        }
    }

    /// A table or CSV cell under the locale (as `tabulate` fills
    /// one): null empty, text bare, the rest displayed.
    pub fn cell(&self, v: &Value) -> String {
        match v {
            Value::Null => String::new(),
            Value::Str(s) => s.clone(),
            other => self.display(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grouped(tag: &str) -> OutputLocale {
        OutputLocale {
            grouping: true,
            ..for_tag(tag).unwrap()
        }
    }

    #[test]
    fn numbers_by_locale() {
        // the decimal mark by default; grouping only when asked
        let ru = for_tag("ru-RU").unwrap();
        assert_eq!(ru.number("1234567.5"), "1234567,5");
        assert_eq!(ru.number("1876"), "1876");
        assert_eq!(
            grouped("ru-RU").number("1234567.5"),
            "1\u{a0}234\u{a0}567,5"
        );
        assert_eq!(grouped("ru-RU").number("-42"), "-42");
        assert_eq!(grouped("es").number("1234"), "1234");
        assert_eq!(grouped("es").number("12345.5"), "12.345,5");
        assert_eq!(grouped("en-US").number("1234567.25"), "1,234,567.25");
        assert_eq!(grouped("en-US").number("1e21"), "1e21");
        assert_eq!(grouped("de-CH").number("1234.5"), "1\u{2019}234.5");
        assert_eq!(for_tag("de-CH").unwrap().number("1234.5"), "1234.5");
        assert!(for_tag("C").is_none() && for_tag("xx").is_none());
        assert!(is_canonical("C.UTF-8") && is_canonical("") && !is_canonical("ru_RU.UTF-8"));
        assert!(for_tag("ru_RU.UTF-8").is_some());
    }

    #[test]
    fn values_by_locale() {
        let ru = for_tag("ru").unwrap();
        let (s, n, o) = crate::temporal::parse_iso("1899-05-06T14:30:00").unwrap();
        // a date takes the month's date form
        assert_eq!(ru.instant(s, n, o), "6 мая 1899 г., 14:30:00");
        let de = for_tag("de").unwrap();
        assert_eq!(de.instant(s, n, o), "06.05.1899, 14:30:00");
        let v = Value::Record(vec![
            ("n".into(), Value::Float(1234.5)),
            ("t".into(), Value::Str("x".into())),
        ]);
        assert_eq!(ru.quarb(&v), "%(n = 1234,5; t = \"x\")");
        assert_eq!(ru.display(&Value::Int(1000000)), "1000000");
        assert_eq!(ru.cell(&Value::Null), "");
        assert_eq!(ru.csv_delimiter, b';');
        assert_eq!(for_tag("en").unwrap().csv_delimiter, b',');
    }
}
