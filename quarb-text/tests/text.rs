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
        values(&m, "/section[::lemma = \"Two\"]::"),
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
    assert_eq!(values(&m, "//unordered-item::"), vec!["Alice", "captain"]);
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
        rows: vec![vec![quarb_text::Cell {
            label: Some("Location".into()),
            text: "Campion".into(),
        }]],
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
        values(&m, r#"//section[::lemma == (/war/)] | markdown"#),
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
        vec!["@@@!litogramma\n\n@# First attempt\n\nThe birds split into small groups.\n\n#@"]
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
    let (Ok(bin), Ok(dia)) = (std::env::var("ATREP_BIN"), std::env::var("LITOGRAMMA_DIA")) else {
        eprintln!("skip: set ATREP_BIN && LITOGRAMMA_DIA to run the parse gate");
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
        values(
            &m,
            "//section[::lemma = \"Aftermath\"]::grammata @| [1] | [$_ == (/^The emus/)] @| count"
        ),
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
        Block::Heading {
            level: 3,
            lemma: "Aftermath".into(),
        },
        Block::Verbatim {
            lang: Some("rust".into()),
            text: "fn main() {}".into(),
        },
    ]);
    // The annotation answers at either depth.
    assert_eq!(values(&m, "//section::level"), vec!["3"]);
    assert_eq!(values(&m, "//section::::level"), vec!["3"]);
    assert_eq!(values(&m, "//verbatim::lang"), vec!["rust"]);
    // Predicates see the alias too.
    assert_eq!(
        values(&m, "//section[::level = 3]::lemma"),
        vec!["Aftermath"]
    );
    // An undeclared metadata key stays four-colon only.
    assert_eq!(values(&m, "//section::nonesuch"), vec![""]);
    // Core metadata is never aliased: `::name` is not `:::name`.
    let names = values(&m, "//section | %(data = ::name; core = :::name)");
    assert!(names[0].contains("data = null"), "{names:?}");
}

/// Ruling #35 as amended: resolution pairs (family, onym); a
/// callout whose source declares no family (an HTML noteref)
/// takes its resolved body's family, and dangles as a footnote.
#[test]
fn open_family_callouts_take_their_bodys_family() {
    use quarb_text::NoteFamily;
    let m = TextModel::build(vec![
        Block::Paragraph {
            text: "Prose.".into(),
        },
        Block::NoteRef {
            onym: "a".into(),
            family: None,
            margin: false,
            at: None,
        },
        Block::NoteRef {
            onym: "b".into(),
            family: None,
            margin: false,
            at: None,
        },
        Block::NoteRef {
            onym: "c".into(),
            family: None,
            margin: false,
            at: None,
        },
        Block::Open {
            kind: Container::Note {
                onym: "a".into(),
                family: NoteFamily::Footnote,
                margin: false,
            },
            lemma: None,
        },
        Block::Text {
            text: "The footnote.".into(),
        },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::Note {
                onym: "b".into(),
                family: NoteFamily::Endnote,
                margin: false,
            },
            lemma: None,
        },
        Block::Text {
            text: "The endnote.".into(),
        },
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
        Block::Paragraph {
            text: "Emus advanced.".into(),
        },
        Block::IndexMark { term: "emu".into() },
        Block::IndexMark {
            term: "wheat districts|textbf".into(),
        },
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
            vec![
                "Happy the man, whose wish && care".into(),
                "A few paternal acres bound,".into(),
            ],
            vec![
                "Content to breathe his native air,".into(),
                "In his own ground.".into(),
            ],
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
        [
            "Ode\nHappy the man, whose wish && care\nA few paternal acres bound,\n\nContent to breathe his native air,\nIn his own ground."
        ]
    );
    // sub-block structure: only the verse block carries <block>
    assert_eq!(values(&m, "//verse<block> @| count"), ["1"]);
    assert_eq!(values(&m, "//stichos<block> @| count"), ["0"]);
}

