//! `qua man`: the command inventory and the syntax index as the
//! manual reads them. The text is `langspec/commands.tsv` and
//! `langspec/syntax-index.tsv` of the language repository, vendored
//! here (`scripts/vendor-commands.sh`, once per train) so the door
//! works offline and never drifts from the spec; the documentation
//! gate diffs the vendored copies against the language's. The same
//! text answers `quai :man NAME`, the Jupyter kernel's `%docs NAME`
//! and `qc man`.

const INVENTORY: &str = include_str!("commands.tsv");

/// The syntax index: one row per glyph of the language, the pipes,
/// sigils, operators and brackets a command name does not reach.
const SYNTAX: &str = include_str!("syntax-index.tsv");

/// The parsed signatures of a built-in, one per form of its cell
/// (spec: Signatures, draft 203); `None` for a library command and
/// for the syntax rows that are no call.
pub fn signatures(name: &str) -> Option<&'static [crate::syntax::signature::Signature]> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static SIGS: OnceLock<HashMap<String, Vec<crate::syntax::signature::Signature>>> =
        OnceLock::new();
    SIGS.get_or_init(|| {
        entries()
            .into_iter()
            .filter(|e| e.library.is_empty())
            .filter_map(|e| {
                crate::syntax::signature::parse_cell(&e.synopsis)
                    .ok()
                    .map(|s| (e.name, s))
            })
            .collect()
    })
    .get(name)
    .map(Vec::as_slice)
}

/// The options a built-in declares, under the notation: every form's,
/// each once.
pub fn options_of(name: &str) -> Option<Vec<&'static crate::syntax::signature::OptSpec>> {
    let forms = signatures(name)?;
    let mut out: Vec<&'static crate::syntax::signature::OptSpec> = Vec::new();
    for o in forms.iter().flat_map(|f| f.options.iter()) {
        if !out.iter().any(|seen| {
            (seen.long.is_some() && seen.long == o.long)
                || (seen.short.is_some() && seen.short == o.short)
        }) {
            out.push(o);
        }
    }
    Some(out)
}

/// Whether the inventory's row for the command as written (`edges`,
/// `graph:closeness`) declares the long option `long` (`--harmonic`),
/// in any of its forms. A library's rows are read too, which
/// [`signatures`] leaves out: the parser asks this to tell a graph
/// command's option from an undirected hop written first.
pub fn declares_long(written: &str, long: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static LONGS: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    LONGS
        .get_or_init(|| {
            let mut out: HashMap<String, Vec<String>> = HashMap::new();
            for e in entries() {
                let Ok(forms) = crate::syntax::signature::parse_cell(&e.synopsis) else {
                    continue;
                };
                out.entry(e.written()).or_default().extend(
                    forms
                        .iter()
                        .flat_map(|f| f.options.iter())
                        .filter_map(|o| o.long.clone()),
                );
            }
            out
        })
        .get(written)
        .is_some_and(|longs| longs.iter().any(|l| l == long))
}

/// Whether the built-in's rows list `--of`, under the notation.
pub fn takes_of_by_signature(name: &str) -> Option<bool> {
    options_of(name).map(|opts| opts.iter().any(|o| o.long.as_deref() == Some("--of")))
}

/// One row of the inventory.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub library: String,
    pub pipes: String,
    pub synopsis: String,
    pub kind: String,
    pub typed: String,
    pub onnull: String,
    pub en: String,
    pub ru: String,
    pub note_en: String,
    pub note_ru: String,
    pub anchor: String,
}

impl Entry {
    /// The name as written in a query: `pos` of the `morph` library
    /// is `morph:pos`.
    pub fn written(&self) -> String {
        if self.library.is_empty() {
            self.name.clone()
        } else {
            format!("{}:{}", self.library, self.name)
        }
    }
}

