//! Units written as words (the header-units round, ruling
//! 2026-11-09): a column title's tail such as `metric tonnes per
//! year`, `[millions of cubic feet per second]` or `EUR million`
//! read as a unit expression and a scaling factor for the cells. An
//! exact word list — singular and plural, British and American,
//! Russian beside the GOST designations — never a fuzzy match. The
//! same dictionary serves `convert(tonnes)`, `12_tonnes` and unit
//! text in cells (`5 metric tonnes`).

use std::collections::HashMap;

/// A unit read from words: the unit expression the table reads
/// (`t/a`, `ft^3/s`, `USD`), the factor the cells take (`1e6` for
/// `millions of`), and what was read, for the info line.
#[derive(Debug, Clone, PartialEq)]
pub struct WordUnit {
    pub expr: String,
    pub factor: f64,
}

/// Why a tail did not read as a unit: the first word run that
/// resolved to nothing, quoted for the message (`'per yer' is no
/// unit`), or an ambiguity with its pointer.
#[derive(Debug, Clone, PartialEq)]
pub enum WordRefusal {
    /// A run of words that names no unit.
    Unknown(String),
    /// A word that names two units: the pointer says which to write.
    Ambiguous { word: String, pointer: String },
}

impl WordRefusal {
    pub fn told(&self) -> String {
        match self {
            WordRefusal::Unknown(w) => format!("'{w}' is no unit"),
            WordRefusal::Ambiguous { word, pointer } => format!("'{word}' is ambiguous: {pointer}"),
        }
    }
}

