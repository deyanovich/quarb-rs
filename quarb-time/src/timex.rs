//! Time expressions in text (spec: Time Expressions and the
//! Timeline): `| timex(LOCALE)` finds the absolute, relative and
//! vague time expressions of a text and writes each one's value in
//! a fixed notation (see [`crate::period`]).
//!
//! Recognition is by finite lists and nothing else: the locale's
//! month and weekday names, its numerals to a hundred, its unit
//! words in the inflected forms listed, and its rules — templates
//! over those lists. No stemmer, no tagger, no inference: an
//! expression outside the lists is not found. The lists live in
//! [`crate::timexdata`], one table per locale.

use crate::datenames::Names;
use crate::period::{Grain, Period, WEEKDAY_CODES};
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The kinds, as TimeML's TIMEX3 names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Date,
    Time,
    Duration,
    Relative,
    Vague,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Date => "date",
            Kind::Time => "time",
            Kind::Duration => "duration",
            Kind::Relative => "relative",
            Kind::Vague => "vague",
        }
    }
}

/// One time expression found in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The expression as written.
    pub text: String,
    /// Its position in the text, in characters from zero.
    pub at: usize,
    pub kind: Kind,
    /// The value in the fixed notation; `None` for a vague one.
    pub value: Option<String>,
    /// The precision of a date or a time.
    pub grain: Option<Grain>,
}

/// A part of the day, as a word beside a clock hour names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Before noon: twelve is midnight.
    Am,
    /// After noon: one to eleven are 13 to 23.
    Pm,
    /// At night: six to eleven are the evening's, twelve is
    /// midnight, one to five the small hours.
    Night,
}

/// What a rule's match means.
#[derive(Clone, Copy, Debug)]
pub enum Act {
    /// A calendar date from whichever of year, month and day the
    /// template captured.
    Date,
    /// A time of day from the captured hour (and minute), moved by
    /// this many minutes (`half past` is +30, `quarter to` −15).
    Clock(i32),
    /// A time of day from the captured hour and a count of minutes
    /// in words or digits, after it (+1: `ten minutes past six`,
    /// `huit heures quarante-cinq`) or before it (−1: `twenty
    /// minutes before nine`).
    ClockMin(i32),
    /// The same with the hour named by the rule (`midi moins
    /// vingt`: twelve, minus the count).
    ClockAt(u32, i32),
    /// A relative span: the sign, the captured count (one without
    /// a count) and unit.
    Rel(i64),
    /// The next (+1) or the last (−1) of the captured weekday.
    Weekday(i64),
    /// A duration: the captured count (one without) and unit.
    Dur,
    /// A fixed value.
    Const(Kind, &'static str, Option<Grain>),
    /// Found, and not valued.
    Vague,
}

/// A template and its meaning. The template is a regular
/// expression over slots: `{n}` a numeral, `{a}` a numeral or an
/// article standing for one, `{u}` a unit, `{M}` a month name,
/// `{W}` a weekday name, `{d}` a day number with the locale's
/// ordinal mark, `{dn}` a day as digits or a numeral, `{dd}` and
/// `{mm}` a numeric day and month, `{y}` a year, `{t}` a clock
/// time `H:MM`, `{H}` an hour as digits or a numeral, `{p}` a part
/// of the day. A space stands for any run of white space.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub pat: &'static str,
    pub act: Act,
    /// Words that, standing directly before the match, cancel it
    /// (`la mañana` is the morning, a bare `mañana` tomorrow).
    pub not_after: &'static [&'static str],
    /// Words that, standing directly after the match, cancel it;
    /// `{M}` stands for any month name (`on the 7th` is the day
    /// alone unless `of October` follows).
    pub not_before: &'static [&'static str],
}

pub const fn r(pat: &'static str, act: Act) -> Rule {
    Rule {
        pat,
        act,
        not_after: &[],
        not_before: &[],
    }
}

pub const fn rn(pat: &'static str, act: Act, not_after: &'static [&'static str]) -> Rule {
    Rule {
        pat,
        act,
        not_after,
        not_before: &[],
    }
}

pub const fn rb(pat: &'static str, act: Act, not_before: &'static [&'static str]) -> Rule {
    Rule {
        pat,
        act,
        not_after: &[],
        not_before,
    }
}

/// A locale's numerals from one to a hundred. `simple` words stand
/// alone; where the language compounds by juxtaposition, `tens`
/// and `ones` combine across `sep` (a regular expression), the
/// tens word first.
#[derive(Clone, Debug, Default)]
pub struct Numerals {
    pub simple: Vec<(String, u32)>,
    pub tens: Vec<(String, u32)>,
    pub ones: Vec<(String, u32)>,
    pub sep: &'static str,
    /// Articles standing for one where `{a}` allows them.
    pub one: Vec<String>,
    /// Which of them also name the hour one (`une heure`; English
    /// `an` never does).
    pub hour_one: Vec<String>,
}

/// A locale's lists. The seven unit rows are second, minute, hour,
/// day, week, month, year.
pub struct Lang {
    /// Whether words are delimited (false for Chinese and
    /// Japanese: no word boundary is asked for).
    pub spaced: bool,
    /// Whether running text abbreviates months (`Oct. 6`,
    /// `6 окт.`): the name table's abbreviations then read as
    /// months, with or without their point. Off where an
    /// abbreviation is a common word of the language (`mar`,
    /// `set`).
    pub abbr: bool,
    pub units: [&'static [&'static str]; 7],
    /// Month forms beyond the name table's (1 = January).
    pub months: &'static [(u32, &'static str)],
    /// Weekday forms beyond the name table's (0 = Monday).
    pub weekdays: &'static [(u32, &'static str)],
    /// What may follow a day number, as a regular expression
    /// (`(?:st|nd|rd|th)?`).
    pub ordinal: &'static str,
    pub numerals: fn() -> Numerals,
    /// The words of numbers past the lists (`hundred and`,
    /// `thousand`): a numeral standing directly after one of them,
    /// or after another numeral, is the tail of a larger number
    /// and is not read (`one hundred and fifty years` is not fifty
    /// years).
    pub above: &'static [&'static str],
    /// The hour named by its ordinal, where a language counts the
    /// minutes of the hour under way (`сорок минут девятого` is
    /// 8:40, `половина десятого` 9:30): each form with the hour
    /// it names, read as one less.
    pub hour_ordinals: &'static [(&'static str, u32)],
    pub parts: &'static [(&'static str, Part)],
    pub rules: &'static [Rule],
}