/// The inventory's rows, in the file's order (comments skipped).
pub fn entries() -> Vec<Entry> {
    let mut header: Option<Vec<&str>> = None;
    let mut out = Vec::new();
    for line in INVENTORY.lines() {
        if line.trim().is_empty() || line.starts_with("# ") {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        let Some(h) = &header else {
            header = Some(cells);
            continue;
        };
        let col = |name: &str| -> String {
            h.iter()
                .position(|c| *c == name)
                .and_then(|i| cells.get(i))
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        out.push(Entry {
            name: col("name"),
            library: col("library"),
            pipes: col("pipes"),
            synopsis: col("synopsis"),
            kind: col("kind"),
            typed: col("typed"),
            onnull: col("onnull"),
            en: col("en"),
            ru: col("ru"),
            note_en: col("note_en"),
            note_ru: col("note_ru"),
            anchor: col("anchor"),
        });
    }
    out
}

/// The entry of a command, as written (`upper`, `morph:pos`).
pub fn entry(name: &str) -> Option<Entry> {
    entries().into_iter().find(|e| e.written() == name)
}

/// Whether the inventory lists the command as a scalar one (the
/// `kind` column): the commands that broadcast over a structure's
/// items (spec: Scalar commands, draft 166). Read once.
pub fn is_scalar_kind(name: &str) -> bool {
    static SCALARS: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    SCALARS
        .get_or_init(|| {
            entries()
                .into_iter()
                .filter(|e| e.kind == "scalar")
                .map(|e| e.written())
                .collect()
        })
        .contains(name)
}

/// The manual page of one command: its line, the call forms, the
/// pipes, the typed signature, the result on a null topic, the
/// note and the spec section.
pub fn page(e: &Entry, ru: bool) -> String {
    let (line, note) = if ru {
        (&e.ru, &e.note_ru)
    } else {
        (&e.en, &e.note_en)
    };
    let mut s = format!("{} — {}\n", e.written(), line);
    let row = |s: &mut String, label: &str, text: &str| {
        if !text.is_empty() {
            s.push_str(&format!("  {label:<9} {text}\n"));
        }
    };
    row(&mut s, "synopsis", &e.synopsis);
    row(&mut s, "pipes", &e.pipes);
    row(&mut s, "type", &e.typed);
    row(&mut s, "on null", &e.onnull);
    row(&mut s, "note", note);
    row(&mut s, "spec", &e.anchor);
    s.trim_end().to_string()
}

/// Every command, one per line with its one-liner; `pipe` keeps the
/// commands of one pipe (`|`, `@|`, `$|`).
pub fn list(pipe: Option<&str>, ru: bool) -> String {
    let rows: Vec<Entry> = entries()
        .into_iter()
        .filter(|e| pipe.is_none_or(|p| e.pipes.split_whitespace().any(|x| x == p)))
        .collect();
    let width = rows
        .iter()
        .map(|e| e.written().chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|e| {
            let name = e.written();
            let pad = " ".repeat(width - name.chars().count());
            format!("{name}{pad}  {}", if ru { &e.ru } else { &e.en })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One row of the syntax index: a glyph of the language, its name,
/// where the specification defines it and the page of the Language
/// Reference that treats it.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub id: String,
    /// The glyph as written, its variable parts in angle quotes
    /// (`⟨a⟩..⟨b⟩`) and an open body as `␣` (`[␣]`).
    pub glyph: String,
    pub name_en: String,
    pub name_ru: String,
    /// The spec label that defines it, or `see:ID` for a second
    /// spelling of the row `ID`.
    pub anchor: String,
    /// The Language Reference page; `model` for a statement of a
    /// model file (no page each: the spec section is its home), `-`
    /// for a row kept only for the spec's index (stale syntax).
    pub entry: String,
    /// What the glyph does, one sentence (the columns `does_en` and
    /// `does_ru`, where the index carries them).
    pub en: String,
    pub ru: String,
    /// The kind of the glyph (pipe, sigil, operator, …), where the
    /// index carries the column.
    pub kind: String,
}

impl Glyph {
    /// The glyph's own marks: the variable parts and the open body
    /// left out, `..` of `⟨a⟩..⟨b⟩`, `[]` of `[␣]`.
    pub fn marks(&self) -> String {
        let mut out = String::new();
        let mut held = false;
        for c in self.glyph.chars() {
            match c {
                '⟨' => held = true,
                '⟩' => held = false,
                '␣' | ' ' => {}
                c if !held => out.push(c),
                _ => {}
            }
        }
        out
    }

    /// Whether `written` is this glyph with its variable parts
    /// filled in: `1..5` for `⟨a⟩..⟨b⟩`, `$.total` for `$.⟨name⟩`,
    /// `[::n > 3]` for `[␣]`. A glyph that is all variable (`⟨name⟩`)
    /// is no pattern: any word would be it.
    pub fn is_written_as(&self, written: &str) -> bool {
        if self.marks().is_empty() {
            return false;
        }
        // the glyph as a sequence of literal runs and variable parts
        enum Part {
            Run(String),
            /// A variable part: one character at least; a count
            /// (`⟨N⟩`, `⟨n⟩`, `⟨k⟩`, `⟨m⟩`) is digits.
            Var {
                digits: bool,
            },
            /// An open body (`␣`): anything, nothing included.
            Body,
        }
        let mut parts: Vec<Part> = Vec::new();
        let mut name: Option<String> = None;
        for c in self.glyph.chars() {
            match (c, &mut name) {
                ('⟨', None) => name = Some(String::new()),
                ('⟩', Some(n)) => {
                    let digits = matches!(n.as_str(), "N" | "n" | "k" | "m");
                    parts.push(Part::Var { digits });
                    name = None;
                }
                (c, Some(n)) => n.push(c),
                ('␣', None) => parts.push(Part::Body),
                (c, None) => match parts.last_mut() {
                    Some(Part::Run(run)) => run.push(c),
                    _ => parts.push(Part::Run(c.to_string())),
                },
            }
        }
        if parts.iter().all(|p| matches!(p, Part::Run(_))) {
            return false;
        }
        // backtracking over the parts: the glyphs are short
        fn fits(parts: &[Part], text: &str) -> bool {
            match parts.split_first() {
                None => text.is_empty(),
                Some((Part::Run(run), rest)) => text
                    .strip_prefix(run.as_str())
                    .is_some_and(|t| fits(rest, t)),
                Some((Part::Body, rest)) => text
                    .char_indices()
                    .map(|(at, _)| at)
                    .chain([text.len()])
                    .any(|at| fits(rest, &text[at..])),
                Some((Part::Var { digits }, rest)) => {
                    let mut ends = text
                        .char_indices()
                        .map(|(at, _)| at)
                        .skip(1)
                        .chain([text.len()]);
                    !text.is_empty()
                        && ends.any(|at| {
                            (!*digits || text[..at].chars().all(|c| c.is_ascii_digit()))
                                && fits(rest, &text[at..])
                        })
                }
            }
        }
        fits(&parts, written)
    }
}

/// The rows of the syntax index, in the file's order. The header is
/// the file's first line, written as a comment.
pub fn glyphs() -> Vec<Glyph> {
    let mut lines = SYNTAX.lines();
    let header: Vec<&str> = lines
        .next()
        .unwrap_or_default()
        .trim_start_matches('#')
        .trim_start()
        .split('\t')
        .collect();
    let at = |name: &str| header.iter().position(|c| *c == name);
    let (id, glyph, name_en, name_ru, anchor, entry) = (
        at("id"),
        at("glyph"),
        at("name_en"),
        at("name_ru"),
        at("anchor"),
        at("entry"),
    );
    let (en, ru, kind) = (at("does_en"), at("does_ru"), at("kind"));
    lines
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|line| {
            let cells: Vec<&str> = line.split('\t').collect();
            let cell = |i: Option<usize>| -> String {
                i.and_then(|i| cells.get(i))
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            };
            Glyph {
                id: cell(id),
                glyph: cell(glyph),
                name_en: cell(name_en),
                name_ru: cell(name_ru),
                anchor: cell(anchor),
                entry: cell(entry),
                en: cell(en),
                ru: cell(ru),
                kind: cell(kind),
            }
        })
        // a row without an entry is stale syntax, kept for the
        // spec's index alone: the door does not show it
        .filter(|g| g.entry != "-")
        .collect()
}

/// The rows a word of syntax names, nearest reading first: the glyph
/// as the index writes it; its own marks (`..`, `[]`, `$.`); the
/// glyph with its variable parts filled in (`1..5`, `$.total`); the
/// name of its reference entry (`aggregation-pipe`, the stable name:
/// it is the page's address); the row's id. The first reading that
/// names any row is the answer.
pub fn glyphs_of(word: &str) -> Vec<Glyph> {
    let all = glyphs();
    let bare: String = word.chars().filter(|c| !c.is_whitespace()).collect();
    // a written form has something written in it: a word of marks
    // alone is a glyph or none (`=>*` is not the expression head `=`
    // with `>*` for its body)
    let filled = word
        .chars()
        .any(|c| c.is_alphanumeric() || c.is_whitespace());
    let readings: [&dyn Fn(&Glyph) -> bool; 5] = [
        &|g| g.glyph == word,
        &|g| !bare.is_empty() && g.marks() == bare,
        &|g| filled && g.is_written_as(word),
        &|g| g.entry == word && g.entry != "model",
        &|g| g.id == word,
    ];
    for (i, reading) in readings.into_iter().enumerate() {
        let mut rows: Vec<Glyph> = all.iter().filter(|g| reading(g)).cloned().collect();
        if rows.is_empty() {
            continue;
        }
        // a written form fits several glyphs (`$.total` is `$.⟨name⟩`,
        // and `$⟨N⟩` would take it whole were a count not digits):
        // the ones that say the most of it in their own marks
        if i == 2 {
            let most = rows
                .iter()
                .map(|g| g.marks().chars().count())
                .max()
                .unwrap_or(0);
            rows.retain(|g| g.marks().chars().count() == most);
        }
        return rows;
    }
    Vec::new()
}

/// The Language Reference's address of an entry. The reference is
/// English: a page under `--ru` names the spec section alone, as a
/// command's page does.
fn reference_url(entry: &str) -> String {
    format!("https://quarb.org/reference/{entry}.html")
}

/// The manual page of one glyph: its line, what it does where the
/// index says, the spelling it stands for (a second spelling), the
/// spec section and the reference page (a statement of a model file
/// has the section alone).
pub fn glyph_page(g: &Glyph, ru: bool) -> String {
    let all = glyphs();
    // a second spelling takes its section, and its sentence where it
    // has none of its own, from the row it stands for
    let canonical = g
        .anchor
        .strip_prefix("see:")
        .and_then(|id| all.iter().find(|c| c.id == id));
    let name = |g: &Glyph| {
        if ru {
            g.name_ru.clone()
        } else {
            g.name_en.clone()
        }
    };
    let what = |g: &Glyph| if ru { g.ru.clone() } else { g.en.clone() };
    let mut s = format!("{} — {}\n", g.glyph, name(g));
    let row = |s: &mut String, label: &str, text: &str| {
        if !text.is_empty() {
            s.push_str(&format!("  {label:<9} {text}\n"));
        }
    };
    let sentence = match what(g) {
        w if w.is_empty() => canonical.map(&what).unwrap_or_default(),
        w => w,
    };
    row(&mut s, "what", &sentence);
    if !g.kind.is_empty() {
        row(&mut s, "kind", &g.kind);
    }
    if let Some(c) = canonical {
        row(&mut s, "see", &format!("{} — {}", c.glyph, name(c)));
    }
    let anchor = canonical.map_or(g.anchor.as_str(), |c| c.anchor.as_str());
    if !anchor.starts_with("see:") {
        row(&mut s, "spec", anchor);
    }
    let entry = match g.entry.as_str() {
        "-" | "" => canonical.map_or("", |c| c.entry.as_str()),
        e => e,
    };
    if !ru && !matches!(entry, "" | "-" | "model") {
        row(&mut s, "reference", &reference_url(entry));
    }
    s.trim_end().to_string()
}

/// Glyphs one per line: the glyph, its kind where the index names
/// the kinds, its name (`man syntax`, `man syntax operator`).
pub fn glyph_list(rows: &[Glyph], ru: bool) -> String {
    let width = |of: &dyn Fn(&Glyph) -> usize| rows.iter().map(of).max().unwrap_or(0);
    let glyph = width(&|g| g.glyph.chars().count());
    let kind = width(&|g| g.kind.chars().count());
    rows.iter()
        .map(|g| {
            let pad = " ".repeat(glyph - g.glyph.chars().count());
            let name = if ru { &g.name_ru } else { &g.name_en };
            if kind == 0 {
                format!("{}{pad}  {name}", g.glyph)
            } else {
                let gap = " ".repeat(kind - g.kind.chars().count());
                format!("{}{pad}  {}{gap}  {name}", g.glyph, g.kind)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The kinds the index names, in the order it first names them.
pub fn kinds() -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for g in glyphs() {
        if !g.kind.is_empty() && !kinds.contains(&g.kind) {
            kinds.push(g.kind);
        }
    }
    kinds
}

/// `man [--ru] [NAME | GLYPH | syntax [KIND]]`: the door's arguments
/// to its text, or the message of a word neither table has.
///
/// A command's name is its page. A glyph of the language (a pipe, a
/// sigil, an operator, a bracket) is the page of every row that
/// writes it, as the index writes it (`<=>`, `::`), by its own marks
/// (`..`, `[]`), or with its variable parts filled in (`$.total`).
/// A pipe is both: its page, then the commands that stand after it.
/// `syntax` lists the glyphs as the bare `man` lists the commands,
/// and `syntax KIND` those of one kind (`syntax operator`, `syntax
/// sigils`).
pub fn man(args: &[&str]) -> Result<String, String> {
    let ru = args.iter().any(|a| *a == "--ru");
    let words: Vec<&str> = args.iter().copied().filter(|a| *a != "--ru").collect();
    let pages = |rows: &[Glyph]| -> String {
        rows.iter()
            .map(|g| glyph_page(g, ru))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    match words.as_slice() {
        [] => Ok(list(None, ru)),
        [pipe] if matches!(*pipe, "|" | "@|" | "$|") => {
            let rows = glyphs_of(pipe);
            let commands = list(Some(pipe), ru);
            Ok(if rows.is_empty() {
                commands
            } else {
                format!("{}\n\n{commands}", pages(&rows))
            })
        }
        [word] if *word == "syntax" && entry(word).is_none() => Ok(glyph_list(&glyphs(), ru)),
        [word, kind] if *word == "syntax" && entry(word).is_none() => {
            let kinds = kinds();
            // the kind as the index names it, or its plural
            let named = kinds.iter().find(|k| {
                k.as_str() == *kind
                    || format!("{k}s") == *kind
                    || k.strip_suffix('y')
                        .is_some_and(|stem| format!("{stem}ies") == *kind)
            });
            match named {
                Some(k) => {
                    let rows: Vec<Glyph> = glyphs().into_iter().filter(|g| g.kind == *k).collect();
                    Ok(glyph_list(&rows, ru))
                }
                None if kinds.is_empty() => Err("the syntax index names no kinds".into()),
                None => Err(format!(
                    "no kind '{kind}' of syntax; the kinds: {}",
                    kinds.join(", ")
                )),
            }
        }
        [name] => {
            let rows = glyphs_of(name);
            match (entry(name), rows.is_empty()) {
                // a reading that is a command and a glyph (`#`, `%`)
                (Some(e), false) => Ok(format!("{}\n\n{}", page(&e, ru), pages(&rows))),
                (Some(e), true) => Ok(page(&e, ru)),
                (None, false) => Ok(pages(&rows)),
                (None, true) => Err(unknown(name, ru)),
            }
        }
        _ => Err(
            "man takes one name, one glyph, or the word syntax: man upper, man '@|', \
             man '<=>', man syntax, man syntax operator, man --ru upper"
                .into(),
        ),
    }
}

/// A line of a session that asks for the manual: `man`, `man sort`,
/// `man @|`, `man --ru sort`, and the same behind the REPL's colon
/// (`:man sort`). `None` for any other line; a query never opens
/// with a bare word, so `man` at the head of a line is free. The
/// hosts that have no shell (the browser extension, the playground)
/// reach the manual this way and no other. Quotes around a glyph, a
/// shell's habit, are taken off: `man '<=>'`.
pub fn line(line: &str) -> Option<Result<String, String>> {
    let t = line.trim();
    let rest = t.strip_prefix(":man").or_else(|| t.strip_prefix("man"))?;
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let words: Vec<&str> = rest
        .split_whitespace()
        .map(|w| {
            ['\'', '"']
                .iter()
                .find_map(|q| {
                    w.strip_prefix(*q)
                        .and_then(|w| w.strip_suffix(*q))
                        .filter(|w| !w.is_empty())
                })
                .unwrap_or(w)
        })
        .collect();
    Some(man(&words))
}

/// The message of a word neither table has: the nearest command
/// names, and the glyphs that carry the word's marks.
fn unknown(name: &str, ru: bool) -> String {
    // a command the word is part of, or one (of three letters at
    // least: `s` is in every other word) that is part of it
    let near: Vec<String> = entries()
        .iter()
        .map(Entry::written)
        .filter(|n| {
            n.to_lowercase().contains(&name.to_lowercase())
                || (n.chars().count() >= 3 && name.to_lowercase().contains(&n.to_lowercase()))
        })
        .take(5)
        .collect();
    // a word of marks: the glyphs it is part of
    let symbolic = !name.chars().any(char::is_alphanumeric);
    let part_of: Vec<Glyph> = if symbolic {
        glyphs()
            .into_iter()
            .filter(|g| g.marks().contains(name))
            .take(12)
            .collect()
    } else {
        Vec::new()
    };
    if !part_of.is_empty() {
        return format!(
            "no glyph '{name}' alone; it is part of:\n{}",
            glyph_list(&part_of, ru)
        );
    }
    if near.is_empty() {
        format!(
            "no command '{name}', and no glyph (`man` alone lists the commands, `man syntax` \
             the glyphs)"
        )
    } else {
        format!("no command '{name}'; the nearest: {}", near.join(", "))
    }
}

/// The most operands a built-in command takes after a pipe, read
/// from its synopsis (spec: the adjacency rule — a surplus operand
/// is refused): the longest form's operand count, `None` where a
/// form is open-ended (`…`), where the inventory has no row, or
/// where the command belongs to a library (its own gate counts).
/// Options (`--name=`) are not operands.
pub fn operand_bound(name: &str) -> Option<usize> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static BOUNDS: OnceLock<HashMap<String, Option<usize>>> = OnceLock::new();
    BOUNDS
        .get_or_init(|| {
            entries()
                .into_iter()
                .filter(|e| e.library.is_empty())
                .map(|e| {
                    // The widest form's parenthesis is the bound (the
                    // braced slot is no operand); a `…` leaves it open,
                    // and a row that is no call has none.
                    let bound = signatures(&e.name).and_then(|forms| {
                        forms
                            .iter()
                            .map(crate::syntax::signature::Signature::max_operands)
                            .try_fold(0usize, |acc, m| m.map(|m| acc.max(m)))
                    });
                    (e.name.clone(), bound)
                })
                .collect()
        })
        .get(name)
        .copied()
        .flatten()
}

/// The synopsis of a built-in, as the inventory writes it.
pub fn synopsis(name: &str) -> Option<String> {
    entry(name).map(|e| e.synopsis)
}

/// The registry's own listing as TSV (`qua --commands`): name,
/// kind, pipes, layer, the result on a null topic, and the one
/// line, for the documentation gate to diff against the inventory.
pub fn registry_tsv() -> String {
    use crate::syntax::registry::{Agg, Layer, OnNull, SPECS};
    let mut s = String::from("name\tkind\tpipes\tlayer\tonnull\tdoc\n");
    for spec in SPECS {
        let kind = match (spec.scalar, spec.agg) {
            (true, None) => "scalar",
            (true, Some(Agg::Reducing)) => "both",
            // `number` is both: the numeric cast under a locale on
            // `|`, the outline command on `@|` (the inventory's word).
            (true, Some(Agg::Context)) if spec.name == "number" => "both",
            // `shift` is a context command with a value form (`| shift(1)`
            // on a list), not a renderer like csv or pretty
            (true, Some(Agg::Context)) if spec.name == "shift" => "context",
            // `tree` draws the context on `@|` and each thread's node
            // on `|` (ruling 2026-11-11): a context command still
            (true, Some(Agg::Context)) if spec.name == "tree" => "context",
            (true, Some(Agg::Context)) => "rendering",
            (false, Some(Agg::Reducing)) => "reducing",
            // `collapse` and the positional selections by key are the
            // inventory's thread commands (threads in, threads out),
            // its kind word for the family (lang c1e56732)
            (false, Some(Agg::Context)) if spec.name == "collapse" => "thread",
            (false, Some(Agg::Context)) => "context",
            (_, Some(Agg::Keyed)) => "thread",
            (_, Some(Agg::Association { .. })) => "assoc",
            (false, None) => "none",
        };
        let mut pipes = Vec::new();
        if spec.scalar {
            pipes.push("|");
        }
        if spec.agg.is_some() {
            pipes.push("@|");
        }
        // `session` heads a query and rides no pipe, as `now()` does
        // (the inventory's `expr` rows are no pipeline command); it is
        // registered for its options' sake and, like `now`, not listed.
        if spec.name == "session" {
            continue;
        }
        let layer = match spec.layer {
            Layer::Value => "value",
            Layer::Node => "node",
        };
        // The on-null cell in the inventory's own vocabulary (`no
        // thread`, `"null"`, `v`, `0`, `1`, `""`, `unchanged`), from
        // the vendored row; the registry's word where a command has
        // no row.
        let onnull = entry(spec.name).map(|e| e.onnull).unwrap_or_else(|| {
            match spec.null {
                OnNull::Null => "null",
                OnNull::Nothing => "no thread",
                OnNull::Keep => "",
            }
            .to_string()
        });
        s.push_str(&format!(
            "{}\t{kind}\t{}\t{layer}\t{onnull}\t{}\n",
            spec.name,
            pipes.join(" "),
            spec.doc
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inventory_and_the_registry_name_the_same_commands() {
        // every inventory row is a command the registry knows (the
        // call operand now() excepted), and every registered
        // command has its row
        let names: std::collections::BTreeSet<String> =
            entries().iter().map(Entry::written).collect();
        for e in entries() {
            // a library's row names the library's command (the
            // engine's own `graph:`; a linked library's rows are
            // the host's to check)
            if !e.library.is_empty() {
                if e.library == "graph" {
                    use crate::pipeline::library::CommandLibrary as _;
                    assert!(
                        crate::graph::library::GRAPH.synopsis(&e.name).is_some(),
                        "inventory row without a library command: {}",
                        e.written()
                    );
                }
                if e.library == "syntax" {
                    use crate::pipeline::library::CommandLibrary as _;
                    assert!(
                        crate::syntax::construct::SYNTAX.synopsis(&e.name).is_some(),
                        "inventory row without a library command: {}",
                        e.written()
                    );
                }
                continue;
            }
            assert!(
                e.written() == "now" || crate::syntax::registry::spec(&e.written()).is_some(),
                "inventory row without a command: {}",
                e.written()
            );
        }
        use crate::pipeline::library::CommandLibrary as _;
        for syn in crate::graph::library::GRAPH.commands() {
            assert!(
                names.contains(&format!("graph:{}", syn.name)),
                "library command without an inventory row: graph:{}",
                syn.name
            );
        }
        for syn in crate::syntax::construct::SYNTAX.commands() {
            assert!(
                names.contains(&format!("syntax:{}", syn.name)),
                "library command without an inventory row: syntax:{}",
                syn.name
            );
        }
        for spec in crate::syntax::registry::SPECS {
            assert!(
                spec.name == "#" || names.contains(spec.name),
                "command without an inventory row: {}",
                spec.name
            );
        }
    }

    #[test]
    fn every_built_in_row_writes_the_notation() {
        // Under the notation every built-in cell parses (the syntax
        // rows that are no call excepted), and a row that declares
        // a short letter or a long name twice is refused here, at
        // build, not at the first call.
        let mut bad = Vec::new();
        for e in entries().into_iter().filter(|e| e.library.is_empty()) {
            let opens_as_call = e.synopsis.starts_with(&format!("{}(", e.name))
                || e.synopsis.starts_with(&format!("{}{{", e.name));
            if !e.name.chars().all(|c| c.is_alphanumeric() || c == '_') || !opens_as_call {
                continue;
            }
            if let Err(why) = crate::syntax::signature::parse_cell(&e.synopsis) {
                bad.push(format!("{}: {why}", e.name));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn the_row_gives_the_operand_bound() {
        assert_eq!(operand_bound("round"), Some(1));
        assert_eq!(operand_bound("sum"), Some(0));
        assert_eq!(operand_bound("csv"), None);
        assert_eq!(operand_bound("morph:pos"), None);
        // a row that is no call has no bound
        assert_eq!(operand_bound("#"), None);
    }

    #[test]
    fn the_door_answers() {
        let p = man(&["upper"]).unwrap();
        assert!(p.starts_with("upper — the text in upper case\n"), "{p}");
        assert!(p.contains("  on null   null"), "{p}");
        assert!(p.contains("  spec      sec:value-commands"), "{p}");
        let ru = man(&["--ru", "upper"]).unwrap();
        assert!(ru.contains("верхнем"), "{ru}");
        let all = man(&[]).unwrap();
        assert!(all.lines().count() > 150, "{}", all.lines().count());
        let aggs = man(&["@|"]).unwrap();
        assert!(aggs.lines().any(|l| l.starts_with("tree ")), "{aggs}");
        assert!(!aggs.lines().any(|l| l.starts_with("upper ")), "{aggs}");
        let e = man(&["uper"]).unwrap_err();
        assert!(e.contains("no command 'uper'"), "{e}");
        assert!(
            registry_tsv()
                .lines()
                .any(|l| l.starts_with("tree\tcontext\t| @|\tnode\t"))
        );
    }
}
