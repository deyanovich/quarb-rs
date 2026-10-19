//! Periods (spec: The Temporal Fragment, The Calendar Reading): a
//! stretch of the UTC calendar named at a grain — a year, a month,
//! a day, an hour, a minute or a second — and the arithmetic of
//! anchoring a time expression's value against one (spec: Time
//! Expressions and the Timeline).
//!
//! The value notation is the one `timex` writes: ISO-8601 at
//! reduced precision for a date (`1899-10-06`, `2025-10`, `1812`),
//! `X` for a field left out (`XXXX-04-08`), `T06:30` for a time of
//! day, a signed ISO duration for a relative (`+P1D`, `-P2M`) or a
//! signed ordinal and an RFC 5545 weekday code (`+1MO`, `-1FR`).

use crate::temporal::{civil_from_days, components, days_from_civil};
use std::cmp::Ordering;

/// A period's precision, coarsest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Grain {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl Grain {
    pub fn word(self) -> &'static str {
        match self {
            Grain::Year => "year",
            Grain::Month => "month",
            Grain::Day => "day",
            Grain::Hour => "hour",
            Grain::Minute => "minute",
            Grain::Second => "second",
        }
    }

    pub fn from_word(w: &str) -> Option<Grain> {
        Some(match w {
            "year" => Grain::Year,
            "month" => Grain::Month,
            "day" => Grain::Day,
            "hour" => Grain::Hour,
            "minute" => Grain::Minute,
            "second" => Grain::Second,
            _ => return None,
        })
    }
}

/// A period: its first second on the UTC timeline, and its grain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Period {
    pub start: i64,
    pub grain: Grain,
    /// The calendar whose month or year the period names (spec:
    /// Calendars); the Gregorian for every period born of text.
    pub calendar: crate::calendar::Calendar,
}

