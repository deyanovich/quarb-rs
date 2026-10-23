//! `qua man`: the command inventory as the manual reads it. The
//! text is `langspec/commands.tsv` of the language repository,
//! vendored here (`scripts/vendor-commands.sh`, once per train) so
//! the door works offline and never drifts from the spec; the
//! documentation gate diffs the vendored copy against the
//! language's. The same text answers `quai :man NAME`, the Jupyter
//! kernel's `%docs NAME` and `qc man`.

const INVENTORY: &str = include_str!("commands.tsv");

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

/// `man [--ru] [NAME | '|' | @| | $|]`: the door's arguments to its
/// text, or the message of a name the inventory lacks.
pub fn man(args: &[&str]) -> Result<String, String> {
    let ru = args.iter().any(|a| *a == "--ru");
    let words: Vec<&str> = args.iter().copied().filter(|a| *a != "--ru").collect();
    match words.as_slice() {
        [] => Ok(list(None, ru)),
        [pipe] if matches!(*pipe, "|" | "@|" | "$|") => Ok(list(Some(pipe), ru)),
        [name] => entry(name).map(|e| page(&e, ru)).ok_or_else(|| {
            let near: Vec<String> = entries()
                .iter()
                .map(Entry::written)
                .filter(|n| {
                    n.to_lowercase().contains(&name.to_lowercase())
                        || name.to_lowercase().contains(&n.to_lowercase())
                })
                .take(5)
                .collect();
            if near.is_empty() {
                format!("no command '{name}' (`man` alone lists them)")
            } else {
                format!("no command '{name}'; the nearest: {}", near.join(", "))
            }
        }),
        _ => Err("man takes one name, or one pipe: man upper, man @|, man --ru upper".into()),
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
                .map(|e| (e.name.clone(), synopsis_bound(&e.synopsis)))
                .collect()
        })
        .get(name)
        .copied()
        .flatten()
}

/// The operand bound of one synopsis cell (`round / round(d)` → 1,
/// `group(key; key…)` → none).
fn synopsis_bound(synopsis: &str) -> Option<usize> {
    let mut max = 0;
    for form in synopsis.split(" / ") {
        let form = form
            .trim()
            .trim_start_matches("@| ")
            .trim_start_matches("| ");
        if !form.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Some(open) = form.find('(') else {
            continue;
        };
        let inner = form[open + 1..].trim_end_matches(')');
        if inner.contains('…') {
            return None;
        }
        let n = inner
            .split(';')
            .map(str::trim)
            .filter(|a| !a.is_empty() && !a.starts_with("--"))
            .count();
        max = max.max(n);
    }
    Some(max)
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
            (true, Some(Agg::Context)) => "rendering",
            (false, Some(Agg::Reducing)) => "reducing",
            (false, Some(Agg::Context)) => "context",
            (_, Some(Agg::Keyed)) => "keyed",
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
        let layer = match spec.layer {
            Layer::Value => "value",
            Layer::Node => "node",
        };
        // The on-null cell in the inventory's own vocabulary (`no
        // thread`, `"null"`, `v`, `0`, `1`, `""`, `unchanged`), from
        // the vendored row; the registry's word where a command has
        // no row.
        let onnull = entry(spec.name)
            .map(|e| e.onnull)
            .unwrap_or_else(|| {
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
            assert!(
                e.written() == "now" || crate::syntax::registry::spec(&e.written()).is_some(),
                "inventory row without a command: {}",
                e.written()
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
    fn the_synopsis_gives_the_operand_bound() {
        assert_eq!(synopsis_bound("upper"), Some(0));
        assert_eq!(synopsis_bound("round / round(d)"), Some(1));
        assert_eq!(synopsis_bound("log / log(b) / log(x; b)"), Some(2));
        assert_eq!(synopsis_bound("group(key) / group(key; key…)"), None);
        assert_eq!(synopsis_bound("melt(id; id…) / melt(id; --name=; --value=)"), None);
        assert_eq!(synopsis_bound("| number(LOCALE) / @| number / @| number(level)"), Some(1));
        assert_eq!(synopsis_bound("sh(cmd) / `cmd` / (;cmd)"), Some(1));
        assert_eq!(synopsis_bound("s/pattern/replacement/mods"), Some(0));
        assert_eq!(operand_bound("round"), Some(1));
        assert_eq!(operand_bound("sum"), Some(0));
        assert_eq!(operand_bound("csv"), None);
        assert_eq!(operand_bound("morph:pos"), None);
        assert_eq!(operand_bound("#"), Some(0));
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
                .any(|l| l.starts_with("tree\tcontext\t@|\tnode\t"))
        );
    }
}
