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
        text("/SYNOPSIS @| tree(traits; 0)", &fs),
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
        text("/archives/gro/entries/0 @| tree(0; props)", &d),
        "/ ::archives\n└┄┄ archives/gro/entries\n    └── 0 ::surname ::given"
    );
    assert_eq!(
        text("/archives/gro/entries/1 @| tree(0; %)", &d),
        "/ %(archives = null)\n└┄┄ archives/gro/entries\n    └── 1 %(surname = \"Crease\"; given = \"Ernest\")"
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
