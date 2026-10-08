//! The Tom Sawyer fixture (pandoc's .rtf from the same Markdown
//! as the ODT fixture): outline levels, the list table's bullet
//! and numbered levels, a positional table, inline footnotes with
//! their callouts, a HYPERLINK field, emphasis and code — and
//! hand-written RTF for what pandoc does not write: the
//! stylesheet chain, `\trhdr`, `\deleted`, `\xe`, bookmarks, an
//! endnote, the code page's `\'xx` bytes, the info group.

use quarb::{AstAdapter, QueryResult};
use quarb_text::Block;
use quarb_text_rtf::{blocks, declared_encoding, parse};

const RTF: &[u8] = include_bytes!("fixtures/tom-sawyer.rtf");

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns
            .iter()
            .map(|n| model.name(*n).unwrap_or_default())
            .collect(),
    }
}

#[test]
fn the_fixture_reads_as_its_markdown_source_does() {
    let m = parse(RTF).unwrap();
    assert_eq!(
        values(&m, "//section::lemma"),
        ["Chapter I", "The old lady"]
    );
    assert_eq!(
        values(&m, "//section[::lemma = \"The old lady\"]:::depth"),
        ["2"]
    );
    assert_eq!(
        values(&m, "/section[1]/paragraph[1]::"),
        ["“Tom!” No answer. “Tom!” No answer."]
    );
    assert_eq!(
        values(&m, "//unordered-list[1]/unordered-item @| count"),
        ["2"]
    );
    assert_eq!(
        values(
            &m,
            "//unordered-list[1]/unordered-item[2]/ordered-list/ordered-item::"
        ),
        ["nested one", "nested two"]
    );
    // pandoc declares no header row: positional, lemmaless.
    assert_eq!(values(&m, "//*<cell> @| count"), ["6"]);
    assert_eq!(values(&m, "//*<cell>[::lemma] @| count"), ["0"]);
    assert_eq!(values(&m, "//footnote<deixis> @| count"), ["2"]);
    assert_eq!(
        values(&m, "//footnote<note>::"),
        ["The first call.", "The second call, with emphasis."]
    );
    assert_eq!(
        values(&m, "//footnote<deixis>[1]->footnote::"),
        ["The first call."]
    );
    assert_eq!(values(&m, "//footnote<deixis>[1]::::at"), ["10"]);
    assert_eq!(
        values(&m, "//ref::target"),
        ["https://www.gutenberg.org/ebooks/74"]
    );
    assert_eq!(values(&m, "//ref::"), ["Gutenberg text"]);
    assert_eq!(values(&m, "//emphasis::"), ["through", "emphasis"]);
    assert_eq!(values(&m, "//code::"), ["code"]);
    // pandoc declares `\ansi` and no code page.
    assert_eq!(declared_encoding(RTF).as_deref(), Some("windows-1252"));
}

#[test]
fn the_stylesheet_chain_and_the_quote_names() {
    let rtf = br#"{\rtf1\ansi
{\stylesheet{\s0 Normal;}{\s1\outlinelevel0 heading 1;}{\s2\sbasedon1 My Title;}{\s3 Quote;}{\s4\sbasedon3 Fancy;}}
\pard\s2 Styled heading\par
\pard\outlinelevel2 Direct heading\par
\pard\s4 Quoted one\par
\pard\s3 Quoted two\par
\pard Plain\par
}"#;
    let b = blocks(rtf).unwrap();
    assert!(matches!(&b[0], Block::Heading { level: 1, lemma } if lemma == "Styled heading"));
    assert!(matches!(&b[1], Block::Heading { level: 3, lemma } if lemma == "Direct heading"));
    assert!(matches!(&b[2], Block::Open { .. }));
    assert!(matches!(&b[3], Block::Text { text } if text == "Quoted one"));
    assert!(matches!(&b[4], Block::Text { text } if text == "Quoted two"));
    assert!(matches!(&b[5], Block::Close { .. }));
    assert!(matches!(&b[6], Block::Paragraph { text } if text == "Plain"));
    assert_eq!(b.len(), 7);
}

#[test]
fn header_rows_deletions_marks_bookmarks_and_an_endnote() {
    let rtf = br#"{\rtf1\ansi
\trowd\trhdr\cellx3000\cellx6000 \pard\intbl Port\cell \pard\intbl Host\cell \row
\trowd\cellx3000\cellx6000 \pard\intbl 8080\cell \pard\intbl web\cell \row
\pard{\*\bkmkstart later}A later paragraph {\deleted gone }kept{\*\bkmkstart point} with an {\xe Aunt Polly}index mark.\par
\pard A note{\footnote\ftnalt\pard An endnote.\par} here, {\field{\*\fldinst{HYPERLINK \l "later"}}{\fldrslt{back up}}} and on.\par
}"#;
    let m = parse(rtf).unwrap();
    assert_eq!(
        values(&m, r#"//*<cell>[::lemma = "Port"]::"#),
        ["Port: 8080"]
    );
    assert_eq!(
        values(&m, "//paragraph[1]::"),
        ["A later paragraph kept with an index mark."]
    );
    assert_eq!(values(&m, "//paragraph[1]::onym"), ["later"]);
    assert_eq!(values(&m, "//anchor::onym"), ["point"]);
    assert_eq!(values(&m, "//index-mark::term"), ["Aunt Polly"]);
    assert_eq!(values(&m, "//endnote<deixis> @| count"), ["1"]);
    assert_eq!(values(&m, "//endnote<note>::"), ["An endnote."]);
    assert_eq!(
        values(&m, "//paragraph[2]::"),
        ["A note here, back up and on."]
    );
    assert_eq!(values(&m, "//ref::target"), ["later"]);
    assert_eq!(values(&m, "//ref::"), ["back up"]);
    // An internal ref resolves to its bearer in-document.
    assert_eq!(values(&m, "//ref::::resolved"), ["true"]);
    assert_eq!(
        values(&m, "//ref-->::"),
        ["A later paragraph kept with an index mark."]
    );
}

#[test]
fn the_code_page_decodes_the_bytes_and_is_reported() {
    // «Мёртвые души» in windows-1251 escapes, a \u escape with its
    // substitute, and the info group.
    let rtf = b"{\\rtf1\\ansi\\ansicpg1251\\deff0{\\fonttbl{\\f0 Times;}}\
{\\info{\\title \\'cc\\'b8\\'f0\\'f2\\'e2\\'fb\\'e5 \\'e4\\'f3\\'f8\\'e8}{\\author \\'c3\\'ee\\'e3\\'ee\\'eb\\'fc}{\\creatim\\yr1842\\mo5\\dy21}}\
\\pard \\'cf\\'ee\\'fd\\'ec\\'e0 \\u8212? \\'f2\\'ee\\'ec \\'ef\\'e5\\'f0\\'e2\\'fb\\'e9.\\par}";
    assert_eq!(declared_encoding(rtf).as_deref(), Some("windows-1251"));
    let m = parse(rtf).unwrap();
    assert_eq!(values(&m, "//paragraph::"), ["Поэма — том первый."]);
    assert_eq!(values(&m, "^::title"), ["Мёртвые души"]);
    assert_eq!(values(&m, "^::author"), ["Гоголь"]);
    assert_eq!(values(&m, "^::::created"), ["1842-05-21T00:00:00"]);
}

#[test]
fn not_rtf_is_refused() {
    assert!(parse(b"plain text").is_err());
}