/// The word forms and the symbol each names. Lowercase; a form of
/// several words is matched longest first. A symbol must be one the
/// table reads (quanto's packs, the engine's own declarations, the
/// currencies).
const WORDS: &[(&[&str], &str)] = &[
    // length
    (
        &[
            "metre",
            "metres",
            "meter",
            "meters",
            "метр",
            "метра",
            "метров",
            "метры",
        ],
        "m",
    ),
    (
        &[
            "kilometre",
            "kilometres",
            "kilometer",
            "kilometers",
            "километр",
            "километра",
            "километров",
            "километры",
            "км",
        ],
        "km",
    ),
    (
        &[
            "centimetre",
            "centimetres",
            "centimeter",
            "centimeters",
            "сантиметр",
            "сантиметра",
            "сантиметров",
            "см",
        ],
        "cm",
    ),
    (
        &[
            "millimetre",
            "millimetres",
            "millimeter",
            "millimeters",
            "миллиметр",
            "миллиметра",
            "миллиметров",
            "мм",
        ],
        "mm",
    ),
    (&["mile", "miles", "миля", "мили", "миль"], "mi"),
    (
        &[
            "nautical mile",
            "nautical miles",
            "морская миля",
            "морских миль",
        ],
        "nmi",
    ),
    (&["foot", "feet", "фут", "фута", "футов", "футы"], "ft"),
    // area and volume
    (
        &["hectare", "hectares", "гектар", "гектара", "гектаров", "га"],
        "ha",
    ),
    (&["acre", "acres", "акр", "акра", "акров"], "ac"),
    (
        &[
            "litre",
            "litres",
            "liter",
            "liters",
            "литр",
            "литра",
            "литров",
            "литры",
            "л",
        ],
        "L",
    ),
    (
        &["gallon", "gallons", "галлон", "галлона", "галлонов"],
        "gal",
    ),
    // mass
    (
        &[
            "kilogram",
            "kilograms",
            "kilogramme",
            "kilogrammes",
            "килограмм",
            "килограмма",
            "килограммов",
            "килограммы",
            "кг",
        ],
        "kg",
    ),
    (
        &[
            "gram",
            "grams",
            "gramme",
            "grammes",
            "грамм",
            "грамма",
            "граммов",
            "граммы",
            "г",
        ],
        "g",
    ),
    (
        &[
            "milligram",
            "milligrams",
            "миллиграмм",
            "миллиграмма",
            "миллиграммов",
            "мг",
        ],
        "mg",
    ),
    (
        &[
            "tonne",
            "tonnes",
            "metric ton",
            "metric tons",
            "metric tonne",
            "metric tonnes",
            "тонна",
            "тонны",
            "тонн",
            "т",
        ],
        "t",
    ),
    (&["ton", "tons", "short ton", "short tons"], "ton"),
    (&["ounce", "ounces", "унция", "унции", "унций"], "oz"),
    // time
    (
        &[
            "second",
            "seconds",
            "sec",
            "secs",
            "секунда",
            "секунды",
            "секунд",
            "сек",
            "с",
        ],
        "s",
    ),
    (
        &["minute", "minutes", "минута", "минуты", "минут", "мин"],
        "min",
    ),
    (
        &[
            "hour",
            "hours",
            "hr",
            "hrs",
            "час",
            "часа",
            "часов",
            "часы",
            "ч",
        ],
        "h",
    ),
    (
        &[
            "day",
            "days",
            "день",
            "дня",
            "дней",
            "дни",
            "сутки",
            "суток",
            "сут",
        ],
        "d",
    ),
    (&["week", "weeks", "неделя", "недели", "недель", "нед"], "w"),
    (
        &[
            "year", "years", "yr", "yrs", "annum", "p.a.", "год", "года", "годы", "лет", "г.",
        ],
        "a",
    ),
    // energy, power, pressure
    (&["watt", "watts", "ватт", "ватта", "ваттов", "вт"], "W"),
    (
        &[
            "kilowatt",
            "kilowatts",
            "киловатт",
            "киловатта",
            "киловаттов",
            "квт",
        ],
        "kW",
    ),
    (
        &[
            "megawatt",
            "megawatts",
            "мегаватт",
            "мегаватта",
            "мегаваттов",
            "мвт",
        ],
        "MW",
    ),
    (
        &[
            "kilowatt-hour",
            "kilowatt-hours",
            "kilowatt hour",
            "kilowatt hours",
            "kwh",
            "киловатт-час",
            "киловатт-часа",
            "киловатт-часов",
            "квт·ч",
            "квтч",
        ],
        "kWh",
    ),
    (&["joule", "joules", "джоуль", "джоуля", "джоулей"], "J"),
    (
        &["pascal", "pascals", "паскаль", "паскаля", "паскалей"],
        "Pa",
    ),
    (
        &["kelvin", "kelvins", "кельвин", "кельвина", "кельвинов"],
        "K",
    ),
    // dimension one
    (
        &[
            "percent",
            "per cent",
            "pct",
            "процент",
            "процента",
            "процентов",
            "проценты",
            "%",
        ],
        "%",
    ),
    (
        &["radian", "radians", "радиан", "радиана", "радианов"],
        "rad",
    ),
    // currencies, by word and by sign
    (
        &[
            "dollar",
            "dollars",
            "us dollar",
            "us dollars",
            "u.s. dollars",
            "usd",
            "$",
            "доллар",
            "доллара",
            "долларов",
            "доллары",
            "долл.",
        ],
        "USD",
    ),
    (&["euro", "euros", "eur", "€", "евро"], "EUR"),
    (
        &[
            "pound sterling",
            "pounds sterling",
            "sterling",
            "gbp",
            "£",
            "фунт стерлингов",
            "фунтов стерлингов",
        ],
        "GBP",
    ),
    (
        &[
            "rouble",
            "roubles",
            "ruble",
            "rubles",
            "rub",
            "₽",
            "рубль",
            "рубля",
            "рублей",
            "рубли",
            "руб.",
            "руб",
            "р.",
        ],
        "RUB",
    ),
    (&["yen", "jpy", "¥", "иена", "иены", "иен"], "JPY"),
    (&["yuan", "cny", "юань", "юаня", "юаней"], "CNY"),
    (
        &[
            "swiss franc",
            "swiss francs",
            "franc",
            "francs",
            "chf",
            "франк",
            "франка",
            "франков",
        ],
        "CHF",
    ),
];

/// Words that name two units: refused with the pointer unless an
/// override says which.
const AMBIGUOUS: &[(&str, &str)] = &[
    ("pound", "write £ (sterling) or lb (the mass)"),
    ("pounds", "write £ (sterling) or lb (the mass)"),
    ("фунт", "write £ (sterling) or lb (the mass)"),
    ("фунты", "write £ (sterling) or lb (the mass)"),
    ("фунтов", "write £ (sterling) or lb (the mass)"),
];

/// The power words before a unit: `square kilometres` → `km^2`.
const POWERS: &[(&[&str], u8)] = &[
    (
        &[
            "square",
            "sq",
            "sq.",
            "квадратный",
            "квадратных",
            "квадратные",
            "кв.",
            "кв",
        ],
        2,
    ),
    (
        &[
            "cubic",
            "cu",
            "cu.",
            "кубический",
            "кубических",
            "кубические",
            "куб.",
            "куб",
        ],
        3,
    ),
];

