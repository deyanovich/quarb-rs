//! Calendars (spec: Calendars): a calendar is a *reading* of an
//! instant or a period, never a type. Like the source offset it is
//! kept for reading and display while comparison, arithmetic and
//! containment stay on the timeline. The conversions are ICU4X's
//! (`icu_calendar`, behind the default-on `calendars` feature);
//! without the feature only the Gregorian reading converts.

/// The calendars a reading can name. `Gregorian` is the default
/// and the proleptic Gregorian calendar every instant is born in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Calendar {
    #[default]
    Gregorian,
    Julian,
    Hebrew,
    /// The Umm al-Qura calendar of Saudi Arabia (KACST), the
    /// civil Hijri calendar.
    Islamic,
    /// The tabular (arithmetic) Hijri calendar, type II leap years,
    /// Friday epoch.
    IslamicTabular,
    Persian,
    Coptic,
    /// The Ethiopian calendar in the Amete Mihret era.
    Ethiopian,
    Buddhist,
    /// The Indian national (Śaka) calendar.
    Indian,
    /// The Republic of China (Minguo) calendar.
    Roc,
    /// The Japanese calendar with its modern eras (Meiji on).
    Japanese,
    /// The traditional Chinese lunisolar calendar.
    Chinese,
    /// The traditional Korean lunisolar calendar.
    Dangi,
}

/// The names `calendar(NAME)` and the formatters take, in the
/// order the refusal lists them.
pub const NAMES: [(&str, Calendar); 14] = [
    ("gregorian", Calendar::Gregorian),
    ("julian", Calendar::Julian),
    ("hebrew", Calendar::Hebrew),
    ("islamic", Calendar::Islamic),
    ("islamic-tabular", Calendar::IslamicTabular),
    ("persian", Calendar::Persian),
    ("coptic", Calendar::Coptic),
    ("ethiopian", Calendar::Ethiopian),
    ("buddhist", Calendar::Buddhist),
    ("indian", Calendar::Indian),
    ("roc", Calendar::Roc),
    ("japanese", Calendar::Japanese),
    ("chinese", Calendar::Chinese),
    ("dangi", Calendar::Dangi),
];

/// The shipped names, comma-separated, for a refusal.
pub fn shipped() -> String {
    NAMES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

impl Calendar {
    /// The calendar a name denotes, case folded; `islamic-umalqura`
    /// and `islamic_tabular` read as their hyphenated names.
    pub fn from_name(name: &str) -> Option<Calendar> {
        let n = name.trim().to_ascii_lowercase().replace('_', "-");
        let n = match n.as_str() {
            "islamic-umalqura" | "umalqura" | "hijri" => "islamic",
            "islamic-tbla" | "hijri-tabular" => "islamic-tabular",
            "ethiopic" => "ethiopian",
            "minguo" => "roc",
            "korean" => "dangi",
            other => other,
        };
        NAMES.iter().find(|(k, _)| *k == n).map(|(_, c)| *c)
    }

    /// The name `calendar` yields and the refusal lists.
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(_, c)| *c == self)
            .map(|(n, _)| *n)
            .unwrap_or("gregorian")
    }

    /// The CLDR calendar identifier whose data names the months and
    /// eras (`ca-<id>.json`); Julian has none and takes Gregorian's.
    pub fn cldr(self) -> &'static str {
        match self {
            Calendar::Gregorian | Calendar::Julian => "gregorian",
            Calendar::Hebrew => "hebrew",
            Calendar::Islamic => "islamic-umalqura",
            Calendar::IslamicTabular => "islamic-tbla",
            Calendar::Persian => "persian",
            Calendar::Coptic => "coptic",
            Calendar::Ethiopian => "ethiopic",
            Calendar::Buddhist => "buddhist",
            Calendar::Indian => "indian",
            Calendar::Roc => "roc",
            Calendar::Japanese => "japanese",
            Calendar::Chinese => "chinese",
            Calendar::Dangi => "dangi",
        }
    }

    /// The era a year with no era named belongs to: the calendar's
    /// current era, so `%Y` read back without an era means what it
    /// wrote. None where the calendar counts without eras.
    pub fn current_era(self) -> Option<&'static str> {
        match self {
            Calendar::Gregorian | Calendar::Julian => Some("ce"),
            Calendar::Hebrew => Some("am"),
            Calendar::Islamic | Calendar::IslamicTabular => Some("ah"),
            Calendar::Persian => Some("ap"),
            Calendar::Coptic => Some("am"),
            Calendar::Ethiopian => Some("am"),
            Calendar::Buddhist => Some("be"),
            Calendar::Indian => Some("shaka"),
            Calendar::Roc => Some("roc"),
            Calendar::Japanese => Some("reiwa"),
            Calendar::Chinese | Calendar::Dangi => None,
        }
    }
}

