//! Cache correctness: the round-trip must be identical, and every
//! corruption / skew must be rejected (→ `None`, never a wrong arbor).

use super::*;
use crate::CodeAdapter;

const RUST: &str = r#"
mod m {
    struct S { a: i32, b: i32 }
    fn helper(x: i32, y: i32) -> i32 {
        let mut t = 0;
        for i in 0..x {
            for j in 0..y {
                t = t + i * j;
            }
        }
        t
    }
    fn main() { let _ = helper(2, 3); }
}
"#;

const PY: &str = "class C:\n    def a(self, x):\n        def b():\n            return x\n        return b\n\ndef top(n):\n    return [i for i in range(n)]\n";

const JS: &str = "function outer(a) {\n  const f = (x) => { return x + a; };\n  return f;\n}\nclass K { m() { return 1; } }\n";

const C: &str = "#include <stdio.h>\nstatic int helper(int a, int b) { return a + b; }\nint main(void) {\n  for (int i = 0; i < 3; i++) { printf(\"%d\", helper(i, 1)); }\n  return 0;\n}\n";

fn lang_of(ext: &str) -> tree_sitter::Language {
    crate::language(ext).unwrap()
}

/// Render a query's results to comparable strings (values by
/// display, node results by locator), so two adapters can be
/// checked for observational equivalence.
fn rendered(adapter: &CodeAdapter, query: &str) -> Vec<String> {
    match quarb::run(query, adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| adapter.locator(n)).collect(),
    }
}

fn adapter_from_nodes(src: &str, nodes: Vec<Node>) -> CodeAdapter {
    CodeAdapter {
        source: src.to_string(),
        nodes,
    }
}

/// Parse (uncached), serialize, deserialize, and assert the arbor
/// is field-for-field identical and observationally identical.
fn check_round_trip(src: &str, ext: &str) {
    let lang = lang_of(ext);
    let tag = lang_tag(ext);
    assert_ne!(tag, 0, "unsupported ext {ext}");
    let m = meta(tag, &lang);
    let a = CodeAdapter::parse_raw(src, ext).unwrap();

    let bytes = encode(
        &a.nodes,
        tag,
        &lang,
        &m,
        &quarb::sha256(src.as_bytes()),
        src.len() as u64,
    )
    .expect("encode should succeed for a real grammar");
    let nodes_b = decode(&bytes, tag, &lang, src).expect("decode should succeed");

    assert_eq!(a.nodes.len(), nodes_b.len(), "{ext}: node count");
    assert_eq!(a.nodes, nodes_b, "{ext}: node vec identity");

    // Observational: a battery of queries must agree byte-for-byte.
    let b = adapter_from_nodes(src, nodes_b);
    for q in [
        "//*::;kind",
        "//*::;start-line",
        "//*::;end-line",
        "//*::;field",
        "//*::;n-children",
        "//*::",
        "/*",
    ] {
        assert_eq!(rendered(&a, q), rendered(&b, q), "{ext}: query {q}");
    }
}

#[test]
fn round_trip_rust() {
    check_round_trip(RUST, "rs");
}
#[test]
fn round_trip_python() {
    check_round_trip(PY, "py");
}
#[test]
fn round_trip_javascript() {
    check_round_trip(JS, "js");
}
#[test]
fn round_trip_c() {
    check_round_trip(C, "c");
}

#[test]
fn round_trip_edge_cases() {
    for (src, ext) in [
        ("", "rs"),                                         // empty
        ("x", "rs"),                                        // single token (error tree)
        ("fn f(){}", "rs"),                                 // no trailing newline
        ("fn f(){}\r\n", "rs"),                             // CRLF
        ("// αβγ ünïcödé 日本語\nfn f(){}\n", "rs"),        // multibyte
        ("fn f(){ if a { if b { if c { g() } } } }", "rs"), // deep nesting
        ("fn f( ", "rs"),                                   // syntax error / MISSING nodes
        ("\n\n\n", "py"),                                   // whitespace only
    ] {
        check_round_trip(src, ext);
    }
}

// ---- rejection: a hit must be refused whenever anything is off ----

fn valid_bytes(src: &str, ext: &str) -> (Vec<u8>, tree_sitter::Language, u16) {
    let lang = lang_of(ext);
    let tag = lang_tag(ext);
    let m = meta(tag, &lang);
    let a = CodeAdapter::parse_raw(src, ext).unwrap();
    let bytes = encode(
        &a.nodes,
        tag,
        &lang,
        &m,
        &quarb::sha256(src.as_bytes()),
        src.len() as u64,
    )
    .unwrap();
    (bytes, lang, tag)
}

fn refix_footer(bytes: &mut [u8]) {
    let n = bytes.len();
    let f = fnv1a(&bytes[..n - 8]).to_le_bytes();
    bytes[n - 8..].copy_from_slice(&f);
}

#[test]
fn reject_footer_mismatch() {
    let (mut b, lang, tag) = valid_bytes(RUST, "rs");
    b[HEADER_LEN + 1] ^= 0xff; // flip a body byte, do NOT fix the footer
    assert!(decode(&b, tag, &lang, RUST).is_none());
}

