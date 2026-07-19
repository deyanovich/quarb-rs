//! Offline tests over a purpose-built temporary vault.
//!
//! Metatheca stamps states from the wall clock with no injection
//! hook, so nothing here asserts an absolute hash or instant:
//! states are addressed relatively (`~N`, `current`) or through
//! hashes and instants read back from the vault at runtime.

use metatheca::{Fact, Vault};
use quarb_metatheca::MetathecaAdapter;

/// genesis → add notes/a.md → add img.png → add docs/spec.md →
/// mv notes/a.md docs/a.md → rm img.png → re-blob docs/a.md.
/// Seven states; head sees docs/a.md ("v2\n") and docs/spec.md,
/// with img.png's entry orphaned.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    let vault = Vault::init(&root).unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.md"), "v1\n").unwrap();
    std::fs::write(src.join("img.png"), b"\x89PNG\r\n\x1a\n123").unwrap();
    std::fs::write(src.join("spec.md"), "# spec\n").unwrap();
    vault.add(&[src.join("a.md")], Some("notes/a.md")).unwrap();
    vault.add(&[src.join("img.png")], Some("img.png")).unwrap();
    vault
        .add(&[src.join("spec.md")], Some("docs/spec.md"))
        .unwrap();
    vault.mv("notes/a.md", "docs/a.md").unwrap();
    vault.rm("img.png").unwrap();
    let entry = vault.resolve_entry("docs/a.md").unwrap();
    let blob = vault.deposit_blob(b"v2\n").unwrap();
    vault
        .commit(&[Fact::blob_ref(entry, &blob.to_hex(), 3)])
        .unwrap();
    (dir, root)
}

