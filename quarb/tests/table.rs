//! The HTML grid beside the text table: one record stream, one
//! column rule, the cells typed for the stylesheet.
use quarb::{TABLE_CSS, Value, closes_with_table, table_columns, table_html_plain};

fn rec(fields: &[(&str, Value)]) -> Value {
    Value::Record(
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

#[test]
fn the_grid_has_a_header_a_row_per_record_and_typed_cells() {
    let rows = vec![
        rec(&[
            ("title", Value::Str("Emma".into())),
            ("price", Value::Int(25)),
        ]),
        rec(&[
            ("title", Value::Str("Iliad & Odyssey".into())),
            ("price", Value::Null),
        ]),
    ];
    let html = table_html_plain(&rows, None);
    assert!(html.starts_with("<table class=\"quarb-table\">\n<thead>\n<tr><th>title</th><th class=\"num\">price</th></tr>"), "{html}");
    assert!(
        html.contains("<tr><td>Emma</td><td class=\"num\">25</td></tr>"),
        "{html}"
    );
    assert!(
        html.contains("<td>Iliad &amp; Odyssey</td><td class=\"null\"></td>"),
        "{html}"
    );
    assert!(html.ends_with("</tbody>\n</table>"), "{html}");
}

#[test]
fn named_columns_select_and_order_and_a_scalar_rides_value() {
    let rows = vec![rec(&[("a", Value::Int(1)), ("b", Value::Int(2))])];
    let cols = vec!["b".to_string(), "a".to_string(), "c".to_string()];
    assert_eq!(table_columns(&rows, Some(&cols)), cols);
    let html = table_html_plain(&rows, Some(&cols));
    assert!(
        html.contains("<th class=\"num\">b</th><th class=\"num\">a</th><th>c</th>"),
        "{html}"
    );
    assert!(
        html.contains("<td class=\"num\">2</td><td class=\"num\">1</td><td class=\"null\"></td>"),
        "{html}"
    );
    let scalars = vec![Value::Str("x".into()), Value::Int(3)];
    assert_eq!(table_columns(&scalars, None), ["value"]);
    let html = table_html_plain(&scalars, None);
    // A mixed column is not numeric: the header stays left.
    assert!(html.contains("<th>value</th>"), "{html}");
    assert!(
        html.contains("<td>x</td>") && html.contains("<td class=\"num\">3</td>"),
        "{html}"
    );
}

#[test]
fn text_is_escaped_and_nested_values_are_marked() {
    let rows = vec![rec(&[
        ("t", Value::Str("<b>\"x\" & 'y'</b>".into())),
        ("l", Value::list(vec![Value::Int(1), Value::Int(2)])),
    ])];
    let html = table_html_plain(&rows, None);
    assert!(
        html.contains("<td>&lt;b&gt;&quot;x&quot; &amp; &#39;y&#39;&lt;/b&gt;</td>"),
        "{html}"
    );
    assert!(html.contains("<td class=\"nested\">@(1; 2)</td>"), "{html}");
}

#[test]
fn the_rule_is_the_closing_command() {
    assert!(closes_with_table("/books/* | %(t = /title::) @| table"));
    assert!(closes_with_table("/books/* @| table(title; price)"));
    assert!(closes_with_table("//*<table> @| table"));
    assert!(closes_with_table(
        "//a[::href] | %(t = ::; h = ::href) @| table"
    ));
    assert!(!closes_with_table("/books/* @| csv"));
    assert!(!closes_with_table("/books/* @| table | first"));
    assert!(!closes_with_table("/books/* | table"));
    assert!(!closes_with_table("/books/*"));
    assert!(!closes_with_table("not a query ]["));
    assert!(TABLE_CSS.contains(".quarb-table"));
}