fn month_days(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if days_from_civil(y + 1, 1, 1) - days_from_civil(y, 1, 1) == 366 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// The years a period may name: wide enough for any calendar a
/// text speaks of, narrow enough that no arithmetic here overflows.
const YEARS: std::ops::RangeInclusive<i64> = -999_999..=999_999;

impl Period {
    /// The period of `grain` holding the civil fields; the fields
    /// below the grain are ignored. `None` for a date the calendar
    /// lacks.
    pub fn of(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32, grain: Grain) -> Option<Period> {
        let (mo, d, h, mi, s) = match grain {
            Grain::Year => (1, 1, 0, 0, 0),
            Grain::Month => (mo, 1, 0, 0, 0),
            Grain::Day => (mo, d, 0, 0, 0),
            Grain::Hour => (mo, d, h, 0, 0),
            Grain::Minute => (mo, d, h, mi, 0),
            Grain::Second => (mo, d, h, mi, s),
        };
        if !YEARS.contains(&y)
            || !(1..=12).contains(&mo)
            || d < 1
            || d > month_days(y, mo)
            || h > 23
            || mi > 59
            || s > 59
        {
            return None;
        }
        Some(Period {
            start: days_from_civil(y, mo, d) * 86400 + h as i64 * 3600 + mi as i64 * 60 + s as i64,
            grain,
            calendar: Default::default(),
        })
    }

    /// The civil fields of the period's first second.
    pub fn fields(&self) -> (i64, u32, u32, u32, u32, u32) {
        components(self.start)
    }

    /// The same period read in `calendar` (spec: Calendars): the
    /// timeline unchanged, the month and year bounds the calendar's.
    pub fn in_calendar(self, calendar: crate::calendar::Calendar) -> Period {
        Period { calendar, ..self }
    }

    /// The first second after the period.
    pub fn end(&self) -> i64 {
        let (y, mo, ..) = self.fields();
        let days = self.start.div_euclid(86400);
        match self.grain {
            Grain::Year if self.calendar != Default::default() => {
                crate::calendar::next_year_start(self.calendar, days)
                    .map_or(self.start, |d| d * 86400)
            }
            Grain::Month if self.calendar != Default::default() => {
                crate::calendar::next_month_start(self.calendar, days)
                    .map_or(self.start, |d| d * 86400)
            }
            Grain::Year => days_from_civil(y + 1, 1, 1) * 86400,
            Grain::Month => {
                let (y, mo) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
                days_from_civil(y, mo, 1) * 86400
            }
            Grain::Day => self.start + 86400,
            Grain::Hour => self.start + 3600,
            Grain::Minute => self.start + 60,
            Grain::Second => self.start + 1,
        }
    }

    /// The period of a coarser (or the same) grain holding this
    /// one; a finer grain has none.
    pub fn at(&self, grain: Grain) -> Option<Period> {
        if grain > self.grain {
            return None;
        }
        // a calendar's month or year begins where the calendar says
        if self.calendar != Default::default() && matches!(grain, Grain::Year | Grain::Month) {
            let days = self.start.div_euclid(86400);
            let start = match grain {
                Grain::Year => crate::calendar::year_start(self.calendar, days)?,
                _ => crate::calendar::month_start(self.calendar, days)?,
            };
            return Some(Period {
                start: start * 86400,
                grain,
                calendar: self.calendar,
            });
        }
        let (y, mo, d, h, mi, s) = self.fields();
        Period::of(y, mo, d, h, mi, s, grain).map(|p| p.in_calendar(self.calendar))
    }

    /// An instant at the grain it prints at: the day for a midnight
    /// instant with no offset, the second otherwise.
    pub fn of_instant(secs: i64, nanos: u32, offset_min: Option<i16>) -> Period {
        let midnight = secs.rem_euclid(86400) == 0 && nanos == 0 && offset_min.is_none();
        Period {
            start: secs,
            grain: if midnight { Grain::Day } else { Grain::Second },
            calendar: Default::default(),
        }
    }

    /// Periods compare as intervals of the timeline. The grains
    /// nest, so two periods are disjoint (ordered by the timeline)
    /// or nested (equal).
    pub fn interval_cmp(&self, other: &Period) -> Ordering {
        if self.end() <= other.start {
            Ordering::Less
        } else if other.end() <= self.start {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// ISO-8601 at reduced precision, at the period's own grain. A
    /// month or a year of another calendar has no ISO form of its
    /// own and prints as the day it begins on (spec: Calendars).
    pub fn format(&self) -> String {
        if self.calendar != Default::default() && matches!(self.grain, Grain::Year | Grain::Month) {
            return Period {
                start: self.start,
                grain: Grain::Day,
                calendar: Default::default(),
            }
            .format();
        }
        let (y, mo, d, h, mi, s) = self.fields();
        let year = if y < 0 {
            format!("-{:04}", -y)
        } else {
            format!("{y:04}")
        };
        match self.grain {
            Grain::Year => year,
            Grain::Month => format!("{year}-{mo:02}"),
            Grain::Day => format!("{year}-{mo:02}-{d:02}"),
            Grain::Hour => format!("{year}-{mo:02}-{d:02}T{h:02}"),
            Grain::Minute => format!("{year}-{mo:02}-{d:02}T{h:02}:{mi:02}"),
            Grain::Second => format!("{year}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"),
        }
    }

    /// Parse ISO-8601 at reduced precision: `1812`, `2025-10`,
    /// `1899-10-06`, `1899-10-06T14`, `1899-10-06T14:30`,
    /// `1899-10-06T14:30:05` (a trailing `Z` allowed on the timed
    /// forms). A year is four digits or more, with an optional
    /// leading `-`.
    pub fn parse(text: &str) -> Option<Period> {
        let (date, time) = match text.split_once('T') {
            Some((d, t)) => (d, Some(t.strip_suffix('Z').unwrap_or(t))),
            None => (text, None),
        };
        let (neg, date) = match date.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, date),
        };
        let mut parts = date.split('-');
        let year = parts.next()?;
        if year.len() < 4 || year.len() > 6 {
            return None;
        }
        let y = digits(year)? as i64;
        let y = if neg { -y } else { y };
        let mo = parts.next();
        let d = parts.next();
        if parts.next().is_some() {
            return None;
        }
        let two = |p: &str| if p.len() == 2 { digits(p) } else { None };
        let (mo_n, d_n) = (
            match mo {
                Some(p) => Some(two(p)?),
                None => None,
            },
            match d {
                Some(p) => Some(two(p)?),
                None => None,
            },
        );
        let Some(time) = time else {
            return match (mo_n, d_n) {
                (None, _) => Period::of(y, 1, 1, 0, 0, 0, Grain::Year),
                (Some(mo), None) => Period::of(y, mo, 1, 0, 0, 0, Grain::Month),
                (Some(mo), Some(d)) => Period::of(y, mo, d, 0, 0, 0, Grain::Day),
            };
        };
        let (mo, d) = (mo_n?, d_n?);
        let (h, mi, s, grain) = parse_clock(time)?;
        Period::of(y, mo, d, h, mi, s, grain)
    }
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `HH`, `HH:MM` or `HH:MM:SS`, with the grain written.
fn parse_clock(time: &str) -> Option<(u32, u32, u32, Grain)> {
    let mut parts = time.split(':');
    let two = |p: Option<&str>| match p {
        Some(p) if p.len() == 2 => digits(p).map(Some),
        Some(_) => None,
        None => Some(None),
    };
    let h = two(parts.next())??;
    let mi = two(parts.next())?;
    let s = two(parts.next())?;
    if parts.next().is_some() || h > 23 || mi.is_some_and(|m| m > 59) || s.is_some_and(|s| s > 59) {
        return None;
    }
    Some(match (mi, s) {
        (None, _) => (h, 0, 0, Grain::Hour),
        (Some(mi), None) => (h, mi, 0, Grain::Minute),
        (Some(mi), Some(s)) => (h, mi, s, Grain::Second),
    })
}

/// A relative expression's offset, as `timex` writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offset {
    /// A signed ISO-8601 duration in calendar units (`+P1D`,
    /// `-P2M`, `+PT5M`): the fields are counts, the sign is one
    /// for all.
    Span {
        sign: i64,
        years: i64,
        months: i64,
        days: i64,
        hours: i64,
        minutes: i64,
        seconds: i64,
    },
    /// A signed ordinal and a weekday (`+1MO`: the first Monday
    /// after; `-1FR`: the last Friday before). `weekday` counts
    /// from Monday = 0.
    Weekday { nth: i64, weekday: u32 },
}

/// The RFC 5545 weekday codes, from Monday.
pub const WEEKDAY_CODES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

impl Offset {
    /// Parse `+P1D`, `-P1Y2M`, `+PT5M`, `+P2W`, `+1MO`, `-2FR`. The
    /// sign is required: a bare `P2W` is a duration, not an offset.
    pub fn parse(text: &str) -> Option<Offset> {
        let (sign, rest) = match text.as_bytes().first()? {
            b'+' => (1, &text[1..]),
            b'-' => (-1, &text[1..]),
            _ => return None,
        };
        if let Some(body) = rest.strip_prefix('P') {
            let (years, months, days, hours, minutes, seconds) = parse_span_fields(body)?;
            return Some(Offset::Span {
                sign,
                years,
                months,
                days,
                hours,
                minutes,
                seconds,
            });
        }
        let split = rest.find(|c: char| !c.is_ascii_digit())?;
        let (n, code) = rest.split_at(split);
        let nth = digits(n)? as i64;
        let weekday = WEEKDAY_CODES.iter().position(|c| *c == code)? as u32;
        if nth == 0 {
            return None;
        }
        Some(Offset::Weekday {
            nth: sign * nth,
            weekday,
        })
    }

    /// The finest calendar unit a span names (weeks count as days).
    fn finest(&self) -> Grain {
        match *self {
            Offset::Weekday { .. } => Grain::Day,
            Offset::Span {
                years,
                months,
                days,
                hours,
                minutes,
                seconds,
                ..
            } => {
                if seconds != 0 {
                    Grain::Second
                } else if minutes != 0 {
                    Grain::Minute
                } else if hours != 0 {
                    Grain::Hour
                } else if days != 0 {
                    Grain::Day
                } else if months != 0 {
                    Grain::Month
                } else {
                    let _ = years;
                    Grain::Year
                }
            }
        }
    }
}

/// The integer fields of an ISO-8601 duration body (after `P`):
/// `nY nM nW nD` and, after `T`, `nH nM nS`. Weeks fold into days.
/// At least one field is required.
fn parse_span_fields(body: &str) -> Option<(i64, i64, i64, i64, i64, i64)> {
    let (date, time) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    let mut out = [0i64; 6]; // Y M D h m s
    let mut any = false;
    let mut read = |part: &str, units: &[(char, usize, i64)]| -> Option<()> {
        let mut num = String::new();
        let mut next = 0; // units come in order, each at most once
        for c in part.chars() {
            if c.is_ascii_digit() {
                num.push(c);
                continue;
            }
            let at = units[next..].iter().position(|(u, ..)| *u == c)? + next;
            let (_, slot, scale) = units[at];
            let n: i64 = num.parse().ok()?;
            out[slot] = out[slot].checked_add(n.checked_mul(scale)?)?;
            num.clear();
            next = at + 1;
            any = true;
        }
        num.is_empty().then_some(())
    };
    read(date, &[('Y', 0, 1), ('M', 1, 1), ('W', 2, 7), ('D', 2, 1)])?;
    if let Some(time) = time {
        if time.is_empty() {
            return None;
        }
        read(time, &[('H', 3, 1), ('M', 4, 1), ('S', 5, 1)])?;
    }
    any.then_some((out[0], out[1], out[2], out[3], out[4], out[5]))
}

impl Period {
    /// Move by an offset (spec: Anchoring one expression). A span
    /// moves by calendar units and keeps the grain — except that a
    /// period finer than the day drops to the day when the offset
    /// names no unit finer than the day (`the next day` after
    /// 20:35 is a day: the text states no hour). A day the target
    /// month lacks becomes that month's last day. An offset finer
    /// than the grain has no result. A weekday offset needs the
    /// day or finer, and yields a day.
    pub fn shift(&self, offset: &Offset) -> Option<Period> {
        if offset.finest() > self.grain {
            return None;
        }
        let (y, mo, d, h, mi, s) = self.fields();
        match *offset {
            Offset::Weekday { nth, weekday } => {
                let day = self.start.div_euclid(86400);
                // Monday = 0, as the codes count.
                let today = (day + 3).rem_euclid(7);
                let target = weekday as i64;
                let day = if nth > 0 {
                    let ahead = (target - today).rem_euclid(7);
                    let ahead = if ahead == 0 { 7 } else { ahead };
                    day + ahead + (nth - 1) * 7
                } else {
                    let back = (today - target).rem_euclid(7);
                    let back = if back == 0 { 7 } else { back };
                    day - back - (-nth - 1) * 7
                };
                let (y, mo, d) = civil_from_days(day);
                Period::of(y, mo, d, 0, 0, 0, Grain::Day)
            }
            Offset::Span {
                sign,
                years,
                months,
                days,
                hours,
                minutes,
                seconds,
            } => {
                let total = (y * 12 + (mo as i64 - 1))
                    .checked_add(sign.checked_mul(years.checked_mul(12)?.checked_add(months)?)?)?;
                let (y, mo) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
                if !YEARS.contains(&y) {
                    return None;
                }
                let d = d.min(month_days(y, mo));
                let grain = if self.grain > Grain::Day && offset.finest() <= Grain::Day {
                    Grain::Day
                } else {
                    self.grain
                };
                let base = Period::of(y, mo, d, h, mi, s, grain)?;
                let delta = days
                    .checked_mul(86400)?
                    .checked_add(hours.checked_mul(3600)?)?
                    .checked_add(minutes.checked_mul(60)?)?
                    .checked_add(seconds)?;
                let start = base.start.checked_add(sign.checked_mul(delta)?)?;
                let moved = Period {
                    start,
                    grain,
                    calendar: base.calendar,
                };
                YEARS.contains(&moved.fields().0).then_some(moved)
            }
        }
    }

    /// This day at a time of day (`T06`, `T06:30`, `T06:30:15`): the
    /// day or finer is needed; the result has the grain written.
    pub fn at_time(&self, time: &str) -> Option<Period> {
        if self.grain < Grain::Day {
            return None;
        }
        let (h, mi, s, grain) = parse_clock(time.strip_prefix('T')?)?;
        let (y, mo, d, ..) = self.fields();
        Period::of(y, mo, d, h, mi, s, grain)
    }

    /// Complete a date with fields left out (`XXXX-04-08`,
    /// `XXXX-10`, `XXXX-XX-08`) from this period: the missing
    /// fields are this period's, which must carry them. With
    /// `forward`, the first such date at or after this period.
    pub fn complete(&self, partial: &str, forward: bool) -> Option<Period> {
        let mut parts = partial.split('-');
        let (year, mo, d) = (parts.next()?, parts.next(), parts.next());
        if parts.next().is_some() {
            return None;
        }
        let field = |p: &str, width: usize| -> Option<Option<u32>> {
            if p.len() != width {
                None
            } else if p.bytes().all(|b| b == b'X') {
                Some(None)
            } else {
                digits(p).map(Some)
            }
        };
        let year = field(year, 4)?;
        let mo = match mo {
            Some(p) => field(p, 2)?,
            None => return None, // a bare year leaves nothing out
        };
        let d = match d {
            Some(p) => Some(field(p, 2)??),
            None => None,
        };
        if year.is_some() && mo.is_some() {
            return None; // nothing left out: a complete date
        }
        let grain = if d.is_some() {
            Grain::Day
        } else {
            Grain::Month
        };
        if mo.is_none() && self.grain < Grain::Month {
            return None;
        }
        let (ay, amo, ..) = self.fields();
        let build = |y: i64, m: u32| Period::of(y, m, d.unwrap_or(1), 0, 0, 0, grain);
        let (mut y, mut m) = (year.map_or(ay, |y| y as i64), mo.unwrap_or(amo));
        if !forward {
            return build(y, m);
        }
        // The first such date at or after this period: step the
        // field the expression left out (the month where it names
        // none, else the year) until the date is not behind. A day
        // some months or years lack (the 31st, 29 February) is
        // looked for a little further.
        for _ in 0..(12 * 8) {
            if let Some(p) = build(y, m)
                && p.interval_cmp(self) != Ordering::Less
            {
                return Some(p);
            }
            if mo.is_none() {
                m += 1;
                if m > 12 {
                    m = 1;
                    if year.is_some() {
                        return None;
                    }
                    y += 1;
                }
            } else if year.is_none() {
                y += 1;
            } else {
                return None;
            }
        }
        None
    }
}

/// What a `timex` value text is, by its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A complete date: its own period.
    Complete(Period),
    /// A relative offset, a time of day, or a date with fields left
    /// out: valued against a point.
    Anchored,
    /// A duration, or text in none of the notations.
    None,
}

