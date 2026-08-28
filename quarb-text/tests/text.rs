//! End-to-end tests: queries run through the engine against
//! text-level documents assembled from producer event streams.

use quarb_text::{Block, Container, TextModel};

fn values(model: &TextModel, query: &str) -> Vec<String> {
    match quarb::run(query, model).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn nodes(model: &TextModel, query: &str) -> Vec<String> {
    let mut got: Vec<String> = match quarb::run(query, model).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| model.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

fn outline() -> TextModel {
    TextModel::build(vec![
        Block::Paragraph {
            text: "Preamble.".into(),
        },
        Block::Heading {
            level: 1,
            lemma: "One".into(),
        },
        Block::Paragraph {
            text: "In one.".into(),
        },
        Block::Heading {
            level: 3,
            lemma: "Deep".into(),
        },
        Block::Paragraph {
            text: "In deep.".into(),
        },
        Block::Heading {
            level: 2,
            lemma: "Mid".into(),
        },
        Block::Paragraph {
            text: "In mid.".into(),
        },
        Block::Heading {
            level: 1,
            lemma: "Two".into(),
        },
        Block::Paragraph {
            text: "In two.".into(),
        },
    ])
}

/// The outline rule: a heading closes every open section at its
/// level or deeper, a skipped level (h1 → h3) nests directly, and
/// pre-heading content belongs to the document root.
#[test]
fn sections_derive_from_flat_headings() {
    let m = outline();
    assert_eq!(values(&m, "/section::lemma"), vec!["One", "Two"]);
    // The skipped-level h3 and the following h2 are siblings under
    // the h1: 2 < 3 closes "Deep", 2 > 1 keeps "One" open.
    assert_eq!(
        nodes(&m, "/section/section"),
        vec!["/section[1]/section[1]", "/section[1]/section[2]"]
    );
    assert_eq!(values(&m, "/section/section::lemma"), vec!["Deep", "Mid"]);
    assert_eq!(values(&m, "/section/section::::level"), vec!["3", "2"]);
    // Pre-heading content is root body, not a section's.
    assert_eq!(values(&m, "/paragraph::"), vec!["Preamble."]);
}

/// Bare `::` is the flattened prose of the subtree, lemma first.
#[test]
fn prose_flattens_with_lemma() {
    let m = outline();
    assert_eq!(
        values(&m, "/section[::lemma = 'Two']::"),
        vec!["Two\nIn two."]
    );
}

/// A heading inside an open container is decorative, not
/// sectioning: it lowers to a paragraph.
#[test]
fn heading_inside_container_lowers_to_paragraph() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Heading {
            level: 2,
            lemma: "Decorative".into(),
        },
        Block::Paragraph {
            text: "Quoted.".into(),
        },
        Block::Close { hypograph: None },
    ]);
    assert_eq!(nodes(&m, "//section"), Vec::<String>::new());
    assert_eq!(
        values(&m, "/blockquote/paragraph::"),
        vec!["Decorative", "Quoted."]
    );
}

/// A blockquote's hypograph is its attribution — a property, and
/// the tail of the flattened prose.
#[test]
fn blockquote_hypograph() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Paragraph {
            text: "Quoted wisdom.".into(),
        },
        Block::Close {
            hypograph: Some("— Sage".into()),
        },
    ]);
    assert_eq!(values(&m, "/blockquote::hypograph"), vec!["— Sage"]);
    assert_eq!(values(&m, "/blockquote::"), vec!["Quoted wisdom.\n— Sage"]);
}

