//! End-to-end tests: queries run through the engine against parsed
//! HTML documents.

use quarb_html::HtmlAdapter;

const DOC: &str = r##"<!doctype html>
<html lang="en">
  <body>
    <header><h1 id="top">Welcome</h1></header>
    <main>
      <p class="intro">Hello <strong>world</strong>.</p>
      <ul>
        <li>one</li>
        <li>two</li>
      </ul>
      <a href="#top">back to top</a>
      <a href="https://example.com">external</a>
    </main>
  </body>
</html>"##;

fn nodes(query: &str) -> Vec<String> {
    let adapter = HtmlAdapter::parse(DOC);
    let mut got: Vec<String> = match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| adapter.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

fn values(query: &str) -> Vec<String> {
    let adapter = HtmlAdapter::parse(DOC);
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
fn navigate_by_tag() {
    assert_eq!(
        nodes("/html/body/main/ul/li"),
        vec!["/html/body/main/ul/li[1]", "/html/body/main/ul/li[2]"]
    );
    assert_eq!(values("//h1::"), vec!["Welcome"]);
    // text content includes descendant text
    assert_eq!(values("//p::"), vec!["Hello world."]);
}

#[test]
fn attributes_as_properties() {
    assert_eq!(values("//p::class"), vec!["intro"]);
    assert_eq!(values("//html::lang"), vec!["en"]);
    // via metadata too
    assert_eq!(values("//p:::classes"), vec!["intro"]);
    assert_eq!(values("//h1::id"), vec!["top"]);
    assert_eq!(values("//h1:::tag"), vec!["h1"]);
    // `:traits` is the node's trait list, the kind (the tag) first;
    // `:kind` is that first word
    assert_eq!(values("//strong:traits"), vec!["strong, inline"]);
    assert_eq!(values("//h1:traits"), vec!["h1, block, heading"]);
    assert_eq!(values("//h1:kind"), vec!["h1"]);
    assert_eq!(values("/html:kind"), vec!["html"]);
}

/// `:::attrs` enumerates attribute NAMES, in document order — the
/// discovery half of the attribute story (`::name` reads a value,
/// `:::attrs` says which names exist, so a renamed attribute stays
/// probeable by its value's shape).
#[test]
fn attribute_names_as_metadata() {
    assert_eq!(values("//html:::attrs"), vec!["lang"]);
    assert_eq!(values("//p:::attrs"), vec!["class"]);
    // token test, like `:::classes`: which elements carry an href?
    assert_eq!(
        nodes("//*[:::attrs *= 'href']"),
        vec!["/html/body/main/a[1]", "/html/body/main/a[2]"]
    );
    // no attributes → an empty list, not a miss
    assert_eq!(values("//ul:::attrs"), vec![""]);
}

#[test]
fn structural_traits() {
    // headings
    assert_eq!(nodes("//*<heading>"), vec!["/html/body/header/h1"]);
    // links (anchors with href)
    assert_eq!(
        nodes("//*<link>"),
        vec!["/html/body/main/a[1]", "/html/body/main/a[2]"]
    );
    // inline elements inside p
    assert_eq!(nodes("//p/*<inline>"), vec!["/html/body/main/p/strong"]);
    // block elements directly under body (header and main)
    assert_eq!(
        nodes("/html/body/*<block>"),
        vec!["/html/body/header", "/html/body/main"]
    );
}

#[test]
fn predicates_and_pipelines() {
    // list items joined
    assert_eq!(values("//li:: @| join(\", \")"), vec!["one, two"]);
    // count of anchors
    assert_eq!(values("//a @| count"), vec!["2"]);
    // external links (href not starting with #)
    assert_eq!(values("//a[::href !== (/^#/)]::"), vec!["external"]);
    // substring containment: the external link's href holds "example"
    assert_eq!(values("//a[::href *= \"example\"]::"), vec!["external"]);
    // regex match with a /.../ literal (equivalent to ~(...))
    assert_eq!(values("//a[::href == (/example/)]::"), vec!["external"]);
    // ... and no anchor's href contains "missing"
    assert_eq!(values("//a[::href *= \"missing\"]::"), Vec::<String>::new());
}

#[test]
fn resolve_fragment_href() {
    // the "back to top" anchor's href="#top" resolves to <h1 id="top">
    assert_eq!(nodes("//a::href-->"), vec!["/html/body/header/h1"]);
    // follow the fragment and read the target's text
    assert_eq!(values("//a::href-->::"), vec!["Welcome"]);
    // reverse resolution: which nodes' href resolves to the <h1>?
    assert_eq!(nodes("//h1<--(::href)"), vec!["/html/body/main/a[1]"]);
    assert_eq!(values("//h1<--(::href)::"), vec!["back to top"]);
}

/// Numeric aggregation over HTML, whose attribute values are all
/// strings. `avg`/`sum`/`max` must coerce the numeric strings, and a
/// per-group subcontext must too; a non-numeric column stays textual.
#[test]
fn numeric_aggregation_over_string_attributes() {
    let doc = r##"<!doctype html><html><body>
        <user name="Ada"><s rank="3"></s><s rank="1"></s><s rank="2"></s></user>
        <user name="Bo"><s rank="5"></s><s rank="5"></s></user>
      </body></html>"##;
    let adapter = HtmlAdapter::parse(doc);
    let vals = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    // whole-context numeric aggregates coerce the string ranks
    assert_eq!(vals("//s::rank @| sum"), vec!["16"]);
    assert_eq!(vals("//s::rank @| avg"), vec!["3.2"]);
    assert_eq!(vals("//s::rank @| max"), vec!["5"]);
    // per-user average (subcontext), then the average of those
    assert_eq!(vals("//user | .(/s::rank @| avg) | $."), vec!["2.0", "5.0"]);
    assert_eq!(vals("//user | .(/s::rank @| avg) | $. @| avg"), vec!["3.5"]);
    // max is a numeric reduction: names have no numeric reading, so
    // they are skipped and an all-non-numeric input reduces to null.
    // Text ordering is expressed with `sort | last`.
    assert_eq!(vals("//user::name @| max"), vec![""]);
    assert_eq!(vals("//user::name @| sort @| last"), vec!["Bo"]);
}

/// Value expressions over attribute strings: the numeric reading
/// makes text-sourced arithmetic work (and keeps integers exact),
/// and a non-numeric operand yields null rather than an error.
#[test]
fn value_expressions_over_string_attributes() {
    let doc = r##"<!doctype html><html><body>
        <item price="4" qty="3"></item>
        <item price="10" qty="2"></item>
        <item price="7" qty="oops"></item>
      </body></html>"##;
    let adapter = HtmlAdapter::parse(doc);
    let vals = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    // computed comparison in a predicate
    assert_eq!(vals("//item[::price * ::qty > 15]::price"), vec!["10"]);
    // computed pipeline topic; the non-numeric row is null (empty)
    assert_eq!(vals("//item | ::price * ::qty"), vec!["12", "20", ""]);
    // a computed column, pushed then aggregated
    assert_eq!(
        vals("//item | .total(::price * ::qty) | $.total @| sum"),
        vec!["32"]
    );
    assert_eq!(
        vals("//item | .total(::price * ::qty) | $.total @| max"),
        vec!["20"]
    );
}

/// A correlated join whose two conditions must bind to the *same*
/// left-context node. Ada has limit 100, Bo limit 500; order-A
/// (amount 200) exceeds its own user Ada's limit, but order-B
/// (amount 300) does not exceed its own user Bo's limit (500). A
/// buggy per-comparison existential would keep order-B too, because
/// 300 > Ada's 100 — a *different* user.
#[test]
fn correlated_join_shares_one_binding() {
    let doc = r##"<!doctype html><html><body>
        <ul id="users">
          <li class="user" data-id="1" data-limit="100">Ada</li>
          <li class="user" data-id="2" data-limit="500">Bo</li>
        </ul>
        <ul id="orders">
          <li class="order" data-uid="1" data-amount="200">order-A</li>
          <li class="order" data-uid="2" data-amount="300">order-B</li>
        </ul>
      </body></html>"##;
    let adapter = HtmlAdapter::parse(doc);
    let run = |q: &str| match quarb::run(q, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    };
    // Driver-first: the orders drive, each pinned to ONE user
    // (the ON clause reaches the driver as `$$`) — both
    // conditions must hold under the same binding.
    let join = concat!(
        r#"//li[::class = "order"]:: <=> "#,
        r#"//li[::class = "user"][::data-id = $$0::data-uid "#,
        r#"&& ::data-limit < $$0::data-amount]"#,
    );
    assert_eq!(run(join), vec!["order-A"]);
    // Stacked brackets are a conjunction, so they share the binding
    // and must give the identical result.
    let stacked = concat!(
        r#"//li[::class = "order"]:: <=> "#,
        r#"//li[::class = "user"][::data-id = $$0::data-uid]"#,
        r#"[::data-limit < $$0::data-amount]"#,
    );
    assert_eq!(run(stacked), vec!["order-A"]);
}

/// Pathologically deep nesting must not overflow the stack while
/// interning. html5ever parses `<div>` repeated tens of thousands of
/// times into that many nested elements (iteratively); the adapter's
/// interning pass must handle it without self-recursing per level.
#[test]
fn deeply_nested_html_does_not_overflow() {
    use quarb::AstAdapter;
    let depth = 50_000usize;
    let deep = "<div>".repeat(depth);
    // Building the adapter must complete rather than SIGSEGV.
    let adapter = HtmlAdapter::parse(&deep);
    // Confirm every level was interned, counted with an explicit stack
    // (the tree is far too deep to recurse over here — the whole point).
    let mut stack = vec![adapter.root()];
    let mut divs = 0usize;
    while let Some(n) = stack.pop() {
        if adapter.name(n).as_deref() == Some("div") {
            divs += 1;
        }
        stack.extend(adapter.children(n));
    }
    assert_eq!(divs, depth);
}

/// The sibling reach family: `>>` / `<<` select siblings at any
/// distance (matcher first, then `?` nearest / `!` farthest), in
/// document order — CSS's `~` and XPath's following/preceding-
/// sibling axes. The one-step `>` / `<` stay adjacent-only.
#[test]
fn sibling_reach() {
    // main's children: p, ul, a, a
    assert_eq!(
        nodes("//p>>*"),
        vec![
            "/html/body/main/a[1]",
            "/html/body/main/a[2]",
            "/html/body/main/ul",
        ]
    );
    // matcher applies before reach: nearest / farthest *a*
    assert_eq!(nodes("//p>>?a"), vec!["/html/body/main/a[1]"]);
    assert_eq!(nodes("//p>>!a"), vec!["/html/body/main/a[2]"]);
    // adjacent-only `>` finds no a (the ul is in between)
    assert_eq!(nodes("//p>a"), Vec::<String>::new());
    // preceding: nearest is the latest one before the node
    assert_eq!(
        nodes("//a[::href = \"https://example.com\"]<<*"),
        vec![
            "/html/body/main/a[1]",
            "/html/body/main/p",
            "/html/body/main/ul",
        ]
    );
    assert_eq!(
        nodes("//a[::href = \"https://example.com\"]<<?*"),
        vec!["/html/body/main/a[1]"]
    );
    assert_eq!(
        nodes("//a[::href = \"https://example.com\"]<<!*"),
        vec!["/html/body/main/p"]
    );
}

const ANATOMY: &str = r##"<html><body>
<ul id="u">
  <li id="a">The <em>oracle</em> at Delphi<ul><li>Pythia</li><li>tripod</li></ul>and a tail</li>
  <li id="b">Plain <b>leaf</b> item</li>
  <li id="c"><ul><li>only nested</li></ul></li>
  <li id="d">Intro<p>first</p>between<p>second</p></li>
  <li id="e">line one<br>line two</li>
  <li id="f">  </li>
</ul>
</body></html>"##;

fn anatomy(query: &str) -> Vec<String> {
    let adapter = HtmlAdapter::parse(ANATOMY);
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

/// The block anatomy (rulings 2026-11-11): an element with nested
/// blocks answers `:::lemma`, its text before the first of them,
/// `:::hypograph`, its text after the last, and `:::grammata`, what
/// lies between; the three partition its text. A leaf, an element
/// with no nested block, has no lemma and no hypograph, and its
/// whole text is its grammata. Block or inline is told by the
/// element's name; an inline child keeps its text in the run.
#[test]
fn the_block_anatomy_partitions_an_element_s_text() {
    let fact = |id: &str, key: &str| {
        anatomy(&format!("//li[::id = \"{id}\"]:::{key}"))
            .pop()
            .unwrap()
    };
    // an item holding a nested list: its own line is the lemma
    assert_eq!(fact("a", "lemma"), "The oracle at Delphi");
    assert_eq!(fact("a", "grammata"), "Pythia tripod");
    assert_eq!(fact("a", "hypograph"), "and a tail");
    // a leaf, inline children or not: all of it is the grammata
    assert_eq!(fact("b", "lemma"), "null");
    assert_eq!(fact("b", "grammata"), "Plain leaf item");
    assert_eq!(fact("b", "hypograph"), "null");
    // no text before or after the nested block: null, not ""
    assert_eq!(fact("c", "lemma"), "null");
    assert_eq!(fact("c", "grammata"), "only nested");
    assert_eq!(fact("c", "hypograph"), "null");
    // text between two nested blocks belongs to the grammata, and
    // two blocks written with no space between read as two words
    assert_eq!(fact("d", "lemma"), "Intro");
    assert_eq!(fact("d", "grammata"), "first between second");
    assert_eq!(fact("d", "hypograph"), "null");
    // a line break is no block and reads as a space
    assert_eq!(fact("e", "grammata"), "line one line two");
    // no text at all
    assert_eq!(fact("f", "grammata"), "null");
    // the query the facts were asked for: one line per item that
    // has items of its own
    assert_eq!(
        anatomy("//ul[::id = \"u\"]/li[:::lemma]:::lemma"),
        ["The oracle at Delphi", "Intro"]
    );
    // the bare projection is unchanged: all the descendant text
    assert_eq!(
        anatomy("//li[::id = \"a\"]::"),
        ["The oracle at DelphiPythiatripodand a tail"]
    );
}

/// `:::texts` is the structural reading, XPath's `text()`: the list
/// of the element's immediate text runs, no child element's text in
/// it, inline ones included; each run collapsed and trimmed, the
/// empty ones dropped, null when none.
#[test]
fn the_immediate_text_runs() {
    let texts = |id: &str| {
        anatomy(&format!("//li[::id = \"{id}\"]:::texts"))
            .pop()
            .unwrap()
    };
    assert_eq!(texts("a"), "@(\"The\"; \"at Delphi\"; \"and a tail\")");
    assert_eq!(texts("b"), "@(\"Plain\"; \"item\")");
    assert_eq!(texts("c"), "null");
    assert_eq!(texts("d"), "@(\"Intro\"; \"between\")");
    assert_eq!(texts("e"), "@(\"line one\"; \"line two\")");
    assert_eq!(texts("f"), "null");
    // a comparison holds when any run satisfies it (the set reading
    // of a list), and the spread answers one run at a time
    assert_eq!(anatomy("//li[:::texts = \"at Delphi\"]::id"), ["a"]);
    assert_eq!(
        anatomy("//li[::id = \"a\"]:::texts | ..."),
        ["The", "at Delphi", "and a tail"]
    );
    // the facts a node answers are listed
    assert_eq!(
        anatomy("//li[::id = \"a\"] | :meta"),
        ["@(:::tag; :::attrs; :::n-attrs; :::lemma; :::grammata; :::hypograph; :::texts)"]
    );
}
