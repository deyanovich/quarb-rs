//! The table model (litogramma's `table` / `row` / `header-row` /
//! `cell` sims, `hspan` / `vspan` as monosims first inside a cell):
//! rows are records, cells fields named by their column — the
//! shared table rule's shape, so `<tabula>` / `<row>` / `<cell>`
//! answer as over every other adapter.

use std::path::Path;

use quarb::QueryResult;

fn values(model: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

const DOC: &str = "@@@!litogramma

= Shelf

@+ Books
@+=
@+:
Title
:+@
@+:
Author
:+@
@+:
Year
:+@
=+@
@+-
@+:
Emma
:+@
@+:
Austen
:+@
@+:
1815
:+@
-+@
@+-
@+:
@+>(2) The Iliad and the Odyssey
:+@
@+:
antiquity
:+@
-+@
@+-
@+:
Pages
:+@
@+: Author
Twain
:+@
@+:
1876
:+@
-+@
+@ Three rows.
";

#[test]
fn rows_are_records_and_cells_fields_named_by_their_column() {
    let m = quarb_text_koine::parse_str(DOC, Path::new(".")).unwrap();
    assert_eq!(values(&m, "//*<tabula> @| count"), ["1"]);
    assert_eq!(values(&m, "//*<tabula>::lemma"), ["Books"]);
    assert_eq!(values(&m, "//*<row> @| count"), ["3"]);
    assert_eq!(
        values(&m, "//*<row>[::taxis = 1]/*/*::"),
        ["Title: Emma", "Author: Austen", "Year: 1815"]
    );
    // A cell spanning two columns leaves the next column empty: the
    // year still stands in its own column.
    assert_eq!(
        values(&m, "//*<row>[::taxis = 2]/*/*::"),
        ["Title: The Iliad and the Odyssey", "Year: antiquity"]
    );
    // A cell's own lemma names its column, winning over the header.
    assert_eq!(
        values(&m, "//*<row>[::taxis = 3]/*/*[::lemma = \"Author\"]::"),
        ["Author: Twain"]
    );
    assert_eq!(
        values(&m, "//*<cell>[::lemma = \"Title\"]::"),
        [
            "Title: Emma",
            "Title: The Iliad and the Odyssey",
            "Title: Pages"
        ]
    );
}
