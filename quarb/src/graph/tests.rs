use super::*;
use crate::graph::adapter::Provenance;
use crate::syntax::ast::{ArithOp, PatSeg};
use crate::syntax::lexer::lex;
use crate::syntax::parser::parse;
use std::collections::HashMap;

/// Ruling #33 — the glob matcher behind pattern literals.
#[test]
fn glob_match_anchors_and_stars() {
    use PatSeg::{Lit, Star};
    let lit = |t: &str| Lit(t.to_string());
    // contains
    assert!(glob_match(&[Star, lit("web"), Star], "app-web-2"));
    assert!(!glob_match(&[Star, lit("web"), Star], "app"));
    // prefix / suffix anchor
    assert!(glob_match(&[lit("app"), Star], "app-web"));
    assert!(!glob_match(&[lit("app"), Star], "the-app"));
    assert!(glob_match(&[Star, lit(".gz")], "data.gz"));
    assert!(!glob_match(&[Star, lit(".gz")], "data.gz.bak"));
    // both-anchored multi-segment
    assert!(glob_match(&[lit("a"), Star, lit("z")], "abcz"));
    assert!(glob_match(&[lit("a"), Star, lit("z")], "az"));
    assert!(!glob_match(&[lit("a"), Star, lit("z")], "abc"));
    // the star spans empty
    assert!(glob_match(&[Star, lit("x"), Star], "x"));
    // backtracking: the first occurrence is not the match
    assert!(glob_match(&[Star, lit("ab"), Star, lit("b")], "ab-ab-b"));
    // no segments match only the empty text
    assert!(glob_match(&[] as &[PatSeg], ""));
    assert!(!glob_match(&[] as &[PatSeg], "x"));
    // multi-byte text walks char boundaries safely
    assert!(glob_match(&[Star, lit("gun")], "льюис gun"));
}

/// Ruling #33 — null never matches; `!=` therefore keeps holes.
#[test]
fn pattern_test_null_semantics() {
    use PatSeg::{Lit, Star};
    let p = [Star, Lit("x".into()), Star];
    assert!(!pattern_test(&p, &Value::Null));
    assert!(pattern_test(&p, &Value::Str("axb".into())));
    // non-string values test their string rendering, as `*=` does
    assert!(pattern_test(
        &[Star, Lit("42".into()), Star],
        &Value::Int(142)
    ));
}

#[test]
fn quantital_coercion_and_arith() {
    let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::value::quantity::scale_expr;
    let q = |value: f64, base: &str, written: Option<(f64, &str)>| Value::Quantity {
        value,
        base: base.into(),
        written: written.map(|(v, u)| (v, u.to_string())),
    };
    let len = q(42000.0, "m", Some((42.0, "km")));
    // Coercion: unit text either side; a bare number carries no
    // dimension, so it is never equal to a length (spec:
    // Relations), whichever unit one might read it in.
    assert_eq!(
        value_cmp(&len, &Value::Str("5km".into()), sc),
        Some(Ordering::Greater)
    );
    assert!(value_eq(&len, &Value::Str("42km".into()), sc));
    assert!(!value_eq(&len, &Value::Float(42.0), sc));
    assert!(!value_eq(&len, &Value::Float(42000.0), sc));
    assert_eq!(value_cmp(&len, &Value::Float(42.0), sc), None);
    assert_eq!(
        value_cmp(&len, &Value::Str("30mi".into()), sc),
        Some(Ordering::Less)
    );
    // Dimension mismatch: the comparison fails rather than lies.
    let power = q(290.0, "kg*m^2/s^3", Some((290.0, "W")));
    assert_eq!(value_cmp(&power, &Value::Str("5km".into()), sc), None);
    assert!(!value_eq(&power, &Value::Str("5km".into()), sc));
    // kW text against a W-minted quantity — the motivating case.
    assert_eq!(
        value_cmp(&power, &Value::Str("0.2kW".into()), sc),
        Some(Ordering::Greater)
    );
    // Arithmetic: Q ± unit text, Q × n; the display forms.
    assert_eq!(len.to_string(), "42_km");
    let sum = arith(ArithOp::Add, &len, &Value::Str("500 m".into()), sc);
    assert_eq!(sum.to_string(), "42.5_km");
    let doubled = arith(ArithOp::Mul, &len, &Value::Int(2), sc);
    assert_eq!(doubled.to_string(), "84_km");
    // Numbers never lift into ± (| convert is the explicit road).
    assert!(matches!(
        arith(ArithOp::Add, &len, &Value::Int(5), sc),
        Value::Null
    ));
}

#[test]
fn durational_coercion_and_lift() {
    let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::value::quantity::scale_expr;
    let two_h = Value::seconds(7200, 0);
    // Comparison coercion beside a typed Duration: span text
    // (either grammar); a bare number is not seconds, time being
    // a dimension like the others (spec: Relations).
    assert_eq!(
        value_cmp(&two_h, &Value::Str("90min".into()), sc),
        Some(Ordering::Greater)
    );
    assert!(value_eq(&two_h, &Value::Str("PT2H".into()), sc));
    assert!(!value_eq(&two_h, &Value::Int(7200), sc));
    // No durational reading: the comparison fails.
    assert_eq!(value_cmp(&two_h, &Value::Str("hello".into()), sc), None);
    assert!(!value_eq(&two_h, &Value::Str("hello".into()), sc));
    // The arithmetic text-lift: instant − span text = instant,
    // instant − instant text = duration (disjoint grammars).
    let (s, n, _) = crate::temporal::parse_iso("2026-07-12T09:00:00Z").unwrap();
    let now = Value::Instant {
        secs: s,
        nanos: n,
        offset_min: Some(0),
        calendar: Default::default(),
    };
    let earlier = arith(ArithOp::Sub, &now, &Value::Str("12h".into()), sc);
    assert_eq!(earlier.to_string(), "2026-07-11T21:00:00Z");
    let span = arith(ArithOp::Sub, &now, &Value::Str("2026-07-12".into()), sc);
    // instant − instant: a span with no written unit prints scaled
    assert_eq!(span.to_string(), "9h");
    // Numbers never lift in arithmetic (epoch or seconds?).
    assert!(matches!(
        arith(ArithOp::Sub, &now, &Value::Int(300), sc),
        Value::Null
    ));
    // Duration ± span text stays a duration.
    let sum = arith(ArithOp::Add, &two_h, &Value::Str("30min".into()), sc);
    assert_eq!(sum.to_string(), "2h 30min");
    // A quantity is no scaling factor: Duration × Quantity (and
    // ÷) is dimensional nonsense — null, like Q × Q — never the
    // base magnitude leaking in as a bare number (5 kg × 2 h
    // must not be PT10H).
    let five_kg = Value::Quantity {
        value: 5.0,
        base: "kg".into(),
        written: Some((5.0, "kg".into())),
    };
    assert!(matches!(
        arith(ArithOp::Mul, &two_h, &five_kg, sc),
        Value::Null
    ));
    assert!(matches!(
        arith(ArithOp::Mul, &five_kg, &two_h, sc),
        Value::Null
    ));
    assert!(matches!(
        arith(ArithOp::Div, &two_h, &five_kg, sc),
        Value::Null
    ));
}

/// A tiny in-memory arbor for testing navigation without I/O.
struct MockTree {
    names: Vec<Option<String>>,
    kids: HashMap<u64, Vec<u64>>,
    /// Nodes outside the tree (crosslink targets only).
    loose: Vec<u64>,
    /// Outgoing crosslinks: node -> [(label, target)].
    links: HashMap<u64, Vec<(String, u64)>>,
    /// Edge properties: (source, label, target) -> [(name, value)].
    edge_props: HashMap<(u64, String, u64), Vec<(String, Value)>>,
    /// Per-node data provenance overrides.
    prov: HashMap<u64, Provenance>,
}

impl MockTree {
    /// ```text
    /// (root) ├ a ├ x.rs
    ///        │   └ y.txt
    ///        └ b ├ z.rs
    ///            └ deep └ w.rs
    /// ```
    fn sample() -> Self {
        let names = vec![
            None,                 // 0 root
            Some("a".into()),     // 1
            Some("x.rs".into()),  // 2
            Some("y.txt".into()), // 3
            Some("b".into()),     // 4
            Some("z.rs".into()),  // 5
            Some("deep".into()),  // 6
            Some("w.rs".into()),  // 7
        ];
        let mut kids = HashMap::new();
        kids.insert(0, vec![1, 4]);
        kids.insert(1, vec![2, 3]);
        kids.insert(4, vec![5, 6]);
        kids.insert(6, vec![7]);
        // x.rs --ref--> w.rs
        let mut links = HashMap::new();
        links.insert(2, vec![("ref".to_string(), 7)]);
        MockTree {
            names,
            kids,
            loose: Vec::new(),
            links,
            edge_props: HashMap::new(),
            prov: HashMap::new(),
        }
    }

    /// The sample tree with a `mgr` crosslink cycle laid over it:
    /// x.rs → z.rs → w.rs → x.rs.
    fn cyclic() -> Self {
        let mut t = MockTree::sample();
        t.links = HashMap::new();
        t.links.insert(2, vec![("mgr".to_string(), 5)]);
        t.links.insert(5, vec![("mgr".to_string(), 7)]);
        t.links.insert(7, vec![("mgr".to_string(), 2)]);
        t
    }

    /// ```text
    /// (root) └ div └ ul └ li └ ul └ li └ ul └ li
    /// ```
    /// Three levels of nesting for `(/ul/li)` repetition.
    fn lists() -> Self {
        let names = vec![
            None,
            Some("div".into()), // 1
            Some("ul".into()),  // 2
            Some("li".into()),  // 3
            Some("ul".into()),  // 4
            Some("li".into()),  // 5
            Some("ul".into()),  // 6
            Some("li".into()),  // 7
        ];
        let mut kids = HashMap::new();
        for (parent, child) in [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7)] {
            kids.insert(parent, vec![child]);
        }
        MockTree {
            names,
            kids,
            loose: Vec::new(),
            links: HashMap::new(),
            edge_props: HashMap::new(),
            prov: HashMap::new(),
        }
    }

    /// The weighted tree with a second edge converging on
    /// deep: a -4-> x.rs -12-> deep, a -1-> y.txt -5-> deep.
    fn converging() -> Self {
        let mut t = MockTree::weighted();
        t.links.get_mut(&3).map(|v| v.push(("e".to_string(), 6)));
        t.links.insert(3, vec![("e".to_string(), 6)]);
        t.edge_props.insert(
            (3, "e".to_string(), 6),
            vec![("qty".to_string(), Value::Int(5))],
        );
        t
    }

    /// A crosslink diamond with a back edge:
    /// a → x.rs, a → y.txt; x.rs → deep, y.txt → deep;
    /// deep → x.rs. All edges labeled `e`.
    fn diamond() -> Self {
        let mut t = MockTree::sample();
        t.links = HashMap::new();
        t.links
            .insert(1, vec![("e".to_string(), 2), ("e".to_string(), 3)]);
        t.links.insert(2, vec![("e".to_string(), 6)]);
        t.links.insert(3, vec![("e".to_string(), 6)]);
        t.links.insert(6, vec![("e".to_string(), 2)]);
        t
    }
}

impl MockTree {
    /// The sample tree with weighted `e` edges laid over it:
    /// a -4-> x.rs, a -1-> y.txt; x.rs -12-> deep.
    fn weighted() -> Self {
        let mut t = MockTree::sample();
        t.links = HashMap::new();
        t.links
            .insert(1, vec![("e".to_string(), 2), ("e".to_string(), 3)]);
        t.links.insert(2, vec![("e".to_string(), 6)]);
        let mut w = |s: u64, t_: u64, q: i64| {
            t.edge_props.insert(
                (s, "e".to_string(), t_),
                vec![("qty".to_string(), Value::Int(q))],
            );
        };
        w(1, 2, 4);
        w(1, 3, 1);
        w(2, 6, 12);
        t
    }
}

impl AstAdapter for MockTree {
    fn root(&self) -> NodeId {
        NodeId(0)
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.kids
            .get(&node.0)
            .map(|v| v.iter().map(|&i| NodeId(i)).collect())
            .unwrap_or_default()
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.names[node.0 as usize].clone()
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.kids
            .iter()
            .find(|(_, v)| v.contains(&node.0))
            .map(|(&p, _)| NodeId(p))
    }
    fn non_tree_nodes(&self) -> Vec<NodeId> {
        self.loose.iter().map(|&i| NodeId(i)).collect()
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.links
            .get(&node.0)
            .map(|v| v.iter().map(|(l, t)| (l.clone(), NodeId(*t))).collect())
            .unwrap_or_default()
    }
    /// `.rs` files are `<code>`, other files `<text>`; the
    /// directories carry no traits (so `<!*>` finds them).
    fn traits(&self, node: NodeId) -> Vec<String> {
        match self.names[node.0 as usize].as_deref() {
            Some(n) if n.ends_with(".rs") => vec!["code".into(), "file".into()],
            Some(n) if n.contains('.') => vec!["text".into(), "file".into()],
            _ => Vec::new(),
        }
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.links
            .iter()
            .flat_map(|(&src, v)| {
                v.iter()
                    .filter(|(_, t)| *t == node.0)
                    .map(move |(l, _)| (l.clone(), NodeId(src)))
            })
            .collect()
    }
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        self.edge_props
            .get(&(source.0, label.to_string(), target.0))?
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
    fn provenance(&self, node: NodeId) -> Provenance {
        self.prov.get(&node.0).cloned().unwrap_or_default()
    }
}

fn run(q: &str, t: &impl AstAdapter) -> Vec<u64> {
    match eval(&parse(&lex(q).unwrap()).unwrap(), t) {
        QueryResult::Nodes(ns) => ns.into_iter().map(|n| n.0).collect(),
        QueryResult::Values(_) => panic!("expected nodes, got values"),
    }
}

fn vals(q: &str, t: &MockTree) -> Vec<Value> {
    match eval(&parse(&lex(q).unwrap()).unwrap(), t) {
        QueryResult::Values(vs) => vs,
        QueryResult::Nodes(_) => panic!("expected values, got nodes"),
    }
}

