//! The dictionary vocabulary (spec: The Dictionary Vocabulary; the
//! Lingvo DSL paragraph): a `.dsl` dictionary reads through atrep's
//! importer into lexigramma and lowers to entries, senses and the
//! inline apparatus; the spec's queries answer on atrep's own
//! fixtures (three Webster 1913 entries with their abbreviations
//! file; two of Dahl's).

use std::path::Path;

use quarb::QueryResult;
use quarb_text_koine::{dsl_declared_encoding, is_dsl_path, parse_dsl, parse_dsl_file, parse_str};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/dsl")
        .join(name)
}

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

#[test]
fn entries_senses_and_the_apparatus_answer_the_spec_queries() {
    let m = parse_dsl_file(&fixture("webster.dsl")).unwrap();
    // A card is an entry named by its headword; a sub-card nests.
    assert_eq!(values(&m, "//entry @| count"), ["5"]);
    assert_eq!(
        values(&m, "/entry::lemma"),
        ["Liberal", "Liberalism", "to bank", "bank"]
    );
    assert_eq!(
        values(&m, "//entry[::lemma = \"to bank\"]/entry::lemma"),
        ["bank up"]
    );
    // The headword registers as the onym: a cross-reference lands.
    assert_eq!(
        values(&m, "//entry[::lemma = \"Liberal\"]::onym"),
        ["Liberal"]
    );
    assert_eq!(
        values(&m, "//entry[::lemma = \"Liberal\"]//ref-->::lemma"),
        ["Liberalism"]
    );
    // Senses by their numbering and margins, nested as subdivided;
    // the definition is the sense's prose.
    assert_eq!(
        values(&m, "//entry[::lemma = \"Liberal\"]/sense::taxis"),
        ["1", "2", "3", "4"]
    );
    assert_eq!(
        values(
            &m,
            "//entry[::lemma = \"Liberal\"]/sense[::taxis = 2]/sense::"
        ),
        [
            "Not strict or rigorous; free; as, a liberal translation of a classic.",
            "Not narrow or contracted in mind; catholic."
        ]
    );
    assert_eq!(
        values(&m, "//entry[::lemma = \"Liberal\"]/sense[::taxis = 1]::"),
        [
            "Free by birth; hence, befitting a freeman or gentleman; refined; noble. a liberal ancestry"
        ]
    );
    // The inline apparatus: spans named by their sim, their text
    // the projection, no block trait.
    assert_eq!(values(&m, "//grammar::"), ["a.", "n.", "v. i.", "n."]);
    // Webster's `Etym:` line is the etymology (atrep 0.3.9).
    assert_eq!(
        values(&m, "//etymology::"),
        [
            "F. libéral, L. liberalis, fr. liber free.",
            "OE. banke; akin to E. bench."
        ]
    );
    assert_eq!(
        values(&m, "//pronunciation::"),
        ["lĭb′ẽr·al", "lĭb′ẽr·al·ĭz’m"]
    );
    assert_eq!(
        values(&m, "//citation::"),
        [
            "a liberal ancestry",
            "a liberal giver",
            "We walked along the river bank."
        ]
    );
    assert_eq!(
        values(&m, "//entry[//citation:: = \"a liberal giver\"]::lemma"),
        ["Liberal"]
    );
    assert_eq!(values(&m, "//label::"), ["Obs."]);
    // An untyped label has no kind.
    assert_eq!(values(&m, "//label::kind"), [""]);
    assert_eq!(
        values(&m, "//entry[//label:: = \"Obs.\"]::lemma"),
        ["Liberal"]
    );
    assert_eq!(values(&m, "//citation<block> @| count"), ["0"]);
    // The secondary text is a span of kind `secondary`.
    assert_eq!(
        values(&m, "//span[::kind = \"secondary\"]::"),
        ["Syn. — Generous; bountiful; munificent; ample; profuse; free."]
    );
}

