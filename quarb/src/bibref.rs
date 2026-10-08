//! Reference texts read under a citation standard — `| bibref(gost)`
//! and the `?references=` lowering (spec: Reference Lists).
//!
//! A reference is one text (a bibliography's paragraph) written
//! under a standard's grammar; the parser reads the fields that
//! grammar places without doubt and leaves the rest absent, and a
//! text that does not parse as a reference at all is `None` (the
//! function's null), as `strptime` answers for a text that is no
//! date. Four standards, each an open tag the implementation
//! carries (an unknown tag is refused naming them):
//!
//! - `gost` — GOST R 7.0.100-2018: areas separated by ` . – `,
//!   the heading `Author. Title : subtitle / responsibility`, the
//!   analytic ` // host` for an article, `Город : Издатель, год`,
//!   `320 с.` / `С. 45–52`, `Т. 5`, `№ 3`, `ISBN`, `DOI`, `URL:`
//!   with `(дата обращения: …)`, the content-and-medium note
//!   (`Текст : электронный`) or `[Электронный ресурс]`.
//! - `gb7714` — GB/T 7714-2015: `作者. 题名[M]. 出版地: 出版者,
//!   年: 页.`, the type code after the title ([M] [J] [C] [D] [N]
//!   [R] [S] [P] [EB] …, `/OL` the carrier), `刊名, 年, 卷(期):
//!   页.`, `(更新日期)[引用日期]. URL`; full-width and ASCII
//!   punctuation alike.
//! - `cmos` — Chicago (bibliography entries): `Surname, Given.
//!   Title. City: Publisher, Year.`, an article `"Title." Journal
//!   vol, no. N (Year): pages.`, a DOI or URL at the end.
//! - `ams` — AMS (amsplain): `I. Surname, Title, Publisher, City,
//!   Year.`, an article `I. Surname, Title, J. Name vol (Year), no.
//!   N, pages.`.
//!
//! Numbering prefixes (`12.`, `[12]`) are stripped before the
//! parse and remembered as the entry's label.

use std::collections::HashMap;

/// The standards the implementation carries, by tag.
pub const STANDARDS: &[&str] = &["gost", "gb7714", "cmos", "ams"];

/// A reference as read: its list label when the text was
/// numbered, its genus, and the fields the grammar placed, under
/// the English (BibLaTeX-style) names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reference {
    pub label: Option<String>,
    pub genus: Option<&'static str>,
    pub fields: Vec<(&'static str, String)>,
}

impl Reference {
    fn set(&mut self, key: &'static str, value: impl Into<String>) {
        let value: String = value.into();
        let value = value.trim().trim_end_matches([',', ';']).trim().to_string();
        if value.is_empty() || self.fields.iter().any(|(k, _)| *k == key) {
            return;
        }
        self.fields.push((key, value));
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The field under the bibliography's canonical campus (what
    /// a lowered `bib` node carries): the English names map to
    /// the Latin canon the census aliases.
    pub fn campi(&self) -> Vec<(String, String)> {
        self.fields
            .iter()
            .map(|(k, v)| (campus(k).to_string(), v.clone()))
            .collect()
    }
}

/// The canonical campus of an English field name.
pub fn campus(key: &str) -> &'static str {
    match key {
        "author" => "auctor",
        "title" => "titulus",
        "year" => "annus",
        "journal" => "ephemeris",
        "booktitle" => "titulus-libri",
        "volume" => "volumen",
        "number" => "numerus",
        "pages" => "paginae",
        "publisher" => "officina",
        "address" => "locus",
        "edition" => "editio",
        "series" => "series",
        "institution" => "institutum",
        "medium" => "modus",
        "url" => "url",
        "doi" => "doi",
        "isbn" => "isbn",
        "urldate" => "urldate",
        "note" => "nota",
        _ => "nota",
    }
}

/// Parse `text` under `standard`; `Err` names the standards for a
/// tag the implementation lacks, `Ok(None)` a text that is no
/// reference under the grammar.
pub fn parse(standard: &str, text: &str) -> Result<Option<Reference>, String> {
    if !STANDARDS.contains(&standard) {
        return Err(format!(
            "bibref: unknown standard {standard:?} — this build reads {}",
            STANDARDS.join(", ")
        ));
    }
    let text = normalize(text);
    let (label, body) = strip_label(&text);
    let body = body.trim();
    if body.chars().filter(|c| c.is_alphabetic()).count() < 4 {
        return Ok(None);
    }
    let mut r = match standard {
        "gost" => gost(body),
        "gb7714" => gb7714(body),
        "cmos" => cmos(body),
        "ams" => ams(body),
        other => {
            return Err(format!(
                "bibref: unknown standard {other:?} — this build reads {}",
                STANDARDS.join(", ")
            ));
        }
    };
    if let Some(r) = r.as_mut() {
        r.label = label;
    }
    Ok(r)
}

/// Whitespace collapsed; full-width punctuation and the dashes a
/// standard admits brought to one spelling.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = true;
    for c in text.chars() {
        let c = match c {
            '\u{FF0E}' => '.',
            '\u{FF1A}' => ':',
            '\u{FF0C}' => ',',
            '\u{FF1B}' => ';',
            '\u{FF08}' => '(',
            '\u{FF09}' => ')',
            '\u{FF3B}' | '\u{3010}' => '[',
            '\u{FF3D}' | '\u{3011}' => ']',
            '\u{3002}' => '.',
            '\u{2013}' | '\u{2014}' | '\u{2012}' | '\u{2015}' => '–',
            '\u{00A0}' | '\u{3000}' => ' ',
            c => c,
        };
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// `12.`, `12)`, `[12]`, `12` + tab/space before a capital or a
/// CJK start: the entry's number, off the text.
fn strip_label(text: &str) -> (Option<String>, &str) {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix('[') {
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !n.is_empty()
            && let Some(after) = rest[n.len()..].strip_prefix(']')
        {
            return (Some(n), after.trim_start());
        }
    }
    let n: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !n.is_empty() && n.len() <= 4 {
        let rest = &t[n.len()..];
        if let Some(after) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
            // `12. Author` — but not a year opening a text.
            let after = after.trim_start();
            if n.len() <= 3 && after.chars().next().is_some_and(|c| !c.is_ascii_digit()) {
                return (Some(n), after);
            }
        }
    }
    (None, t)
}

fn year_in(s: &str) -> Option<String> {
    // The last four-digit year in the text, 1500–2099.
    let b = s.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i..i + 4].iter().all(u8::is_ascii_digit)
            && (i == 0 || !b[i - 1].is_ascii_digit())
            && (i + 4 == b.len() || !b[i + 4].is_ascii_digit())
        {
            let y: u32 = s[i..i + 4].parse().unwrap_or(0);
            if (1500..=2099).contains(&y) {
                found = Some(s[i..i + 4].to_string());
            }
        }
        i += 1;
    }
    found
}