/// `*//` is the universal search: the root and its descendants
/// in document order, then the nodes outside the tree in the
/// adapter's order; `^//` and `//` stay the proper descendants.
#[test]
fn universal_search_reaches_the_whole_arbor() {
    let mut t = MockTree::sample();
    // A node joined to the tree by a crosslink alone.
    t.names.push(Some("x.rs".into())); // 8
    t.loose.push(8);
    t.links.insert(3, vec![("see".to_string(), 8)]);
    assert_eq!(run("*//x.rs", &t), vec![2, 8]);
    assert_eq!(run("//x.rs", &t), vec![2]);
    assert_eq!(run("^//x.rs", &t), vec![2]);
    // The root is in the universe and first.
    assert_eq!(run("*//*", &t), vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(run("//*", &t), vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(run("*//*[:is-non-tree]", &t), vec![8]);
    // Steps continue from the found nodes as usual.
    assert_eq!(run("*//y.txt->see", &t), vec![8]);
    // Head position only, no reach.
    assert!(parse(&lex("*//?x.rs").unwrap()).is_err());
    assert!(parse(&lex("/a/*//x.rs").unwrap()).is_ok());
    assert_eq!(run("/a/*//x.rs", &t), Vec::<u64>::new());
    assert_eq!(
        crate::syntax::unparse::unparse(&parse(&lex("*//x.rs[1]").unwrap()).unwrap()),
        "*//x.rs[1]"
    );
    // As an operand, root-anchored like `^`.
    assert_eq!(
        vals("/a | %(n = (*//x.rs @| count); d = (//x.rs @| count))", &t),
        vec![Value::Record(vec![
            ("n".to_string(), Value::Int(2)),
            ("d".to_string(), Value::Int(1)),
        ])]
    );
}

#[test]
fn child_and_descendant() {
    let t = MockTree::sample();
    assert_eq!(run("/a", &t), vec![1]);
    assert_eq!(run("/a/x.rs", &t), vec![2]);
    assert_eq!(run("//*.rs", &t), vec![2, 5, 7]);
}

#[test]
fn parent_and_ancestor() {
    let t = MockTree::sample();
    assert_eq!(run("//w.rs\\deep", &t), vec![6]);
    // ancestors of w.rs named 'b' -> node 4
    assert_eq!(run("//w.rs\\\\b", &t), vec![4]);
}

#[test]
fn proximal_and_distal_descendant() {
    let t = MockTree::sample();
    // .rs descendants of b: z.rs (depth 1), w.rs (depth 2)
    assert_eq!(run("/b//*.rs", &t), vec![5, 7]);
    assert_eq!(run("/b//?*.rs", &t), vec![5]); // nearest
    assert_eq!(run("/b//!*.rs", &t), vec![7]); // farthest
}

#[test]
fn siblings() {
    let t = MockTree::sample();
    assert_eq!(run("/a/x.rs>y.txt", &t), vec![3]);
    assert_eq!(run("/a/y.txt<x.rs", &t), vec![2]);
    assert!(run("/a/y.txt>*", &t).is_empty()); // no next sibling
}

#[test]
fn leaf_anchor() {
    let t = MockTree::sample();
    // 'deep' has a child, so it is not a leaf
    assert!(run("/b/deep$", &t).is_empty());
    assert_eq!(run("/b/z.rs$", &t), vec![5]);
}

#[test]
fn regex_name() {
    let t = MockTree::sample();
    assert_eq!(run("//(/.*\\.rs/)", &t), vec![2, 5, 7]);
}

#[test]
fn correlation() {
    let t = MockTree::sample();
    // Driver-first: the first expression drives; the joined
    // expression's ON clause references the driver as `$$…`.
    // Nodes at the same depth as `a` (depth 1): a and b.
    assert_eq!(run("//* <=> //a[:depth = $$0:depth]", &t), vec![1, 4]);
    // All .rs files except the one `x.rs` names (join by name).
    assert_eq!(
        run("//*.rs <=> //x.rs[:name != $$0:name]", &t),
        vec![5, 7]
    );
    // Chained: an unconstrained second entry binds its first
    // candidate; the depth condition still filters the driver.
    assert_eq!(
        run("//* <=> //a[:depth = $$0:depth] <=> //x.rs", &t),
        vec![1, 4]
    );
    // A later entry may reference an earlier one's witness
    // (`$*1`): x.rs sits at depth 2, the a-witness at depth 1,
    // so no driver survives the second ON.
    assert!(
        run(
            "//* <=> //a[:depth = $$0:depth] <=> //x.rs[:depth = $$1:depth]",
            &t
        )
        .is_empty()
    );
}

#[test]
fn nested_predicates_read_the_thread_witness() {
    let t = MockTree::sample();
    // Driver: every .rs file; joined: the node of the same name,
    // so each thread's witness is itself (x.rs→2, z.rs→5, w.rs→7).
    // A root-anchored subexpression in a pipeline filter reads
    // `$*1`: it must see THIS thread's witness, never search the
    // trace for some tuple that happens to satisfy it. Only
    // w.rs's witness sits at depth 3; x.rs's and z.rs's at 2.
    assert_eq!(
        run(
            "//*.rs <=> //*[:name = $$0:name] \
             | [(^//*[:name = $$1:name && :depth = 3])]",
            &t
        ),
        vec![7]
    );
    assert_eq!(
        run(
            "//*.rs <=> //*[:name = $$0:name] \
             | [(^//*[:name = $$1:name && :depth = 2])]",
            &t
        ),
        vec![2, 5]
    );
    // A pipe hop's step predicate reads the same witness: from
    // each thread, climb to every ancestor and step to its .rs
    // children other than the witness itself, at depth 2. Only
    // w.rs's thread reaches one (z.rs, under b); x.rs's and
    // z.rs's only reach themselves. Under the old re-search any
    // .rs file was "not the witness" for SOME tuple, admitting
    // x.rs and z.rs too.
    assert_eq!(
        run(
            "//*.rs <=> //*[:name = $$0:name] \
             | \\\\*/*.rs[:name != $$1:name && :depth = 2]",
            &t
        ),
        vec![5]
    );
}

#[test]
fn crosslinks() {
    let t = MockTree::sample();
    // x.rs --ref--> w.rs
    assert_eq!(run("//x.rs->ref", &t), vec![7]);
    assert_eq!(run("//x.rs->*", &t), vec![7]);
    // wrong label -> nothing
    assert!(run("//x.rs->other", &t).is_empty());
    // incoming: w.rs is pointed at by x.rs
    assert_eq!(run("//w.rs<-ref", &t), vec![2]);
    // continue navigating from the crosslink target
    assert_eq!(run("//x.rs->ref\\deep", &t), vec![6]);
}

#[test]
fn quantified_child_group() {
    let t = MockTree::lists();
    // li nodes at nesting depths 1..3 (nodes 3, 5, 7)
    assert_eq!(run("/div(/ul/li)+", &t), vec![3, 5, 7]);
    assert_eq!(run("/div(/ul/li)+?", &t), vec![3]); // shallowest
    assert_eq!(run("/div(/ul/li)+!", &t), vec![7]); // deepest
    assert_eq!(run("/div(/ul/li){2}", &t), vec![5]);
    assert_eq!(run("/div(/ul/li){1;2}", &t), vec![3, 5]);
    assert_eq!(run("/div(/ul/li){1;2}!", &t), vec![5]);
    // min above the tree's depth: nothing
    assert!(run("/div(/ul/li){4;}", &t).is_empty());
}

#[test]
fn quantified_crosslinks_on_a_cycle() {
    // mgr cycle: x.rs (2) → z.rs (5) → w.rs (7) → x.rs (2)
    let t = MockTree::cyclic();
    // Simple paths: the anchor is visited, so the cycle cannot
    // close — the chain stops after two hops, bound or no bound.
    assert_eq!(run("//x.rs(->mgr)+", &t), vec![5, 7]);
    assert_eq!(run("//x.rs(->mgr)+?", &t), vec![5]);
    assert_eq!(run("//x.rs(->mgr)+!", &t), vec![7]);
    assert_eq!(run("//x.rs(->mgr){2}", &t), vec![7]);
    // `*` admits zero repetitions: the anchor itself joins.
    assert_eq!(run("//x.rs(->mgr)*", &t), vec![2, 5, 7]);
    // Zero-repetition proximal: the anchor is the shortest match.
    assert_eq!(run("//x.rs(->mgr)*?", &t), vec![2]);
    // Incoming links run the cycle backwards.
    assert_eq!(run("//x.rs(<-mgr)+", &t), vec![7, 5]);
}

#[test]
fn group_alternation() {
    let t = MockTree::sample();
    // Strict form: each alternative spells its own nav-op.
    assert_eq!(run("(/a|/b)", &t), vec![1, 4]);
    // Tolerated form: the op before '(' distributes over names.
    assert_eq!(run("/a/(x.rs|y.txt)", &t), vec![2, 3]);
    // Subpath alternatives of different lengths.
    assert_eq!(run("/b/(z.rs|deep/w.rs)", &t), vec![5, 7]);
    // Nested group inside an alternative.
    assert_eq!(run("(/a/(x.rs|y.txt)|/b/z.rs)", &t), vec![2, 3, 5]);
    // Mixing strict and tolerated alternatives refuses.
    let toks = lex("/a/(x.rs|/y.txt)").unwrap();
    assert!(parse(&toks).is_err());
    // An empty alternative refuses.
    let toks = lex("(/a|)").unwrap();
    assert!(parse(&toks).is_err());
}

#[test]
fn label_algebra() {
    // x.rs --ref--> w.rs, --doc--> w.rs, --ref--> z.rs: the
    // selection reads the labels a crosslink carries together.
    let mut t = MockTree::sample();
    t.links.insert(
        2,
        vec![
            ("ref".to_string(), 7),
            ("doc".to_string(), 7),
            ("ref".to_string(), 5),
        ],
    );
    assert_eq!(run("/a/x.rs->(ref && doc)", &t), vec![7]);
    assert_eq!(run("/a/x.rs->(ref || doc)", &t), vec![7, 5]);
    assert_eq!(run("/a/x.rs->(doc || ref)", &t), vec![7, 5]);
    assert_eq!(run("/a/x.rs->(!doc)", &t), vec![5]);
    assert_eq!(run("/a/x.rs->(!(ref || doc))", &t), Vec::<u64>::new());
    assert_eq!(run("/a/x.rs->(ref && !doc)", &t), vec![5]);
    assert_eq!(run("/a/x.rs->(r* && !d*)", &t), vec![5]);
    // every crosslink carries a label: `!*` selects nothing
    assert_eq!(run("/a/x.rs->(!*)", &t), Vec::<u64>::new());
    // the incoming arrow mirrors it
    assert_eq!(run("//w.rs<-(ref && doc)", &t), vec![2]);
    assert_eq!(run("//z.rs<-(!doc)", &t), vec![2]);
    assert_eq!(run("//z.rs<-(ref && doc)", &t), Vec::<u64>::new());
    // a quantifier applies to the hop whatever it carries
    let c = MockTree::cyclic();
    assert_eq!(run("/a/x.rs->(mgr && !x){2}", &c), vec![7]);
    assert_eq!(run("/a/x.rs(->(mgr || x)){2}", &c), vec![7]);
    // the plain alternation is the union under its older spelling
    assert_eq!(
        run("/a/x.rs->(ref|doc)", &t),
        run("/a/x.rs->(ref || doc)", &t)
    );
    // the words of the family spell the operators too
    assert_eq!(run("/a/x.rs->(ref .and. .not. doc)", &t), vec![5]);
}

#[test]
fn dot_wildcard_and_sugar() {
    let t = MockTree::sample();
    // `//` is expressible as a dot-wildcard pattern.
    assert_eq!(run("/b(/.)*/w.rs", &t), run("/b//w.rs", &t));
    // `(/.){2}` — grandchildren; `/{2}` is sugar for it.
    assert_eq!(run("(/.){2}", &t), vec![2, 3, 5, 6]);
    assert_eq!(run("/{2}", &t), run("(/.){2}", &t));
    // A named hop takes a brace quantifier directly.
    let lists = MockTree::lists();
    assert_eq!(run("/div/ul/li(/ul/li){1}", &lists), vec![5]);
    // Outside a pattern, `.` stays a literal name (no such node).
    assert!(run("/.", &t).is_empty());
}

#[test]
fn zero_repetitions_keep_the_anchor() {
    let t = MockTree::sample();
    // b itself (0 reps) plus deep (1 rep).
    assert_eq!(run("/b(/deep)*", &t), vec![4, 6]);
    // {0,1} spells the same reach explicitly.
    assert_eq!(run("/b(/deep){0;1}", &t), vec![4, 6]);
}

#[test]
fn quantifier_bound_refuses_out_loud() {
    /// The lists tree with a quantifier bound of 2.
    struct Bounded(MockTree);
    impl AstAdapter for Bounded {
        fn root(&self) -> NodeId {
            self.0.root()
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            self.0.children(node)
        }
        fn name(&self, node: NodeId) -> Option<String> {
            self.0.name(node)
        }
        fn parent(&self, node: NodeId) -> Option<NodeId> {
            self.0.parent(node)
        }
        fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
            self.0.links(node)
        }
        fn quantifier_bound(&self) -> usize {
            2
        }
    }
    let t = Bounded(MockTree::lists());
    // The third li sits beyond the bound: the walk still has
    // edges to take when the rounds run out, so the closure
    // would be silently incomplete — refused, out loud.
    let refusal = |q: &str| {
        let out = run(q, &t);
        let msg = take_refusal();
        (out, msg)
    };
    let (out, msg) = refusal("/div(/ul/li)+");
    assert!(out.is_empty());
    assert!(msg.unwrap().contains("quantifier bound reached"));
    // An explicit ceiling beyond the bound refuses up front —
    // never a silent clamp, never a silent empty.
    let (out, msg) = refusal("/div(/ul/li){1;3}");
    assert!(out.is_empty());
    assert!(msg.unwrap().contains("explicit ceiling"));
    // A minimum beyond the bound can never be satisfied.
    let (out, msg) = refusal("/div(/ul/li){3;}");
    assert!(out.is_empty());
    assert!(msg.unwrap().contains("minimum"));
    // An explicit ceiling INSIDE the bound is the user asking
    // for exactly that depth: complete at n, silent.
    let (out, msg) = refusal("/div(/ul/li){1;2}");
    assert_eq!(out, vec![3, 5]);
    assert!(msg.is_none());
    // The iteration loop is bounded the same way: an exact
    // count, a ceiling or a minimum beyond the bound refuses,
    // an open loop runs to the bound and stops.
    let looped = |q: &str| match crate::run(q, &t) {
        Ok(QueryResult::Values(vs)) => (vs, take_refusal()),
        Ok(QueryResult::Nodes(ns)) => (
            ns.into_iter().map(|n| Value::Int(n.0 as i64)).collect(),
            take_refusal(),
        ),
        Err(e) => {
            let _ = take_refusal();
            (Vec::new(), Some(e.to_string()))
        }
    };
    let (out, msg) = looped("/div | .n(1){5} | $.n");
    assert!(out.is_empty());
    assert!(msg.unwrap().contains("loop's explicit ceiling"));
    let (out, msg) = looped("/div | .n(1){1;3} | $.n");
    assert!(out.is_empty());
    assert!(msg.unwrap().contains("loop's explicit ceiling"));
    let (out, msg) = looped("/div | .n(1){3;} | $.n");
    assert!(out.is_empty());
    assert!(msg.unwrap().contains("loop's minimum"));
    let (out, msg) = looped("/div | .n(1){1;2} | $.n");
    assert_eq!(out, vec![Value::Int(1)]);
    assert!(msg.is_none());
}

#[test]
fn bound_hit_with_drained_frontier_is_complete() {
    /// The lists tree bounded at exactly its chain depth.
    struct Exact(MockTree);
    impl AstAdapter for Exact {
        fn root(&self) -> NodeId {
            self.0.root()
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            self.0.children(node)
        }
        fn name(&self, node: NodeId) -> Option<String> {
            self.0.name(node)
        }
        fn parent(&self, node: NodeId) -> Option<NodeId> {
            self.0.parent(node)
        }
        fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
            self.0.links(node)
        }
        fn quantifier_bound(&self) -> usize {
            3
        }
    }
    let t = Exact(MockTree::lists());
    // Three rounds reach all three li's and the probe round
    // finds nothing more: the closure is complete — no refusal.
    assert_eq!(run("/div(/ul/li)+", &t), vec![3, 5, 7]);
    assert!(take_refusal().is_none());
}