/// The unit letters of the ISO-8601 duration, by unit row, and
/// whether the unit stands after `T`.
const UNIT_CODES: [(char, bool); 7] = [
    ('S', true),
    ('M', true),
    ('H', true),
    ('D', false),
    ('W', false),
    ('M', false),
    ('Y', false),
];

struct Compiled {
    spaced: bool,
    /// Words before a numeral that make it the tail of a larger
    /// number: every numeral form, and the locale's `above`.
    larger: Vec<String>,
    rules: Vec<(Regex, &'static Rule)>,
    simple: HashMap<String, u32>,
    tens: HashMap<String, u32>,
    ones: HashMap<String, u32>,
    units: HashMap<String, usize>,
    ordinals: HashMap<String, u32>,
    months: HashMap<String, u32>,
    weekdays: HashMap<String, u32>,
    parts: HashMap<String, Part>,
}

/// Lowercase, white-space runs to one space. With `hyphens`, a
/// hyphen counts as white space (numerals: `twenty-one`).
fn key(form: &str, hyphens: bool) -> String {
    let mut out = String::with_capacity(form.len());
    let mut gap = false;
    for c in form.chars() {
        if c.is_whitespace() || (hyphens && c == '-') {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
        }
        gap = false;
        out.extend(c.to_lowercase());
    }
    out
}

/// An alternation of forms, the longest first; a space in a form
/// matches any white space (and a hyphen, for numerals).
fn alternation<'a>(forms: impl Iterator<Item = &'a str>, hyphens: bool) -> String {
    let mut forms: Vec<String> = forms.map(|f| key(f, hyphens)).collect();
    forms.sort_by(|a, b| {
        b.chars()
            .count()
            .cmp(&a.chars().count())
            .then_with(|| a.cmp(b))
    });
    forms.dedup();
    let gap = if hyphens { r"[\s\-]+" } else { r"\s+" };
    forms
        .iter()
        .map(|f| regex::escape(f).replace(' ', gap))
        .collect::<Vec<_>>()
        .join("|")
}

const DAY: &str = "0?[1-9]|[12][0-9]|3[01]";

