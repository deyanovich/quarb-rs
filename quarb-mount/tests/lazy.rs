//! Latent mounts: declared now, opened when first reached — and
//! the laziness unobservable from the query's side.

use std::cell::Cell;
use std::rc::Rc;

use quarb::{AstAdapter, NodeId, Value};
use quarb_html::HtmlAdapter;
use quarb_json::JsonAdapter;
use quarb_mount::{Latent, Mount, MountAdapter, Opener};

/// An opener that counts how often it ran and opens `json`.
fn counting(json: &'static str, count: &Rc<Cell<usize>>) -> Opener {
    let count = count.clone();
    Box::new(move || {
        count.set(count.get() + 1);
        JsonAdapter::parse(json)
            .map(|a| Box::new(a) as Box<dyn AstAdapter>)
            .map_err(|e| e.to_string())
    })
}

fn table() -> (MountAdapter, Rc<Cell<usize>>, Rc<Cell<usize>>) {
    let (na, nb) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let mut m = MountAdapter::new(Vec::new());
    m.declare(Latent {
        name: "a".into(),
        target: Some("/data/a.json".into()),
        opener: counting(r#"{"x": [1, 2]}"#, &na),
        catalog: None,
    })
    .unwrap();
    m.declare(Latent {
        name: "b".into(),
        target: Some("/data/b.json".into()),
        opener: counting(r#"{"x": [3]}"#, &nb),
        catalog: None,
    })
    .unwrap();
    (m, na, nb)
}

fn values(q: &str, a: &impl AstAdapter) -> Vec<Value> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(v) => v,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_hop_by_name_opens_that_mount_alone() {
    let (m, na, nb) = table();
    assert_eq!(values("/a/x/* @| count", &m), [Value::Int(2)]);
    assert_eq!((na.get(), nb.get()), (1, 0));
    assert!(m.is_active(0) && !m.is_active(1));
    // Opened once: the second query reuses the adapter.
    assert_eq!(values("/a/x/*:: @| sum", &m), [Value::Int(3)]);
    assert_eq!(na.get(), 1);
}

#[test]
fn an_unanchored_search_opens_every_mount() {
    let (m, na, nb) = table();
    assert_eq!(values("//x/* @| count", &m), [Value::Int(3)]);
    assert_eq!((na.get(), nb.get()), (1, 1));
    let (m, na, nb) = table();
    assert_eq!(
        values("/*:name", &m),
        [Value::Str("a".into()), Value::Str("b".into())]
    );
    assert_eq!((na.get(), nb.get()), (1, 1));
}

#[test]
fn identities_answer_from_the_declaration() {
    let (m, na, nb) = table();
    let ids = m.tree_identities();
    assert_eq!(
        ids.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>(),
        ["/data/a.json", "/data/b.json"]
    );
    assert_eq!((na.get(), nb.get()), (0, 0));
    // The stand-in root resolves — and opens the mount — when
    // the engine touches it.
    let (_, root_b) = ids[1].clone();
    assert_eq!(m.name(root_b).as_deref(), Some("b"));
    assert_eq!((na.get(), nb.get()), (0, 1));
    assert_eq!(m.children(root_b).len(), 1);
}

#[test]
fn a_reference_lands_on_a_latent_mount_through_its_identity() {
    // A page in `site` links to the URL declared for the latent
    // mount `page`: the arrow lands there and opens it.
    let site = HtmlAdapter::parse(
        r#"<html><body><a href="https://docs.test/guide.html">guide</a></body></html>"#,
    );
    let mut m = MountAdapter::new(vec![Mount {
        name: "site".into(),
        target: Some("https://docs.test/index.html".into()),
        adapter: Box::new(site),
    }]);
    let opened = Rc::new(Cell::new(0));
    let o = opened.clone();
    let idx = m
        .declare(Latent {
            name: "page".into(),
            target: None,
            opener: Box::new(move || {
                o.set(o.get() + 1);
                Ok(Box::new(HtmlAdapter::parse(
                    "<html><body><h1>Guide</h1></body></html>",
                )) as Box<dyn AstAdapter>)
            }),
            catalog: None,
        })
        .unwrap();
    m.declare_identity(idx, "https://docs.test/guide.html".into());
    assert_eq!(values("/site//a-->:name", &m), [Value::Str("page".into())]);
    assert_eq!(opened.get(), 1);
    assert_eq!(
        values("/site//a-->//h1::", &m),
        [Value::Str("Guide".into())]
    );
    assert_eq!(opened.get(), 1);
}

#[test]
fn a_failed_opener_refuses_and_the_mount_stands_empty() {
    let mut m = MountAdapter::new(Vec::new());
    m.declare(Latent {
        name: "gone".into(),
        target: Some("/data/gone.json".into()),
        opener: Box::new(|| Err("no such file".into())),
        catalog: None,
    })
    .unwrap();
    let err = quarb::run("/gone/* @| count", &m).unwrap_err();
    assert!(
        err.to_string().contains("mount 'gone': no such file"),
        "{err}"
    );
    assert!(m.is_active(0));
    assert_eq!(values("/gone/* @| count", &m), [Value::Int(0)]);
}

#[test]
fn a_live_table_takes_a_new_mount() {
    let (mut m, _, _) = table();
    assert_eq!(m.len(), 2);
    let c = JsonAdapter::parse(r#"{"x": [4, 5, 6]}"#).unwrap();
    m.push(Mount {
        name: "c".into(),
        target: None,
        adapter: Box::new(c),
    })
    .unwrap();
    assert_eq!(values("//x/* @| count", &m), [Value::Int(6)]);
    assert!(
        m.push(Mount {
            name: "c".into(),
            target: None,
            adapter: Box::new(JsonAdapter::parse("{}").unwrap()),
        })
        .is_err()
    );
    let _ = NodeId(0);
}

#[test]
fn a_latent_mount_estimates_from_its_catalog_without_opening() {
    let (mut m, na, _) = table();
    m.declare(Latent {
        name: "shelf".into(),
        target: Some("/data/shelf.tar".into()),
        opener: Box::new(|| Err("never opened".into())),
        catalog: Some(Box::new(|ask| {
            if !ask.scope.is_empty() {
                return None;
            }
            Some(quarb::Catalog::plain(
                (4200, 4200),
                Some((9_000_000, 9_000_000)),
                quarb::Latency::Local,
                quarb::Confidence::Exact,
            ))
        })),
    })
    .unwrap();
    let plan = quarb::Query::parse("/shelf/*[::title = \"Emma\"] | ::title", &m)
        .unwrap()
        .plan(&m);
    let e = plan.branches[0].steps[1].estimate.clone().unwrap();
    assert_eq!(e.nodes, (0, 4200));
    assert_eq!(e.bytes, Some((9_000_000, 9_000_000)));
    assert_eq!(e.confidence, quarb::Confidence::Bound);
    let plan = quarb::Query::parse("/shelf//*", &m).unwrap().plan(&m);
    let e = plan.branches[0].steps[1].estimate.clone().unwrap();
    assert_eq!(e.nodes, (4200, u64::MAX));
    assert_eq!(e.confidence, quarb::Confidence::Bound);
    // A step deeper than the root is a bounded unknown that keeps
    // the root's bytes.
    let deep = quarb::Query::parse("/shelf/book/*", &m).unwrap().plan(&m);
    let e = deep.estimate().unwrap();
    assert_eq!(e.nodes, (0, u64::MAX));
    assert_eq!(e.bytes, Some((9_000_000, 9_000_000)));
    assert!(!m.is_active(2));
    assert_eq!(na.get(), 0);
}

#[test]
fn a_root_step_is_priced_across_the_mounts() {
    let (mut m, na, nb) = table();
    let shelf = |n: u64| {
        Some(quarb::Catalog::plain(
            (n, n),
            Some((n * 1000, n * 1000)),
            quarb::Latency::Local,
            quarb::Confidence::Exact,
        ))
    };
    m.declare(Latent {
        name: "c".into(),
        target: None,
        opener: Box::new(|| Err("never".into())),
        catalog: Some(Box::new(move |_| shelf(5))),
    })
    .unwrap();
    // `/*` is the mounts; `//*` across them counts the active ones'
    // arenas and reads the latent one's catalog.
    m.activate(0).unwrap();
    m.activate(1).unwrap();
    let plan = quarb::Query::parse("/*", &m).unwrap().plan(&m);
    assert_eq!(plan.estimate().unwrap().nodes, (3, 3));
    let plan = quarb::Query::parse("//*", &m).unwrap().plan(&m);
    let e = plan.estimate().unwrap();
    assert_eq!(e.nodes.0, 5 + 3 + 2);
    assert_eq!(e.nodes.1, u64::MAX);
    assert_eq!(e.bytes, Some((5000, 5000)));
    assert_eq!(e.latency, quarb::Latency::Local);
    assert_eq!((na.get(), nb.get()), (1, 1));
    assert!(!m.is_active(2));
}

#[test]
fn a_latent_mount_narrows_to_the_path_the_plan_records() {
    let (mut m, _, _) = table();
    let shelf = |scope: &[&str]| {
        let (entries, bytes) = match scope {
            [] => (3, 9_000),
            ["verse"] => (1, 2_000),
            ["verse", "iliad.txt"] => (1, 2_000),
            _ => return None,
        };
        Some(quarb::Catalog::plain(
            (entries, entries),
            Some((bytes, bytes)),
            quarb::Latency::Local,
            quarb::Confidence::Exact,
        ))
    };
    m.declare(Latent {
        name: "shelf".into(),
        target: None,
        opener: Box::new(|| Err("never".into())),
        catalog: Some(Box::new(move |ask| shelf(ask.scope))),
    })
    .unwrap();
    // The folder's own listing prices the walk under it.
    let plan = quarb::Query::parse("/shelf/verse//*", &m).unwrap().plan(&m);
    let e = plan.estimate().unwrap();
    assert_eq!(e.nodes, (1, u64::MAX));
    assert_eq!(e.bytes, Some((2_000, 2_000)));
    // One member's header prices a reading under it.
    let plan = quarb::Query::parse("/shelf/verse/iliad.txt//paragraph", &m)
        .unwrap()
        .plan(&m);
    let e = plan.estimate().unwrap();
    assert_eq!(e.nodes, (0, u64::MAX));
    assert_eq!(e.bytes, Some((2_000, 2_000)));
    // A path the catalog cannot place is bounded by the whole.
    let plan = quarb::Query::parse("/shelf/prose//*", &m).unwrap().plan(&m);
    let e = plan.estimate().unwrap();
    assert_eq!(e.nodes, (0, u64::MAX));
    assert_eq!(e.bytes, Some((9_000, 9_000)));
    assert!(!m.is_active(2));
}