fn first_year(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i..i + 4].iter().all(u8::is_ascii_digit)
            && (i == 0 || !b[i - 1].is_ascii_digit())
            && (i + 4 == b.len() || !b[i + 4].is_ascii_digit())
        {
            let y: u32 = s[i..i + 4].parse().unwrap_or(0);
            if (1500..=2099).contains(&y) {
                return Some(s[i..i + 4].to_string());
            }
        }
        i += 1;
    }
    None
}

fn find_url(s: &str) -> Option<String> {
    let start = s.find("http://").or_else(|| s.find("https://"))?;
    // To the next space; a parenthesis inside a URL (Wikisource's
    // `(Пушкин)`) stays, an unbalanced closing one at the end is
    // the text's.
    let mut url: String = s[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '[' && *c != ']')
        .collect();
    while url.ends_with(')') && url.matches(')').count() > url.matches('(').count() {
        url.pop();
    }
    Some(url.trim_end_matches(['.', ',', ';']).to_string())
}

fn find_doi(s: &str) -> Option<String> {
    let i = s.find("10.").filter(|&i| {
        let before = &s[..i];
        before.to_ascii_lowercase().contains("doi") || before.ends_with('/')
    })?;
    let doi: String = s[i..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '(' && *c != ')')
        .collect();
    Some(doi.trim_end_matches(['.', ',', ';']).to_string())
}

fn pages_range(s: &str) -> Option<String> {
    // `45–52`, `45-52`, `45`.
    let t = s.trim().trim_end_matches('.');
    let ok = !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == '–' || c == '-')
        && t.chars().next().is_some_and(|c| c.is_ascii_digit());
    ok.then(|| t.replace('-', "–"))
}

// ---------------------------------------------------------------
// GOST R 7.0.100-2018
// ---------------------------------------------------------------

/// The areas of a GOST reference: split at ` . – ` (the area
/// separator; a hyphen reads as the dash too).
fn gost_areas(body: &str) -> Vec<String> {
    let mut areas = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // `. – ` or `. - ` — not inside an URL.
        if chars[i] == '.'
            && i + 3 < chars.len()
            && chars[i + 1] == ' '
            && (chars[i + 2] == '–' || chars[i + 2] == '-')
            && chars[i + 3] == ' '
        {
            areas.push(cur.trim().to_string());
            cur.clear();
            i += 4;
            continue;
        }
        cur.push(chars[i]);
        i += 1;
    }
    let tail = cur.trim().trim_end_matches('.').trim().to_string();
    if !tail.is_empty() {
        areas.push(tail);
    }
    areas
}

