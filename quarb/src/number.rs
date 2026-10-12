//! The numeric reading of text (spec: The Numeric Fragment, and
//! chapter 7's "Delimited Text"): the canonical form — digits, an
//! optional sign, `.` as the decimal point, an optional exponent,
//! no grouping — and, under a declared convention, grouped digits.
//! Under `point` (the default) `,` groups thousands; under `comma`
//! `,` is the decimal point and `.` groups; under either a space,
//! U+00A0, U+202F and the apostrophe group. A group is exactly
//! three digits, so `12,50` under `point` has no reading (an
//! aggregate omits it rather than counting it wrong) and `1,234`
//! reads 1234.

use crate::value::Value;

/// Which character is the decimal point: the source's declaration
/// (`?decimal=`), `point` when it says nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Decimal {
    #[default]
    Point,
    Comma,
}

impl Decimal {
    pub fn from_label(label: &str) -> Option<Decimal> {
        match label.trim().to_ascii_lowercase().as_str() {
            "point" | "." => Some(Decimal::Point),
            "comma" | "," => Some(Decimal::Comma),
            _ => None,
        }
    }
}

/// Read `text` as a number under the convention: an exact `Int`
/// when the text has integer form, a finite `Float` otherwise,
/// `None` when it has no reading.
pub fn read(text: &str, decimal: Decimal) -> Option<Value> {
    // The three floats Python spells by name read back by it.
    match text.trim() {
        "nan" => return Some(Value::Float(f64::NAN)),
        "inf" => return Some(Value::Float(f64::INFINITY)),
        "-inf" => return Some(Value::Float(f64::NEG_INFINITY)),
        _ => {}
    }
    let canonical = canonical(text.trim(), decimal)?;
    if let Ok(n) = canonical.parse::<i64>() {
        return Some(Value::Int(n));
    }
    canonical
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite())
        .map(Value::Float)
}

/// Rewrite `text` into the canonical form the parsers take, or
/// `None` when it is not a number under the convention. The
/// grammar: `[+-] digits-with-groups [dec fraction] [e [+-] digits]`
/// — groups only in the integer part, each exactly three digits
/// after the first group of one to three.
pub fn canonical(text: &str, decimal: Decimal) -> Option<String> {
    let (dec, group) = match decimal {
        Decimal::Point => ('.', ','),
        Decimal::Comma => (',', '.'),
    };
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    if let Some(&c) = chars.peek()
        && (c == '+' || c == '-')
    {
        out.push(c);
        chars.next();
    }
    // The integer part, with its groups.
    let mut int_digits = String::new();
    let mut groups: Vec<usize> = vec![0];
    let mut grouped = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            int_digits.push(c);
            *groups.last_mut().unwrap() += 1;
            chars.next();
        } else if c == group
            || c == ' '
            || c == '\u{A0}'
            || c == '\u{202F}'
            || c == '\''
            || c == '\u{2019}'
        {
            if *groups.last().unwrap() == 0 {
                return None;
            }
            grouped = true;
            groups.push(0);
            chars.next();
        } else {
            break;
        }
    }
    if int_digits.is_empty() && !matches!(chars.peek(), Some(c) if *c == dec) {
        return None;
    }
    if grouped {
        // The first group one to three digits, every later group
        // exactly three; a separator with nothing after it is not
        // a group.
        let (first, rest) = groups.split_first()?;
        if *first == 0 || *first > 3 || rest.iter().any(|g| *g != 3) {
            return None;
        }
    }
    out.push_str(&int_digits);
    // The fraction.
    if let Some(&c) = chars.peek()
        && c == dec
    {
        chars.next();
        let mut frac = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                frac.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if frac.is_empty() && int_digits.is_empty() {
            return None;
        }
        if int_digits.is_empty() {
            out.push('0');
        }
        out.push('.');
        out.push_str(&frac);
    }
    // The exponent.
    if let Some(&c) = chars.peek()
        && (c == 'e' || c == 'E')
    {
        chars.next();
        out.push('e');
        if let Some(&s) = chars.peek()
            && (s == '+' || s == '-')
        {
            out.push(s);
            chars.next();
        }
        let mut exp = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                exp.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if exp.is_empty() {
            return None;
        }
        out.push_str(&exp);
    }
    if chars.next().is_some() {
        return None;
    }
    Some(out)
}