#[test]
fn proximal_answer_within_bound_is_complete() {
    /// The mgr cycle bounded below its circumference.
    struct Tight(MockTree);
    impl AstAdapter for Tight {
        fn root(&self) -> NodeId {
            self.0.root()
        }
        fn children(&self, node: NodeId) -> Vec<NodeId> {
            self.0.children(node)
        }
        fn name(&self, node: NodeId) -> Option<String> {
            self.0.name(node)
        }
        fn parent(&self, node: NodeId) -> Option<NodeId> {
            self.0.parent(node)
        }
        fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
            self.0.links(node)
        }
        fn quantifier_bound(&self) -> usize {
            2
        }
    }
    let t = Tight(MockTree::cyclic());
    // The nearest tier with a survivor is the whole answer —
    // a live frontier past it is irrelevant, not a truncation.
    assert_eq!(run("//x.rs(->mgr)+?", &t), vec![5]);
    assert!(take_refusal().is_none());
}

#[test]
fn group_in_a_predicate_path() {
    let t = MockTree::sample();
    // x.rs has an outgoing ref chain; y.txt does not.
    assert_eq!(run("/a/*[(->ref)+]", &t), vec![2]);
    let cyc = MockTree::cyclic();
    // every node on the mgr cycle reaches something in two hops
    assert_eq!(run("//*[(->mgr){2}]", &cyc), vec![2, 5, 7]);
}

#[test]
fn group_predicates_filter_before_reach() {
    // mgr cycle: x.rs (2) → z.rs (5) → w.rs (7) → 2
    let t = MockTree::cyclic();
    // nearest satisfying: w.rs is two hops out; without the
    // predicate, +? would stop at z.rs (tier one).
    assert_eq!(run("//x.rs(->mgr)+[:name = w.rs]?", &t), vec![7]);
    assert_eq!(run("//x.rs(->mgr)+?", &t), vec![5]);
    // farthest satisfying
    assert_eq!(run("//x.rs(->mgr)+[:name == (/rs/)]!", &t), vec![7]);
    // no survivor anywhere: empty, not an error
    assert!(run("//x.rs(->mgr)+[:name = \"nope\"]?", &t).is_empty());
    // an unquantified group takes predicates too — endpoints of
    // different-length alternatives, filtered uniformly
    let s = MockTree::sample();
    assert_eq!(run("/b/(z.rs|deep/w.rs)[:name = w.rs]", &s), vec![7]);
    // zero repetitions: the anchor itself must pass
    assert_eq!(run("/a(/x.rs)*[:name = \"a\"]", &s), vec![1]);
    // the walked edge is in scope for the group's predicate
    let w = MockTree::weighted();
    assert_eq!(run("//a(->e)+[$-::qty = 12]", &w), vec![6]);
    // positional predicates refuse
    let toks = lex("/a(/b)+[1]").unwrap();
    assert!(parse(&toks).is_err());
}

#[test]
fn arrived_edges_aggregate() {
    let t = MockTree::converging();
    // deep is reached by two crossings; one capsa, both edges.
    assert_eq!(
        vals("//a->e->e | .(@-::qty) | $.", &t),
        vec![Value::list(vec![Value::Int(12), Value::Int(5)])]
    );
    // bare @- reads the labels
    assert_eq!(
        vals("//a->e->e | .(@-) | $.", &t),
        vec![Value::list(vec![
            Value::Str("e".into()),
            Value::Str("e".into())
        ])]
    );
    // tree walks arrive by [child] edges
    assert_eq!(
        vals("/a | .(@-) | $.", &t),
        vec![Value::list(vec![Value::Str("[child]".into())])]
    );
    // per-path capsae stay distinct when their breadcrumbs
    // differ — one final crossing each; identical breadcrumbs
    // merge, and their crossings aggregate
    assert_eq!(
        vals("//a(->e .($-::qty))->e | .(@-) @| count", &t),
        vec![Value::Int(2)]
    );
    assert_eq!(
        vals("//a(->e .(1))->e | .(@-::qty) | $.", &t),
        vec![Value::list(vec![Value::Int(12), Value::Int(5)])]
    );
    // command form: `| @-::prop` sets the topic directly, and
    // `each` forks it
    assert_eq!(
        vals("//a->e->e | @-::qty | ... @| sum", &t),
        vec![Value::Int(17)]
    );
}

#[test]
fn context_accessor_and_inline_pipes() {
    let t = MockTree::sample();
    // Bare @_ reads the peers' topics; projected, their nodes.
    assert_eq!(
        vals("//*.rs:name | .all((@_ @| join(\", \"))) | $.all", &t),
        vec![
            Value::Str("x.rs, z.rs, w.rs".into()),
            Value::Str("x.rs, z.rs, w.rs".into()),
            Value::Str("x.rs, z.rs, w.rs".into()),
        ]
    );
    // The snapshot rule: a filter's own predicate reads the
    // command's INPUT (all three), not the shrinking output.
    assert_eq!(
        vals("//*.rs | [(@@_:name @| count) = 3] @| count", &t),
        vec![Value::Int(3)]
    );
    // Inline | mirrors per-capsa semantics (list reduction);
    // inline @| explodes a lone list into a context.
    assert_eq!(
        vals("/a/x.rs | .n(((@@_:name | count))) | $.n", &t),
        vec![Value::Int(1)]
    );
    // Capsa-preserving aggregates ride the inline pipe too.
    assert_eq!(
        vals(
            "//*.rs | .top((@@_:name @| sort @| [1..2] @| join(\"-\"))) | $.top",
            &t
        )[0],
        Value::Str("w.rs-x.rs".into())
    );
    // Outside any context (navigation predicates), @_ is null.
    assert_eq!(run("/*[@_ = null]", &t).len(), 2);
    // Pushes refuse inline.
    let toks = lex("/a | .x((@_ | .bad))").unwrap();
    assert!(parse(&toks).is_err());
}

#[test]
fn conditionals() {
    let t = MockTree::sample();
    // basic selection by truthiness
    assert_eq!(
        vals(
            "/a/* | %(:name; kind = (:name == (/rs/) ? \"code\" : \"text\"))",
            &t
        ),
        vec![
            Value::Record(vec![
                ("name".into(), Value::Str("x.rs".into())),
                ("kind".into(), Value::Str("code".into())),
            ]),
            Value::Record(vec![
                ("name".into(), Value::Str("y.txt".into())),
                ("kind".into(), Value::Str("text".into())),
            ]),
        ]
    );
    // chained else — the multi-branch form, no inner parens
    assert_eq!(
        vals(
            "//w.rs | ((:depth = 1 ? \"top\" : :depth = 2 ? \"mid\" : \"deep\"))",
            &t
        ),
        vec![Value::Str("deep".into())]
    );
    // only the taken branch evaluates: the untaken side would
    // be null-valued, not an error, and structural conditions
    // work in the condition
    assert_eq!(
        vals("/b | ((/deep ? \"has-deep\" : \"flat\"))", &t),
        vec![Value::Str("has-deep".into())]
    );
    // composes with pipe tails
    assert_eq!(
        vals("/a | (((1 = 1 ? \"yes\" : \"no\") | upper))", &t),
        vec![Value::Str("YES".into())]
    );
    // null condition is falsy
    assert_eq!(
        vals("/a | (($_ ? \"topic\" : \"none\"))", &t),
        vec![Value::Str("none".into())]
    );
}

#[test]
fn trait_negation() {
    let t = MockTree::sample();
    // files that are not code
    assert_eq!(run("//*<file && !code>", &t), vec![3]);
    // traitless nodes (the directories)
    assert_eq!(run("/*<!*>", &t), vec![1, 4]);
    // negation inside a disjunction: text or non-file — all
    // but the .rs files
    assert_eq!(run("/a/*<text || !file>", &t), vec![3]);
    // parenthesized algebra, CNF underneath
    assert_eq!(run("//*<(code || text) && !text>", &t), vec![2, 5, 7]);
}

#[test]
fn map_pipe() {
    // deep is reached by two weighted crossings: qty [12, 5].
    let t = MockTree::converging();
    // map: transform each element, capsae unchanged
    assert_eq!(
        vals("//a->e->e | .(@-::qty) | $. $| ($_ * 10) | json", &t),
        vec![Value::Str("[120, 50]".into())]
    );
    // filter elements ($_ = the element): the comprehension WHERE
    assert_eq!(
        vals("//a->e->e | .(@-::qty) | $. $| [$_ > 6] | json", &t),
        vec![Value::Str("[12]".into())]
    );
    // positional slice
    assert_eq!(
        vals("//a->e->e | .(@-::qty) | $. $| [1..1] | json", &t),
        vec![Value::Str("[12]".into())]
    );
    // @_ inside a map reads the ELEMENT context: self-normalize
    assert_eq!(
        vals(
            "//a->e->e | .(@-::qty) | $. $| (($_ * 100 div (@_ @| sum) | round)) | json",
            &t
        ),
        vec![Value::Str("[71, 29]".into())]
    );
    // null maps to null; a non-list degenerates to plain |
    assert_eq!(vals("/a | $_ $| upper", &t), vec![Value::Null]);
    assert_eq!(
        vals("/a | :name $| upper", &t),
        vec![Value::Str("A".into())]
    );
    // pushes refuse inside a map
    let toks = lex("/a | @. $| .bad").unwrap();
    assert!(parse(&toks).is_err());
}

#[test]
fn simple_path_dedup_is_by_visited_set() {
    // a → {x.rs, y.txt} → deep → x.rs (all labeled `e`): two
    // routes meet at deep with different histories; only the
    // y.txt route may continue back to x.rs. Deduping in-flight
    // paths by node alone would drop one history at deep and
    // could lose the count-3 match.
    let t = MockTree::diamond();
    assert_eq!(run("//a(->e)+", &t), vec![2, 3, 6]);
    // the longest simple path: a → y.txt → deep → x.rs
    assert_eq!(run("//a(->e)+!", &t), vec![2]);
}

#[test]
fn edge_accessor() {
    let t = MockTree::weighted();
    // `$-::prop` filters BY the edge in the hop's own predicate:
    // a's heavy outgoing edges only.
    assert_eq!(run("//a->e[$-::qty > 1]", &t), vec![2]);
    // Bare `$-` reads the label.
    assert_eq!(run("//a->*[$- = \"e\"]", &t), vec![2, 3]);
    // Incoming direction: the walked edge points into the node.
    assert_eq!(run("//deep<-e[$-::qty = 12]", &t), vec![2]);
    // Null where no edge is in scope: nothing has walked one.
    assert!(run("/a[$-::qty = 4]", &t).is_empty());
    // Edge property in a projection-bearing predicate command after
    // the hop is NOT in scope (the edge belongs to the hop).
    assert_eq!(
        vals("//a->e[$-::qty > 1]:name", &t),
        vec![Value::Str("x.rs".into())]
    );
}

#[test]
fn pattern_breadcrumbs() {
    let t = MockTree::weighted();
    // Push the walked edge's property; read it back per path.
    // a -4-> x.rs -12-> deep: one path, register [4, 12].
    assert_eq!(
        vals(
            "//a(->e .($-::qty))+ | [:name = \"deep\"] | @. | product",
            &t
        ),
        vec![Value::Int(48)]
    );
    // Bare $- pushes the label; tree hops push [child].
    assert_eq!(
        vals("(/a .($-)){1} | $.", &t),
        vec![Value::Str("[child]".into())]
    );
    // Depth as a value: count the breadcrumbs.
    assert_eq!(
        vals("//a(->e .(1))+! | @. | count", &t),
        vec![Value::Int(2)]
    );
    // A named push lands as a named regula. (Spaced: glued
    // `.q(` would lex into the matcher name, exactly as
    // `/x.rs(...)` keeps its filename.)
    assert_eq!(
        vals("//a(->e .q($-::qty)){1;2}! | $.q", &t),
        vec![Value::Int(12)]
    );
}

#[test]
fn per_path_results() {
    // The diamond: two routes from a to deep, each its own
    // result capsa once the pattern pushes.
    let t = MockTree::diamond();
    assert_eq!(
        vals(
            "//a(->e .(:name))+ | [:name = \"deep\"] | @. | join(\"-\")",
            &t
        ),
        vec![
            Value::Str("x.rs-deep".into()),
            Value::Str("y.txt-deep".into()),
        ]
    );
    // Without pushes the same walk dedups by node, as before.
    assert_eq!(run("//a(->e)+ | [:name = \"deep\"]", &t), vec![6]);
}