/// The corpus reading (ruling #62, the sentence tier of ruling
/// #64): under every prose block its sentences, under each its
/// tokens — UAX #29 word segments with their class as a trait,
/// `::lower`, and document-wide `::::n` / `::::sentence`; the
/// block's own prose and the renderers are unchanged, and a
/// sibling hop stops at the sentence.
#[test]
fn corpus_reading_tokenizes_prose_blocks() {
    let mut m = TextModel::parse_plain(
        "CHAPTER I\n\nTom’s aunt said: “Dr. Smith owes 3,000 dollars.” No answer.\n\nSecond one.",
    );
    m.tokenize();
    assert_eq!(values(&m, "//paragraph @| count"), ["3"]);
    assert_eq!(
        values(&m, "//paragraph[2]::"),
        ["Tom’s aunt said: “Dr. Smith owes 3,000 dollars.” No answer."]
    );
    // don’t-style apostrophes and 3,000 stay whole; punctuation is its own token.
    assert_eq!(
        values(&m, "//paragraph[2]//token::"),
        [
            "Tom’s", "aunt", "said", ":", "“", "Dr", ".", "Smith", "owes", "3,000", "dollars", ".",
            "”", "No", "answer", "."
        ]
    );
    // The sentence tier: the paragraph's sentences are its
    // children, the tokens theirs; a sentence answers its text and
    // its document-wide ordinal, is derived, and is no block.
    assert_eq!(values(&m, "//paragraph[2]/sentence @| count"), ["3"]);
    assert_eq!(values(&m, "//paragraph[2]/sentence[3]::"), ["No answer."]);
    assert_eq!(values(&m, "//paragraph[2]/sentence[3]::::n"), ["4"]);
    assert_eq!(
        values(&m, "//paragraph[2]/sentence[3]/token::"),
        ["No", "answer", "."]
    );
    assert_eq!(values(&m, "//sentence<derived> @| count"), ["5"]);
    assert_eq!(values(&m, "//sentence<block> @| count"), ["0"]);
    assert_eq!(values(&m, "//paragraph[2]/* @| count"), ["3"]);
    assert_eq!(
        values(&m, "//paragraph[2]/sentence[2]>sentence::"),
        ["No answer."]
    );
    assert_eq!(values(&m, "//token<word> @| count"), ["13"]);
    assert_eq!(values(&m, "//token<number>::"), ["3,000"]);
    assert_eq!(values(&m, "//token<punct> @| count"), ["7"]);
    assert_eq!(values(&m, "//token[:: = \"Tom’s\"]::lower"), ["tom’s"]);
    assert_eq!(values(&m, "//token[:: = \"Smith\"]::class"), ["word"]);
    // Positions count through the document; the heading's tokens come first.
    assert_eq!(values(&m, "//token[:: = \"Tom’s\"]::::n"), ["3"]);
    assert_eq!(values(&m, "//token[:: = \"Tom’s\"]::n"), ["3"]);
    // UAX #29 breaks after "Dr." — the sentence ordinal shows it,
    // and the sibling hop stops there (ruling #64): the sentence
    // is the token's parent.
    assert_eq!(values(&m, "//token[:: = \"Dr\"]::::sentence"), ["2"]);
    assert_eq!(values(&m, "//token[:: = \"Smith\"]::::sentence"), ["3"]);
    assert_eq!(
        values(&m, "//token[:: = \"Dr\"]>token>token @| count"),
        ["0"]
    );
    assert_eq!(values(&m, "//token[:: = \"Dr\"]>token::"), ["."]);
    // `::sentence` is the sentence's text; `::::sentence` its ordinal.
    assert_eq!(
        values(&m, "//token[:: = \"Smith\"]::sentence"),
        ["Smith owes 3,000 dollars.”"]
    );
    assert_eq!(values(&m, "//token[:: = \"No\"]::sentence"), ["No answer."]);
    // Quotation marks wear <quote> beside <punct> (ruling #65).
    assert_eq!(values(&m, "//token<quote>::"), ["“", "”"]);
    assert_eq!(values(&m, "//token<quote><punct> @| count"), ["2"]);
    assert_eq!(values(&m, "//token<quote>::class"), ["punct", "punct"]);
    // A token is not a block; the paragraph still is.
    assert_eq!(values(&m, "//token<block> @| count"), ["0"]);
    assert_eq!(values(&m, "//paragraph<block> @| count"), ["3"]);
    // The renderers see prose, never tokens.
    let md = quarb::koine::render_node(
        &m,
        quarb::adapter::NodeId(0),
        quarb::koine::Render::Markdown,
    );
    assert!(!md.contains("Tom’s\n\naunt"), "{md}");
    assert!(md.contains("Tom’s aunt said"), "{md}");
}