#[test]
fn reject_truncated() {
    let (b, lang, tag) = valid_bytes(RUST, "rs");
    for cut in [0, 10, HEADER_LEN, HEADER_LEN + 4, b.len() - 1] {
        assert!(decode(&b[..cut.min(b.len())], tag, &lang, RUST).is_none());
    }
}

#[test]
fn reject_format_version_skew() {
    let (mut b, lang, tag) = valid_bytes(RUST, "rs");
    b[8] = 99; // bogus format_version
    refix_footer(&mut b);
    assert!(decode(&b, tag, &lang, RUST).is_none());
}

#[test]
fn reject_grammar_digest_skew() {
    let (mut b, lang, tag) = valid_bytes(RUST, "rs");
    b[40] ^= 0x01; // perturb the grammar digest, keep the footer valid
    refix_footer(&mut b);
    assert!(decode(&b, tag, &lang, RUST).is_none());
}

#[test]
fn reject_wrong_language_tag() {
    // A rust cache file decoded as if it were python: tag mismatch.
    let (b, _lang, _tag) = valid_bytes(RUST, "rs");
    let pylang = lang_of("py");
    assert!(decode(&b, lang_tag("py"), &pylang, RUST).is_none());
}

#[test]
fn grammar_stamp_changes_the_digest() {
    // A different grammar-crate version stamp must change the
    // fingerprint (this is what invalidates a cache across a
    // scanner-only grammar upgrade the runtime tables can't see).
    let lang = lang_of("rs");
    let d1 = grammar_digest("ts-rust=0.23.3", "0.1.0", &lang);
    let d2 = grammar_digest("ts-rust=0.23.4", "0.1.0", &lang);
    let d3 = grammar_digest("ts-rust=0.23.3", "0.1.1", &lang);
    assert_ne!(d1, d2, "grammar version must affect the digest");
    assert_ne!(d1, d3, "crate version must affect the digest");
}

#[test]
fn reject_absurd_node_count_without_oom() {
    // A corrupt header claiming an enormous node_count must not force
    // a huge allocation — it must fail gracefully to None.
    let (mut b, lang, tag) = valid_bytes(RUST, "rs");
    b[112..120].copy_from_slice(&u64::MAX.to_le_bytes()); // node_count
    refix_footer(&mut b);
    assert!(decode(&b, tag, &lang, RUST).is_none());
}

#[test]
fn reject_wrong_content() {
    // A validly-footered file whose stored content hash does not
    // match the source handed to decode (wrong file at this path).
    let (b, lang, tag) = valid_bytes(RUST, "rs");
    assert!(decode(&b, tag, &lang, "fn different() {}").is_none());
    // The right content still loads.
    assert!(decode(&b, tag, &lang, RUST).is_some());
}

// ---- file-level store/load, incl. a concurrency stress ----

#[test]
fn file_store_load_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().to_path_buf());
    let lang = lang_of("c");
    let tag = lang_tag("c");
    let hash = quarb::sha256(C.as_bytes());
    let a = CodeAdapter::parse_raw(C, "c").unwrap();

    // Miss before store.
    assert!(load(&cache, tag, &lang, &hash, C).is_none());

    store(&cache, &a.nodes, tag, &lang, &hash, C.len() as u64);
    let nodes = load(&cache, tag, &lang, &hash, C).expect("hit after store");
    assert_eq!(a.nodes, nodes);

    // Wrong content is a miss even though the path could collide.
    let other = "int q(void){return 9;}\n";
    assert!(load(&cache, tag, &lang, &quarb::sha256(other.as_bytes()), other).is_none());
}

#[test]
fn concurrent_store_is_benign() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().to_path_buf());
    let lang = lang_of("rs");
    let tag = lang_tag("rs");
    let hash = quarb::sha256(RUST.as_bytes());
    let a = CodeAdapter::parse_raw(RUST, "rs").unwrap();

    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                store(&cache, &a.nodes, tag, &lang, &hash, RUST.len() as u64);
            });
        }
    });
    // Exactly one final file, and it loads to the identical arbor.
    let nodes = load(&cache, tag, &lang, &hash, RUST).expect("hit after concurrent store");
    assert_eq!(a.nodes, nodes);
}

#[test]
fn set_cache_makes_parse_transparent() {
    let dir = tempfile::tempdir().unwrap();
    crate::set_cache(Some(Cache::new(dir.path().to_path_buf())));

    // First parse: miss → parse → store. Second: hit → load.
    let a = CodeAdapter::parse(RUST, "rs").unwrap(); // populates
    let b = CodeAdapter::parse(RUST, "rs").unwrap(); // from cache
    for q in ["//function_item::name", "//*::;kind", "//*::;start-line"] {
        assert_eq!(
            rendered(&a, q),
            rendered(&b, q),
            "cached parse differs: {q}"
        );
    }
    crate::set_cache(None);
}