/// One day read in a calendar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarDate {
    /// The year as the calendar counts it in its era (Reiwa 6,
    /// Hebrew 5784).
    pub year: i64,
    /// The year on the calendar's single continuous count (the
    /// Gregorian year for Japanese and Buddhist, the Hebrew year
    /// for Hebrew).
    pub extended_year: i64,
    /// The era's code (`reiwa`, `ce`, `ah`), None where the calendar
    /// has no eras.
    pub era: Option<String>,
    /// The month's ordinal position in its year, from 1.
    pub month: u8,
    /// The CLDR month code: `M01`…`M13`, `M05L` for a leap month.
    pub code: String,
    /// The day of the month, from 1.
    pub day: u8,
    /// The day of the year, from 1.
    pub day_of_year: u16,
    /// Whether the year is a leap year of its calendar.
    pub leap_year: bool,
    /// The months in the year.
    pub months_in_year: u8,
    /// The related ISO year (the Gregorian year the lunisolar year
    /// began in), for the calendars that print it.
    pub related_iso: i64,
    /// The sexagenary cycle position of the year, 1–60, for the
    /// Chinese and Korean calendars.
    pub cyclic_year: Option<u8>,
}

impl CalendarDate {
    /// Whether the month is an intercalary one (its code ends in L).
    pub fn leap_month(&self) -> bool {
        self.code.ends_with('L')
    }
}

/// Days since the Unix epoch of a civil date in the proleptic
/// Gregorian calendar (the engine's own arithmetic, no ICU).
fn gregorian_days(y: i64, m: u32, d: u32) -> i64 {
    crate::temporal::days_from_civil(y, m, d)
}

/// A Gregorian day read without ICU: what the `Gregorian` reading
/// answers, feature or no feature.
fn gregorian_date(days: i64) -> CalendarDate {
    let (y, m, d) = crate::temporal::civil_from_days(days);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let doy = (days - gregorian_days(y, 1, 1) + 1) as u16;
    let (era, year) = if y > 0 { ("ce", y) } else { ("bce", 1 - y) };
    CalendarDate {
        year,
        extended_year: y,
        era: Some(era.to_string()),
        month: m as u8,
        code: format!("M{m:02}"),
        day: d as u8,
        day_of_year: doy,
        leap_year: leap,
        months_in_year: 12,
        related_iso: y,
        cyclic_year: None,
    }
}

/// The day `days` since the Unix epoch read in `calendar`. None
/// when the build has no calendars and the reading is not
/// Gregorian, or the day lies outside the calendar's range.
pub fn date(calendar: Calendar, days: i64) -> Option<CalendarDate> {
    if calendar == Calendar::Gregorian {
        return Some(gregorian_date(days));
    }
    icu::date(calendar, days)
}

/// The day since the Unix epoch of a date given by its fields in
/// `calendar`: the era (None for the calendar's current one), the
/// year in that era, the month code (`M05L` for a leap month) and
/// the day. None for a date the calendar does not have.
pub fn days_from_code(
    calendar: Calendar,
    era: Option<&str>,
    year: i64,
    code: &str,
    day: u8,
) -> Option<i64> {
    if calendar == Calendar::Gregorian {
        let m: u32 = code.strip_prefix('M')?.parse().ok()?;
        if !(1..=12).contains(&m) || day == 0 || day > 31 {
            return None;
        }
        let y = match era {
            Some("bce") => 1 - year,
            _ => year,
        };
        let days = gregorian_days(y, m, u32::from(day));
        // a day the month does not have (February 30) is no date
        let (yy, mm, dd) = crate::temporal::civil_from_days(days);
        return (yy == y && mm == m && dd == u32::from(day)).then_some(days);
    }
    icu::days_from_code(calendar, era, year, code, day)
}

