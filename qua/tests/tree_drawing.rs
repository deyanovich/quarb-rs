//! `@| tree` (spec: The tree): the context drawn as Unix `tree`
//! draws a directory — every context node a starting point, the
//! mount root the top line, a dotted stub for a skipped path, the
//! crosslinks one hop deep, and the operands (depth, traits, props,
//! `%`, columns).

use quarb::{AstAdapter, NodeId, QueryResult, Value};
use std::path::PathBuf;

fn text(q: &str, a: &impl AstAdapter) -> String {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => {
            assert_eq!(vs.len(), 1, "{q}: one text value, got {vs:?}");
            match &vs[0] {
                Value::Str(s) => s.clone(),
                other => panic!("{q}: expected text, got {other:?}"),
            }
        }
        other => panic!("{q}: expected a value, got {other:?}"),
    }
}

fn refused(q: &str, a: &impl AstAdapter) -> String {
    match quarb::run(q, a) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

/// A directory shaped like a small group root: two symlinks at the
/// top, a SYNOPSIS directory, and a project directory with two
/// files over a kilobyte.
fn biz() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("SYNOPSIS")).unwrap();
    std::fs::create_dir_all(dir.join("litogramma-llc")).unwrap();
    std::fs::write(dir.join("SYNOPSIS/CLAUDE.md"), "x".repeat(548)).unwrap();
    std::fs::write(dir.join("SYNOPSIS/README.md"), "x".repeat(633)).unwrap();
    std::fs::write(
        dir.join("litogramma-llc/operating-agreement.md"),
        "x".repeat(1200),
    )
    .unwrap();
    std::fs::write(dir.join("litogramma-llc/purpose.txt"), "x".repeat(300)).unwrap();
    std::fs::write(
        dir.join("litogramma-llc/s-corp-purpose.txt"),
        "x".repeat(400),
    )
    .unwrap();
    std::fs::write(dir.join("litogramma-llc/strategy.md"), "x".repeat(1100)).unwrap();
    std::os::unix::fs::symlink("SYNOPSIS/CLAUDE.md", dir.join("CLAUDE.md")).unwrap();
    std::os::unix::fs::symlink("SYNOPSIS/README.md", dir.join("README.md")).unwrap();
    dir
}

#[test]
fn every_node_is_unix_tree() {
    let dir = biz();
    let fs = quarb_fs::FsAdapter::new(&dir).unwrap();
    assert_eq!(
        text("//* @| tree", &fs),
        "\
.
├── CLAUDE.md -> SYNOPSIS/CLAUDE.md
├── README.md -> SYNOPSIS/README.md
├── SYNOPSIS
│   ├── CLAUDE.md
│   └── README.md
└── litogramma-llc
    ├── operating-agreement.md
    ├── purpose.txt
    ├── s-corp-purpose.txt
    └── strategy.md"
    );
    // a starting point below the root hangs by the dotted stub of
    // its skipped path, shared with its siblings
    assert_eq!(
        text("//<file>[:::size > 1_kB] @| tree", &fs),
        "\
.
└┄┄ litogramma-llc
    ├── operating-agreement.md
    └── strategy.md"
    );
    // the depth counts below each starting point; the traits follow
    // the name
    assert_eq!(
        text("/SYNOPSIS @| tree(0; -t)", &fs),
        ". <dir>\n└── SYNOPSIS <dir>"
    );
    assert_eq!(
        text("/* @| tree(1)", &fs),
        "\
.
├── CLAUDE.md -> SYNOPSIS/CLAUDE.md
├── README.md -> SYNOPSIS/README.md
├── SYNOPSIS
│   ├── CLAUDE.md
│   └── README.md
└── litogramma-llc
    ├── operating-agreement.md
    ├── purpose.txt
    ├── s-corp-purpose.txt
    └── strategy.md"
    );
    // a column: the expression's value two spaces after the name,
    // nothing where it is null
    let sized = text("/SYNOPSIS/* @| tree(:::size)", &fs);
    let (top, rest) = sized.split_once('\n').unwrap();
    assert!(top.starts_with(".  ") && top.ends_with("_B"), "{top}");
    assert_eq!(
        rest,
        "\
└┄┄ SYNOPSIS
    ├── CLAUDE.md  548_B
    └── README.md  633_B"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"archives":{"gro":{"entries":[
            {"surname":"Ali","given":"Ahmed"},
            {"surname":"Crease","given":"Ernest"}]}}}"#,
    )
    .unwrap()
}

