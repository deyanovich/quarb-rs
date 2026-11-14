//! The mark of a joined expression (ruling 2026-11-14): `D <=>
//! J[cond] .al` files the node the join binds as the mark `.al` on
//! the driver's thread, read back as `$$.al`; `$$1`, `$$2` stay
//! valid beside the names. The join mark is the mark that ENDS the
//! joined expression; every other mark inside it is its own.

use quarb::{QueryResult, Value};

/// lang's music fixture, the part the join needs.
fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"artists": [{"id": 1, "name": "Holst"},
                        {"id": 2, "name": "Bartok"},
                        {"id": 3, "name": "Satie"}],
            "albums":  [{"id": 1, "title": "The Planets", "year": 1918, "artist_id": 1},
                        {"id": 2, "title": "Mikrokosmos", "year": 1939, "artist_id": 2},
                        {"id": 3, "title": "Gymnopedies", "year": 1888, "artist_id": 3},
                        {"id": 4, "title": "Quartets", "year": 1928, "artist_id": 2}],
            "tracks":  [{"id": 1, "title": "Mars", "album_id": 1, "secs": 430},
                        {"id": 2, "title": "Venus", "album_id": 1, "secs": 480},
                        {"id": 3, "title": "Jupiter", "album_id": 1, "secs": 470},
                        {"id": 4, "title": "Bourree", "album_id": 2, "secs": 95},
                        {"id": 5, "title": "Ostinato", "album_id": 2, "secs": 240},
                        {"id": 6, "title": "Gymnopedie No.1", "album_id": 3, "secs": 210},
                        {"id": 7, "title": "Quartet No.4", "album_id": 4, "secs": 1500}]}"#,
    )
    .unwrap()
}

fn outcome(q: &str) -> (Vec<String>, Vec<String>) {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .run(&doc(), &run)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let values = match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::to_quarb).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    };
    (
        values,
        out.diagnostics.iter().map(|d| d.to_string()).collect(),
    )
}

fn values(q: &str) -> Vec<String> {
    outcome(q).0
}