/// [`days_from_code`] by the month's ordinal position instead of
/// its code: the month of that position in that year, leap months
/// counted in their place.
pub fn days_from_ordinal(
    calendar: Calendar,
    era: Option<&str>,
    year: i64,
    month: u8,
    day: u8,
) -> Option<i64> {
    if calendar == Calendar::Gregorian {
        return days_from_code(calendar, era, year, &format!("M{month:02}"), day);
    }
    icu::days_from_ordinal(calendar, era, year, month, day)
}

/// The first day of the month that holds `days`, in `calendar`.
pub fn month_start(calendar: Calendar, days: i64) -> Option<i64> {
    let d = date(calendar, days)?;
    days_from_code(calendar, d.era.as_deref(), d.year, &d.code, 1)
}

/// The first day of the month after the one that holds `days`.
pub fn next_month_start(calendar: Calendar, days: i64) -> Option<i64> {
    let d = date(calendar, days)?;
    let start = days_from_code(calendar, d.era.as_deref(), d.year, &d.code, 1)?;
    // the days of the month: the month's last day is the day before
    // the next month's first; ask the calendar for the length
    let len = icu_or_gregorian_days_in_month(calendar, start)?;
    Some(start + i64::from(len))
}

/// The first day of the year that holds `days`, in `calendar`.
pub fn year_start(calendar: Calendar, days: i64) -> Option<i64> {
    let d = date(calendar, days)?;
    Some(days - i64::from(d.day_of_year) + 1)
}

/// The first day of the year after the one that holds `days`.
pub fn next_year_start(calendar: Calendar, days: i64) -> Option<i64> {
    let start = year_start(calendar, days)?;
    let len = icu_or_gregorian_days_in_year(calendar, start)?;
    Some(start + i64::from(len))
}

fn icu_or_gregorian_days_in_month(calendar: Calendar, days: i64) -> Option<u8> {
    if calendar == Calendar::Gregorian {
        let (y, m, _) = crate::temporal::civil_from_days(days);
        let next = if m == 12 {
            gregorian_days(y + 1, 1, 1)
        } else {
            gregorian_days(y, m + 1, 1)
        };
        return Some((next - gregorian_days(y, m, 1)) as u8);
    }
    icu::days_in_month(calendar, days)
}

fn icu_or_gregorian_days_in_year(calendar: Calendar, days: i64) -> Option<u16> {
    if calendar == Calendar::Gregorian {
        let (y, _, _) = crate::temporal::civil_from_days(days);
        return Some((gregorian_days(y + 1, 1, 1) - gregorian_days(y, 1, 1)) as u16);
    }
    icu::days_in_year(calendar, days)
}

#[cfg(feature = "calendars")]
mod icu {
    use super::{Calendar, CalendarDate};
    use icu_calendar::types::{Month, YearInput};
    use icu_calendar::{AnyCalendar, AnyCalendarKind, Date};

    /// Rata die of the Unix epoch (1970-01-01 is R.D. 719163).
    const EPOCH_RD: i64 = 719_163;

    fn kind(calendar: Calendar) -> AnyCalendarKind {
        match calendar {
            Calendar::Gregorian => AnyCalendarKind::Gregorian,
            Calendar::Julian => AnyCalendarKind::Julian,
            Calendar::Hebrew => AnyCalendarKind::Hebrew,
            Calendar::Islamic => AnyCalendarKind::HijriUmmAlQura,
            Calendar::IslamicTabular => AnyCalendarKind::HijriTabularTypeIIFriday,
            Calendar::Persian => AnyCalendarKind::Persian,
            Calendar::Coptic => AnyCalendarKind::Coptic,
            Calendar::Ethiopian => AnyCalendarKind::Ethiopian,
            Calendar::Buddhist => AnyCalendarKind::Buddhist,
            Calendar::Indian => AnyCalendarKind::Indian,
            Calendar::Roc => AnyCalendarKind::Roc,
            Calendar::Japanese => AnyCalendarKind::Japanese,
            Calendar::Chinese => AnyCalendarKind::Chinese,
            Calendar::Dangi => AnyCalendarKind::Dangi,
        }
    }

    fn any(calendar: Calendar) -> AnyCalendar {
        AnyCalendar::new(kind(calendar))
    }

    fn rd(days: i64) -> icu_calendar::types::RataDie {
        icu_calendar::types::RataDie::new(days + EPOCH_RD)
    }

    fn days_of(d: &Date<AnyCalendar>) -> i64 {
        d.to_rata_die().to_i64_date() - EPOCH_RD
    }

