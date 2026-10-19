//! Duration designations by locale (spec: Duration Designations by
//! Locale): the short unit forms a language writes for the seven
//! units of the mixed duration form, days down to nanoseconds, as
//! `dfmt(LOCALE)` and the output locale write them, and as the span
//! text parser reads them under every locale.
//!
//! The forms are CLDR's short duration units (`cldr-units-full`),
//! with two settled exceptions: English is the canonical form, the
//! Latin symbols glued to the number (`5d 3h 5min`), and Russian is
//! GOST 8.417 (`сут ч мин с мс мкс нс`). Each form is a CLDR
//! pattern, `{0}` standing for the number; a form without `{0}`
//! (Arabic's one and two days) is written alone and read back as
//! that count. Word designations take the language's plural form
//! by the CLDR plural category of the count.

/// The units of the mixed form, largest first, by their canonical
/// designation.
pub const UNITS: [&str; 7] = ["d", "h", "min", "s", "ms", "us", "ns"];

/// One language's forms: for each of [`UNITS`], the patterns by
/// plural category (`one`, `two`, `few`, `many`, `zero`, `other`);
/// a category the list lacks takes `other`, the last entry.
pub struct DurationNames {
    pub lang: &'static str,
    pub forms: [&'static [(&'static str, &'static str)]; 7],
}

macro_rules! same {
    ($p:expr) => {
        &[("other", $p)]
    };
}

/// English: the canonical form, the Latin symbols glued.
pub static EN: DurationNames = DurationNames {
    lang: "en",
    forms: [
        same!("{0}d"),
        same!("{0}h"),
        same!("{0}min"),
        same!("{0}s"),
        same!("{0}ms"),
        same!("{0}us"),
        same!("{0}ns"),
    ],
};

pub static FR: DurationNames = DurationNames {
    lang: "fr",
    forms: [
        same!("{0} j"),
        same!("{0} h"),
        same!("{0} min"),
        same!("{0} s"),
        same!("{0} ms"),
        same!("{0} μs"),
        same!("{0} ns"),
    ],
};

pub static DE: DurationNames = DurationNames {
    lang: "de",
    forms: [
        same!("{0} Tg."),
        same!("{0} Std."),
        same!("{0} Min."),
        same!("{0} Sek."),
        same!("{0} ms"),
        same!("{0} μs"),
        same!("{0} ns"),
    ],
};

pub static ES: DurationNames = DurationNames {
    lang: "es",
    forms: [
        same!("{0} d"),
        same!("{0} h"),
        same!("{0} min"),
        same!("{0} s"),
        same!("{0} ms"),
        same!("{0} μs"),
        same!("{0} ns"),
    ],
};

pub static PT: DurationNames = DurationNames {
    lang: "pt",
    forms: [
        &[("one", "{0} dia"), ("other", "{0} dias")],
        same!("{0} h"),
        same!("{0} min"),
        same!("{0} s"),
        same!("{0} ms"),
        same!("{0} μs"),
        same!("{0} ns"),
    ],
};

pub static IT: DurationNames = DurationNames {
    lang: "it",
    forms: [
        &[("one", "{0} giorno"), ("other", "{0} giorni")],
        same!("{0} h"),
        same!("{0} min"),
        same!("{0} s"),
        same!("{0} ms"),
        same!("{0} μs"),
        same!("{0} ns"),
    ],
};

pub static TR: DurationNames = DurationNames {
    lang: "tr",
    forms: [
        same!("{0} gün"),
        same!("{0} sa."),
        same!("{0} dk."),
        same!("{0} sn."),
        same!("{0} msn"),
        same!("{0} μsn"),
        same!("{0} nsn"),
    ],
};

/// Russian: GOST 8.417, the designations spaced as the standard
/// writes them.
pub static RU: DurationNames = DurationNames {
    lang: "ru",
    forms: [
        same!("{0} сут"),
        same!("{0} ч"),
        same!("{0} мин"),
        same!("{0} с"),
        same!("{0} мс"),
        same!("{0} мкс"),
        same!("{0} нс"),
    ],
};

pub static JA: DurationNames = DurationNames {
    lang: "ja",
    forms: [
        same!("{0} 日"),
        same!("{0} 時間"),
        same!("{0} 分"),
        same!("{0} 秒"),
        same!("{0} ms"),
        same!("{0} μs"),
        same!("{0} ns"),
    ],
};

/// Chinese: the only shipped language that glues the designation.
pub static ZH: DurationNames = DurationNames {
    lang: "zh",
    forms: [
        same!("{0}天"),
        same!("{0}小时"),
        same!("{0}分钟"),
        same!("{0}秒"),
        same!("{0}毫秒"),
        same!("{0}微秒"),
        same!("{0}纳秒"),
    ],
};

/// Arabic: the day by CLDR's six plural categories; one day and two
/// days are words without a number.
pub static AR: DurationNames = DurationNames {
    lang: "ar",
    forms: [
        &[
            ("zero", "{0} يوم"),
            ("one", "يوم"),
            ("two", "يومان"),
            ("few", "{0} أيام"),
            ("many", "{0} يومًا"),
            ("other", "{0} يوم"),
        ],
        same!("{0} س"),
        same!("{0} د"),
        same!("{0} ث"),
        same!("{0} ملي ث"),
        same!("{0} م.ث."),
        same!("{0} ن.ث."),
    ],
};

/// Every shipped table, English first.
pub static ALL: [&DurationNames; 11] = [&EN, &ZH, &ES, &AR, &PT, &RU, &JA, &DE, &FR, &TR, &IT];

/// The table for a BCP 47 locale: the language decides (`fr-CA`,
/// `fr`, `fr_CA` alike); the empty tag is English, the canonical
/// form. None for a language no table covers.
pub fn names(locale: &str) -> Option<&'static DurationNames> {
    let lang = crate::language(locale);
    Some(match lang.as_str() {
        "en" | "" => &EN,
        "zh" => &ZH,
        "es" => &ES,
        "ar" => &AR,
        "pt" => &PT,
        "ru" => &RU,
        "ja" => &JA,
        "de" => &DE,
        "fr" => &FR,
        "tr" => &TR,
        "it" => &IT,
        _ => return None,
    })
}