/// Items take their flavor from the enclosing list; ordered items
/// carry taxis from the list's start; a tight item's inline text is
/// its own, a nested list is its child.
#[test]
fn lists_and_items() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::UnorderedList,
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "alpha".into(),
        },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "beta".into(),
        },
        Block::Open {
            kind: Container::UnorderedList,
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "beta-child".into(),
        },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::OrderedList { start: 3 },
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "third".into(),
        },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "fourth".into(),
        },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
    ]);
    assert_eq!(
        values(&m, "/unordered-list/unordered-item::"),
        vec!["alpha", "beta\nbeta-child"]
    );
    assert_eq!(
        nodes(&m, "//unordered-item/unordered-list/unordered-item"),
        vec!["/unordered-list/unordered-item[2]/unordered-list/unordered-item"]
    );
    assert_eq!(values(&m, "//ordered-item::taxis"), vec!["3", "4"]);
}

/// Table denormalization: an ordered list with the `<table>` trait,
/// caption as lemma, rows as ordered items (taxis = row number),
/// cells as `Header: value` unordered items, empty cells skipped.
#[test]
fn tables_denormalize_to_nested_lists() {
    let m = TextModel::build(vec![Block::Table {
        lemma: Some("Crew".into()),
        headers: Some(vec!["Name".into(), "Role".into()]),
        rows: vec![
            vec!["Alice".into(), "captain".into()],
            vec!["Bob".into(), "".into()],
        ],
    }]);
    assert_eq!(nodes(&m, "//*<table>"), vec!["/ordered-list"]);
    assert_eq!(values(&m, "/ordered-list::lemma"), vec!["Crew"]);
    assert_eq!(values(&m, "//ordered-item::taxis"), vec!["1", "2"]);
    assert_eq!(
        values(&m, "//unordered-item::"),
        vec!["Name: Alice", "Role: captain", "Name: Bob"]
    );
}

/// A headerless table keeps bare cell text.
#[test]
fn headerless_table_keeps_bare_cells() {
    let m = TextModel::build(vec![Block::Table {
        lemma: None,
        headers: None,
        rows: vec![vec!["Alice".into(), "captain".into()]],
    }]);
    assert_eq!(
        values(&m, "//unordered-item::"),
        vec!["Alice", "captain"]
    );
}

/// Plain text: blank-line-separated paragraphs, each collapsed to
/// one line.
#[test]
fn plain_text_paragraphs() {
    let m = TextModel::parse_plain("One line\nsame paragraph.\n\n   \nSecond paragraph.\n");
    assert_eq!(
        values(&m, "/paragraph::"),
        vec!["One line same paragraph.", "Second paragraph."]
    );
    assert_eq!(nodes(&m, "//section"), Vec::<String>::new());
}

/// Ruling #25: the column name is the cell's `::lemma` — property
/// projection, not folded text — and rows/cells carry traits.
#[test]
fn cells_are_addressable_by_lemma() {
    let m = TextModel::build(vec![Block::Table {
        lemma: Some("Crew".into()),
        headers: Some(vec!["Name".into(), "Role".into()]),
        rows: vec![
            vec!["Alice".into(), "captain".into()],
            vec!["Bob".into(), "cook".into()],
        ],
    }]);
    // Address a cell by its column name, no regex in sight.
    assert_eq!(
        values(&m, "//*<cell>[::lemma = \"Role\"]::"),
        vec!["Role: captain", "Role: cook"]
    );
    // Scope by row, read by column.
    assert_eq!(
        values(&m, "//*<row>[::taxis = 2]/*/*[::lemma = \"Name\"]::"),
        vec!["Name: Bob"]
    );
    // The flattened prose still reads `lemma: value` byte for byte.
    assert_eq!(
        values(&m, "//*<cell>[::lemma = \"Name\"]::lemma"),
        vec!["Name", "Name"]
    );
}

/// A per-cell label (a row's `th`, the infobox dialect) wins over
/// positional headers and lands as the lemma.
#[test]
fn row_labels_become_cell_lemmas() {
    let m = TextModel::build(vec![Block::Table {
        lemma: None,
        headers: None,
        rows: vec![vec![
            quarb_text::Cell {
                label: Some("Location".into()),
                text: "Campion".into(),
            },
        ]],
    }]);
    assert_eq!(values(&m, "//*<cell>::lemma"), vec!["Location"]);
    assert_eq!(values(&m, "//*<cell>::"), vec!["Location: Campion"]);
}