    fn read(d: &Date<AnyCalendar>) -> CalendarDate {
        let year = d.year();
        let extended = i64::from(year.extended_year());
        let (era, era_year) = match year.era() {
            Some(e) => (Some(e.era.to_string()), i64::from(e.year)),
            None => (None, extended),
        };
        let cyclic_year = year.cyclic().map(|c| c.year);
        let related_iso = year.cyclic().map_or(extended, |c| i64::from(c.related_iso));
        let month = d.month();
        CalendarDate {
            year: era_year,
            extended_year: extended,
            era,
            month: month.ordinal,
            code: month.to_input().code().0.to_string(),
            day: d.day_of_month().0,
            day_of_year: d.day_of_year().0,
            leap_year: d.is_in_leap_year(),
            months_in_year: d.months_in_year(),
            related_iso,
            cyclic_year,
        }
    }

    pub(super) fn date(calendar: Calendar, days: i64) -> Option<CalendarDate> {
        let d = Date::from_rata_die(rd(days), any(calendar));
        Some(read(&d))
    }

    pub(super) fn days_from_code(
        calendar: Calendar,
        era: Option<&str>,
        year: i64,
        code: &str,
        day: u8,
    ) -> Option<i64> {
        // `M05L`: the month number and the leap mark
        let body = code.strip_prefix('M')?;
        let (digits, leap) = match body.strip_suffix('L') {
            Some(d) => (d, true),
            None => (body, false),
        };
        let number: u8 = digits.parse().ok()?;
        let month = if leap {
            Month::leap(number)
        } else {
            Month::new(number)
        };
        let d = Date::try_new(year_input(calendar, era, year)?, month, day, any(calendar)).ok()?;
        Some(days_of(&d))
    }

    /// The year as ICU takes it: in the era named, else the
    /// calendar's current era, else the single count.
    fn year_input(calendar: Calendar, era: Option<&str>, year: i64) -> Option<YearInput<'static>> {
        let year = i32::try_from(year).ok()?;
        Some(match era.or_else(|| calendar.current_era()) {
            Some(era) => YearInput::EraYear(era_static(era)?, year),
            None => YearInput::Extended(year),
        })
    }

    /// An era code as a static string (ICU borrows it): the codes
    /// the readings answer.
    fn era_static(era: &str) -> Option<&'static str> {
        const ERAS: &[&str] = &[
            "ce", "bce", "am", "ah", "bh", "ap", "be", "shaka", "roc", "broc", "aa", "incar",
            "mundi", "reiwa", "heisei", "showa", "taisho", "meiji",
        ];
        ERAS.iter().copied().find(|e| *e == era)
    }

    pub(super) fn days_from_ordinal(
        calendar: Calendar,
        era: Option<&str>,
        year: i64,
        month: u8,
        day: u8,
    ) -> Option<i64> {
        // The month at that ordinal: the normal months in order, a
        // leap month where the year has one. Try the candidates and
        // keep the one whose ordinal matches.
        let year = year_input(calendar, era, year)?;
        for n in 1..=13u8 {
            for m in [Month::new(n), Month::leap(n)] {
                if let Ok(d) = Date::try_new(year, m, 1, any(calendar))
                    && d.month().ordinal == month
                {
                    let d = Date::try_new(year, m, day, any(calendar)).ok()?;
                    return Some(days_of(&d));
                }
            }
        }
        None
    }

    pub(super) fn days_in_month(calendar: Calendar, days: i64) -> Option<u8> {
        Some(Date::from_rata_die(rd(days), any(calendar)).days_in_month())
    }

    pub(super) fn days_in_year(calendar: Calendar, days: i64) -> Option<u16> {
        Some(Date::from_rata_die(rd(days), any(calendar)).days_in_year())
    }
}

#[cfg(not(feature = "calendars"))]
mod icu {
    use super::{Calendar, CalendarDate};

    pub(super) fn date(_: Calendar, _: i64) -> Option<CalendarDate> {
        None
    }
    pub(super) fn days_from_code(
        _: Calendar,
        _: Option<&str>,
        _: i64,
        _: &str,
        _: u8,
    ) -> Option<i64> {
        None
    }
    pub(super) fn days_from_ordinal(
        _: Calendar,
        _: Option<&str>,
        _: i64,
        _: u8,
        _: u8,
    ) -> Option<i64> {
        None
    }
    pub(super) fn days_in_month(_: Calendar, _: i64) -> Option<u8> {
        None
    }
    pub(super) fn days_in_year(_: Calendar, _: i64) -> Option<u16> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(y: i64, m: u32, d: u32) -> i64 {
        crate::temporal::days_from_civil(y, m, d)
    }

