//! `qua man`: the command inventory and the syntax index at the
//! command line — one command's page, every command, a glyph's page
//! (a pipe, a sigil, an operator), a pipe's page and its commands,
//! the glyphs, the Russian line; `--commands` lists the registry as
//! TSV.

use std::process::Command;

fn qua(args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        out.status.success(),
    )
}

#[test]
fn man_is_the_door_to_the_inventory() {
    let (page, ok) = qua(&["man", "upper"]);
    assert!(
        ok && page.starts_with("upper — the text in upper case\n"),
        "{page}"
    );
    assert!(page.contains("  on null   null\n"), "{page}");
    let (ru, ok) = qua(&["man", "--ru", "upper"]);
    assert!(ok && ru.contains("верхнем"), "{ru}");
    let (all, ok) = qua(&["man"]);
    assert!(
        ok && all.lines().count() > 150 && all.contains("\ntree "),
        "{all}"
    );
    let (aggs, ok) = qua(&["man", "@|"]);
    assert!(
        ok && aggs.contains("\ntree ") && !aggs.contains("\nupper "),
        "{aggs}"
    );
    let (missing, ok) = qua(&["man", "uper"]);
    assert!(!ok && missing.contains("no command 'uper'"), "{missing}");
    let (tsv, ok) = qua(&["--commands"]);
    assert!(
        ok && tsv.starts_with("name\tkind\tpipes\tlayer\tonnull\tdoc\n"),
        "{tsv}"
    );
    assert!(tsv.contains("\ntree\tcontext\t| @|\tnode\t"), "{tsv}");
}

/// The symbolic syntax has its pages (the user, 2026-11-12: "qua man
/// should also explain the symbolic syntax: pipes, sigils,
/// operators"), from the language repository's syntax index.
#[test]
fn man_explains_the_symbolic_syntax() {
    // an operator, a sigil, a projection: the name, the spec section,
    // the reference page
    let (page, ok) = qua(&["man", "<=>"]);
    assert!(ok && page.starts_with("<=> — correlation\n"), "{page}");
    assert!(
        page.contains("  spec      sec:correlation-operator\n"),
        "{page}"
    );
    assert!(
        page.contains("  reference https://quarb.org/reference/correlation.html"),
        "{page}"
    );
    // what it does, one sentence, and its kind
    assert!(
        page.contains("\n  what      Joins each driving node")
            && page.contains("\n  kind      operator\n"),
        "{page}"
    );
    for (glyph, name) in [
        ("::", "property projection"),
        ("$_", "topic"),
        ("=>", "value match"),
    ] {
        let (page, ok) = qua(&["man", glyph]);
        assert!(
            ok && page.starts_with(&format!("{glyph} — {name}\n")),
            "{glyph}: {page}"
        );
    }
    // a glyph of two readings: both pages
    let (page, ok) = qua(&["man", ">"]);
    assert!(
        ok && page.contains("> — next sibling\n") && page.contains("> — greater than\n"),
        "{page}"
    );
    // a glyph with variable parts: by its own marks, and as written
    let (page, ok) = qua(&["man", ".."]);
    assert!(ok && page.starts_with("⟨a⟩..⟨b⟩ — range\n"), "{page}");
    let (page, ok) = qua(&["man", "1..5"]);
    assert!(ok && page.starts_with("⟨a⟩..⟨b⟩ — range\n"), "{page}");
    let (page, ok) = qua(&["man", "$.total"]);
    assert!(ok && page.starts_with("$.⟨name⟩ — named reg\n"), "{page}");
    assert!(!page.contains("match capture"), "{page}");
    let (page, ok) = qua(&["man", "$3"]);
    assert!(ok && page.contains("$⟨N⟩ — match capture\n"), "{page}");
    let (page, ok) = qua(&["man", "[::n > 3]"]);
    assert!(ok && page.starts_with("[␣] — predicate\n"), "{page}");
    let (page, ok) = qua(&["man", "@|{.k}"]);
    assert!(
        ok && page.starts_with("@|{.⟨name⟩} — fold by a regula\n"),
        "{page}"
    );
    // a second spelling names the one it stands for
    let (page, ok) = qua(&["man", "((.))"]);
    assert!(
        ok && page.starts_with("((.)) — rounded latest mark\n")
            && page.contains("  see       $$. — latest mark\n"),
        "{page}"
    );
    // and takes its sentence from it
    let (canonical, _) = qua(&["man", "$$."]);
    let what = |page: &str| {
        page.lines()
            .find(|l| l.starts_with("  what "))
            .map(str::to_string)
    };
    assert!(
        what(&page).is_some() && what(&page) == what(&canonical),
        "{page}"
    );
    // a statement of a model file: the section, no reference page
    let (page, ok) = qua(&["man", "node"]);
    assert!(
        ok && page.starts_with("node — node constructor\n") && !page.contains("reference"),
        "{page}"
    );
    // the reference entry's name
    let (page, ok) = qua(&["man", "aggregation-pipe"]);
    assert!(ok && page.starts_with("@| — aggregation pipe\n"), "{page}");
    // Russian: the name, and the section alone (the reference is
    // English)
    let (page, ok) = qua(&["man", "--ru", "<=>"]);
    assert!(
        ok && page.starts_with("<=> — корреляция\n") && !page.contains("reference"),
        "{page}"
    );
    assert!(page.contains("\n  what      Соединяет"), "{page}");
    // every glyph the door shows says what it does, in both languages
    for ru in [false, true] {
        let mut args = vec!["man", "syntax"];
        if ru {
            args.push("--ru");
        }
        let (all, ok) = qua(&args);
        assert!(ok, "{all}");
        for line in all.lines() {
            let glyph = line.split("  ").next().unwrap_or_default().trim();
            let mut args = vec!["man", glyph];
            if ru {
                args.push("--ru");
            }
            let (page, _) = qua(&args);
            assert!(
                page.contains("\n  what      "),
                "no sentence for {glyph} (ru: {ru}): {page}"
            );
        }
    }
}

