//! The temporal fragment: ISO-8601 parsing and formatting, and the
//! civil-calendar arithmetic behind `Value::Instant` and
//! `Value::Duration`.
//!
//! An instant is a point on the UTC timeline (seconds + nanoseconds
//! since the epoch); a source's UTC offset is preserved for display
//! only and never affects comparison. Everything here is hand-rolled
//! (Hinnant's civil-calendar algorithms) — no clock is ever read, so
//! queries stay deterministic.

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp as i64 + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Civil date (year, month, day) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The civil components of an instant: (year, month, day, hour,
/// minute, second), in UTC.
pub fn components(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

/// ISO weekday, 1 = Monday .. 7 = Sunday.
pub fn weekday(secs: i64) -> u32 {
    let days = secs.div_euclid(86400);
    ((days + 3).rem_euclid(7) + 1) as u32
}

/// Days in civil month `m` (1..=12) of year `y`, proleptic
/// Gregorian; February follows the module's own leap rule. Used to
/// reject an out-of-range day before it silently normalizes onto a
/// different date. Returns 0 for an out-of-range month.
fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = days_from_civil(y + 1, 1, 1) - days_from_civil(y, 1, 1) == 366;
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

/// Parse ISO-8601: `YYYY-MM-DD`, optionally `T` (or space) and
/// `HH:MM[:SS[.frac]]`, optionally `Z` or `±HH[:]MM`. Returns UTC
/// (secs, nanos) and the source offset in minutes where one was
/// written.
pub fn parse_iso(s: &str) -> Option<(i64, u32, Option<i16>)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86400;
    let mut nanos = 0u32;
    let mut offset: Option<i16> = None;
    let rest = &s[10..];
    if !rest.is_empty() {
        let rb = rest.as_bytes();
        if rb[0] != b'T' && rb[0] != b' ' {
            return None;
        }
        let t = &rest[1..];
        if t.len() < 5 || t.as_bytes()[2] != b':' {
            return None;
        }
        let hour: i64 = t.get(0..2)?.parse().ok()?;
        let minute: i64 = t.get(3..5)?.parse().ok()?;
        if hour > 23 || minute > 59 {
            return None;
        }
        secs += hour * 3600 + minute * 60;
        let mut i = 5;
        if t.len() >= 8 && t.as_bytes()[5] == b':' {
            let second: i64 = t.get(6..8)?.parse().ok()?;
            if second > 60 {
                return None;
            }
            secs += second;
            i = 8;
            if t.len() > i && t.as_bytes()[i] == b'.' {
                let frac_start = i + 1;
                let mut frac_end = frac_start;
                while frac_end < t.len() && t.as_bytes()[frac_end].is_ascii_digit() {
                    frac_end += 1;
                }
                if frac_end == frac_start {
                    return None;
                }
                let digits = &t[frac_start..frac_end.min(frac_start + 9)];
                let mut n: u32 = digits.parse().ok()?;
                for _ in digits.len()..9 {
                    n *= 10;
                }
                nanos = n;
                i = frac_end;
            }
        }
        let tz = &t[i..];
        if !tz.is_empty() {
            if tz == "Z" || tz == "z" {
                offset = Some(0);
            } else {
                let sign = match tz.as_bytes()[0] {
                    b'+' => 1i32,
                    b'-' => -1i32,
                    _ => return None,
                };
                let body = &tz[1..];
                let (oh, om) = match body.len() {
                    2 => (body.parse::<i32>().ok()?, 0),
                    4 => (
                        body.get(0..2)?.parse::<i32>().ok()?,
                        body.get(2..4)?.parse::<i32>().ok()?,
                    ),
                    5 if body.as_bytes()[2] == b':' => (
                        body.get(0..2)?.parse::<i32>().ok()?,
                        body.get(3..5)?.parse::<i32>().ok()?,
                    ),
                    _ => return None,
                };
                if oh > 23 || om > 59 {
                    return None;
                }
                let minutes = sign * (oh * 60 + om);
                secs -= minutes as i64 * 60; // normalize to UTC
                offset = Some(minutes as i16);
            }
        }
    }
    Some((secs, nanos, offset))
}

/// Format an instant as ISO-8601. The preserved offset shifts the
/// displayed wall clock and prints as `±HH:MM` (`Z` for zero); with
/// no offset the instant prints in UTC without a designator, and a
/// midnight instant with no sub-day parts prints as a bare date.
pub fn format_instant(secs: i64, nanos: u32, offset: Option<i16>) -> String {
    let shift = offset.unwrap_or(0) as i64 * 60;
    // Saturating: display must not abort on an instant minted near
    // the i64 rim (adapters can hand us any epoch).
    let local = secs.saturating_add(shift);
    let (y, mo, d, h, mi, s) = components(local);
    let date = format!("{y:04}-{mo:02}-{d:02}");
    let subday = h != 0 || mi != 0 || s != 0 || nanos != 0 || offset.is_some();
    if !subday {
        return date;
    }
    let mut out = format!("{date}T{h:02}:{mi:02}:{s:02}");
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    match offset {
        None => {}
        Some(0) => out.push('Z'),
        Some(m) => {
            let sign = if m < 0 { '-' } else { '+' };
            let m = m.unsigned_abs();
            out.push_str(&format!("{sign}{:02}:{:02}", m / 60, m % 60));
        }
    }
    out
}

/// Format a duration as ISO-8601: `P{d}DT{h}H{m}M{s}S`, zero parts
/// omitted, `PT0S` for zero, a leading `-` for negative durations.
pub fn format_duration(secs: i64, nanos: u32) -> String {
    let neg = secs < 0;
    // Compute the magnitude in i128 so `secs == i64::MIN` cannot
    // overflow (negating or abs-ing it does not fit in i64).
    let (mut s, n): (i128, u32) = if secs < 0 && nanos > 0 {
        (-(secs as i128) - 1, 1_000_000_000 - nanos)
    } else {
        ((secs as i128).abs(), nanos)
    };
    let days = s / 86400;
    s %= 86400;
    let hours = s / 3600;
    s %= 3600;
    let minutes = s / 60;
    s %= 60;
    let mut out = String::from(if neg { "-P" } else { "P" });
    if days != 0 {
        out.push_str(&format!("{days}D"));
    }
    let frac = if n != 0 {
        let f = format!("{n:09}");
        format!(".{}", f.trim_end_matches('0'))
    } else {
        String::new()
    };
    if hours != 0 || minutes != 0 || s != 0 || n != 0 || days == 0 {
        out.push('T');
        if hours != 0 {
            out.push_str(&format!("{hours}H"));
        }
        if minutes != 0 {
            out.push_str(&format!("{minutes}M"));
        }
        if s != 0 || n != 0 || (hours == 0 && minutes == 0 && days == 0) {
            out.push_str(&format!("{s}{frac}S"));
        }
    }
    out
}

const NS: i128 = 1_000_000_000;

/// Scale a decimal component (`5`, `1.5`) by a nanosecond unit,
/// exactly for the integer part, truncating the fraction below a
/// nanosecond.
fn scale_component(num: &str, ns_per: i128) -> Option<i128> {
    let (int_s, frac_s) = match num.split_once('.') {
        Some((a, b)) => (a, b),
        None => (num, ""),
    };
    if int_s.is_empty() && frac_s.is_empty() {
        return None;
    }
    let int_v: i128 = if int_s.is_empty() {
        0
    } else {
        int_s.parse().ok()?
    };
    let mut total = int_v.checked_mul(ns_per)?;
    if !frac_s.is_empty() {
        if !frac_s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits = &frac_s[..frac_s.len().min(18)];
        let val: i128 = digits.parse().ok()?;
        let denom = 10i128.checked_pow(digits.len() as u32)?;
        total = total.checked_add(val.checked_mul(ns_per)? / denom)?;
    }
    Some(total)
}

/// An ISO-8601 duration body (`P5DT3H5M`, `PT0.5S`, `P2W`), as
/// total nanoseconds. Months and years are refused — calendar
/// notions, not fixed spans.
fn parse_iso_duration(s: &str) -> Option<i128> {
    let b = s.as_bytes();
    if b.is_empty() || (b[0] != b'P' && b[0] != b'p') {
        return None;
    }
    let mut i = 1;
    let mut in_time = false;
    let mut total: i128 = 0;
    let mut any = false;
    while i < b.len() {
        if b[i] == b'T' || b[i] == b't' {
            if in_time {
                return None;
            }
            in_time = true;
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            i += 1;
        }
        if i == start || i >= b.len() {
            return None;
        }
        let num = &s[start..i];
        let unit = b[i].to_ascii_uppercase();
        i += 1;
        let ns_per: i128 = match (unit, in_time) {
            (b'W', false) => 604_800 * NS,
            (b'D', false) => 86_400 * NS,
            (b'H', true) => 3_600 * NS,
            (b'M', true) => 60 * NS,
            (b'S', true) => NS,
            _ => return None,
        };
        total = total.checked_add(scale_component(num, ns_per)?)?;
        any = true;
    }
    if !any {
        return None;
    }
    Some(total)
}

/// A systemd time-span body (`5d 3h 5min`, `5d3h5min`, `1.5h`), as
/// total nanoseconds: decimal components glued to unit names,
/// whitespace between components optional, summing in any order.
/// Units (with long forms): ns us ms s m h d w — no months or
/// years (calendar notions), and no default unit (a bare number is
/// not a span).
fn parse_systemd_span(s: &str) -> Option<i128> {
    let mut rest = s;
    let mut total: i128 = 0;
    let mut any = false;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let num = &rest[..num_end];
        rest = &rest[num_end..];
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        if unit_end == 0 {
            return None; // a bare number has no default unit
        }
        let unit = &rest[..unit_end];
        rest = rest[unit_end..].trim_start();
        let ns_per: i128 = match unit {
            "ns" | "nsec" => 1,
            "us" | "usec" => 1_000,
            "ms" | "msec" => 1_000_000,
            "s" | "sec" | "second" | "seconds" => NS,
            // SI discipline: bare `m` is meters (the quantital
            // fragment), never minutes — `min` is the only short
            // minute unit (2026-07-12 ruling).
            "min" | "minute" | "minutes" => 60 * NS,
            "h" | "hr" | "hour" | "hours" => 3_600 * NS,
            "d" | "day" | "days" => 86_400 * NS,
            "w" | "week" | "weeks" => 604_800 * NS,
            _ => return None,
        };
        total = total.checked_add(scale_component(num, ns_per)?)?;
        any = true;
    }
    if !any {
        return None;
    }
    Some(total)
}

