//! One option rule for every parenthesis (spec: Call Operands and the
//! Options paragraph, draft 164): a dash-led last field opens the
//! option segment in a constructor as in a command, and the
//! constructors take none; the trailing `;` closes the operands with
//! an empty last field, so a dash-led operand keeps its reading
//! (`@(--author;)`), as a parenthesized path always did
//! (`@((--author))`, the recommended and canonical spelling).

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"x":1},"b":{"x":2}}"#).unwrap()
}

fn canonical(q: &str) -> String {
    quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .canonical()
}

fn refuse(q: &str) -> String {
    match quarb::Query::parse(q, &doc()) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("{q} parsed"),
    }
}

fn one(q: &str) -> String {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .run(&doc(), &run)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    match out.result {
        quarb::QueryResult::Values(vs) => {
            assert_eq!(vs.len(), 1, "{q}: {vs:?}");
            vs[0].display_form()
        }
        other => panic!("{q}: {other:?}"),
    }
}

#[test]
fn a_dash_led_last_field_is_the_option_segment_in_every_parenthesis() {
    for q in ["= @(--author)", "= @(1; --author)", "= @(-x)", "= @(1; -x)", "= @(--x=1)"] {
        let e = refuse(q);
        assert!(e.contains("'@(…)' takes no option ('-"), "{q}: {e}");
    }
    let e = refuse("= @(--x; 1)");
    assert!(e.contains("'@(…)' takes no option ('--x' given)"), "{e}");
    for q in ["= %(k = 1; --author)", "= %(--author)", "= %(k = 1; -x)"] {
        let e = refuse(q);
        assert!(e.contains("'%(…)' takes no option ('-"), "{q}: {e}");
    }
    let e = refuse("= #(%(1); --author)");
    assert!(e.contains("'--author'"), "{e}");
    // a command: the segment as before, the void and the flags
    let e = refuse("/a | join(--author)");
    assert!(e.contains("'--author' given"), "{e}");
}

#[test]
fn the_trailing_separator_keeps_the_operand_reading() {
    // the three spellings of the hop as an item say the same; the
    // path group is canonical
    let group = canonical("/a | @((--author))");
    assert_eq!(canonical("/a | @(--author;)"), group);
    assert_eq!(canonical("/a | @((--author);)"), group);
    assert_eq!(group, "/a | @((--author))");
    // a dash-led word as an item is the word
    assert_eq!(one("= @(-x;)"), r#"@("-x")"#);
    assert_eq!(one("= @(1; -x;)"), r#"@(1; "-x")"#);
    // the empty last field is admitted everywhere, and prints nowhere
    assert_eq!(one("= @(1; 2;)"), "@(1; 2)");
    assert_eq!(canonical("= @(1; 2;)"), "= @(1; 2)");
    assert_eq!(canonical("= @(-x;)"), "= @(\"-x\")");
    assert_eq!(one("= %(k = 1;)"), "%(k = 1)");
    assert_eq!(canonical("= %(k = 1;)"), canonical("= %(k = 1)"));
    assert_eq!(one("= #(%(1; 2);) | dim"), "@(1; 2)");
    assert_eq!(one("= 3 | round(1;)"), "3");
    // a command's operand: the hop handed headless, the three spellings
    let group = canonical("/a | join((--author))");
    assert_eq!(canonical("/a | join(--author;)"), group);
    assert_eq!(canonical("/a | join((--author);)"), group);
    assert_eq!(group, "/a | join((--author))");
    // a `;` followed by an operand after a dash-led field stays refused
    let e = refuse("/a | join(--author; 1)");
    assert!(e.contains("the options of 'join' stand last"), "{e}");
    // the segment after a dash-led operand: the group keeps it apart
    assert_eq!(
        canonical("/a | join((--author); --of=::x)"),
        "/a | join((--author); --of=::x)"
    );
}
