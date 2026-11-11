//! The rulings of the spec review's settle-first list (2026-11-06):
//! a push body ends in a value, the proximal marker keeps one, the
//! refusal principle, a whole float keeps its point, `$$0` in the
//! driver's pipeline, a forking interpolation, and the warning when
//! a path restarts from the root after an aggregation pipe.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-rulings-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("books.json"),
        r#"{"books": [{"title": "The Histories", "year": -430, "shelf": "A"}, {"title": "Dune", "year": 1965, "shelf": "A"},
                     {"title": "Emma", "year": 1815, "shelf": "B"}]}"#,
    )
    .unwrap();
    let db = rusqlite::Connection::open(dir.join("net.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE stations (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE hops (id INTEGER PRIMARY KEY, from_id INTEGER REFERENCES stations(id), to_id INTEGER REFERENCES stations(id));
         INSERT INTO stations VALUES (1,'Anhalter'),(2,'Borsig'),(3,'Colonnaden'),(4,'Dammtor'),(5,'Elbbruecken');
         INSERT INTO hops VALUES (1,1,2),(2,2,3),(3,1,3),(4,3,4),(5,4,1),(6,3,5);",
    )
    .unwrap();
    std::fs::write(
        dir.join("net.model"),
        "ref /hops/*::from_id --> /stations/*[::id = $] as from;\nref /hops/*::to_id --> /stations/*[::id = $] as to;\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(success, "{args:?}: {err}");
    out
}

fn refused(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(!success, "{args:?} answered: {out}");
    err
}

#[test]
fn a_push_body_ends_in_a_value() {
    let d = fixture("push");
    let e = refused(&d, &["/books/* | .t(/*) | $.t", "books.json"]);
    assert!(
        e.contains("a push body ends in a value, and this one ends in navigation"),
        "{e}"
    );
    assert!(
        e.contains(".t(/manager::name)") && e.contains("/manager | .t"),
        "{e}"
    );
    assert_eq!(
        ok(&d, &["/books/* | .m(::title) | $.m @| count", "books.json"]),
        "3"
    );
    assert_eq!(
        ok(
            &d,
            &["/books/* | .n(/* @| count) | $.n @| first(1)", "books.json"]
        ),
        "3"
    );
}

#[test]
fn the_reach_markers_keep_the_equidistant_set() {
    let d = fixture("proximal");
    // from Anhalter the first round reaches Borsig and Colonnaden:
    // the proximal walk keeps both (ruling 2026-11-07, the 0.53.0
    // reading); one node is `@| [1]` after the reach
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "net.model",
                "/stations/1 | (<-from->to)+?::name",
                "net.db"
            ]
        ),
        "Borsig\nColonnaden"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "net.model",
                "/stations/1 | (<-from->to)+?::name @| [1]",
                "net.db"
            ]
        ),
        "Borsig"
    );
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "net.model",
                "/stations/1 | (<-from->to)+[::name = \"Dammtor\"]?::name",
                "net.db"
            ]
        ),
        "Dammtor"
    );
    // the equidistant set is the definite quantifier's as well
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "net.model",
                "/stations/1 | (<-from->to){1}::name @| count",
                "net.db"
            ]
        ),
        "2"
    );
    // the distal keeps every survivor of the last round
    let distal = ok(
        &d,
        &[
            "--model",
            "net.model",
            "/stations/1 | (<-from->to)+!::name @| count",
            "net.db",
        ],
    );
    assert!(distal.parse::<usize>().unwrap() >= 1, "{distal}");
}

#[test]
fn an_operation_over_unlike_operands_refuses_and_a_missing_reading_stays_null() {
    let d = fixture("refuse");
    let e = refused(&d, &["= 1_km + 1_s"]);
    assert!(
        e.contains("quantities of unlike dimensions (m and s)"),
        "{e}"
    );
    let e = refused(&d, &["= 2024-01-01 + 2024-01-02"]);
    assert!(
        e.contains("the sum is not defined for an instant and an instant"),
        "{e}"
    );
    let e = refused(&d, &["= 2024-01-01 * 2"]);
    assert!(
        e.contains("the product is not defined for an instant and a number"),
        "{e}"
    );
    let e = refused(&d, &["= @(1; 2) + @(1; 2; 3)"]);
    assert!(e.contains("differ in shape"), "{e}");
    assert_eq!(ok(&d, &["= 2024-01-01 + \"abc\""]), "null");
    assert_eq!(ok(&d, &["= \"abc\" + 1"]), "null");
    assert_eq!(ok(&d, &["= 1_km + 1_m"]), "1.001_km");
    assert_eq!(ok(&d, &["= 2024-01-02 - 2024-01-01"]), "1d");
}

