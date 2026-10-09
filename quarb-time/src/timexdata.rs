//! The lists `timex` reads (spec: Time Expressions and the
//! Timeline): per locale, the unit words in the inflected forms
//! carried, the numerals from one to a hundred, and the rules —
//! templates over those lists and the month and weekday names of
//! [`crate::datenames`]. Data, not grammar: an expression outside
//! these lists is not found, and widening a list is the whole of
//! teaching the reader a new expression.
//!
//! Rule order is priority where two rules match the same stretch
//! of text (the earlier and the longer match win first): dates,
//! clock times, relatives, weekdays, vague words, durations.

use crate::period::Grain;
use crate::timex::Act::{Clock, ClockAt, ClockMin, Const, Date, Dur, Rel, Vague, Weekday};
use crate::timex::Kind::{Relative, Time};
use crate::timex::{Lang, Numerals, Part, Rule, r, rb, rn};

/// A relative with a fixed offset.
const fn rel(pat: &'static str, value: &'static str) -> Rule {
    r(pat, Const(Relative, value, None))
}

/// A named hour.
const fn hour(pat: &'static str, value: &'static str) -> Rule {
    r(pat, Const(Time, value, Some(Grain::Hour)))
}

fn words(list: &[(&str, u32)]) -> Vec<(String, u32)> {
    list.iter().map(|(w, n)| (w.to_string(), *n)).collect()
}

/// Every spelling in `forms[i]` (separated by `/`) stands for
/// `first + i`.
fn run(first: u32, forms: &[&str]) -> Vec<(String, u32)> {
    forms
        .iter()
        .enumerate()
        .flat_map(|(i, f)| f.split('/').map(move |w| (w.to_string(), first + i as u32)))
        .collect()
}

pub fn lang(code: &str) -> Option<&'static Lang> {
    Some(match code {
        "en" => &EN,
        "ru" => &RU,
        "de" => &DE,
        "fr" => &FR,
        "es" => &ES,
        "it" => &IT,
        "pt" => &PT,
        "tr" => &TR,
        "ar" => &AR,
        "zh" => &ZH,
        "ja" => &JA,
        _ => return None,
    })
}

// ---------------------------------------------------------------- English

fn en_numerals() -> Numerals {
    let mut simple = run(
        1,
        &[
            "one",
            "two",
            "three",
            "four",
            "five",
            "six",
            "seven",
            "eight",
            "nine",
            "ten",
            "eleven",
            "twelve",
            "thirteen",
            "fourteen",
            "fifteen",
            "sixteen",
            "seventeen",
            "eighteen",
            "nineteen",
        ],
    );
    simple.extend(words(&[
        ("hundred", 100),
        ("a hundred", 100),
        ("one hundred", 100),
    ]));
    Numerals {
        simple,
        tens: words(&[
            ("twenty", 20),
            ("thirty", 30),
            ("forty", 40),
            ("fifty", 50),
            ("sixty", 60),
            ("seventy", 70),
            ("eighty", 80),
            ("ninety", 90),
        ]),
        ones: run(
            1,
            &[
                "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
            ],
        ),
        sep: r"[\s\-]+",
        one: vec!["a".into(), "an".into()],
        hour_one: Vec::new(),
    }
}