/// The CLDR plural category of an integer count in a language
/// (the cardinal rules of CLDR's `plurals.json`, for the shipped
/// languages).
pub fn plural_category(lang: &str, n: u64) -> &'static str {
    match lang {
        "ar" => match n {
            0 => "zero",
            1 => "one",
            2 => "two",
            _ => match n % 100 {
                3..=10 => "few",
                11..=99 => "many",
                _ => "other",
            },
        },
        "ru" => {
            let (m10, m100) = (n % 10, n % 100);
            if m10 == 1 && m100 != 11 {
                "one"
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                "few"
            } else {
                "many"
            }
        }
        "fr" | "pt" => {
            if n <= 1 {
                "one"
            } else {
                "other"
            }
        }
        "ja" | "zh" | "tr" => "other",
        _ => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

impl DurationNames {
    /// The pattern for `n` of unit `unit` (an index into [`UNITS`]).
    fn pattern(&self, unit: usize, n: u64) -> &'static str {
        let category = plural_category(self.lang, n);
        let forms = self.forms[unit];
        forms
            .iter()
            .find(|(c, _)| *c == category)
            .or_else(|| forms.iter().find(|(c, _)| *c == "other"))
            .or_else(|| forms.last())
            .map(|(_, p)| *p)
            .unwrap_or("{0}")
    }

    /// One component written: `n` of unit `unit`, the number as the
    /// caller renders it (`number` may carry the locale's grouping).
    pub fn component(&self, unit: usize, n: u64, number: &str) -> String {
        let pattern = self.pattern(unit, n);
        if pattern.contains("{0}") {
            pattern.replace("{0}", number)
        } else {
            pattern.to_string()
        }
    }

    /// The day component of a magnitude that is no integer count
    /// (the beyond-range fallback): the `other` form with the
    /// number as given.
    pub fn days(&self, number: &str) -> String {
        self.pattern(0, 5).replace("{0}", number)
    }
}