/// An item's lemma is inline: `lemma: prose` — the flatten rule
/// that makes a lemma'd list read as definitions.
#[test]
fn item_lemma_flattens_inline() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::UnorderedList,
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: Some("emu".into()),
        },
        Block::Paragraph {
            text: "a large flightless bird".into(),
        },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
    ]);
    assert_eq!(
        values(&m, "//unordered-item::"),
        vec!["emu: a large flightless bird"]
    );
    assert_eq!(values(&m, "//unordered-item::lemma"), vec!["emu"]);
}

/// The serialization stages: `| markdown` / `| html` / `| atrep`
/// render a node's subtree through the koine renderer — the
/// export button and the pipe are the same verb.
#[test]
fn serialization_stages_render_subtrees() {
    let m = TextModel::build(vec![
        Block::Heading {
            level: 2,
            lemma: "The \"war\"".into(),
        },
        Block::Paragraph {
            text: "Machine guns were requested.".into(),
        },
        Block::Heading {
            level: 3,
            lemma: "First attempt".into(),
        },
        Block::Paragraph {
            text: "The birds split into small groups.".into(),
        },
    ]);
    assert_eq!(
        values(&m, r#"//section[::lemma =~ /war/] | markdown"#),
        vec![
            "## The \"war\"\n\nMachine guns were requested.\n\n### First attempt\n\nThe birds split into small groups."
        ]
    );
    assert_eq!(
        values(&m, r#"//section[::lemma = "First attempt"] | html"#),
        vec!["<h3>First attempt</h3>\n\n<p>The birds split into small groups.</p>"]
    );
    // litogramma: dialektos declaration, relative section depth,
    // explicit close markers.
    assert_eq!(
        values(&m, r#"//section[::lemma = "First attempt"] | atrep"#),
        vec![
            "@@@!litogramma\n\n@# First attempt\n\nThe birds split into small groups.\n\n#@"
        ]
    );
}

/// litogramma forms: the epigraph quote, the definition list for
/// lemma'd items, verbatim with a language genos.
#[test]
fn atrep_emits_litogramma_forms() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Paragraph {
            text: "Invulnerable as tanks.".into(),
        },
        Block::Close {
            hypograph: Some("Major Meredith".into()),
        },
        Block::Table {
            lemma: None,
            headers: None,
            rows: vec![vec![
                quarb_text::Cell {
                    label: Some("Date".into()),
                    text: "2 November 1932".into(),
                },
                quarb_text::Cell {
                    label: Some("Outcome".into()),
                    text: "Minimal impact".into(),
                },
            ]],
        },
        Block::Verbatim {
            lang: Some("rust".into()),
            text: "fn main() {}".into(),
        },
    ]);
    let out = &values(&m, "^ | atrep")[0];
    assert!(out.starts_with("@@@!litogramma\n"), "{out}");
    assert!(
        out.contains("@\"/\nInvulnerable as tanks.\n/\"@ Major Meredith"),
        "{out}"
    );
    assert!(
        out.contains("@:: Date\n@;\n2 November 1932\n;@\n::@"),
        "{out}"
    );
    assert!(out.contains("@@@\"\nfn main() {}\n\"@@@.rust"), "{out}");
}