impl Compiled {
    fn new(lang: &Lang, names: &Names) -> Compiled {
        let num = (lang.numerals)();
        let mut simple = HashMap::new();
        let mut tens = HashMap::new();
        let mut ones = HashMap::new();
        for (map, list) in [
            (&mut simple, &num.simple),
            (&mut tens, &num.tens),
            (&mut ones, &num.ones),
        ] {
            for (form, value) in list {
                map.insert(key(form, true), *value);
            }
        }
        // `{n}` and `{a}` differ by the articles alone.
        let numeral = |articles: bool| {
            let mut alts = vec![r"(?P<nd>[0-9]{1,3})".to_string()];
            if !num.tens.is_empty() {
                alts.push(format!(
                    "(?P<nt>{})(?:{}(?P<nu>{}))?",
                    alternation(num.tens.iter().map(|(f, _)| f.as_str()), true),
                    num.sep,
                    alternation(num.ones.iter().map(|(f, _)| f.as_str()), true),
                ));
            }
            let words = num
                .simple
                .iter()
                .map(|(f, _)| f.as_str())
                .chain(num.one.iter().filter(|_| articles).map(String::as_str));
            alts.push(format!("(?P<ns>{})", alternation(words, true)));
            format!("(?:{})", alts.join("|"))
        };
        for a in &num.one {
            simple.entry(key(a, true)).or_insert(1);
        }
        let hour_words = alternation(
            num.simple
                .iter()
                .filter(|(_, v)| *v <= 24)
                .map(|(f, _)| f.as_str())
                .chain(num.hour_one.iter().map(String::as_str)),
            true,
        );

        let mut ordinals = HashMap::new();
        for (f, h) in lang.hour_ordinals {
            ordinals.insert(key(f, false), *h);
        }
        let ordinal_alt = alternation(lang.hour_ordinals.iter().map(|(f, _)| *f), false);
        let mut units = HashMap::new();
        for (row, forms) in lang.units.iter().enumerate() {
            for f in forms.iter() {
                units.insert(key(f, false), row);
            }
        }
        let unit_alt = alternation(lang.units.iter().flat_map(|f| f.iter().copied()), false);

        // Months: the wide forms as written; the abbreviations (where
        // the locale reads them) with or without their point.
        let mut months = HashMap::new();
        for (i, wide) in names.months.iter().enumerate() {
            for f in wide.iter() {
                months.insert(key(f, false), i as u32 + 1);
            }
        }
        for (m, f) in lang.months {
            months.insert(key(f, false), *m);
        }
        let mut month_alts: Vec<(String, bool)> =
            months.keys().map(|k| (k.clone(), false)).collect();
        if lang.abbr {
            for (i, abbr) in names.months_abbr.iter().enumerate() {
                for f in abbr.iter() {
                    let k = key(f.trim_end_matches('.'), false);
                    if !months.contains_key(&k) {
                        months.insert(k.clone(), i as u32 + 1);
                        month_alts.push((k, true));
                    }
                }
            }
        }
        month_alts.sort_by(|a, b| {
            b.0.chars()
                .count()
                .cmp(&a.0.chars().count())
                .then_with(|| a.0.cmp(&b.0))
        });
        let month_alts: Vec<String> = month_alts
            .iter()
            .map(|(f, point)| {
                let f = regex::escape(f).replace(' ', r"\s+");
                if *point { format!(r"{f}\.?") } else { f }
            })
            .collect();
        let month_alt = month_alts.join("|");

        let mut weekdays = HashMap::new();
        for (i, forms) in names.days.iter().enumerate() {
            for f in forms.iter() {
                weekdays.insert(key(f, false), i as u32);
            }
        }
        for (d, f) in lang.weekdays {
            weekdays.insert(key(f, false), *d);
        }
        let weekday_alt = alternation(weekdays.keys().map(String::as_str), false);

        let mut parts = HashMap::new();
        for (f, p) in lang.parts {
            parts.insert(key(f, false), *p);
        }
        let part_alt = alternation(lang.parts.iter().map(|(f, _)| *f), false);

        let slots: [(&str, String); 16] = [
            ("{n}", numeral(false)),
            ("{a}", numeral(true)),
            ("{u}", format!("(?P<u>{unit_alt})")),
            ("{M}", format!("(?P<M>{month_alt})")),
            ("{W}", format!("(?P<W>{weekday_alt})")),
            ("{dn}", format!("(?:(?P<d>{DAY})|{})", numeral(false))),
            ("{dd}", format!("(?P<d>{DAY})")),
            ("{d}", format!("(?P<d>{DAY}){}", lang.ordinal)),
            ("{mm}", "(?P<mn>0?[1-9]|1[0-2])".to_string()),
            ("{y}", "(?P<y>[12][0-9]{3})".to_string()),
            (
                "{t}",
                "(?P<h>[01]?[0-9]|2[0-3]):(?P<m>[0-5][0-9])".to_string(),
            ),
            (
                "{H}",
                format!("(?:(?P<h>[01]?[0-9]|2[0-4])|(?P<hs>{hour_words}))"),
            ),
            // The hour as digits alone, and as a word alone.
            ("{Hd}", "(?P<h>[01]?[0-9]|2[0-4])".to_string()),
            ("{Hw}", format!("(?P<hs>{hour_words})")),
            ("{p}", format!("(?P<p>{part_alt})")),
            // The hour under way: an ordinal word, or digits with
            // the ordinal's ending (`11го`).
            (
                "{O}",
                format!("(?:(?P<od>[01]?[0-9]|2[0-4])-?го|(?P<o>{ordinal_alt}))"),
            ),
        ];
        static ISO: Rule = r(
            "(?P<y>[0-9]{4})-(?P<mn>0[1-9]|1[0-2])-(?P<d>0[1-9]|[12][0-9]|3[01])",
            Act::Date,
        );
        let rules = std::iter::once(&ISO)
            .chain(lang.rules.iter())
            .map(|rule| {
                let mut pat = rule.pat.replace(' ', r"\s+");
                for (slot, alt) in &slots {
                    pat = pat.replace(slot, alt);
                }
                let pat = if lang.spaced {
                    // A word boundary at both ends. Where a template
                    // ends in an abbreviation's point (`p.m.`,
                    // `г.`) the boundary falls before the point,
                    // which then stays with the sentence.
                    format!(r"(?i)\b(?:{pat})\b")
                } else {
                    format!("(?i)(?:{pat})")
                };
                let re = Regex::new(&pat)
                    .unwrap_or_else(|e| panic!("timex rule {:?} does not compile: {e}", rule.pat));
                (re, rule)
            })
            .collect();
        let mut larger: Vec<String> = num
            .simple
            .iter()
            .chain(&num.tens)
            .chain(&num.ones)
            .map(|(f, _)| key(f, true))
            .chain(lang.above.iter().map(|w| key(w, true)))
            .collect();
        larger.sort();
        larger.dedup();
        Compiled {
            spaced: lang.spaced,
            larger,
            rules,
            simple,
            tens,
            ones,
            units,
            ordinals,
            months,
            weekdays,
            parts,
        }
    }

    /// The captured numeral's value.
    fn number(&self, caps: &regex::Captures<'_>) -> Option<u32> {
        if let Some(d) = caps.name("nd") {
            return d.as_str().parse().ok();
        }
        if let Some(t) = caps.name("nt") {
            let tens = *self.tens.get(&key(t.as_str(), true))?;
            let ones = match caps.name("nu") {
                Some(u) => *self.ones.get(&key(u.as_str(), true))?,
                None => 0,
            };
            return Some(tens + ones);
        }
        let s = caps.name("ns")?;
        self.simple.get(&key(s.as_str(), true)).copied()
    }