/// Classify a value text.
pub fn shape(value: &str) -> Shape {
    if Offset::parse(value).is_some() {
        return Shape::Anchored;
    }
    if let Some(t) = value.strip_prefix('T') {
        return if parse_clock(t).is_some() {
            Shape::Anchored
        } else {
            Shape::None
        };
    }
    if value.contains('X') {
        return Shape::Anchored;
    }
    match Period::parse(value) {
        Some(p) => Shape::Complete(p),
        None => Shape::None,
    }
}

/// Value a `timex` value text against an anchor (spec: Anchoring
/// one expression): a complete date passes through; an offset, a
/// time of day and a date with fields left out are read against
/// the anchor; a duration and anything else are `None`.
pub fn anchor(value: &str, anchor: Option<&Period>, forward: bool) -> Option<Period> {
    if let Some(offset) = Offset::parse(value) {
        return anchor?.shift(&offset);
    }
    if value.starts_with('T') {
        return anchor?.at_time(value);
    }
    if value.contains('X') {
        return anchor?.complete(value, forward);
    }
    Period::parse(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Period {
        Period::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn reduced_precision_round_trips() {
        for s in [
            "1812",
            "2025-10",
            "1899-10-06",
            "1899-10-06T14",
            "1899-10-06T14:30",
            "1899-10-06T14:30:05",
            "0476",
            "-0750",
        ] {
            assert_eq!(p(s).format(), s);
        }
        assert_eq!(p("1899-10-06T14:30Z").format(), "1899-10-06T14:30");
        for bad in [
            "18",
            "1899-13",
            "1899-02-29",
            "1899-10-6",
            "1899-10-06T24",
            "",
            "abc",
            "1899-10-06T",
        ] {
            assert!(Period::parse(bad).is_none(), "{bad}");
        }
        assert_eq!(p("1900-02").end() - p("1900-02").start, 28 * 86400);
        assert_eq!(p("2000-02").end() - p("2000-02").start, 29 * 86400);
    }

    #[test]
    fn periods_are_disjoint_or_nested() {
        use Ordering::*;
        assert_eq!(p("2025-10").interval_cmp(&p("2025-10-06")), Equal);
        assert_eq!(p("2025-10-06").interval_cmp(&p("2025-10")), Equal);
        assert_eq!(p("2025-10-06").interval_cmp(&p("2025-10-07")), Less);
        assert_eq!(p("2025").interval_cmp(&p("2024-12-31T23:59")), Greater);
        assert_eq!(p("2025-10-06T14:30").at(Grain::Month), Some(p("2025-10")));
        assert_eq!(p("2025-10").at(Grain::Day), None);
    }

    #[test]
    fn offsets() {
        let a = |value: &str, at: &str| anchor(value, Some(&p(at)), false).map(|r| r.format());
        // a span moves by calendar units and keeps the grain
        assert_eq!(a("+P2M", "1899-10-06").as_deref(), Some("1899-12-06"));
        assert_eq!(a("+P1D", "1899-10-06").as_deref(), Some("1899-10-07"));
        assert_eq!(a("-P2W", "1899-10-06").as_deref(), Some("1899-09-22"));
        assert_eq!(a("+P1Y", "2025-10").as_deref(), Some("2026-10"));
        assert_eq!(a("+P3M", "2025-11").as_deref(), Some("2026-02"));
        assert_eq!(a("-P1Y", "1812").as_deref(), Some("1811"));
        assert_eq!(
            a("+PT5M", "1899-10-06T23:58").as_deref(),
            Some("1899-10-07T00:03")
        );
        // finer than the day, the anchor drops to the day unless the
        // offset names a finer unit
        assert_eq!(a("+P1D", "1897-05-03T20:35").as_deref(), Some("1897-05-04"));
        assert_eq!(a("+P2M", "1897-05-03T20:35").as_deref(), Some("1897-07-03"));
        assert_eq!(a("+P0D", "1897-05-03T20:35").as_deref(), Some("1897-05-03"));
        assert_eq!(
            a("+PT1H", "1897-05-03T20:35").as_deref(),
            Some("1897-05-03T21:35")
        );
        assert_eq!(
            a("+P1DT1H", "1897-05-03T20:35").as_deref(),
            Some("1897-05-04T21:35")
        );
        // the month's last day where the target month lacks the day
        assert_eq!(a("+P1M", "1899-01-31").as_deref(), Some("1899-02-28"));
        assert_eq!(a("+P1M", "1900-01-31").as_deref(), Some("1900-02-28"));
        assert_eq!(a("+P1M", "2000-01-31").as_deref(), Some("2000-02-29"));
        // an offset finer than the anchor has no result
        assert_eq!(a("+P1D", "1812"), None);
        assert_eq!(a("+PT1H", "1899-10-06"), None);
        assert_eq!(a("+P1M", "1812"), None);
        // weekdays: 1899-10-06 was a Friday
        assert_eq!(a("+1MO", "1899-10-06").as_deref(), Some("1899-10-09"));
        assert_eq!(a("+1FR", "1899-10-06").as_deref(), Some("1899-10-13"));
        assert_eq!(a("-1FR", "1899-10-06").as_deref(), Some("1899-09-29"));
        assert_eq!(a("-1TH", "1899-10-06").as_deref(), Some("1899-10-05"));
        assert_eq!(a("+2MO", "1899-10-06T14:30").as_deref(), Some("1899-10-16"));
        assert_eq!(a("+1MO", "1899-10"), None);
        // a time of day
        assert_eq!(
            a("T06:30", "1899-10-06").as_deref(),
            Some("1899-10-06T06:30")
        );
        assert_eq!(
            a("T18", "1899-10-06T09:00").as_deref(),
            Some("1899-10-06T18")
        );
        assert_eq!(a("T06:30", "1899-10"), None);
        // a complete date passes through; a duration is nothing
        assert_eq!(a("1846-04-08", "1899-10-06").as_deref(), Some("1846-04-08"));
        assert_eq!(a("P2W", "1899-10-06"), None);
        assert_eq!(anchor("+P1D", None, false), None);
        assert_eq!(
            anchor("1812", None, false).map(|r| r.format()).as_deref(),
            Some("1812")
        );
        assert!(Offset::parse("P1D").is_none() && Offset::parse("+0MO").is_none());
        assert!(Offset::parse("+P").is_none() && Offset::parse("+P1DT").is_none());
        assert!(Offset::parse("+P1M1Y").is_none());
    }

    #[test]
    fn fields_left_out() {
        let c =
            |value: &str, at: &str, fwd: bool| anchor(value, Some(&p(at)), fwd).map(|r| r.format());
        assert_eq!(
            c("XXXX-04-08", "1846-03-20", false).as_deref(),
            Some("1846-04-08")
        );
        assert_eq!(c("XXXX-10", "1899", false).as_deref(), Some("1899-10"));
        assert_eq!(
            c("XXXX-XX-08", "1846-03-20", false).as_deref(),
            Some("1846-03-08")
        );
        assert_eq!(c("XXXX-XX-08", "1846", false), None);
        // forward: the first such date at or after the point
        assert_eq!(
            c("XXXX-04-08", "1846-12-20", true).as_deref(),
            Some("1847-04-08")
        );
        assert_eq!(
            c("XXXX-04-08", "1846-04-08", true).as_deref(),
            Some("1846-04-08")
        );
        assert_eq!(
            c("XXXX-04-08", "1846-12-20", false).as_deref(),
            Some("1846-04-08")
        );
        assert_eq!(
            c("XXXX-XX-08", "1846-12-20", true).as_deref(),
            Some("1847-01-08")
        );
        assert_eq!(
            c("XXXX-02-29", "1897-03-01", true).as_deref(),
            Some("1904-02-29")
        );
        assert_eq!(
            c("XXXX-XX-31", "1899-04-02", true).as_deref(),
            Some("1899-05-31")
        );
        assert_eq!(c("XXXX-02-30", "1899-01-01", true), None);
        assert_eq!(shape("XXXX-04-08"), Shape::Anchored);
        assert_eq!(shape("P2W"), Shape::None);
        assert_eq!(shape("1812"), Shape::Complete(p("1812")));
    }
}