fn refused(q: &str) -> String {
    match quarb::Query::parse(q, &doc()) {
        Ok(_) => panic!("{q}: parsed"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_mark_names_the_witness() {
    // the example the ruling was made on: three tables by name
    let named = values(
        "/tracks/*[::secs > 400] .t \
           <=> /albums/*[::id = $$.t::album_id] .al \
           <=> /artists/*[::id = $$.al::artist_id] .ar \
         | %(artist = $$.ar::name; album = $$.al::title; $$.t::title)",
    );
    assert_eq!(
        named,
        [
            r#"%(artist = "Holst"; album = "The Planets"; title = "Mars")"#,
            r#"%(artist = "Holst"; album = "The Planets"; title = "Venus")"#,
            r#"%(artist = "Holst"; album = "The Planets"; title = "Jupiter")"#,
            r#"%(artist = "Bartok"; album = "Quartets"; title = "Quartet No.4")"#,
        ]
    );
    // the numbers stay valid beside the names, and say the same
    assert_eq!(
        values(
            "/tracks/*[::secs > 400] .t \
               <=> /albums/*[::id = $$.t::album_id] .al \
               <=> /artists/*[::id = $$1::artist_id] .ar \
             | %(artist = $$2::name; album = $$1::title; $$.t::title)",
        ),
        named
    );
}

#[test]
fn a_join_mark_is_a_mark() {
    // it takes the next position after the driver's marks, the
    // joined expressions in their order; `$$.` is the latest
    assert_eq!(
        values(
            "/tracks/*[::secs > 1000] .t \
               <=> /albums/*[::id = $$.t::album_id] .al \
               <=> /artists/*[::id = $$.al::artist_id] .ar \
             | %(a = $$.1::title; b = $$.2::title; c = $$.3::name; d = $$.::name)",
        ),
        [r#"%(a = "Quartet No.4"; b = "Quartets"; c = "Bartok"; d = "Bartok")"#]
    );
    // the node side lists it, and a path starts from it
    assert_eq!(
        values(
            "/tracks/*[::secs > 1000] .t <=> /albums/*[::id = $$.t::album_id] .al \
             | %(n = (@@. | count); y = $$.al::year)"
        ),
        ["%(n = 2; y = 1928)"]
    );
    // a condition reads an earlier joined expression by position too
    assert_eq!(
        values(
            "/tracks/*[::secs > 1000] .t \
               <=> /albums/*[::id = $$.t::album_id] .al \
               <=> /artists/*[::id = $$.2::artist_id] .ar \
             | $$.ar::name",
        ),
        [r#""Bartok""#]
    );
    // the parse-time count includes the join marks
    assert!(
        refused("/tracks/* .t <=> /albums/*[::id = $$.t::album_id] .al | %(n = $$.3::title)")
            .contains("no mark at position '.3'; the query marks 2 nodes")
    );
    // a bare mark files the witness with no name
    assert_eq!(
        values("/tracks/*[..1] <=> /albums/*[::id = $$0::album_id] . | $$.::title"),
        [r#""The Planets""#]
    );
    // several marks in a row file it under each
    assert_eq!(
        values(
            "/tracks/*[..1] .t <=> /albums/*[::id = $$.t::album_id] .a .b \
             | %(a = $$.a::title; b = $$.b::title)"
        ),
        [r#"%(a = "The Planets"; b = "The Planets")"#]
    );
    // a later push under the name shadows it, as any mark's
    assert_eq!(
        values(
            "/tracks/*[..1] .t <=> /albums/*[::id = $$.t::album_id] .al \
             | /../../artists/*[3] | .al | $$.al::name"
        ),
        [r#""Satie""#]
    );
}

#[test]
fn the_outer_join_files_no_mark_where_it_binds_none() {
    // no witness, no mark: the name reads as `$$1` does there
    let q = "/tracks/* .t <=>? /albums/*[::id = $$.t::album_id && ::year > 1930] .al";
    assert_eq!(
        values(&format!("{q} | %(t = $$.t::title; al = $$.al::title)")),
        [
            r#"%(t = "Mars"; al = null)"#,
            r#"%(t = "Venus"; al = null)"#,
            r#"%(t = "Jupiter"; al = null)"#,
            r#"%(t = "Bourree"; al = "Mikrokosmos")"#,
            r#"%(t = "Ostinato"; al = "Mikrokosmos")"#,
            r#"%(t = "Gymnopedie No.1"; al = null)"#,
            r#"%(t = "Quartet No.4"; al = null)"#,
        ]
    );
    // the anti-join asks the name
    let (rows, told) = outcome(&format!("{q} | [!$$.al] | $$.t::title"));
    assert_eq!(
        rows,
        [
            r#""Mars""#,
            r#""Venus""#,
            r#""Jupiter""#,
            r#""Gymnopedie No.1""#,
            r#""Quartet No.4""#
        ]
    );
    assert!(told.is_empty(), "{told:?}");
    // a later condition that reads the missing witness binds nothing
    assert_eq!(
        values(&format!(
            "{q} <=>? /artists/*[::id = $$.al::artist_id] .ar \
             | [::secs > 400 || ::secs < 100] | %(t = $$.t::title; ar = $$.ar::name)"
        )),
        [
            r#"%(t = "Mars"; ar = null)"#,
            r#"%(t = "Venus"; ar = null)"#,
            r#"%(t = "Jupiter"; ar = null)"#,
            r#"%(t = "Bourree"; ar = "Bartok")"#,
            r#"%(t = "Quartet No.4"; ar = null)"#,
        ]
    );
}

#[test]
fn every_witness_files_its_own() {
    let (rows, told) = outcome(
        "/albums/*[::year < 1920] .al <=>* /tracks/*[::album_id = $$.al::id] .t \
         | %(al = $$.al::title; t = $$.t::title)",
    );
    assert_eq!(
        rows,
        [
            r#"%(al = "The Planets"; t = "Mars")"#,
            r#"%(al = "The Planets"; t = "Venus")"#,
            r#"%(al = "The Planets"; t = "Jupiter")"#,
            r#"%(al = "Gymnopedies"; t = "Gymnopedie No.1")"#,
        ]
    );
    assert!(told.is_empty(), "{told:?}");
}

#[test]
fn the_plain_join_tells_of_the_mark_it_chose() {
    // the witness read by its mark: the warning names the mark
    let (rows, told) = outcome(
        "/albums/* .al <=> /tracks/*[::album_id = $$.al::id] .t \
         | %(al = $$.al::title; t = $$.t::title)",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("SEVERAL_WITNESSES ($$.t)")
            && told[0].contains("reads the witness ($$.t)"),
        "{told:?}"
    );
    // read by its number, it is named by its number, as before
    let (_, told) = outcome(
        "/albums/* .al <=> /tracks/*[::album_id = $$.al::id] .t \
         | %(al = $$.al::title; t = $$1::title)",
    );
    assert!(told[0].contains("SEVERAL_WITNESSES ($$1)"), "{told:?}");
    // the bare test of the mark is the join filtering: no read
    let (rows, told) =
        outcome("/albums/* .al <=> /tracks/*[::album_id = $$.al::id] .t | [$$.t] | $$.al::title");
    assert_eq!(rows.len(), 4);
    assert!(told.is_empty(), "{told:?}");
    // a read of the node side by position reads every marked witness
    let (_, told) = outcome("/albums/* .al <=> /tracks/*[::album_id = $$.al::id] .t | $$.2::title");
    assert!(told[0].contains("SEVERAL_WITNESSES ($$.t)"), "{told:?}");
}

#[test]
fn the_mark_that_ends_the_expression_is_the_join_mark() {
    // a joined expression with a pipeline of its own binds what the
    // pipeline leaves: the dot that closes it names that node
    assert_eq!(
        values(
            "/tracks/*[::secs > 1000] .t \
               <=> /artists/* .x | [::id = 2] | .w \
               <=> /albums/*[::artist_id = $$.w::id && ::year > 1930] .late \
             | %(w = $$.w::name; same = $$1::name; late = $$.late::title)",
        ),
        [r#"%(w = "Bartok"; same = "Bartok"; late = "Mikrokosmos")"#]
    );
    // the mark before that pipeline is the expression's own
    assert!(
        refused(
            "/tracks/* .t \
               <=> /albums/* .x | /../../artists/*[1] | .w \
               <=> /artists/*[::id = $$.w::id] \
             | $$.x::title",
        )
        .contains("'.x' is filed inside joined expression #1 and is that expression's own"),
    );
    // which its own pipeline reads
    assert_eq!(
        values(
            "/tracks/*[..1] .t \
               <=> /albums/* .x | [::id = $$.x::id && ::year > 1930] \
               <=> /artists/*[::id = $$1::artist_id] \
             | %(al = $$1::title; ar = $$2::name)",
        ),
        [r#"%(al = "Mikrokosmos"; ar = "Bartok")"#]
    );
    // a union: the mark after the last branch, whichever branch the
    // witness came from
    assert_eq!(
        values(
            "/tracks/*[..2] .t \
               <=> /albums/*[::id = $$.t::album_id] || /artists/*[::id = 0] .w \
             | $$.w::title",
        ),
        [r#""The Planets""#, r#""The Planets""#]
    );
}

#[test]
fn what_a_join_mark_refuses() {
    // the name is new
    assert!(
        refused("/tracks/* .t <=> /albums/*[::id = $$.t::album_id] .t | count").contains(
            "the mark '.t' of joined expression #1 is already filed before it: \
             a joined node's mark takes a name of its own"
        )
    );
    assert!(
        refused(
            "/tracks/* .t <=> /albums/*[::id = $$.t::album_id] .al \
             <=> /artists/*[::id = $$.al::artist_id] .al | count"
        )
        .contains("the mark '.al' of joined expression #2 is already filed before it")
    );
    // a name pushed before the join counts, one pushed after does not
    assert!(
        refused("/tracks/* | .al(::title) <=> /albums/*[::id = $$0::album_id] .al | count")
            .contains("is already filed before it")
    );
    assert_eq!(
        values(
            "/tracks/*[..1] <=> /albums/*[::id = $$0::album_id] .al \
             | .al(::title) | $.al"
        ),
        [r#""Mars""#]
    );
    // a condition reads neither its own mark nor a later one
    assert!(
        refused(
            "/tracks/* .t <=> /albums/*[::id = $$.t::album_id && $$.al::year > 1900] .al | count"
        )
        .contains(
            "the condition of joined expression #1 reads its own mark '.al': the candidate \
             is the bare path there, or '$$_'"
        )
    );
    assert!(
        refused(
            "/tracks/* .t <=> /albums/*[::id = $$.t::album_id && $$.ar::id] .al \
             <=> /artists/*[::id = $$.al::artist_id] .ar | count"
        )
        .contains(
            "the condition of joined expression #1 reads '.ar', the mark of joined \
             expression #2"
        )
    );
    // a mark mid-path is the expression's own: nothing outside reads it
    for q in [
        "/tracks/* .t <=> /albums .x /*[::id = $$.t::album_id] | %(x = $$.x:name; ::title)",
        "/tracks/* .t <=> /albums .x /*[::id = $$.t::album_id && $$.x] | count",
    ] {
        assert!(
            refused(q)
                .contains("'.x' is filed inside joined expression #1 and is that expression's own"),
            "{q}"
        );
    }
    // a condition on an expression whose own pipeline navigates was
    // tested on the node the pipeline went to: refused
    assert!(
        refused(
            "/tracks/* .t <=> /albums/*[::id = $$.t::album_id] | /../../artists/*[1] \
             <=> /artists/*[::id = $$1::id] | count"
        )
        .contains("joined expression #1 carries a condition and navigates in its own pipeline")
    );
    // a pipeline that filters, sorts and marks stays, and so does a
    // navigating one with no condition
    assert_eq!(
        values(
            "/tracks/*[..1] .t \
               <=> /albums/*[::id = $$.t::album_id] | [::year > 1900] | .al \
               <=> /artists/* | /../../albums/*[::year < 1900] | .old \
             | %(al = $$.al::title; old = $$.old::title)"
        ),
        [r#"%(al = "The Planets"; old = "Gymnopedies")"#]
    );
    // a mark between the step and its brackets is still no mark
    assert!(
        quarb::Query::parse(
            "/tracks/* .t <=> /albums/* .al [::id = $$.t::album_id] | count",
            &doc()
        )
        .is_err()
    );
}