#[test]
fn structural_edge_labels() {
    let t = MockTree::sample();
    // Every hop defines $-: tree, sibling, and reach walks.
    assert_eq!(run("/a[$- = \"[child]\"]", &t), vec![1]);
    assert_eq!(run("//w.rs\\deep[$- = \"[parent]\"]", &t), vec![6]);
    assert_eq!(run("/a/x.rs>y.txt[$- = \"[next]\"]", &t), vec![3]);
    assert_eq!(run("/a/y.txt<x.rs[$- = \"[prev]\"]", &t), vec![2]);
    // Before any hop, $- is null (nothing walked).
    assert!(run("/a[$- = \"[parent]\"]", &t).is_empty());
}

#[test]
fn pipelines_and_union() {
    let t = MockTree::sample();
    // count all .rs files (aggregation via @|)
    assert_eq!(vals("//*.rs @| count", &t), vec![Value::Int(3)]);
    // names, sorted and joined
    assert_eq!(
        vals("//*.rs:name @| sort @| join(\", \")", &t),
        vec![Value::Str("w.rs, x.rs, z.rs".into())]
    );
    // per-capsa transform: uppercase each name
    assert_eq!(
        vals("//x.rs:name | upper", &t),
        vec![Value::Str("X.RS".into())]
    );
    // union of two node sets: x.rs and z.rs
    assert_eq!(run("//x.rs || //z.rs", &t), vec![2, 5]);
    // aggregation applies to the whole union: count = 2
    assert_eq!(vals("//x.rs || //z.rs @| count", &t), vec![Value::Int(2)]);
}

#[test]
fn registers_and_subcontexts() {
    let t = MockTree::sample();
    // subcontext: count of .rs descendants per top-level dir
    // a has x.rs (1 rs); b has z.rs and w.rs (2 rs)
    assert_eq!(
        vals("/*[:name = \"a\"] | .(//*.rs @| count) | $.", &t),
        vec![Value::Int(1)]
    );
    assert_eq!(
        vals("/*[:name = \"b\"] | .(//*.rs @| count) | $.", &t),
        vec![Value::Int(2)]
    );
    // push then recall a named breadcrumb
    assert_eq!(
        vals("//x.rs:name | .saved | upper | $.saved", &t),
        vec![Value::Str("x.rs".into())]
    );
}

#[test]
fn predicates() {
    let t = MockTree::sample();
    // dirs with exactly 2 children: a and b
    assert_eq!(run("/*[:n-children = 2]", &t), vec![1, 4]);
    // structural condition: root children that have a z.rs child -> b
    assert_eq!(run("/*[/z.rs]", &t), vec![4]);
    // index predicate: first child of a -> x.rs
    assert_eq!(run("/a/*[1]", &t), vec![2]);
    // negation: non-leaf children of root -> a, b
    assert_eq!(run("/*[!:is-leaf]", &t), vec![1, 4]);
    // and + regex match on the name
    assert_eq!(
        run("//*[:is-leaf && :name == (/rs/)]", &t),
        vec![2, 5, 7]
    );
    // comparison against a literal, via core depth
    assert_eq!(run("//*[:depth > 2]", &t), vec![7]);
}

/// `[n]` selects by position among the hop's results (per source
/// node, as filtered by the brackets to its left) — not by
/// sibling position, which is `[:index = n]`.
#[test]
fn index_predicate_positions_among_hop_results() {
    // (root) ├ h ├ p ├ p └ sect ├ title ├ p └ p
    let names = vec![
        None,                 // 0 root
        Some("h".into()),     // 1
        Some("p".into()),     // 2
        Some("p".into()),     // 3
        Some("sect".into()),  // 4
        Some("title".into()), // 5
        Some("p".into()),     // 6
        Some("p".into()),     // 7
    ];
    let mut kids = HashMap::new();
    kids.insert(0, vec![1, 2, 3, 4]);
    kids.insert(4, vec![5, 6, 7]);
    let t = MockTree {
        names,
        kids,
        loose: Vec::new(),
        links: HashMap::new(),
        edge_props: HashMap::new(),
        prov: HashMap::new(),
    };
    // the first p child of the root — even though no p is the
    // root's first *child*
    assert_eq!(run("/p[1]", &t), vec![2]);
    assert_eq!(run("/p[2]", &t), vec![3]);
    assert_eq!(run("/sect/p[2]", &t), vec![7]);
    // a descendant hop yields one list per source node, so a
    // root-anchored //p[1] is the first p in the whole tree
    assert_eq!(run("//p[1]", &t), vec![2]);
    assert_eq!(run("//p[4]", &t), vec![7]);
    // sibling position is the metadata predicate
    assert_eq!(run("/p[:index = 2]", &t), vec![2]);
    assert_eq!(run("/*[:index = 3]", &t), vec![3]);
    assert_eq!(run("/p[:index = 1]", &t), Vec::<u64>::new());
    // brackets apply left to right: filter-then-index differs
    // from index-then-filter
    assert_eq!(run("/*[:index > 1][1]", &t), vec![2]);
    assert_eq!(run("/*[1][:index > 1]", &t), Vec::<u64>::new());
    // negative indexes count from the end of the same list
    assert_eq!(run("/p[-1]", &t), vec![3]);
    assert_eq!(run("//p[-1]", &t), vec![7]);
    assert_eq!(run("//p[-4]", &t), vec![2]);
    assert_eq!(run("/*[:index > 1][-1]", &t), vec![4]);
    // out of range (including 0) selects nothing
    assert_eq!(run("//p[0]", &t), Vec::<u64>::new());
    assert_eq!(run("//p[5]", &t), Vec::<u64>::new());
    assert_eq!(run("//p[-5]", &t), Vec::<u64>::new());
}

/// `[a..b]` keeps the inclusive positional span of the hop's
/// results; either end may be omitted or negative, and ends
/// clamp to the list.
#[test]
fn range_predicates() {
    let names = vec![
        None,                 // 0 root
        Some("h".into()),     // 1
        Some("p".into()),     // 2
        Some("p".into()),     // 3
        Some("sect".into()),  // 4
        Some("title".into()), // 5
        Some("p".into()),     // 6
        Some("p".into()),     // 7
    ];
    let mut kids = HashMap::new();
    kids.insert(0, vec![1, 2, 3, 4]);
    kids.insert(4, vec![5, 6, 7]);
    let t = MockTree {
        names,
        kids,
        loose: Vec::new(),
        links: HashMap::new(),
        edge_props: HashMap::new(),
        prov: HashMap::new(),
    };
    // //p matches nodes 2, 3, 6, 7 in document order
    assert_eq!(run("//p[2..3]", &t), vec![3, 6]);
    assert_eq!(run("//p[2..]", &t), vec![3, 6, 7]);
    assert_eq!(run("//p[..2]", &t), vec![2, 3]);
    assert_eq!(run("//p[2..-1]", &t), vec![3, 6, 7]);
    assert_eq!(run("//p[..-2]", &t), vec![2, 3, 6]);
    // ends clamp; an empty span selects nothing
    assert_eq!(run("//p[..9]", &t), vec![2, 3, 6, 7]);
    assert_eq!(run("//p[5..]", &t), Vec::<u64>::new());
    assert_eq!(run("//p[3..2]", &t), Vec::<u64>::new());
    // ranges compose sequentially with filters
    assert_eq!(run("/*[..2][:index > 1]", &t), vec![2]);
    assert_eq!(run("/*[:index > 1][..2]", &t), vec![2, 3]);
}

/// Value expressions: arithmetic over numeric readings, with
/// conventional precedence, null propagation, and exact integer
/// arithmetic (spec: Value Expressions and Arithmetic).
#[test]
fn value_expressions() {
    let t = MockTree::sample();
    // literals, precedence, grouping (root children: a, b)
    assert_eq!(run("/*[1 + 2 * 3 = 7]", &t), vec![1, 4]);
    assert_eq!(run("/*[(1 + 2) * 3 = 9]", &t), vec![1, 4]);
    // metadata operands: a is the first child, b the second
    assert_eq!(run("/*[:index + 1 = 2]", &t), vec![1]);
    assert_eq!(run("/*[:index * 2 = 4]", &t), vec![4]);
    assert_eq!(run("/*[:index - 2 = -1]", &t), vec![1]);
    assert_eq!(run("/*[- :index = -2]", &t), vec![4]);
    // div is always a float; idiv truncates; mod follows the
    // dividend's sign
    assert_eq!(run("/*[7 div 2 = 3.5]", &t), vec![1, 4]);
    assert_eq!(run("/*[7 idiv 2 = 3]", &t), vec![1, 4]);
    assert_eq!(run("/*[7 mod 2 = 1]", &t), vec![1, 4]);
    assert_eq!(run("/*[0 - 7 mod 2 = -1]", &t), vec![1, 4]);
    // integer arithmetic is exact beyond float precision
    // (2^53 + 1 would round away under f64)
    assert_eq!(
        run("/*[9007199254740993 + 0 = 9007199254740993]", &t),
        vec![1, 4]
    );
    // division by zero gives inf or nan, both truthy as IEEE floats;
    // non-numeric operands are null, and null is falsy
    assert_eq!(run("/*[1 div 0]", &t), run("/*", &t));
    assert_eq!(run("/*[1 idiv 0]", &t), run("/*", &t));
    assert_eq!(run("/*[:name * 2]", &t), Vec::<u64>::new());
    // a glued minus stays a name character: no such property,
    // so the comparison selects nothing (but parses)
    assert_eq!(run("/*[::a-b = 1]", &t), Vec::<u64>::new());
    // boolean groups in operand position keep their old meaning
    assert_eq!(
        run("/*[(:index = 1 || :index = 2) && :is-leaf]", &t),
        Vec::<u64>::new()
    );
}

/// Value expressions as pipeline commands: computed topics and
/// computed (pushed) columns.
#[test]
fn value_expression_commands() {
    let t = MockTree::sample();
    // computed topic on one node: x.rs is child 1 of a
    assert_eq!(vals("/a/x.rs | :index * 2", &t), vec![Value::Int(2)]);
    // a computed column pushed per capsa, then aggregated:
    // indexes 1 and 2 doubled sum to 6
    assert_eq!(
        vals("/* | .double(:index * 2) | $.double @| sum", &t),
        vec![Value::Int(6)]
    );
    // grouped expression command
    assert_eq!(
        vals("/a/x.rs | (:index + 1) * 3", &t),
        vec![Value::Int(6)]
    );
}

/// Keyed aggregates reorder or filter capsae by per-capsa keys,
/// preserving node identity so later commands keep working.
/// `@| group(k) | count` runs fused (no member lists built);
/// it must answer exactly as the two commands do — same counts,
/// same key fields for recall, same order — whether the count
/// is the result or feeds a later command.
#[test]
fn fused_group_count_matches_the_two_commands() {
    let t = MockTree::sample();
    let fused = vals("//* @| group(d = :depth) | count", &t);
    // The unfused route: the group's list topic, counted inside
    // an operand, so no `| count` command follows the group.
    let unfused = vals("//* @| group(d = :depth) | %(n = ($_ @| count)) | :n", &t);
    assert_eq!(fused, unfused);
    assert!(fused.len() > 1);
    assert_eq!(
        fused
            .iter()
            .map(|v| v.to_string().parse::<i64>().unwrap())
            .sum::<i64>(),
        vals("//* @| count", &t)[0]
            .to_string()
            .parse::<i64>()
            .unwrap()
    );
    // The count then feeding a record with the recalled key.
    let recs = vals("//* @| group(d = :depth) | count | %($.d; n = $_)", &t);
    let refs = vals("//* @| group(d = :depth) | %($.d; n = ($_ @| count))", &t);
    assert_eq!(recs, refs);
    // A filter on the count keeps the fused capsae's registers.
    assert_eq!(
        vals("//* @| group(d = :depth) | count | [$_ > 1] | $.d", &t),
        vals(
            "//* @| group(d = :depth) | %(d = $.d; n = ($_ @| count)) | [:n > 1] | :d",
            &t
        )
    );
}

#[test]
fn keyed_aggregates() {
    let t = MockTree::sample();
    // sort leaves by name: w.rs, x.rs, y.txt, z.rs
    assert_eq!(
        vals("//*[:is-leaf] @| sort_by(:name) | :name", &t),
        vec![
            Value::Str("w.rs".into()),
            Value::Str("x.rs".into()),
            Value::Str("y.txt".into()),
            Value::Str("z.rs".into()),
        ]
    );
    // descending via reverse; positional selection takes top-2
    assert_eq!(
        vals(
            "//*[:is-leaf] @| sort_by(:name) @| reverse @| [..2] | :name",
            &t
        ),
        vec![Value::Str("z.rs".into()), Value::Str("y.txt".into())]
    );
    // ... which is what top does in one call
    assert_eq!(
        vals("//*[:is-leaf] @| top(2, :name) | :name", &t),
        vec![Value::Str("z.rs".into()), Value::Str("y.txt".into())]
    );
    assert_eq!(
        vals("//*[:is-leaf] @| bottom(1, :name) | :name", &t),
        vec![Value::Str("w.rs".into())]
    );
    // extreme-key selection keeps node identity (w.rs is node 7)
    assert_eq!(run("//*[:is-leaf] @| max_by(:depth)", &t), vec![7]);
    // ... and every tied capsa: both depth-1 nodes
    assert_eq!(run("//* @| min_by(:depth)", &t), vec![1, 4]);
    // unique_by keeps the first capsa per distinct key
    assert_eq!(run("//* @| unique_by(:depth)", &t), vec![1, 2, 7]);
    // composite keys tie-break
    assert_eq!(
        vals(
            "//*[:is-leaf] @| sort_by(:depth, :name) | :name",
            &t
        )[0],
        Value::Str("x.rs".into())
    );
}