/// `number(LOCALE)`: the convention a locale writes numbers in,
/// from a small table of CLDR's decimal and group symbols — the
/// locale's language decides, with the regions that differ from
/// their language's rule listed by themselves. Unknown locales
/// have no table and the reading is `None`.
pub fn locale_convention(locale: &str) -> Option<Decimal> {
    let locale = locale.trim().replace('_', "-");
    let lower = locale.to_ascii_lowercase();
    // Regions that break their language's rule.
    match lower.as_str() {
        "de-ch" | "de-li" | "fr-ch" | "it-ch" | "en-za" | "es-mx" | "es-us" | "pt-mz" | "fr-ca" => {
            return Some(match lower.as_str() {
                "de-ch" | "de-li" | "fr-ch" | "it-ch" | "es-mx" | "es-us" => Decimal::Point,
                _ => Decimal::Comma,
            });
        }
        _ => {}
    }
    let lang = lower.split('-').next().unwrap_or("");
    Some(match lang {
        "en" | "ja" | "zh" | "ko" | "th" | "hi" | "he" | "ms" | "fil" | "sw" | "ga" | "cy"
        | "mt" | "ur" | "ta" | "te" | "kn" | "ml" | "bn" | "gu" | "mr" | "pa" | "my" | "km"
        | "lo" | "si" | "ne" | "ar" | "fa" => Decimal::Point,
        "ru" | "uk" | "be" | "bg" | "sr" | "hr" | "bs" | "sl" | "mk" | "cs" | "sk" | "pl"
        | "hu" | "ro" | "de" | "fr" | "it" | "es" | "pt" | "nl" | "da" | "sv" | "nb" | "nn"
        | "no" | "fi" | "is" | "et" | "lv" | "lt" | "el" | "tr" | "az" | "kk" | "ky" | "uz"
        | "tg" | "mn" | "ka" | "hy" | "sq" | "ca" | "eu" | "gl" | "af" | "id" | "vi" | "lb"
        | "fo" | "fy" | "rm" | "br" | "oc" | "sc" | "co" => Decimal::Comma,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_and_grouped_under_point() {
        let r = |s: &str| read(s, Decimal::Point);
        assert_eq!(r("42"), Some(Value::Int(42)));
        assert_eq!(r("-7"), Some(Value::Int(-7)));
        assert_eq!(r("+3.5"), Some(Value::Float(3.5)));
        assert_eq!(r("1e3"), Some(Value::Float(1000.0)));
        assert_eq!(r("1,234"), Some(Value::Int(1234)));
        assert_eq!(r("1,234,567.89"), Some(Value::Float(1234567.89)));
        assert_eq!(r("1 234"), Some(Value::Int(1234)));
        assert_eq!(r("1\u{A0}234\u{202F}567"), Some(Value::Int(1234567)));
        assert_eq!(r("1'234.5"), Some(Value::Float(1234.5)));
        assert_eq!(r(".5"), Some(Value::Float(0.5)));
        // not numbers under point
        assert_eq!(r("12,50"), None);
        assert_eq!(r("1,2345"), None);
        assert_eq!(r("1234,567"), None);
        assert_eq!(r(",5"), None);
        assert_eq!(r("1,"), None);
        assert_eq!(r("abc"), None);
        assert_eq!(r("1_000"), None);
        assert_eq!(r(""), None);
        // the three floats Python spells by name, and only those
        assert!(matches!(r("inf"), Some(Value::Float(f)) if f == f64::INFINITY));
        assert!(matches!(r("-inf"), Some(Value::Float(f)) if f == f64::NEG_INFINITY));
        assert!(matches!(r("nan"), Some(Value::Float(f)) if f.is_nan()));
        assert_eq!(r("Infinity"), None);
        assert_eq!(r("NaN"), None);
    }

    #[test]
    fn comma_convention() {
        let r = |s: &str| read(s, Decimal::Comma);
        assert_eq!(r("12,50"), Some(Value::Float(12.5)));
        assert_eq!(r("1 234,5"), Some(Value::Float(1234.5)));
        assert_eq!(r("1.234.567"), Some(Value::Int(1234567)));
        assert_eq!(r("9.75"), None);
        assert_eq!(r("1.234,5"), Some(Value::Float(1234.5)));
        assert_eq!(r("42"), Some(Value::Int(42)));
    }

    #[test]
    fn locales() {
        assert_eq!(locale_convention("ru-RU"), Some(Decimal::Comma));
        assert_eq!(locale_convention("de-CH"), Some(Decimal::Point));
        assert_eq!(locale_convention("de"), Some(Decimal::Comma));
        assert_eq!(locale_convention("en-US"), Some(Decimal::Point));
        assert_eq!(locale_convention("xx-YY"), None);
    }
}