/// Parse span text (spec: The Durational Reading): an ISO-8601
/// duration (with a leading `-` accepted, so [`format_duration`]
/// output round-trips) or a systemd time-span. Returns
/// (secs, nanos) with nanos normalized into [0, 1e9).
/// Span text through a *unit table*: a magnitude glued to (or
/// spaced from) a unit expression the adapter's registry knows —
/// `5rep`, `0.4jaj`, `2fortnight` — reads as a span iff the unit's
/// base dimension is bare seconds. This is the durational
/// counterpart of quantity criteria paying custom units: the
/// builtin span grammar is consulted first by every caller, so
/// `min`/`h`/`d` keep their engine meaning, and a length like `5m`
/// resolves to meters (base `m`), fails the seconds check, and
/// refuses rather than comparing nonsense.
pub(crate) fn span_from_units(
    s: &str,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<(i64, u32)> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let num = num.trim();
    let mag: f64 = if num.is_empty() {
        1.0
    } else {
        num.parse().ok()?
    };
    let (factor, base) = scale(unit.trim())?;
    if base != "s" {
        return None;
    }
    let total = mag * factor;
    if !(total.is_finite() && (0.0..=i64::MAX as f64).contains(&total)) {
        return None;
    }
    let secs = total.floor() as i64;
    let nanos = ((total - total.floor()) * 1e9) as u32;
    Some((secs, nanos))
}