/// Positional selection (`@| [n]`, whole-context) and per-capsa
/// filters (`| [cond]`) as pipeline commands, each on its
/// doctrinally correct pipe.
#[test]
fn positional_and_filter_commands() {
    let t = MockTree::sample();
    assert_eq!(run("//*.rs @| [1]", &t), vec![2]);
    assert_eq!(run("//*.rs @| [-1]", &t), vec![7]);
    assert_eq!(run("//*.rs @| [2..]", &t), vec![5, 7]);
    assert_eq!(
        vals("//*.rs @| [..2] | :name", &t),
        vec![Value::Str("x.rs".into()), Value::Str("z.rs".into())]
    );
    // per-capsa filter: each capsa tested against its node
    assert_eq!(run("//* | [:is-leaf]", &t), vec![2, 3, 5, 7]);
    assert_eq!(
        run("//*.rs @| sort_by(:name) | [:depth > 2]", &t),
        vec![7]
    );
    // `| [1]` parses (it is a row of a table, spec: The Table Kind)
    // and refuses at the run on anything else, pointing at `@| [n]`
    assert!(parse(&lex("//* | [1]").unwrap()).is_ok());
    assert!(parse(&lex("//* @| [:is-leaf]").unwrap()).is_err());
    // ...and the inline `@|` enforces the same discipline
    assert!(parse(&lex("//a | ((::x @| [:is-leaf] @| count))").unwrap()).is_err());
    assert!(parse(&lex("//a | ((::x @| bogus))").unwrap()).is_err());
}

/// The `@_` snapshot must materialize wherever the command reads
/// it: inside conditional/match operands, and in keyed-aggregate
/// keys (a silent `peers: None` there made `@_` null — and a
/// null key excludes every capsa).
#[test]
fn context_snapshot_in_cond_and_agg_keys() {
    let t = MockTree::sample();
    assert_eq!(
        vals("//*.rs | ((@_ @| count) > 2 ? \"many\" : \"few\")", &t),
        vec![
            Value::Str("many".into()),
            Value::Str("many".into()),
            Value::Str("many".into()),
        ]
    );
    // Keys (@_ @| count) - $ord = 2, 1, 0 — the minimum is the
    // third file; with `@_` null every capsa would drop.
    assert_eq!(run("//*.rs @| min_by((@_ @| count) - $ord)", &t), vec![7]);
}

#[test]
fn core_metadata_projection() {
    let t = MockTree::sample();
    assert_eq!(
        vals("/a/*:name", &t),
        vec![Value::Str("x.rs".into()), Value::Str("y.txt".into())]
    );
    assert_eq!(vals("/a:n-children", &t), vec![Value::Int(2)]);
    assert_eq!(vals("/b/deep:depth", &t), vec![Value::Int(2)]);
    assert_eq!(vals("/a/x.rs:is-leaf", &t), vec![Value::Bool(true)]);
    assert_eq!(vals("/b:is-leaf", &t), vec![Value::Bool(false)]);
    assert_eq!(vals("/a/y.txt:index", &t), vec![Value::Int(2)]);
    // parent metadata and level flags
    assert_eq!(vals("/a/x.rs:parent-id", &t), vec![Value::Int(1)]);
    assert_eq!(vals("/a/x.rs:parent-index", &t), vec![Value::Int(1)]);
    assert_eq!(vals("/a:is-top-level", &t), vec![Value::Bool(true)]);
    assert_eq!(vals("/a/x.rs:is-top-level", &t), vec![Value::Bool(false)]);
    // root-relative location paths (root contributes no segment)
    assert_eq!(
        vals("//w.rs:name-path", &t),
        vec![Value::Str("/b/deep/w.rs".into())]
    );
    assert_eq!(
        vals("//w.rs:index-path", &t),
        vec![Value::Str("/2/2/1".into())]
    );
    assert_eq!(
        vals("//w.rs:id-path", &t),
        vec![Value::Str("/4/6/7".into())]
    );
}

#[test]
fn provenance_core_metadata() {
    let mut t = MockTree::sample();
    // x.rs carries a full triple; y.txt only a source; z.rs only
    // a dpid; everything else answers null.
    let (secs, nanos, off) = crate::temporal::parse_iso("2026-04-07T02:00:01Z").unwrap();
    t.prov.insert(
        2,
        Provenance {
            source: Some("api-gateway".into()),
            instant: Some((secs, nanos, off)),
            dpid: Some("request-42".into()),
            ..Default::default()
        },
    );
    t.prov.insert(
        3,
        Provenance {
            source: Some("api-gateway".into()),
            ..Default::default()
        },
    );
    t.prov.insert(
        5,
        Provenance {
            dpid: Some("row-7".into()),
            ..Default::default()
        },
    );
    // Components, typed.
    assert_eq!(
        vals("/a/x.rs:source", &t),
        vec![Value::Str("api-gateway".into())]
    );
    assert_eq!(
        vals("/a/x.rs:instant", &t),
        vec![Value::Instant {
            secs,
            nanos,
            offset_min: off,
            calendar: Default::default()
        }]
    );
    assert_eq!(
        vals("/a/x.rs:dpid", &t),
        vec![Value::Str("request-42".into())]
    );
    // The composite follows kaiv's optionality grammar.
    assert_eq!(
        vals("/a/x.rs:provenance", &t),
        vec![Value::Str(
            "?api-gateway@2026-04-07T02:00:01Z#request-42".into()
        )]
    );
    assert_eq!(
        vals("/a/y.txt:provenance", &t),
        vec![Value::Str("?api-gateway".into())]
    );
    assert_eq!(
        vals("/b/z.rs:provenance", &t),
        vec![Value::Str("?#row-7".into())]
    );
    // Nulls where nothing is recorded.
    assert_eq!(vals("/b/deep:source", &t), vec![Value::Null]);
    assert_eq!(vals("/b/deep:instant", &t), vec![Value::Null]);
    assert_eq!(vals("/b/deep:provenance", &t), vec![Value::Null]);
    // Typed temporal comparison in a criterion.
    assert_eq!(run("//*[:instant > 2020-01-01]", &t), vec![2]);
    assert_eq!(run("//*[:instant > 2030-01-01]", &t), Vec::<u64>::new());
    // Layering: inner wins per component, outer fills the rest.
    let inner = Provenance {
        source: Some("leaf".into()),
        ..Default::default()
    };
    let outer = Provenance {
        source: Some("mount".into()),
        dpid: Some("d1".into()),
        ..Default::default()
    };
    let merged = inner.or(outer);
    assert_eq!(merged.source.as_deref(), Some("leaf"));
    assert_eq!(merged.dpid.as_deref(), Some("d1"));
    assert!(merged.instant.is_none());
    assert_eq!(Provenance::default().canonical(), None);
}

#[test]
fn ancestor_reach() {
    let t = MockTree::sample();
    // ancestors of w.rs that are directories: deep(1), b(2)
    assert_eq!(run("//w.rs\\\\*", &t), vec![6, 4, 0]);
    assert_eq!(run("//w.rs\\\\?*", &t), vec![6]); // nearest
    assert_eq!(run("//w.rs\\\\!*", &t), vec![0]); // farthest (root)
    // the bare ascent, and the two tautologies: the nearest
    // ancestor is the parent, the farthest is the root
    assert_eq!(run("//w.rs\\\\", &t), run("//w.rs\\\\*", &t));
    assert_eq!(run("//w.rs\\\\?", &t), run("//w.rs\\", &t));
    assert_eq!(run("//w.rs\\\\!", &t), run("^", &t));
    assert_eq!(run("//w.rs/..", &t), run("//w.rs\\", &t));
    assert_eq!(run("//w.rs/...", &t), run("//w.rs\\\\", &t));
}

#[test]
fn marks() {
    let t = MockTree::sample();
    // Mark mid-path; recall in a later step predicate — the
    // intra-path back-reference (Cypher's node variables).
    assert_eq!(
        run("/a .a /*[:name = $$.a:name]", &t),
        Vec::<u64>::new()
    );
    assert_eq!(run("//deep .d /w.rs[$$.d:name = \"deep\"]", &t), vec![7]);
    // Recall in a pipeline command operand.
    assert_eq!(
        vals("//w.rs .w \\\\!* | $$.w:name", &t),
        vec![Value::Str("w.rs".into())]
    );
    // Marks inside patterns: recall in a group predicate sees
    // the walk's own marks.
    assert_eq!(
        run("/a/x.rs .x (->ref)[$$.x:name = \"x.rs\"]", &t),
        vec![7]
    );
    // The context-typed push: bare .name in a SCALAR context
    // still feeds the register, not the marks.
    assert_eq!(vals("//*.rs @| count | .n | $.n", &t), vec![Value::Int(3)]);
    // An unset mark yields nothing (predicate false), not an
    // error.
    assert_eq!(run("/a/*[$$.nope:name = \"a\"]", &t), Vec::<u64>::new());
    // Marks seed sub-navigation: an anchored operand's NESTED
    // predicate still sees the thread's marks...
    assert_eq!(
        run("/a/x.rs .x | [^//w.rs[$$.x:name = \"x.rs\"]]", &t),
        vec![2]
    );
    // ...and a subcontext body's branch can anchor on them.
    assert_eq!(
        vals("/a/x.rs .x | .n($$.x:name) | $.n", &t),
        vec![Value::Str("x.rs".into())]
    );
}

#[test]
fn shell_command() {
    let t = MockTree::sample();
    // Gated: without allow_shell the run family refuses.
    let q = "/a/x.rs | :name | `tr a-z A-Z`";
    assert!(crate::run(q, &t).is_err());
    // Allowed: topic through the command; failure nulls.
    struct Shelly(MockTree);
    impl AstAdapter for Shelly {
        fn root(&self) -> NodeId {
            self.0.root()
        }
        fn children(&self, n: NodeId) -> Vec<NodeId> {
            self.0.children(n)
        }
        fn name(&self, n: NodeId) -> Option<String> {
            self.0.name(n)
        }
        fn parent(&self, n: NodeId) -> Option<NodeId> {
            self.0.parent(n)
        }
        fn links(&self, n: NodeId) -> Vec<(String, NodeId)> {
            self.0.links(n)
        }
    }
    let t = Shelly(MockTree::sample());
    let shell = crate::Run {
        allow_shell: true,
        ..Default::default()
    };
    let run_shell = |q: &str| {
        crate::Query::parse(q, &t)
            .unwrap()
            .run(&t, &shell)
            .map(|o| o.result)
    };
    let got = match run_shell(q).unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert_eq!(got, vec![Value::Str("X.RS".into())]);
    let got = match run_shell("/a/x.rs | :name | `false`").unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert_eq!(got, vec![Value::Null]);
}

