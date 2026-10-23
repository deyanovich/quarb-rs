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

/// The rules for a tag, by its language, tailored by its region
/// where the CLDR does (`en-GB`, `fr-CA`, `pt-PT`, `de-CH`, `zh-TW`:
/// [`crate::regional`]); `None` for a tag no table covers (the tool
/// then reports it and stays canonical).
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
        // The instant patterns are the CLDR's current medium date
        // and time forms (ca-gregorian dateFormats, timeFormats and
        // dateTimeFormats medium), an ordinary space wherever the
        // CLDR writes a no-break or a narrow no-break space.
        "en" => ("en", '.', Some(","), 1, "%b %-d, %Y, %-I:%M:%S %p"),
        "zh" => ("zh", '.', Some(","), 1, "%Y年%-m月%-d日 %H:%M:%S"),
        "es" => ("es", ',', Some("."), 2, "%-d %b %Y, %-H:%M:%S"),
        "ar" => (
            "ar",
            '.',
            Some(","),
            1,
            "%d\u{200f}/%m\u{200f}/%Y\u{60c} %-I:%M:%S %p",
        ),
        "pt" => ("pt", ',', Some("."), 1, "%-d de %b de %Y %H:%M:%S"),
        "ru" => ("ru", ',', Some(SPACE), 1, "%-d %b %Y г., %H:%M:%S"),
        "ja" => ("ja", '.', Some(","), 1, "%Y/%m/%d %-H:%M:%S"),
        "de" => ("de", ',', Some("."), 1, "%d.%m.%Y, %H:%M:%S"),
        "fr" => ("fr", ',', Some(NARROW), 1, "%-d %b %Y, %H:%M:%S"),
        "tr" => ("tr", ',', Some("."), 1, "%-d %b %Y %H:%M:%S"),
        "it" => ("it", ',', Some("."), 1, "%-d %b %Y, %H:%M:%S"),
        _ => return None,
    };
    // A region the CLDR tailors takes its own medium forms, number
    // symbols and names (spec: The Output Locale), else the language's.
    let (decimal, group, min_grouping, instant, names) = match crate::regional::regional(trimmed) {
        Some(r) => (r.decimal, Some(r.group), r.min_grouping, r.instant, r.names),
        None => (decimal, group, min_grouping, instant, names),
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
        // An instant is a date: the month takes its date form.
        crate::temporal::strftime_form(self.instant, secs, nanos, offset_min, self.names, true)
    }

    /// An instant read in a calendar, under the locale (spec:
    /// Calendars): the CLDR's medium form of that calendar in the
    /// locale's language, its month and era names (`25 Adar I 5784,
    /// 2:30:00 PM`, `令和6年3月5日 14:30:00`); the Julian calendar,
    /// which the CLDR does not name, takes the Gregorian form with
    /// its own fields.
    pub fn instant_in(
        &self,
        secs: i64,
        nanos: u32,
        offset_min: Option<i16>,
        calendar: crate::calendar::Calendar,
    ) -> String {
        if calendar == Default::default() {
            return self.instant(secs, nanos, offset_min);
        }
        match crate::calnames::names(self.tag, calendar) {
            Some(cn) => crate::temporal::strftime_cal(
                cn.medium,
                secs,
                nanos,
                offset_min,
                self.names,
                calendar,
                Some(cn),
            ),
            None => crate::temporal::strftime_cal(
                self.instant,
                secs,
                nanos,
                offset_min,
                self.names,
                calendar,
                None,
            ),
        }
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
                calendar,
            } => self.instant_in(*secs, *nanos, *offset_min, *calendar),
            // a unit-less span: the mixed form in the locale's
            // duration designations (spec: Duration Designations by
            // Locale), each number under the locale's numbers
            Value::Quantity {
                value,
                base,
                written: None,
            } if base == "s" => match crate::durnames::names(self.tag) {
                Some(names) => {
                    crate::value::quantity::duration_text_with(*value, names, &|n| self.number(n))
                }
                None => v.to_string(),
            },
            // magnitude under the locale's numbers, the unit in its
            // designations
            Value::Quantity { .. } => {
                let s = v.to_string();
                match s.split_once('_') {
                    Some((mag, unit)) => format!(
                        "{}_{}",
                        self.number(mag),
                        crate::value::quantity::designated_unit(unit, self.tag)
                    ),
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

    #[test]
    fn a_quantity_prints_in_the_locales_designations() {
        let q = |value: f64, unit: &str| Value::Quantity {
            value,
            base: "x".into(),
            written: Some((value, unit.into())),
        };
        let ru = for_tag("ru-RU").unwrap();
        assert_eq!(ru.scalar(&q(1.5, "kg")), "1,5_кг");
        assert_eq!(ru.scalar(&q(100.0, "km/h")), "100_км/ч");
        assert_eq!(ru.scalar(&q(2.0, "KiB")), "2_Кибайт");
        assert_eq!(ru.scalar(&q(12.0, "EUR/kg")), "12_EUR/кг");
        assert_eq!(ru.scalar(&q(5.0, "кг")), "5_кг");
        // a span with no written unit: the mixed form, each component
        // in the locale's designations
        let span = Value::Quantity {
            value: 6600.0,
            base: "s".into(),
            written: None,
        };
        assert_eq!(ru.scalar(&span), "1 ч 50 мин");
        let half = Value::Quantity {
            value: 1.5,
            base: "s".into(),
            written: None,
        };
        assert_eq!(ru.scalar(&half), "1 с 500 мс");
        let de = for_tag("de").unwrap();
        assert_eq!(de.scalar(&span), "1 Std. 50 Min.");
        assert_eq!(for_tag("zh").unwrap().scalar(&span), "1小时 50分钟");
        assert_eq!(for_tag("en").unwrap().scalar(&span), "1h 50min");
        assert_eq!(de.scalar(&q(1.5, "кг")), "1,5_kg");
        assert_eq!(de.scalar(&q(3.0, "mi")), "3_mi");
    }

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
        assert_eq!(grouped("de-CH").number("1234.5"), "1'234.5");
        assert_eq!(grouped("de-AT").number("1234.5"), "1\u{a0}234,5");
        assert_eq!(grouped("en-ZA").number("1234.5"), "1\u{a0}234,5");
        assert_eq!(grouped("es-MX").number("1234.5"), "1,234.5");
        assert_eq!(grouped("en-GB").number("1234.5"), "1,234.5");
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
        // the CLDR's current medium forms: a comma before the time
        // in fr, an unpadded hour in en, es and ja, the day and
        // month unpadded in zh, the 12-hour clock in en and ar
        let at = |tag: &str| for_tag(tag).unwrap().instant(s, n, o);
        assert_eq!(at("en"), "May 6, 1899, 2:30:00 PM");
        assert_eq!(at("fr"), "6 mai 1899, 14:30:00");
        assert_eq!(at("es"), "6 may 1899, 14:30:00");
        assert_eq!(at("it"), "6 mag 1899, 14:30:00");
        assert_eq!(at("pt"), "6 de mai. de 1899 14:30:00");
        assert_eq!(at("tr"), "6 May 1899 14:30:00");
        assert_eq!(at("ja"), "1899/05/06 14:30:00");
        assert_eq!(at("zh"), "1899年5月6日 14:30:00");
        assert_eq!(at("ar"), "06\u{200f}/05\u{200f}/1899\u{60c} 2:30:00 م");
        let (s9, n9, o9) = crate::temporal::parse_iso("1899-05-06T09:05:00").unwrap();
        assert_eq!(
            for_tag("es").unwrap().instant(s9, n9, o9),
            "6 may 1899, 9:05:00"
        );
        assert_eq!(
            for_tag("ja").unwrap().instant(s9, n9, o9),
            "1899/05/06 9:05:00"
        );
        assert_eq!(
            for_tag("en").unwrap().instant(s9, n9, o9),
            "May 6, 1899, 9:05:00 AM"
        );
        // a region the CLDR tailors: its own medium forms, day
        // periods, names and number symbols
        assert_eq!(at("en-GB"), "6 May 1899, 14:30:00");
        assert_eq!(at("en_GB.UTF-8"), "6 May 1899, 14:30:00");
        assert_eq!(at("en-AU"), "6 May 1899, 2:30:00 pm");
        assert_eq!(at("en-CA"), "May 6, 1899, 2:30:00 p.m.");
        assert_eq!(at("en-IN"), "6 May 1899, 2:30:00 pm");
        assert_eq!(at("en-US"), "May 6, 1899, 2:30:00 PM");
        assert_eq!(at("fr-CA"), "6 mai 1899, 14 h 30 min 00 s");
        assert_eq!(at("fr-CH"), "6 mai 1899, 14:30:00");
        assert_eq!(at("es-MX"), "6 may 1899, 2:30:00 p.m.");
        assert_eq!(at("es-US"), "6 may 1899, 2:30:00 p.m.");
        assert_eq!(at("es-ES"), "6 may 1899, 14:30:00");
        assert_eq!(at("pt-PT"), "06/05/1899, 14:30:00");
        assert_eq!(at("pt-BR"), "6 de mai. de 1899 14:30:00");
        assert_eq!(at("zh-TW"), "1899年5月6日 下午2:30:00");
        assert_eq!(at("zh-HK"), "1899年5月6日 下午2:30:00");
        assert_eq!(at("zh-CN"), "1899年5月6日 14:30:00");
        assert_eq!(at("de-AT"), "06.05.1899, 14:30:00");
        assert_eq!(at("ar-EG"), "06\u{200f}/05\u{200f}/1899\u{60c} 2:30:00 م");
        assert_eq!(at("ar-MA"), "06\u{200f}/05\u{200f}/1899\u{60c} 14:30:00");
        let (sj, nj, oj) = crate::temporal::parse_iso("1899-01-06T14:30:00").unwrap();
        let month = |tag: &str| {
            crate::temporal::strftime_in("%B %b", sj, nj, oj, for_tag(tag).unwrap().names)
        };
        assert_eq!(month("de-AT"), "Jänner Jän");
        // an instant read in a calendar: the calendar's CLDR medium
        // form in the locale's language
        let (sh, nh, oh) = crate::temporal::parse_iso("2024-03-05T14:30:00").unwrap();
        let cal = |tag: &str, c: crate::calendar::Calendar| {
            for_tag(tag).unwrap().instant_in(sh, nh, oh, c)
        };
        use crate::calendar::Calendar;
        assert_eq!(cal("en", Calendar::Hebrew), "25 Adar I 5784, 2:30:00 PM");
        assert_eq!(
            cal("ru", Calendar::Hebrew),
            "25 адар I 5784 г. AM, 14:30:00"
        );
        assert_eq!(cal("ja", Calendar::Japanese), "令和6年3月5日 14:30:00");
        assert_eq!(cal("en", Calendar::Japanese), "Mar 5, 6 Reiwa, 2:30:00 PM");
        assert_eq!(cal("en", Calendar::Islamic), "Sha. 24, 1445 AH, 2:30:00 PM");
        assert_eq!(cal("de", Calendar::Julian), "21.02.2024, 14:30:00");
        assert_eq!(cal("en", Calendar::Gregorian), "Mar 5, 2024, 2:30:00 PM");
        // the CLDR writes the day in hanidays numerals (二十五); the
        // engine keeps the Latin digits throughout
        assert_eq!(cal("zh", Calendar::Chinese), "2024年正月25 14:30:00");
        assert_eq!(month("de"), "Januar Jan.");
        assert_eq!(month("zh-TW"), "1月 1月");
        let v = Value::record(vec![
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