/// Every designation any table writes, with the unit it names and
/// the count a number-less form implies, longest first, so the
/// span parser can take the longest match at a position.
pub fn designations() -> &'static [(String, usize, Option<u64>)] {
    use std::sync::OnceLock;
    static ALL_FORMS: OnceLock<Vec<(String, usize, Option<u64>)>> = OnceLock::new();
    ALL_FORMS.get_or_init(|| {
        let mut out: Vec<(String, usize, Option<u64>)> = Vec::new();
        for table in ALL {
            for (unit, forms) in table.forms.iter().enumerate() {
                for (category, pattern) in forms.iter() {
                    let (text, implied) = match pattern.split_once("{0}") {
                        Some((_, rest)) => (rest.trim().to_string(), None),
                        None => (
                            pattern.to_string(),
                            Some(match *category {
                                "one" => 1,
                                "two" => 2,
                                "zero" => 0,
                                _ => 1,
                            }),
                        ),
                    };
                    if !text.is_empty()
                        && !out
                            .iter()
                            .any(|(t, u, i)| *t == text && *u == unit && *i == implied)
                    {
                        out.push((text, unit, implied));
                    }
                }
            }
        }
        out.sort_by(|a, b| {
            b.0.chars()
                .count()
                .cmp(&a.0.chars().count())
                .then(a.0.cmp(&b.0))
        });
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_categories() {
        assert_eq!(plural_category("ar", 0), "zero");
        assert_eq!(plural_category("ar", 1), "one");
        assert_eq!(plural_category("ar", 2), "two");
        assert_eq!(plural_category("ar", 3), "few");
        assert_eq!(plural_category("ar", 10), "few");
        assert_eq!(plural_category("ar", 11), "many");
        assert_eq!(plural_category("ar", 99), "many");
        assert_eq!(plural_category("ar", 100), "other");
        assert_eq!(plural_category("ar", 103), "few");
        assert_eq!(plural_category("it", 1), "one");
        assert_eq!(plural_category("it", 2), "other");
        assert_eq!(plural_category("fr", 0), "one");
        assert_eq!(plural_category("ru", 21), "one");
        assert_eq!(plural_category("ru", 22), "few");
        assert_eq!(plural_category("ru", 12), "many");
    }

    #[test]
    fn components_by_language() {
        assert_eq!(EN.component(0, 5, "5"), "5d");
        assert_eq!(FR.component(0, 5, "5"), "5 j");
        assert_eq!(DE.component(1, 3, "3"), "3 Std.");
        assert_eq!(IT.component(0, 1, "1"), "1 giorno");
        assert_eq!(IT.component(0, 2, "2"), "2 giorni");
        assert_eq!(PT.component(0, 1, "1"), "1 dia");
        assert_eq!(PT.component(0, 5, "5"), "5 dias");
        assert_eq!(ZH.component(1, 3, "3"), "3小时");
        assert_eq!(JA.component(1, 3, "3"), "3 時間");
        assert_eq!(AR.component(0, 1, "1"), "يوم");
        assert_eq!(AR.component(0, 2, "2"), "يومان");
        assert_eq!(AR.component(0, 3, "3"), "3 أيام");
        assert_eq!(AR.component(0, 11, "11"), "11 يومًا");
        assert_eq!(AR.component(4, 5, "5"), "5 ملي ث");
        assert_eq!(RU.component(0, 5, "5"), "5 сут");
        // the number as the caller renders it
        assert_eq!(FR.component(3, 1500, "1 500"), "1 500 s");
        assert!(names("fr-CA").is_some());
        assert!(names("ru_RU.UTF-8").is_some());
        assert!(names("xx").is_none());
        assert_eq!(names("").unwrap().lang, "en");
    }

    #[test]
    fn designations_longest_first() {
        let all = designations();
        let pos = |t: &str| all.iter().position(|(d, _, _)| d == t).unwrap();
        assert!(pos("Min.") < pos("min"));
        assert!(pos("ملي ث") < pos("ث"));
        assert!(pos("dias") < pos("d"));
        let day_one = all.iter().find(|(d, _, _)| d == "يوم").unwrap();
        assert_eq!(day_one.1, 0);
        // the two-days word carries its count
        assert!(
            all.iter()
                .any(|(d, u, i)| d == "يومان" && *u == 0 && *i == Some(2))
        );
    }
}