#[test]
fn node_mode_json() {
    let t = MockTree::sample();
    // Serialize a subtree: x.rs has no children (leaf) → its
    // scalar; a.rs's parent 'a' has children → object.
    let out = match crate::run("/a | json", &t).unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert_eq!(out.len(), 1);
    assert!(out[0].to_string().contains("x.rs"));
    // decode(json): array → list, spread into rows.
    let out = match crate::run("^ | \"[1,2,3]\" | decode(json) | ...", &t).unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert_eq!(out, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    // XML: | xml serializes a subtree; decode(xml) → value.
    let out = match crate::run("/a | xml", &t).unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert!(out[0].to_string().starts_with("<a>") && out[0].to_string().contains("x.rs"));
    let out = match crate::run("^ | \"<r><x>1</x></r>\" | decode(xml) | json", &t).unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert_eq!(out, vec![Value::Str("{\"x\": \"1\"}".into())]);
    // Object → record; round-trips through json.
    let out = match crate::run("^ | \"{\\\"a\\\":1}\" | decode(json) | json", &t).unwrap() {
        QueryResult::Values(vs) => vs,
        _ => panic!("values"),
    };
    assert_eq!(out, vec![Value::Str("{\"a\": 1}".into())]);
}

#[test]
fn value_match() {
    let t = MockTree::sample();
    // Equality arms; scrutinee evaluated once; else required.
    assert_eq!(
        vals("/a/* | (:name ?= \"x.rs\" ? 1 : \"y.txt\" ? 2 : 0)", &t),
        vec![Value::Int(1), Value::Int(2)]
    );
    // A regex arm tests == instead of equality.
    assert_eq!(
        vals(
            "//*.rs @| [..1] | (:name ?= (/^x/) ? \"ex\" : \"other\")",
            &t
        ),
        vec![Value::Str("ex".into())]
    );
    // No arm hits: the else.
    assert_eq!(
        vals("/a/*[1] | (:name ?= \"nope\" ? 1 : 9)", &t),
        vec![Value::Int(9)]
    );
}

#[test]
fn anchored_operands() {
    let t = MockTree::sample();
    // Same-name-as-some-node-under-/a: compares existentially
    // against a set gathered from the root, not the candidate.
    assert_eq!(run("//*[:name = ^/a/*:name]", &t), vec![2, 3]);
    // Structural: keep nodes only if the arbor has a w.rs
    // anywhere — true from every candidate.
    assert_eq!(run("/a/*[^//w.rs]", &t), vec![2, 3]);
    // Inline pipe with an anchored head: the whole-tree count,
    // readable from any capsa.
    assert_eq!(
        vals("/a/*[1] | (^//*.rs @| count)", &t),
        vec![Value::Int(3)]
    );
}

#[test]
fn outer_spread() {
    let t = MockTree::sample();
    // x.rs has a ref (to w.rs); y.txt has none. The plain
    // spread drops the refless thread; the outer spread keeps
    // it with a null topic.
    assert_eq!(
        vals("/a/* | .m(->ref:name) | $.m | ...", &t),
        vec![Value::Str("w.rs".into())]
    );
    assert_eq!(
        vals("/a/* | .m(->ref:name) | $.m | ...?", &t),
        vec![Value::Str("w.rs".into()), Value::Null]
    );
}

#[test]
fn outer_correlation() {
    let t = MockTree::sample();
    // Rows: every .rs file (the driver). Joined: /a's children
    // with a matching name. Only x.rs finds a partner; the
    // outer marker keeps the other rows with a null witness.
    let q = "//*.rs <=> /a/*[:name = $$0:name]";
    assert_eq!(run(q, &t), vec![2]);
    let q = "//*.rs <=>? /a/*[:name = $$0:name]";
    assert_eq!(run(q, &t), vec![2, 5, 7]);
    // The witness reads back: the partner's name, or null.
    assert_eq!(
        vals("//*.rs <=>? /a/*[:name = $$0:name] | $$1:name", &t),
        vec![Value::Str("x.rs".into()), Value::Null, Value::Null]
    );
    // The anti-join: a pipeline filter is the WHERE clause,
    // evaluated under the capsa's witness (null-propagating),
    // so testing the null slot keeps only the unmatched rows.
    let q = "//*.rs <=>? /a/*[:name = $$0:name] | [!$$1:name]";
    assert_eq!(run(q, &t), vec![5, 7]);
    // And the matched-only filter is its complement.
    let q = "//*.rs <=>? /a/*[:name = $$0:name] | [$$1:name]";
    assert_eq!(run(q, &t), vec![2]);
}

#[test]
fn outer_correlation_indirect_ctx() {
    let t = MockTree::sample();
    // A correlated reference reached *indirectly* — through a
    // value match's arm, or a nested step predicate — is still
    // part of the entry's ON clause, skipped under the outer
    // join's null binding exactly as a bare one is. Otherwise
    // the unmatched rows (z.rs, w.rs) are wrongly dropped, and
    // semantically equivalent spellings diverge.
    let q = "//*.rs <=>? /a/*[(:name ?= $$0:name ? 1 : 0) = 1]";
    assert_eq!(run(q, &t), vec![2, 5, 7]);
    // The same reference, buried in a nested step predicate
    // (`$$` inside the `/*` step's `[...]`). /a's children are
    // leaves, so no real binding admits any driver; only the
    // null binding keeps them.
    let q = "//*.rs <=>? /a/*[/*[$$0:name = :name]]";
    assert_eq!(run(q, &t), vec![2, 5, 7]);
}

#[test]
fn now_operand() {
    let t = MockTree::sample();
    // The default (library) adapter binds no invocation instant,
    // so now() reads as null.
    assert_eq!(vals("^ | (now())", &t), vec![Value::Null]);

    // A runner that pins the instant: now() denotes it, displayed
    // as UTC (offset_min = Some(0)).
    struct Clocked(MockTree, i64, u32);
    impl AstAdapter for Clocked {
        fn root(&self) -> NodeId {
            self.0.root()
        }
        fn children(&self, n: NodeId) -> Vec<NodeId> {
            self.0.children(n)
        }
        fn name(&self, n: NodeId) -> Option<String> {
            self.0.name(n)
        }
        fn parent(&self, n: NodeId) -> Option<NodeId> {
            self.0.parent(n)
        }
    }
    let (s, n, _) = crate::temporal::parse_iso("2026-07-12T09:00:00Z").unwrap();
    let t = Clocked(MockTree::sample(), s, n);
    let clocked = Run {
        now: Some((t.1, t.2)),
        ..Default::default()
    };
    let got = match eval_with(
        &parse(&lex("^ | (now())").unwrap()).unwrap(),
        &t,
        Some(clocked),
    ) {
        QueryResult::Values(vs) => vs,
        _ => panic!("expected values"),
    };
    assert_eq!(
        got,
        vec![Value::Instant {
            secs: s,
            nanos: n,
            offset_min: Some(0),
            calendar: Default::default(),
        }]
    );
    assert_eq!(got[0].to_string(), "2026-07-12T09:00:00Z");
}

#[test]
fn temporal_arith_overflow_nulls() {
    let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::value::quantity::scale_expr;
    let inst = |secs| Value::Instant {
        secs,
        nanos: 0,
        offset_min: Some(0),
        calendar: Default::default(),
    };
    let dur = |secs| Value::seconds(secs, 0);
    // Instant + Duration past i64::MAX seconds: null, never a
    // panic (debug) or wrapped garbage instant (release).
    assert!(matches!(
        arith(ArithOp::Add, &inst(i64::MAX), &dur(1), sc),
        Value::Null
    ));
    // Span + span is the quantity arithmetic of dimension T, a
    // float magnitude: no overflow to null.
    assert!(matches!(
        arith(ArithOp::Add, &dur(i64::MAX), &dur(1), sc),
        Value::Quantity { .. }
    ));
    // Instant − Instant underflow.
    assert!(matches!(
        arith(ArithOp::Sub, &inst(i64::MIN), &inst(i64::MAX), sc),
        Value::Null
    ));
    // The non-overflowing path is unchanged.
    assert_eq!(
        arith(ArithOp::Add, &inst(0), &dur(60), sc).to_string(),
        "1970-01-01T00:01:00Z"
    );
}

#[test]
fn contains_and_regex_null_propagate() {
    let sc: &dyn Fn(&str) -> Option<(f64, String)> = &crate::value::quantity::scale_expr;
    let name = Value::Str("chapter one".into());
    // A null right-hand side must NOT vacuously match (the bug:
    // Null stringified to "" is a substring of everything).
    assert!(!compare(&name, CmpOp::Contains, &Value::Null, sc));
    // An actual empty string still matches, as it should.
    assert!(compare(
        &name,
        CmpOp::Contains,
        &Value::Str(String::new()),
        sc
    ));
    // A real substring still matches.
    assert!(compare(
        &name,
        CmpOp::Contains,
        &Value::Str("one".into()),
        sc
    ));
    // `=~` against a null pattern doesn't match everything (the
    // bug: Regex::new("") matches any input).
    assert!(!compare(&name, CmpOp::Match, &Value::Null, sc));
    // `!~` against null is true, mirroring `!=` on null.
    assert!(compare(&name, CmpOp::NotMatch, &Value::Null, sc));
    // A real pattern still matches.
    assert!(compare(
        &name,
        CmpOp::Match,
        &Value::Str("^chap".into()),
        sc
    ));
}

#[test]
fn nav_command_resumes_navigation() {
    let t = MockTree::sample();
    // A path-shaped command fans out per thread — the pipeline
    // spelling of a path continuation.
    assert_eq!(run("/* | /*", &t), vec![2, 3, 5, 6]);
    // The register rides across the hop: collect at both
    // levels, lay out as a record.
    let vs = vals("/* | .dir(:name) | /* | .file(:name) | %.", &t);
    assert_eq!(vs.len(), 4);
    let Value::Record(fields) = &vs[0] else {
        panic!("expected a record, got {:?}", vs[0]);
    };
    assert_eq!(
        fields[0],
        ("dir".to_string(), Value::Str("a".into())),
        "first column is the ancestor's push"
    );
    assert_eq!(fields[1], ("file".to_string(), Value::Str("x.rs".into())));
    // A trailing projection ends the command in scalar mode,
    // exactly like a branch projection.
    assert_eq!(
        vals("/a | /*:name", &t),
        vec![Value::Str("x.rs".into()), Value::Str("y.txt".into())]
    );
    // `^` — the anchored command navigates from the root.
    assert_eq!(run("/a/x.rs | ^/b", &t), vec![4]);
    // `(m)` — anchor on the thread's own pebble; both the
    // path-side and the pipe-side mark spellings feed it.
    assert_eq!(run("/a .m /x.rs | $$.m/y.txt", &t), vec![3]);
    assert_eq!(run("/a | .m | /x.rs | $$.m/y.txt", &t), vec![3]);
    // Arrows hop crosslinks as commands too.
    assert_eq!(run("/a/x.rs | ->ref", &t), vec![7]);
    // File-first: a projection enters scalar mode, the push
    // returns to navigation mode, and the hop is legal again.
    assert_eq!(run("/a | :name | . | /*", &t), vec![2, 3]);
}

#[test]
fn nav_command_mode_discipline() {
    let refuse = |q: &str| {
        parse(&lex(q).unwrap())
            .expect_err("should refuse")
            .to_string()
    };
    // Scalar mode: a hop would drop the live topic.
    assert!(refuse("/a:name | /b").contains("scalar mode"));
    assert!(refuse("/a | :name | /b").contains("scalar mode"));
    // Hops are per-thread: neither `@|` nor `$|` takes them.
    assert!(refuse("/a @| /b").contains("per-thread"));
    assert!(refuse("/a | ... $| /b").contains("map pipe"));
}

#[test]
fn named_groups_push_into_the_register() {
    // A named group is a capture, read from `%+`; filing it is
    // explicit — `| %+ | .%` pushes every group under its name,
    // seen by the record view and recalled as a register.
    let t = MockTree::sample();
    let bare = vals(
        "/a/* | [:name == (/^(?<stem>\\w+)\\.(?<ext>\\w+)$/)] | %.",
        &t,
    );
    assert!(
        bare.iter()
            .all(|r| matches!(r, Value::Record(f) if f.is_empty())),
        "{bare:?}"
    );
    let stems = vals(
        "/a/* | [:name == (/^(?<stem>\\w+)\\.(?<ext>\\w+)$/)] | %+:stem",
        &t,
    );
    assert!(
        stems.iter().all(|v| matches!(v, Value::Str(_))),
        "{stems:?}"
    );
    let q = "/a/* | [:name == (/^(?<stem>\\w+)\\.(?<ext>\\w+)$/)] | %+ | .% | %.";
    let rows = vals(q, &t);
    assert!(!rows.is_empty());
    for r in &rows {
        let Value::Record(fields) = r else {
            panic!("expected a record, got {r:?}");
        };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["stem", "ext"], "{r:?}");
    }
    let exts = vals(
        "/a/* | [:name == (/^(?<stem>\\w+)\\.(?<ext>\\w+)$/)] | %+ | .% | $.ext",
        &t,
    );
    assert!(
        exts.iter()
            .all(|v| matches!(v, Value::Str(s) if s == "rs" || s == "txt")),
        "{exts:?}"
    );
    // the numbered captures still bind alongside
    let stems = vals("/a/* | [:name == (/^(?<stem>\\w+)\\.(\\w+)$/)] | $1", &t);
    assert_eq!(stems.len(), exts.len());
}

#[test]
fn record_fields_and_named_captures() {
    // Ruling #48: `:name` reads a record's field; `%+` is the
    // named-captures record.
    let t = MockTree::sample();
    let q = "/a/* | [:name == (/^(?<stem>\\w+)\\.(?<ext>\\w+)$/)] | %+:ext";
    let exts = vals(q, &t);
    assert!(
        !exts.is_empty()
            && exts
                .iter()
                .all(|v| matches!(v, Value::Str(s) if s == "rs" || s == "txt")),
        "{exts:?}"
    );
    let recs = vals("/a/* | [:name == (/^(?<stem>\\w+)/)] | %+", &t);
    assert!(
        recs.iter()
            .all(|r| matches!(r, Value::Record(f) if f.len() == 1 && f[0].0 == "stem")),
        "{recs:?}"
    );
    // a constructed record's field, from the topic and from a register
    let ones = vals("/a/* | %(:name; n = 1) | :n", &t);
    assert!(
        !ones.is_empty() && ones.iter().all(|v| matches!(v, Value::Int(1))),
        "{ones:?}"
    );
    let ones = vals("/a/* | %(:name; n = 1) | .r | $.r:n", &t);
    assert!(
        !ones.is_empty() && ones.iter().all(|v| matches!(v, Value::Int(1))),
        "{ones:?}"
    );
    // a missing field, and a non-record, read as null
    let nulls = vals("/a/* | %(:name; n = 1) | :zzz", &t);
    assert!(nulls.iter().all(|v| matches!(v, Value::Null)), "{nulls:?}");
    // outside any match, `%+` is the empty record
    let empty = vals("/a/* | %+", &t);
    assert!(
        empty
            .iter()
            .all(|v| matches!(v, Value::Record(f) if f.is_empty())),
        "{empty:?}"
    );
}

#[test]
fn list_literal_gathers_every_value() {
    // Ruling #52: `@(a; b)` — each item contributes all its
    // values; the rounded `*(…)` spells the same.
    let t = MockTree::sample();
    assert_eq!(
        vals("/a | @(1; \"x\"; 2 + 1)", &t),
        vec![Value::list(vec![
            Value::Int(1),
            Value::Str("x".into()),
            Value::Int(3)
        ])]
    );
    let gathered = vals("/a | @(/*:name)", &t);
    assert!(
        matches!(&gathered[0], Value::List(items) if items.len() > 1),
        "{gathered:?}"
    );
    assert_eq!(vals("/a | @()", &t), vec![Value::list(Vec::new())]);
    assert_eq!(vals("/a | *(1; 2)", &t), vals("/a | @(1; 2)", &t));
    // display: the Quarb form, bare scalars, quoted strings
    assert_eq!(
        vals("/a | @(1; \"x\")", &t)[0].display_form(),
        "@(1; \"x\")"
    );
}

#[test]
fn json_and_jsonl_over_the_stream() {
    // Ruling #51: `| json` serializes each value; `@| json` the
    // whole stream as one array; `@| jsonl` one document per line.
    let t = MockTree::sample();
    let each = vals("/a/* | :name | json", &t);
    assert!(
        each.len() > 1
            && each
                .iter()
                .all(|v| matches!(v, Value::Str(s) if s.starts_with('"'))),
        "{each:?}"
    );
    let whole = vals("/a/* | :name @| json", &t);
    assert!(
        matches!(&whole[..], [Value::Str(s)] if s.starts_with("[\"") && s.ends_with(']')),
        "{whole:?}"
    );
    let lines = vals("/a/* | :name @| jsonl", &t);
    assert!(
        matches!(&lines[..], [Value::Str(s)] if s.lines().count() == each.len()),
        "{lines:?}"
    );
    // a record's JSON is unchanged by the display ruling
    let rec = vals("/a/* | %(n = :name) | json", &t);
    assert!(
        rec.iter()
            .all(|v| matches!(v, Value::Str(s) if s.starts_with("{\"n\": "))),
        "{rec:?}"
    );
}

#[test]
fn record_push_in_one_step() {
    // Ruling #49: `.%(...)` builds and pushes; `.name%(...)` names it.
    let t = MockTree::sample();
    let ones = vals("/a/* | .r%(:name; n = 1) | $.r:n", &t);
    assert!(
        !ones.is_empty() && ones.iter().all(|v| matches!(v, Value::Int(1))),
        "{ones:?}"
    );
    let views = vals("/a/* | .r%(:name) | %.", &t);
    assert!(
        views
            .iter()
            .all(|v| matches!(v, Value::Record(f) if f.len() == 1 && f[0].0 == "r")),
        "{views:?}"
    );
    let anon = vals("/a/* | .%(:name; n = 2) | $.:n", &t);
    assert!(
        !anon.is_empty() && anon.iter().all(|v| matches!(v, Value::Int(2))),
        "{anon:?}"
    );
    // the enriched form carries the register's named regulae
    let rich = vals("/a/* | .k(:name) | .%%(n = 3) | $.:k", &t);
    assert!(
        !rich.is_empty() && rich.iter().all(|v| matches!(v, Value::Str(_))),
        "{rich:?}"
    );
}