/// The power words after a unit: `metres squared`.
const POWERS_AFTER: &[(&[&str], u8)] = &[(&["squared"], 2), (&["cubed"], 3)];

/// The scale words: a factor the cells take (always a factor, never
/// a prefix change — the unit stays the one the title names).
const SCALES: &[(&[&str], f64)] = &[
    (
        &["thousand", "thousands", "тыс.", "тыс", "тысяч", "тысячи"],
        1e3,
    ),
    (
        &[
            "million",
            "millions",
            "mn",
            "mln",
            "mio",
            "млн",
            "млн.",
            "миллион",
            "миллиона",
            "миллионов",
            "миллионы",
        ],
        1e6,
    ),
    (
        &[
            "billion",
            "billions",
            "bn",
            "bln",
            "млрд",
            "млрд.",
            "миллиард",
            "миллиарда",
            "миллиардов",
            "миллиарды",
        ],
        1e9,
    ),
    (
        &[
            "trillion",
            "trillions",
            "трлн",
            "трлн.",
            "триллион",
            "триллиона",
            "триллионов",
            "триллионы",
        ],
        1e12,
    ),
];

/// The scale letters glued to a currency sign: `$m`, `€bn`, `£k`.
const SIGN_SCALES: &[(&str, f64)] = &[
    ("k", 1e3),
    ("m", 1e6),
    ("mn", 1e6),
    ("bn", 1e9),
    ("tn", 1e12),
];

/// The connective between two phrases: `per`, `/`, Russian `в`
/// (`тонн в год`).
const PER: &[&str] = &["per", "/", "в", "на"];

/// Words skipped between the parts: `of` in `millions of tonnes`.
const FILLER: &[&str] = &["of", "the", "in", "и"];

fn words_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: std::sync::OnceLock<HashMap<&'static str, &'static str>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        for (forms, symbol) in WORDS {
            for f in *forms {
                m.insert(*f, *symbol);
            }
        }
        m
    })
}

/// The symbol a single word form names, if any (lowercase match).
pub fn word_symbol(word: &str) -> Option<&'static str> {
    words_table().get(word.to_lowercase().as_str()).copied()
}

/// Whether a word is a unit word the dictionary knows, the
/// ambiguous ones included (`pounds`): what a declared column's title
/// may drop as its unit tail.
pub fn is_unit_word(word: &str) -> bool {
    let w = word.to_lowercase();
    words_table().contains_key(w.as_str()) || AMBIGUOUS.iter().any(|(a, _)| *a == w)
}

/// The resolver of a word run: an override first (the call's
/// `--units`, a scoped `--unit:col`), then the dictionary.
pub type Override<'a> = &'a dyn Fn(&str) -> Option<String>;

/// One word of a tail: lowercased for the dictionary, which reads
/// words whatever their case (`Metric Tonnes`, `EUR`), and as it is
/// written for the symbol road, where the case is the unit (`Mt` the
/// megatonne and `mt` the millitonne, `A` the ampere and `a` the
/// year, `kB` and `kb`).
#[derive(Clone, Debug)]
struct Word {
    folded: String,
    written: String,
}

impl Word {
    fn as_str(&self) -> &str {
        &self.folded
    }
}

/// The folded words of a run, joined as the dictionary and an
/// override read them.
fn run(words: &[Word]) -> String {
    words.iter().map(Word::as_str).collect::<Vec<_>>().join(" ")
}

/// The words of a tail, the brackets and the punctuation that frames
/// them dropped, a `/` kept as its own word.
fn tokenize(tail: &str) -> Vec<Word> {
    let mut cleaned = String::with_capacity(tail.len() + 8);
    for c in tail.chars() {
        match c {
            '(' | ')' | '[' | ']' | ',' | ';' | ':' => cleaned.push(' '),
            '/' => cleaned.push_str(" / "),
            _ => cleaned.push(c),
        }
    }
    cleaned
        .split_whitespace()
        .map(|w| Word {
            folded: w.to_lowercase(),
            written: w.to_string(),
        })
        .collect()
}

/// A currency sign with a glued scale letter (`$m`, `€bn`): the
/// symbol and the factor.
fn sign_scaled(word: &str) -> Option<(&'static str, f64)> {
    let mut chars = word.chars();
    let sign = chars.next()?;
    let rest: String = chars.collect();
    let symbol = word_symbol(&sign.to_string())?;
    if rest.is_empty() {
        return Some((symbol, 1.0));
    }
    SIGN_SCALES
        .iter()
        .find(|(s, _)| *s == rest)
        .map(|(_, f)| (symbol, *f))
}

