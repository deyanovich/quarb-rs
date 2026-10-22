//! The HTML forms a drawing leaves beside its text (`Outcome::html`):
//! the tree as folding `details`, the histogram as a bar grid; the
//! browser hosts render them where the terminal prints the text.
fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"x":1,"y":[1,2],"z":{"w":"s"}},"n":[1,2,2,3,10,null]}"#).unwrap()
}

fn outcome(q: &str) -> quarb::Outcome {
    let d = doc();
    quarb::Query::parse(q, &d).unwrap().run(&d, &quarb::Run::default()).unwrap()
}

#[test]
fn a_drawing_leaves_its_html_form_on_the_outcome() {
    assert!(quarb::closes_with_drawing("/a @| tree(1; -k)"));
    assert!(quarb::closes_with_drawing("/n/*:: @| histogram(3)"));
    assert!(!quarb::closes_with_drawing("/a @| count"));
    assert!(!quarb::closes_with_drawing("/a | tree"));
    // the tree: the controls, a folding node per node with children,
    // a plain line per leaf, the text escaped
    let o = outcome("/a @| tree(1; -k)");
    let html = o.html.expect("the tree's html");
    assert!(html.starts_with("<div class=\"quarb-tree\"><div class=\"controls\"><button"), "{html}");
    assert!(html.contains("expand all") && html.contains("collapse all"), "{html}");
    assert!(html.contains("<details open class=\"node\"><summary>/ &lt;object&gt;</summary>"), "{html}");
    assert!(html.contains("<details open class=\"node\"><summary>a &lt;object&gt;</summary>"), "{html}");
    assert!(html.contains("<div class=\"node\">x &lt;number&gt;</div>"), "{html}");
    assert!(html.contains("<div class=\"node\">y &lt;array&gt;</div>"), "{html}");
    assert!(html.ends_with("</details></details></div>"), "{html}");
    // a stub is a dotted container, a crosslink a line of its own
    let o = outcome("/a/z/w @| tree(0)");
    let html = o.html.expect("the tree's html");
    assert!(html.contains("<details open class=\"stub\"><summary>a/z</summary><div class=\"node\">w</div></details>"), "{html}");
    // the histogram: a row per bar, the width the count's share of
    // the scale, the null bar aside
    let o = outcome("/n/*:: @| histogram(3)");
    let html = o.html.expect("the histogram's html");
    assert!(html.starts_with("<table class=\"quarb-table quarb-histogram\"><tbody>"), "{html}");
    assert!(html.contains("<tr><td class=\"label\">1–4</td><td class=\"bar\"><div style=\"width:100.0%\"></div></td><td class=\"num\">4</td></tr>"), "{html}");
    assert!(html.contains("<tr><td class=\"label\">4–7</td><td class=\"bar\"><div style=\"width:0.0%\"></div></td><td class=\"num\">0</td></tr>"), "{html}");
    assert!(html.contains("<tr class=\"aside\"><td class=\"label\">null</td><td class=\"bar\"><div style=\"width:25.0%\"></div></td><td class=\"num\">1</td></tr>"), "{html}");
    // no drawing, no form; the stylesheet dresses both
    assert!(outcome("/a @| count").html.is_none());
    assert!(quarb::TABLE_CSS.contains(".quarb-tree") && quarb::TABLE_CSS.contains(".quarb-histogram"));
}