#[test]
fn mark_array_symmetry() {
    // Anchors in double parentheses — the canonical spelling
    // (the single-paren spellings still parse, except `(.)`,
    // which is now the register file).
    let t = MockTree::sample();
    // Bare `.` pockets the node anonymously — mid-path and as
    // a pipe command — and `(N)` / `(.)` recall positionally.
    assert_eq!(run("/a . /x.rs | ((.1))/y.txt", &t), vec![3]);
    assert_eq!(run("/a . /x.rs | $$./y.txt", &t), vec![3]);
    assert_eq!(run("/a | . | /x.rs | ((.1))/y.txt", &t), vec![3]);
    // Names are references over the same slots: the named mark
    // is also position 2. (Bare `| (2)` stays the literal 2 —
    // digit anchors need a continuation; the bare re-seed
    // spellings are `(.)`, `(@)`, `(@name)`.)
    assert_eq!(
        vals("/a . /x.rs .m | ((.2)):name", &t),
        vec![Value::Str("x.rs".into())]
    );
    // The null wart is gone: a bare push in node context marks
    // and files NOTHING in the register.
    assert_eq!(
        vals("/a | . | @. | json", &t),
        vec![Value::Str("[]".into())]
    );
    // Positional recall of marks lives in operand space too.
    assert_eq!(run("/a . /*[((.1))/x.rs]", &t), vec![2, 3]);
}

#[test]
fn marks_fan_out() {
    let t = MockTree::sample();
    // `(@)` re-seeds the thread from every pocket, in push
    // order — one fork per marked node.
    assert_eq!(run("/a/x.rs | . | ^/b/z.rs | . | ((@))", &t), vec![2, 5]);
    // ... and navigation continues from each in parallel.
    assert_eq!(run("/a | . | ^/b | . | ((@))/*.rs", &t), vec![2, 5]);
    // `(@name)` fans over every mark under one name, shadowed
    // ones included.
    assert_eq!(run("/a .s /x.rs | ^/b .s /z.rs | ((@s))", &t), vec![1, 4]);
    // An empty pocket yields nothing, never an error.
    assert_eq!(run("/a | ((@))/x.rs", &t), Vec::<u64>::new());
}

#[test]
fn numeric_mark_names_refused() {
    let refuse = |q: &str| {
        parse(&lex(q).unwrap())
            .expect_err("should refuse")
            .to_string()
    };
    assert!(refuse("/a .1 /b").contains("starts with a letter"));
    assert!(refuse("/a | .1").contains("starts with a letter"));
}

#[test]
fn pushes_return_to_navigation_mode() {
    let t = MockTree::sample();
    // A computed push files the value (and shows it as the
    // topic, as ever) — but the thread is back in navigation
    // mode: the hop is legal, and it rebuilds the capsae with
    // the filed value safe in the register.
    let vs = vals("/* | .dir(:name) | /*[:name = x.rs] | %.", &t);
    assert_eq!(vs.len(), 1);
    let Value::Record(fields) = &vs[0] else {
        panic!("expected a record, got {:?}", vs[0]);
    };
    assert_eq!(fields[0], ("dir".to_string(), Value::Str("a".into())));
    // A bare push files the projected topic and reopens
    // navigation the same way.
    assert_eq!(vals("/a:name | . | $.", &t), vec![Value::Str("a".into())]);
}

// ---- capsa provenance: origins on every value ----------------

fn traced(q: &str, t: &MockTree) -> Vec<Traced> {
    eval_traced_prov(&parse(&lex(q).unwrap()).unwrap(), t)
}

fn origins(known: &[(u64, u32)]) -> Origins {
    Origins {
        known: known
            .iter()
            .map(|&(n, s)| Origin {
                position: (NodeId(n)).into(),
                command: s,
            })
            .collect(),
        more: 0,
    }
}

/// Two nodes with a provenance triple each, for the projections.
fn provenanced() -> MockTree {
    let mut t = MockTree::sample();
    let (s2, n2, o2) = crate::temporal::parse_iso("2026-04-07T02:00:01Z").unwrap();
    let (s7, n7, o7) = crate::temporal::parse_iso("2026-05-01T00:00:00Z").unwrap();
    t.prov.insert(
        2,
        Provenance {
            source: Some("api-gateway".into()),
            instant: Some((s2, n2, o2)),
            dpid: Some("request-42".into()),
            ..Default::default()
        },
    );
    t.prov.insert(
        7,
        Provenance {
            source: Some("crm".into()),
            instant: Some((s7, n7, o7)),
            dpid: Some("row-7".into()),
            ..Default::default()
        },
    );
    t
}

#[test]
fn push_records_the_read_node_and_command() {
    let t = MockTree::sample();
    // A push at command 1 reads the node it stands at; the recall
    // hands the filed value back with that origin.
    let rows = traced("/a/* | .n(:name) | $.n", &t);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].node, NodeId(2));
    assert_eq!(rows[0].prov.origins, origins(&[(2, 1)]));
    assert_eq!(rows[1].prov.origins, origins(&[(3, 1)]));
    // A branch-level projection is command 0.
    let rows = traced("/a/x.rs:name", &t);
    assert_eq!(rows[0].prov.origins, origins(&[(2, 0)]));
}

#[test]
fn expression_unions_its_operands_origins() {
    let t = MockTree::sample();
    // The recall carries the push's coordinate; the fresh read
    // is at the current node and command.
    let rows = traced("/a/x.rs | .n(:index) | ->ref | $.n + :index", &t);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].node, NodeId(7));
    assert_eq!(rows[0].prov.origins, origins(&[(2, 1), (7, 3)]));
}

#[test]
fn selection_aggregate_keeps_the_winners_origins() {
    let t = MockTree::sample();
    // `max` hands back one input: its origins are that input's
    // alone, not the union of everything reduced.
    let rows = traced("/a/*:index @| max", &t);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prov.origins.known.len(), 1);
    let all = traced("/a/*:index @| count", &t);
    assert!(
        all[0].prov.origins.known.len() > 1,
        "a computed reduction keeps the union"
    );
}

#[test]
fn a_projected_count_streams_and_keeps_the_counted_nodes() {
    let t = MockTree::sample();
    // The streamed count over a projection is the node count (a
    // null value is an input too), and its origins are the nodes.
    let plain = traced("/a/* @| count", &t);
    let projected = traced("/a/*:index @| count", &t);
    assert_eq!(plain[0].topic, projected[0].topic);
    let Some(Value::Int(n)) = projected[0].topic else {
        panic!("{projected:?}")
    };
    assert_eq!(projected[0].prov.origins.known.len(), n as usize);
    let missing = traced("/a/*::no-such-property @| count", &t);
    assert_eq!(missing[0].topic, projected[0].topic);
}

#[test]
fn a_count_behind_a_descendant_lead_step_streams_and_agrees() {
    let t = MockTree::sample();
    // `//paragraph//token @| count`: a descendant lead step walks
    // its subtree for the frontier, and nested bases count a node
    // once — the same figure the materializing walk gives.
    let streamed = vals("//*//* @| count", &t);
    let walked = vals("//*//* @| unique @| count", &t);
    assert_eq!(streamed, walked);
    let streamed = vals("//a//x.rs @| count", &t);
    let walked = vals("//a//x.rs @| unique @| count", &t);
    assert_eq!(streamed, walked);
    assert_ne!(streamed, vec![Value::Int(0)]);
}

#[test]
fn fork_keeps_the_register_provenance() {
    let t = MockTree::sample();
    // The cross-capsa scenario: after a hop the capsa stands
    // elsewhere, but the regula still says where it was read.
    let rows = traced("/a/x.rs | .n(:name) | ->ref | $.n", &t);
    assert_eq!(rows[0].node, NodeId(7));
    assert_eq!(rows[0].topic, Some(Value::Str("x.rs".into())));
    assert_eq!(rows[0].prov.origins, origins(&[(2, 1)]));
}

#[test]
fn record_keeps_a_part_per_field() {
    let t = MockTree::sample();
    let rows = traced(
        "/a/x.rs | .n(:index) | ->ref | %(a = $.n; b = :name)",
        &t,
    );
    let p = &rows[0].prov;
    assert_eq!(p.origins, origins(&[(2, 1), (7, 3)]));
    assert_eq!(p.part("a").origins, origins(&[(2, 1)]));
    assert_eq!(p.part("b").origins, origins(&[(7, 3)]));
    // `.%` spreads the parts into registers; a recall gets the
    // field's own part back.
    let rows = traced(
        "/a/x.rs | .n(:index) | ->ref | %(a = $.n; b = :name) | .% | $.b",
        &t,
    );
    assert_eq!(rows[0].prov.origins, origins(&[(7, 3)]));
    let rows = traced(
        "/a/x.rs | .n(:index) | ->ref | %(a = $.n; b = :name) | .% | %.",
        &t,
    );
    assert_eq!(rows[0].prov.part("a").origins, origins(&[(2, 1)]));
    assert_eq!(rows[0].prov.part("b").origins, origins(&[(7, 3)]));
}

#[test]
fn aggregate_unions_and_bounds() {
    let t = MockTree::sample();
    let rows = traced("/a/* | :index @| count", &t);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prov.origins, origins(&[(2, 1), (3, 1)]));
    // Past the cap only the count grows.
    let mut wide = MockTree::sample();
    wide.names = (0..13)
        .map(|i| if i == 0 { None } else { Some(format!("n{i}")) })
        .collect();
    wide.kids = HashMap::new();
    wide.kids.insert(0, (1..13).collect());
    wide.links = HashMap::new();
    let rows = traced("/* | :index @| sum", &wide);
    assert_eq!(rows[0].prov.origins.known.len(), ORIGIN_CAP);
    assert_eq!(rows[0].prov.origins.more, 4);
}

#[test]
fn predicate_reads_are_not_origins() {
    let t = MockTree::sample();
    // The condition's test reads w.rs, the taken branch reads
    // only the register.
    let rows = traced(
        "/a/x.rs | .n(:index) | ->ref | (:name = \"w.rs\" ? $.n : 0)",
        &t,
    );
    assert_eq!(rows[0].topic, Some(Value::Int(1)));
    assert_eq!(rows[0].prov.origins, origins(&[(2, 1)]));
}

#[test]
fn group_and_ungroup_carry_origins() {
    let t = MockTree::sample();
    // Files grouped by their directory's name: the key was read
    // at the parent (one origin, deduplicated on join); the
    // group's own provenance is its members' — the index each
    // member read at itself.
    let rows = traced("/*/* | .d(\\:name) | :index @| group(d = $.d)", &t);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].prov.origins, origins(&[(2, 2), (3, 2)]));
    assert_eq!(rows[1].prov.origins, origins(&[(5, 2), (6, 2)]));
    let rows = traced(
        "/*/* | .d(\\:name) | :index @| group(d = $.d) | $.d",
        &t,
    );
    assert_eq!(rows[0].prov.origins, origins(&[(1, 1)]));
    let rows = traced(
        "/*/* | .d(\\:name) | :index @| group(d = $.d) @| ungroup | $.d",
        &t,
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3].prov.origins, origins(&[(4, 1)]));
}

#[test]
fn a_group_sums_a_record_field() {
    // A reducing aggregate over a group of records reads the field
    // to reduce through `--of` (draft 164: `sum(--of=:n)`), and a
    // field read on the group's list projects each member, so
    // `:n | sum` says the same. Both agree with grouping the values.
    let t = MockTree::sample();
    let by_value = traced(
        "/*/* | .d(\\:name) | :index @| group(d = $.d) | sum",
        &t,
    );
    let keyed = traced(
        "/*/* | .d(\\:name) | %(n = :index) @| group(d = $.d) | sum(--of=:n)",
        &t,
    );
    let read = traced(
        "/*/* | .d(\\:name) | %(n = :index) @| group(d = $.d) | :n | sum",
        &t,
    );
    assert_eq!(by_value.len(), 2);
    for i in 0..2 {
        assert!(by_value[i].topic.is_some());
        assert_eq!(keyed[i].topic, by_value[i].topic);
        assert_eq!(read[i].topic, by_value[i].topic);
    }
}

#[test]
fn piped_operand_reads_its_path() {
    let t = MockTree::sample();
    let rows = traced("/a/x.rs | .n(->ref @| count) | $.n", &t);
    assert_eq!(rows[0].topic, Some(Value::Int(1)));
    assert!(
        rows[0]
            .prov
            .origins
            .known
            .iter()
            .any(|o| o.position == NodeId(7).into())
    );
}

