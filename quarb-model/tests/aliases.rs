//! `alias` statements: other spellings a name or a trait answers
//! to — a way in, never a way out.
use quarb_model::{ModelAdapter, parse_model};

const DOC: &str = r##"<!doctype html><html><head><title>T</title>
<meta name="keywords" content="Execution Model, BeautifulSoup">
<meta property="article:section" content="Guides">
<link rel="canonical" href="https://example.org/t.html"></head>
<body><h1>T</h1><h2 id="a">A</h2><p>one</p><p>two</p></body></html>"##;

fn over(model: &str) -> ModelAdapter<quarb_text::TextModel> {
    ModelAdapter::new(quarb_text_html::parse(DOC), parse_model(model).unwrap())
}

fn values(a: &impl quarb::AstAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    }
}

#[test]
fn a_name_alias_is_a_way_in() {
    let a = over("alias para paragraph;");
    assert_eq!(values(&a, "//para::"), vec!["one", "two"]);
    // Canonical spellings keep working; reflection stays canonical.
    assert_eq!(values(&a, "//paragraph::"), vec!["one", "two"]);
    assert_eq!(values(&a, "//para:::name"), vec!["paragraph", "paragraph"]);
}

#[test]
fn a_substitution_alias_rewrites_canonical_names() {
    // Every node also answers to what the substitution produces
    // from its canonical name: `paragraph` → `p`, `section` → `s`.
    let a = over("alias s/^(.).*$/$1/;");
    assert_eq!(values(&a, "//p::"), vec!["one", "two"]);
    assert_eq!(values(&a, "//s::lemma"), vec!["T", "A"]);
    // /i compares the written form case-insensitively.
    let a = over("alias s/^(.).*$/$1/i;");
    assert_eq!(values(&a, "//P::"), vec!["one", "two"]);
}

#[test]
fn a_trait_alias_reaches_the_canonical_trait() {
    let a = over("alias <chunk> <block>;");
    assert_eq!(values(&a, "//paragraph<chunk>::"), vec!["one", "two"]);
    // A written trait that no alias admits still misses.
    assert!(values(&a, "//paragraph<lump>::").is_empty());
}

#[test]
fn a_trait_substitution_with_i_accepts_any_case() {
    // The page's tags are `tag:Execution-Model` and
    // `tag:BeautifulSoup`; stripping the namespace under /i lets
    // `<beautifulsoup>` reach them, and `<tag:BeautifulSoup>`
    // stays a way in too.
    let a = over("alias <s/^tag://i>;");
    use quarb::AstAdapter;
    let root = a.root();
    assert!(a.has_trait(root, "beautifulsoup"));
    assert!(a.has_trait(root, "BEAUTIFULSOUP"));
    assert!(a.has_trait(root, "tag:BeautifulSoup"));
    assert!(!a.has_trait(root, "category:guides"));
    assert!(a.has_trait(root, "category:Guides"));
    // Without /i the case must match.
    let a = over("alias <s/^tag://>;");
    assert!(a.has_trait(a.root(), "BeautifulSoup"));
    assert!(!a.has_trait(a.root(), "beautifulsoup"));
}

#[test]
fn alias_statements_are_checked_at_parse_time() {
    assert!(parse_model("alias s/(/x/;").is_err());
    assert!(parse_model("alias s/a/b/q;").is_err());
    assert!(parse_model("alias lonely;").is_err());
    // A trait alias brackets both spellings.
    assert!(parse_model("alias <chunk> block;").is_err());
    assert!(parse_model("alias <chunk> <block>;").is_ok());
}