#[test]
fn the_sorting_form_is_a_hidden_inflection_on_the_entry() {
    let m = parse_dsl_file(&fixture("webster.dsl")).unwrap();
    // `{to} bank`: the unsorted part stays in the lemma, the
    // sorted remainder is a lookup key, hidden, on the entry
    // itself (met in the headword line).
    assert_eq!(values(&m, "//inflection"), ["/entry[3]/span"]);
    assert_eq!(values(&m, "//inflection::"), ["bank"]);
    assert_eq!(values(&m, "//inflection::kind"), ["sort"]);
    assert_eq!(values(&m, "//inflection<hidden> @| count"), ["1"]);
    assert_eq!(
        values(&m, "//entry[//inflection:: = \"bank\"]::lemma"),
        ["to bank"]
    );
    // Hidden: not in any paragraph's prose. (The card's second
    // headword is a variant, in a paragraph of its own.)
    assert_eq!(
        values(&m, "//entry[::lemma = \"to bank\"]/paragraph[1]/variant::"),
        ["bank"]
    );
    assert_eq!(
        values(&m, "//entry[::lemma = \"to bank\"]/paragraph[1]::"),
        ["bank"]
    );
    assert_eq!(
        values(&m, "//entry[::lemma = \"to bank\"]/paragraph[2]::"),
        ["v. i."]
    );
}

#[test]
fn the_abbreviations_list_resolves_the_labels() {
    let m = parse_dsl_file(&fixture("webster.dsl")).unwrap();
    // The included abbreviations file is the list of that kind.
    assert_eq!(values(&m, "//unordered-list::kind"), ["abbreviations"]);
    assert_eq!(
        values(&m, "//unordered-list[::kind = \"abbreviations\"]/*::lemma"),
        ["a.", "n.", "v. i.", "Obs.", "Etym:"]
    );
    // A label resolves against it by its text.
    assert_eq!(values(&m, "//label-->::lemma"), ["Obs."]);
    assert_eq!(values(&m, "//label-->::"), ["Obs.: obsolete"]);
    // Without the abbreviations file the labels stay unresolved.
    let bytes = std::fs::read(fixture("webster.dsl")).unwrap();
    let alone = parse_dsl(&bytes, None).unwrap();
    assert_eq!(values(&alone, "//unordered-list @| count"), ["0"]);
    assert_eq!(values(&alone, "//label::"), ["Obs."]);
}

#[test]
fn the_dictionarys_identity_and_languages() {
    let m = parse_dsl_file(&fixture("webster.dsl")).unwrap();
    assert_eq!(
        values(&m, ":::title"),
        ["Webster's Revised Unabridged Dictionary (1913)"]
    );
    assert_eq!(values(&m, ":::languages"), ["en, en"]);
}

#[test]
fn a_russian_dictionary_keeps_its_stress_and_citations() {
    let m = parse_dsl_file(&fixture("dahl.dsl")).unwrap();
    // The stress mark becomes the combining acute on its letter.
    assert_eq!(values(&m, "/entry::lemma"), ["КНИ́ГА", "ПЕ́РО"]);
    assert_eq!(values(&m, "//label::"), ["умалит."]);
    // `[lang name="Latin"]penna[/lang]` is an equivalent in that
    // language, and a cross-reference to a stressed headword lands
    // (atrep 0.3.9).
    assert_eq!(values(&m, "//equivalent::"), ["penna"]);
    assert_eq!(values(&m, "//equivalent::lang"), ["la"]);
    assert_eq!(values(&m, "//ref<dangling> @| count"), ["0"]);
    assert_eq!(values(&m, "//ref-->::lemma"), ["КНИ́ГА"]);
    assert_eq!(
        values(&m, "//entry[//citation:: = \"Записная книжка.\"]::lemma"),
        ["КНИ́ГА"]
    );
    assert_eq!(values(&m, ":::languages"), ["ru, ru"]);
}

