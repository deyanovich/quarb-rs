//! The edge model's weights (spec: Weights; Weight Potentials, rulings
//! 2026-11-07): declared by a model's `weights(…)` clause or a
//! `weight-potential`, read by a budget and `$-:weights`, never
//! inferred from the edge's properties.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-weights-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("roads.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE stations (id INTEGER PRIMARY KEY, name TEXT, opened TEXT);
         CREATE TABLE stages (id INTEGER PRIMARY KEY, from_id INTEGER, to_id INTEGER,
                              distance TEXT, days TEXT, stops INTEGER);
         INSERT INTO stations VALUES (1,'Sardis','2024-01-01T00:00:00Z'),
                                     (2,'Susa','2024-01-03T00:00:00Z'),
                                     (3,'Ecbatana','2024-01-05T00:00:00Z');
         INSERT INTO stages VALUES (1,1,2,'18 parasang','3 d',3),
                                   (2,2,3,'94.5 parasang','18.9 d',20);",
    )
    .unwrap();
    std::fs::write(
        dir.join("roads.model"),
        "unit parasang, parasangs = 5550 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::distance; ::days);\n",
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
fn declared_weights_feed_the_budget_and_the_weights_fact() {
    let d = fixture("declared");
    let m = ["--model", "roads.model"];
    // the weights, in the canonical dimension order (length before time)
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/stations/*[::id = 1] | ->stage | $-:weights", "roads.db"]
            ]
            .concat()
        ),
        "@(18_parasang; 3_d)"
    );
    // a budget reads the weight of its dimension
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &[
                    "/stations/*[::id = 1](->stage){;100_parasang} | ::name",
                    "roads.db"
                ]
            ]
            .concat()
        ),
        "Sardis\nSusa"
    );
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &[
                    "/stations/*[::id = 1](->stage){;120_parasang} | ::name",
                    "roads.db"
                ]
            ]
            .concat()
        ),
        "Sardis\nSusa\nEcbatana"
    );
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/stations/*[::id = 1](->stage){;30_d} @| count", "roads.db"]
            ]
            .concat()
        ),
        "3"
    );
    // a dimension no weight carries: nothing
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &[
                    "/stations/*[::id = 1](->stage){;100_EUR} @| count",
                    "roads.db"
                ]
            ]
            .concat()
        ),
        "1"
    );
    // the graph library reads the same set by unit
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &[
                    "/stations/* @| graph:distance(->stage; --from=/stations/*[::id = 1] --weight=parasang) @| [2]",
                    "roads.db"
                ]
            ]
            .concat()
        ),
        "18_parasang"
    );
}

#[test]
fn a_property_is_no_weight_unless_declared() {
    let d = fixture("undeclared");
    std::fs::write(
        d.join("plain.model"),
        "unit parasang, parasangs = 5550 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage;\n",
    )
    .unwrap();
    let m = ["--model", "plain.model"];
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/stations/*[::id = 1] | ->stage | $-:weights", "roads.db"]
            ]
            .concat()
        ),
        "@()"
    );
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &[
                    "/stations/*[::id = 1](->stage){;100_parasang} @| count",
                    "roads.db"
                ]
            ]
            .concat()
        ),
        "1"
    );
    // the property form of the weight option is retired
    let e = refused(
        &d,
        &[
            &m[..],
            &[
                "/stations/* @| graph:distance(->stage; --weight=::stops)",
                "roads.db",
            ],
        ]
        .concat(),
    );
    assert!(e.contains("a property is no weight"), "{e}");
}

#[test]
fn weights_are_checked_at_load() {
    let d = fixture("checks");
    for (name, clause, expect) in [
        ("bare", "weights(::stops)", "names no quantity"),
        (
            "dimless",
            "weights(::distance; ::distance)",
            "names '::distance' twice",
        ),
    ] {
        std::fs::write(
            d.join(format!("{name}.model")),
            format!(
                "unit parasang, parasangs = 5550 m;\n\
                 ref /stages/*::from_id --> /stations/*[::id = $];\n\
                 ref /stages/*::to_id --> /stations/*[::id = $];\n\
                 edge /stages/*: ::from_id -- ::to_id as stage {clause};\n"
            ),
        )
        .unwrap();
        let e = refused(
            &d,
            &[
                "--model",
                &format!("{name}.model"),
                "/stations/* @| count",
                "roads.db",
            ],
        );
        assert!(e.contains(expect), "{name}: {e}");
    }
    std::fs::write(
        d.join("same.model"),
        "unit parasang, parasangs = 5550 m;\n\
         unit league, leagues = 4828 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::distance; ::leagues);\n",
    )
    .unwrap();
    let db = rusqlite::Connection::open(d.join("roads.db")).unwrap();
    db.execute_batch(
        "ALTER TABLE stages ADD COLUMN leagues TEXT; UPDATE stages SET leagues = '20 league';",
    )
    .unwrap();
    let e = refused(
        &d,
        &["--model", "same.model", "/stations/* @| count", "roads.db"],
    );
    assert!(e.contains("share the dimension"), "{e}");
}

#[test]
fn a_weight_potential_weighs_an_edge_by_the_difference_of_its_ends() {
    let d = fixture("potential");
    std::fs::write(
        d.join("timed.model"),
        "unit parasang, parasangs = 5550 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::distance);\n\
         weight-potential /stations/*::opened;\n",
    )
    .unwrap();
    let m = ["--model", "timed.model"];
    // Sardis → Susa: two days later; the duration joins the set
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/stations/*[::id = 1] | ->stage | $-:weights", "roads.db"]
            ]
            .concat()
        ),
        "@(18_parasang; 2d)"
    );
    // walked against the edge, the sign flips
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/stations/*[::id = 2] | <-stage | $-:weights", "roads.db"]
            ]
            .concat()
        ),
        "@(18_parasang; -2d)"
    );
    // the budget on the potential bounds the ends' difference
    assert_eq!(
        ok(
            &d,
            &[
                &m[..],
                &["/stations/*[::id = 1](->stage){;60_h} | ::name", "roads.db"]
            ]
            .concat()
        ),
        "Sardis\nSusa"
    );
    // a potential may not share a dimension with a declared weight
    std::fs::write(
        d.join("clash.model"),
        "unit parasang, parasangs = 5550 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::days);\n\
         weight-potential /stations/*::opened;\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &["--model", "clash.model", "/stations/* @| count", "roads.db"],
    );
    assert!(e.contains("already a declared weight's"), "{e}");
}

#[test]
fn the_braces_stack_and_a_rule_folds_the_legs() {
    let d = fixture("rules");
    let m = ["--model", "roads.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "roads.db"]].concat());
    let no = |query: &str| refused(&d, &[&m[..], &[query, "roads.db"]].concat());
    // a count brace and a budget brace, each a condition of its own
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;1}{;120_parasang} | ::name"),
        "Sardis\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){1;}{;120_parasang} | ::name"),
        "Susa\nEcbatana"
    );
    // two budgets: the total is the last brace's, the costs every brace's
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;120_parasang}{;30_d} | ::name"),
        "Sardis\nSusa\nEcbatana"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;120_parasang}{;10_d} | ::name"),
        "Sardis\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;120_parasang}{;10_d} | $-:total"),
        "null\n3_d"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){1;}{;120_parasang} | $-:costs"),
        "@(1; 18_parasang)\n@(2; 112.5_parasang)"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){1;} | $-:costs"),
        "@(1)\n@(2)"
    );
    // max and min fold the extreme leg
    assert_eq!(
        q("/stations/*[::id = 1](->stage){max:;20_d} | ::name"),
        "Sardis\nSusa\nEcbatana"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){max:;10_d} | ::name"),
        "Sardis\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){min:;10_d} | $-:costs"),
        "null\n@(3_d)\n@(3_d)"
    );
    // a zero floor — the boundless idiom — keeps the written unit's
    // factor: days, not seconds labelled days
    assert_eq!(
        q("/stations/*[::id = 1](->stage){min:0_d;} | $-:total"),
        "null\n3_d\n3_d"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){max:0_parasang;} | $-:total"),
        "null\n18_parasang\n94.5_parasang"
    );
    // the refusals
    let e = no("/stations/*[::id = 1](->stage){;10_parasang}{;5_km} | ::name");
    assert!(e.contains("two braces measure m"), "{e}");
    let e = no("/stations/*[::id = 1](->stage){1;2}{3;4} | ::name");
    assert!(e.contains("two braces measure the hop count"), "{e}");
    let e = no("/stations/*[::id = 1](->stage){abs:3} | ::name");
    assert!(
        e.contains("is a cost function, and this brace counts hops"),
        "{e}"
    );
    let e = no("/stations/*[::id = 1](->stage)+{3} | ::name");
    assert!(e.contains("a brace never follows '+' or '*'"), "{e}");
    let e = no("/stations/*[::id = 1](->stage){quux:;3_d} | ::name");
    assert!(e.contains("names no cost function"), "{e}");
}

#[test]
fn a_signed_weight_takes_a_rule() {
    let d = fixture("signed");
    std::fs::write(
        d.join("timed.model"),
        "unit parasang, parasangs = 5550 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::distance);\n\
         weight-potential /stations/*::opened;\n",
    )
    .unwrap();
    let m = ["--model", "timed.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "roads.db"]].concat());
    // against the potential every leg is negative: the sum refuses
    let e = refused(
        &d,
        &[
            &m[..],
            &["/stations/*[::id = 3](<-stage){;60_h} | ::name", "roads.db"],
        ]
        .concat(),
    );
    assert!(e.contains("a negative weight"), "{e}");
    // abs and fall take the descent, rise finds none
    assert_eq!(
        q("/stations/*[::id = 3](<-stage){abs:;60_h} | ::name"),
        "Ecbatana\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 3](<-stage){fall:;60_h} | ::name"),
        "Ecbatana\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 3](<-stage){rise:;60_h} | ::name"),
        "Ecbatana\nSusa\nSardis"
    );
    assert_eq!(
        q("/stations/*[::id = 3](<-stage){abs:;60_h} | $-:costs"),
        "null\n@(48_h)"
    );
    // forward, max bounds the longest leg where the sum bounds the walk
    assert_eq!(
        q("/stations/*[::id = 1](->stage){max:;50_h} | ::name"),
        "Sardis\nSusa\nEcbatana"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;50_h} | ::name"),
        "Sardis\nSusa"
    );
}

#[test]
fn a_budget_floor_above_its_ceiling_is_refused_and_equal_bounds_stand() {
    // ruling 2026-11-08: the bounds compared after conversion to one
    // dimension, the inversion refused naming both; equal bounds
    // stand; the count form's pointer spells the brace with ';'
    let d = fixture("inverted");
    let m = ["--model", "roads.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "roads.db"]].concat());
    let no = |query: &str| refused(&d, &[&m[..], &[query, "roads.db"]].concat());
    let e = no("/stations/*[::id = 1](->stage){600_km;100_parasang} | ::name");
    assert!(
        e.contains(
            "the budget's floor '600_km' lies above its ceiling '100_parasang' (100_parasang is \
             555_km)"
        ),
        "{e}"
    );
    let e = no("/stations/*[::id = 1](->stage){4_d;72_h} | ::name");
    assert!(
        e.contains("'4_d' lies above its ceiling '72_h' (72_h is 3_d)"),
        "{e}"
    );
    // one unit on both sides: nothing to re-express
    let e = no("/stations/*[::id = 1](->stage){4_d;3_d} | ::name");
    assert!(
        e.contains("'4_d' lies above its ceiling '3_d': a budget"),
        "{e}"
    );
    // equal bounds, in one unit or two: the walk costing exactly that
    assert_eq!(
        q("/stations/*[::id = 1](->stage){18_parasang;18_parasang} | ::name"),
        "Susa"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){3_d;72_h} | ::name"),
        "Susa"
    );
    let e = no("/stations/*[::id = 1](->stage){5;2} | ::name");
    assert!(e.contains("quantifier {5;2} has max below min"), "{e}");
}

#[test]
fn a_typed_clause_reads_a_bare_number_in_its_unit() {
    let d = fixture("typed");
    let db = rusqlite::Connection::open(d.join("roads.db")).unwrap();
    db.execute_batch(
        "ALTER TABLE stages ADD COLUMN km INTEGER; ALTER TABLE stages ADD COLUMN scenes INTEGER;
         UPDATE stages SET km = 100, scenes = 2 WHERE id = 1;
         UPDATE stages SET km = 524, scenes = 5 WHERE id = 2;",
    )
    .unwrap();
    std::fs::write(
        d.join("typed.model"),
        "unit scene;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage \
         weights(::km<_km>; ::days; ::scenes<_scene>);\n",
    )
    .unwrap();
    let m = ["--model", "typed.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "roads.db"]].concat());
    assert_eq!(
        q("/stations/*[::id = 1] | ->stage | $-:weights"),
        "@(100_km; 3_d; 2_scene)"
    );
    // the canonical dimension order: the bases, then the opaque units
    // by name — a currency before a declared unit, after the time
    db.execute_batch(
        "ALTER TABLE stages ADD COLUMN fare TEXT; UPDATE stages SET fare = '135 EUR';",
    )
    .unwrap();
    std::fs::write(
        d.join("fared.model"),
        "unit scene;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage \
         weights(::fare; ::scenes<_scene>; ::days; ::km<_km>);\n",
    )
    .unwrap();
    assert_eq!(
        ok(
            &d,
            &[
                "--model",
                "fared.model",
                "/stations/*[::id = 1] | ->stage | $-:weights",
                "roads.db"
            ]
        ),
        "@(100_km; 3_d; 135_EUR; 2_scene)"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;120_km} | ::name"),
        "Sardis\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){;10_scene} | $-:total"),
        "null\n2_scene\n7_scene"
    );
    // the unit is built in or declared; the column holds numbers
    for (name, clause, expect) in [
        ("nounit", "weights(::km<_quux>)", "is no unit"),
        ("text", "weights(::distance<_km>)", "names no number"),
        ("absent", "weights(::nothere)", "names no property"),
    ] {
        std::fs::write(
            d.join(format!("{name}.model")),
            format!(
                "ref /stages/*::from_id --> /stations/*[::id = $];\n\
                 ref /stages/*::to_id --> /stations/*[::id = $];\n\
                 edge /stages/*: ::from_id -- ::to_id as stage {clause};\n"
            ),
        )
        .unwrap();
        let e = refused(
            &d,
            &[
                "--model",
                &format!("{name}.model"),
                "/stations/* @| count",
                "roads.db",
            ],
        );
        assert!(e.contains(expect), "{name}: {e}");
    }
}

#[test]
fn a_typed_potential_reads_a_bare_number_in_its_unit() {
    let d = fixture("typed-potential");
    let db = rusqlite::Connection::open(d.join("roads.db")).unwrap();
    db.execute_batch(
        "ALTER TABLE stations ADD COLUMN elevation INTEGER;
         UPDATE stations SET elevation = 1200 WHERE id = 1;
         UPDATE stations SET elevation = 1650 WHERE id = 2;
         UPDATE stations SET elevation = 1450 WHERE id = 3;",
    )
    .unwrap();
    std::fs::write(
        d.join("climb.model"),
        "ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::days);\n\
         weight-potential /stations/*::elevation<_m>;\n",
    )
    .unwrap();
    let m = ["--model", "climb.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "roads.db"]].concat());
    // the climb joins the set, metres before days
    assert_eq!(
        q("/stations/*[::id = 1] | ->stage | $-:weights"),
        "@(450_m; 3_d)"
    );
    // rise and fall over the two legs: +450, then −200
    assert_eq!(
        q("/stations/*[::id = 1](->stage){rise:;1000_m} | $-:costs"),
        "null\n@(450_m)\n@(450_m)"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){fall:;1000_m} | $-:costs"),
        "null\n@(0_m)\n@(200_m)"
    );
    // the cost prints in the brace's written unit
    assert_eq!(
        q("/stations/*[::id = 1](->stage){rise:;1_km} | $-:total"),
        "null\n0.45_km\n0.45_km"
    );
    // a bare number without a type is no potential, never a time
    for (name, stmt, expect) in [
        (
            "bare",
            "weight-potential /stations/*::elevation;",
            "a bare number has no dimension",
        ),
        (
            "absent",
            "weight-potential /stations/*::nothere;",
            "names no property",
        ),
        (
            "nounit",
            "weight-potential /stations/*::elevation<_quux>;",
            "is no unit",
        ),
    ] {
        std::fs::write(
            d.join(format!("{name}.model")),
            format!(
                "ref /stages/*::from_id --> /stations/*[::id = $];\n\
                 ref /stages/*::to_id --> /stations/*[::id = $];\n\
                 edge /stages/*: ::from_id -- ::to_id as stage weights(::days);\n\
                 {stmt}\n"
            ),
        )
        .unwrap();
        let e = refused(
            &d,
            &[
                "--model",
                &format!("{name}.model"),
                "/stations/* @| count",
                "roads.db",
            ],
        );
        assert!(e.contains(expect), "{name}: {e}");
    }
}

#[test]
fn a_star_unit_brace_names_the_dimension_and_an_absent_floor_is_no_bound() {
    // rulings 2026-11-07/08: `{*_km}` / `{min:*_m}` name the dimension
    // and the printing unit with no bound (the star, Quarb's "any
    // number of"; the bare `{_km}` of draft.219 is refused with the
    // pointer); an absent floor admits every cost, the negative ones
    // under max and min included; a bound is a signed quantity
    let d = fixture("star-unit");
    let db = rusqlite::Connection::open(d.join("roads.db")).unwrap();
    db.execute_batch(
        "ALTER TABLE stations ADD COLUMN elevation INTEGER;
         UPDATE stations SET elevation = 1200 WHERE id = 1;
         UPDATE stations SET elevation = 1650 WHERE id = 2;
         UPDATE stations SET elevation = 1450 WHERE id = 3;",
    )
    .unwrap();
    std::fs::write(
        d.join("climb.model"),
        "unit parasang, parasangs = 5550 m;\n\
         ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::days);\n\
         weight-potential /stations/*::elevation<_m>;\n",
    )
    .unwrap();
    // the fixture's model weighs the distance and the days
    let r = |query: &str| ok(&d, &["--model", "roads.model", query, "roads.db"]);
    // the climb model weighs the days and the climb, one weight per
    // dimension
    let m = ["--model", "climb.model"];
    let q = |query: &str| ok(&d, &[&m[..], &[query, "roads.db"]].concat());
    // every walk, its cost in the unit the brace writes
    assert_eq!(
        r("/stations/*[::id = 1](->stage){*_parasang} | $-:total"),
        "null\n18_parasang\n112.5_parasang"
    );
    assert_eq!(
        r("/stations/*[::id = 1](->stage){*_km} | $-:total"),
        "null\n99.9_km\n624.375_km"
    );
    assert_eq!(
        r("/stations/*[::id = 1](->stage){*_d} | ::name"),
        "Sardis\nSusa\nEcbatana"
    );
    // a rule takes the star unit: the extreme leg, signed
    assert_eq!(
        q("/stations/*[::id = 1](->stage){min:*_m} | $-:total"),
        "null\n450_m\n-200_m"
    );
    // the braces stack with a star unit among them
    assert_eq!(
        q("/stations/*[::id = 1](->stage){1;}{*_d}{min:*_m} | $-:costs"),
        "@(1; 3_d; 450_m)\n@(2; 21.9_d; -200_m)"
    );
    // an absent floor is no bound: the descent passes it
    assert_eq!(
        q("/stations/*[::id = 1](->stage){min:;9000_m} | ::name"),
        "Sardis\nSusa\nEcbatana"
    );
    // a signed bound: no leg descends more than a hundred metres
    assert_eq!(
        q("/stations/*[::id = 1](->stage){min:-100_m;} | ::name"),
        "Sardis\nSusa"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){min:-300_m;} | ::name"),
        "Sardis\nSusa\nEcbatana"
    );
    assert_eq!(
        q("/stations/*[::id = 1](->stage){max:;-100_m} | ::name"),
        "Sardis"
    );
    // a star takes no bound; the rule without either is refused
    // pointing at the star unit; the bare unit of draft.219 and the
    // star alone point at their spellings; the unit is built in or
    // declared
    for (brace, expect) in [
        ("{*_m;20_km}", "a star takes no bound"),
        ("{*_m;}", "a star takes no bound"),
        ("{;*_m}", "a star takes no bound"),
        ("{min:;}", "'{min:*_m}'"),
        ("{*_}", "names none"),
        ("{*_quux}", "names no unit the run knows"),
        ("{_km}", "written with the star, '{*_km}'"),
        ("{min:_m}", "'{min:*_m}'"),
        ("{*}", "'(->x)*'"),
        ("{1_m}", "has no exact form"),
    ] {
        let e = refused(
            &d,
            &[
                "--model",
                "climb.model",
                &format!("/stations/*[::id = 1](->stage){brace} | ::name"),
                "roads.db",
            ],
        );
        assert!(e.contains(expect), "{brace}: {e}");
    }
    // the typed clause writes its unit with the underscore
    std::fs::write(
        d.join("old.model"),
        "ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::days<d>);\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &["--model", "old.model", "/stations/* @| count", "roads.db"],
    );
    assert!(e.contains("'::km<_km>'") && e.contains("'<_d>'"), "{e}");
    std::fs::write(
        d.join("oldpot.model"),
        "ref /stages/*::from_id --> /stations/*[::id = $];\n\
         ref /stages/*::to_id --> /stations/*[::id = $];\n\
         edge /stages/*: ::from_id -- ::to_id as stage weights(::days);\n\
         weight-potential /stations/*::elevation<m>;\n",
    )
    .unwrap();
    let e = refused(
        &d,
        &[
            "--model",
            "oldpot.model",
            "/stations/* @| count",
            "roads.db",
        ],
    );
    assert!(e.contains("'::elevation<_m>'"), "{e}");
}

#[test]
fn a_typed_clause_on_a_quantity_column_checks_the_dimension_and_shows_its_unit() {
    // ruling 2026-11-08: the type on a column of quantities is a
    // dimension check and the unit the edge carries — the cells
    // convert on the fly, display only, the magnitude untouched
    let d = fixture("typed-quantity");
    let db = rusqlite::Connection::open(d.join("roads.db")).unwrap();
    db.execute_batch(
        "ALTER TABLE stations ADD COLUMN elevation TEXT;
         UPDATE stations SET elevation = '0 m' WHERE id = 1;
         UPDATE stations SET elevation = '2000 m' WHERE id = 2;
         UPDATE stations SET elevation = '3.5 km' WHERE id = 3;",
    )
    .unwrap();
    let decl = "unit parasang, parasangs = 5550 m;\n\
                ref /stages/*::from_id --> /stations/*[::id = $];\n\
                ref /stages/*::to_id --> /stations/*[::id = $];\n";
    let model = |name: &str, tail: &str| {
        std::fs::write(d.join(format!("{name}.model")), format!("{decl}{tail}")).unwrap();
    };
    let q = |model: &str, query: &str| {
        ok(
            &d,
            &["--model", &format!("{model}.model"), query, "roads.db"],
        )
    };
    model(
        "shown",
        "edge /stages/*: ::from_id -- ::to_id as stage weights(::distance<_km>; ::days<_h>);\n",
    );
    // '18 parasang' shown in km, '3 d' in hours
    assert_eq!(
        q("shown", "/stations/*[::id = 1] | ->stage | $-:weights"),
        "@(99.9_km; 72_h)"
    );
    // the budget selects the same walks: the magnitudes are unchanged
    assert_eq!(
        q(
            "shown",
            "/stations/*[::id = 1](->stage){;120_parasang} | ::name"
        ),
        "Sardis\nSusa\nEcbatana"
    );
    assert_eq!(
        q(
            "shown",
            "/stations/*[::id = 1](->stage){;100_parasang} | ::name"
        ),
        "Sardis\nSusa"
    );
    // a total prints in the brace's unit, as before
    assert_eq!(
        q("shown", "/stations/*[::id = 1](->stage){;30_d} | $-:total"),
        "null\n3_d\n21.9_d"
    );
    // the cells' own unit as the type changes nothing
    model(
        "same",
        "edge /stages/*: ::from_id -- ::to_id as stage weights(::distance<_parasang>; \
         ::days<_d>);\n",
    );
    assert_eq!(
        q("same", "/stations/*[::id = 1] | ->stage | $-:weights"),
        "@(18_parasang; 3_d)"
    );
    // a typed potential on quantity cells: the difference in the
    // type's unit, the cells written in m and in km alike
    model(
        "climb",
        "edge /stages/*: ::from_id -- ::to_id as stage weights(::days<_d>);\n\
         weight-potential /stations/*::elevation<_km>;\n",
    );
    assert_eq!(
        q("climb", "/stations/*[::id = 1] | ->stage | $-:weights"),
        "@(2_km; 3_d)"
    );
    assert_eq!(
        q("climb", "/stations/*[::id = 2] | ->stage | $-:weights"),
        "@(1.5_km; 18.9_d)"
    );
    assert_eq!(
        q("climb", "/stations/*[::id = 3] | <-stage | $-:weights"),
        "@(-1.5_km; 18.9_d)"
    );
    // a cell of another dimension refuses the model at load, naming
    // the property; a currency is a dimension of its own
    for (name, tail, expect) in [
        (
            "clash-time",
            "edge /stages/*: ::from_id -- ::to_id as stage weights(::days<_m>);\n",
            "weights(::days<_m>) types the rows' cells in m, and the rows hold 3 d — a quantity \
             of another dimension",
        ),
        (
            "clash-money",
            "edge /stages/*: ::from_id -- ::to_id as stage weights(::distance<_EUR>);\n",
            "weights(::distance<_EUR>) types the rows' cells in EUR, and the rows hold 18 \
             parasang — a quantity of another dimension",
        ),
        (
            "clash-potential",
            "edge /stages/*: ::from_id -- ::to_id as stage weights(::days);\n\
             weight-potential /stations/*::elevation<_h>;\n",
            "'::elevation<_h>' types the nodes' cells in h, and the nodes hold 0 m — a quantity \
             of another dimension",
        ),
    ] {
        model(name, tail);
        let e = refused(
            &d,
            &[
                "--model",
                &format!("{name}.model"),
                "/stations/* @| count",
                "roads.db",
            ],
        );
        assert!(e.contains(expect), "{name}: {e}");
    }
    // past the load check, a cell of another dimension among the
    // rows is no weight on its edge: the edge carries none of that
    // dimension, and a budget of it does not cross
    db.execute_batch("UPDATE stages SET days = '5 km' WHERE id = 2;")
        .unwrap();
    assert_eq!(
        q("shown", "/stations/*[::id = 2] | ->stage | $-:weights"),
        "@(524.475_km)"
    );
    assert_eq!(
        q("shown", "/stations/*[::id = 1](->stage){;1000_h} | ::name"),
        "Sardis\nSusa"
    );
}