#[test]
fn a_document_hangs_its_starting_points_by_their_path() {
    let d = doc();
    assert_eq!(
        text("/archives/gro/entries/* @| tree(::surname)", &d),
        "\
/
└┄┄ archives/gro/entries
    ├── 0  Ali
    │   ├── surname
    │   └── given
    └── 1  Crease
        ├── surname
        └── given"
    );
    assert_eq!(
        text("/archives/gro/entries/* @| tree(0; ::given)", &d),
        "\
/
└┄┄ archives/gro/entries
    ├── 0  Ahmed
    └── 1  Ernest"
    );
    // props names the properties, % gives them as a record
    assert_eq!(
        text("/archives/gro/entries/0 @| tree(0; --props)", &d),
        "/ ::archives\n└┄┄ archives/gro/entries\n    └── 0 ::surname ::given"
    );
    assert_eq!(
        text("/archives/gro/entries/1 @| tree(0; -r)", &d),
        "/\n└┄┄ archives/gro/entries\n    └── 1 %(surname = \"Crease\"; given = \"Ernest\")"
    );
    // a parenthesised column is a body on each node, a bare word
    // in it a command
    assert_eq!(
        text("/archives/gro/entries/0 @| tree(0; (count))", &d),
        "/  1\n└┄┄ archives/gro/entries\n    └── 0  1"
    );
    // the root itself as a starting point draws the whole document
    assert_eq!(text("^ @| tree(2)", &d), "/\n└── archives\n    └── gro");
    // a value context is refused; so is the pipe form and a stray word
    let e = refused("/archives/gro/entries/*::surname @| tree", &d);
    assert!(e.contains("tree draws nodes"), "{e}");
    let e = refused("/archives/gro/entries/* | tree", &d);
    assert!(e.contains("'tree' uses '@|'"), "{e}");
    let e = refused("/archives/gro/entries/* @| tree(sizes)", &d);
    assert!(e.contains("tree takes a depth"), "{e}");
}

#[test]
fn several_mounts_are_a_forest() {
    let m = quarb_mount::MountAdapter::new(vec![
        quarb_mount::Mount {
            name: "r".into(),
            target: Some("r.json".into()),
            adapter: Box::new(doc()),
        },
        quarb_mount::Mount {
            name: "s".into(),
            target: Some("s.json".into()),
            adapter: Box::new(doc()),
        },
    ]);
    assert_eq!(
        text("/*/archives/gro/entries/0 @| tree(0; ::surname)", &m),
        "\
r
└┄┄ archives/gro/entries
    └── 0  Ali
s
└┄┄ archives/gro/entries
    └── 0  Ali"
    );
}

/// People and companies with crosslinks: `works_at` from a person
/// to a company, `founded_by` back.
struct Firm;

impl AstAdapter for Firm {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        match node.0 {
            0 => vec![NodeId(1), NodeId(4)],
            1 => vec![NodeId(2), NodeId(3)],
            4 => vec![NodeId(5)],
            _ => Vec::new(),
        }
    }
    fn name(&self, node: NodeId) -> Option<String> {
        Some(
            match node.0 {
                1 => "people",
                2 => "ada",
                3 => "charles",
                4 => "companies",
                5 => "analytical",
                _ => return None,
            }
            .to_string(),
        )
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match node.0 {
            1 | 4 => Some(NodeId(0)),
            2 | 3 => Some(NodeId(1)),
            5 => Some(NodeId(4)),
            _ => None,
        }
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        match node.0 {
            2 | 3 => vec![("works_at".to_string(), NodeId(5))],
            5 => vec![("founded_by".to_string(), NodeId(3))],
            _ => Vec::new(),
        }
    }
}