    fn value(
        &self,
        act: Act,
        caps: &regex::Captures<'_>,
    ) -> Option<(Kind, Option<String>, Option<Grain>)> {
        let unit = |caps: &regex::Captures<'_>| {
            let u = caps.name("u")?;
            self.units.get(&key(u.as_str(), false)).copied()
        };
        let span = |n: u32, row: usize| {
            let (code, timed) = UNIT_CODES[row];
            format!("P{}{n}{code}", if timed { "T" } else { "" })
        };
        // A count is asked for only where the template has one.
        let counted = ["nd", "nt", "ns"].iter().any(|g| caps.name(g).is_some());
        let count = |caps: &regex::Captures<'_>| {
            if counted {
                self.number(caps).filter(|n| *n >= 1)
            } else {
                Some(1)
            }
        };
        Some(match act {
            Act::Vague => (Kind::Vague, None, None),
            Act::Const(kind, value, grain) => (kind, Some(value.to_string()), grain),
            Act::Dur => {
                let row = unit(caps)?;
                (Kind::Duration, Some(span(count(caps)?, row)), None)
            }
            Act::Rel(sign) => {
                let row = unit(caps)?;
                let sign = if sign < 0 { '-' } else { '+' };
                (
                    Kind::Relative,
                    Some(format!("{sign}{}", span(count(caps)?, row))),
                    None,
                )
            }
            Act::Weekday(sign) => {
                let w = caps.name("W")?;
                let day = *self.weekdays.get(&key(w.as_str(), false))?;
                let sign = if sign < 0 { '-' } else { '+' };
                (
                    Kind::Relative,
                    Some(format!("{sign}1{}", WEEKDAY_CODES[day as usize])),
                    None,
                )
            }
            Act::Clock(_) | Act::ClockMin(_) | Act::ClockAt(..) => {
                let minutes = |sign: i32| {
                    let n = self.number(caps).filter(|n| (1..60).contains(n))? as i32;
                    Some(sign.signum() * n)
                };
                let (delta, fixed) = match act {
                    Act::ClockMin(sign) => (minutes(sign)?, None),
                    Act::ClockAt(hour, sign) => (minutes(sign)?, Some(hour)),
                    Act::Clock(delta) => (delta, None),
                    _ => unreachable!(),
                };
                let counted_minutes = !matches!(act, Act::Clock(_));
                // The hour under way counts as one less.
                let under_way: Option<u32> = match (caps.name("od"), caps.name("o")) {
                    (Some(d), _) => Some(d.as_str().parse::<u32>().ok()?.checked_sub(1)?),
                    (_, Some(w)) => {
                        Some(self.ordinals.get(&key(w.as_str(), false))?.checked_sub(1)?)
                    }
                    _ => None,
                };
                let hour: u32 = match (fixed.or(under_way), caps.name("h"), caps.name("hs")) {
                    (Some(hour), ..) => hour,
                    (None, Some(h), _) => h.as_str().parse().ok()?,
                    (None, None, Some(w)) => *self.simple.get(&key(w.as_str(), true))?,
                    (None, None, None) if !counted_minutes => self.number(caps)?,
                    (None, None, None) => return None,
                };
                let minute: Option<u32> = match caps.name("m") {
                    Some(m) => Some(m.as_str().parse().ok()?),
                    None => None,
                };
                if hour > 24 {
                    return None;
                }
                let part = caps
                    .name("p")
                    .and_then(|p| self.parts.get(&key(p.as_str(), false)).copied());
                let hour = match part {
                    None => hour % 24,
                    Some(_) if hour > 12 => hour % 24,
                    Some(Part::Am) => hour % 12,
                    Some(Part::Pm) => hour % 12 + 12,
                    Some(Part::Night) => match hour {
                        12 => 0,
                        1..=5 => hour,
                        _ => hour + 12,
                    },
                };
                let total =
                    (hour as i32 * 60 + minute.unwrap_or(0) as i32 + delta).rem_euclid(1440);
                let (h, m) = (total / 60, total % 60);
                if minute.is_some() || delta != 0 {
                    (
                        Kind::Time,
                        Some(format!("T{h:02}:{m:02}")),
                        Some(Grain::Minute),
                    )
                } else {
                    (Kind::Time, Some(format!("T{h:02}")), Some(Grain::Hour))
                }
            }
            Act::Date => {
                let year: Option<i64> = match caps.name("y") {
                    Some(y) => Some(y.as_str().parse().ok()?),
                    None => None,
                };
                let month: Option<u32> = match (caps.name("M"), caps.name("mn")) {
                    (Some(m), _) => Some(
                        *self
                            .months
                            .get(&key(m.as_str().trim_end_matches('.'), false))?,
                    ),
                    (None, Some(n)) => Some(n.as_str().parse().ok()?),
                    (None, None) => None,
                };
                let day: Option<u32> = match caps.name("d") {
                    Some(d) => Some(d.as_str().parse().ok()?),
                    None if counted => Some(self.number(caps)?),
                    None => None,
                };
                let (value, grain) = match (year, month, day) {
                    (Some(y), Some(m), Some(d)) => (
                        Period::of(y, m, d, 0, 0, 0, Grain::Day)?.format(),
                        Grain::Day,
                    ),
                    (Some(y), Some(m), None) => (
                        Period::of(y, m, 1, 0, 0, 0, Grain::Month)?.format(),
                        Grain::Month,
                    ),
                    (Some(y), None, None) => (
                        Period::of(y, 1, 1, 0, 0, 0, Grain::Year)?.format(),
                        Grain::Year,
                    ),
                    (None, Some(m), Some(d)) => {
                        // A leap year: 29 February is a date.
                        Period::of(2000, m, d, 0, 0, 0, Grain::Day)?;
                        (format!("XXXX-{m:02}-{d:02}"), Grain::Day)
                    }
                    (None, Some(m), None) => (format!("XXXX-{m:02}"), Grain::Month),
                    (None, None, Some(d)) if (1..=31).contains(&d) => {
                        (format!("XXXX-XX-{d:02}"), Grain::Day)
                    }
                    _ => return None,
                };
                (Kind::Date, Some(value), Some(grain))
            }
        })
    }
}