/// The 7.1 shape split at sentence ends, an initial's period kept
/// with its name and the heading (name and title) as one area.
fn gost_sentences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        cur.push(c);
        if c == '.' && chars.get(i + 1) == Some(&' ') {
            let word: String = cur
                .trim_end_matches('.')
                .rsplit(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_string();
            // An initial, a place abbreviation (`М.`, `Л.`, `СПб.`),
            // `с.`/`С.`/`Т.`/`Вып.` keep their sentence.
            let core = word.trim_end_matches(',');
            let initial =
                core.chars().count() <= 2 && core.chars().next().is_some_and(char::is_uppercase);
            let abbr = matches!(
                core,
                "СПб"
                    | "Пг"
                    | "с"
                    | "С"
                    | "Т"
                    | "Вып"
                    | "Кн"
                    | "Ч"
                    | "изд"
                    | "ред"
                    | "пер"
                    | "т"
                    | "др"
                    | "сост"
            );
            if !initial && !abbr {
                out.push(cur.trim().trim_end_matches('.').trim().to_string());
                cur.clear();
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    let tail = cur.trim().trim_end_matches('.').trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    // The heading and the title are one area: rejoin the first
    // two when the first is a bare name.
    if out.len() >= 2
        && gost_heading(&out[0]).is_none()
        && gost_heading(&format!("{}. {}", out[0], out[1])).is_some()
    {
        let joined = format!("{}. {}", out[0], out[1]);
        out.splice(0..2, [joined]);
    }
    out
}

/// `Фамилия, И. О. Заглавие` → (`Фамилия, И. О.`, `Заглавие`): the
/// surname (one or two words, a comma after it or not) and every
/// initial that follows, each a capital or a digraph with a
/// period; `None` when the heading opens with no such name.
fn gost_heading(heading: &str) -> Option<(String, &str)> {
    let h = heading.trim();
    let mut rest = h;
    let is_initial = |w: &str| {
        let core = w.trim_end_matches(',').trim_end_matches('.');
        w.ends_with('.')
            && !core.is_empty()
            && core.chars().count() <= 2
            && core.chars().next().is_some_and(char::is_uppercase)
            && core.chars().all(char::is_alphabetic)
    };
    let mut words = 0;
    let mut author = String::new();
    loop {
        let w: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        if w.is_empty() || is_initial(&w) {
            break;
        }
        if words >= 2 || !w.chars().next().is_some_and(char::is_uppercase) {
            return None;
        }
        author.push_str(&w);
        author.push(' ');
        rest = rest[w.len()..].trim_start();
        words += 1;
    }
    if words == 0 {
        return None;
    }
    let mut initials = 0;
    loop {
        let w: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        if !is_initial(&w) || w.ends_with(',') {
            break;
        }
        author.push_str(&w);
        author.push(' ');
        rest = rest[w.len()..].trim_start();
        initials += 1;
    }
    if initials == 0 {
        return None;
    }
    let author = author.trim().trim_end_matches(',').to_string();
    let title = rest.trim();
    (!title.is_empty()).then_some((author, title))
}

/// `Фамилия, И. О.` / `Фамилия И. О.` — a name with initials.
fn looks_like_person(s: &str) -> bool {
    let s = s.trim();
    let has_initial = s.split_whitespace().any(|w| {
        w.len() <= 3 && w.ends_with('.') && w.chars().next().is_some_and(char::is_uppercase)
    });
    let first_upper = s.chars().next().is_some_and(char::is_uppercase);
    has_initial && first_upper && !s.contains(':') && s.chars().count() < 60
}

fn gost(body: &str) -> Option<Reference> {
    let mut areas = gost_areas(body);
    let dashed = areas.len() >= 2;
    if !dashed {
        // GOST 7.1's shape, periods for areas: `Пушкин А. С.
        // Капитанская дочка. М.: Наука, 1964. 285 с.` — the
        // sentences after the heading become the areas.
        areas = gost_sentences(body);
    }
    let head = areas.first()?.as_str();
    // An entry has areas after its heading and one of the
    // heading's own marks: a person, a responsibility statement
    // (` / `), an analytic host (` // `), or a publication area.
    // In the 7.1 shape (no dashed areas) the heading's name is the
    // one mark; a publication area alone is any style's.
    let marked = head.contains(" / ")
        || head.contains(" // ")
        || gost_heading(head).is_some()
        || (dashed
            && areas
                .iter()
                .skip(1)
                .any(|a| a.contains(':') && year_in(a).is_some()));
    if head.is_empty() || areas.len() < 2 || !marked {
        return None;
    }
    let mut r = Reference::default();
    // The heading: `Author. Title : subtitle / responsibility // host`.
    let (heading, host) = match head.split_once(" // ") {
        Some((h, host)) => (h, Some(host)),
        None => (head, None),
    };
    let (heading, responsibility) = match heading.split_once(" / ") {
        Some((h, resp)) => (h, Some(resp)),
        None => (heading, None),
    };
    // The author is the heading's first sentence when it reads as
    // a person; the title the rest.
    // The split eats the sentence period; the initial keeps it.
    // The heading is a surname and its initials — each a capital
    // (a digraph like `Ю.`/`Ya.` too) closed by a period, with a
    // comma after the surname in 7.0.100 and none in 7.1 — so a
    // period that closes an initial does not end it; the title
    // begins after the last initial.
    let (author, title) = match gost_heading(heading) {
        Some((a, t)) => (Some(a), t),
        None => (None, heading.trim()),
    };
    let mut title = title.to_string();
    let mut medium: Option<String> = None;
    if let Some(i) = title.find("[Электронный ресурс]") {
        medium = Some("электронный".into());
        title = format!(
            "{} {}",
            &title[..i],
            &title[i + "[Электронный ресурс]".len()..]
        );
    }
    let title = normalize(&title);
    if title.is_empty() {
        return None;
    }
    if let Some(a) = author {
        r.set("author", a);
    } else if let Some(resp) = responsibility
        && looks_like_person(resp.split(',').next().unwrap_or(resp))
    {
        r.set("author", resp.split(" ; ").next().unwrap_or(resp));
    }
    r.set("title", title.as_str());
    let mut genus = if host.is_some() { "article" } else { "book" };
    if title.starts_with("ГОСТ") || title.starts_with("ISO ") {
        genus = "standard";
    }
    if let Some(resp) = responsibility
        && (resp.contains("дис.") || resp.contains("диссертац"))
    {
        genus = "thesis";
    }
    if body.contains(" : дис. ") || body.contains(": дис. ") || body.contains("автореф. дис.")
    {
        genus = "thesis";
    }
    if let Some(host) = host {
        // `Журнал. – 2019. – Т. 5, № 3. – С. 45–52` continues in the
        // areas; the host's name is the text before the first area
        // break, which the split already made the heading's tail.
        r.set("journal", host.trim());
    }
    for area in areas.iter().skip(1) {
        let a = area.trim();
        let lower = a.to_lowercase();
        if let Some(rest) = a.strip_prefix("URL:").or_else(|| a.strip_prefix("URL :")) {
            if let Some(u) = find_url(rest) {
                r.set("url", u);
            }
            if let Some(i) = rest.find("дата обращения:") {
                let d: String = rest[i + "дата обращения:".len()..]
                    .chars()
                    .skip_while(|c| c.is_whitespace())
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                    .collect();
                r.set("urldate", d);
            }
            continue;
        }
        if let Some(rest) = a.strip_prefix("DOI:").or_else(|| a.strip_prefix("DOI ")) {
            r.set("doi", rest.trim());
            continue;
        }
        if let Some(rest) = a.strip_prefix("ISBN") {
            r.set("isbn", rest.trim());
            continue;
        }
        if lower.starts_with("текст :")
            || lower.starts_with("текст:")
            || lower.starts_with("изображение")
        {
            medium = Some(a.split(':').nth(1).unwrap_or("").trim().to_string());
            continue;
        }
        // Pages: `320 с.` (a book's extent), `С. 45–52` (an
        // article's range).
        if let Some(p) = a.strip_prefix("С.").or_else(|| a.strip_prefix("C.")) {
            if let Some(p) = pages_range(p) {
                r.set("pages", p);
            }
            continue;
        }
        if let Some(n) = a.strip_suffix(" с.").or_else(|| a.strip_suffix(" с")) {
            if n.chars().all(|c| c.is_ascii_digit() || c == ' ') {
                r.set("pages", n.trim());
                continue;
            }
        }
        // Volume and number: `Т. 5, № 3`, `Вып. 2`, `№ 3`.
        if a.starts_with("Т.")
            || a.starts_with("№")
            || a.starts_with("Вып.")
            || a.starts_with("Кн.")
        {
            for part in a.split(',') {
                let part = part.trim();
                if let Some(v) = part.strip_prefix("Т.") {
                    r.set("volume", v.trim());
                } else if let Some(n) = part.strip_prefix("№") {
                    r.set("number", n.trim());
                } else if let Some(n) = part.strip_prefix("Вып.") {
                    r.set("number", n.trim());
                }
            }
            continue;
        }
        // A bare year area (an article's), or the publication area
        // `Город : Издатель, 2019`.
        if a.len() == 4 && a.chars().all(|c| c.is_ascii_digit()) {
            r.set("year", a);
            continue;
        }
        // The publication area: `Город : Издатель, 2019`, or 7.1's
        // `М.: Наука, 1964`; the first year of a span.
        if let Some((place, rest)) = a.split_once(" : ").or_else(|| a.split_once(':'))
            && !place.trim().contains(' ')
            && let Some(y) = first_year(rest)
        {
            let year_at = rest.find(&y).unwrap_or(rest.len());
            r.set("address", place.trim());
            r.set("publisher", rest[..year_at].trim().trim_end_matches(','));
            r.set("year", y);
            continue;
        }
        if lower.contains("изд.") && (lower.contains("-е") || lower.contains("перераб"))
        {
            r.set("edition", a);
            continue;
        }
    }
    if r.get("year").is_none() {
        // Not the access date: the text before the `URL:` area.
        let before_url = body.split("URL:").next().unwrap_or(body);
        if let Some(y) = first_year(before_url) {
            r.set("year", y);
        }
    }
    if let Some(m) = medium {
        r.set("medium", m);
    }
    r.genus = Some(genus);
    Some(r)
}

// ---------------------------------------------------------------
// GB/T 7714-2015
// ---------------------------------------------------------------

fn gb_genus(code: &str) -> Option<&'static str> {
    Some(match code {
        "M" => "book",
        "J" => "article",
        "C" => "inproceedings",
        "D" => "thesis",
        "N" => "newspaper",
        "R" => "report",
        "S" => "standard",
        "P" => "patent",
        "EB" => "online",
        "DB" => "database",
        "CP" => "software",
        "A" => "incollection",
        "G" => "collection",
        "Z" => "misc",
        _ => return None,
    })
}

fn gb7714(body: &str) -> Option<Reference> {
    // The type code after the title: `[M]`, `[J]`, `[EB/OL]`.
    let (code_at, code, carrier, after_code) = {
        let mut found = None;
        let mut i = 0;
        while let Some(off) = body[i..].find('[') {
            let at = i + off;
            let end = match body[at..].find(']') {
                Some(e) => at + e,
                None => break,
            };
            let inner = &body[at + 1..end];
            let (code, carrier) = match inner.split_once('/') {
                Some((c, k)) => (c, Some(k)),
                None => (inner, None),
            };
            if gb_genus(code).is_some()
                && carrier.is_none_or(|k| matches!(k, "OL" | "DK" | "MT" | "CD"))
            {
                found = Some((
                    at,
                    code.to_string(),
                    carrier.map(str::to_string),
                    body[end + 1..].to_string(),
                ));
                break;
            }
            i = end + 1;
        }
        found?
    };
    let before = body[..code_at].trim();
    let mut r = Reference::default();
    // `作者. 题名` — the author ends at the first `. ` (a CJK
    // author list has no space: `王明, 李华. 题名`).
    let (author, title) = match before.split_once(". ") {
        Some((a, t)) if !a.is_empty() => (Some(a.trim()), t.trim()),
        _ => match before.find(".") {
            Some(i) if i > 0 && !before[..i].contains(' ') => {
                (Some(before[..i].trim()), before[i + 1..].trim())
            }
            _ => (None, before),
        },
    };
    if title.is_empty() {
        return None;
    }
    if let Some(a) = author {
        r.set("author", a);
    }
    r.set("title", title);
    if let Some(k) = &carrier {
        r.set("medium", k.as_str());
    }
    r.genus = gb_genus(&code);
    let rest = after_code.trim().trim_start_matches('.').trim();
    // The dates: `(更新日期)[引用日期]`.
    let mut rest_s = rest.to_string();
    if let Some(open) = rest_s.find('(')
        && let Some(close) = rest_s[open..].find(')')
    {
        let d = rest_s[open + 1..open + close].trim().to_string();
        if d.len() >= 4 && d.chars().take(4).all(|c| c.is_ascii_digit()) {
            r.set("year", &d[..4]);
            rest_s = format!("{} {}", &rest_s[..open], &rest_s[open + close + 1..]);
        }
    }
    if let Some(open) = rest_s.find('[')
        && let Some(close) = rest_s[open..].find(']')
    {
        let d = rest_s[open + 1..open + close].trim().to_string();
        if d.len() >= 4 && d.chars().take(4).all(|c| c.is_ascii_digit()) {
            r.set("urldate", d);
            rest_s = format!("{} {}", &rest_s[..open], &rest_s[open + close + 1..]);
        }
    }
    if let Some(u) = find_url(&rest_s) {
        r.set("url", u.clone());
        rest_s = rest_s.replace(&u, "");
    }
    if let Some(d) = find_doi(&rest_s) {
        r.set("doi", d);
    }
    let rest = normalize(&rest_s);
    let rest = rest.trim().trim_end_matches('.').trim();
    match code.as_str() {
        "J" | "N" => {
            // `刊名, 年, 卷(期): 页` or `报纸名, 年-月-日(版次)`.
            let mut parts = rest.split(',').map(str::trim);
            if let Some(j) = parts.next().filter(|j| !j.is_empty()) {
                r.set("journal", j);
            }
            for p in parts {
                if r.get("year").is_none()
                    && year_in(p).is_some()
                    && p.chars().take(4).all(|c| c.is_ascii_digit())
                {
                    r.set("year", &p[..4]);
                    continue;
                }
                // `卷(期): 页`
                let (vol_no, pages) = match p.split_once(':') {
                    Some((v, pg)) => (v.trim(), Some(pg.trim())),
                    None => (p, None),
                };
                if let Some((v, n)) = vol_no.split_once('(') {
                    if !v.trim().is_empty() {
                        r.set("volume", v.trim());
                    }
                    r.set("number", n.trim_end_matches(')').trim());
                } else if !vol_no.is_empty() && vol_no.chars().all(|c| c.is_ascii_digit()) {
                    r.set("volume", vol_no);
                }
                if let Some(pg) = pages.and_then(pages_range) {
                    r.set("pages", pg);
                }
            }
        }
        _ => {
            // `出版地: 出版者, 年: 页` (a thesis `学位授予单位`, a
            // report `机构`).
            let (place_pub, year_pages) = match rest.rsplit_once(',') {
                Some((a, b)) if year_in(b).is_some() => (a.trim(), b.trim()),
                _ => (rest, ""),
            };
            if let Some((place, publisher)) = place_pub.split_once(':') {
                r.set("address", place.trim());
                r.set("publisher", publisher.trim());
            } else if !place_pub.is_empty() && !year_pages.is_empty() {
                r.set("publisher", place_pub);
            }
            let (year, pages) = match year_pages.split_once(':') {
                Some((y, p)) => (y.trim(), Some(p.trim())),
                None => (year_pages, None),
            };
            if let Some(y) = year_in(year) {
                r.set("year", y);
            }
            if let Some(pg) = pages.and_then(pages_range) {
                r.set("pages", pg);
            }
        }
    }
    if r.get("year").is_none()
        && let Some(y) = year_in(body)
    {
        r.set("year", y);
    }
    Some(r)
}

// ---------------------------------------------------------------
// Chicago (bibliography entries)
// ---------------------------------------------------------------

/// `Surname, Given`, `Surname, Given, and Given Surname` — a name
/// list opening with an inverted name.
fn cmos_author(s: &str) -> bool {
    let s = s.trim();
    let Some((surname, rest)) = s.split_once(", ") else {
        return false;
    };
    // The surname stands first; a part that opens with an initial
    // (`G. Cantor, Über …`) is the AMS order, not Chicago's.
    let opens_with_initial = surname
        .split_whitespace()
        .next()
        .is_some_and(|w| w.ends_with('.') && w.chars().filter(|c| c.is_alphabetic()).count() <= 2);
    let ok_surname = !surname.is_empty()
        && surname.chars().next().is_some_and(char::is_uppercase)
        && surname.split_whitespace().count() <= 3
        && !surname.contains('"')
        && !opens_with_initial;
    let rest_ok = rest.chars().next().is_some_and(|c| c.is_uppercase()) && !rest.contains('"');
    ok_surname && rest_ok && s.chars().count() < 120
}

/// Sentences of a Chicago entry: split at `. ` outside quotes and
/// parentheses, an initial's period kept with its name.
fn cmos_segments(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut in_quote = false;
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let was_quoted = in_quote;
        match c {
            '"' => in_quote = !in_quote,
            '“' => in_quote = true,
            '”' => in_quote = false,
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        cur.push(c);
        // A closing quote after the period that ends the quoted
        // title (`"Title." Journal`) ends the segment too.
        if was_quoted
            && !in_quote
            && (c == '"' || c == '”')
            && cur.trim_end_matches(['"', '”']).ends_with('.')
            && chars.get(i + 1) == Some(&' ')
        {
            out.push(cur.trim().to_string());
            cur.clear();
            i += 2;
            continue;
        }
        if c == '.' && !in_quote && depth <= 0 && chars.get(i + 1) == Some(&' ') {
            // An initial (`J.`) or an abbreviation (`no.`, `vol.`,
            // `ed.`, `trans.`) does not end a segment.
            let word: String = cur
                .trim_end_matches('.')
                .rsplit(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_string();
            let abbr = word.chars().count() == 1
                || matches!(
                    word.to_ascii_lowercase().as_str(),
                    "no" | "vol"
                        | "ed"
                        | "eds"
                        | "trans"
                        | "rev"
                        | "comp"
                        | "jr"
                        | "st"
                        | "mr"
                        | "mrs"
                        | "dr"
                        | "pp"
                        | "p"
                        | "ch"
                        | "chap"
                        | "sec"
                );
            if !abbr {
                out.push(cur.trim().to_string());
                cur.clear();
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    let tail = cur.trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn cmos(body: &str) -> Option<Reference> {
    // A Chicago entry dates itself; prose seldom does.
    year_in(body)?;
    let segs = cmos_segments(body);
    if segs.is_empty() {
        return None;
    }
    let mut r = Reference::default();
    let mut idx = 0;
    // An inverted name — or, with the title and an imprint still
    // to come, a one-word author (the ancients: `Herodotus. The
    // Histories. …`).
    let one_word = |s: &str| {
        let s = s.trim_end_matches('.');
        !s.is_empty()
            && !s.contains([' ', ',', '"', '“'])
            && s.chars().next().is_some_and(char::is_uppercase)
            && s.chars().count() <= 30
    };
    if cmos_author(&segs[0]) || (segs.len() >= 3 && one_word(&segs[0])) {
        r.set("author", segs[0].trim_end_matches('.'));
        idx = 1;
    }
    let Some(title_seg) = segs.get(idx) else {
        return None;
    };
    let quoted = title_seg.starts_with('"') || title_seg.starts_with('“');
    let title = title_seg
        .trim()
        .trim_matches(|c| c == '"' || c == '“' || c == '”')
        .trim_end_matches('.')
        .trim()
        .to_string();
    // An entry names its author or quotes its title; a prose
    // sentence with a year in it does neither.
    if title.is_empty() || (r.get("author").is_none() && !quoted) {
        return None;
    }
    r.set("title", title);
    idx += 1;
    let tail = segs[idx..].join(" ");
    if let Some(d) = find_doi(&tail) {
        r.set("doi", d);
    } else if let Some(u) = find_url(&tail) {
        r.set("url", u);
    }
    if quoted {
        // `Journal vol, no. N (Year): pages.` — or a chapter `In
        // Book, edited by …, pages. City: Publisher, Year.`
        let rest = segs.get(idx).map(String::as_str).unwrap_or("");
        if let Some(book) = rest.strip_prefix("In ") {
            r.genus = Some("incollection");
            r.set("booktitle", book.split(',').next().unwrap_or(book));
            // `…, edited by X, 101–159.`: the pages close the part.
            if let Some(p) = book
                .rsplit(',')
                .next()
                .and_then(|p| pages_range(p.trim().trim_end_matches('.')))
            {
                r.set("pages", p);
            }
            if let Some(y) = year_in(&tail) {
                r.set("year", y);
            }
            if let Some(i) = tail.rfind(": ")
                && let Some((place, publisher)) = tail[..i]
                    .rsplit_once(". ")
                    .map(|(_, p)| p)
                    .unwrap_or(&tail[..i])
                    .split_once(": ")
            {
                r.set("address", place);
                r.set("publisher", publisher.trim_end_matches(','));
            }
        } else {
            r.genus = Some("article");
            if let Some(open) = rest.find('(')
                && let Some(close) = rest[open..].find(')')
            {
                let before = rest[..open].trim();
                let year = rest[open + 1..open + close].trim();
                if let Some(y) = year_in(year) {
                    r.set("year", y);
                }
                let after = rest[open + close + 1..]
                    .trim()
                    .trim_start_matches(':')
                    .trim();
                if let Some(p) = pages_range(after.split(['.', ' ']).next().unwrap_or("")) {
                    r.set("pages", p);
                }
                // `Journal 12, no. 3`
                let (journal_vol, number) = match before.split_once(", no. ") {
                    Some((jv, n)) => (jv.trim(), Some(n.trim())),
                    None => (before, None),
                };
                let (journal, vol) = match journal_vol.rsplit_once(' ') {
                    Some((j, v)) if v.chars().all(|c| c.is_ascii_digit()) => (j, Some(v)),
                    _ => (journal_vol, None),
                };
                r.set("journal", journal);
                if let Some(v) = vol {
                    r.set("volume", v);
                }
                if let Some(n) = number {
                    r.set("number", n);
                }
            } else if !rest.is_empty() {
                r.set("journal", rest.split(',').next().unwrap_or(rest));
                if let Some(y) = year_in(&tail) {
                    r.set("year", y);
                }
            }
        }
    } else {
        r.genus = Some("book");
        // The remaining segments: `Vol. 1.`, `2nd ed.`, `Translated
        // by …`, `City: Publisher, Year.`
        for seg in &segs[idx..] {
            let mut seg = seg.trim_end_matches('.').trim();
            // `Vol. 1.` glues to the next sentence (the abbreviation
            // rule): the volume, then the rest.
            if let Some(v) = seg.strip_prefix("Vol. ") {
                let (num, rest) = v.split_once(". ").unwrap_or((v, ""));
                r.set("volume", num);
                seg = rest.trim();
                if seg.is_empty() {
                    continue;
                }
            }
            if seg.ends_with(" ed") || seg.ends_with(" edition") {
                r.set("edition", seg);
            } else if let Some((place, pub_year)) = seg.split_once(": ")
                && let Some(y) = year_in(pub_year)
                && pub_year.trim_end_matches('.').ends_with(&y)
            {
                r.set("address", place);
                r.set(
                    "publisher",
                    pub_year[..pub_year.len() - 4].trim().trim_end_matches(','),
                );
                r.set("year", y);
            }
        }
        if r.get("year").is_none()
            && let Some(y) = year_in(&tail)
        {
            r.set("year", y);
        }
    }
    Some(r)
}

// ---------------------------------------------------------------
// AMS (amsplain)
// ---------------------------------------------------------------

/// `I. Surname`, `I. J. Surname and K. Other` — initials before
/// surnames.
fn ams_author(s: &str) -> bool {
    let s = s.trim();
    let words: Vec<&str> = s.split_whitespace().collect();
    let is_initial = |w: &str| {
        w.ends_with('.')
            && w.chars().next().is_some_and(char::is_uppercase)
            && w.chars().filter(|c| c.is_alphabetic()).count() <= 2
            && w.chars().all(|c| c.is_alphabetic() || c == '.' || c == '-')
    };
    let initials = words.iter().filter(|w| is_initial(w)).count();
    initials >= 1
        && words.first().is_some_and(|w| is_initial(w))
        && !s.contains('(')
        && s.chars().count() < 120
}

fn ams(body: &str) -> Option<Reference> {
    year_in(body)?;
    let body = body.trim().trim_end_matches('.');
    // Commas outside parentheses are the field separators.
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in body.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth <= 0 {
            parts.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    if parts.len() < 2 {
        return None;
    }
    // An AMS entry opens with its authors (initials before the
    // surnames); without them the comma fields name nothing.
    if !ams_author(&parts[0]) {
        return None;
    }
    let mut r = Reference::default();
    r.set("author", parts[0].as_str());
    let mut idx = 1;
    let title = parts.get(idx)?.trim();
    if title.is_empty() || !title.chars().next().is_some_and(|c| c.is_alphabetic()) {
        return None;
    }
    r.set("title", title);
    idx += 1;
    let rest: Vec<&str> = parts[idx..].iter().map(String::as_str).collect();
    let tail = rest.join(", ");
    if let Some(d) = find_doi(&tail) {
        r.set("doi", d);
    } else if let Some(u) = find_url(&tail) {
        r.set("url", u);
    }
    // An article: `J. Name vol (Year)` as one part, then `no. N`,
    // then the pages.
    let journal_part = rest
        .iter()
        .position(|p| p.contains('(') && p.ends_with(')') && year_in(p).is_some());
    if let Some(ji) = journal_part {
        r.genus = Some("article");
        let p = rest[ji];
        let open = p.rfind('(').unwrap_or(p.len());
        let head = p[..open].trim();
        if let Some(y) = year_in(&p[open..]) {
            r.set("year", y);
        }
        let (journal, vol) = match head.rsplit_once(' ') {
            Some((j, v)) if v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty() => (j, Some(v)),
            _ => (head, None),
        };
        r.set("journal", journal);
        if let Some(v) = vol {
            r.set("volume", v);
        }
        for p in &rest[ji + 1..] {
            if let Some(n) = p.strip_prefix("no. ") {
                r.set("number", n);
            } else if let Some(pg) = pages_range(p) {
                r.set("pages", pg);
            } else if let Some(pg) = p.strip_prefix("pp. ").and_then(pages_range) {
                r.set("pages", pg);
            }
        }
    } else {
        r.genus = Some("book");
        // `Publisher, City, Year` — the year last, the city before
        // it, the publisher before that; `vol. N` and `2nd ed.`
        // where they stand.
        let mut tail_parts: Vec<&str> = rest.clone();
        if let Some(last) = tail_parts.last()
            && let Some(y) = year_in(last)
            && last.trim().len() == 4
        {
            r.set("year", y);
            tail_parts.pop();
        }
        let mut plain: Vec<&str> = Vec::new();
        for p in tail_parts {
            if let Some(v) = p.strip_prefix("vol. ").or_else(|| p.strip_prefix("Vol. ")) {
                r.set("volume", v);
            } else if p.ends_with(" ed.") || p.ends_with(" ed") {
                r.set("edition", p);
            } else if p.starts_with("Lecture Notes") || p.starts_with("Grad. Texts") {
                r.set("note", p);
            } else {
                plain.push(p);
            }
        }
        match plain.as_slice() {
            [publisher, city] => {
                r.set("publisher", *publisher);
                r.set("address", *city);
            }
            // `Mathematische Werke, Mayer & Müller, Berlin`: the
            // series or collection, then the imprint.
            [series, publisher, city] => {
                r.set("series", *series);
                r.set("publisher", *publisher);
                r.set("address", *city);
            }
            [publisher] => r.set("publisher", *publisher),
            _ => {}
        }
        if r.get("year").is_none()
            && let Some(y) = year_in(body)
        {
            r.set("year", y);
        }
    }
    Some(r)
}

// ---------------------------------------------------------------
// citations in prose
// ---------------------------------------------------------------

/// A citation found in prose: the entry it names (a list number,
/// or a responsible party with a year) and the pinpoint after
/// the comma, with its byte span in the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Citation {
    pub lo: usize,
    pub hi: usize,
    pub key: CitationKey,
    pub locator: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CitationKey {
    Number(String),
    AuthorYear { party: String, year: String },
}

/// The citations in `text`: bracketed numbers (`[12]`, `[12–14]`,
/// `[3, 7]`, `[12, с. 45]`, `[12, p. 45]`) and author-year
/// parentheses (`(Иванов, 2019)`, `(Smith 2019, 45)`, `(王, 2015)`,
/// with `и др.` / `et al.` / `等`).
pub fn citations(text: &str) -> Vec<Citation> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'[' || c == b'(' {
            let close = if c == b'[' { b']' } else { b')' };
            let Some(end) = b[i..].iter().position(|&x| x == close) else {
                i += 1;
                continue;
            };
            let inner = &text[i + 1..i + end];
            let found = if c == b'[' {
                bracket_citations(inner, i, i + end + 1)
            } else {
                author_year_citation(inner, i, i + end + 1)
                    .into_iter()
                    .collect()
            };
            out.extend(found);
            i += end + 1;
            continue;
        }
        i += 1;
    }
    out
}

fn bracket_citations(inner: &str, lo: usize, hi: usize) -> Vec<Citation> {
    let inner = inner.trim();
    if inner.is_empty() || !inner.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Vec::new();
    }
    // `12, с. 45` — a pinpoint after the comma; `3, 7` — two
    // entries; `12–14` — a range of entries.
    let mut out = Vec::new();
    let mut locator: Option<String> = None;
    let mut numbers: Vec<String> = Vec::new();
    for part in inner.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.chars().all(|c| c.is_ascii_digit()) {
            numbers.push(part.to_string());
        } else if let Some((a, b)) = part.split_once(['–', '-'])
            && a.chars().all(|c| c.is_ascii_digit())
            && b.chars().all(|c| c.is_ascii_digit())
            && let (Ok(x), Ok(y)) = (a.parse::<u32>(), b.parse::<u32>())
            && x <= y
            && y - x < 50
        {
            for n in x..=y {
                numbers.push(n.to_string());
            }
        } else if !numbers.is_empty() {
            locator = Some(part.to_string());
        } else {
            return Vec::new();
        }
    }
    for n in numbers {
        out.push(Citation {
            lo,
            hi,
            key: CitationKey::Number(n),
            locator: locator.clone(),
        });
    }
    out
}

fn author_year_citation(inner: &str, lo: usize, hi: usize) -> Option<Citation> {
    let inner = inner.trim();
    // The year: the first four-digit token; before it the party,
    // after it (past a comma) the pinpoint.
    let year_at = inner
        .char_indices()
        .find(|(i, c)| {
            c.is_ascii_digit()
                && inner[*i..].len() >= 4
                && inner[*i..*i + 4].chars().all(|c| c.is_ascii_digit())
        })?
        .0;
    let year = &inner[year_at..year_at + 4];
    if year_in(year).is_none() {
        return None;
    }
    let party = inner[..year_at].trim().trim_end_matches([',', ' ']).trim();
    if party.is_empty() || party.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let party = party
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic())
        .then_some(party)?;
    let after = inner[year_at + 4..].trim();
    let locator = after
        .strip_prefix(',')
        .or_else(|| after.strip_prefix(':'))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty());
    Some(Citation {
        lo,
        hi,
        key: CitationKey::AuthorYear {
            party: party.to_string(),
            year: year.to_string(),
        },
        locator,
    })
}

/// Match an author-year citation against the entries: the first
/// whose responsible party begins with the party's surname (the
/// `и др.` / `et al.` / `等` dropped) and whose year agrees.
pub fn resolve_author_year<'a>(
    party: &str,
    year: &str,
    entries: &'a [(String, Option<String>, Option<String>)],
) -> Option<&'a str> {
    let surname = party
        .split([',', ' '])
        .next()
        .unwrap_or(party)
        .trim_end_matches("等")
        .to_lowercase();
    entries
        .iter()
        .find(|(_, author, y)| {
            y.as_deref() == Some(year)
                && author
                    .as_deref()
                    .is_some_and(|a| a.to_lowercase().starts_with(&surname))
        })
        .map(|(onym, _, _)| onym.as_str())
}