    #[test]
    fn names_round_trip() {
        for (n, c) in NAMES {
            assert_eq!(Calendar::from_name(n), Some(c));
            assert_eq!(c.name(), n);
        }
        assert_eq!(
            Calendar::from_name("Islamic_Tabular"),
            Some(Calendar::IslamicTabular)
        );
        assert_eq!(Calendar::from_name("french-republican"), None);
        assert!(shipped().starts_with("gregorian, julian, hebrew"));
    }

    #[test]
    fn gregorian_reading_needs_no_icu() {
        let d = date(Calendar::Gregorian, days(2024, 3, 5)).unwrap();
        assert_eq!((d.year, d.month, d.day, d.day_of_year), (2024, 3, 5, 65));
        assert_eq!(d.code, "M03");
        assert_eq!(d.era.as_deref(), Some("ce"));
        assert!(d.leap_year);
        let b = date(Calendar::Gregorian, days(-43, 1, 1)).unwrap();
        assert_eq!((b.year, b.era.as_deref()), (44, Some("bce")));
        assert_eq!(
            days_from_code(Calendar::Gregorian, None, 2024, "M03", 5),
            Some(days(2024, 3, 5))
        );
        assert_eq!(
            days_from_code(Calendar::Gregorian, Some("bce"), 44, "M01", 1),
            Some(days(-43, 1, 1))
        );
        assert_eq!(
            days_from_code(Calendar::Gregorian, None, 2023, "M02", 29),
            None
        );
        assert_eq!(
            month_start(Calendar::Gregorian, days(2024, 3, 5)),
            Some(days(2024, 3, 1))
        );
        assert_eq!(
            next_month_start(Calendar::Gregorian, days(2024, 12, 5)),
            Some(days(2025, 1, 1))
        );
        assert_eq!(
            year_start(Calendar::Gregorian, days(2024, 3, 5)),
            Some(days(2024, 1, 1))
        );
        assert_eq!(
            next_year_start(Calendar::Gregorian, days(2024, 3, 5)),
            Some(days(2025, 1, 1))
        );
    }

    #[cfg(feature = "calendars")]
    #[test]
    fn readings_in_every_calendar() {
        let d = days(2024, 3, 5);
        let read = |c: Calendar| date(c, d).unwrap();
        let h = read(Calendar::Hebrew);
        assert_eq!((h.year, h.month, h.day), (5784, 6, 25));
        assert_eq!(h.code, "M05L");
        assert!(h.leap_month() && h.leap_year);
        assert_eq!(h.months_in_year, 13);
        let i = read(Calendar::Islamic);
        assert_eq!(
            (i.year, i.month, i.day, i.code.as_str()),
            (1445, 8, 24, "M08")
        );
        let j = read(Calendar::Julian);
        assert_eq!((j.year, j.month, j.day), (2024, 2, 21));
        let p = read(Calendar::Persian);
        assert_eq!((p.year, p.month, p.day), (1402, 12, 15));
        let c = read(Calendar::Coptic);
        assert_eq!((c.year, c.month, c.day), (1740, 6, 26));
        let e = read(Calendar::Ethiopian);
        assert_eq!((e.year, e.month, e.day), (2016, 6, 26));
        let b = read(Calendar::Buddhist);
        assert_eq!((b.year, b.month, b.day), (2567, 3, 5));
        let n = read(Calendar::Indian);
        assert_eq!((n.year, n.month, n.day), (1945, 12, 15));
        let r = read(Calendar::Roc);
        assert_eq!((r.year, r.month, r.day), (113, 3, 5));
        let ja = read(Calendar::Japanese);
        assert_eq!((ja.year, ja.month, ja.day), (6, 3, 5));
        assert_eq!(ja.era.as_deref(), Some("reiwa"));
        assert_eq!(ja.extended_year, 2024);
        let z = read(Calendar::Chinese);
        assert_eq!((z.month, z.day), (1, 25));
        assert_eq!(z.related_iso, 2024);
        assert_eq!(z.cyclic_year, Some(41)); // jia-chen, the Wood Dragon
        assert!(z.era.is_none());
        let k = read(Calendar::Dangi);
        assert_eq!((k.month, k.day), (1, 25));
        // the era codes the readings answer, for the name tables
        let codes: Vec<(Calendar, Option<String>)> =
            NAMES.iter().map(|(_, c)| (*c, read(*c).era)).collect();
        eprintln!("ERA CODES {codes:?}");
        // Meiji 6 (ICU's modern eras begin with the Gregorian
        // adoption of 1873); before that the Gregorian eras
        let m = date(Calendar::Japanese, days(1873, 5, 6)).unwrap();
        assert_eq!((m.era.as_deref(), m.year), (Some("meiji"), 6));
        let old = date(Calendar::Japanese, days(1828, 9, 9)).unwrap();
        assert_eq!((old.era.as_deref(), old.year), (Some("ce"), 1828));
        let broc = date(Calendar::Roc, days(1899, 5, 6)).unwrap();
        assert_eq!((broc.era.as_deref(), broc.year), (Some("broc"), 13));
        let aa = date(Calendar::Ethiopian, days(-10, 1, 1)).unwrap();
        eprintln!("ETHIOPIAN -10 {:?} {}", aa.era, aa.year);
        let bce = date(Calendar::Julian, days(-43, 3, 15)).unwrap();
        assert_eq!((bce.era.as_deref(), bce.year), (Some("bce"), 44));
    }