/// The atrep parse gate: what `| atrep` emits, atrep's own parser
/// accepts. Env-gated: needs ATREP_BIN and LITOGRAMMA_DIA.
#[test]
fn atrep_output_parses() {
    let (Ok(bin), Ok(dia)) = (
        std::env::var("ATREP_BIN"),
        std::env::var("LITOGRAMMA_DIA"),
    ) else {
        eprintln!("skip: set ATREP_BIN and LITOGRAMMA_DIA to run the parse gate");
        return;
    };
    let m = TextModel::build(vec![
        Block::Heading {
            level: 2,
            lemma: "Aftermath".into(),
        },
        Block::Paragraph {
            text: "The emus prevailed.".into(),
        },
        Block::Open {
            kind: Container::UnorderedList,
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "a bounty system".into(),
        },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
    ]);
    let doc = &values(&m, "^ | atrep")[0];
    let dir = std::env::temp_dir().join("quarb-atrep-gate");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&dia, dir.join("litogramma.dia")).unwrap();
    let path = dir.join("gate.atd");
    std::fs::write(&path, format!("{doc}\n")).unwrap();
    let out = std::process::Command::new(&bin)
        .arg("check")
        .arg(&path)
        .output()
        .expect("run atrep check");
    assert!(
        out.status.success(),
        "atrep rejected the emission:\n{}\n---\n{doc}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Ruling #27: `::grammata` (the body between lemma and
/// hypograph) and the friendly aliases — `::title`, `::body`,
/// `::attribution`, `::ord` — answered beside the Greek.
#[test]
fn grammata_and_the_friendly_aliases() {
    let m = TextModel::build(vec![
        Block::Heading {
            level: 2,
            lemma: "Aftermath".into(),
        },
        Block::Paragraph {
            text: "The emus prevailed.".into(),
        },
        Block::Table {
            lemma: None,
            headers: None,
            rows: vec![vec![quarb_text::Cell {
                label: Some("Outcome".into()),
                text: "Emu victory".into(),
            }]],
        },
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Paragraph {
            text: "Invulnerable as tanks.".into(),
        },
        Block::Close {
            hypograph: Some("Major Meredith".into()),
        },
    ]);
    // A section's grammata is its body without the title.
    assert_eq!(
        values(&m, "//section[::lemma = \"Aftermath\"]::grammata @| [1] | [$_ =~ /^The emus/] @| count"),
        vec!["1"]
    );
    // A cell's body is the value without the label fold.
    assert_eq!(values(&m, "//*<cell>::grammata"), vec!["Emu victory"]);
    assert_eq!(values(&m, "//*<cell>::body"), vec!["Emu victory"]);
    // Aliases answer beside the Greek, wherever properties go.
    assert_eq!(values(&m, "//*<cell>::title"), vec!["Outcome"]);
    assert_eq!(
        values(&m, "//section[::title = \"Aftermath\"]::lemma"),
        vec!["Aftermath"]
    );
    assert_eq!(
        values(&m, "//blockquote::attribution"),
        vec!["Major Meredith"]
    );
    assert_eq!(
        values(&m, "//blockquote::grammata"),
        vec!["Invulnerable as tanks."]
    );
    assert_eq!(values(&m, "//*<row>[::ord = 1]::taxis"), vec!["1"]);
}

/// Ruling #29: a closed-surface adapter answers its annotations
/// at `::` too — data first, alias only when the node has no
/// such property, and core metadata never aliased.
#[test]
fn closed_surface_adapters_alias_their_metadata() {
    let m = TextModel::build(vec![
        Block::Heading { level: 3, lemma: "Aftermath".into() },
        Block::Verbatim { lang: Some("rust".into()), text: "fn main() {}".into() },
    ]);
    // The annotation answers at either depth.
    assert_eq!(values(&m, "//section::level"), vec!["3"]);
    assert_eq!(values(&m, "//section::::level"), vec!["3"]);
    assert_eq!(values(&m, "//verbatim::lang"), vec!["rust"]);
    // Predicates see the alias too.
    assert_eq!(values(&m, "//section[::level = 3]::lemma"), vec!["Aftermath"]);
    // An undeclared metadata key stays four-colon only.
    assert_eq!(values(&m, "//section::nonesuch"), vec![""]);
    // Core metadata is never aliased: `::name` is not `:::name`.
    let names = values(&m, "//section | rec(\"data\", ::name, \"core\", :::name)");
    assert!(names[0].contains("data = null"), "{names:?}");
}

/// Ruling #35 as amended: resolution pairs (family, onym); a
/// callout whose source declares no family (an HTML noteref)
/// takes its resolved body's family, and dangles as a footnote.
#[test]
fn open_family_callouts_take_their_bodys_family() {
    use quarb_text::NoteFamily;
    let m = TextModel::build(vec![
        Block::Paragraph { text: "Prose.".into() },
        Block::NoteRef { onym: "a".into(), family: None, margin: false },
        Block::NoteRef { onym: "b".into(), family: None, margin: false },
        Block::NoteRef { onym: "c".into(), family: None, margin: false },
        Block::Open {
            kind: Container::Note { onym: "a".into(), family: NoteFamily::Footnote, margin: false },
            lemma: None,
        },
        Block::Text { text: "The footnote.".into() },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::Note { onym: "b".into(), family: NoteFamily::Endnote, margin: false },
            lemma: None,
        },
        Block::Text { text: "The endnote.".into() },
        Block::Close { hypograph: None },
    ]);
    assert_eq!(values(&m, "//*<deixis>->footnote::"), ["The footnote."]);
    assert_eq!(values(&m, "//*<deixis>->endnote::"), ["The endnote."]);
    assert_eq!(values(&m, "//*<dangling>::onym"), ["c"]);
    // the body IS the note: <note> marks the two bodies,
    // whichever family; callouts answer <deixis> alone
    assert_eq!(values(&m, "//*<note> @| count"), ["2"]);
    assert_eq!(values(&m, "//*<deixis> @| count"), ["3"]);
}