#[test]
fn crosslinks_are_listed_one_hop_deep() {
    // a target in the context not drawn yet is drawn under its
    // crosslink line; a target of that target shows its path alone,
    // `-> ...` marking its onward links; a node is drawn once
    assert_eq!(
        text("//* @| tree", &Firm),
        "\
/
├── people
│   ├── ada
│   │   ->works_at /companies/analytical
│   │       ->founded_by /people/charles -> ...
│   └── charles
│       ->works_at /companies/analytical
└── companies"
    );
    // only the context's nodes are targets: the companies out of
    // the context, no arrow points at them
    assert_eq!(
        text("/people/* @| tree", &Firm),
        "/\n└┄┄ people\n    ├── ada\n    └── charles"
    );
}

#[test]
fn the_display_words_are_switches_and_the_old_spellings_point_at_them() {
    let d = quarb_json::JsonAdapter::parse(r#"{"a":{"x":1}}"#).unwrap();
    for (q, hint) in [
        (
            "/a @| tree(traits)",
            "'traits' is a string here: write --traits or -t",
        ),
        (
            "/a @| tree(kind)",
            "'kind' is a string here: write --kind or -k",
        ),
        (
            "/a @| tree(0; props)",
            "'props' is a string here: write --props or -p",
        ),
        (
            "/a @| tree(%)",
            "'%' is retired as tree's operand: write --record or -r",
        ),
    ] {
        let e = quarb::run(q, &d).unwrap_err().to_string();
        assert!(e.contains(hint), "{q}: {e}");
    }
    // the switches mix with the positional depth and columns
    // (the switches stand last, after the depth and the columns, as
    // the one option rule has it)
    for q in [
        "/a @| tree(1; -t)",
        "/a @| tree(0; ::x; -r)",
        "/a @| tree(0; --traits --props)",
        "/a @| tree(-p)",
    ] {
        quarb::run(q, &d).unwrap_or_else(|e| panic!("{q}: {e}"));
    }
}

#[test]
fn short_switches_bundle_as_in_the_shell() {
    let d = quarb_json::JsonAdapter::parse(r#"{"a":{"x":1}}"#).unwrap();
    let text = |q: &str| match quarb::run(q, &d).unwrap_or_else(|e| panic!("{q}: {e}")) {
        quarb::QueryResult::Values(vs) => vs
            .iter()
            .map(|v| v.display_form())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("{q}: {other:?}"),
    };
    assert_eq!(text("/a @| tree(1; -tp)"), text("/a @| tree(1; -t -p)"));
    // --kind prints the first trait alone, where -t prints them all
    assert_eq!(
        text("/a @| tree(1; -k)"),
        "/ <object>\n└── a <object>\n    └── x <number>"
    );
    assert_eq!(
        text("/a @| tree(1; -kt)"),
        text("/a @| tree(1; --kind --traits)")
    );
    assert_eq!(text("/a @| tree(1; -pt)"), text("/a @| tree(1; -p -t)"));
    assert_eq!(
        text("/a @| tree(0; ::x; -rt)"),
        text("/a @| tree(0; ::x; -r -t)")
    );
    // the unparse prints them singly
    assert_eq!(
        quarb::Query::parse("/a @| tree(1; -tp)", &d)
            .unwrap()
            .canonical(),
        "/a @| tree(1; -t -p)"
    );
    // an unknown letter names itself and the command's short switches
    let e = quarb::run("/a @| tree(1; -tx)", &d)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("'-x' in '-tx' is no switch of 'tree' (its short switches: -k -t -p -r -s)"),
        "{e}"
    );
    // the last letter keeps its value: `-to=…` on a reducer
    assert_eq!(text("/a | ::x @| sum(-o=$_ * 2)"), "2");
    // long options never bundle
    let e = quarb::run("/a @| tree(1; --tp)", &d)
        .unwrap_err()
        .to_string();
    assert!(e.contains("no option '--tp'"), "{e}");
}

/// Runs of same-named sibling leaves collapse into one line with
/// their count when the lines would read alike; `--sample=n` draws
/// the first n children of a node that has more and one line "… and
/// N more"; `-r` prints the scalar properties alone, none when none,
/// six then an ellipsis.
#[test]
fn leaf_runs_collapse_sampling_and_the_record_part() {
    let d = quarb_json::JsonAdapter::parse(
        r#"{"rows":[{"k":1},{"k":1},{"k":2}],"flat":["a","a","a","b"],
            "o":{"x":1,"y":"two","z":[1,2],"w":{"k":1},"b":true,"c":null,"d":3,"e":4,"f":5,"g":6}}"#,
    )
    .unwrap();
    let text = |q: &str| match quarb::run(q, &d).unwrap_or_else(|e| panic!("{q}: {e}")) {
        quarb::QueryResult::Values(vs) => vs
            .iter()
            .map(|v| v.display_form())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("{q}: {other:?}"),
    };
    // leaves named by position differ, and do not collapse; rows
    // with children are not leaves
    assert_eq!(
        text("/flat @| tree(1)"),
        "/\n└── flat\n    ├── 0\n    ├── 1\n    ├── 2\n    └── 3"
    );
    assert_eq!(
        text("/rows @| tree(1)"),
        "/\n└── rows\n    ├── 0\n    ├── 1\n    └── 2"
    );
    // same-named leaves whose lines read alike collapse; a column
    // that differs keeps them apart
    assert_eq!(
        text("/rows/*/k @| tree(0)"),
        "/\n└── rows\n    ├┄┄ 0\n    │   └── k\n    ├┄┄ 1\n    │   └── k\n    └┄┄ 2\n        └── k"
    );
    assert_eq!(
        text("/flat/* @| tree(0; -k)"),
        "/ <object>\n└┄┄ flat\n    ├── 0 <string>\n    ├── 1 <string>\n    ├── 2 <string>\n    └── 3 <string>"
    );
    // --sample=n: the first n, then the more line
    assert_eq!(
        text("/flat @| tree(1; -s=2)"),
        "/\n└── flat\n    ├── 0\n    ├── 1\n    └── … and 2 more"
    );
    assert_eq!(
        text("/o @| tree(1; --sample=3)"),
        "/\n└── o\n    ├── x\n    ├── y\n    ├── z\n    └── … and 7 more"
    );
    // -r: the scalar properties, six then an ellipsis; a child with
    // children (z, w) and a null member are no fields; none → nothing
    assert_eq!(
        text("/o @| tree(0; -r)"),
        "/\n└── o %(x = 1; y = \"two\"; b = true; d = 3; e = 4; f = 5; …)"
    );
    assert_eq!(text("/o/w @| tree(0; -r)"), "/\n└┄┄ o\n    └── w %(k = 1)");
    assert_eq!(text("/o/x @| tree(0; -r)"), "/\n└┄┄ o\n    └── x");
}