static EN: Lang = Lang {
    spaced: true,
    abbr: true,
    units: [
        &["second", "seconds"],
        &["minute", "minutes"],
        &["hour", "hours"],
        &["day", "days"],
        &["week", "weeks"],
        &["month", "months"],
        &["year", "years"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "(?:st|nd|rd|th)?",
    numerals: en_numerals,
    above: &[
        "hundred",
        "hundred and",
        "thousand",
        "thousand and",
        "million",
    ],
    hour_ordinals: &[],
    parts: &[
        // Written without the last point: the boundary falls
        // before it (see `Compiled::new`).
        ("a.m", Part::Am),
        ("a. m", Part::Am),
        ("am", Part::Am),
        ("in the morning", Part::Am),
        ("p.m", Part::Pm),
        ("p. m", Part::Pm),
        ("pm", Part::Pm),
        ("in the afternoon", Part::Pm),
        ("in the evening", Part::Pm),
        ("at night", Part::Night),
    ],
    rules: &[
        // With its weekday where one is written beside it: the text
        // then holds both, and the two can be checked against each
        // other.
        r("(on )?{W},? (the )?{d} (of )?{M}(,? {y})?", Date),
        r("(on )?{W},? {M} {d}(,? {y})?", Date),
        r("(the )?{d} (of )?{M},? {y}", Date),
        r("{M} {d},? {y}", Date),
        r("(the )?{d} (of )?{M}", Date),
        r("{M} {d}", Date),
        r("(in |since |by |until |during |of )?{M},? {y}", Date),
        r("(in|since|by|until|during) {y}", Date),
        r("the year {y}", Date),
        r("in {M}", Date),
        // The day alone, unless its month follows (`on the 7th of
        // October` is the fuller date's to read).
        rb("on the {d}", Date, &["of", "{M}"]),
        r("{t}(\\s*{p})?", Clock(0)),
        // `8.45 p.m.`: the point as the separator, read only beside
        // a.m. or p.m. (a bare `8.45` is a number).
        r(
            "(?P<h>[01]?[0-9]|2[0-3])\\.(?P<m>[0-5][0-9])\\s*{p}",
            Clock(0),
        ),
        r("{n} minutes (past|after) {H}( {p})?", ClockMin(1)),
        r("{n} minutes (before|to|of) {H}( {p})?", ClockMin(-1)),
        r("half[\\s-]past {H}( {p})?", Clock(30)),
        r("(a )?quarter (past|after) {H}( {p})?", Clock(15)),
        r("(a )?quarter (to|before|of) {H}( {p})?", Clock(-15)),
        r("{H} o'clock( {p})?", Clock(0)),
        r("{Hd}\\s*{p}", Clock(0)),
        r("{Hw} {p}", Clock(0)),
        hour("noon|midday|mid-day", "T12"),
        hour("midnight", "T00"),
        rel("the day after tomorrow", "+P2D"),
        rel("the day before yesterday", "-P2D"),
        rel("to-?morrow", "+P1D"),
        rel("yesterday", "-P1D"),
        rel("to-?day", "+P0D"),
        rel("(the |that )?same day", "+P0D"),
        rel(
            "(the |on the )?(next|following) (morning|afternoon|evening|night)",
            "+P1D",
        ),
        rel("the morning after", "+P1D"),
        rel(
            "(the )?(previous|preceding) (morning|afternoon|evening|night)",
            "-P1D",
        ),
        rel("last (night|evening)", "-P1D"),
        rel("the (night|evening) before", "-P1D"),
        r(
            "{a} {u} (later|afterwards|afterward|after|hence|thereafter)",
            Rel(1),
        ),
        r("{a} {u} from now", Rel(1)),
        r("after {a} {u}", Rel(1)),
        r("{a} {u} (earlier|before|ago|previously)", Rel(-1)),
        // A bare unit is one, in the singular (`the following
        // days` is no day).
        r(
            "(the |on the )?(next|following) (?P<u>second|minute|hour|day|week|month|year)",
            Rel(1),
        ),
        r(
            "the (?P<u>second|minute|hour|day|week|month|year) after",
            Rel(1),
        ),
        r(
            "the (previous|preceding) (?P<u>second|minute|hour|day|week|month|year)",
            Rel(-1),
        ),
        rel("last week", "-P1W"),
        rel("last month", "-P1M"),
        rel("last year", "-P1Y"),
        r(
            "the (?P<u>second|minute|hour|day|week|month|year) before",
            Rel(-1),
        ),
        r("(the |on the )?(next|following) {W}", Weekday(1)),
        r("(the )?(previous|preceding|last) {W}", Weekday(-1)),
        r(
            "that (morning|afternoon|evening|night|day|week|month|year)",
            Vague,
        ),
        r("(the |that )?same (morning|afternoon|evening|night)", Vague),
        r("one (day|morning|afternoon|evening|night)", Vague),
        r("this (morning|afternoon|evening)", Vague),
        r("to-?night", Vague),
        r("in the (morning|afternoon|evening|night)", Vague),
        r(
            "at (dawn|dusk|sunrise|sunset|daybreak|nightfall|night)",
            Vague,
        ),
        r(
            "(some|a few|several|many) {u}( (later|afterwards|afterward|after|before|earlier|ago))?",
            Vague,
        ),
        r(
            "some time (later|afterwards|afterward|after|before|earlier|ago)",
            Vague,
        ),
        r("soon( after(wards?)?)?", Vague),
        r("shortly (after(wards?)?|before)", Vague),
        r("long ago", Vague),
        r("once upon a time", Vague),
        r("recently|lately", Vague),
        r("(on )?{W}", Vague),
        // `in eighty days` is the span a thing takes (the French
        // original has `en`, where a displacement would be `dans`).
        r("(for|in|within) {a} {u}", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- Russian

fn ru_numerals() -> Numerals {
    let ones = run(
        1,
        &[
            "один/одна/одно/одну/одного/одной/одному/одним/одном",
            "два/две/двух/двум/двумя",
            "три/трёх/трех/трём/трем/тремя",
            "четыре/четырёх/четырех/четырём/четырем/четырьмя",
            "пять/пяти/пятью",
            "шесть/шести/шестью",
            "семь/семи/семью",
            "восемь/восьми/восемью/восьмью",
            "девять/девяти/девятью",
        ],
    );
    let mut simple = ones.clone();
    simple.extend(run(
        10,
        &[
            "десять/десяти/десятью",
            "одиннадцать/одиннадцати/одиннадцатью",
            "двенадцать/двенадцати/двенадцатью",
            "тринадцать/тринадцати/тринадцатью",
            "четырнадцать/четырнадцати/четырнадцатью",
            "пятнадцать/пятнадцати/пятнадцатью",
            "шестнадцать/шестнадцати/шестнадцатью",
            "семнадцать/семнадцати/семнадцатью",
            "восемнадцать/восемнадцати/восемнадцатью",
            "девятнадцать/девятнадцати/девятнадцатью",
        ],
    ));
    simple.extend(words(&[("сто", 100), ("ста", 100)]));
    let mut tens = Vec::new();
    for (n, forms) in [
        (20, "двадцать/двадцати/двадцатью"),
        (30, "тридцать/тридцати/тридцатью"),
        (40, "сорок/сорока"),
        (50, "пятьдесят/пятидесяти/пятьюдесятью"),
        (60, "шестьдесят/шестидесяти/шестьюдесятью"),
        (70, "семьдесят/семидесяти/семьюдесятью"),
        (80, "восемьдесят/восьмидесяти/восемьюдесятью/восьмьюдесятью"),
        (90, "девяносто/девяноста"),
    ] {
        tens.extend(forms.split('/').map(|f| (f.to_string(), n)));
    }
    Numerals {
        simple,
        tens,
        ones,
        sep: r"\s+",
        one: Vec::new(),
        hour_one: Vec::new(),
    }
}

static RU: Lang = Lang {
    spaced: true,
    abbr: true,
    units: [
        &[
            "секунда",
            "секунду",
            "секунды",
            "секунд",
            "секундой",
            "секундами",
        ],
        &["минута", "минуту", "минуты", "минут", "минутой", "минутами"],
        &["час", "часа", "часов", "часом", "часами"],
        &[
            "день",
            "дня",
            "дней",
            "днём",
            "днем",
            "днями",
            "сутки",
            "суток",
            "сутками",
        ],
        &[
            "неделя",
            "неделю",
            "недели",
            "недель",
            "неделей",
            "неделями",
        ],
        &["месяц", "месяца", "месяцев", "месяцем", "месяцами"],
        &["год", "года", "лет", "годом", "годами"],
    ],
    months: &[
        (1, "январе"),
        (2, "феврале"),
        (3, "марте"),
        (4, "апреле"),
        (5, "мае"),
        (6, "июне"),
        (7, "июле"),
        (8, "августе"),
        (9, "сентябре"),
        (10, "октябре"),
        (11, "ноябре"),
        (12, "декабре"),
    ],
    weekdays: &[
        (2, "среду"),
        (2, "середа"),
        (2, "середу"),
        (4, "пятницу"),
        (5, "субботу"),
    ],
    ordinal: "(?:-?(?:го|е|ое))?",
    numerals: ru_numerals,
    above: &[
        "двести",
        "триста",
        "четыреста",
        "пятьсот",
        "шестьсот",
        "семьсот",
        "восемьсот",
        "девятьсот",
        "тысяча",
        "тысячи",
        "тысяч",
        "тысячу",
    ],
    hour_ordinals: &[
        ("первого", 1),
        ("перваго", 1),
        ("второго", 2),
        ("втораго", 2),
        ("третьего", 3),
        ("третьяго", 3),
        ("четвёртого", 4),
        ("четвертого", 4),
        ("четвертаго", 4),
        ("пятого", 5),
        ("пятаго", 5),
        ("шестого", 6),
        ("шестаго", 6),
        ("седьмого", 7),
        ("седьмаго", 7),
        ("восьмого", 8),
        ("восьмаго", 8),
        ("девятого", 9),
        ("девятаго", 9),
        ("десятого", 10),
        ("десятаго", 10),
        ("одиннадцатого", 11),
        ("одиннадцатаго", 11),
        ("двенадцатого", 12),
        ("двенадцатаго", 12),
    ],
    parts: &[
        ("утра", Part::Am),
        ("утром", Part::Am),
        ("вечером", Part::Pm),
        ("днём", Part::Pm),
        ("днем", Part::Pm),
        ("ночью", Part::Night),
        ("пополуночи", Part::Am),
        ("дня", Part::Pm),
        ("вечера", Part::Pm),
        ("пополудни", Part::Pm),
        ("ночи", Part::Night),
    ],
    rules: &[
        r("(в |во )?{W},? {d} {M}( {y})?( года| г\\.)?", Date),
        r("{d} {M} {y}( года| г\\.)?", Date),
        r("{d} {M}", Date),
        r("{M} {d}", Date),
        r("(в |к |с |до )?{M} {y}( года| г\\.)?", Date),
        r("(в |к |с |до )?{y} (году|года|год|г\\.)", Date),
        r("в {M}", Date),
        // The day alone, where the word says it is a day of the
        // month (a bare `8-го` may be the eighth of anything).
        r("{dd}(-го)? числа", Date),
        r("{dd}\\.{mm}\\.{y}", Date),
        r("(в )?{t}( {p})?", Clock(0)),
        // The minutes of the hour under way (`сорок минут
        // девятого` is 8:40), its half and quarter, and the
        // minutes short of the hour (`без четверти девять`).
        r("{n} (минут|минуты|минуту) {O}( {p})?", ClockMin(1)),
        r("(в )?(половина|половине|половину) {O}( {p})?", Clock(30)),
        r("(в )?четверть {O}( {p})?", Clock(15)),
        r("(в )?без четверти {H}( {p})?", Clock(-15)),
        r("(в )?без {n} (минут )?{H}( {p})?", ClockMin(-1)),
        r("в начале {O}", Vague),
        r("в {H} (час|часа|часов)( {p})?", Clock(0)),
        r("{H} (час|часа|часов) {p}", Clock(0)),
        hour("в час ночи", "T01"),
        hour("в час дня", "T13"),
        hour("(в )?полдень", "T12"),
        hour("(в )?полночь", "T00"),
        rel("послезавтра", "+P2D"),
        rel("позавчера|третьего дня", "-P2D"),
        rel(
            "(на|в) следующий день|на другой день|назавтра|на завтра|завтра",
            "+P1D",
        ),
        rel("на следующее утро|на другое утро|наутро", "+P1D"),
        rel("вчера|накануне", "-P1D"),
        rel("прошлой ночью|прошлым вечером", "-P1D"),
        rel("сегодня", "+P0D"),
        rel("на следующей неделе", "+P1W"),
        rel("на прошлой неделе", "-P1W"),
        rel("в следующем месяце", "+P1M"),
        rel("в прошлом месяце", "-P1M"),
        rel("в следующем году|на следующий год|на другой год", "+P1Y"),
        rel("в прошлом году", "-P1Y"),
        r("(через|спустя) {n} {u}", Rel(1)),
        r("{n} {u} (спустя|позже|позднее)", Rel(1)),
        // A bare unit is one, in the singular the phrase governs
        // (`час спустя`, `часом позже`); `четверть часа спустя`
        // is not an hour.
        r(
            "(через|спустя) (?P<u>секунду|минуту|час|день|сутки|неделю|месяц|год)",
            Rel(1),
        ),
        r(
            "(?P<u>секунду|минуту|час|день|сутки|неделю|месяц|год) спустя",
            Rel(1),
        ),
        r(
            "(?P<u>секундой|минутой|часом|днём|днем|неделей|месяцем|годом) (позже|позднее)",
            Rel(1),
        ),
        r("{n} {u} (назад|тому назад|раньше|ранее)", Rel(-1)),
        r(
            "(?P<u>секунду|минуту|час|день|сутки|неделю|месяц|год) (назад|тому назад)",
            Rel(-1),
        ),
        r(
            "(?P<u>секундой|минутой|часом|днём|днем|неделей|месяцем|годом) (раньше|ранее)",
            Rel(-1),
        ),
        r("за {n} {u} до (этого|того)", Rel(-1)),
        r(
            "за (?P<u>секунду|минуту|час|день|сутки|неделю|месяц|год) до (этого|того)",
            Rel(-1),
        ),
        r("(в|на) следующ(ий|ую|ее) {W}", Weekday(1)),
        r(
            "в (прошлый|прошлую|прошлое|минувший|минувшую|минувшее) {W}",
            Weekday(-1),
        ),
        r("в тот (же )?(вечер|день|год|час|миг|момент)", Vague),
        r("в то (же )?(утро|время|лето)", Vague),
        r(
            "в ту (же )?(ночь|зиму|осень|весну|пору|минуту|неделю)",
            Vague,
        ),
        r("тем (же )?(вечером|утром|летом)", Vague),
        r("той (же )?(ночью|зимой|осенью|весной)", Vague),
        r("однажды|как-то раз", Vague),
        r("вскоре|недавно|давно|на днях", Vague),
        r("(через|спустя) несколько {u}", Vague),
        r("несколько {u}( (спустя|назад|позже|раньше|ранее))?", Vague),
        r("некоторое время спустя|через некоторое время", Vague),
        r("утром|вечером|ночью", Vague),
        r("(в |во )?{W}", Vague),
        r("в течение {n} {u}", Dur),
        r(
            "в течение (?P<u>секунды|минуты|часа|дня|суток|недели|месяца|года)",
            Dur,
        ),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- German

fn de_numerals() -> Numerals {
    let units = [
        "ein", "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun",
    ];
    let mut simple = run(
        1,
        &[
            "eins",
            "zwei",
            "drei",
            "vier",
            "fünf",
            "sechs",
            "sieben",
            "acht",
            "neun",
            "zehn",
            "elf",
            "zwölf",
            "dreizehn",
            "vierzehn",
            "fünfzehn",
            "sechzehn",
            "siebzehn",
            "achtzehn",
            "neunzehn",
        ],
    );
    for (n, tens) in [
        (20, "zwanzig"),
        (30, "dreißig/dreissig"),
        (40, "vierzig"),
        (50, "fünfzig"),
        (60, "sechzig"),
        (70, "siebzig"),
        (80, "achtzig"),
        (90, "neunzig"),
    ] {
        for t in tens.split('/') {
            simple.push((t.to_string(), n));
            for (i, u) in units.iter().enumerate() {
                simple.push((format!("{u}und{t}"), n + i as u32 + 1));
            }
        }
    }
    simple.extend(words(&[("hundert", 100), ("einhundert", 100)]));
    Numerals {
        simple,
        one: ["ein", "eine", "einen", "einem", "einer", "eines"]
            .iter()
            .map(|w| w.to_string())
            .collect(),
        hour_one: vec!["ein".into()],
        ..Numerals::default()
    }
}

static DE: Lang = Lang {
    spaced: true,
    abbr: true,
    units: [
        &["Sekunde", "Sekunden"],
        &["Minute", "Minuten"],
        &["Stunde", "Stunden"],
        &["Tag", "Tage", "Tagen", "Tages", "Tags"],
        &["Woche", "Wochen"],
        &["Monat", "Monate", "Monaten", "Monats"],
        &["Jahr", "Jahre", "Jahren", "Jahres", "Jahrs"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "\\.?",
    numerals: de_numerals,
    above: &["hundert", "tausend", "und"],
    hour_ordinals: &[],
    parts: &[
        ("morgens", Part::Am),
        ("vormittags", Part::Am),
        ("nachmittags", Part::Pm),
        ("abends", Part::Pm),
        ("nachts", Part::Night),
    ],
    rules: &[
        r("{d} {M} {y}", Date),
        r("{d} {M}", Date),
        r("(im )?{M} {y}", Date),
        r("(im Jahre?|anno) {y}", Date),
        r("im {M}", Date),
        r("{dd}\\.{mm}\\.{y}", Date),
        r("(um )?{t}( Uhr)?", Clock(0)),
        r("(um )?{H} Uhr( {p})?", Clock(0)),
        r("(um )?halb {H}", Clock(-30)),
        hour("(um )?Mitternacht", "T00"),
        rel("übermorgen", "+P2D"),
        rel("vorgestern", "-P2D"),
        rel(
            "am (nächsten|folgenden|anderen) (Tag|Tage|Morgen|Abend)|tags darauf",
            "+P1D",
        ),
        rn(
            "(?-i:morgen)",
            Const(Relative, "+P1D", None),
            &["heute", "gestern"],
        ),
        rel(
            "gestern|am Vortag|am Tage? zuvor|tags zuvor|am Vorabend",
            "-P1D",
        ),
        rel("heute", "+P0D"),
        rel("(in der )?nächsten? Woche", "+P1W"),
        rel("(in der )?(letzten?|vorigen?) Woche", "-P1W"),
        rel("(im )?nächsten Monat", "+P1M"),
        rel("(im )?(letzten|vorigen) Monat", "-P1M"),
        rel(
            "nächstes Jahr|im (nächsten|folgenden) Jahr|im Jahr darauf",
            "+P1Y",
        ),
        rel(
            "(letztes|voriges) Jahr|im (letzten|vorigen) Jahr|im Jahr zuvor",
            "-P1Y",
        ),
        r("{a} {u} (später|danach|darauf|nachher)", Rel(1)),
        r("(nach|in) {a} {u}", Rel(1)),
        r("{a} {u} (früher|zuvor|vorher|davor)", Rel(-1)),
        r("vor {a} {u}", Rel(-1)),
        r("(am )?(nächsten|folgenden|kommenden) {W}", Weekday(1)),
        r("(am )?(letzten|vorigen|vergangenen) {W}", Weekday(-1)),
        r("an jenem (Abend|Tag|Tage|Morgen)|in jener Nacht", Vague),
        r("am selben (Tag|Tage|Abend|Morgen)", Vague),
        r("eines (Tages|Abends|Morgens|Nachts)", Vague),
        r("bald( darauf)?|kurz (darauf|danach|zuvor)", Vague),
        r("(nach|vor) (einigen|mehreren|wenigen) {u}", Vague),
        r(
            "(einige|mehrere|wenige|ein paar) {u}( (später|danach|darauf|zuvor|früher|vorher))?",
            Vague,
        ),
        r("neulich|kürzlich|damals", Vague),
        r("am (Abend|Morgen|Nachmittag|Vormittag)|in der Nacht", Vague),
        r("abends|morgens|nachts|mittags", Vague),
        r("(am )?{W}", Vague),
        r("{a} {u} lang", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- French

fn fr_numerals() -> Numerals {
    let low = [
        "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix", "onze",
        "douze", "treize", "quatorze", "quinze", "seize", "dix-sept", "dix-huit", "dix-neuf",
    ];
    // `un` / `une` stand for one only where a rule lets the article
    // count (`une heure après`): see `Numerals::one`.
    let mut simple: Vec<(String, u32)> = low
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, w)| (w.to_string(), i as u32 + 1))
        .collect();
    for (n, t) in [
        (20, "vingt"),
        (30, "trente"),
        (40, "quarante"),
        (50, "cinquante"),
        (60, "soixante"),
    ] {
        simple.push((t.to_string(), n));
        simple.push((format!("{t} et un"), n + 1));
        simple.push((format!("{t} et une"), n + 1));
        for k in 2..=9 {
            simple.push((format!("{t}-{}", low[k - 1]), n + k as u32));
        }
    }
    // 70–79 on soixante, 80–99 on quatre-vingt.
    simple.push(("soixante-dix".into(), 70));
    simple.push(("soixante et onze".into(), 71));
    for k in 12..=19 {
        simple.push((format!("soixante-{}", low[k - 1]), 60 + k as u32));
    }
    simple.push(("quatre-vingts".into(), 80));
    simple.push(("quatre-vingt".into(), 80));
    for k in 1..=19 {
        simple.push((format!("quatre-vingt-{}", low[k - 1]), 80 + k as u32));
    }
    simple.push(("cent".into(), 100));
    Numerals {
        simple,
        one: vec!["un".into(), "une".into()],
        hour_one: vec!["une".into()],
        ..Numerals::default()
    }
}

static FR: Lang = Lang {
    spaced: true,
    abbr: false,
    units: [
        &["seconde", "secondes"],
        &["minute", "minutes"],
        &["heure", "heures"],
        &["jour", "jours", "journée", "journées"],
        &["semaine", "semaines"],
        &["mois"],
        &["an", "ans", "année", "années"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "(?:er)?",
    numerals: fr_numerals,
    above: &["cents", "mille"],
    hour_ordinals: &[],
    parts: &[
        ("du matin", Part::Am),
        ("matin", Part::Am),
        ("de l'après-midi", Part::Pm),
        ("après-midi", Part::Pm),
        ("après midi", Part::Pm),
        ("de relevée", Part::Pm),
        ("du soir", Part::Pm),
        ("soir", Part::Pm),
        ("de la nuit", Part::Night),
    ],
    rules: &[
        r("(le )?{W} {d} {M}( {y})?", Date),
        r("(le )?{d} {M} {y}", Date),
        r("(le )?{d} {M}", Date),
        r("(en )?{M} {y}", Date),
        r("en {y}", Date),
        r("(l'an|l'année) {y}", Date),
        r("(en |au mois de |au mois d'){M}", Date),
        r("{dd}/{mm}/{y}", Date),
        r("{t}", Clock(0)),
        r("{H}\\s*h\\s*(?P<m>[0-5][0-9])", Clock(0)),
        // The minutes after the hour, in words or digits (`huit
        // heures quarante-cinq`, `8 heures 45 soir`), and the
        // fractions.
        r("((à|vers) )?{H} heures? {n}( minutes)?( {p})?", ClockMin(1)),
        r("((à|vers) )?{H} heures? moins {n}( {p})?", ClockMin(-1)),
        r("((à|vers) )?{H} heures? et demie( {p})?", Clock(30)),
        r("((à|vers) )?{H} heures? (et|un) quart( {p})?", Clock(15)),
        r(
            "((à|vers) )?{H} heures? moins (le|un) quart( {p})?",
            Clock(-15),
        ),
        r("(à|vers) {H} heures?( {p})?", Clock(0)),
        r("{H} heures? {p}", Clock(0)),
        r("midi et demie?", Const(Time, "T12:30", Some(Grain::Minute))),
        r(
            "minuit et demie?",
            Const(Time, "T00:30", Some(Grain::Minute)),
        ),
        r("midi moins {n}", ClockAt(12, -1)),
        r("minuit moins {n}", ClockAt(0, -1)),
        hour("midi", "T12"),
        hour("minuit", "T00"),
        rel("après-demain|le surlendemain", "+P2D"),
        rel("avant-hier|l'avant-veille", "-P2D"),
        rel(
            "le lendemain( matin| soir)?|le jour (suivant|d'après)|demain",
            "+P1D",
        ),
        rel(
            "la veille( au soir)?|le jour (précédent|d'avant)|hier",
            "-P1D",
        ),
        rel("aujourd'hui", "+P0D"),
        rel("la semaine (suivante|prochaine|d'après)", "+P1W"),
        rel("la semaine (précédente|dernière|passée|d'avant)", "-P1W"),
        rel("le mois (suivant|prochain)", "+P1M"),
        rel("le mois (précédent|dernier|passé)", "-P1M"),
        rel("l'année (suivante|prochaine|d'après)|l'an prochain", "+P1Y"),
        rel(
            "l'année (précédente|dernière|passée)|l'an (dernier|passé)",
            "-P1Y",
        ),
        r("{a} {u} (plus tard|après)", Rel(1)),
        r("(après|au bout de|dans) {a} {u}", Rel(1)),
        r("{a} {u} (plus tôt|auparavant|avant)", Rel(-1)),
        r("il y a {a} {u}", Rel(-1)),
        r("(le )?{W} (suivant|prochain|d'après)", Weekday(1)),
        r("(le )?{W} (précédent|dernier|passé|d'avant)", Weekday(-1)),
        r(
            "ce (soir|matin|jour)-là|cette (nuit|année|semaine)-là",
            Vague,
        ),
        r("ce soir|ce matin|cette nuit", Vague),
        r("un (jour|soir|matin)|une (nuit|fois)", Vague),
        r("bientôt|peu (de temps )?(après|avant)", Vague),
        r("il y a (quelques|plusieurs) {u}", Vague),
        r(
            "(quelques|plusieurs) {u}( (plus tard|après|plus tôt|auparavant|avant))?",
            Vague,
        ),
        r("récemment|naguère|jadis|autrefois", Vague),
        r("le (soir|matin)", Vague),
        r("(le )?{W}", Vague),
        r("(pendant|durant) {a} {u}", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- Spanish

fn es_numerals() -> Numerals {
    let mut simple = run(
        1,
        &[
            "uno",
            "dos",
            "tres",
            "cuatro",
            "cinco",
            "seis",
            "siete",
            "ocho",
            "nueve",
            "diez",
            "once",
            "doce",
            "trece",
            "catorce",
            "quince",
            "dieciséis/dieciseis",
            "diecisiete",
            "dieciocho",
            "diecinueve",
            "veinte",
            "veintiuno/veintiún/veintiun/veintiuna",
            "veintidós/veintidos",
            "veintitrés/veintitres",
            "veinticuatro",
            "veinticinco",
            "veintiséis/veintiseis",
            "veintisiete",
            "veintiocho",
            "veintinueve",
        ],
    );
    let ones = [
        "uno/un/una",
        "dos",
        "tres",
        "cuatro",
        "cinco",
        "seis",
        "siete",
        "ocho",
        "nueve",
    ];
    for (n, t) in [
        (30, "treinta"),
        (40, "cuarenta"),
        (50, "cincuenta"),
        (60, "sesenta"),
        (70, "setenta"),
        (80, "ochenta"),
        (90, "noventa"),
    ] {
        simple.push((t.to_string(), n));
        for (i, forms) in ones.iter().enumerate() {
            for o in forms.split('/') {
                simple.push((format!("{t} y {o}"), n + i as u32 + 1));
            }
        }
    }
    simple.extend(words(&[("cien", 100), ("ciento", 100)]));
    Numerals {
        simple,
        one: vec!["un".into(), "una".into()],
        hour_one: vec!["una".into()],
        ..Numerals::default()
    }
}

static ES: Lang = Lang {
    spaced: true,
    abbr: false,
    units: [
        &["segundo", "segundos"],
        &["minuto", "minutos"],
        &["hora", "horas"],
        &["día", "días", "dia", "dias"],
        &["semana", "semanas"],
        &["mes", "meses"],
        &["año", "años"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "º?",
    numerals: es_numerals,
    above: &[
        "doscientos",
        "trescientos",
        "cuatrocientos",
        "quinientos",
        "seiscientos",
        "setecientos",
        "ochocientos",
        "novecientos",
        "mil",
    ],
    hour_ordinals: &[],
    parts: &[
        ("de la mañana", Part::Am),
        ("de la madrugada", Part::Am),
        ("de la tarde", Part::Pm),
        ("de la noche", Part::Night),
    ],
    rules: &[
        r("(el )?{d} de {M} (de|del) {y}", Date),
        r("(el )?{d} de {M}", Date),
        r("(en )?{M} (de|del) {y}", Date),
        r("en (el año )?{y}", Date),
        r("(el )?año {y}", Date),
        r("en {M}", Date),
        r("{dd}/{mm}/{y}", Date),
        r("(a las? )?{t}", Clock(0)),
        r("a las? {H}( {p})?", Clock(0)),
        hour("(al )?mediodía", "T12"),
        hour("(a )?medianoche", "T00"),
        rel("pasado mañana", "+P2D"),
        rel("anteayer|antes de ayer", "-P2D"),
        rel(
            "(al|el) día siguiente|al otro día|(a )?la mañana siguiente",
            "+P1D",
        ),
        rn(
            "mañana",
            Const(Relative, "+P1D", None),
            &[
                "la", "esta", "esa", "aquella", "una", "cada", "de", "misma", "buena",
            ],
        ),
        rel("ayer|anoche|la víspera|el día anterior", "-P1D"),
        rel("hoy", "+P0D"),
        rel("la semana (siguiente|próxima|que viene)", "+P1W"),
        rel("la semana (anterior|pasada)", "-P1W"),
        rel("el mes (siguiente|próximo|que viene)", "+P1M"),
        rel("el mes (anterior|pasado)", "-P1M"),
        rel("(el|al) año (siguiente|próximo|que viene)", "+P1Y"),
        rel("el año (anterior|pasado)", "-P1Y"),
        r("{a} {u} (después|más tarde)", Rel(1)),
        r(
            "(después de|tras|al cabo de|dentro de|a los|a las) {a} {u}",
            Rel(1),
        ),
        r("{a} {u} (antes|atrás)", Rel(-1)),
        r("hace {a} {u}", Rel(-1)),
        r("(el )?{W} (siguiente|próximo|que viene)", Weekday(1)),
        r("(el )?próximo {W}", Weekday(1)),
        r("(el )?{W} (anterior|pasado)", Weekday(-1)),
        r("(el )?pasado {W}", Weekday(-1)),
        r("(aquella|esa|esta) (noche|tarde|mañana|semana)", Vague),
        r("(aquel|ese) (día|año|mes)", Vague),
        r("un día|una (noche|tarde|mañana|vez)", Vague),
        r("pronto|poco (después|antes)", Vague),
        r("hace (unos|unas|algunos|algunas|varios|varias) {u}", Vague),
        r(
            "(unos|unas|algunos|algunas|varios|varias) {u}( (después|más tarde|antes|atrás))?",
            Vague,
        ),
        r("recientemente|antaño", Vague),
        r("por la (mañana|tarde|noche)", Vague),
        r("(el )?{W}", Vague),
        r("durante {a} {u}", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- Italian

fn it_numerals() -> Numerals {
    let ones = [
        "uno", "due", "tre", "quattro", "cinque", "sei", "sette", "otto", "nove",
    ];
    let mut simple = run(
        1,
        &[
            "uno",
            "due",
            "tre",
            "quattro",
            "cinque",
            "sei",
            "sette",
            "otto",
            "nove",
            "dieci",
            "undici",
            "dodici",
            "tredici",
            "quattordici",
            "quindici",
            "sedici",
            "diciassette",
            "diciotto",
            "diciannove",
        ],
    );
    for (n, t) in [
        (20, "venti"),
        (30, "trenta"),
        (40, "quaranta"),
        (50, "cinquanta"),
        (60, "sessanta"),
        (70, "settanta"),
        (80, "ottanta"),
        (90, "novanta"),
    ] {
        simple.push((t.to_string(), n));
        // The tens word loses its vowel before uno and otto.
        let stem = &t[..t.len() - 1];
        for (i, o) in ones.iter().enumerate() {
            let k = n + i as u32 + 1;
            match *o {
                "uno" => {
                    simple.push((format!("{stem}uno"), k));
                    simple.push((format!("{stem}un"), k));
                }
                "otto" => simple.push((format!("{stem}otto"), k)),
                "tre" => {
                    simple.push((format!("{t}tré"), k));
                    simple.push((format!("{t}tre"), k));
                }
                _ => simple.push((format!("{t}{o}"), k)),
            }
        }
    }
    simple.push(("cento".into(), 100));
    Numerals {
        simple,
        one: vec!["un".into(), "una".into()],
        hour_one: vec!["una".into()],
        ..Numerals::default()
    }
}

static IT: Lang = Lang {
    spaced: true,
    abbr: false,
    units: [
        &["secondo", "secondi"],
        &["minuto", "minuti"],
        &["ora", "ore"],
        &["giorno", "giorni"],
        &["settimana", "settimane"],
        &["mese", "mesi"],
        &["anno", "anni"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "º?",
    numerals: it_numerals,
    above: &["mille", "mila"],
    hour_ordinals: &[],
    parts: &[
        ("di mattina", Part::Am),
        ("del mattino", Part::Am),
        ("del pomeriggio", Part::Pm),
        ("di sera", Part::Pm),
        ("della sera", Part::Pm),
        ("di notte", Part::Night),
    ],
    rules: &[
        r("(il |l')?{d} {M} {y}", Date),
        r("(il |l')?{d} {M}", Date),
        r("(nel |di |in )?{M} (del )?{y}", Date),
        r("nel {y}", Date),
        r("(nell'anno|l'anno) {y}", Date),
        r("(in|a) {M}", Date),
        r("{dd}/{mm}/{y}", Date),
        r("(alle |all')?{t}", Clock(0)),
        r("alle {H}( {p})?", Clock(0)),
        hour("all'una del pomeriggio", "T13"),
        hour("all'una", "T01"),
        hour("(a )?mezzogiorno", "T12"),
        hour("(a )?mezzanotte", "T00"),
        rel("dopodomani", "+P2D"),
        rel("l'altro ieri|ieri l'altro|avantieri", "-P2D"),
        rel(
            "il giorno (dopo|seguente|successivo)|(all'|l')indomani|domani",
            "+P1D",
        ),
        rel("(la mattina|il mattino|la sera) (dopo|seguente)", "+P1D"),
        rel(
            "il giorno (prima|precedente|innanzi)|la vigilia|la (sera|notte) prima|ieri",
            "-P1D",
        ),
        rel("oggi", "+P0D"),
        rel(
            "la settimana (dopo|seguente|successiva|prossima|ventura)",
            "+P1W",
        ),
        rel("la settimana (prima|precedente|scorsa|passata)", "-P1W"),
        rel(
            "il mese (dopo|seguente|successivo|prossimo|venturo)",
            "+P1M",
        ),
        rel("il mese (prima|precedente|scorso|passato)", "-P1M"),
        rel("l'anno (dopo|seguente|successivo|prossimo|venturo)", "+P1Y"),
        rel("l'anno (prima|precedente|scorso|passato)", "-P1Y"),
        r("{a} {u} (dopo|più tardi)", Rel(1)),
        r("(dopo|fra|tra) {a} {u}", Rel(1)),
        r("{a} {u} (prima|addietro|innanzi|fa)", Rel(-1)),
        r(
            "(il |la )?{W} (dopo|seguente|successivo|successiva|prossimo|prossima)",
            Weekday(1),
        ),
        r(
            "(il |la )?{W} (prima|precedente|scorso|scorsa|passato|passata)",
            Weekday(-1),
        ),
        r("quella (sera|notte|mattina|settimana|volta)", Vague),
        r("quel (giorno|pomeriggio|mese)|quell'anno", Vague),
        r("stasera|stamattina|stanotte|stamani", Vague),
        r("un giorno|una (sera|notte|mattina|volta)", Vague),
        r("presto|poco (dopo|prima)", Vague),
        r(
            "(alcuni|alcune|parecchi|parecchie|pochi|poche|vari|varie) {u}( (dopo|più tardi|prima|fa))?",
            Vague,
        ),
        r(
            "qualche (giorno|settimana|mese|anno|ora|minuto)( (dopo|più tardi|prima|fa))?",
            Vague,
        ),
        r("di recente|recentemente|un tempo", Vague),
        r("(il |la )?{W}", Vague),
        r("per {a} {u}", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- Portuguese

fn pt_numerals() -> Numerals {
    let ones = [
        "um/uma",
        "dois/duas",
        "três/tres",
        "quatro",
        "cinco",
        "seis",
        "sete",
        "oito",
        "nove",
    ];
    let mut simple = run(1, &ones);
    simple.extend(run(
        10,
        &[
            "dez",
            "onze",
            "doze",
            "treze",
            "catorze/quatorze",
            "quinze",
            "dezasseis/dezesseis",
            "dezassete/dezessete",
            "dezoito",
            "dezanove/dezenove",
        ],
    ));
    for (n, tens) in [
        (20, "vinte"),
        (30, "trinta"),
        (40, "quarenta"),
        (50, "cinquenta/cinqüenta"),
        (60, "sessenta"),
        (70, "setenta"),
        (80, "oitenta"),
        (90, "noventa"),
    ] {
        for t in tens.split('/') {
            simple.push((t.to_string(), n));
            for (i, forms) in ones.iter().enumerate() {
                for o in forms.split('/') {
                    simple.push((format!("{t} e {o}"), n + i as u32 + 1));
                }
            }
        }
    }
    simple.extend(words(&[("cem", 100), ("cento", 100)]));
    simple.retain(|(f, _)| f != "um" && f != "uma");
    Numerals {
        simple,
        one: vec!["um".into(), "uma".into()],
        hour_one: vec!["uma".into()],
        ..Numerals::default()
    }
}

static PT: Lang = Lang {
    spaced: true,
    abbr: false,
    units: [
        &["segundo", "segundos"],
        &["minuto", "minutos"],
        &["hora", "horas"],
        &["dia", "dias"],
        &["semana", "semanas"],
        &["mês", "meses", "mes"],
        &["ano", "anos"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "º?",
    numerals: pt_numerals,
    above: &[
        "duzentos",
        "trezentos",
        "quatrocentos",
        "quinhentos",
        "seiscentos",
        "setecentos",
        "oitocentos",
        "novecentos",
        "mil",
        "cento e",
        "mil e",
    ],
    hour_ordinals: &[],
    parts: &[
        ("da manhã", Part::Am),
        ("da madrugada", Part::Am),
        ("da tarde", Part::Pm),
        ("da noite", Part::Night),
    ],
    rules: &[
        r("{d} de {M} de {y}", Date),
        r("{d} de {M}", Date),
        r("(em )?{M} de {y}", Date),
        r("em {y}", Date),
        r("(no )?ano (de )?{y}", Date),
        r("em {M}", Date),
        r("{dd}/{mm}/{y}", Date),
        r("(às |as )?{t}", Clock(0)),
        r("(às|as) {H}( horas)?( {p})?", Clock(0)),
        hour("à uma( hora)? da tarde", "T13"),
        hour("à uma( hora)?", "T01"),
        hour("(ao )?meio-dia", "T12"),
        hour("(à )?meia-noite", "T00"),
        rel("depois de amanhã", "+P2D"),
        rel("anteontem", "-P2D"),
        rel(
            "no dia seguinte|no outro dia|na manhã seguinte|amanhã",
            "+P1D",
        ),
        rel(
            "ontem|(na|a) véspera|no dia anterior|na noite anterior",
            "-P1D",
        ),
        rel("hoje", "+P0D"),
        rel(
            "na semana seguinte|na próxima semana|na semana que vem",
            "+P1W",
        ),
        rel("na semana (anterior|passada)", "-P1W"),
        rel("no mês seguinte|no próximo mês|no mês que vem", "+P1M"),
        rel("no mês (anterior|passado)", "-P1M"),
        rel("no ano seguinte|no próximo ano|no ano que vem", "+P1Y"),
        rel("no ano (anterior|passado)", "-P1Y"),
        r("{a} {u} (depois|mais tarde)", Rel(1)),
        r(
            "(depois de|após|passados|passadas|ao fim de|daqui a|dentro de) {a} {u}",
            Rel(1),
        ),
        r("{a} {u} (antes|atrás)", Rel(-1)),
        r("há {a} {u}", Rel(-1)),
        r("(na |no )?{W} seguinte", Weekday(1)),
        r("(na |no )?(próxima|próximo) {W}", Weekday(1)),
        r("(na |no )?{W} (anterior|passada|passado)", Weekday(-1)),
        r(
            "naquela (noite|tarde|manhã|semana)|naquele (dia|ano|mês)",
            Vague,
        ),
        r("esta (noite|tarde|manhã)", Vague),
        r("um dia|uma (noite|tarde|manhã|vez)", Vague),
        r("logo depois|em breve|pouco (depois|antes)", Vague),
        r("há (alguns|algumas|vários|várias|uns|umas) {u}", Vague),
        r(
            "(alguns|algumas|vários|várias|uns|umas) {u}( (depois|mais tarde|antes|atrás))?",
            Vague,
        ),
        r("recentemente|outrora", Vague),
        r("(na |no )?{W}", Vague),
        r("durante {a} {u}", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- Turkish

fn tr_numerals() -> Numerals {
    let ones = run(
        1,
        &[
            "bir", "iki", "üç", "dört", "beş", "altı", "yedi", "sekiz", "dokuz",
        ],
    );
    let mut simple = ones.clone();
    simple.push(("yüz".into(), 100));
    Numerals {
        simple,
        tens: words(&[
            ("on", 10),
            ("yirmi", 20),
            ("otuz", 30),
            ("kırk", 40),
            ("elli", 50),
            ("altmış", 60),
            ("yetmiş", 70),
            ("seksen", 80),
            ("doksan", 90),
        ]),
        ones,
        sep: r"\s+",
        one: Vec::new(),
        hour_one: Vec::new(),
    }
}

static TR: Lang = Lang {
    spaced: true,
    abbr: false,
    units: [
        &["saniye"],
        &["dakika"],
        &["saat"],
        &["gün"],
        &["hafta"],
        &["ay"],
        &["yıl", "sene"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "\\.?",
    numerals: tr_numerals,
    above: &["bin"],
    hour_ordinals: &[],
    parts: &[],
    rules: &[
        r("{d} {M} {y}", Date),
        r("{d} {M}", Date),
        r("{M} {y}", Date),
        r("{y} (yılında|yılı|senesinde)", Date),
        r("{dd}\\.{mm}\\.{y}", Date),
        r("(saat )?{t}", Clock(0)),
        r("saat {H}", Clock(0)),
        hour("gece yarısı", "T00"),
        rel("öbür gün", "+P2D"),
        rel("evvelsi gün", "-P2D"),
        rel("ertesi (gün|sabah)|yarın", "+P1D"),
        rel("dün|önceki gün", "-P1D"),
        rel("bugün", "+P0D"),
        rel("(ertesi|gelecek) hafta|haftaya", "+P1W"),
        rel("geçen hafta", "-P1W"),
        rel("(ertesi|gelecek) ay", "+P1M"),
        rel("geçen ay", "-P1M"),
        rel("(ertesi|gelecek) (yıl|sene)", "+P1Y"),
        rel("geçen (yıl|sene)", "-P1Y"),
        r("{n} {u} sonra", Rel(1)),
        r("{n} {u} (önce|evvel)", Rel(-1)),
        r("(gelecek|önümüzdeki|ertesi) {W}", Weekday(1)),
        r("geçen {W}", Weekday(-1)),
        r("o (akşam|gece|gün|sabah|yıl|hafta)", Vague),
        r("bir (gün|akşam|gece|sabah)", Vague),
        r("yakında|az sonra|biraz sonra|kısa süre sonra", Vague),
        r("birkaç {u}( (sonra|önce|evvel))?", Vague),
        r("geçenlerde|bir zamanlar", Vague),
        r("{W}( günü)?", Vague),
        r("{n} {u}( boyunca)?", Dur),
    ],
};

// ---------------------------------------------------------------- Arabic

fn ar_numerals() -> Numerals {
    let ones = [
        "واحد/واحدة",
        "اثنان/اثنين/اثنتان/اثنتين",
        "ثلاثة/ثلاث",
        "أربعة/أربع",
        "خمسة/خمس",
        "ستة/ست",
        "سبعة/سبع",
        "ثمانية/ثماني/ثمان",
        "تسعة/تسع",
    ];
    let mut simple = run(1, &ones);
    simple.extend(run(
        10,
        &[
            "عشرة/عشر",
            "أحد عشر/إحدى عشرة",
            "اثنا عشر/اثني عشر/اثنتا عشرة/اثنتي عشرة",
            "ثلاثة عشر/ثلاث عشرة",
            "أربعة عشر/أربع عشرة",
            "خمسة عشر/خمس عشرة",
            "ستة عشر/ست عشرة",
            "سبعة عشر/سبع عشرة",
            "ثمانية عشر/ثماني عشرة",
            "تسعة عشر/تسع عشرة",
        ],
    ));
    for (n, tens) in [
        (20, "عشرون/عشرين"),
        (30, "ثلاثون/ثلاثين"),
        (40, "أربعون/أربعين"),
        (50, "خمسون/خمسين"),
        (60, "ستون/ستين"),
        (70, "سبعون/سبعين"),
        (80, "ثمانون/ثمانين"),
        (90, "تسعون/تسعين"),
    ] {
        for t in tens.split('/') {
            simple.push((t.to_string(), n));
            // The unit first, then `و` and the tens.
            for (i, forms) in ones.iter().enumerate() {
                for o in forms.split('/') {
                    simple.push((format!("{o} و{t}"), n + i as u32 + 1));
                }
            }
        }
    }
    simple.extend(words(&[("مئة", 100), ("مائة", 100)]));
    Numerals {
        simple,
        ..Numerals::default()
    }
}

static AR: Lang = Lang {
    spaced: true,
    abbr: false,
    units: [
        &["ثانية", "ثوان", "ثواني"],
        &["دقيقة", "دقائق"],
        &["ساعة", "ساعات"],
        &["يوم", "يوما", "يوماً", "أيام"],
        &["أسبوع", "أسبوعا", "أسبوعاً", "أسابيع"],
        &["شهر", "شهرا", "شهراً", "أشهر", "شهور"],
        &["سنة", "سنوات", "سنين", "عام", "عاما", "عاماً", "أعوام"],
    ],
    months: &[],
    weekdays: &[],
    ordinal: "",
    numerals: ar_numerals,
    above: &["ألف", "آلاف", "مئتان", "مئتين"],
    hour_ordinals: &[],
    parts: &[],
    rules: &[
        r("{d} (من )?{M} (عام |سنة )?{y}", Date),
        r("{d} (من )?{M}", Date),
        r("{M} (عام |سنة )?{y}", Date),
        r("(في )?(عام|سنة) {y}", Date),
        r("{dd}/{mm}/{y}", Date),
        r("{t}", Clock(0)),
        rel("بعد غد", "+P2D"),
        rel("أول أمس|أول من أمس", "-P2D"),
        rel("في (صباح )?اليوم التالي|غدا|غداً", "+P1D"),
        rel("أمس|بالأمس|البارحة", "-P1D"),
        rel("بعد يومين", "+P2D"),
        rel("(قبل|منذ) يومين", "-P2D"),
        rel("بعد أسبوعين", "+P2W"),
        rel("(قبل|منذ) أسبوعين", "-P2W"),
        rel("بعد شهرين", "+P2M"),
        rel("(قبل|منذ) شهرين", "-P2M"),
        rel("بعد (عامين|سنتين)", "+P2Y"),
        rel("(قبل|منذ) (عامين|سنتين)", "-P2Y"),
        rel("بعد يوم", "+P1D"),
        rel("(قبل|منذ) يوم", "-P1D"),
        rel("بعد أسبوع", "+P1W"),
        rel("(قبل|منذ) أسبوع", "-P1W"),
        rel("بعد شهر", "+P1M"),
        rel("(قبل|منذ) شهر", "-P1M"),
        rel("بعد (عام|سنة)", "+P1Y"),
        rel("(قبل|منذ) (عام|سنة)", "-P1Y"),
        rel("الأسبوع (المقبل|القادم|التالي)", "+P1W"),
        rel("الأسبوع الماضي", "-P1W"),
        rel("الشهر (المقبل|القادم|التالي)", "+P1M"),
        rel("الشهر الماضي", "-P1M"),
        rel(
            "(العام|السنة) (المقبل|القادم|التالي|المقبلة|القادمة|التالية)",
            "+P1Y",
        ),
        rel("(العام|السنة) (الماضي|الماضية)", "-P1Y"),
        r("بعد {n} {u}", Rel(1)),
        r("(قبل|منذ) {n} {u}", Rel(-1)),
        r("(يوم )?{W} (المقبل|القادم|التالي)", Weekday(1)),
        r("(يوم )?{W} الماضي", Weekday(-1)),
        r(
            "(في )?(ذلك|تلك) (اليوم|المساء|الصباح|الليلة|السنة|العام|الأسبوع)",
            Vague,
        ),
        r("ذات (يوم|مساء|ليلة|صباح|مرة)", Vague),
        r("قريبا|قريباً|بعد قليل|مؤخرا|مؤخراً", Vague),
        r("(بعد|قبل|منذ) (بضعة|عدة|بضع) {u}", Vague),
        r("(بضعة|عدة|بضع) {u}", Vague),
        r("بعد أيام", Vague),
        r("(يوم )?{W}", Vague),
        r("لمدة {n} {u}", Dur),
        r("{n} {u}", Dur),
    ],
};

// ---------------------------------------------------------------- Chinese, Japanese

/// The numerals both write alike: 一 … 九十九, and 百.
fn han_numerals(two: &[&str], hundred: &[&str]) -> Numerals {
    let digits = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let mut simple: Vec<(String, u32)> = Vec::new();
    for (i, d) in digits.iter().enumerate() {
        simple.push((d.to_string(), i as u32 + 1));
    }
    for t in two {
        simple.push((t.to_string(), 2));
    }
    for tens in 1..=9u32 {
        let head = if tens == 1 {
            String::new()
        } else {
            digits[tens as usize - 1].to_string()
        };
        simple.push((format!("{head}十"), tens * 10));
        for (i, d) in digits.iter().enumerate() {
            simple.push((format!("{head}十{d}"), tens * 10 + i as u32 + 1));
        }
    }
    for h in hundred {
        simple.push((h.to_string(), 100));
    }
    Numerals {
        simple,
        ..Numerals::default()
    }
}

fn zh_numerals() -> Numerals {
    han_numerals(&["两", "兩"], &["百", "一百"])
}

fn ja_numerals() -> Numerals {
    han_numerals(&[], &["百"])
}

static ZH: Lang = Lang {
    spaced: false,
    abbr: false,
    units: [
        &["秒", "秒钟", "秒鐘"],
        &["分钟", "分鐘"],
        &["小时", "个小时", "小時", "個小時", "钟头", "个钟头"],
        &["天"],
        &["周", "週", "星期", "个星期", "個星期", "礼拜", "个礼拜"],
        &["个月", "個月"],
        &["年"],
    ],
    months: &[],
    weekdays: &[
        (0, "周一"),
        (1, "周二"),
        (2, "周三"),
        (3, "周四"),
        (4, "周五"),
        (5, "周六"),
        (6, "周日"),
        (0, "礼拜一"),
        (1, "礼拜二"),
        (2, "礼拜三"),
        (3, "礼拜四"),
        (4, "礼拜五"),
        (5, "礼拜六"),
        (6, "礼拜天"),
    ],
    ordinal: "",
    numerals: zh_numerals,
    above: &["千", "万", "萬", "零", "〇"],
    hour_ordinals: &[],
    parts: &[
        ("凌晨", Part::Am),
        ("早上", Part::Am),
        ("上午", Part::Am),
        ("下午", Part::Pm),
        ("晚上", Part::Night),
    ],
    rules: &[
        r("{y}年{M}{dn}(日|号|號)", Date),
        r("{M}{dn}(日|号|號)", Date),
        r("{y}年{M}", Date),
        r("{y}年", Date),
        r("{t}", Clock(0)),
        r("({p})?{H}(点|點)半", Clock(30)),
        r("({p})?{H}(点|點)(钟|鐘)?", Clock(0)),
        hour("中午|正午", "T12"),
        hour("午夜|半夜", "T00"),
        rel("后天|後天", "+P2D"),
        rel("前天", "-P2D"),
        rel("第二天|次日|翌日|明天|明日", "+P1D"),
        rel("昨天|昨日|昨晚|昨夜|前一天|头一天", "-P1D"),
        rel("今天|今日", "+P0D"),
        rel("下周|下週|下星期|下个星期|下礼拜", "+P1W"),
        rel("上周|上週|上星期|上个星期|上礼拜", "-P1W"),
        rel("下个月|下個月|下月", "+P1M"),
        rel("上个月|上個月|上月", "-P1M"),
        rel("后年|後年", "+P2Y"),
        rel("前年", "-P2Y"),
        rel("明年|第二年|次年|翌年", "+P1Y"),
        rel("去年|前一年", "-P1Y"),
        r("{n}{u}(之后|以后|之後|以後|后|後)", Rel(1)),
        r("(过了|過了){n}{u}", Rel(1)),
        r("{n}{u}(之前|以前|前)", Rel(-1)),
        r("下(个|個)?{W}", Weekday(1)),
        r("上(个|個)?{W}", Weekday(-1)),
        r(
            "那天晚上|那一天|那天|那晚|那年|那时|那時|当时|當時|当天|當天|当晚|當晚",
            Vague,
        ),
        r("有一天|有一次|有一年", Vague),
        r(
            "不久(以后|之后|以後|之後|后|後)?|很快|最近|从前|從前|很久以前",
            Vague,
        ),
        r("(几|幾){u}(之后|以后|后|之後|以後|後|之前|以前|前)?", Vague),
        r("{W}", Vague),
        r("{n}{u}", Dur),
    ],
};

static JA: Lang = Lang {
    spaced: false,
    abbr: false,
    units: [
        &["秒", "秒間"],
        &["分", "分間"],
        &["時間"],
        &["日", "日間"],
        &["週間", "週"],
        &[
            "か月",
            "ヶ月",
            "カ月",
            "ケ月",
            "箇月",
            "か月間",
            "ヶ月間",
            "カ月間",
        ],
        &["年", "年間"],
    ],
    months: &[
        (1, "一月"),
        (2, "二月"),
        (3, "三月"),
        (4, "四月"),
        (5, "五月"),
        (6, "六月"),
        (7, "七月"),
        (8, "八月"),
        (9, "九月"),
        (10, "十月"),
        (11, "十一月"),
        (12, "十二月"),
    ],
    weekdays: &[],
    ordinal: "",
    numerals: ja_numerals,
    above: &["千", "万"],
    hour_ordinals: &[],
    parts: &[("午前", Part::Am), ("午後", Part::Pm)],
    rules: &[
        r("{y}年{M}{dn}日", Date),
        r("{M}{dn}日", Date),
        r("{y}年{M}", Date),
        r("{y}年", Date),
        r("{t}", Clock(0)),
        r("({p})?{H}時半", Clock(30)),
        r("({p})?{H}時((?P<m>[0-5]?[0-9])分)?", Clock(0)),
        hour("正午", "T12"),
        hour("真夜中", "T00"),
        rel("明後日|あさって", "+P2D"),
        rel("一昨日|おととい", "-P2D"),
        rel("翌日|次の日|あくる日|明くる日|翌朝|明日", "+P1D"),
        rel("昨日|前日|前の日|前夜|昨夜|昨晩", "-P1D"),
        rel("今日", "+P0D"),
        rel("再来週", "+P2W"),
        rel("翌週|来週|次の週", "+P1W"),
        rel("先週|前の週", "-P1W"),
        rel("翌月|来月", "+P1M"),
        rel("先月", "-P1M"),
        rel("再来年", "+P2Y"),
        rel("一昨年|おととし", "-P2Y"),
        rel("翌年|来年|次の年", "+P1Y"),
        rel("去年|昨年|前年", "-P1Y"),
        r("{n}{u}(後|のち|あと)", Rel(1)),
        r("{n}{u}(前|まえ)", Rel(-1)),
        r("(来週|次|翌週)の{W}", Weekday(1)),
        r("(先週|前)の{W}", Weekday(-1)),
        r("その(日|夜|晩|朝|年|時|とき|頃|ころ)", Vague),
        r("ある(日|夜|晩|朝|年|時|とき)", Vague),
        r("やがて|まもなく|間もなく|しばらくして|最近|かつて", Vague),
        r("数{u}(後|前)?", Vague),
        r("{W}", Vague),
        r(
            "{n}(?P<u>秒間|秒|分間|時間|日間|週間|[かヶカケ箇]月間?|年間)",
            Dur,
        ),
    ],
};
