//! Composition over a filesystem fixture.
use quarb_compose::ComposeAdapter;
use quarb_fs::{FsAdapter, FsOptions};

fn fixture(name: &str) -> std::path::PathBuf {
    // A unique dir per test: the two tests run in parallel, so a shared
    // dir would let one's `remove_dir_all` wipe the other mid-read.
    let dir = std::env::temp_dir().join(format!("quarb-compose-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("store.json"),
        r#"{"books": [{"t": "Dune", "p": 9}, {"t": "Emma", "p": 7}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("names.csv"), "name,qty\nAda,2\nBo,1\n").unwrap();
    std::fs::write(dir.join("plain.txt"), "not a tree").unwrap();
    dir
}

fn values(a: &impl quarb::AstAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{:?}", n)).collect(),
    }
}

#[test]
fn grafts_parse_lazily_and_compose() {
    let dir = fixture("lazy");
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    // Through the boundary: fs path, then json path.
    assert_eq!(values(&a, "/store.json/books/*[/p:: < 8]/t::"), ["Emma"]);
    // CSV grafts too.
    assert_eq!(values(&a, "/names.csv/*[::qty > 1]::name"), ["Ada"]);
    // A plain text file grafts at the text level: blank-line
    // paragraphs under the leaf.
    assert_eq!(values(&a, "/plain.txt/* @| count"), ["1"]);
    assert_eq!(values(&a, "/plain.txt/paragraph::"), ["not a tree"]);
    assert_eq!(values(&a, "/plain.txt::"), ["not a tree"]);
    // Inner parents climb back out to the outer tree.
    assert_eq!(
        values(&a, "/store.json/books\\\\store.json/books/*/t:: @| count"),
        ["2"]
    );
}

/// An archive leaf grafts by path — and the archive composes in
/// turn, so one path runs filesystem → tar.gz → JSON.
#[test]
fn archive_leaves_graft_by_path() {
    let dir = fixture("archive");
    let tarball = {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut ar = tar::Builder::new(gz);
        let json =
            br#"{"services": [{"name": "web", "port": 8080}, {"name": "db", "port": 5432}]}"#;
        let mut h = tar::Header::new_gnu();
        h.set_size(json.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "app/config/services.json", &json[..])
            .unwrap();
        ar.into_inner().unwrap().finish().unwrap()
    };
    std::fs::write(dir.join("app.tar.gz"), tarball).unwrap();

    let with_paths = ComposeAdapter::with_source_paths(
        FsAdapter::with_options(&dir, FsOptions::default()).unwrap(),
        |fs, n| Some(fs.path(n)),
    );
    assert_eq!(
        values(
            &with_paths,
            "//*.tar.gz//services.json/services/*[/port:: > 6000]/name::"
        ),
        ["web"]
    );
    // Without the path hook, the binary leaf stays a leaf.
    let without = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    assert_eq!(values(&without, "/app.tar.gz/* @| count"), ["0"]);
}

/// Regression: grafted node ids must stay inside the low 56 bits so
/// they survive being packed into a `MountAdapter`, which reserves the
/// high byte (bits 56–63) for the mount index. A graft tag at bit 63
/// spilled into that byte — corrupting the mount index and panicking
/// when a mount wrapped an archive/bucket `ComposeAdapter`. The tag now
/// lives at bit 55, inside the inner window.
#[test]
fn grafted_ids_fit_the_mount_inner_window() {
    let dir = fixture("mount-window");
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    match quarb::run("/store.json/books/*", &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => {
            assert!(!ns.is_empty(), "expected grafted book nodes");
            for n in ns {
                assert_eq!(
                    n.0 >> 56,
                    0,
                    "grafted id {:#x} escapes the 56-bit mount inner window",
                    n.0
                );
            }
        }
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

/// A grafted node's data provenance falls through to the outer leaf
/// it was parsed from: the file's path and mtime, exactly as the
/// file itself answers them.
#[test]
fn grafted_nodes_inherit_outer_leaf_provenance() {
    let dir = fixture("prov");
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    // Inside the graft and on the file: same source, same instant.
    assert_eq!(
        values(&a, "/store.json/books/*[/t:: = 'Dune']:::source"),
        values(&a, "/store.json:::source")
    );
    assert_eq!(
        values(&a, "/store.json/books/*[/t:: = 'Dune']:::instant"),
        values(&a, "/store.json:::instant")
    );
    // The source is the real file path, not the jar-style locator.
    assert!(values(&a, "/store.json/books:::source")[0].ends_with("store.json"));
    assert!(!values(&a, "/store.json/books:::source")[0].contains('!'));
}

/// `::key` dual exposure answers *at* the graft root, not only one
/// level in: the outer leaf holding the JSON text is also the
/// grafted document's root.
#[test]
fn dual_exposure_at_graft_root() {
    let dir = std::env::temp_dir().join("quarb-compose-dualroot-test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("meta.json"),
        r#"{"device":"phone","geo":{"city":"London"}}"#,
    )
    .unwrap();
    let fs = quarb_fs::FsAdapter::new(&dir).unwrap();
    let a = quarb_compose::ComposeAdapter::new(fs);
    // The file node IS the graft root: the fs adapter has no
    // ::device for it, so the property falls through to the
    // grafted document's root.
    let got = match quarb::run("/'meta.json'::device", &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        _ => panic!("expected values"),
    };
    assert_eq!(got, ["phone"]);
}

/// Source leaves graft at the level the mount chose: the syntax
/// level by default (`//function_item` answers), the code level
/// with `SourceGraft::Code` (declared identifiers are the
/// names — the filesystem path continues into the program).
#[test]
fn source_graft_level_is_per_mount() {
    use quarb_compose::SourceGraft;
    let dir = fixture("source-graft");
    std::fs::write(
        dir.join("lexer.rs"),
        "/// Scan.\npub fn lex(input: &str) -> usize {\n    fn is_name_char(c: char) -> bool {\n        c.is_alphanumeric()\n    }\n    input.chars().filter(|c| is_name_char(*c)).count()\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("util.py"), "def helper(a, b):\n    return a + b\n").unwrap();
    std::fs::write(
        dir.join("main.c"),
        "static int helper(int a, int b) { return a + b; }\n",
    )
    .unwrap();

    // Default: the syntax level, exactly as before.
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    assert_eq!(
        values(&a, "/lexer.rs//function_item::name"),
        ["lex", "is_name_char"]
    );
    assert_eq!(values(&a, "//lex @| count"), ["0"]);

    // Opted up: the code level, one namespace — dirs, files,
    // declarations.
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap())
        .with_source_graft(SourceGraft::Code);
    assert_eq!(values(&a, "/lexer.rs/lex/is_name_char @| count"), ["1"]);
    // lex, is_name_char, the filter closure (a lambda IS a
    // <function>), and the two helpers.
    assert_eq!(values(&a, "//*<function>:::name @| count"), ["5"]);
    assert_eq!(values(&a, "//helper:::name @| count"), ["2"]);
    assert_eq!(values(&a, "//function_item @| count"), ["0"]);
    // The locator shows the seam with the bang, the query never
    // sees it.
    let node = match quarb::run("//is_name_char", &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns[0],
        _ => panic!("expected nodes"),
    };
    let fs_locator = |n| a.outer().path(n).display().to_string();
    assert!(
        a.locator(node, fs_locator)
            .ends_with("lexer.rs!/lex/is_name_char")
    );
}

/// A site as a tarball of pages, read at the text level: every
/// page grafts as sections and paragraphs, and the file that
/// holds a page wears what the page's head declares — its tags
/// as traits, its category, its Open Graph facts.
#[test]
fn a_tarball_of_pages_at_the_text_level() {
    use quarb_archive::ArchiveAdapter;
    use quarb_compose::DocumentGraft;
    let page = |name: &str, title: &str, section: &str, tags: &str, body: &str| {
        format!(
            r##"<!doctype html><html lang="en"><head><title>{title}</title>
<meta name="keywords" content="{tags}">
<meta property="article:section" content="{section}">
<meta property="og:type" content="article">
<link rel="canonical" href="https://quarb.org/{name}">
</head><body><nav>chrome</nav><main><h1>{title}</h1>{body}</main><footer>foot</footer></body></html>"##
        )
    };
    let files = [
        (
            "capsa.html",
            page(
                "capsa.html",
                "Capsa",
                "Data Model",
                "capsa, register",
                "<h2>The register</h2><p>The register holds push-only scalars.</p>",
            ),
        ),
        (
            "guides/jq.html",
            page(
                "guides/jq.html",
                "Quarb for jq Users",
                "Guides",
                "guide, jq",
                "<h2>Filters</h2><p>select becomes a predicate.</p>",
            ),
        ),
        (
            "guides/sql.html",
            page(
                "guides/sql.html",
                "Quarb for SQL Users",
                "Guides",
                "guide, sql",
                "<h2 id=\"joins\">Joins</h2><p>The register keeps the witness.</p>",
            ),
        ),
    ];
    let bytes = {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut ar = tar::Builder::new(gz);
        for (name, html) in &files {
            let mut h = tar::Header::new_gnu();
            h.set_size(html.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(&mut h, name, html.as_bytes()).unwrap();
        }
        ar.into_inner().unwrap().finish().unwrap()
    };
    let site = ComposeAdapter::new(ArchiveAdapter::from_tar_bytes(&bytes).unwrap())
        .with_document_graft(DocumentGraft::Text);
    // The text level, not the DOM: sections and paragraphs, chrome gone.
    assert_eq!(
        values(&site, "/guides/jq.html//section::lemma"),
        vec!["Quarb for jq Users", "Filters"]
    );
    assert_eq!(
        values(&site, "//paragraph[:: *= \"register\"] | \\::lemma"),
        vec!["The register", "Joins"]
    );
    // A page's declared tags are the traits of the file that holds it.
    assert_eq!(
        values(&site, "//*<tag:guide>::title"),
        vec!["Quarb for jq Users", "Quarb for SQL Users"]
    );
    assert_eq!(
        values(&site, "//*<tag:capsa>::category"),
        vec!["Data Model"]
    );
    assert_eq!(
        values(&site, "//*[::category = \"Guides\"]::tags"),
        vec!["guide, jq", "guide, sql"]
    );
    // ...and every declaration, verbatim, under the name it was made with.
    assert_eq!(values(&site, "/capsa.html::::\"og:type\""), vec!["article"]);
    // A hit links back to its page, narrowed to its section.
    assert_eq!(
        values(&site, "//paragraph[:: *= \"witness\"] | link | :href"),
        vec![
            "https://quarb.org/guides/sql.html#joins:~:text=The%20register%20keeps%20the%20witness%2E"
        ]
    );
    // The DOM level is still the default.
    let dom = ComposeAdapter::new(ArchiveAdapter::from_tar_bytes(&bytes).unwrap());
    assert_eq!(values(&dom, "/capsa.html//h2::"), vec!["The register"]);
}