#[test]
fn diverging_paths_share_their_prefix_as_solid_nodes() {
    let d = quarb_json::JsonAdapter::parse(
        r#"{"archives":{"gro":{"entries":[{"s":"Ali"},{"s":"Crease"}],"index":{"a":1}},"pro":{"x":1}}}"#,
    )
    .unwrap();
    let text = |q: &str| match quarb::run(q, &d).unwrap() {
        quarb::QueryResult::Values(vs) => vs
            .iter()
            .map(|v| v.display_form())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("{q}: {other:?}"),
    };
    // one skipped path: one stub, as before
    assert_eq!(
        text("/archives/gro/entries/* @| tree(0; ::s)"),
        "/\n└┄┄ archives/gro/entries\n    ├── 0  Ali\n    └── 1  Crease"
    );
    // the paths diverge under gro: archives and gro drawn once, solid,
    // the stubs hanging from the divergence
    assert_eq!(
        text("/archives/gro/*/* @| tree(0)"),
        "\
/
└── archives
    └── gro
        ├┄┄ entries
        │   ├── 0
        │   └── 1
        └┄┄ index
            └── a"
    );
    // a divergence right below the top
    assert_eq!(
        text("/archives/*/* @| tree(0)"),
        "\
/
└── archives
    ├┄┄ gro
    │   ├── entries
    │   └── index
    └┄┄ pro
        └── x"
    );
    // a starting point beside a divergence hangs from the shared node
    assert_eq!(
        text("/archives/gro//*[::s || :name = \"index\"] @| tree(0)"),
        "\
/
└── archives
    └── gro
        ├┄┄ entries
        │   ├── 0
        │   └── 1
        └── index"
    );
    // the column prints on every solid line, the top's included
    assert_eq!(
        text("/archives/gro/*/* @| tree(0; (/* @| count))"),
        "\
/  1
└── archives  2
    └── gro  2
        ├┄┄ entries
        │   ├── 0  1
        │   └── 1  1
        └┄┄ index
            └── a  0"
    );
}
