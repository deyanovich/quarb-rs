//! Draft 189: the graph library's bridges, articulation points and
//! routes over a declared edge, and a model's `graph` declaration
//! deciding cost, never answers (spec: Graph Declarations).
use std::path::{Path, PathBuf};

const MODEL: &str = "\
ref /stages/*::from_id --> /stations/*[::id = $];
ref /stages/*::to_id --> /stations/*[::id = $];
edge /stages/*: ::from_id -- ::to_id as stage;
graph /stations/*(--stage) { artpoints; conncomp; };
graph /stations/*(--stage[$-::parasangs > 50]) { conncomp; };
";

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-graphdecl-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("roads.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE stations (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         INSERT INTO stations VALUES (1, 'Ephesus'), (2, 'Sardis'), (3, 'Gates'), (4, 'Euphrates');
         CREATE TABLE stages (id INTEGER PRIMARY KEY, from_id INTEGER REFERENCES stations(id),
           to_id INTEGER REFERENCES stations(id), parasangs TEXT);
         INSERT INTO stages VALUES (1, 1, 2, '18'), (2, 2, 3, '100'), (3, 2, 4, '90'), (4, 3, 4, '20');",
    )
    .unwrap();
    std::fs::write(dir.join("roads.model"), MODEL).unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(["--model", "roads.model", "--diagnostics=info"])
        .args(args)
        .arg("roads.db")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

#[test]
fn bridges_artpoints_and_routes_over_a_declared_edge() {
    let dir = fixture("commands");
    // Ephesus-Sardis is the one bridge; Sardis is the one cut
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:artpoints(--stage)"]);
    assert!(ok, "{err}");
    assert_eq!(out, "false\ntrue\nfalse\nfalse");
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:bridges(--stage) | json"]);
    assert!(ok, "{err}");
    assert_eq!(out.matches("\"source\"").count(), 1, "{out}");
    assert!(out.contains("/stations/"), "{out}");
    // the route from Ephesus to the Euphrates, by hops through Sardis
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| graph:route(--stage; --from=/stations/*[::id = 1])"],
    );
    assert!(ok, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    let last = lines[3];
    let (a, b, c) = (
        last.find("/stations/1").unwrap_or(usize::MAX),
        last.find("/stations/2").unwrap_or(usize::MAX),
        last.find("/stations/4").unwrap_or(usize::MAX),
    );
    assert!(a < b && b < c, "{last}");
    assert!(!last.contains("/stations/3"), "{last}");
    // the edges table with the betweenness column
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| graph:betweenness(--stage; --edges) | json"],
    );
    assert!(ok, "{err}");
    assert_eq!(out.matches("\"betweenness\"").count(), 4, "{out}");
}

#[test]
fn a_declared_graph_is_reported_and_decides_nothing() {
    let dir = fixture("declared");
    // the context is the scope entire: the source has nothing kept
    // yet in a one-shot run, so the engine computes and says so
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:artpoints(--stage)"]);
    assert!(ok, "{err}");
    assert_eq!(out, "false\ntrue\nfalse\nfalse");
    assert!(
        err.contains("info NOT_PUSHED (graph): the source has no answer for the declared artpoints(--stage) on /stations/*; computed over 4 nodes"),
        "{err}"
    );
    // a narrower context is not the scope
    // (Sardis, Gates and the Euphrates are a triangle: nothing cuts)
    let (out, err, ok) = qua(
        &dir,
        &["/stations/*[::id != 1] @| graph:artpoints(--stage)"],
    );
    assert!(ok, "{err}");
    assert_eq!(out, "false\nfalse\nfalse");
    assert!(
        err.contains("info NOT_PUSHED (graph): the context is 3 of the 4 nodes of /stations/*, computed over 3 nodes"),
        "{err}"
    );
    // another hop is another graph
    let (_, err, ok) = qua(&dir, &["/stations/* @| graph:artpoints(->stage)"]);
    assert!(ok, "{err}");
    assert!(
        err.contains("info NOT_PUSHED (graph): the hop or the options differ from the declared artpoints(--stage) on /stations/*"),
        "{err}"
    );
    // a measure the model does not declare says nothing of graphs
    let (_, err, ok) = qua(&dir, &["/stations/* @| graph:kcore(--stage)"]);
    assert!(ok, "{err}");
    assert!(!err.contains("(graph)"), "{err}");
    // the bound refuses before computing, naming the cost
    let (_, err, ok) = qua(
        &dir,
        &[
            "--graph-bound",
            "5",
            "/stations/* @| graph:conncomp(--stage)",
        ],
    );
    assert!(!ok);
    assert!(
        err.contains("refused: graph:conncomp would compute over 4 nodes and 4 edges, 8 cost units past the graph bound 5"),
        "{err}"
    );
}

#[test]
fn a_hop_takes_a_predicate_and_a_reference_reads_its_node() {
    let dir = fixture("draft191");
    // `$-` the edge: the two long stages
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| edges(--stage[$-::parasangs > 50]) | json"],
    );
    assert!(ok, "{err}");
    assert_eq!(out.matches("\"source\"").count(), 2, "{out}");
    // the target as topic: the stages into Sardis, from either side
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| edges(--stage[::name = \"Sardis\"]) | json"],
    );
    assert!(ok, "{err}");
    assert_eq!(out.matches("\"source\"").count(), 3, "{out}");
    // a node column reads the nodes it names
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| graph:bridges(--stage) | :source::name"],
    );
    assert!(ok, "{err}");
    assert_eq!(out, "@(\"Ephesus\")");
    // a route's nodes read their names along the way
    let (out, err, ok) = qua(
        &dir,
        &[
            "/stations/* @| graph:route(--stage; --from=/stations/*[::id = 1]) | map(::name) | join(\" > \")",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(
        out.lines().last().unwrap_or(""),
        "Ephesus > Sardis > Euphrates",
        "{out}"
    );
    // the windowed declaration is read: the same hop text matches
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| graph:conncomp(--stage[$-::parasangs > 50])"],
    );
    assert!(ok, "{err}");
    assert_eq!(out, "1\n2\n2\n2");
    assert!(
        err.contains("info NOT_PUSHED (graph): the source has no answer for the declared conncomp(--stage[$-::parasangs > 50]) on /stations/*; computed over 4 nodes"),
        "{err}"
    );
}

#[test]
fn closeness_communities_modularity_and_triangles_over_the_stations() {
    let dir = fixture("third");
    // Sardis, Gates and the Euphrates are a triangle: one each, three in all
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:triangles(--stage)"]);
    assert!(ok, "{err}");
    assert_eq!(out, "0\n1\n1\n1");
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:triangles(--stage) @| sum"]);
    assert!(ok, "{err}");
    assert_eq!(out, "3");
    // Ephesus has one neighbour: 0; Sardis three of which one pair: 1/3
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| graph:triangles(--stage; --coefficient)"],
    );
    assert!(ok, "{err}");
    let c: Vec<f64> = out.lines().map(|l| l.parse().unwrap()).collect();
    assert!(
        c[0] == 0.0 && (c[1] - 1.0 / 3.0).abs() < 1e-12 && c[2] == 1.0 && c[3] == 1.0,
        "{out}"
    );
    // closeness: Sardis reaches the three others in one hop each
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:closeness(--stage)"]);
    assert!(ok, "{err}");
    let c: Vec<f64> = out.lines().map(|l| l.parse().unwrap()).collect();
    assert!((c[1] - 1.0).abs() < 1e-12, "{out}");
    assert!((c[0] - (3.0 / 3.0) * (3.0 / 5.0)).abs() < 1e-12, "{out}");
    // communities, and their modularity through the attached regula
    let (out, err, ok) = qua(&dir, &["/stations/* @| graph:communities(--stage)"]);
    assert!(ok, "{err}");
    assert!(out.lines().all(|l| l.parse::<i64>().is_ok()), "{out}");
    assert_eq!(out.lines().next(), Some("1"));
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* @| graph:communities(--stage) | .:c @| graph:modularity(--stage; --by=:.c)"],
    );
    assert!(ok, "{err}");
    let q: f64 = out.trim().parse().unwrap_or_else(|_| panic!("{out}"));
    assert!((-0.5..=1.0).contains(&q), "{q}");
    // a partition by name scores below one: every node its own part
    let (out, err, ok) = qua(
        &dir,
        &["/stations/* | .:p(::name) @| graph:modularity(--stage; --by=:.p)"],
    );
    assert!(ok, "{err}");
    let own: f64 = out.trim().parse().unwrap();
    assert!(own < 0.0, "{own}");
}