/// The digits other scripts write and the typographic apostrophe,
/// each as its ASCII counterpart, and the underscore as a space
/// (plain-text editions mark italics with it: `_3 May._`, and a
/// regular expression counts it a word character): one character
/// for one, so positions in characters hold.
fn fold(c: char) -> char {
    match c {
        '\u{0660}'..='\u{0669}' => (b'0' + (c as u32 - 0x0660) as u8) as char,
        '\u{06F0}'..='\u{06F9}' => (b'0' + (c as u32 - 0x06F0) as u8) as char,
        '\u{FF10}'..='\u{FF19}' => (b'0' + (c as u32 - 0xFF10) as u8) as char,
        '\u{2019}' | '\u{02BC}' => '\'',
        '\u{FF1A}' => ':',
        '_' => ' ',
        // Turkish capital dotted I, which simple case folding
        // leaves apart from `i`.
        '\u{0130}' => 'i',
        _ => c,
    }
}

/// The last `n` characters of `s`.
fn tail(s: &str, n: usize) -> &str {
    match s.char_indices().rev().nth(n.saturating_sub(1)) {
        Some((i, _)) => &s[i..],
        None => s,
    }
}

/// The locales that carry lists, in the order the refusal names
/// them.
pub const CARRIED: &str = "en, zh, es, ar, pt, ru, ja, de, fr, tr, it";

fn compiled(locale: &str) -> Option<&'static Compiled> {
    static TABLE: OnceLock<std::sync::Mutex<HashMap<String, &'static Compiled>>> = OnceLock::new();
    let lang = crate::language(locale);
    let lang = if lang.is_empty() {
        "en".to_string()
    } else {
        lang
    };
    let data = crate::timexdata::lang(&lang)?;
    let names = crate::datenames::names(&lang)?;
    let mut table = TABLE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = table.get(&lang) {
        return Some(c);
    }
    // Compiled once per locale and kept for the life of the
    // process: a handful of tables, read on every call.
    let c: &'static Compiled = Box::leak(Box::new(Compiled::new(data, names)));
    table.insert(lang, c);
    Some(c)
}