/// Sentence bonds (ruling #62): a `.desm` abbreviation undoes a
/// UAX #29 break; the tokens and their positions are untouched,
/// only `::::sentence` and `::sentence` change.
#[test]
fn sentence_tailoring_undoes_declared_breaks() {
    let text = "Tom met Dr. Smith. He owed 3,000 dollars. so he said. Then Mrs. Polly came.";
    let mut plain = TextModel::parse_plain(text);
    plain.tokenize();
    assert_eq!(values(&plain, "//token[:: = \"Smith\"]::::sentence"), ["2"]);
    // UAX #29 itself keeps "dollars. so" together (no break before a
    // lowercase letter): five sentences before the tailoring.
    assert_eq!(
        values(
            &plain,
            "//token @| group(s = ::::sentence) | count @| count"
        ),
        ["5"]
    );
    let tailoring = syndesmos::Syndesmos::parse("Dr.\nMrs.\n").unwrap();
    let mut tailored = TextModel::parse_plain(text);
    tailored.tokenize_with(Some(&tailoring));
    assert_eq!(
        values(&tailored, "//token[:: = \"Smith\"]::::sentence"),
        ["1"]
    );
    assert_eq!(
        values(&tailored, "//token[:: = \"Smith\"]::sentence"),
        ["Tom met Dr. Smith."]
    );
    // "so he said." joins the sentence before it; "Then Mrs. Polly came." is one.
    assert_eq!(
        values(&tailored, "//token[:: = \"so\"]::sentence"),
        ["He owed 3,000 dollars. so he said."]
    );
    assert_eq!(
        values(&tailored, "//token[:: = \"Polly\"]::::sentence"),
        ["3"]
    );
    assert_eq!(
        values(
            &tailored,
            "//token @| group(s = ::::sentence) | count @| count"
        ),
        ["3"]
    );
    // Same tokens, same positions.
    assert_eq!(
        values(&tailored, "//token @| count"),
        values(&plain, "//token @| count")
    );
    assert_eq!(
        values(&tailored, "//token[:: = \"Polly\"]::::n"),
        values(&plain, "//token[:: = \"Polly\"]::::n")
    );
    // An abbreviation is a whole word: "Undr." is not "Dr.".
    let mut edge = TextModel::parse_plain("He said Undr. Then left.");
    edge.tokenize_with(Some(&tailoring));
    assert_eq!(values(&edge, "//token[:: = \"Then\"]::::sentence"), ["2"]);
}