#[test]
fn value_side_provenance_projection() {
    let t = provenanced();
    // One origin: the node's own canonical text.
    assert_eq!(
        vals("/a/x.rs | .n(:index) | $.n:%provenance:text", &t),
        vec![Value::Str(
            "?api-gateway@2026-04-07T02:00:01Z#request-42".into()
        )]
    );
    assert_eq!(
        vals("/a/x.rs | .n(:index) | $.n:%provenance:source", &t),
        vec![Value::Str("api-gateway".into())]
    );
    // Two origins: the list, `;`-separated, first-read order;
    // the source and dpid of the first; the instant of the
    // newest.
    let q = "/a/x.rs | .n(:index) | ->ref | $.n + :index";
    assert_eq!(
        vals(&format!("{q} | $_:%provenance:text"), &t),
        vec![Value::Str(
            "?api-gateway@2026-04-07T02:00:01Z#request-42;crm@2026-05-01T00:00:00Z#row-7".into()
        )]
    );
    assert_eq!(
        vals(&format!("{q} | $_:%provenance:source"), &t),
        vec![Value::Str("api-gateway".into())]
    );
    assert_eq!(
        vals(&format!("{q} | $_:%provenance:dpid"), &t),
        vec![Value::Str("request-42".into())]
    );
    let (s7, n7, o7) = crate::temporal::parse_iso("2026-05-01T00:00:00Z").unwrap();
    assert_eq!(
        vals(&format!("{q} | $_:%provenance:instant"), &t),
        vec![Value::Instant {
            secs: s7,
            nanos: n7,
            offset_min: o7,
            calendar: Default::default()
        }]
    );
    // The shaped keys: the entries as records, the coordinates.
    let (s2, n2, o2) = crate::temporal::parse_iso("2026-04-07T02:00:01Z").unwrap();
    // The record: source, the node's path within it, the
    // instant and the rung it came from (the mock records
    // instants without a rung), the dpid.
    let rec = |src: &str, path: &str, inst: (i64, u32, Option<i16>), dpid: &str| {
        Value::Record(vec![
            ("source".into(), Value::Str(src.into())),
            ("path".into(), Value::Str(path.into())),
            (
                "instant".into(),
                Value::Instant {
                    secs: inst.0,
                    nanos: inst.1,
                    offset_min: inst.2,
                    calendar: Default::default(),
                },
            ),
            ("instant-from".into(), Value::Null),
            ("dpid".into(), Value::Str(dpid.into())),
        ])
    };
    assert_eq!(
        vals(&format!("{q} | $_:@provenance"), &t),
        vec![Value::list(vec![
            rec("api-gateway", "/a/x.rs", (s2, n2, o2), "request-42"),
            rec("crm", "/b/deep/w.rs", (s7, n7, o7), "row-7"),
        ])]
    );
    // The record carries the singular keys, then the elided count
    // and the canonical text (spec: Record Fields).
    let Value::Record(whole) = &vals(&format!("{q} | $_:%provenance"), &t)[0] else {
        panic!("a record")
    };
    let Value::Record(core) = rec("api-gateway", "/a/x.rs", (s7, n7, o7), "request-42") else {
        panic!("a record")
    };
    assert_eq!(&whole[..5], &core[..]);
    assert_eq!(whole[5], ("elided".to_string(), Value::Int(0)));
    assert!(matches!(&whole[6], (k, Value::Str(t)) if k == "text" && t.starts_with("?api-gateway@")));
    assert_eq!(
        vals(&format!("{q} | $_:%provenance:path"), &t),
        vec![Value::Str("/a/x.rs".into())]
    );
    // A node with no recorded provenance still has a path; the
    // access rung needs an invocation instant, which the mock
    // does not bind.
    assert_eq!(
        vals("/a/y.txt:path", &t),
        vec![Value::Str("/a/y.txt".into())]
    );
    assert_eq!(vals("/a/y.txt:instant", &t), vec![Value::Null]);
    let coord = |id: i64, command: i64| {
        Value::Record(vec![
            ("id".into(), Value::Int(id)),
            ("command".into(), Value::Int(command)),
        ])
    };
    assert_eq!(vals(&format!("{q} | $_:%origin:id"), &t), vec![Value::Int(2)]);
    assert_eq!(vals(&format!("{q} | $_:%origin"), &t), vec![coord(2, 1)]);
    assert_eq!(
        vals(&format!("{q} | $_:@origin"), &t),
        vec![Value::list(vec![coord(2, 1), coord(7, 3)])]
    );
    // The coordinate stood on again: the node-id anchor.
    assert_eq!(
        vals(&format!("{q} | $_:%origin:id | . | ((!7)):name"), &t),
        vec![Value::Str("w.rs".into())]
    );
    assert_eq!(vals(&format!("{q} | $_:%provenance:elided"), &t), vec![Value::Int(0)]);
    // A record field's own part, through the field colon.
    assert_eq!(
        vals(
            "/a/x.rs | .n(:index) | ->ref | .r%(a = $.n; b = :name) | $.r:b:%provenance:source",
            &t
        ),
        vec![Value::Str("crm".into())]
    );
    // Built into a shape of the user's own.
    let vs = vals(
        "/a/x.rs | .n(:index) | %(v = $.n; from = $.n:%provenance:text)",
        &t,
    );
    let Value::Record(fields) = &vs[0] else {
        panic!("expected a record")
    };
    assert_eq!(
        fields[1],
        (
            "from".to_string(),
            Value::Str("?api-gateway@2026-04-07T02:00:01Z#request-42".into())
        )
    );
    // A node answers the shaped keys too: one entry, its own
    // coordinate, none elided.
    assert_eq!(
        vals("/a/x.rs:@provenance | count", &t),
        vec![Value::Int(1)]
    );
    assert_eq!(vals("/a/x.rs:origin", &t), vec![Value::Int(2)]);
    assert_eq!(vals("/a/x.rs:%origin", &t), vec![coord(2, 0)]);
    assert_eq!(vals("/a/x.rs:elided", &t), vec![Value::Int(0)]);
    // Origins without provenance answer null.
    assert_eq!(
        vals("/a/y.txt | .n(:index) | $.n:%provenance:text", &t),
        vec![Value::Null]
    );
}

#[test]
fn value_meta_needs_a_value_base() {
    assert!(parse(&lex("/a/x.rs | 1:::source").unwrap()).is_err());
    assert!(parse(&lex("/a | $.n:%provenance:text").unwrap()).is_ok());
}

// ---- cross-capsa reads: X:.r ------------------------------------

#[test]
fn peer_read_is_by_location() {
    let t = MockTree::sample();
    // x.rs --ref--> w.rs. Every file's capsa files its index;
    // x.rs reads the regula of the capsa standing at w.rs, w.rs
    // reads x.rs's through the reverse link; nobody stands at
    // the other end for y.txt and z.rs: no value, null as a
    // scalar.
    assert_eq!(
        vals("//*<file> | .v(:index) | ->ref:.v", &t),
        vec![Value::Int(1), Value::Null, Value::Null, Value::Null]
    );
    assert_eq!(
        vals("//*<file> | .v(:index) | <-ref:.v", &t),
        vec![Value::Null, Value::Null, Value::Null, Value::Int(1)]
    );
    // The other accessors: the top regula, positional, the
    // whole register, the record views.
    assert_eq!(vals("/a/x.rs | .v(7) | ->ref:.v", &t), vec![Value::Null]);
    let t2 = MockTree::sample();
    assert_eq!(
        vals(
            "//*<file> | .v(:index) | .w(:name) | ->ref:. | ($$_:name = \"x.rs\" ? $_ : \"-\")",
            &t2
        ),
        vec![
            Value::Str("w.rs".into()),
            Value::Str("-".into()),
            Value::Str("-".into()),
            Value::Str("-".into())
        ]
    );
    // Positional: the first regula of the peer at w.rs.
    assert_eq!(
        vals("//*<file> | .v(:index * 10) | .w(:name) | ->ref:.1", &t),
        vec![Value::Int(10), Value::Null, Value::Null, Value::Null]
    );
}

#[test]
fn peer_read_sees_the_input_snapshot() {
    let t = MockTree::sample();
    // A round that both reads a peer's value and repoints the
    // same name: every capsa reads the value the command
    // received, whatever the evaluation order — x.rs and w.rs
    // swap through the link in one command.
    let pair = "//*<file>[:name = \"x.rs\" || :name = \"w.rs\"]";
    let vs = vals(
        &format!(
            "{pair} | .v((:name = \"x.rs\" ? 5 : 9)) | .v((:name = \"x.rs\" ? ->ref:.v : <-ref:.v)) | $.v"
        ),
        &t,
    );
    // x.rs (evaluated first) takes w.rs's 9; w.rs takes x.rs's 5
    // — the value the command received, not the 9 x.rs just
    // filed.
    assert_eq!(vs, vec![Value::Int(9), Value::Int(5)]);
    // Two rounds: x.rs files a from w.rs's v, w.rs then reads
    // x.rs's a as b.
    assert_eq!(
        vals(
            &format!(
                "{pair} | .v((:name = \"x.rs\" ? 5 : 9)) | .a(->ref:.v) | .b(<-ref:.a) | $.b"
            ),
            &t
        ),
        vec![Value::Null, Value::Int(9)]
    );
}

#[test]
fn peer_read_after_a_fork_reads_the_children() {
    let t = MockTree::sample();
    // The thread at `a` files p, forks into its children, each
    // files its index. Nobody stands at `a` any more, so a read
    // at `a` finds nothing; the family is its children, read
    // by navigating to them: every member answers, the shared
    // prefix in copies.
    assert_eq!(
        vals("/a | .p(1) | /* | .r(:index) | \\:.p", &t),
        vec![Value::Null, Value::Null]
    );
    assert_eq!(
        vals("/a | .p(1) | /* | .r(:index) | (\\/*:.r @| sum)", &t),
        vec![Value::Int(3), Value::Int(3)]
    );
    assert_eq!(
        vals("/a | .p(1) | /* | .r(:index) | (\\/*:.p @| count)", &t),
        vec![Value::Int(2), Value::Int(2)]
    );
    // A record per peer: `%.` at each sibling.
    assert_eq!(
        vals("/a | .p(1) | /* | .r(:index) | (\\/*:%. | :r @| sum)", &t),
        vec![Value::Int(3), Value::Int(3)]
    );
}

#[test]
fn peer_read_answers_every_capsa_standing_there() {
    let t = MockTree::sample();
    // Two capsae converge on `a` with different registers: both
    // are read. With equal registers they deduplicate into one.
    assert_eq!(
        vals("/a/* | .v(:index) | \\ | (\\/a:.v @| count)", &t),
        vec![Value::Int(2), Value::Int(2)]
    );
    assert_eq!(
        vals("/a/* | .v(1) | \\ | (\\/a:.v @| count)", &t),
        vec![Value::Int(1)]
    );
}

#[test]
fn peer_read_carries_the_regula_provenance() {
    let t = MockTree::sample();
    // The value read from w.rs's capsa keeps where w.rs read it
    // (node 7, command 1); the node it is read at is not an
    // origin, nor is the navigation.
    let pair = "//*<file>[:name = \"x.rs\" || :name = \"w.rs\"]";
    let rows = traced(&format!("{pair} | .v(:index) | ->ref:.v"), &t);
    assert_eq!(rows[0].node, NodeId(2));
    assert_eq!(rows[0].prov.origins, origins(&[(7, 1)]));
    let t2 = provenanced();
    assert_eq!(
        vals(&format!("{pair} | .v(:index) | ->ref:.v:%provenance:source"), &t2)[0],
        Value::Str("crm".into())
    );
}

#[test]
fn peer_read_is_null_without_a_context() {
    let t = MockTree::sample();
    // In a navigation predicate no context exists: null, so the
    // comparison fails.
    assert_eq!(run("/a/x.rs[->ref:.v = 1]", &t), Vec::<u64>::new());
    assert_eq!(run("/a/x.rs[!->ref:.v]", &t), vec![2]);
}

#[test]
fn peer_read_needs_a_node_side() {
    // A projected path is a value, not a node; so is a recall.
    assert!(parse(&lex("/a | $.v:.r").unwrap()).is_err());
    assert!(parse(&lex("/a | ->ref::x:.r").unwrap()).is_err());
    // A spaced colon is still the conditional's else.
    assert!(parse(&lex("/a | (::x ? 1 : .5)").unwrap()).is_ok());
}

#[test]
fn node_id_anchor() {
    let t = MockTree::sample();
    // `((!N))` stands on the node with that id, at the branch
    // level and in operand position; an id the adapter never
    // minted (the mock indexes by id and would panic) is no
    // node.
    assert_eq!(run("((!7))", &t), vec![7]);
    assert_eq!(vals("((!7)):name", &t), vec![Value::Str("w.rs".into())]);
    assert_eq!(
        vals("/a | ((!7))\\:name", &t),
        vec![Value::Str("deep".into())]
    );
    assert_eq!(run("((!99))", &t), Vec::<u64>::new());
    assert_eq!(vals("/a | ((!99)):name", &t), Vec::<Value>::new());
    // Round trip through the id key: what `:id` and
    // `:origin` report, the anchor accepts.
    assert_eq!(vals("/b/deep/w.rs:id", &t), vec![Value::Int(7)]);
    assert_eq!(
        vals("/a/x.rs | .v(:index) | ->ref | $.v:%origin:id", &t),
        vec![Value::Int(2)]
    );
    // The anchor takes an expression: a value's origin stood on
    // again, a coordinate record by its id, an arithmetic id, a
    // list of ids forking one thread each.
    assert_eq!(
        vals(
            "/a/x.rs | .v(:index) | ->ref | .o($.v:%origin:id) | ((!$.o)):name",
            &t
        ),
        vec![Value::Str("x.rs".into())]
    );
    assert_eq!(
        vals(
            "/a/x.rs | .v(:index) | ->ref | ((!$.v:%origin)):name",
            &t
        ),
        vec![Value::Str("x.rs".into())]
    );
    assert_eq!(
        vals("/a/* | .v(:index) | ((!$.v + 1)):name", &t),
        vec![Value::Str("x.rs".into()), Value::Str("y.txt".into())]
    );
    assert_eq!(
        vals("/a | ((!@(2; 3; 99))):name", &t),
        vec![Value::Str("x.rs".into()), Value::Str("y.txt".into())]
    );
    // The anchor's reads are not origins.
    let rows = traced(
        "/a/x.rs | .v(:index) | ->ref | ((!$.v:%origin:id)) | :name",
        &t,
    );
    assert_eq!(rows[0].node, NodeId(2));
    assert_eq!(rows[0].prov.origins, origins(&[(2, 4)]));
}

#[test]
fn quantified_push_iterates() {
    let t = MockTree::sample();
    // `{N}` files N regulae, each round reading the last.
    assert_eq!(
        vals("/a | .v(1) | .v($.v * 2){3} | $.v", &t),
        vec![Value::Int(8)]
    );
    assert_eq!(
        vals("/a | .v(1) | .v($.v * 2){3} | @. | count", &t),
        vec![Value::Int(4)]
    );
    // A round is synchronous: peers read the previous round's
    // values through the snapshot, whatever the order.
    let pair = "//*<file>[:name = \"x.rs\" || :name = \"w.rs\"]";
    assert_eq!(
        vals(
            &format!(
                "{pair} | .v(:index * 10) | .v(($.v + (->ref:.v | default(0)) + (<-ref:.v | default(0)))){{2}} | $.v"
            ),
            &t
        ),
        vec![Value::Int(40), Value::Int(40)]
    );
    // The open forms stop at a fixpoint: a round that repoints
    // no name — after `min` rounds, within the bound.
    assert_eq!(
        vals(
            "/a | .v(5) | .v(($.v > 8 ? $.v : $.v + 1)){1;} | @. | count",
            &t
        ),
        vec![Value::Int(6)]
    );
    assert_eq!(
        vals("/a | .v(5) | .v($.v){3;} | @. | count", &t),
        vec![Value::Int(5)]
    );
    assert_eq!(
        vals("/a | .v(5) | .v($.v + 1){2;3} | $.v", &t),
        vec![Value::Int(8)]
    );
    // Only a push takes the quantifier.
    assert!(parse(&lex("/a | upper{3}").unwrap()).is_err());
    assert!(parse(&lex("/a | .v(1){3;2}").unwrap()).is_err());
}