/// The time expressions of `text` under `locale`'s lists, in the
/// order of the text. `None` for a locale that carries none (see
/// [`CARRIED`]).
pub fn find(text: &str, locale: &str) -> Option<Vec<Found>> {
    let c = compiled(locale)?;
    let folded: String = text.chars().map(fold).collect();
    // Byte offset in `folded` → character index, and character
    // index → byte offset in `text`.
    let folded_chars: Vec<usize> = folded.char_indices().map(|(i, _)| i).collect();
    let text_bytes: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let char_at = |byte: usize| folded_chars.partition_point(|&b| b < byte);
    let word = |c: char| c.is_alphanumeric();

    struct Hit {
        start: usize,
        end: usize,
        rule: usize,
        kind: Kind,
        value: Option<String>,
        grain: Option<Grain>,
    }
    let mut hits: Vec<Hit> = Vec::new();
    for (ri, (re, rule)) in c.rules.iter().enumerate() {
        let (act, not_after) = (&rule.act, rule.not_after);
        for caps in re.captures_iter(&folded) {
            let m = caps.get(0).expect("the whole match");
            if m.is_empty() {
                continue;
            }
            // A part of a hyphened word is not the word: `midi` in
            // `après-midi`, `matin` in `ce matin-là`.
            if c.spaced {
                let mut before = folded[..m.start()].chars().rev();
                let mut after = folded[m.end()..].chars();
                let joined =
                    |a: Option<char>, b: Option<char>| a == Some('-') && b.is_some_and(word);
                if joined(before.next(), before.next()) || joined(after.next(), after.next()) {
                    continue;
                }
            }
            // The tail of a larger number is not a number.
            let numeral_first = ["nd", "nt", "ns"]
                .iter()
                .any(|g| caps.name(g).is_some_and(|n| n.start() == m.start()));
            if numeral_first {
                // `1.920 часов`, `115,200 minutes`: digits behind a
                // thousands separator are the tail of the number.
                let mut back = folded[..m.start()].chars().rev();
                if matches!(back.next(), Some('.' | ',' | '\u{a0}' | '\u{202f}'))
                    && back.next().is_some_and(|c| c.is_ascii_digit())
                {
                    continue;
                }
                let before = key(tail(&folded[..m.start()], 40), true);
                let larger = c.larger.iter().any(|w| {
                    before.ends_with(w.as_str())
                        && !(c.spaced
                            && before[..before.len() - w.len()]
                                .chars()
                                .next_back()
                                .is_some_and(word))
                });
                if larger {
                    continue;
                }
            }
            if !not_after.is_empty() {
                let before = folded[..m.start()].trim_end().to_lowercase();
                let vetoed = not_after.iter().any(|w| {
                    before.ends_with(w)
                        && !before[..before.len() - w.len()]
                            .chars()
                            .next_back()
                            .is_some_and(word)
                });
                if vetoed {
                    continue;
                }
            }
            if !rule.not_before.is_empty() {
                let after = folded[m.end()..].trim_start().to_lowercase();
                let next: String = after.chars().take_while(|c| word(*c)).collect();
                let vetoed = rule.not_before.iter().any(|w| {
                    if *w == "{M}" {
                        c.months.contains_key(&next)
                    } else {
                        next == *w
                    }
                });
                if vetoed {
                    continue;
                }
            }
            let Some((kind, value, grain)) = c.value(*act, &caps) else {
                continue;
            };
            hits.push(Hit {
                start: m.start(),
                end: m.end(),
                rule: ri,
                kind,
                value,
                grain,
            });
        }
    }
    // The earliest wins, then the longest, then the rule listed
    // first; an expression overlapping one already taken is
    // dropped.
    hits.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(a.rule.cmp(&b.rule))
    });
    let mut out = Vec::new();
    let mut taken = 0;
    for h in hits {
        if h.start < taken {
            continue;
        }
        taken = h.end;
        let (from, to) = (char_at(h.start), char_at(h.end));
        out.push(Found {
            text: text[text_bytes[from]..text_bytes[to]].to_string(),
            at: from,
            kind: h.kind,
            value: h.value,
            grain: h.grain,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text → kind value`, one line per expression.
    fn read(text: &str, locale: &str) -> Vec<String> {
        find(text, locale)
            .unwrap()
            .iter()
            .map(|f| {
                format!(
                    "{} → {} {}",
                    f.text,
                    f.kind.word(),
                    f.value.as_deref().unwrap_or("-")
                )
            })
            .collect()
    }

    #[test]
    fn every_locale_compiles() {
        for l in CARRIED.split(", ") {
            assert!(find("", l).is_some(), "{l}");
        }
        assert!(find("x", "xx").is_none());
        assert!(find("x", "pl").is_none());
    }

    #[test]
    fn english() {
        assert_eq!(
            read(
                "On 6 October 1899 he arrived. The next day he wrote to her. \
                 Two days later the answer came. That evening he left.",
                "en"
            ),
            [
                "6 October 1899 → date 1899-10-06",
                "The next day → relative +P1D",
                "Two days later → relative +P2D",
                "That evening → vague -",
            ]
        );
        assert_eq!(
            read("In October 2025, and again in 1812; on April 8th.", "en"),
            [
                "In October 2025 → date 2025-10",
                "in 1812 → date 1812",
                "April 8th → date XXXX-04-08",
            ]
        );
        assert_eq!(
            read(
                "two months earlier; the following Monday; last Friday; \
                 twenty-one years ago; a week later",
                "en"
            ),
            [
                "two months earlier → relative -P2M",
                "the following Monday → relative +1MO",
                "last Friday → relative -1FR",
                "twenty-one years ago → relative -P21Y",
                "a week later → relative +P1W",
            ]
        );
        assert_eq!(
            read(
                "He waited five minutes, then two weeks. At 6:30 p.m. and at half past six; \
                 at ten o'clock at night; at noon.",
                "en"
            ),
            [
                "five minutes → duration PT5M",
                "two weeks → duration P2W",
                "6:30 p.m → time T18:30",
                "half past six → time T06:30",
                "ten o'clock at night → time T22",
                "noon → time T12",
            ]
        );
        assert_eq!(
            read(
                "on Monday; soon; some years later; yesterday; \
                 the day after tomorrow; 1899-10-06",
                "en"
            ),
            [
                "on Monday → vague -",
                "soon → vague -",
                "some years later → vague -",
                "yesterday → relative -P1D",
                "the day after tomorrow → relative +P2D",
                "1899-10-06 → date 1899-10-06",
            ]
        );
        assert_eq!(
            read(
                "on the 8th, at half past six in the evening; a quarter to one at night",
                "en"
            ),
            [
                "on the 8th → date XXXX-XX-08",
                "half past six in the evening → time T18:30",
                "a quarter to one at night → time T00:45",
            ]
        );
        // the weekday beside a date, the day alone and its month,
        // the clock in words, and the span `in` names
        assert_eq!(
            read(
                "Wednesday, the 2nd of October; on the 7th of October; on the 7th; \
                 Left London, Wednesday, October 2nd, at 8.45 p.m.; \
                 twenty minutes before nine; ten minutes past ten in the evening; \
                 half-past eight; in eighty days; an hour",
                "en"
            ),
            [
                "Wednesday, the 2nd of October → date XXXX-10-02",
                "the 7th of October → date XXXX-10-07",
                "on the 7th → date XXXX-XX-07",
                "Wednesday, October 2nd → date XXXX-10-02",
                "8.45 p.m → time T20:45",
                "twenty minutes before nine → time T08:40",
                "ten minutes past ten in the evening → time T22:10",
                "half-past eight → time T08:30",
                "in eighty days → duration P80D",
            ]
        );
        assert_eq!(
            read(
                "Quitté Londres, mercredi 2 octobre, 8 heures 45 soir. \
                 à huit heures quarante-cinq; à neuf heures moins le quart; \
                 l'après-midi; à midi; une heure après; une heure; \
                 en quatre-vingts jours; pendant une heure",
                "fr"
            ),
            [
                "mercredi 2 octobre → date XXXX-10-02",
                "8 heures 45 soir → time T20:45",
                "à huit heures quarante-cinq → time T08:45",
                "à neuf heures moins le quart → time T08:45",
                "midi → time T12",
                "une heure après → relative +PT1H",
                "quatre-vingts jours → duration P80D",
                "pendant une heure → duration PT1H",
            ]
        );
        assert_eq!(
            read(
                "vers deux heures après midi; à midi moins vingt; vers midi et demi; \
                 à une heure du matin",
                "fr"
            ),
            [
                "vers deux heures après midi → time T14",
                "midi moins vingt → time T11:40",
                "midi et demi → time T12:30",
                "à une heure du matin → time T01",
            ]
        );
        // an article is no hour, and a place is no time
        assert!(read("A quarter of an hour; Anam; an am", "en").is_empty());
        // the tail of a larger number is not read
        assert_eq!(
            read(
                "three hundred years; one hundred and fifty years; a hundred years; \
                 twenty-one years; to-morrow; last week; the last hour",
                "en"
            ),
            [
                "a hundred years → duration P100Y",
                "twenty-one years → duration P21Y",
                "to-morrow → relative +P1D",
                "last week → relative -P1W",
            ]
        );
        // italics marked with underscores, as plain-text editions do
        assert_eq!(
            read(
                "_3 May. Bistritz._--Left Munich at 8:35 P. M., on 1st May. _3 August_.--At midnight",
                "en"
            ),
            [
                "3 May → date XXXX-05-03",
                "8:35 P. M → time T20:35",
                "1st May → date XXXX-05-01",
                "3 August → date XXXX-08-03",
                "midnight → time T00",
            ]
        );
        // positions count characters; nothing is found in a word
        let f = find("“Tomorrow,” she said — Wednesdays, someone days.", "en").unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].text.as_str(), f[0].at), ("Tomorrow", 1));
        // a day the calendar lacks is not read; the month still is
        assert_eq!(
            read("30 February 1899", "en"),
            ["February 1899 → date 1899-02"]
        );
        assert_eq!(
            read("Oct. 6, 1899", "en"),
            ["Oct. 6, 1899 → date 1899-10-06"]
        );
        let g = find("6 October 1899 at 6:30", "en").unwrap();
        assert_eq!(g[0].grain, Some(Grain::Day));
        assert_eq!(g[1].grain, Some(Grain::Minute));
    }

    #[test]
    fn russian() {
        assert_eq!(
            read(
                "6 октября 1899 года он приехал. На следующий день написал ей. \
                 Через два месяца пришёл ответ. Двумя месяцами позже он уехал. \
                 В тот вечер шёл дождь.",
                "ru"
            ),
            [
                "6 октября 1899 года → date 1899-10-06",
                "На следующий день → relative +P1D",
                "Через два месяца → relative +P2M",
                "Двумя месяцами позже → relative +P2M",
                "В тот вечер → vague -",
            ]
        );
        assert_eq!(
            read(
                "в октябре 1899 года; в 1812 году; 8 апреля; через неделю; \
                 двадцать один год назад; три дня спустя; вчера; послезавтра",
                "ru-RU"
            ),
            [
                "в октябре 1899 года → date 1899-10",
                "в 1812 году → date 1812",
                "8 апреля → date XXXX-04-08",
                "через неделю → relative +P1W",
                "двадцать один год назад → relative -P21Y",
                "три дня спустя → relative +P3D",
                "вчера → relative -P1D",
                "послезавтра → relative +P2D",
            ]
        );
        assert_eq!(
            read(
                "в следующую среду; в прошлый понедельник; пять минут; \
                 в шесть часов вечера; несколько дней спустя; 06.10.1899",
                "ru"
            ),
            [
                "в следующую среду → relative +1WE",
                "в прошлый понедельник → relative -1MO",
                "пять минут → duration PT5M",
                "в шесть часов вечера → time T18",
                "несколько дней спустя → vague -",
                "06.10.1899 → date 1899-10-06",
            ]
        );
    }

    #[test]
    fn the_other_locales() {
        assert_eq!(
            read(
                "Am 6. Oktober 1899 kam er an. Am nächsten Tag schrieb er. Zwei Monate später; vor drei Wochen; gestern; einundzwanzig Jahre",
                "de"
            ),
            [
                "6. Oktober 1899 → date 1899-10-06",
                "Am nächsten Tag → relative +P1D",
                "Zwei Monate später → relative +P2M",
                "vor drei Wochen → relative -P3W",
                "gestern → relative -P1D",
                "einundzwanzig Jahre → duration P21Y",
            ]
        );
        assert_eq!(
            read(
                "Le 6 octobre 1899 il arriva. Le lendemain, il écrivit. Deux mois plus tard ; il y a trois semaines ; avant-hier ; quatre-vingt-onze ans",
                "fr"
            ),
            [
                "Le 6 octobre 1899 → date 1899-10-06",
                "Le lendemain → relative +P1D",
                "Deux mois plus tard → relative +P2M",
                "il y a trois semaines → relative -P3W",
                "avant-hier → relative -P2D",
                "quatre-vingt-onze ans → duration P91Y",
            ]
        );
        assert_eq!(
            read(
                "El 6 de octubre de 1899 llegó. Al día siguiente escribió. Dos meses después; hace tres semanas; por la mañana; mañana; veintiún años",
                "es"
            ),
            [
                "El 6 de octubre de 1899 → date 1899-10-06",
                "Al día siguiente → relative +P1D",
                "Dos meses después → relative +P2M",
                "hace tres semanas → relative -P3W",
                "por la mañana → vague -",
                "mañana → relative +P1D",
                "veintiún años → duration P21Y",
            ]
        );
        assert_eq!(
            read(
                "Il 6 ottobre 1899 arrivò. Il giorno dopo scrisse. Due mesi dopo; tre settimane fa; ieri; ventuno anni",
                "it"
            ),
            [
                "Il 6 ottobre 1899 → date 1899-10-06",
                "Il giorno dopo → relative +P1D",
                "Due mesi dopo → relative +P2M",
                "tre settimane fa → relative -P3W",
                "ieri → relative -P1D",
                "ventuno anni → duration P21Y",
            ]
        );
        assert_eq!(
            read(
                "Em 6 de outubro de 1899 chegou. No dia seguinte escreveu. Dois meses depois; há três semanas; ontem; vinte e um anos",
                "pt"
            ),
            [
                "6 de outubro de 1899 → date 1899-10-06",
                "No dia seguinte → relative +P1D",
                "Dois meses depois → relative +P2M",
                "há três semanas → relative -P3W",
                "ontem → relative -P1D",
                "vinte e um anos → duration P21Y",
            ]
        );
        assert_eq!(
            read(
                "6 Ekim 1899'da geldi. Ertesi gün yazdı. İki ay sonra; üç hafta önce; dün; yirmi bir yıl",
                "tr"
            ),
            [
                "6 Ekim 1899 → date 1899-10-06",
                "Ertesi gün → relative +P1D",
                "İki ay sonra → relative +P2M",
                "üç hafta önce → relative -P3W",
                "dün → relative -P1D",
                "yirmi bir yıl → duration P21Y",
            ]
        );
        assert_eq!(
            read(
                "وصل في 6 أكتوبر 1899. في اليوم التالي كتب. بعد ثلاثة أشهر؛ قبل يومين؛ أمس؛ ٢١ سنة",
                "ar"
            ),
            [
                "6 أكتوبر 1899 → date 1899-10-06",
                "في اليوم التالي → relative +P1D",
                "بعد ثلاثة أشهر → relative +P3M",
                "قبل يومين → relative -P2D",
                "أمس → relative -P1D",
                "٢١ سنة → duration P21Y",
            ]
        );
        assert_eq!(
            read(
                "1899年10月6日他到了。第二天他写了信。两个月后；三周前；昨天；二十一年",
                "zh"
            ),
            [
                "1899年10月6日 → date 1899-10-06",
                "第二天 → relative +P1D",
                "两个月后 → relative +P2M",
                "三周前 → relative -P3W",
                "昨天 → relative -P1D",
                "二十一年 → duration P21Y",
            ]
        );
        assert!(read("一百二十年；三百年", "zh").is_empty());
        assert!(read("cent vingt ans ; trois cent ans", "fr").is_empty());
        assert_eq!(
            read(
                "в среду 2го октября, 35 минут 11го вечером; 40 минут девятаго утром; \
                 половина десятого; четверть десятого вечера; без четверти девять; \
                 без десяти минут девять; в начале десятого",
                "ru"
            ),
            [
                "в среду 2го октября → date XXXX-10-02",
                "35 минут 11го вечером → time T22:35",
                "40 минут девятаго утром → time T08:40",
                "половина десятого → time T09:30",
                "четверть десятого вечера → time T21:15",
                "без четверти девять → time T08:45",
                "без десяти минут девять → time T08:50",
                "в начале десятого → vague -",
            ]
        );
        assert_eq!(
            read(
                "Апреля 8-го. Июня 12. 8-го числа; 21 числа; 8-го класса",
                "ru"
            ),
            [
                "Апреля 8-го → date XXXX-04-08",
                "Июня 12 → date XXXX-06-12",
                "8-го числа → date XXXX-XX-08",
                "21 числа → date XXXX-XX-21",
            ]
        );
        assert_eq!(
            read("четверть часа спустя; час спустя; годом позже", "ru"),
            ["час спустя → relative +PT1H", "годом позже → relative +P1Y"]
        );
        assert_eq!(
            read("the following days; the following day", "en"),
            ["the following day → relative +P1D"]
        );
        assert!(read("в 1.920 часов, или во 115.200 минут", "ru").is_empty());
        assert!(read("115,200 minutes", "en").is_empty());
        assert_eq!(
            read("сто двадцать лет; сто лет", "ru"),
            ["сто лет → duration P100Y"]
        );
        assert_eq!(
            read(
                "1899年10月6日に着いた。翌日、手紙を書いた。二か月後；三週間前；昨日；二十一年間",
                "ja"
            ),
            [
                "1899年10月6日 → date 1899-10-06",
                "翌日 → relative +P1D",
                "二か月後 → relative +P2M",
                "三週間前 → relative -P3W",
                "昨日 → relative -P1D",
                "二十一年間 → duration P21Y",
            ]
        );
    }
}
