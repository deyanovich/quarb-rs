//! Transliteration: the ISO letter tables for the major non-Latin
//! scripts, and the two Russian systems used in practice beside
//! ISO 9. Table-driven, one code point at a time, with the few
//! contextual rules each standard has (inherent vowels, shadda,
//! BGN's initial ye). A code point outside the scheme's table
//! passes through unchanged, combining marks included, so a text
//! that mixes scripts transliterates its own scripts and keeps the
//! rest.

use unicode_normalization::UnicodeNormalization;

/// The schemes by name; the empty name is the script's own ISO
/// scheme, chosen per code point. The Russian systems answer to
/// their Russian names too (гост, научная, бгн, паспорт).
pub const SCHEMES: &[&str] = &[
    "iso",
    "iso9",
    "iso843",
    "iso9985",
    "iso9984",
    "iso259",
    "iso233",
    "iso15919",
    "iso11940",
    "iso3602",
    "gost",
    "scientific",
    "bgn",
    "passport",
];

/// Transliterate `text` under `scheme` (`""` or `"iso"` for the
/// script's own ISO scheme). An unknown scheme is an error.
pub fn translit(text: &str, scheme: &str) -> Result<String, String> {
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "" | "iso" => "iso",
        "iso9" | "iso-9" | "iso 9" | "gost-a" | "гост-а" | "гост а" => "iso9",
        "gost" | "gost-b" | "gost7.79" | "gost-7.79" | "гост" | "гост-б" | "гост б" => {
            "gost"
        }
        "passport" | "паспорт" | "мвд" | "icao" | "мид" => "passport",
        "iso843" | "iso-843" => "iso843",
        "iso9985" | "iso-9985" => "iso9985",
        "iso9984" | "iso-9984" => "iso9984",
        "iso259" | "iso-259" => "iso259",
        "iso233" | "iso-233" => "iso233",
        "iso15919" | "iso-15919" => "iso15919",
        "iso11940" | "iso-11940" => "iso11940",
        "iso3602" | "iso-3602" | "kunrei" => "iso3602",
        "scientific" | "scholarly" | "научная" | "науч" | "научн" => "scientific",
        "bgn" | "bgn-pcgn" | "pcgn" | "bgn/pcgn" | "бгн" => "bgn",
        other => {
            return Err(format!(
                "translit: unknown scheme '{other}' (iso, iso9, iso843, iso9985, iso9984, \
                 iso259, iso233, iso15919, iso11940, iso3602; for Russian: gost/гост, \
                 scientific/научная, bgn/бгн, passport/паспорт)"
            ));
        }
    };
    let text: String = text.nfc().collect();
    Ok(match scheme {
        "iso" => auto(&text),
        "iso9" => cased(&text, |c| lookup(ISO9, c)),
        "scientific" => cased(&text, |c| lookup(SCIENTIFIC, c)),
        "bgn" => bgn(&text),
        "gost" => gost_b(&text),
        "passport" => cased(&text, |c| lookup(PASSPORT, c)),
        "iso843" => greek(&text),
        "iso9985" => cased(&text, |c| lookup(ARMENIAN, c)),
        "iso9984" => plain(&text, |c| lookup(GEORGIAN, c)),
        "iso259" => hebrew(&text),
        "iso233" => arabic(&text),
        "iso15919" => devanagari(&text),
        "iso11940" => plain(&text, |c| lookup(THAI, c)),
        "iso3602" => kana(&text),
        _ => unreachable!(),
    })
}

fn lookup(table: &[(char, &'static str)], c: char) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
}

/// Apply `map` to the lowercase of each letter, restoring case: an
/// uppercase letter titlecases its output, and an uppercase letter
/// beside another uppercase letter uppercases it whole (ЩУКА → ŜUKA,
/// Щука → Ŝuka).
fn cased(text: &str, map: impl Fn(char) -> Option<&'static str>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        let lower: char = c.to_lowercase().next().unwrap_or(c);
        match map(lower) {
            Some(m) if c.is_uppercase() => {
                let neighbour_upper = chars.get(i + 1).is_some_and(|n| n.is_uppercase())
                    || (i > 0 && chars[i - 1].is_uppercase());
                if neighbour_upper {
                    out.extend(m.chars().flat_map(char::to_uppercase));
                } else {
                    let mut it = m.chars();
                    if let Some(first) = it.next() {
                        out.extend(first.to_uppercase());
                    }
                    out.push_str(it.as_str());
                }
            }
            Some(m) => out.push_str(m),
            None => out.push(c),
        }
    }
    out
}

/// Caseless scripts: map or pass through.
fn plain(text: &str, map: impl Fn(char) -> Option<&'static str>) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match map(c) {
            Some(m) => out.push_str(m),
            None => out.push(c),
        }
    }
    out
}

/// Each code point under its own script's ISO scheme.
fn auto(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let mut run_script = script_of(' ');
    let flush = |run: &mut String, script: Script, out: &mut String| {
        if run.is_empty() {
            return;
        }
        let piece = match script {
            Script::Cyrillic => cased(run, |c| lookup(ISO9, c)),
            Script::Greek => greek(run),
            Script::Armenian => cased(run, |c| lookup(ARMENIAN, c)),
            Script::Georgian => plain(run, |c| lookup(GEORGIAN, c)),
            Script::Hebrew => hebrew(run),
            Script::Arabic => arabic(run),
            Script::Devanagari => devanagari(run),
            Script::Thai => plain(run, |c| lookup(THAI, c)),
            Script::Kana => kana(run),
            Script::Other => run.clone(),
        };
        out.push_str(&piece);
        run.clear();
    };
    for c in text.chars() {
        let s = script_of(c);
        // combining marks and spaces stay with the run they follow
        let s = if s == Script::Other && (is_mark(c) || c.is_whitespace()) {
            run_script
        } else {
            s
        };
        if s != run_script {
            flush(&mut run, run_script, &mut out);
            run_script = s;
        }
        run.push(c);
    }
    flush(&mut run, run_script, &mut out);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Cyrillic,
    Greek,
    Armenian,
    Georgian,
    Hebrew,
    Arabic,
    Devanagari,
    Thai,
    Kana,
    Other,
}