pub fn parse_span(s: &str) -> Option<(i64, u32)> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, s),
    };
    let total = if body.starts_with('P') || body.starts_with('p') {
        parse_iso_duration(body)?
    } else {
        if neg {
            return None; // the systemd form carries no sign
        }
        parse_systemd_span(body)?
    };
    let total = if neg { -total } else { total };
    let secs = i64::try_from(total.div_euclid(NS)).ok()?;
    let nanos = total.rem_euclid(NS) as u32;
    Some((secs, nanos))
}

/// The ISO-8601 week date: (week-numbering year, week). The week
/// containing the year's first Thursday is week 1.
pub fn iso_week(secs: i64) -> (i64, u32) {
    let days = secs.div_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let doy = (days - days_from_civil(y, 1, 1) + 1) as i64; // 1-based
    let wd = weekday(secs) as i64; // 1 = Monday
    let week = (doy - wd + 10) / 7;
    let weeks_in = |year: i64| -> i64 {
        let jan1 = days_from_civil(year, 1, 1);
        let jan1_wd = ((jan1 + 3).rem_euclid(7)) + 1;
        let leap = days_from_civil(year + 1, 1, 1) - jan1 == 366;
        if jan1_wd == 4 || (leap && jan1_wd == 3) {
            53
        } else {
            52
        }
    };
    let _ = (m, d);
    if week < 1 {
        (y - 1, weeks_in(y - 1) as u32)
    } else if week > weeks_in(y) {
        (y + 1, 1)
    } else {
        (y, week as u32)
    }
}