/// A pipe is a glyph and a list: its page, then the commands that
/// stand after it. `syntax` lists the glyphs.
#[test]
fn a_pipe_is_its_page_and_its_commands() {
    let (page, ok) = qua(&["man", "@|"]);
    assert!(ok && page.starts_with("@| — aggregation pipe\n"), "{page}");
    assert!(
        page.contains("\ntree ") && !page.contains("\nupper "),
        "{page}"
    );
    let (page, ok) = qua(&["man", "|"]);
    assert!(
        ok && page.starts_with("| — pipe\n") && page.contains("\nupper "),
        "{page}"
    );
    let (all, ok) = qua(&["man", "syntax"]);
    assert!(ok && all.lines().count() > 250, "{all}");
    assert!(
        all.lines()
            .any(|l| l.starts_with("<=> ") && l.ends_with(" correlation")),
        "{all}"
    );
    // the stale rows the index keeps for the spec's own index are
    // not shown
    assert!(!all.contains("leaf anchor"), "{all}");
    // the glyphs of one kind, by the kind's name or its plural
    let (pipes, ok) = qua(&["man", "syntax", "pipe"]);
    assert!(ok, "{pipes}");
    assert!(
        pipes.lines().count() >= 3
            && pipes.lines().count() < 20
            && pipes.lines().all(|l| l.contains("  pipe  ")),
        "{pipes}"
    );
    assert!(pipes.lines().any(|l| l.starts_with("@| ")), "{pipes}");
    assert_eq!(qua(&["man", "syntax", "pipes"]).0, pipes);
    let (operators, ok) = qua(&["man", "syntax", "operators"]);
    assert!(
        ok && operators.lines().any(|l| l.starts_with("<=> ")),
        "{operators}"
    );
    let (sigils, ok) = qua(&["man", "syntax", "sigil"]);
    assert!(
        ok && sigils.lines().any(|l| l.starts_with("$_ ")),
        "{sigils}"
    );
    let (missing, ok) = qua(&["man", "syntax", "nosuch"]);
    assert!(
        !ok && missing.contains("no kind 'nosuch' of syntax; the kinds: ")
            && missing.contains("operator")
            && !missing.contains("retired"),
        "{missing}"
    );
}

/// A word neither table has: the commands near it, the glyphs it is
/// part of, or both doors named.
#[test]
fn a_word_neither_table_has() {
    let (missing, ok) = qua(&["man", "uper"]);
    assert!(!ok && missing.contains("no command 'uper'"), "{missing}");
    let (missing, ok) = qua(&["man", "qqqq"]);
    assert!(
        !ok && missing.contains("no command 'qqqq', and no glyph")
            && missing.contains("`man syntax` the glyphs"),
        "{missing}"
    );
    // marks that are no glyph alone: the glyphs they are part of
    let (missing, ok) = qua(&["man", "=>*"]);
    assert!(
        !ok && missing.contains("no glyph '=>*' alone; it is part of:") && missing.contains("<=>*"),
        "{missing}"
    );
    let (missing, ok) = qua(&["man", "%%%"]);
    assert!(
        !ok && missing.contains("no command '%%%', and no glyph"),
        "{missing}"
    );
    let (missing, ok) = qua(&["man", "sort", "top"]);
    assert!(
        !ok && missing.contains("man takes one name, one glyph, or the word syntax"),
        "{missing}"
    );
    // the help names the door
    let (help, ok) = qua(&["--help"]);
    assert!(
        ok && help.contains("The manual: `man` as the first argument"),
        "{help}"
    );
}