/// A CoNLL-U annotation replaces the built-in tokens and sentences
/// by offset alignment and adds lemma, parts of speech, relation
/// and the head edge; a multiword range aligns as the unit; a form
/// that does not align is an error naming the offset.
#[test]
fn conllu_annotation_replaces_the_token_layer() {
    let mut m = TextModel::parse_plain("Tom said: “Dr. Smith owes it.”\n\nHe don't.");
    let conllu = "\
# text = Tom said: “Dr. Smith owes it.”
1\tTom\tTom\tPROPN\tNNP\t_\t2\tnsubj\t_\t_
2\tsaid\tsay\tVERB\tVBD\tTense=Past\t0\troot\t_\tSpaceAfter=No
3\t:\t:\tPUNCT\t:\t_\t2\tpunct\t_\t_
4\t“\t“\tPUNCT\t``\t_\t7\tpunct\t_\tSpaceAfter=No
5\tDr.\tDr.\tPROPN\tNNP\t_\t6\tcompound\t_\t_
6\tSmith\tSmith\tPROPN\tNNP\t_\t7\tnsubj\t_\t_
7\towes\towe\tVERB\tVBZ\t_\t2\tccomp\t_\t_
8\tit\tit\tPRON\tPRP\t_\t7\tobj\t_\tSpaceAfter=No
9\t.\t.\tPUNCT\t.\t_\t2\tpunct\t_\tSpaceAfter=No
10\t”\t”\tPUNCT\t''\t_\t2\tpunct\t_\t_

# newpar
# text = He don't.
1\tHe\the\tPRON\tPRP\t_\t3\tnsubj\t_\t_
2-3\tdon't\t_\t_\t_\t_\t_\t_\t_\tSpaceAfter=No
2\tdo\tdo\tAUX\tVBP\t_\t3\taux\t_\t_
3\tn't\tnot\tPART\tRB\tPolarity=Neg\t0\troot\t_\t_
4\t.\t.\tPUNCT\t.\t_\t3\tpunct\t_\t_
";
    m.annotate_conllu(conllu).unwrap();
    // The tool's tokens: "Dr." is one, and the sentence is the tool's.
    assert_eq!(values(&m, "//token[:: = \"Dr.\"]::::sentence"), ["1"]);
    assert_eq!(
        values(&m, "//token[:: = \"Smith\"]::sentence"),
        ["Tom said: “Dr. Smith owes it.”"]
    );
    assert_eq!(values(&m, "//token[:: = \"owes\"]::lemma"), ["owe"]);
    assert_eq!(values(&m, "//token[:: = \"owes\"]::upos"), ["VERB"]);
    assert_eq!(values(&m, "//token[:: = \"said\"]::feats"), ["Tense=Past"]);
    assert_eq!(values(&m, "//token[:: = \"Smith\"]::deprel"), ["nsubj"]);
    // The head edge and its reverse.
    assert_eq!(values(&m, "//token[:: = \"Smith\"]->head::"), ["owes"]);
    assert_eq!(
        values(&m, "//token[:: = \"owes\"]<-head:: @| join(\" \")"),
        ["“ Smith it"]
    );
    assert_eq!(values(&m, "//token[:: = \"said\"]->head @| count"), ["0"]);
    // The multiword range: its words are tokens sharing the span.
    assert_eq!(
        values(&m, "//paragraph[2]/sentence/token::"),
        ["He", "do", "n't", "."]
    );
    // The tool's sentences are the tier (ruling #64).
    assert_eq!(values(&m, "//sentence @| count"), ["2"]);
    assert_eq!(
        values(&m, "//paragraph[1]/sentence[1]::"),
        ["Tom said: “Dr. Smith owes it.”"]
    );
    assert_eq!(values(&m, "//token[:: = \"n't\"]::lemma"), ["not"]);
    assert_eq!(values(&m, "//token[:: = \"do\"]::sentence"), ["He don't."]);
    assert_eq!(values(&m, "//token[:: = \"n't\"]::::n"), ["13"]);
    // Positions run through the document; the block's prose is untouched.
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        ["Tom said: “Dr. Smith owes it.”"]
    );
    // Misalignment is an error naming the offset.
    let mut bad = TextModel::parse_plain("Tom said hello.");
    let err = bad
        .annotate_conllu("1\tTim\ttim\tPROPN\t_\t_\t0\troot\t_\t_\n")
        .unwrap_err();
    assert!(
        err.contains("does not align") && err.contains("offset 0"),
        "{err}"
    );
    // Tokenizing twice is refused.
    assert!(m.annotate_conllu(conllu).is_err());
}

/// Under an annotation the class follows UPOS (ruling #65): a
/// lettered `NUM` is a number, a tagger's `PUNCT` is punctuation,
/// and a form the tags leave alone keeps the form rule.
#[test]
fn annotated_classes_follow_upos() {
    let mut m = TextModel::parse_plain("It cost 3k or 5 %.");
    let conllu = "\
# text = It cost 3k or 5 %.
1\tIt\tit\tPRON\tPRP\t_\t2\tnsubj\t_\t_
2\tcost\tcost\tVERB\tVBD\t_\t0\troot\t_\t_
3\t3k\t3k\tNUM\tCD\t_\t2\tobj\t_\t_
4\tor\tor\tCCONJ\tCC\t_\t6\tcc\t_\t_
5\t5\t5\tNUM\tCD\t_\t6\tnummod\t_\t_
6\t%\t%\tSYM\tNN\t_\t3\tconj\t_\tSpaceAfter=No
7\t.\t.\tPUNCT\t.\t_\t2\tpunct\t_\t_
";
    m.annotate_conllu(conllu).unwrap();
    assert_eq!(values(&m, "//token<number>::"), ["3k", "5"]);
    assert_eq!(values(&m, "//token<punct>::"), ["%", "."]);
    assert_eq!(values(&m, "//token<word> @| count"), ["3"]);
    assert_eq!(values(&m, "//token[:: = \"3k\"]::class"), ["number"]);
}