/// The field census for the record a function answers: an alias
/// map so `:author` and `:auctor` meet.
pub fn alias_map() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    for k in [
        "author",
        "title",
        "year",
        "journal",
        "booktitle",
        "series",
        "volume",
        "number",
        "pages",
        "publisher",
        "address",
        "edition",
        "institution",
        "medium",
        "url",
        "doi",
        "isbn",
        "urldate",
        "note",
    ] {
        m.insert(campus(k), k);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(standard: &str, text: &str) -> Vec<(&'static str, String)> {
        parse(standard, text).unwrap().expect("a reference").fields
    }

    fn get<'a>(f: &'a [(&'static str, String)], k: &str) -> Option<&'a str> {
        f.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn gost_book_and_article() {
        let book = "12. Гиббон, Э. История упадка и разрушения Римской империи : в 7 т. / Э. Гиббон ; пер. с англ. В. Н. Неведомского. – Москва : Терра, 1997. – Т. 1. – 624 с. – ISBN 5-300-00993-7. – Текст : непосредственный.";
        let r = parse("gost", book).unwrap().unwrap();
        assert_eq!(r.label.as_deref(), Some("12"));
        assert_eq!(r.genus, Some("book"));
        let f = &r.fields;
        assert_eq!(get(f, "author"), Some("Гиббон, Э."));
        let r71 = parse(
            "gost",
            "Пушкин А. С. Капитанская дочка. М.: Наука, 1964. 285 с.",
        )
        .unwrap()
        .unwrap();
        assert_eq!(r71.get("author"), Some("Пушкин А. С."));
        assert_eq!(r71.get("title"), Some("Капитанская дочка"));
        assert_eq!(r71.get("address"), Some("М."));
        assert_eq!(r71.get("publisher"), Some("Наука"));
        assert_eq!(r71.get("year"), Some("1964"));
        assert_eq!(r71.get("pages"), Some("285"));
        let two = parse("gost", "Томашевский, Б. В. Писатель и книга : очерк текстологии / Б. В. Томашевский. – Москва : Искусство, 1959. – 280 с.").unwrap().unwrap();
        assert_eq!(two.get("author"), Some("Томашевский, Б. В."));
        assert_eq!(
            two.get("title"),
            Some("Писатель и книга : очерк текстологии")
        );
        assert_eq!(
            get(f, "title"),
            Some("История упадка и разрушения Римской империи : в 7 т.")
        );
        assert_eq!(get(f, "address"), Some("Москва"));
        assert_eq!(get(f, "publisher"), Some("Терра"));
        assert_eq!(get(f, "year"), Some("1997"));
        assert_eq!(get(f, "volume"), Some("1"));
        assert_eq!(get(f, "pages"), Some("624"));
        assert_eq!(get(f, "isbn"), Some("5-300-00993-7"));
        assert_eq!(get(f, "medium"), Some("непосредственный"));
        let article = "Иванов, И. И. Геродот и его источники / И. И. Иванов // Вестник древней истории. – 2019. – Т. 79, № 3. – С. 45–52. – DOI 10.1000/vdi.2019.3.45. – URL: https://example.org/vdi/45 (дата обращения: 12.03.2020).";
        let r = parse("gost", article).unwrap().unwrap();
        assert_eq!(r.genus, Some("article"));
        let f = &r.fields;
        assert_eq!(get(f, "journal"), Some("Вестник древней истории"));
        assert_eq!(get(f, "year"), Some("2019"));
        assert_eq!(get(f, "volume"), Some("79"));
        assert_eq!(get(f, "number"), Some("3"));
        assert_eq!(get(f, "pages"), Some("45–52"));
        assert_eq!(get(f, "doi"), Some("10.1000/vdi.2019.3.45"));
        assert_eq!(get(f, "url"), Some("https://example.org/vdi/45"));
        assert_eq!(get(f, "urldate"), Some("12.03.2020"));
        assert!(
            parse("gost", "Просто фраза без структуры")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn gb7714_book_article_and_online() {
        let f = fields(
            "gb7714",
            "[1] 司马迁. 史记[M]. 北京: 中华书局, 1959: 100-120.",
        );
        assert_eq!(get(&f, "author"), Some("司马迁"));
        assert_eq!(get(&f, "title"), Some("史记"));
        assert_eq!(get(&f, "address"), Some("北京"));
        assert_eq!(get(&f, "publisher"), Some("中华书局"));
        assert_eq!(get(&f, "year"), Some("1959"));
        assert_eq!(get(&f, "pages"), Some("100–120"));
        let r = parse(
            "gb7714",
            "王明, 李华. 《论语》成书考[J]. 文史哲, 2015, 10(3): 12-15.",
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.genus, Some("article"));
        assert_eq!(r.get("journal"), Some("文史哲"));
        assert_eq!(r.get("year"), Some("2015"));
        assert_eq!(r.get("volume"), Some("10"));
        assert_eq!(r.get("number"), Some("3"));
        assert_eq!(r.get("pages"), Some("12–15"));
        let r = parse("gb7714", "[3] 中国国家图书馆. 古籍数字化[EB/OL]. (2015-01-02)[2016-03-04]. http://www.nlc.cn/gu.").unwrap().unwrap();
        assert_eq!(r.label.as_deref(), Some("3"));
        assert_eq!(r.genus, Some("online"));
        assert_eq!(r.get("medium"), Some("OL"));
        assert_eq!(r.get("year"), Some("2015"));
        assert_eq!(r.get("urldate"), Some("2016-03-04"));
        assert_eq!(r.get("url"), Some("http://www.nlc.cn/gu"));
        assert!(
            parse("gb7714", "一段没有类型标识的文字。")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cmos_book_article_and_chapter() {
        let r = parse("cmos", "Gibbon, Edward. The History of the Decline and Fall of the Roman Empire. Vol. 1. London: Strahan and Cadell, 1776.").unwrap().unwrap();
        assert_eq!(r.genus, Some("book"));
        assert_eq!(r.get("author"), Some("Gibbon, Edward"));
        assert_eq!(
            r.get("title"),
            Some("The History of the Decline and Fall of the Roman Empire")
        );
        assert_eq!(r.get("volume"), Some("1"));
        assert_eq!(r.get("address"), Some("London"));
        assert_eq!(r.get("publisher"), Some("Strahan and Cadell"));
        assert_eq!(r.get("year"), Some("1776"));
        let r = parse("cmos", "Austen, Jane. \"On the Reading of Novels.\" The Quarterly Review 12, no. 3 (1816): 45–67. https://doi.org/10.1000/qr.1816.")
            .unwrap()
            .unwrap();
        assert_eq!(r.genus, Some("article"));
        assert_eq!(r.get("title"), Some("On the Reading of Novels"));
        assert_eq!(r.get("journal"), Some("The Quarterly Review"));
        assert_eq!(r.get("volume"), Some("12"));
        assert_eq!(r.get("number"), Some("3"));
        assert_eq!(r.get("year"), Some("1816"));
        assert_eq!(r.get("pages"), Some("45–67"));
        assert_eq!(r.get("doi"), Some("10.1000/qr.1816"));
        let r = parse("cmos", "Johnson, Samuel. \"The Life of Milton.\" In The Lives of the Most Eminent English Poets, 101–159. London: Bathurst, 1781.").unwrap().unwrap();
        assert_eq!(r.genus, Some("incollection"));
        assert_eq!(
            r.get("booktitle"),
            Some("The Lives of the Most Eminent English Poets")
        );
        assert_eq!(r.get("pages"), Some("101–159"));
        // An AMS entry is no Chicago one: the initial opens it.
        assert!(parse("cmos", "G. Cantor, Über eine Eigenschaft des Inbegriffes aller reellen algebraischen Zahlen, J. Reine Angew. Math. 77 (1874), 258–262.").unwrap().is_none());
        let r = parse("cmos", "Herodotus. The Histories. Translated by A. D. Godley. Cambridge, MA: Harvard University Press, 1920.").unwrap().unwrap();
        assert_eq!(r.get("author"), Some("Herodotus"));
        assert_eq!(r.get("title"), Some("The Histories"));
        assert_eq!(r.get("publisher"), Some("Harvard University Press"));
        assert_eq!(r.get("year"), Some("1920"));
    }

    #[test]
    fn ams_book_and_article() {
        let r = parse("ams", "1. E. Gibbon, The history of the decline and fall of the Roman Empire, Strahan and Cadell, London, 1776.").unwrap().unwrap();
        assert_eq!(r.label.as_deref(), Some("1"));
        assert_eq!(r.genus, Some("book"));
        assert_eq!(r.get("author"), Some("E. Gibbon"));
        assert_eq!(
            r.get("title"),
            Some("The history of the decline and fall of the Roman Empire")
        );
        assert_eq!(r.get("publisher"), Some("Strahan and Cadell"));
        assert_eq!(r.get("address"), Some("London"));
        assert_eq!(r.get("year"), Some("1776"));
        let r = parse("ams", "G. Cantor and R. Dedekind, On a property of the collection of all real algebraic numbers, J. Reine Angew. Math. 77 (1874), no. 2, 258–262.").unwrap().unwrap();
        assert_eq!(r.genus, Some("article"));
        assert_eq!(r.get("author"), Some("G. Cantor and R. Dedekind"));
        assert_eq!(r.get("journal"), Some("J. Reine Angew. Math."));
        assert_eq!(r.get("volume"), Some("77"));
        assert_eq!(r.get("year"), Some("1874"));
        assert_eq!(r.get("number"), Some("2"));
        assert_eq!(r.get("pages"), Some("258–262"));
        // A Chicago entry is no AMS one: no initials open it.
        assert!(parse("ams", "Gibbon, Edward. The History of the Decline and Fall of the Roman Empire. Vol. 1. London: Strahan and Cadell, 1776.").unwrap().is_none());
    }

    #[test]
    fn unknown_standard_and_citations() {
        let e = parse("apa", "x").unwrap_err();
        assert!(e.contains("gost, gb7714, cmos, ams"), "{e}");
        let c = citations(
            "As shown [12] and [3, 7] and [12–14, с. 45]; see (Иванов, 2019) and (Smith et al. 2019, 45) and (王, 2015).",
        );
        let nums: Vec<String> = c
            .iter()
            .filter_map(|c| match &c.key {
                CitationKey::Number(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(nums, ["12", "3", "7", "12", "13", "14"]);
        assert_eq!(c[5].locator.as_deref(), Some("с. 45"));
        let ay: Vec<(String, String, Option<String>)> = c
            .iter()
            .filter_map(|c| match &c.key {
                CitationKey::AuthorYear { party, year } => {
                    Some((party.clone(), year.clone(), c.locator.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(ay.len(), 3, "{c:?}");
        assert_eq!(ay[0], ("Иванов".to_string(), "2019".to_string(), None));
        assert_eq!(
            ay[1],
            (
                "Smith et al.".to_string(),
                "2019".to_string(),
                Some("45".to_string())
            )
        );
        assert_eq!(ay[2], ("王".to_string(), "2015".to_string(), None));
        let entries = vec![
            (
                "1".to_string(),
                Some("Иванов, И. И.".to_string()),
                Some("2019".to_string()),
            ),
            (
                "2".to_string(),
                Some("Smith, John".to_string()),
                Some("2019".to_string()),
            ),
        ];
        assert_eq!(resolve_author_year("Иванов", "2019", &entries), Some("1"));
        assert_eq!(
            resolve_author_year("Smith et al.", "2019", &entries),
            Some("2")
        );
        assert_eq!(resolve_author_year("Smith", "2018", &entries), None);
    }
}