const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `strftime`-style formatting (the C/POSIX standard Perl, Python,
/// and Ruby share), in the instant's own offset (UTC when none).
/// Supported: %Y %y %C %m %d %e %H %I %p %M %S %j %a %A %b %B %u %w
/// %V %G %g %F %T %R %D %s %z %Z %n %t %%. An unknown specifier
/// passes through literally.
pub fn strftime(fmt: &str, secs: i64, nanos: u32, offset_min: Option<i16>) -> String {
    let _ = nanos;
    let local = secs.saturating_add(offset_min.unwrap_or(0) as i64 * 60);
    let (y, mo, d, h, mi, se) = components(local);
    let wd = weekday(local); // 1 = Monday
    let doy = local.div_euclid(86400) - days_from_civil(y, 1, 1) + 1;
    let (gy, gw) = iso_week(local);
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{y:04}")),
            Some('y') => out.push_str(&format!("{:02}", y.rem_euclid(100))),
            Some('C') => out.push_str(&format!("{:02}", y.div_euclid(100))),
            Some('m') => out.push_str(&format!("{mo:02}")),
            Some('d') => out.push_str(&format!("{d:02}")),
            Some('e') => out.push_str(&format!("{d:2}")),
            Some('H') => out.push_str(&format!("{h:02}")),
            Some('I') => {
                let h12 = if h % 12 == 0 { 12 } else { h % 12 };
                out.push_str(&format!("{h12:02}"));
            }
            Some('p') => out.push_str(if h < 12 { "AM" } else { "PM" }),
            Some('M') => out.push_str(&format!("{mi:02}")),
            Some('S') => out.push_str(&format!("{se:02}")),
            Some('j') => out.push_str(&format!("{doy:03}")),
            Some('a') => out.push_str(&DAYS[(wd - 1) as usize][..3]),
            Some('A') => out.push_str(DAYS[(wd - 1) as usize]),
            Some('b') => out.push_str(&MONTHS[(mo - 1) as usize][..3]),
            Some('B') => out.push_str(MONTHS[(mo - 1) as usize]),
            Some('u') => out.push_str(&wd.to_string()),
            Some('w') => out.push_str(&(wd % 7).to_string()),
            Some('V') => out.push_str(&format!("{gw:02}")),
            Some('G') => out.push_str(&format!("{gy:04}")),
            Some('g') => out.push_str(&format!("{:02}", gy.rem_euclid(100))),
            Some('F') => out.push_str(&format!("{y:04}-{mo:02}-{d:02}")),
            Some('T') => out.push_str(&format!("{h:02}:{mi:02}:{se:02}")),
            Some('R') => out.push_str(&format!("{h:02}:{mi:02}")),
            Some('D') => out.push_str(&format!("{mo:02}/{d:02}/{:02}", y.rem_euclid(100))),
            Some('s') => out.push_str(&secs.to_string()),
            Some('z') => {
                let m = offset_min.unwrap_or(0);
                let sign = if m < 0 { '-' } else { '+' };
                let m = m.unsigned_abs();
                out.push_str(&format!("{sign}{:02}{:02}", m / 60, m % 60));
            }
            Some('Z') => out.push_str(match offset_min {
                None | Some(0) => "UTC",
                _ => "",
            }),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// `strptime`-style parsing — [`strftime`]'s inverse, sharing its
/// C/POSIX specifier standard and fixed English names (no locale
/// is read). Fields the format does not carry default to the Unix
/// epoch's (1970-01-01T00:00:00); a parsed `%z` offset is kept for
/// display and a format without one reads as UTC; the entire input
/// must match. Parsing specifiers: %Y %y %C %m %d %e %H %I %p %M
/// %S %a %A %b %B %F %T %R %D %s %z %n %t %%; whitespace in the
/// format matches a run of input whitespace.
pub fn strptime(input: &str, fmt: &str) -> Option<(i64, u32, Option<i16>)> {
    let inb = input.as_bytes();
    let mut i = 0usize; // input cursor
    let (mut year, mut mo, mut d) = (1970i64, 1u32, 1u32);
    let (mut h, mut mi, mut se) = (0u32, 0u32, 0u32);
    let mut century: Option<i64> = None;
    let mut h12: Option<u32> = None;
    let mut pm: Option<bool> = None;
    let mut offset: Option<i16> = None;
    let mut epoch_direct: Option<i64> = None;

    // Parse 1..=max digits (a leading space tolerated when `pad`).
    fn digits(b: &[u8], i: &mut usize, max: usize, pad: bool) -> Option<i64> {
        if pad && *i < b.len() && b[*i] == b' ' {
            *i += 1;
        }
        let start = *i;
        while *i < b.len() && *i - start < max && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start {
            return None;
        }
        std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
    }
    // Case-insensitive fixed-name match: full form first, then the
    // three-letter abbreviation. Returns the 0-based index.
    fn name_match(b: &[u8], i: &mut usize, names: &[&str]) -> Option<usize> {
        // Compare on raw bytes (all names are ASCII): a match can then
        // never slice the input inside a multibyte char, and a name
        // shorter than three bytes (`AM`/`PM`) can never over-index.
        let rest = &b[*i..];
        for (k, name) in names.iter().enumerate() {
            let nb = name.as_bytes();
            if rest.len() >= nb.len() && rest[..nb.len()].eq_ignore_ascii_case(nb) {
                *i += nb.len();
                return Some(k);
            }
        }
        for (k, name) in names.iter().enumerate() {
            let nb = name.as_bytes();
            let alen = nb.len().min(3);
            if rest.len() >= alen && rest[..alen].eq_ignore_ascii_case(&nb[..alen]) {
                *i += alen;
                return Some(k);
            }
        }
        None
    }

    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            // Format whitespace matches a run of input whitespace.
            while i < inb.len() && (inb[i] as char).is_whitespace() {
                i += 1;
            }
            continue;
        }
        if c != '%' {
            // A format literal matches its own (possibly multibyte)
            // char, not a single input byte reinterpreted as a char.
            // `i` is always kept on a char boundary, so this slice is
            // safe.
            if input[i..].starts_with(c) {
                i += c.len_utf8();
                continue;
            }
            return None;
        }
        match chars.next() {
            Some('Y') => year = digits(inb, &mut i, 4, false)?,
            Some('y') => {
                let v = digits(inb, &mut i, 2, false)?;
                year = if v >= 69 { 1900 + v } else { 2000 + v };
            }
            Some('C') => century = Some(digits(inb, &mut i, 2, false)?),
            Some('m') => mo = digits(inb, &mut i, 2, false)? as u32,
            Some('d') => d = digits(inb, &mut i, 2, false)? as u32,
            Some('e') => d = digits(inb, &mut i, 2, true)? as u32,
            Some('H') => h = digits(inb, &mut i, 2, false)? as u32,
            Some('I') => h12 = Some(digits(inb, &mut i, 2, false)? as u32),
            Some('p') => {
                let k = name_match(inb, &mut i, &["AM", "PM"])?;
                pm = Some(k == 1);
            }
            Some('M') => mi = digits(inb, &mut i, 2, false)? as u32,
            Some('S') => se = digits(inb, &mut i, 2, false)? as u32,
            Some('a') | Some('A') => {
                // Parsed and discarded: the date fields decide.
                name_match(inb, &mut i, &DAYS)?;
            }
            Some('b') | Some('B') => mo = name_match(inb, &mut i, &MONTHS)? as u32 + 1,
            Some('F') => {
                year = digits(inb, &mut i, 4, false)?;
                if i >= inb.len() || inb[i] != b'-' {
                    return None;
                }
                i += 1;
                mo = digits(inb, &mut i, 2, false)? as u32;
                if i >= inb.len() || inb[i] != b'-' {
                    return None;
                }
                i += 1;
                d = digits(inb, &mut i, 2, false)? as u32;
            }
            Some(sp @ ('T' | 'R')) => {
                h = digits(inb, &mut i, 2, false)? as u32;
                if i >= inb.len() || inb[i] != b':' {
                    return None;
                }
                i += 1;
                mi = digits(inb, &mut i, 2, false)? as u32;
                // %T carries seconds; %R stops at minutes.
                if sp == 'T' {
                    if i >= inb.len() || inb[i] != b':' {
                        return None;
                    }
                    i += 1;
                    se = digits(inb, &mut i, 2, false)? as u32;
                }
            }
            Some('D') => {
                mo = digits(inb, &mut i, 2, false)? as u32;
                if i >= inb.len() || inb[i] != b'/' {
                    return None;
                }
                i += 1;
                d = digits(inb, &mut i, 2, false)? as u32;
                if i >= inb.len() || inb[i] != b'/' {
                    return None;
                }
                i += 1;
                let v = digits(inb, &mut i, 2, false)?;
                year = if v >= 69 { 1900 + v } else { 2000 + v };
            }
            Some('s') => {
                let neg = i < inb.len() && inb[i] == b'-';
                if neg {
                    i += 1;
                }
                let v = digits(inb, &mut i, 19, false)?;
                epoch_direct = Some(if neg { -v } else { v });
            }
            Some('z') => {
                if i < inb.len() && (inb[i] == b'Z' || inb[i] == b'z') {
                    i += 1;
                    offset = Some(0);
                } else {
                    let sign = match inb.get(i) {
                        Some(b'+') => 1i32,
                        Some(b'-') => -1i32,
                        _ => return None,
                    };
                    i += 1;
                    let oh = digits(inb, &mut i, 2, false)? as i32;
                    if i < inb.len() && inb[i] == b':' {
                        i += 1;
                    }
                    let om = digits(inb, &mut i, 2, false)? as i32;
                    if oh > 23 || om > 59 {
                        return None;
                    }
                    offset = Some((sign * (oh * 60 + om)) as i16);
                }
            }
            Some('n') | Some('t') => {
                while i < inb.len() && (inb[i] as char).is_whitespace() {
                    i += 1;
                }
            }
            Some('%') => {
                if i < inb.len() && inb[i] == b'%' {
                    i += 1;
                } else {
                    return None;
                }
            }
            _ => return None, // unknown specifier: no parse
        }
    }
    // Trailing input beyond the format is a non-match.
    while i < inb.len() && (inb[i] as char).is_whitespace() {
        i += 1;
    }
    if i != inb.len() {
        return None;
    }

    if let Some(secs) = epoch_direct {
        // Bound %s to the civil years 0000-9999 (what the other
        // mint routes' four-digit years already imply): an epoch
        // near i64::MAX would overflow display-offset arithmetic,
        // and a nineteen-digit "timestamp" is junk, not a reading.
        if !(-62_167_219_200..=253_402_300_799).contains(&secs) {
            return None;
        }
        return Some((secs, 0, offset));
    }
    if let Some(c) = century {
        year = c * 100 + year.rem_euclid(100);
    }
    if let Some(hh) = h12 {
        if !(1..=12).contains(&hh) {
            return None;
        }
        h = match pm {
            Some(true) => hh % 12 + 12,
            Some(false) => hh % 12,
            None => hh,
        };
    }
    if !(1..=12).contains(&mo)
        || d < 1
        || d > days_in_month(year, mo)
        || h > 23
        || mi > 59
        || se > 60
    {
        return None;
    }
    let mut secs =
        days_from_civil(year, mo, d) * 86400 + h as i64 * 3600 + mi as i64 * 60 + se as i64;
    if let Some(m) = offset {
        secs -= m as i64 * 60;
    }
    Some((secs, 0, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for &(y, m, d) in &[(1970, 1, 1), (2024, 2, 29), (1969, 12, 31), (2026, 7, 11)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn iso_parse_and_format() {
        assert_eq!(parse_iso("2024-02-15"), Some((1707955200, 0, None)));
        assert_eq!(
            parse_iso("2024-02-15T13:26:40Z"),
            Some((1708003600, 0, Some(0)))
        );
        assert_eq!(
            parse_iso("2024-02-15T14:26:40+01:00"),
            Some((1708003600, 0, Some(60)))
        );
        assert_eq!(format_instant(1707955200, 0, None), "2024-02-15");
        assert_eq!(
            format_instant(1708003600, 0, Some(0)),
            "2024-02-15T13:26:40Z"
        );
        assert_eq!(
            format_instant(1708003600, 0, Some(60)),
            "2024-02-15T14:26:40+01:00"
        );
        assert_eq!(parse_iso("not a date"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0, 0), "PT0S");
        assert_eq!(format_duration(86400 + 3661, 0), "P1DT1H1M1S");
        assert_eq!(format_duration(-90, 0), "-PT1M30S");
        assert_eq!(format_duration(2592000, 0), "P30D");
    }

    #[test]
    fn iso_weeks() {
        // 2024-01-01 is a Monday: week 1 of 2024.
        let (s, _, _) = parse_iso("2024-01-01").unwrap();
        assert_eq!(iso_week(s), (2024, 1));
        // 2023-01-01 is a Sunday: it belongs to 2022-W52.
        let (s, _, _) = parse_iso("2023-01-01").unwrap();
        assert_eq!(iso_week(s), (2022, 52));
        // 2020-12-31 falls in 2020-W53 (a 53-week year).
        let (s, _, _) = parse_iso("2020-12-31").unwrap();
        assert_eq!(iso_week(s), (2020, 53));
    }

    #[test]
    fn strftime_specifiers() {
        let (s, n, o) = parse_iso("2024-02-15T13:26:40Z").unwrap();
        assert_eq!(
            strftime("%Y-%m-%d %H:%M:%S", s, n, o),
            "2024-02-15 13:26:40"
        );
        assert_eq!(
            strftime("%A, %B %e (%a %b)", s, n, o),
            "Thursday, February 15 (Thu Feb)"
        );
        assert_eq!(strftime("%G-w%V day %u", s, n, o), "2024-w07 day 4");
        assert_eq!(strftime("%I:%M %p %z %Z", s, n, o), "01:26 PM +0000 UTC");
        assert_eq!(strftime("100%% %q", s, n, o), "100% %q");
        let (s, n, o) = parse_iso("2024-02-15T14:26:40+01:00").unwrap();
        assert_eq!(strftime("%T %z", s, n, o), "14:26:40 +0100");
    }

    #[test]
    fn span_from_unit_table() {
        // A stand-in registry: the Klingon hour and day, and a
        // length unit that must NOT read as a span.
        let scale = |u: &str| -> Option<(f64, String)> {
            match u {
                "rep" => Some((4500.0, "s".into())),
                "jaj" => Some((108000.0, "s".into())),
                "kellicam" => Some((2000.0, "m".into())),
                _ => None,
            }
        };
        assert_eq!(span_from_units("5rep", &scale), Some((22500, 0)));
        assert_eq!(span_from_units("0.5jaj", &scale), Some((54000, 0)));
        assert_eq!(span_from_units("jaj", &scale), Some((108000, 0)));
        // Wrong dimension refuses; unknown unit refuses.
        assert_eq!(span_from_units("3kellicam", &scale), None);
        assert_eq!(span_from_units("3glorp", &scale), None);
        // Negative spans refuse, like builtin span text.
        assert_eq!(span_from_units("-1rep", &scale), None);
    }

    #[test]
    fn span_parsing() {
        // systemd form: glued, spaced, fractional, any order.
        assert_eq!(parse_span("12h"), Some((43200, 0)));
        assert_eq!(parse_span("5d3h5min"), Some((443100, 0)));
        assert_eq!(parse_span("5d 3h 5min"), Some((443100, 0)));
        assert_eq!(parse_span("1.5h"), Some((5400, 0)));
        assert_eq!(parse_span("5min3h5d"), Some((443100, 0)));
        assert_eq!(parse_span("2w"), Some((1209600, 0)));
        assert_eq!(parse_span("250ms"), Some((0, 250_000_000)));
        assert_eq!(parse_span("1s500ms"), Some((1, 500_000_000)));
        // ISO-8601 form, round-tripping format_duration.
        assert_eq!(parse_span("P5DT3H5M"), Some((443100, 0)));
        assert_eq!(parse_span("PT0S"), Some((0, 0)));
        assert_eq!(parse_span("P2W"), Some((1209600, 0)));
        assert_eq!(parse_span("PT0.5S"), Some((0, 500_000_000)));
        assert_eq!(parse_span("-PT1M30S"), Some((-90, 0)));
        for secs in [0i64, 90, 443100, 2592000] {
            let text = format_duration(secs, 0);
            assert_eq!(parse_span(&text), Some((secs, 0)), "{text}");
        }
        // No default unit, no months/years, no sign on spans.
        assert_eq!(parse_span("300"), None);
        assert_eq!(parse_span("5M"), None);
        assert_eq!(parse_span("1y"), None);
        assert_eq!(parse_span("P1M"), None);
        assert_eq!(parse_span("P1Y"), None);
        assert_eq!(parse_span("-5d"), None);
        assert_eq!(parse_span(""), None);
        assert_eq!(parse_span("hello"), None);
    }

    #[test]
    fn strptime_specifiers() {
        // The prime pair: strptime inverts strftime.
        let (s, n, o) = parse_iso("2024-02-15T13:26:40Z").unwrap();
        for fmt in ["%Y-%m-%d %H:%M:%S", "%F %T", "%d/%m/%Y %H:%M:%S"] {
            let text = strftime(fmt, s, n, o);
            assert_eq!(strptime(&text, fmt), Some((s, 0, None)), "{fmt}");
        }
        // US date, epoch defaults for the missing time.
        assert_eq!(
            strptime("02/15/2024", "%m/%d/%Y"),
            parse_iso("2024-02-15").map(|(s, n, _)| (s, n, None))
        );
        // Month names, either length, case-insensitive.
        assert_eq!(
            strptime("Feb 15 2024", "%b %d %Y"),
            parse_iso("2024-02-15").map(|(s, n, _)| (s, n, None))
        );
        assert_eq!(
            strptime("february 15 2024", "%B %d %Y"),
            parse_iso("2024-02-15").map(|(s, n, _)| (s, n, None))
        );
        // 12-hour clock with %p.
        assert_eq!(
            strptime("Feb 15 2024 3:04pm", "%b %d %Y %I:%M%p"),
            parse_iso("2024-02-15T15:04").map(|(s, n, _)| (s, n, None))
        );
        // %z is kept for display; the timeline point normalizes.
        assert_eq!(
            strptime("2024-02-15 14:26:40 +0100", "%Y-%m-%d %H:%M:%S %z"),
            Some((1708003600, 0, Some(60)))
        );
        // %s is the epoch directly.
        assert_eq!(strptime("1708003600", "%s"), Some((1708003600, 0, None)));
        // Weekday names parse and defer to the date fields.
        assert_eq!(
            strptime("Thursday, February 15, 2024", "%A, %B %d, %Y"),
            parse_iso("2024-02-15").map(|(s, n, _)| (s, n, None))
        );
        // Non-matches are None: bad literal, trailing junk, bad month.
        assert_eq!(strptime("2024:02:15", "%Y-%m-%d"), None);
        assert_eq!(strptime("02/15/2024 extra", "%m/%d/%Y"), None);
        assert_eq!(strptime("13/45/2024", "%m/%d/%Y"), None);
    }

    #[test]
    fn weekdays() {
        assert_eq!(weekday(0), 4); // 1970-01-01 was a Thursday
        let (s, _, _) = parse_iso("2026-07-11").unwrap();
        assert_eq!(weekday(s), 6); // a Saturday
    }

    #[test]
    fn invalid_civil_dates_reject() {
        // 2026 is not a leap year: Feb 29 has no reading, rather than
        // silently normalizing onto March 1.
        assert_eq!(parse_iso("2026-02-29"), None);
        // A real leap day still parses.
        assert!(parse_iso("2024-02-29").is_some());
        // April has 30 days; the 31st is not a date.
        assert_eq!(parse_iso("2024-04-31"), None);
        // strptime shares the rule in its final validation.
        assert_eq!(strptime("02/31/2024", "%m/%d/%Y"), None);
        assert_eq!(strptime("2026-02-29", "%F"), None);
    }

    #[test]
    fn duration_i64_min_round_trips() {
        // secs == i64::MIN must not overflow when formatting, and the
        // text must still round-trip (spec: durations round-trip).
        let text = format_duration(i64::MIN, 0);
        assert!(!text.contains("--"), "{text}");
        assert_eq!(parse_span(&text), Some((i64::MIN, 0)));
    }

    #[test]
    fn strptime_non_ascii_no_panic() {
        // A name-parse whose candidate byte length lands inside a
        // multibyte char must fail gracefully, not panic.
        assert_eq!(strptime("Jul\u{1F389}", "%b"), None);
        // %p's two-byte names must not over-index on a non-match.
        assert_eq!(strptime("3:04xy", "%I:%M%p"), None);
    }

    #[test]
    fn strptime_non_ascii_literal() {
        // A non-ASCII format literal parses what strftime wrote.
        let (s, n, o) = parse_iso("2024-02-15").unwrap();
        let text = strftime("%Y-%m-%d год", s, n, o);
        assert_eq!(text, "2024-02-15 год");
        assert_eq!(strptime(&text, "%Y-%m-%d год"), Some((s, 0, None)));
    }
}