fn script_of(c: char) -> Script {
    match c as u32 {
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0xA640..=0xA69F => Script::Cyrillic,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0530..=0x058F | 0xFB13..=0xFB17 => Script::Armenian,
        0x10A0..=0x10FF | 0x2D00..=0x2D2F => Script::Georgian,
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::Hebrew,
        0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arabic,
        0x0900..=0x097F | 0xA8E0..=0xA8FF => Script::Devanagari,
        0x0E00..=0x0E7F => Script::Thai,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Script::Kana,
        _ => Script::Other,
    }
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

// ---------------------------------------------------------------- Cyrillic

/// ISO 9:1995 (= GOST 7.79 system A): one Latin letter per Cyrillic
/// letter, with diacritics, for every language the standard covers.
const ISO9: &[(char, &str)] = &[
    ('а', "a"),
    ('б', "b"),
    ('в', "v"),
    ('г', "g"),
    ('д', "d"),
    ('е', "e"),
    ('ё', "ë"),
    ('ж', "ž"),
    ('з', "z"),
    ('и', "i"),
    ('й', "j"),
    ('к', "k"),
    ('л', "l"),
    ('м', "m"),
    ('н', "n"),
    ('о', "o"),
    ('п', "p"),
    ('р', "r"),
    ('с', "s"),
    ('т', "t"),
    ('у', "u"),
    ('ф', "f"),
    ('х', "h"),
    ('ц', "c"),
    ('ч', "č"),
    ('ш', "š"),
    ('щ', "ŝ"),
    ('ъ', "ʺ"),
    ('ы', "y"),
    ('ь', "ʹ"),
    ('э', "è"),
    ('ю', "û"),
    ('я', "â"),
    // Ukrainian, Belarusian, Rusyn
    ('ґ', "g\u{300}"),
    ('є', "ê"),
    ('і', "ì"),
    ('ї', "ï"),
    ('ў', "ŭ"),
    // Serbian, Macedonian
    ('ђ', "đ"),
    ('ј', "ǰ"),
    ('љ', "l\u{302}"),
    ('њ', "n\u{302}"),
    ('ћ', "ć"),
    ('џ', "d\u{302}"),
    ('ѓ', "ǵ"),
    ('ќ', "ḱ"),
    ('ѕ', "ẑ"),
    // Bulgarian and pre-reform Russian
    ('ѫ', "ǎ"),
    ('ѣ', "ě"),
    ('ѳ', "f\u{300}"),
    ('ѵ', "y\u{300}"),
    // Kazakh, Kyrgyz, Tatar, Bashkir, Mongolian, Tajik, and the like
    ('ә', "a\u{30b}"),
    ('ғ', "ġ"),
    ('қ', "ķ"),
    ('ң', "ņ"),
    ('ө', "ô"),
    ('ұ', "u\u{307}"),
    ('ү', "ù"),
    ('һ', "ḥ"),
    ('ӣ', "ī"),
    ('ӯ', "ū"),
    ('ҳ', "ḩ"),
    ('ҷ', "ç"),
    ('ҝ', "k\u{337}"),
    ('ӑ', "ă"),
    ('ӓ', "ä"),
    ('ӗ', "ĕ"),
    ('ӧ', "ö"),
    ('ӱ', "ü"),
    ('ҕ', "ğ"),
    ('ҙ', "ẑ"),
    ('ҫ', "ş"),
    ('ҷ', "ç"),
    ('ҹ', "ç\u{306}"),
    ('ҭ', "ţ"),
    ('ҳ', "ḩ"),
    ('ӏ', "‡"),
    ('ѐ', "è"),
    ('ѝ', "ì"),
    ('ӂ', "z\u{306}"),
    ('ӌ', "c\u{326}"),
    ('ҁ', "c\u{326}"),
];

/// The scientific (scholarly) system for Russian, «научная
/// транслитерация», the one the linguistic literature uses: ISO 9
/// with x, šč, ju, ja.
const SCIENTIFIC: &[(char, &str)] = &[
    ('а', "a"),
    ('б', "b"),
    ('в', "v"),
    ('г', "g"),
    ('д', "d"),
    ('е', "e"),
    ('ё', "ë"),
    ('ж', "ž"),
    ('з', "z"),
    ('и', "i"),
    ('й', "j"),
    ('к', "k"),
    ('л', "l"),
    ('м', "m"),
    ('н', "n"),
    ('о', "o"),
    ('п', "p"),
    ('р', "r"),
    ('с', "s"),
    ('т', "t"),
    ('у', "u"),
    ('ф', "f"),
    ('х', "x"),
    ('ц', "c"),
    ('ч', "č"),
    ('ш', "š"),
    ('щ', "šč"),
    ('ъ', "ʺ"),
    ('ы', "y"),
    ('ь', "ʹ"),
    ('э', "è"),
    ('ю', "ju"),
    ('я', "ja"),
    ('ѣ', "ě"),
    ('ѳ', "f"),
    ('ѵ', "i"),
    ('і', "i"),
    ('є', "je"),
    ('ї', "ji"),
    ('ґ', "g"),
    ('ў', "ŭ"),
];

/// ГОСТ 7.79-2000 system Б: the ASCII system, what «транслит по
/// ГОСТу» means in practice (system А is ISO 9). ц is cz, or c
/// before е, и, й, ы; ъ and ь are grave accents.
const GOST_B: &[(char, &str)] = &[
    ('а', "a"),
    ('б', "b"),
    ('в', "v"),
    ('г', "g"),
    ('д', "d"),
    ('е', "e"),
    ('ё', "yo"),
    ('ж', "zh"),
    ('з', "z"),
    ('и', "i"),
    ('й', "j"),
    ('к', "k"),
    ('л', "l"),
    ('м', "m"),
    ('н', "n"),
    ('о', "o"),
    ('п', "p"),
    ('р', "r"),
    ('с', "s"),
    ('т', "t"),
    ('у', "u"),
    ('ф', "f"),
    ('х', "x"),
    ('ц', "cz"),
    ('ч', "ch"),
    ('ш', "sh"),
    ('щ', "shh"),
    ('ъ', "``"),
    ('ы', "y`"),
    ('ь', "`"),
    ('э', "e`"),
    ('ю', "yu"),
    ('я', "ya"),
    ('ѣ', "ye"),
    ('ѳ', "fh"),
    ('ѵ', "yh"),
    ('ѫ', "o`"),
    ('ґ', "g`"),
    ('є', "ye"),
    ('і', "i"),
    ('ї', "yi"),
    ('ў', "u`"),
];

fn gost_b(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() * 2);
    for (i, &c) in chars.iter().enumerate() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        let piece = if lower == 'ц'
            && chars
                .get(i + 1)
                .is_some_and(|n| "еийы".contains(n.to_lowercase().next().unwrap_or(*n)))
        {
            Some("c")
        } else {
            lookup(GOST_B, lower)
        };
        match piece {
            Some(m) if c.is_uppercase() => {
                let neighbour_upper = chars.get(i + 1).is_some_and(|n| n.is_uppercase())
                    || (i > 0 && chars[i - 1].is_uppercase());
                if neighbour_upper {
                    out.extend(m.chars().flat_map(char::to_uppercase));
                } else {
                    let mut it = m.chars();
                    if let Some(first) = it.next() {
                        out.extend(first.to_uppercase());
                    }
                    out.push_str(it.as_str());
                }
            }
            Some(m) => out.push_str(m),
            None => out.push(c),
        }
    }
    out
}