fn values(a: &MetathecaAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn states_and_time_travel() {
    let (_dir, root) = fixture();
    let a = MetathecaAdapter::open(&root).unwrap();
    // The whole chain enumerates; the head is its last state.
    assert_eq!(values(&a, "/states/* @| count"), ["7"]);
    assert_eq!(values(&a, "/head::;seq"), ["6"]);
    assert_eq!(values(&a, "/states/*<genesis>::;seq"), ["0"]);
    // As-of listings: before the rm, three files; at the head, two.
    assert_eq!(values(&a, "/states/'~2'/paths//*<entry> @| count"), ["3"]);
    assert_eq!(values(&a, "/paths//*<entry> @| count"), ["2"]);
    // Time travel: the file's content before and after the re-blob,
    // and at its original path.
    assert_eq!(values(&a, "/paths/docs/a.md::"), ["v2\n"]);
    assert_eq!(values(&a, "/states/'~2'/paths/docs/a.md::"), ["v1\n"]);
    assert_eq!(values(&a, "/states/'~5'/paths/notes/a.md::"), ["v1\n"]);
    // The head alias answers like the root's own tree.
    assert_eq!(values(&a, "/head/paths/docs/a.md::"), ["v2\n"]);
    assert_eq!(values(&a, "/paths/docs/a.md::size"), ["3 B"]);
}

#[test]
fn chain_navigation() {
    let (_dir, root) = fixture();
    let a = MetathecaAdapter::open(&root).unwrap();
    // The ancestry walk spans the chain.
    assert_eq!(values(&a, "/head(->previous)+ @| count"), ["6"]);
    assert_eq!(values(&a, "/head->previous::;seq"), ["5"]);
    assert_eq!(values(&a, "/head::previous~>::;seq"), ["5"]);
    // The successor, from the linear chain.
    assert_eq!(values(&a, "/states/'~1'<-previous::;seq"), ["6"]);
    // Reach ?: the nearest ancestor state where img.png still
    // existed — the state just before the rm.
    let nearest = values(&a, "/head(->previous)+[/paths/img.png]? | ::;short");
    assert_eq!(nearest, values(&a, "/states/'~2'::;short"));
}

#[test]
fn fact_timelines() {
    let (_dir, root) = fixture();
    let a = MetathecaAdapter::open(&root).unwrap();
    // The mv is visible as path events: assert, retract, assert.
    assert_eq!(
        values(&a, "/paths/docs/a.md/*[::kind = \"core/path\"] | ::path"),
        ["notes/a.md", "notes/a.md", "docs/a.md"]
    );
    assert_eq!(
        values(&a, "/paths/docs/a.md/*[::kind = \"core/path\"] | ::linked"),
        ["true", "false", "true"]
    );
    // Every content change is a blob-ref event with an instant.
    assert_eq!(
        values(&a, "/paths/docs/a.md/*[::kind = \"core/blob-ref\"] @| count"),
        ["2"]
    );
    assert_eq!(
        values(
            &a,
            "/paths/docs/a.md/*[::kind = \"core/blob-ref\"][::at > '2020-01-01'] @| count"
        ),
        ["2"]
    );
    assert_eq!(
        values(
            &a,
            "/paths/docs/a.md/*[::kind = \"core/blob-ref\"][::at > '2100-01-01'] @| count"
        ),
        ["0"]
    );
    // A timeline truncates at its coordinate: as of the mv state,
    // the re-blob hasn't happened.
    assert_eq!(
        values(
            &a,
            "/states/'~2'/paths/docs/a.md/*[::kind = \"core/blob-ref\"] @| count"
        ),
        ["1"]
    );
}

#[test]
fn added_and_changed() {
    let (_dir, root) = fixture();
    let a = MetathecaAdapter::open(&root).unwrap();
    // The mv state added exactly its two path facts...
    assert_eq!(
        values(&a, "/states/'~2'->added | ::kind"),
        ["core/path", "core/path"]
    );
    // ...both about the same entry (node results dedupe, so the
    // converging edges land once), and the fact events link back
    // to their introducing state.
    assert_eq!(
        values(&a, "/states/'~2'->added->entry | ::path"),
        ["docs/a.md"]
    );
    assert_eq!(values(&a, "/states/'~2'->added->state | ::;seq"), ["4"]);
    // The diff surface: the one entry that changed in that state.
    assert_eq!(values(&a, "/states/'~2'/entries/*<changed> @| count"), ["1"]);
    assert_eq!(values(&a, "/states/'~2'/entries/*<changed>::path"), ["docs/a.md"]);
}

#[test]
fn stateref_aliasing() {
    let (_dir, root) = fixture();
    let vault = Vault::open(&root).unwrap();
    let a = MetathecaAdapter::open(&root).unwrap();
    // `current`, a full hash, a 12-hex prefix, and an ISO instant
    // all land on their state without enumeration.
    assert_eq!(values(&a, "/states/current::;seq"), ["6"]);
    let s3 = vault.resolve("~3").unwrap();
    let full = s3.to_hex();
    let prefix = &full[..12];
    assert_eq!(values(&a, &format!("/states/{full}::;seq")), ["3"]);
    assert_eq!(values(&a, &format!("/states/'{prefix}'::;seq")), ["3"]);
    let at = vault.get_state(&s3).unwrap().created_at_ns;
    let iso = metatheca::format_iso8601_ns(at);
    assert_eq!(values(&a, &format!("/states/'{iso}'::;seq")), ["3"]);
    // An entry addressed by its UUID, and its fact-event names by
    // their blob hashes.
    let entry = vault.resolve_entry("docs/a.md").unwrap();
    assert_eq!(
        values(&a, &format!("/entries/'{}'::path", entry.hyphenated())),
        ["docs/a.md"]
    );
}

#[test]
fn orphans_and_projection() {
    let (_dir, root) = fixture();
    let a = MetathecaAdapter::open(&root).unwrap();
    // The rm'd image survives as an orphaned entry — visible under
    // /entries, absent from /paths — and keeps its facts.
    assert_eq!(values(&a, "/entries/* @| count"), ["3"]);
    assert_eq!(values(&a, "/entries/*<orphan> @| count"), ["1"]);
    assert_eq!(values(&a, "/entries/*<orphan>::mime"), ["image/png"]);
    // At the pre-rm coordinate nothing is orphaned.
    assert_eq!(values(&a, "/states/'~2'/entries/*<orphan> @| count"), ["0"]);
    // The generic fact projection: a single-field body unwraps to
    // its scalar, a multi-field body reads as canonical JSON.
    assert_eq!(values(&a, "/entries/*<orphan>::'core/mime'"), ["image/png"]);
    let vault = Vault::open(&root).unwrap();
    let entry = vault.resolve_entry("docs/a.md").unwrap();
    let blob = vault
        .current_fact(&entry.hyphenated().to_string(), "core/blob-ref")
        .unwrap()
        .unwrap();
    let hex = blob.body.get("blob").unwrap().as_str().unwrap().to_string();
    assert_eq!(
        values(&a, "/paths/docs/a.md::'core/blob-ref'"),
        [format!("{{\"blob\":\"{hex}\",\"size\":3}}")]
    );
}