#[test]
fn a_compressed_dictionary_and_the_path_rule() {
    let dir = std::env::temp_dir().join(format!("quarb-koine-dsl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // `.dsl.dz` is gzip; the `#INCLUDE` resolves beside it.
    let bytes = std::fs::read(fixture("webster.dsl")).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &bytes).unwrap();
    std::fs::write(dir.join("webster.dsl.dz"), gz.finish().unwrap()).unwrap();
    std::fs::copy(fixture("abbrev.dsl"), dir.join("abbrev.dsl")).unwrap();
    let m = parse_dsl_file(&dir.join("webster.dsl.dz")).unwrap();
    assert_eq!(values(&m, "//entry @| count"), ["5"]);
    assert_eq!(values(&m, "//unordered-list::kind"), ["abbreviations"]);
    assert!(is_dsl_path(Path::new("x/dict.dsl")));
    assert!(is_dsl_path(Path::new("x/dict.DSL.dz")));
    assert!(!is_dsl_path(Path::new("x/dict.dz")));
    assert!(!is_dsl_path(Path::new("x/notes.md")));
}

#[test]
fn the_declared_encoding_is_the_bom_or_the_code_page_header() {
    assert_eq!(
        dsl_declared_encoding(b"\xFF\xFE#\x00N\x00"),
        Some("utf-16le".into())
    );
    assert_eq!(
        dsl_declared_encoding(b"\xFE\xFF\x00#"),
        Some("utf-16be".into())
    );
    assert_eq!(
        dsl_declared_encoding(b"#NAME \"x\"\n#SOURCE_CODE_PAGE \"Cyrillic\"\n"),
        Some("windows-1251".into())
    );
    assert_eq!(dsl_declared_encoding(b"#NAME \"x\"\n"), None);
}

/// A dictionary written in lexigramma itself: the title line is its
/// declared identity and the languages line `:::languages` on the
/// native route too, and the abbreviations list — undeclared, so
/// the one standing before the first entry (the `abbreviations`
/// sim, `@[[ … ]]@`, is the declared form from the atrep release
/// after 0.3.8; a sim, never a genos, by the user's ruling) —
/// resolves a label against any of an item's comma-separated
/// spellings.
#[test]
fn a_native_lexigramma_dictionary_declares_itself() {
    let src = "@@@!lexigramma\n\n@= Pocket Latin =@\n\n@=/ la en /=@\n\n@::;\n@:: poet., poetic.\n@;\npoetic\n;@\n::@\n;::@\n\n@! penna\n@:(1)\nA feather @[ poet. ]@ or a pen.\n:@\n!@\n";
    let m = parse_str(src, Path::new(".")).unwrap();
    assert_eq!(values(&m, ":::title"), ["Pocket Latin"]);
    assert_eq!(values(&m, ":::languages"), ["la, en"]);
    assert_eq!(values(&m, "//entry::lemma"), ["penna"]);
    assert_eq!(values(&m, "//unordered-list::kind"), ["abbreviations"]);
    assert_eq!(values(&m, "//label-->::"), ["poet., poetic.: poetic"]);
}

/// The abbreviations list declared by its sim (`@[[ … ]]@`, atrep
/// 0.3.9; a sim, never a genos) resolves the labels wherever it
/// stands, and the DSL importer now writes that form.
#[test]
fn the_abbreviations_sim_declares_the_list() {
    let src = "@@@!lexigramma\n\n@= Pocket Latin =@\n\n@! penna\n@:(1)\nA feather @[ poet. ]@ or a pen.\n:@\n!@\n\n@[[\n@::;\n@:: poet., poetic.\n@;\npoetic\n;@\n::@\n;::@\n]]@\n";
    let m = parse_str(src, Path::new(".")).unwrap();
    assert_eq!(values(&m, "//unordered-list::kind"), ["abbreviations"]);
    assert_eq!(values(&m, "//label-->::"), ["poet., poetic.: poetic"]);
    let m = parse_dsl_file(&fixture("webster.dsl")).unwrap();
    assert_eq!(values(&m, "//unordered-list::kind"), ["abbreviations"]);
    assert_eq!(values(&m, "//label-->::lemma"), ["Obs."]);
}