/// The passport system (МВД 2014, ICAO Doc 9303): ASCII, no
/// diacritics, ъ is ie, ь is dropped.
const PASSPORT: &[(char, &str)] = &[
    ('а', "a"),
    ('б', "b"),
    ('в', "v"),
    ('г', "g"),
    ('д', "d"),
    ('е', "e"),
    ('ё', "e"),
    ('ж', "zh"),
    ('з', "z"),
    ('и', "i"),
    ('й', "i"),
    ('к', "k"),
    ('л', "l"),
    ('м', "m"),
    ('н', "n"),
    ('о', "o"),
    ('п', "p"),
    ('р', "r"),
    ('с', "s"),
    ('т', "t"),
    ('у', "u"),
    ('ф', "f"),
    ('х', "kh"),
    ('ц', "ts"),
    ('ч', "ch"),
    ('ш', "sh"),
    ('щ', "shch"),
    ('ъ', "ie"),
    ('ы', "y"),
    ('ь', ""),
    ('э', "e"),
    ('ю', "iu"),
    ('я', "ia"),
];

/// BGN/PCGN 1947 for Russian: the Anglo-American practical system.
const BGN: &[(char, &str)] = &[
    ('а', "a"),
    ('б', "b"),
    ('в', "v"),
    ('г', "g"),
    ('д', "d"),
    ('е', "e"),
    ('ё', "ë"),
    ('ж', "zh"),
    ('з', "z"),
    ('и', "i"),
    ('й', "y"),
    ('к', "k"),
    ('л', "l"),
    ('м', "m"),
    ('н', "n"),
    ('о', "o"),
    ('п', "p"),
    ('р', "r"),
    ('с', "s"),
    ('т', "t"),
    ('у', "u"),
    ('ф', "f"),
    ('х', "kh"),
    ('ц', "ts"),
    ('ч', "ch"),
    ('ш', "sh"),
    ('щ', "shch"),
    ('ъ', "ʺ"),
    ('ы', "y"),
    ('ь', "ʹ"),
    ('э', "e"),
    ('ю', "yu"),
    ('я', "ya"),
    ('ѣ', "ye"),
    ('ѳ', "f"),
    ('ѵ', "i"),
    ('і', "i"),
];