/// A cast row's `cluster` column names a coreference chain of the
/// annotation: every mention of the chain is the character's, its
/// first token carrying the mention (ruling #79 addendum).
#[test]
fn cast_table_resolves_coreference_chains() {
    let conllu = "\
# global.Entity = eid-etype-head-other
# text = Tom ran. He laughed. The boy sat.
1\tTom\tTom\tPROPN\tNNP\t_\t2\tnsubj\t_\tEntity=(e1-person-1)
2\tran\trun\tVERB\tVBD\t_\t0\troot\t_\tSpaceAfter=No
3\t.\t.\tPUNCT\t.\t_\t2\tpunct\t_\t_
4\tHe\the\tPRON\tPRP\t_\t5\tnsubj\t_\tEntity=(e1-person-1)
5\tlaughed\tlaugh\tVERB\tVBD\t_\t0\troot\t_\tSpaceAfter=No
6\t.\t.\tPUNCT\t.\t_\t5\tpunct\t_\t_
7\tThe\tthe\tDET\tDT\t_\t8\tdet\t_\tEntity=(e1-person-2
8\tboy\tboy\tNOUN\tNN\t_\t9\tnsubj\t_\tEntity=e1)
9\tsat\tsit\tVERB\tVBD\t_\t0\troot\t_\tSpaceAfter=No
10\t.\t.\tPUNCT\t.\t_\t9\tpunct\t_\t_
";
    let mut m = TextModel::parse_conllu_corpus(conllu).unwrap();
    let row = quarb_text::CastRow {
        id: "tom".to_string(),
        name: "Tom Sawyer".to_string(),
        pattern: None,
        words: 0,
        fields: vec![("cluster".to_string(), "e1".to_string())],
    };
    m.apply_cast(&[row]);
    assert_eq!(values(&m, "//character::lemma"), ["Tom Sawyer"]);
    assert_eq!(
        values(&m, "//character[::onym = \"tom\"]<-character @| count"),
        ["3"]
    );
    assert_eq!(
        values(&m, "//token[::prosopon = \"tom\"]::"),
        ["Tom", "He", "The"]
    );
    assert_eq!(
        values(&m, "//token[:: = \"He\"]->character::lemma"),
        ["Tom Sawyer"]
    );
    assert_eq!(
        values(
            &m,
            "//token[::prosopon = \"tom\"][::deprel = \"nsubj\"]->head::lemma"
        ),
        ["run", "laugh"]
    );
}

/// A cast pattern whose alternatives differ in length ("Tom" or
/// "Tom Sawyer") matches a run of one token as well as the widest
/// run it declares; the run's first token carries the mention.
#[test]
fn cast_pattern_alternatives_of_different_length() {
    let conllu = "\
# text = Tom sat. Tom Sawyer ran.
1\tTom\tTom\tPROPN\tNNP\t_\t2\tnsubj\t_\t_
2\tsat\tsit\tVERB\tVBD\t_\t0\troot\t_\tSpaceAfter=No
3\t.\t.\tPUNCT\t.\t_\t2\tpunct\t_\t_
4\tTom\tTom\tPROPN\tNNP\t_\t6\tnsubj\t_\t_
5\tSawyer\tSawyer\tPROPN\tNNP\t_\t4\tflat\t_\t_
6\tran\trun\tVERB\tVBD\t_\t0\troot\t_\tSpaceAfter=No
7\t.\t.\tPUNCT\t.\t_\t6\tpunct\t_\t_
";
    let mut m = TextModel::parse_conllu_corpus(conllu).unwrap();
    let row = quarb_text::CastRow {
        id: "tom".to_string(),
        name: "Tom Sawyer".to_string(),
        pattern: Some(regex::Regex::new("^(Tom|Tom Sawyer)$").unwrap()),
        words: 2,
        fields: vec![],
    };
    m.apply_cast(&[row]);
    assert_eq!(
        values(&m, "//token[::prosopon = \"tom\"]::"),
        ["Tom", "Tom"]
    );
    assert_eq!(
        values(&m, "//character[::onym = \"tom\"]<-character @| count"),
        ["2"]
    );
}