/// A symbol with a trailing exponent digit (`ft3`, `m2`, `км2`).
fn digit_power(word: &str) -> Option<(String, u8)> {
    let last = word.chars().last()?;
    let n = last.to_digit(10)? as u8;
    if !(2..=3).contains(&n) {
        return None;
    }
    let stem: String = word.chars().take(word.chars().count() - 1).collect();
    (!stem.is_empty()).then_some((stem, n))
}

/// One phrase of the tail at `i`: `[power] unit [power]`, the unit a
/// word run of up to three words, a symbol the table reads, a sign
/// with a glued scale, or a symbol with a digit exponent. Returns
/// the unit factor-expression term, the scale it carried, and the
/// index past it.
fn phrase(
    words: &[Word],
    mut i: usize,
    resolve: Override<'_>,
    symbol_known: &dyn Fn(&str) -> bool,
) -> Result<(String, f64, usize), WordRefusal> {
    let mut power: u8 = 1;
    let mut factor = 1.0;
    // a scale before the unit (`millions of cubic feet`)
    while let Some(w) = words.get(i) {
        if let Some((_, f)) = SCALES.iter().find(|(forms, _)| forms.contains(&w.as_str())) {
            factor *= f;
            i += 1;
        } else if FILLER.contains(&w.as_str()) {
            i += 1;
        } else {
            break;
        }
    }
    if let Some(w) = words.get(i)
        && let Some((_, p)) = POWERS.iter().find(|(forms, _)| forms.contains(&w.as_str()))
    {
        power = *p;
        i += 1;
    }
    // the unit: the override's runs first (longest first), then the
    // dictionary's
    let mut found: Option<(String, usize)> = None;
    for len in (1..=3).rev() {
        if i + len > words.len() {
            continue;
        }
        if let Some(u) = resolve(&run(&words[i..i + len])) {
            found = Some((u, len));
            break;
        }
    }
    if found.is_none() {
        for len in (1..=3).rev() {
            if i + len > words.len() {
                continue;
            }
            if let Some(sym) = word_symbol(&run(&words[i..i + len])) {
                found = Some((sym.to_string(), len));
                break;
            }
        }
    }
    let (mut unit, len) = match found {
        Some(f) => f,
        None => {
            let Some(w) = words.get(i) else {
                return Err(WordRefusal::Unknown(run(words)));
            };
            if let Some((_, pointer)) = AMBIGUOUS.iter().find(|(a, _)| *a == w.folded) {
                return Err(WordRefusal::Ambiguous {
                    word: w.folded.clone(),
                    pointer: pointer.to_string(),
                });
            }
            // The symbol road reads the word as it is written: a
            // symbol's case is the unit. (It read the lowercased
            // word, so `Mt per year` was millitonnes, `kB` kilobits,
            // `A per m` years per metre.) A word form with a digit
            // exponent (`feet2`) is still the dictionary's.
            if let Some((sym, f)) = sign_scaled(&w.folded) {
                factor *= f;
                (sym.to_string(), 1)
            } else if let Some((stem, p)) = digit_power(&w.written)
                && (symbol_known(&stem) || word_symbol(&stem).is_some())
            {
                power = p;
                (
                    if symbol_known(&stem) {
                        stem
                    } else {
                        word_symbol(&stem).map_or(stem, str::to_string)
                    },
                    1,
                )
            } else if symbol_known(&w.written) {
                (w.written.clone(), 1)
            } else {
                return Err(WordRefusal::Unknown(run(&words[i..])));
            }
        }
    };
    i += len;
    if let Some(w) = words.get(i)
        && let Some((_, p)) = POWERS_AFTER
            .iter()
            .find(|(forms, _)| forms.contains(&w.as_str()))
    {
        power = *p;
        i += 1;
    }
    if power > 1 {
        unit = format!("{unit}^{power}");
    }
    Ok((unit, factor, i))
}