/// BGN/PCGN with its one contextual rule: е and ё take a y at the
/// start of a word and after a vowel, й, ъ or ь.
fn bgn(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let vowelish = |c: char| "аеёиоуыэюяйъь".contains(c.to_lowercase().next().unwrap_or(c));
    for (i, &c) in chars.iter().enumerate() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        let yotated = matches!(lower, 'е' | 'ё')
            && (i == 0 || !chars[i - 1].is_alphabetic() || vowelish(chars[i - 1]));
        let piece = match (lower, yotated) {
            ('е', true) => Some("ye"),
            ('ё', true) => Some("yë"),
            _ => lookup(BGN, lower),
        };
        match piece {
            Some(m) if c.is_uppercase() => {
                let neighbour_upper = chars.get(i + 1).is_some_and(|n| n.is_uppercase())
                    || (i > 0 && chars[i - 1].is_uppercase());
                if neighbour_upper {
                    out.extend(m.chars().flat_map(char::to_uppercase));
                } else {
                    let mut it = m.chars();
                    if let Some(first) = it.next() {
                        out.extend(first.to_uppercase());
                    }
                    out.push_str(it.as_str());
                }
            }
            Some(m) => out.push_str(m),
            None => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------- Greek

/// ISO 843:1997 transliteration: the base letters, with any accent
/// or diaeresis carried over as the combining mark it is.
const GREEK: &[(char, &str)] = &[
    ('α', "a"),
    ('β', "v"),
    ('γ', "g"),
    ('δ', "d"),
    ('ε', "e"),
    ('ζ', "z"),
    ('η', "ī"),
    ('θ', "th"),
    ('ι', "i"),
    ('κ', "k"),
    ('λ', "l"),
    ('μ', "m"),
    ('ν', "n"),
    ('ξ', "x"),
    ('ο', "o"),
    ('π', "p"),
    ('ρ', "r"),
    ('σ', "s"),
    ('ς', "s"),
    ('τ', "t"),
    ('υ', "y"),
    ('φ', "f"),
    ('χ', "ch"),
    ('ψ', "ps"),
    ('ω', "ō"),
    ('ϝ', "w"),
    ('ϛ', "st"),
    ('ϟ', "q"),
    ('ϡ', "ss"),
];

fn greek(text: &str) -> String {
    // decompose so the base letter maps and the marks follow it
    let nfd: String = text.nfd().collect();
    let out = cased(&nfd, |c| lookup(GREEK, c));
    out.nfc().collect()
}

// ---------------------------------------------------------------- Armenian

/// ISO 9985:1996.
const ARMENIAN: &[(char, &str)] = &[
    ('ա', "a"),
    ('բ', "b"),
    ('գ', "g"),
    ('դ', "d"),
    ('ե', "e"),
    ('զ', "z"),
    ('է', "ē"),
    ('ը', "ë"),
    ('թ', "t’"),
    ('ժ', "ž"),
    ('ի', "i"),
    ('լ', "l"),
    ('խ', "x"),
    ('ծ', "ç"),
    ('կ', "k"),
    ('հ', "h"),
    ('ձ', "j"),
    ('ղ', "ġ"),
    ('ճ', "č̣"),
    ('մ', "m"),
    ('յ', "y"),
    ('ն', "n"),
    ('շ', "š"),
    ('ո', "o"),
    ('չ', "č"),
    ('պ', "p"),
    ('ջ', "ǰ"),
    ('ռ', "ṙ"),
    ('ս', "s"),
    ('վ', "v"),
    ('տ', "t"),
    ('ր', "r"),
    ('ց', "c’"),
    ('ւ', "w"),
    ('փ', "p’"),
    ('ք', "k’"),
    ('օ', "ò"),
    ('ֆ', "f"),
    ('և', "ew"),
];

// ---------------------------------------------------------------- Georgian

/// ISO 9984:1996 (Mkhedruli, with the archaic letters).
const GEORGIAN: &[(char, &str)] = &[
    ('ა', "a"),
    ('ბ', "b"),
    ('გ', "g"),
    ('დ', "d"),
    ('ე', "e"),
    ('ვ', "v"),
    ('ზ', "z"),
    ('თ', "t’"),
    ('ი', "i"),
    ('კ', "k"),
    ('ლ', "l"),
    ('მ', "m"),
    ('ნ', "n"),
    ('ო', "o"),
    ('პ', "p"),
    ('ჟ', "ž"),
    ('რ', "r"),
    ('ს', "s"),
    ('ტ', "t"),
    ('უ', "u"),
    ('ფ', "p’"),
    ('ქ', "k’"),
    ('ღ', "ḡ"),
    ('ყ', "q"),
    ('შ', "š"),
    ('ჩ', "č’"),
    ('ც', "c’"),
    ('ძ', "j"),
    ('წ', "c"),
    ('ჭ', "č"),
    ('ხ', "x"),
    ('ჯ', "ǰ"),
    ('ჰ', "h"),
    ('ჱ', "ē"),
    ('ჲ', "y"),
    ('ჳ', "w"),
    ('ჴ', "ẖ"),
    ('ჵ', "ō"),
    ('ჶ', "f"),
];

// ---------------------------------------------------------------- Hebrew

/// ISO 259:1984: the letters, the shin and sin dots, and the vowel
/// points; a dagesh doubles nothing here (the letter table has no
/// spirant pairs), so it passes silently.
const HEBREW: &[(char, &str)] = &[
    ('א', "ʾ"),
    ('ב', "b"),
    ('ג', "g"),
    ('ד', "d"),
    ('ה', "h"),
    ('ו', "w"),
    ('ז', "z"),
    ('ח', "ḥ"),
    ('ט', "ṭ"),
    ('י', "y"),
    ('כ', "k"),
    ('ך', "k"),
    ('ל', "l"),
    ('מ', "m"),
    ('ם', "m"),
    ('נ', "n"),
    ('ן', "n"),
    ('ס', "s"),
    ('ע', "ʿ"),
    ('פ', "p"),
    ('ף', "p"),
    ('צ', "ṣ"),
    ('ץ', "ṣ"),
    ('ק', "q"),
    ('ר', "r"),
    ('ש', "š"),
    ('ת', "t"),
    // points
    ('\u{5b8}', "ā"),
    ('\u{5b7}', "a"),
    ('\u{5b5}', "ē"),
    ('\u{5b6}', "e"),
    ('\u{5b4}', "i"),
    ('\u{5b9}', "ō"),
    ('\u{5ba}', "ō"),
    ('\u{5bb}', "u"),
    ('\u{5b0}', "ə"),
    ('\u{5b1}', "ĕ"),
    ('\u{5b2}', "ă"),
    ('\u{5b3}', "ŏ"),
    ('\u{5bc}', ""),
    ('\u{5be}', "-"),
    ('\u{5c3}', ":"),
    ('\u{5f3}', "'"),
    ('\u{5f4}', "\""),
];

fn hebrew(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let is_point = |c: char| matches!(c as u32, 0x05B0..=0x05C7);
    while i < chars.len() {
        let c = chars[i];
        if c == 'ש' {
            // the shin or sin dot decides, wherever NFC put it among
            // the letter's points; a dotless shin is š
            let mut j = i + 1;
            let mut sin = false;
            let mut dot_at = None;
            while j < chars.len() && is_point(chars[j]) {
                match chars[j] {
                    '\u{5c2}' => {
                        sin = true;
                        dot_at = Some(j);
                    }
                    '\u{5c1}' => dot_at = Some(j),
                    _ => {}
                }
                j += 1;
            }
            out.push(if sin { 'ś' } else { 'š' });
            i += 1;
            // emit the remaining points, skipping the dot
            while i < j {
                if Some(i) != dot_at
                    && let Some(m) = lookup(HEBREW, chars[i])
                {
                    out.push_str(m);
                }
                i += 1;
            }
            continue;
        }
        if c == 'ו' {
            // holam male (וֹ) is ô, shuruq (וּ) is û
            match chars.get(i + 1) {
                Some('\u{5b9}') | Some('\u{5ba}') => {
                    out.push('ô');
                    i += 2;
                    continue;
                }
                Some('\u{5bc}') => {
                    out.push('û');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        match lookup(HEBREW, c) {
            Some(m) => out.push_str(m),
            None => out.push(c),
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- Arabic

/// ISO 233:1984 with the Persian additions; the short vowels and
/// tanwīn follow their letters, shadda doubles the consonant.
const ARABIC: &[(char, &str)] = &[
    ('ء', "ʾ"),
    ('آ', "ʾā"),
    ('أ', "ʾ"),
    ('ؤ', "ʾ"),
    ('إ', "ʾ"),
    ('ئ', "ʾ"),
    ('ا', "ā"),
    ('ب', "b"),
    ('ة', "ẗ"),
    ('ت', "t"),
    ('ث', "ṯ"),
    ('ج', "ǧ"),
    ('ح', "ḥ"),
    ('خ', "ẖ"),
    ('د', "d"),
    ('ذ', "ḏ"),
    ('ر', "r"),
    ('ز', "z"),
    ('س', "s"),
    ('ش', "š"),
    ('ص', "ṣ"),
    ('ض', "ḍ"),
    ('ط', "ṭ"),
    ('ظ', "ẓ"),
    ('ع', "ʿ"),
    ('غ', "ġ"),
    ('ف', "f"),
    ('ق', "q"),
    ('ك', "k"),
    ('ل', "l"),
    ('م', "m"),
    ('ن', "n"),
    ('ه', "h"),
    ('و', "w"),
    ('ى', "ỳ"),
    ('ي', "y"),
    ('پ', "p"),
    ('چ', "č"),
    ('ژ', "ž"),
    ('گ', "g"),
    ('ک', "k"),
    ('ی', "y"),
    ('ڤ', "v"),
    ('\u{64e}', "a"),
    ('\u{650}', "i"),
    ('\u{64f}', "u"),
    ('\u{652}', ""),
    ('\u{64b}', "an"),
    ('\u{64d}', "in"),
    ('\u{64c}', "un"),
    ('\u{670}', "ā"),
    ('،', ","),
    ('؛', ";"),
    ('؟', "?"),
    ('٠', "0"),
    ('١', "1"),
    ('٢', "2"),
    ('٣', "3"),
    ('٤', "4"),
    ('٥', "5"),
    ('٦', "6"),
    ('٧', "7"),
    ('٨', "8"),
    ('٩', "9"),
];

fn arabic(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // the last consonant's piece and where it ended, so a shadda —
    // which NFC may place after the vowel sign — doubles the
    // consonant in place, before its vowel
    let mut last: Option<(&str, usize)> = None;
    let is_sign = |c: char| matches!(c as u32, 0x064B..=0x0652 | 0x0670);
    for c in text.chars() {
        if c == '\u{651}' {
            if let Some((piece, at)) = last {
                out.insert_str(at, piece);
                last = Some((piece, at + piece.len()));
            }
            continue;
        }
        match lookup(ARABIC, c) {
            Some(m) => {
                out.push_str(m);
                if !is_sign(c) {
                    last = Some((m, out.len()));
                }
            }
            None => {
                out.push(c);
                last = None;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- Devanagari

/// ISO 15919:2001, Devanagari.
const DEVA_VOWELS: &[(char, &str)] = &[
    ('अ', "a"),
    ('आ', "ā"),
    ('इ', "i"),
    ('ई', "ī"),
    ('उ', "u"),
    ('ऊ', "ū"),
    ('ऋ', "r̥"),
    ('ॠ', "r̥̄"),
    ('ऌ', "l̥"),
    ('ॡ', "l̥̄"),
    ('ऍ', "ê"),
    ('ए', "e"),
    ('ऐ', "ai"),
    ('ऑ', "ô"),
    ('ओ', "o"),
    ('औ', "au"),
];
const DEVA_MATRAS: &[(char, &str)] = &[
    ('ा', "ā"),
    ('ि', "i"),
    ('ी', "ī"),
    ('ु', "u"),
    ('ू', "ū"),
    ('ृ', "r̥"),
    ('ॄ', "r̥̄"),
    ('ॢ', "l̥"),
    ('ॣ', "l̥̄"),
    ('ॅ', "ê"),
    ('े', "e"),
    ('ै', "ai"),
    ('ॉ', "ô"),
    ('ो', "o"),
    ('ौ', "au"),
];
const DEVA_CONSONANTS: &[(char, &str)] = &[
    ('क', "k"),
    ('ख', "kh"),
    ('ग', "g"),
    ('घ', "gh"),
    ('ङ', "ṅ"),
    ('च', "c"),
    ('छ', "ch"),
    ('ज', "j"),
    ('झ', "jh"),
    ('ञ', "ñ"),
    ('ट', "ṭ"),
    ('ठ', "ṭh"),
    ('ड', "ḍ"),
    ('ढ', "ḍh"),
    ('ण', "ṇ"),
    ('त', "t"),
    ('थ', "th"),
    ('द', "d"),
    ('ध', "dh"),
    ('न', "n"),
    ('प', "p"),
    ('फ', "ph"),
    ('ब', "b"),
    ('भ', "bh"),
    ('म', "m"),
    ('य', "y"),
    ('र', "r"),
    ('ल', "l"),
    ('ळ', "ḷ"),
    ('व', "v"),
    ('श', "ś"),
    ('ष', "ṣ"),
    ('स', "s"),
    ('ह', "h"),
];
const DEVA_NUKTA: &[(char, &str)] = &[
    ('क', "q"),
    ('ख', "k͟h"),
    ('ग', "ġ"),
    ('ज', "z"),
    ('ड', "ṛ"),
    ('ढ', "ṛh"),
    ('फ', "f"),
    ('य', "ẏ"),
];
const DEVA_SIGNS: &[(char, &str)] = &[
    ('ं', "ṁ"),
    ('ँ', "m̐"),
    ('ः', "ḥ"),
    ('ऽ', "’"),
    ('।', "."),
    ('॥', ".."),
    ('ॐ', "oṁ"),
    ('०', "0"),
    ('१', "1"),
    ('२', "2"),
    ('३', "3"),
    ('४', "4"),
    ('५', "5"),
    ('६', "6"),
    ('७', "7"),
    ('८', "8"),
    ('९', "9"),
];

fn devanagari(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() * 2);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(m) = lookup(DEVA_CONSONANTS, c) {
            let mut cons = m;
            let mut j = i + 1;
            if chars.get(j) == Some(&'\u{93c}') {
                cons = lookup(DEVA_NUKTA, c).unwrap_or(m);
                j += 1;
            }
            out.push_str(cons);
            match chars.get(j) {
                Some('\u{94d}') => {
                    // virama: no vowel
                    j += 1;
                }
                Some(&n) if lookup(DEVA_MATRAS, n).is_some() => {
                    out.push_str(lookup(DEVA_MATRAS, n).unwrap());
                    j += 1;
                }
                _ => out.push('a'),
            }
            i = j;
            continue;
        }
        if let Some(m) = lookup(DEVA_VOWELS, c)
            .or_else(|| lookup(DEVA_SIGNS, c))
            .or_else(|| lookup(DEVA_MATRAS, c))
        {
            out.push_str(m);
        } else if c != '\u{94d}' && c != '\u{93c}' {
            out.push(c);
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------- Thai

/// ISO 11940:1998: a letter-for-letter transliteration (the standard
/// is reversible by design), letters, vowel signs, tone marks, digits.
const THAI: &[(char, &str)] = &[
    ('ก', "k"),
    ('ข', "k̄h"),
    ('ฃ', "ḳ̄h"),
    ('ค', "kh"),
    ('ฅ', "k̛h"),
    ('ฆ', "ḳh"),
    ('ง', "ng"),
    ('จ', "c"),
    ('ฉ', "c̄h"),
    ('ช', "ch"),
    ('ซ', "s"),
    ('ฌ', "c̣h"),
    ('ญ', "ỵ"),
    ('ฎ', "ḍ"),
    ('ฏ', "ṭ"),
    ('ฐ', "ṭ̄h"),
    ('ฑ', "ṯh"),
    ('ฒ', "t̛h"),
    ('ณ', "ṇ"),
    ('ด', "d"),
    ('ต', "t"),
    ('ถ', "t̄h"),
    ('ท', "th"),
    ('ธ', "ṭh"),
    ('น', "n"),
    ('บ', "b"),
    ('ป', "p"),
    ('ผ', "p̄h"),
    ('ฝ', "f̄"),
    ('พ', "ph"),
    ('ฟ', "f"),
    ('ภ', "p̣h"),
    ('ม', "m"),
    ('ย', "y"),
    ('ร', "r"),
    ('ฤ', "v"),
    ('ล', "l"),
    ('ฦ', "ł"),
    ('ว', "w"),
    ('ศ', "ṣ̄"),
    ('ษ', "s̛̄"),
    ('ส', "s̄"),
    ('ห', "h̄"),
    ('ฬ', "ḷ"),
    ('อ', "x"),
    ('ฮ', "ḥ"),
    ('ะ', "a"),
    ('\u{e31}', "ạ"),
    ('า', "ā"),
    ('ำ', "å"),
    ('\u{e34}', "i"),
    ('\u{e35}', "ī"),
    ('\u{e36}', "ụ"),
    ('\u{e37}', "ụ̄"),
    ('\u{e38}', "u"),
    ('\u{e39}', "ū"),
    ('เ', "e"),
    ('แ', "æ"),
    ('โ', "o"),
    ('ใ', "ı"),
    ('ไ', "ị"),
    ('ๅ', "ǂ"),
    ('\u{e47}', "\u{306}"),
    ('ๆ', "ǂ"),
    ('\u{e3a}', "\u{323}"),
    ('\u{e48}', "\u{300}"),
    ('\u{e49}', "\u{302}"),
    ('\u{e4a}', "\u{301}"),
    ('\u{e4b}', "\u{30c}"),
    ('\u{e4c}', "\u{312}"),
    ('\u{e4d}', "\u{30a}"),
    ('\u{e4e}', "\u{30d}"),
    ('๐', "0"),
    ('๑', "1"),
    ('๒', "2"),
    ('๓', "3"),
    ('๔', "4"),
    ('๕', "5"),
    ('๖', "6"),
    ('๗', "7"),
    ('๘', "8"),
    ('๙', "9"),
    ('฿', "฿"),
];

// ---------------------------------------------------------------- Japanese kana

/// ISO 3602:1989 (Kunrei-shiki), hiragana and katakana: the
/// syllables, the small ya/yu/yo contractions, the sokuon
/// doubling, and the circumflex for a long vowel.
const KANA: &[(char, &str)] = &[
    ('あ', "a"),
    ('い', "i"),
    ('う', "u"),
    ('え', "e"),
    ('お', "o"),
    ('か', "ka"),
    ('き', "ki"),
    ('く', "ku"),
    ('け', "ke"),
    ('こ', "ko"),
    ('さ', "sa"),
    ('し', "si"),
    ('す', "su"),
    ('せ', "se"),
    ('そ', "so"),
    ('た', "ta"),
    ('ち', "ti"),
    ('つ', "tu"),
    ('て', "te"),
    ('と', "to"),
    ('な', "na"),
    ('に', "ni"),
    ('ぬ', "nu"),
    ('ね', "ne"),
    ('の', "no"),
    ('は', "ha"),
    ('ひ', "hi"),
    ('ふ', "hu"),
    ('へ', "he"),
    ('ほ', "ho"),
    ('ま', "ma"),
    ('み', "mi"),
    ('む', "mu"),
    ('め', "me"),
    ('も', "mo"),
    ('や', "ya"),
    ('ゆ', "yu"),
    ('よ', "yo"),
    ('ら', "ra"),
    ('り', "ri"),
    ('る', "ru"),
    ('れ', "re"),
    ('ろ', "ro"),
    ('わ', "wa"),
    ('ゐ', "wi"),
    ('ゑ', "we"),
    ('を', "o"),
    ('ん', "n"),
    ('が', "ga"),
    ('ぎ', "gi"),
    ('ぐ', "gu"),
    ('げ', "ge"),
    ('ご', "go"),
    ('ざ', "za"),
    ('じ', "zi"),
    ('ず', "zu"),
    ('ぜ', "ze"),
    ('ぞ', "zo"),
    ('だ', "da"),
    ('ぢ', "zi"),
    ('づ', "zu"),
    ('で', "de"),
    ('ど', "do"),
    ('ば', "ba"),
    ('び', "bi"),
    ('ぶ', "bu"),
    ('べ', "be"),
    ('ぼ', "bo"),
    ('ぱ', "pa"),
    ('ぴ', "pi"),
    ('ぷ', "pu"),
    ('ぺ', "pe"),
    ('ぽ', "po"),
    ('ぁ', "a"),
    ('ぃ', "i"),
    ('ぅ', "u"),
    ('ぇ', "e"),
    ('ぉ', "o"),
    ('ゔ', "vu"),
];

fn kana(text: &str) -> String {
    // katakana → hiragana, then one table
    let chars: Vec<char> = text
        .chars()
        .map(|c| match c as u32 {
            0x30A1..=0x30F6 => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut sokuon = false;
    for &c in &chars {
        match c {
            'っ' => {
                sokuon = true;
                continue;
            }
            'ゃ' | 'ゅ' | 'ょ' => {
                // きゃ → kya: the i of the preceding syllable yields
                if out.ends_with('i') {
                    out.pop();
                }
                out.push_str(match c {
                    'ゃ' => "ya",
                    'ゅ' => "yu",
                    _ => "yo",
                });
                continue;
            }
            'ー' => {
                circumflex(&mut out);
                continue;
            }
            _ => {}
        }
        match lookup(KANA, c) {
            Some(m) => {
                if sokuon {
                    if let Some(first) = m.chars().next() {
                        out.push(first);
                    }
                    sokuon = false;
                }
                // a lone vowel kana after the same vowel lengthens it: おう, おお, うう, ええ, ああ
                let long = m.len() == 1
                    && matches!(
                        (out.chars().last(), m),
                        (Some('o'), "u" | "o")
                            | (Some('u'), "u")
                            | (Some('e'), "e")
                            | (Some('a'), "a")
                    );
                if long {
                    circumflex(&mut out);
                } else {
                    out.push_str(m);
                }
            }
            None => {
                sokuon = false;
                out.push(c);
            }
        }
    }
    out
}

fn circumflex(out: &mut String) {
    let hat = match out.pop() {
        Some('a') => 'â',
        Some('i') => 'î',
        Some('u') => 'û',
        Some('e') => 'ê',
        Some('o') => 'ô',
        Some(other) => {
            out.push(other);
            return;
        }
        None => return,
    };
    out.push(hat);
}

#[cfg(test)]
mod tests {
    use super::translit;

    fn t(text: &str, scheme: &str) -> String {
        translit(text, scheme).unwrap()
    }

    #[test]
    fn russian_three_ways() {
        assert_eq!(t("Щука, ёж и объём", "iso9"), "Ŝuka, ëž i obʺëm");
        assert_eq!(t("Щука, ёж и объём", "scientific"), "Ščuka, ëž i obʺëm");
        assert_eq!(t("Щука, ёж и объём", "научная"), "Ščuka, ëž i obʺëm");
        assert_eq!(t("Щука, ёж и объём", "bgn"), "Shchuka, yëzh i obʺyëm");
        assert_eq!(t("Щука, ёж и объём", "гост"), "Shhuka, yozh i ob``yom");
        assert_eq!(t("Цирк, царь, отец", "gost"), "Cirk, czar`, otecz");
        assert_eq!(t("Щука, ёж и объём", "паспорт"), "Shchuka, ezh i obieem");
        assert_eq!(t("Юлия Ильинична", "passport"), "Iuliia Ilinichna");
        assert_eq!(t("Ельцин", "bgn"), "Yel\u{2b9}tsin".replace('\u{2b9}', "ʹ"));
        assert_eq!(t("замо\u{301}к", "scholarly"), "zamo\u{301}k");
        assert_eq!(t("ЩУКА", "iso9"), "ŜUKA");
        assert_eq!(t("Київ", "iso9"), "Kiïv");
        assert_eq!(t("Ђорђе", "iso9"), "Đorđe");
    }

    #[test]
    fn the_other_scripts() {
        assert_eq!(t("Αθήνα", "iso843"), "Athī́na");
        assert_eq!(t("Երևան", "iso9985"), "Erewan");
        assert_eq!(t("თბილისი", "iso9984"), "t’bilisi");
        assert_eq!(t("שָׁלוֹם", "iso259"), "šālôm");
        assert_eq!(t("שִׂמְחָה", "iso259"), "śiməḥāh");
        assert_eq!(t("مُحَمَّد", "iso233"), "muḥammad");
        assert_eq!(t("हिन्दी", "iso15919"), "hindī");
        assert_eq!(t("नमस्ते", "iso15919"), "namaste");
        assert_eq!(t("กข", "iso11940"), "kk̄h");
        assert_eq!(t("とうきょう", "iso3602"), "tôkyô");
        assert_eq!(t("きって しんぶん", "iso3602"), "kitte sinbun");
        assert_eq!(t("トウキョウ", "iso3602"), "tôkyô");
    }

    #[test]
    fn auto_picks_the_script_per_run() {
        assert_eq!(t("Москва and Αθήνα", "iso"), "Moskva and Athī́na");
        assert_eq!(t("plain latin", ""), "plain latin");
        assert!(translit("x", "klingon").is_err());
    }
}

/// The spelling tables `modernize(table)` knows (ruling #82): a
/// historical orthography folded to the modern one, letter by
/// letter, case kept. `ru-1918` is the pre-reform Russian
/// alphabet (ѣ→е, і→и, ѳ→ф, ѵ→и, the word-final ъ dropped);
/// `long-s` is early-modern Latin typography (ſ→s, the ff/fi/fl/
/// ffi/ffl/ft/st ligatures opened).
pub const SPELLINGS: &[&str] = &["ru-1918", "long-s"];

/// Fold `text` from a historical spelling to the modern one under
/// `table`. An unknown table is an error.
pub fn modernize(text: &str, table: &str) -> Result<String, String> {
    let table = match table.to_ascii_lowercase().as_str() {
        "ru-1918" | "ru1918" | "pre-1918" | "pre1918" | "дореформенная" | "дореф" | "старая" => {
            "ru-1918"
        }
        "long-s" | "longs" | "long_s" | "early-modern" | "ligatures" => "long-s",
        other => {
            return Err(format!(
                "modernize: unknown spelling table '{other}' (ru-1918/дореформенная, long-s)"
            ));
        }
    };
    let text: String = text.nfc().collect();
    Ok(match table {
        "ru-1918" => ru_1918(&text),
        "long-s" => long_s(&text),
        _ => unreachable!(),
    })
}

/// The letters the 1918 reform removed, to their replacements.
const RU_1918: &[(char, &str)] = &[
    ('ѣ', "е"),
    ('Ѣ', "Е"),
    ('і', "и"),
    ('І', "И"),
    ('ѳ', "ф"),
    ('Ѳ', "Ф"),
    ('ѵ', "и"),
    ('Ѵ', "И"),
];

fn ru_1918(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if let Some((_, m)) = RU_1918.iter().find(|(k, _)| *k == c) {
            out.push_str(m);
            continue;
        }
        // The hard sign at the end of a word — before a non-letter
        // or the end — goes; inside a word (объ-) it stays.
        if (c == 'ъ' || c == 'Ъ')
            && i > 0
            && chars[i - 1].is_alphabetic()
            && !chars.get(i + 1).is_some_and(|n| n.is_alphabetic())
        {
            continue;
        }
        out.push(c);
    }
    out
}

/// Early-modern Latin typography: the long s and the typographic
/// ligatures, opened.
const LONG_S: &[(char, &str)] = &[
    ('ſ', "s"),
    ('ﬀ', "ff"),
    ('ﬁ', "fi"),
    ('ﬂ', "fl"),
    ('ﬃ', "ffi"),
    ('ﬄ', "ffl"),
    ('ﬅ', "ft"),
    ('ﬆ', "st"),
];

fn long_s(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match LONG_S.iter().find(|(k, _)| *k == c) {
            Some((_, m)) => out.push_str(m),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod spelling_tests {
    use super::modernize;

    #[test]
    fn ru_1918_folds_the_removed_letters_and_the_final_hard_sign() {
        assert_eq!(
            modernize("Повѣсти Бѣлкина", "ru-1918").unwrap(),
            "Повести Белкина"
        );
        assert_eq!(modernize("міръ и миръ", "ru-1918").unwrap(), "мир и мир");
        assert_eq!(
            modernize("Ѳедоръ объѣхалъ", "дореформенная").unwrap(),
            "Федор объехал"
        );
        assert_eq!(modernize("ъ", "ru-1918").unwrap(), "ъ");
        assert!(modernize("x", "klingon").is_err());
    }

    #[test]
    fn long_s_and_ligatures_open() {
        assert_eq!(
            modernize("ſhall ﬁnd ﬆill", "long-s").unwrap(),
            "shall find still"
        );
    }
}