#[test]
fn a_whole_float_keeps_its_point() {
    let d = fixture("float");
    assert_eq!(ok(&d, &["= 2.0"]), "2.0");
    assert_eq!(ok(&d, &["= 4 div 2"]), "2.0");
    assert_eq!(ok(&d, &["= 1.5 + 0.5"]), "2.0");
    assert_eq!(ok(&d, &["= 2.0 | type"]), "float");
    assert_eq!(ok(&d, &["= @(2.0; 3) | json"]), "[2.0, 3]");
    assert_eq!(ok(&d, &["= 2.5"]), "2.5");
    assert_eq!(ok(&d, &["= 7 idiv 2"]), "3");
}

#[test]
fn the_driver_register_in_the_drivers_pipeline_is_its_own_node() {
    let d = fixture("driver");
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* <=> /books/*[::shelf = $$0::shelf] @| [..1] | $$0::title",
                "books.json"
            ]
        ),
        "The Histories"
    );
}

#[test]
fn a_forking_interpolation_is_refused() {
    let d = fixture("interp");
    let e = refused(&d, &["= \"${/books/*::title}\"", "books.json"]);
    assert!(
        e.contains("takes one value, and the expression forks into 3"),
        "{e}"
    );
    assert!(e.contains("@| join"), "{e}");
    assert_eq!(
        ok(
            &d,
            &["= \"${/books/*::title @| join(', ')}\"", "books.json"]
        ),
        "The Histories, Dune, Emma"
    );
    assert_eq!(
        ok(&d, &["/books/* | \"${::title}!\" @| count", "books.json"]),
        "3"
    );
}

#[test]
fn a_path_after_an_aggregation_pipe_warns_that_it_restarted_from_the_root() {
    let d = fixture("warn");
    let (success, out, err) = qua(
        &d,
        &[
            "--diagnostics=warning",
            "/books/* @| count | .n | /books/*::title @| count",
            "books.json",
        ],
    );
    assert!(success, "{err}");
    assert_eq!(out, "3");
    assert!(err.contains("NAVIGATION_RESTARTED"), "{err}");
    assert!(
        err.contains("restarted from the root after the aggregation pipe"),
        "{err}"
    );
}

#[test]
fn a_bare_read_at_the_head_has_nothing_to_project_from() {
    let d = fixture("head");
    std::fs::write(
        d.join("page.html"),
        "<html lang=\"en\"><body><p>x</p></body></html>",
    )
    .unwrap();
    for q in ["= ::lang", "= :name", "= $_", "= ::lang<text>"] {
        let e = refused(&d, &[q, "page.html"]);
        assert!(e.contains("nothing to project from"), "{q}: {e}");
    }
    // a path as the operand is a subquery from the root; `^` is redundant
    assert_eq!(ok(&d, &["= /html::lang", "page.html"]), "en");
    assert_eq!(ok(&d, &["= ^/html::lang", "page.html"]), "en");
    // the projector stays an operand of the commands that take it
    assert_eq!(
        ok(&d, &["/books/* @| max(--of=::year)", "books.json"]),
        "1965"
    );
}

#[test]
fn a_quoted_push_name_pushes_and_recalls() {
    let d = fixture("pushname");
    assert_eq!(ok(&d, &["= 1 | .\"3\"(2) | $.\"3\""]), "2");
    assert_eq!(ok(&d, &["= 1 | .\"x y\"(2) | $.\"x y\""]), "2");
    assert_eq!(ok(&d, &["= 1 | .\"3\"(2) | $.1"]), "2");
    let e = refused(&d, &["= 1 | .3(2) | $.3"]);
    assert!(e.contains("quote any other ('| .\"3\"')"), "{e}");
}

#[test]
fn a_record_key_is_found_by_value() {
    let d = fixture("keys");
    assert_eq!(ok(&d, &["= %(1.0 = \"a\") | $_[[1]]"]), "a");
    assert_eq!(ok(&d, &["= %(1 = \"a\") | $_[[1.0]]"]), "a");
    assert_eq!(ok(&d, &["= %(1.0 = \"a\") | :::keys"]), "@(1.0)");
    // a number and its text are two keys
    assert_eq!(
        ok(&d, &["= %(1 = \"a\"; \"1\" = \"b\") | $_[[\"1\"]]"]),
        "b"
    );
    assert_eq!(ok(&d, &["= %(1 = \"a\"; \"1\" = \"b\") | $_[[1]]"]), "a");
}

#[test]
fn a_capture_binds_across_the_steps_later_predicates() {
    let d = fixture("captures");
    // the conjunction's binding is the step's: a group a pattern
    // comparison makes reads as `$1` in a later predicate, across a
    // positional one too
    assert_eq!(
        ok(
            &d,
            &[
                r#"/books/*[::title == (/^(\w)/)][$1 = "E"] | ::title"#,
                "books.json"
            ]
        ),
        "Emma"
    );
    assert_eq!(
        ok(
            &d,
            &[
                r#"/books/*[::title == (/^(\w)/)][1][$1 = "T"] | ::title"#,
                "books.json"
            ]
        ),
        "The Histories"
    );
    assert_eq!(
        ok(
            &d,
            &[
                r#"/books/*[::title == (/^(\w)/)][1][$1 = "E"] | ::title"#,
                "books.json"
            ]
        ),
        ""
    );
}

#[test]
fn a_unions_branch_without_a_projector_answers_its_nodes() {
    let d = fixture("union");
    assert_eq!(
        ok(&d, &["/books/1 || /books/*::title", "books.json"]),
        "/books/1\nThe Histories\nDune\nEmma"
    );
    assert_eq!(
        ok(&d, &["/books/*::title || /books/1", "books.json"]),
        "The Histories\nDune\nEmma\n/books/1"
    );
}

#[test]
fn a_quantified_group_after_a_pipe_walks_with_its_facts() {
    let d = fixture("group-stage");
    let m = ["--model", "net.model"];
    let glued = ok(
        &d,
        &[
            &m[..],
            &[
                "/hops/*[::id = 1](::to_id-->)+ | %(h = $-:hops; n = ::name)",
                "net.db",
            ],
        ]
        .concat(),
    );
    let piped = ok(
        &d,
        &[
            &m[..],
            &[
                "/hops/*[::id = 1] | (::to_id-->)+ | %(h = $-:hops; n = ::name)",
                "net.db",
            ],
        ]
        .concat(),
    );
    assert_eq!(piped, glued);
    assert_eq!(piped, r#"%(h = 1; n = "Borsig")"#);
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/hops/*[::id = 1] | (::to_id-->) | ::name", "net.db"]
            ]
            .concat()
        ),
        "Borsig"
    );
}

#[test]
fn the_ordinal_in_a_regula_folds_reducing_command_is_the_groups() {
    let d = fixture("fold-ordinal");
    let groups: usize = ok(
        &d,
        &["/books/* | .s(::shelf) @|{.s} count @| count", "books.json"],
    )
    .parse()
    .unwrap();
    let ordinals: Vec<String> = (1..=groups).map(|i| format!("%(i = {i})")).collect();
    assert_eq!(
        ok(
            &d,
            &["/books/* | .s(::shelf) @|{.s} %(i = $ord)", "books.json"]
        )
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>(),
        ordinals
    );
    // The group's ordinal in every position alike (ruling
    // 2026-11-10): the bare expression answered 1, 1, 1 and the
    // aggregate push with a selector null.
    let numbers: Vec<String> = (1..=groups).map(|i| i.to_string()).collect();
    assert_eq!(
        ok(&d, &["/books/* | .s(::shelf) @|{.s} ($ord)", "books.json"])
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        numbers
    );
    // each book is filed its shelf's ordinal: A, A, B
    assert_eq!(
        ok(
            &d,
            &["/books/* | .s(::shelf) | .i@{.s}($ord) | $.i", "books.json"]
        ),
        "1\n1\n2"
    );
    // A reducing command without a fold stands for the one group
    // there is: 1, in the record, the bare expression and the
    // aggregate push alike. (The record answered null.)
    assert_eq!(
        ok(&d, &["/books/* @| %(i = $ord)", "books.json"]),
        "%(i = 1)"
    );
    assert_eq!(ok(&d, &["/books/* @| ($ord)", "books.json"]), "1");
    assert_eq!(
        ok(&d, &["/books/* | .i@($ord) | $.i", "books.json"]),
        "1\n1\n1"
    );
    // a nested pipeline is its own ground: its reducing command
    // stands for its own one group, not the enclosing fold's
    assert_eq!(
        ok(
            &d,
            &[
                "/books/* | .s(::shelf) @|{.s} %(i = $ord; j = (@(7; 8; 9) @| %(k = $ord)))",
                "books.json"
            ]
        ),
        "%(i = 1; j = %(k = 1))\n%(i = 2; j = %(k = 1))"
    );
    // the thread's own position is what a row-wise command reads
    assert_eq!(ok(&d, &["/books/* | ($ord)", "books.json"]), "1\n2\n3");
}

#[test]
fn two_quantities_multiply_and_divide_into_a_compound_unit() {
    let d = fixture("quantities");
    assert_eq!(ok(&d, &["= 10_km div 2_h"]), "5_km/h");
    assert_eq!(ok(&d, &["= 2_m * 3_m"]), "6_m^2");
    assert_eq!(ok(&d, &["= 6_m^2 div 2_m"]), "3_m");
    assert_eq!(ok(&d, &["= 10_km div 2_h * 2_h"]), "10_km");
    assert_eq!(ok(&d, &["= 1_EUR div 2_kg"]), "0.5_EUR/kg");
    assert_eq!(ok(&d, &["= 10_km div 2_h | dimension"]), "L/T");
    // a quotient of one dimension is a bare number
    assert_eq!(ok(&d, &["= 10_km div 2_km"]), "5.0");
    assert_eq!(ok(&d, &["= 10_km div 2_m"]), "5000.0");
    assert_eq!(ok(&d, &["= 10_km div 0_h"]), "null");
}

#[test]
fn the_join_candidate_is_read_bare_or_as_the_position() {
    // `$*` is retired (ruling 2026-11-11): a node is `$$`, a value `$`
    let d = fixture("join-candidate");
    for q in [
        "/books/* | $*",
        "/books/* <=> /books/*[::shelf = $$0::shelf && $*::year > 0] @| count",
    ] {
        let e = refused(&d, &[q, "books.json"]);
        assert!(
            e.contains(
                "'$*' is retired: a join condition reads its candidate bare ('::limit') \
                 or as the position ('$$_::limit')"
            ),
            "{q}: {e}"
        );
    }
    // the reserved star spellings went with it: none is a spelling
    // of the language, and each refusal names what exists
    let e = refused(&d, &["/books/* | @@*", "books.json"]);
    assert!(
        e.contains("'@@*' is retired: the context's positions are '@@_'"),
        "{e}"
    );
    let e = refused(&d, &["/books/* | $*[1]", "books.json"]);
    assert!(e.contains("'$*' is retired"), "{e}");
    for q in ["/books/* | $$*", "/books/* | $$*[1]"] {
        let e = refused(&d, &[q, "books.json"]);
        assert!(
            e.contains("'$$_' is the position") && e.contains("'$$N' a bound node"),
            "{q}: {e}"
        );
    }
    for q in [
        "/books/* <=> /books/*[::shelf = $$0::shelf && ::year > 0] @| count",
        "/books/* <=> /books/*[::shelf = $$0::shelf && $$_::year > 0] @| count",
    ] {
        assert_eq!(ok(&d, &[q, "books.json"]), "3", "{q}");
    }
}

#[test]
fn a_mounted_documents_missing_location_yields_nothing() {
    let d = fixture("location-miss");
    std::fs::write(
        d.join("a.html"),
        r##"<html><body><a id="x" href="b.html#there">to b</a><a id="y" href="b.html#nowhere">miss</a></body></html>"##,
    )
    .unwrap();
    std::fs::write(
        d.join("b.html"),
        r#"<html><body><p id="there">found</p></body></html>"#,
    )
    .unwrap();
    assert_eq!(
        ok(&d, &["//a | ::href--> | ::id", "a.html", "b.html"]),
        "there"
    );
    assert_eq!(
        ok(
            &d,
            &[
                r#"//a[::id = "y"] | ::href--> @| count"#,
                "a.html",
                "b.html"
            ]
        ),
        "0"
    );
}

#[test]
fn a_walk_returning_to_a_node_under_the_acyclic_promise_refuses() {
    let d = fixture("acyclic");
    std::fs::write(
        d.join("cyc.json"),
        r#"{"rows": [{"id": 1, "v": "a", "next": 2}, {"id": 2, "v": "b", "next": 1}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("cyc.model"),
        "ref /rows/*::next --> /rows/*[::id = $] acyclic;\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &[
            "--model",
            "cyc.model",
            "/rows/*[::id = 2](::next-->)+ | ::v",
            "cyc.json",
        ],
    );
    assert!(
        e.contains("a cycle under the promise 'ref /rows/*::next --> /rows/* acyclic'"),
        "{e}"
    );
    std::fs::write(
        d.join("free.model"),
        "ref /rows/*::next --> /rows/*[::id = $];\n",
    )
    .unwrap();
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "free.model",
                "/rows/*[::id = 2](::next-->)+ | ::v",
                "cyc.json"
            ]
        ),
        "a"
    );
}

#[test]
fn a_constructor_sees_the_traits_declared_before_it_and_a_later_declaration_refuses() {
    let d = fixture("declaration-order");
    std::fs::write(
        d.join("rows.json"),
        r#"{"rows": [{"id": 1, "v": "a"}, {"id": 2, "v": "b"}, {"id": 3, "v": "c"}]}"#,
    )
    .unwrap();
    std::fs::write(
        d.join("first.model"),
        "trait <small>: *[::id < 3];\nnode /t/x: /rows/*<small>::v;\n",
    )
    .unwrap();
    assert_eq!(
        ok(
            &d,
            &["--model", "first.model", "/t/* @| count", "rows.json"]
        ),
        "2"
    );
    std::fs::write(
        d.join("later.model"),
        "node /t/x: /rows/*<small>::v;\ntrait <small>: *[::id < 3];\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &["--model", "later.model", "/t/* @| count", "rows.json"],
    );
    assert!(
        e.contains("its filter <small> names a trait declared later"),
        "{e}"
    );
    std::fs::write(
        d.join("forward.model"),
        "node /a/x: /b/*::name;\nnode /b/y: /rows/*::v;\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &["--model", "forward.model", "/a/* @| count", "rows.json"],
    );
    assert!(
        e.contains("its path /b names a container declared later"),
        "{e}"
    );
}

#[test]
fn a_prefix_on_a_unit_that_does_not_take_it_is_refused() {
    let d = fixture("prefixes");
    assert!(refused(&d, &["= 1_Kim"]).contains("'Ki' is a binary prefix"));
    assert!(refused(&d, &["= 1_mB"]).contains("no fraction of a 'B'"));
    assert!(refused(&d, &["= 1_kh"]).contains("'h' takes no prefix"));
    assert_eq!(ok(&d, &["= 1_km | dimension"]), "L");
    assert_eq!(ok(&d, &["= 1_KiB | dimension"]), "B");
    assert_eq!(ok(&d, &["= 1_min | dimension"]), "T");
    assert_eq!(ok(&d, &["= 1_nm | dimension"]), "L");
}

#[test]
fn the_edge_carries_one_label() {
    let d = fixture("labels");
    let m = ["--model", "net.model"];
    // an edge is one element with one label (spec: the edge model,
    // ruling 2026-11-07): `$-:label` is that label, what bare `$-`
    // answers; a reference reached under two labels is two edges.
    // The label of a resolution's edge is the reference label, the
    // property's name without its `_id` (draft.186, and the
    // resolution's formalia since 2026-11-10): the edge read after
    // the step said the full name, which its own predicate did not
    // match (the review's graph T1)
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/hops/*[::id = 1] | ::to_id--> | $-:label", "net.db"]
            ]
            .concat()
        ),
        "to"
    );
    assert_eq!(
        ok(
            &d,
            &[&m[..], &["/hops/*[::id = 1] | ::to_id--> | $-", "net.db"]].concat()
        ),
        "to"
    );
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &[
                    "/hops/*[::id = 1] | ::to_id-->[$- = \"to\"] @| count",
                    "net.db"
                ]
            ]
            .concat()
        ),
        "1"
    );
    let e = refused(&d, &["/hops/*[::id = 1] | ::to_id--> | $-:depth", "net.db"]);
    assert!(
        e.contains("its label ':label' and its weights ':weights'"),
        "{e}"
    );
    let e = refused(
        &d,
        &["/hops/*[::id = 1] | ::to_id--> | $-:labels", "net.db"],
    );
    assert!(e.contains("':label'"), "{e}");
}

#[test]
fn an_uncarried_locale_is_refused_naming_the_carried_ones() {
    let d = fixture("locales");
    let e = refused(&d, &[r#"= "1,5" | number(xx)"#]);
    assert!(
        e.contains("no number conventions for locale \"xx\"") && e.contains("carried: en, zh"),
        "{e}"
    );
    assert_eq!(ok(&d, &[r#"= "1,5" | number(de)"#]), "1.5");
}

/// Ruling 2026-11-10: `datetime` takes no operand (a locale named
/// there read nothing), and the refusal points at `strptime`.
#[test]
fn datetime_takes_no_operand() {
    let d = fixture("datetime-operand");
    assert_eq!(ok(&d, &[r#"= "2024-01-01" | datetime"#]), "2024-01-01");
    for q in [
        r#"= "2024-01-01" | datetime(ru)"#,
        r#"= "2024-01-01" | datetime(xx)"#,
        r#"= datetime("2024-01-01"; ru)"#,
    ] {
        let e = refused(&d, &[q]);
        assert!(
            e.contains("'datetime' takes no operand") && e.contains("strptime(fmt; LOCALE)"),
            "{q}: {e}"
        );
    }
    // the parser that reads a locale's words still does
    assert_eq!(
        ok(&d, &[r#"= "15 февраля 2024" | strptime("%d %B %Y"; ru)"#]),
        "2024-02-15"
    );
}

/// Ruling 2026-11-10: `isoformat` prints the `Z`, for every instant.
#[test]
fn isoformat_prints_the_z() {
    let d = fixture("isoformat-z");
    assert_eq!(
        ok(&d, &["= 2024-02-15T10:00:00+02:00 | isoformat"]),
        "2024-02-15T08:00:00Z"
    );
    assert_eq!(
        ok(&d, &["= 2024-02-15T10:00:00 | isoformat"]),
        "2024-02-15T10:00:00Z"
    );
    // the text reads back as the same instant
    assert_eq!(
        ok(
            &d,
            &["= 2024-02-15T10:00:00+02:00 | isoformat | datetime | epoch"]
        ),
        ok(&d, &["= 2024-02-15T10:00:00+02:00 | epoch"])
    );
}

#[test]
fn a_used_models_definitions_join_the_users() {
    let d = fixture("use-defs");
    std::fs::write(
        d.join("defs.model"),
        "def &shelved($s): /books/*[::shelf = $s]::title;\nmacro &two($x): = $x + 2;\n",
    )
    .unwrap();
    std::fs::write(d.join("user.model"), "use defs.model;\n").unwrap();
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "user.model",
                r#"&shelved("A") @| count"#,
                "books.json"
            ]
        ),
        "2"
    );
    assert_eq!(
        ok(&d, &["--model", "user.model", "= &two(1)", "books.json"]),
        "3"
    );
}

#[test]
fn a_plain_map_leaves_an_indexed_tables_index_column_alone() {
    let d = fixture("map-index");
    assert_eq!(
        ok(&d, &["= #(%(a = 1); %(a = 2)) | autoindex | map(* 10)"]),
        "#(@(ix; a);\n  %(1; 10);\n  %(2; 20))"
    );
    // the index is reached through `--to=:::index` alone
    assert_eq!(
        ok(
            &d,
            &["= #(%(a = 1); %(a = 2)) | autoindex | map(* 10; --to=:::index)"]
        ),
        "#(@(ix; a);\n  %(10; 1);\n  %(20; 2))"
    );
    assert_eq!(
        ok(&d, &["= #(%(a = 1); %(a = 2)) | map(* 10)"]),
        "#(@(a);\n  %(10);\n  %(20))"
    );
}

#[test]
fn a_sum_of_integer_looking_text_is_an_integer() {
    let d = fixture("text-sum");
    std::fs::write(d.join("fares.csv"), "n,s\n310,a\n1730,b\n").unwrap();
    // the reducers read a text as the operators do: `"310" + 1` is 311
    assert_eq!(ok(&d, &["/row::n @| sum", "fares.csv"]), "2040");
    assert_eq!(ok(&d, &["/row::n @| product", "fares.csv"]), "536300");
    assert_eq!(ok(&d, &["/row | ::n + 1", "fares.csv"]), "311\n1731");
    // a mean is a float, whole or not
    assert_eq!(ok(&d, &["/row::n @| avg", "fares.csv"]), "1020.0");
    std::fs::write(d.join("mixed.csv"), "n\n1.5\n2\n").unwrap();
    assert_eq!(ok(&d, &["/row::n @| sum", "mixed.csv"]), "3.5");
}

/// Ruling 2026-11-10: a null key matches nothing in a join
/// condition, whatever the comparison; `=` is unchanged elsewhere.
#[test]
fn a_null_key_matches_nothing_in_a_join_condition() {
    let d = fixture("join-null-key");
    std::fs::write(
        d.join("shop.json"),
        r#"{"orders": [
              {"id": 1, "customer_email": "ada@example.org", "phone": "111"},
              {"id": 2, "customer_email": null, "phone": "222"},
              {"id": 3, "phone": null},
              {"id": 4, "customer_email": "nobody@example.org", "phone": "333"}],
            "customers": [
              {"name": "Ada", "email": "ada@example.org", "phone": "111"},
              {"name": "Babbage", "email": null, "phone": "222", "vip": true},
              {"name": "Carroll", "phone": null}]}"#,
    )
    .unwrap();
    // order:customer pairs, one per line of the answer
    let pairs = |on: &str, join: &str| {
        ok(
            &d,
            &[
                &format!(r#"/orders/* {join} /customers/*{on} | "${{::id}}:${{$$1::name}}""#),
                "shop.json",
            ],
        )
        .replace('\n', " ")
    };
    // two absent keys do not pair: orders 2 and 3 find no customer
    assert_eq!(pairs("[::email = $$0::customer_email]", "<=>"), "1:Ada");
    // the outer join keeps the driver, its slot null
    assert_eq!(
        pairs("[::email = $$0::customer_email]", "<=>?"),
        "1:Ada 2: 3: 4:"
    );
    // every operator: `!=` does not pair an absent key with every node
    assert_eq!(pairs("[::email != $$0::customer_email]", "<=>"), "4:Ada");
    // unknown passes through a negation
    assert_eq!(pairs("[!(::email = $$0::customer_email)]", "<=>"), "4:Ada");
    // and yields to a true alternative: order 2 pairs by its phone
    assert_eq!(
        pairs(
            "[::email = $$0::customer_email || ::phone = $$0::phone]",
            "<=>"
        ),
        "1:Ada 2:Babbage"
    );
    // a comparison that reads one side alone keeps today's `=`:
    // `::vip = null` holds for the customers with no such property
    assert_eq!(
        pairs("[::phone = $$0::phone && ::vip = null]", "<=>"),
        "1:Ada"
    );
    // so does a plain predicate of the joined expression
    assert_eq!(
        pairs("[::email = null][::phone = $$0::phone]", "<=>"),
        "2:Babbage"
    );
    // and `=` outside a join condition
    assert_eq!(
        ok(
            &d,
            &["/orders/*[::customer_email = null] | ::id", "shop.json"]
        )
        .replace('\n', " "),
        "2 3"
    );
}

/// A bare `null` in a row of a matrix literal is the missing cell,
/// not the text "null" (lang's finding, 2026-11-10): the column keeps
/// its type, as in the positional-record spelling of the same table.
#[test]
fn a_bare_null_in_a_matrix_row_is_the_null() {
    let d = fixture("matrix-null");
    assert_eq!(
        ok(&d, &["= #(@(1; null); @(3; 4)) | header"]),
        "@(:1<*; int>; :2<*; int>)"
    );
    assert_eq!(ok(&d, &["= #(@(1; null); @(3; 4)) | $_[1; 2]"]), "null");
    assert_eq!(
        ok(&d, &["= #(@(1; 2); @(3; null)) | header"]),
        ok(&d, &["= #(%(1; 2); %(3; null)) | header"])
    );
    // a header of names is untouched: the record rows carry the null
    assert_eq!(
        ok(&d, &[r#"= #(@(name; age); %("Braund"; null)) | $_[1; 2]"#]),
        "null"
    );
}

/// `--table`, an alias of `--pretty` through 0.37, is refused with
/// the pointer.
#[test]
fn the_table_flag_is_retired() {
    let d = fixture("table-flag");
    let e = refused(&d, &["--table", "= 1"]);
    assert!(
        e.contains("'--table' is retired") && e.contains("--pretty"),
        "{e}"
    );
}

fn review_items(tag: &str) -> PathBuf {
    let d = fixture(tag);
    std::fs::write(
        d.join("items.json"),
        r#"{"items": [{"n": "x12", "k": null}, {"n": "y7", "k": "a"}, {"n": "zz", "k": null}],
            "keys": [{"c": null, "v": "N"}, {"c": "a", "v": "A"}]}"#,
    )
    .unwrap();
    d
}

/// Ruling 2026-11-10: a pattern arm of the switch binds its
/// groups for its own result, and for nothing else.
#[test]
fn a_pattern_arm_reads_its_own_captures() {
    let d = review_items("arm-captures");
    assert_eq!(
        ok(
            &d,
            &[
                r#"/items/* | (::n ?= (/x(\d+)/) ? $1 : (/y(\d+)/) ? "y" ++ $1 : "no")"#,
                "items.json"
            ]
        ),
        "12\ny7\nno"
    );
    // the thread's own captures stand in the else and after the
    // conditional: the filter's group for y7, the arm's for x12
    assert_eq!(
        ok(
            &d,
            &[
                r#"/items/* | [::n == (/(\w)\d/)] | (::n ?= (/x(\d+)/) ? $1 : $1) ++ "/" ++ $1"#,
                "items.json"
            ]
        ),
        "12/x\ny/y"
    );
}

/// The value match `=>` pairs no null key with a null cell (spec:
/// Value Match; the join's condition got the same rule in 520b7542).
#[test]
fn a_null_key_matches_nothing_in_a_value_match() {
    let d = review_items("value-match-null");
    assert_eq!(
        ok(&d, &["/items/*::k=>(/keys/*::c) | ::v", "items.json"]),
        "A"
    );
}

/// Ruling 2026-11-10: attached regulae agree by the value equality
/// of `=`, not by the value's structure.
#[test]
fn attached_regulae_agree_as_equals_compares() {
    let d = review_items("attached-equal");
    let attach = |values: &str| {
        qua(
            &d,
            &[
                &format!(
                    "/items/* | .i(:index) | ^/keys/*[1] | .:t(($.i = 1 ? {values})) | :.t @| count"
                ),
                "items.json",
            ],
        )
    };
    for values in [
        "1 : 1.0",
        "1_km : 1000_m",
        "2024-02-15T10:00:00+02:00 : 2024-02-15T08:00:00Z",
    ] {
        let (success, out, err) = attach(values);
        assert!(success && out == "3", "{values}: {out} {err}");
    }
    let (success, _, err) = attach("1 : 2");
    assert!(
        !success && err.contains("attach differing values under 't' (1 and 2)"),
        "{err}"
    );
}

/// Ruling 2026-11-10: a double-quoted string has four escapes, and
/// the backslash before a backtick is no longer one. The printer
/// escapes the rounded hole's opener of today, `(_`.
#[test]
fn four_escapes_in_a_double_quoted_string() {
    let d = fixture("escapes");
    assert_eq!(ok(&d, &[r#"= "a\`b\$c\(d\\e\"f""#]), r#"a\`b$c(d\e"f"#);
    assert_eq!(ok(&d, &[r#"= "a\nb""#]), r"a\nb");
    assert_eq!(ok(&d, &["= @('n(_1 + 1)')"]), r#"@("n\(_1 + 1)")"#);
    assert_eq!(ok(&d, &[r#"= @("n\(_1 + 1)")"#]), r#"@("n\(_1 + 1)")"#);
}

/// The strict expansion refuses a null (and splices an empty text):
/// its refusal says so.
#[test]
fn the_strict_expansion_refuses_a_null() {
    let d = fixture("strict-null");
    let e = refused(&d, &[r#"= "${null:?}""#]);
    assert!(
        e.contains("the strict expansion '${null:?}' is null"),
        "{e}"
    );
    assert_eq!(ok(&d, &[r#"= "a${"":?}b""#]), "ab");
}