    #[cfg(feature = "calendars")]
    #[test]
    fn fields_back_to_days() {
        let d = days(2024, 3, 5);
        assert_eq!(
            days_from_code(Calendar::Hebrew, None, 5784, "M05L", 25),
            Some(d)
        );
        assert_eq!(
            days_from_ordinal(Calendar::Hebrew, None, 5784, 6, 25),
            Some(d)
        );
        assert_eq!(
            days_from_ordinal(Calendar::Hebrew, None, 5784, 7, 1),
            days_from_code(Calendar::Hebrew, None, 5784, "M06", 1)
        );
        assert_eq!(
            days_from_code(Calendar::Islamic, None, 1445, "M08", 24),
            Some(d)
        );
        assert_eq!(
            days_from_ordinal(Calendar::Julian, None, 1828, 8, 28),
            Some(days(1828, 9, 9))
        );
        assert_eq!(
            days_from_ordinal(Calendar::Japanese, None, 6, 3, 5),
            Some(d)
        );
        assert_eq!(
            days_from_ordinal(Calendar::Japanese, Some("meiji"), 6, 5, 6),
            Some(days(1873, 5, 6))
        );
        assert_eq!(
            days_from_ordinal(Calendar::Japanese, Some("ce"), 1828, 9, 9),
            Some(days(1828, 9, 9))
        );
        assert_eq!(
            days_from_ordinal(Calendar::Julian, Some("bce"), 44, 3, 15),
            Some(days(-43, 3, 13))
        );
        assert_eq!(days_from_ordinal(Calendar::Roc, None, 113, 3, 5), Some(d));
        assert_eq!(
            days_from_ordinal(Calendar::Buddhist, None, 2567, 3, 5),
            Some(d)
        );
        assert_eq!(days_from_code(Calendar::Hebrew, None, 5784, "M14", 1), None);
        assert_eq!(days_from_ordinal(Calendar::Hebrew, None, 5783, 13, 1), None); // no 13th month
        // month and year bounds in a calendar
        assert_eq!(
            month_start(Calendar::Hebrew, d),
            days_from_code(Calendar::Hebrew, None, 5784, "M05L", 1)
        );
        assert_eq!(
            next_month_start(Calendar::Hebrew, d),
            days_from_code(Calendar::Hebrew, None, 5784, "M06", 1)
        );
        assert_eq!(
            year_start(Calendar::Hebrew, d),
            days_from_code(Calendar::Hebrew, None, 5784, "M01", 1)
        );
        assert_eq!(
            next_year_start(Calendar::Hebrew, d),
            days_from_code(Calendar::Hebrew, None, 5785, "M01", 1)
        );
        assert_eq!(year_start(Calendar::Chinese, d), Some(days(2024, 2, 10)));
        assert_eq!(
            next_year_start(Calendar::Chinese, d),
            Some(days(2025, 1, 29))
        );
    }
}