/// Ruling #36: marks are invisible anchors — no `<block>` trait,
/// no contribution to the surrounding prose — carrying `::term`
/// as written with `|...` directives stripped.
#[test]
fn index_marks_are_invisible_anchors() {
    let m = TextModel::build(vec![
        Block::Paragraph { text: "Emus advanced.".into() },
        Block::IndexMark { term: "emu".into() },
        Block::IndexMark { term: "wheat districts|textbf".into() },
    ]);
    assert_eq!(values(&m, "//index-mark::term"), ["emu", "wheat districts"]);
    assert_eq!(values(&m, "/paragraph::"), ["Emus advanced."]);
    assert_eq!(values(&m, "//*<block> @| count"), ["1"]);
}

/// Ruling #37: verse holds strophes holding stichos lines; the
/// stichos taxis numbers lines continuously across strophes (the
/// citation coordinate), and the flattened prose separates
/// strophes with a blank line.
#[test]
fn verse_lines_carry_the_citation_coordinate() {
    let m = TextModel::build(vec![Block::Verse {
        lemma: Some("Ode".into()),
        strophes: vec![
            vec!["Happy the man, whose wish and care".into(),
                 "A few paternal acres bound,".into()],
            vec!["Content to breathe his native air,".into(),
                 "In his own ground.".into()],
        ],
        hypograph: None,
    }]);
    assert_eq!(values(&m, "//verse::lemma"), ["Ode"]);
    assert_eq!(values(&m, "//strophe @| count"), ["2"]);
    assert_eq!(values(&m, "//stichos @| count"), ["4"]);
    // continuous numbering across strophes
    assert_eq!(
        values(&m, r#"//stichos[::taxis = 3]::"#),
        ["Content to breathe his native air,"]
    );
    assert_eq!(values(&m, "//strophe[2]/stichos[1]::taxis"), ["3"]);
    // strophes separate with a blank line in the flattened prose
    assert_eq!(
        values(&m, "//verse::"),
        ["Ode\nHappy the man, whose wish and care\nA few paternal acres bound,\n\nContent to breathe his native air,\nIn his own ground."]
    );
    // sub-block structure: only the verse block carries <block>
    assert_eq!(values(&m, "//verse<block> @| count"), ["1"]);
    assert_eq!(values(&m, "//stichos<block> @| count"), ["0"]);
}