/// A tail of words as a unit: `[scale [of]] phrase {per phrase}
/// [scale]`. `symbol_known` says whether a bare word is a symbol the
/// run's table reads (`km`, `EUR`), `resolve` the override. `None`
/// when the tail is empty.
pub fn parse_unit_words(
    tail: &str,
    resolve: Override<'_>,
    symbol_known: &dyn Fn(&str) -> bool,
) -> Result<Option<WordUnit>, WordRefusal> {
    let words = tokenize(tail);
    if words.is_empty() {
        return Ok(None);
    }
    let mut i = 0;
    let mut factor = 1.0;
    let (first, f, next) = phrase(&words, i, resolve, symbol_known)?;
    factor *= f;
    i = next;
    let mut expr = first;
    while let Some(w) = words.get(i) {
        if PER.contains(&w.as_str()) {
            let (term, f, next) = phrase(&words, i + 1, resolve, symbol_known)?;
            factor *= f;
            expr = format!("{expr}/{term}");
            i = next;
        } else if let Some((_, f)) = SCALES.iter().find(|(forms, _)| forms.contains(&w.as_str())) {
            // a scale after the unit (`EUR million`)
            factor *= f;
            i += 1;
        } else {
            return Err(WordRefusal::Unknown(run(&words[i..])));
        }
    }
    Ok(Some(WordUnit { expr, factor }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(w: &str) -> bool {
        // the table's symbols, the Cyrillic designations among them
        matches!(
            w,
            "km" | "m" | "м" | "ft" | "s" | "t" | "a" | "eur" | "usd" | "ft3"
        )
    }
    fn none(_: &str) -> Option<String> {
        None
    }

    fn read(tail: &str) -> Result<Option<WordUnit>, WordRefusal> {
        parse_unit_words(tail, &none, &known)
    }

    #[test]
    fn the_phrases_read() {
        assert_eq!(
            read("metric tonnes per year").unwrap().unwrap(),
            WordUnit {
                expr: "t/a".into(),
                factor: 1.0
            }
        );
        assert_eq!(
            read("[millions of cubic feet per second]")
                .unwrap()
                .unwrap(),
            WordUnit {
                expr: "ft^3/s".into(),
                factor: 1e6
            }
        );
        assert_eq!(
            read("EUR million").unwrap().unwrap(),
            WordUnit {
                expr: "EUR".into(),
                factor: 1e6
            }
        );
        assert_eq!(
            read("$m").unwrap().unwrap(),
            WordUnit {
                expr: "USD".into(),
                factor: 1e6
            }
        );
        assert_eq!(
            read("thousand km").unwrap().unwrap(),
            WordUnit {
                expr: "km".into(),
                factor: 1e3
            }
        );
        assert_eq!(
            read("square kilometres").unwrap().unwrap(),
            WordUnit {
                expr: "km^2".into(),
                factor: 1.0
            }
        );
        assert_eq!(
            read("ft3").unwrap().unwrap(),
            WordUnit {
                expr: "ft^3".into(),
                factor: 1.0
            }
        );
        assert_eq!(
            read("тонн в год").unwrap().unwrap(),
            WordUnit {
                expr: "t/a".into(),
                factor: 1.0
            }
        );
        // a Cyrillic designation stays as written: the table reads it,
        // the display canonicalizes
        assert_eq!(
            read("млн м3").unwrap().unwrap(),
            WordUnit {
                expr: "м^3".into(),
                factor: 1e6
            }
        );
        assert_eq!(
            read("тыс. руб.").unwrap().unwrap(),
            WordUnit {
                expr: "RUB".into(),
                factor: 1e3
            }
        );
        assert_eq!(
            read("percent").unwrap().unwrap(),
            WordUnit {
                expr: "%".into(),
                factor: 1.0
            }
        );
        assert_eq!(
            read("tons").unwrap().unwrap(),
            WordUnit {
                expr: "ton".into(),
                factor: 1.0
            }
        );
    }

    #[test]
    fn what_does_not_read_is_told() {
        assert_eq!(
            read("tonnes per yer").unwrap_err().told(),
            "'yer' is no unit"
        );
        assert_eq!(
            read("pounds").unwrap_err().told(),
            "'pounds' is ambiguous: write £ (sterling) or lb (the mass)"
        );
        assert!(read("people").is_err());
        assert_eq!(read("").unwrap(), None);
    }

    #[test]
    fn an_override_comes_first() {
        let over = |w: &str| (w == "pounds").then(|| "lb".to_string());
        assert_eq!(
            parse_unit_words("pounds", &over, &known).unwrap().unwrap(),
            WordUnit {
                expr: "lb".into(),
                factor: 1.0
            }
        );
        let over = |w: &str| (w == "tons").then(|| "t".to_string());
        assert_eq!(
            parse_unit_words("tons per year", &over, &known)
                .unwrap()
                .unwrap(),
            WordUnit {
                expr: "t/a".into(),
                factor: 1.0
            }
        );
    }
}
