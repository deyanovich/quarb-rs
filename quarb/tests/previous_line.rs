//! The previous-line forms (ruling 2026-11-11): `&_` and `&-N`,
//! found and resolved by the host before the engine parses a line.

use quarb::{PreviousLine, previous_line_forms, resolve_previous_line};

fn resolved(line: &str, last: usize) -> String {
    resolve_previous_line(line, last)
        .unwrap_or_else(|e| panic!("{line}: {e}"))
        .unwrap_or_else(|| panic!("{line}: no form found"))
}

#[test]
fn the_forms_are_found_where_the_sigil_is_the_query_s_own() {
    // the designator is what a label replaces; the form as written
    // is kept for a refusal
    assert_eq!(
        previous_line_forms("&_ | count"),
        [PreviousLine {
            start: 1,
            end: 2,
            back: 1,
            written: "&_".into(),
        }]
    );
    assert_eq!(
        previous_line_forms("/a | &-12"),
        [PreviousLine {
            start: 6,
            end: 9,
            back: 12,
            written: "&-12".into(),
        }]
    );
    // a suffix is not part of the form
    assert_eq!(previous_line_forms("&_#")[0].written, "&_");
    assert_eq!(previous_line_forms("&-2!")[0].written, "&-2");
    // a name goes on: no form
    for line in ["&_x", "&-2x", "&_.a", "&-x", "&-", "&3", "&name", "&_(1)"] {
        assert!(previous_line_forms(line).is_empty(), "{line}");
    }
    // inside a text or a comment the characters are not the sigil
    assert!(previous_line_forms(r#"= "&_ and &-2 and (=_)""#).is_empty());
    assert!(previous_line_forms("/a (!! &_ (=_) ) | count").is_empty());
    // the rounded twins, with the live marker inside the parentheses
    assert_eq!(
        previous_line_forms("(=_) | count"),
        [PreviousLine {
            start: 2,
            end: 3,
            back: 1,
            written: "(=_)".into(),
        }]
    );
    assert_eq!(previous_line_forms("(=-3)")[0].back, 3);
    assert_eq!(previous_line_forms("(=_!)")[0].written, "(=_!)");
    // a rounded fragment by name is no form
    for line in ["(=3)", "(=name)", "(=_x)", "(=-x)"] {
        assert!(previous_line_forms(line).is_empty(), "{line}");
    }
}

#[test]
fn a_form_resolves_to_the_label_it_names() {
    assert_eq!(resolved("&_", 7), "&7");
    assert_eq!(resolved("&-1", 7), "&7");
    assert_eq!(resolved("&-2 @| count", 7), "&6 @| count");
    assert_eq!(resolved("&_# | upper", 7), "&7# | upper");
    assert_eq!(resolved("&_!", 7), "&7!");
    assert_eq!(resolved("&-7", 7), "&1");
    // the rounded twins resolve to the rounded label
    assert_eq!(resolved("(=_) @| count", 7), "(=7) @| count");
    assert_eq!(resolved("(=-2)", 7), "(=6)");
    assert_eq!(resolved("(=_!)", 7), "(=7!)");
    // several in one line, the rest of the line as written
    assert_eq!(
        resolved(
            r#"/a[::k = "&_"] | %(x = (&_ @| count); y = (&-3 @| count))"#,
            9
        ),
        r#"/a[::k = "&_"] | %(x = (&9 @| count); y = (&7 @| count))"#
    );
    // a line with none is left alone
    assert_eq!(resolve_previous_line("/a | &3", 7), Ok(None));
    assert_eq!(resolve_previous_line(r#"= "&_""#, 7), Ok(None));
}

#[test]
fn a_form_before_the_first_line_is_refused() {
    let e = resolve_previous_line("&_", 0).unwrap_err();
    assert_eq!(e, "'&_': no line before this one");
    let e = resolve_previous_line("&-2 | count", 0).unwrap_err();
    assert_eq!(e, "'&-2': no line before this one");
    let e = resolve_previous_line("&-4", 3).unwrap_err();
    assert_eq!(e, "'&-4': no line 4 back (3 ran before this one)");
    let e = resolve_previous_line("(=-4)", 3).unwrap_err();
    assert_eq!(e, "'(=-4)': no line 4 back (3 ran before this one)");
    // a host whose labels are not 1 to N (a notebook kernel, whose
    // labels are its execution counts) answers each label itself
    let labels = [2usize, 5, 9];
    let among = |line: &str| {
        quarb::resolve_previous_line_with(line, labels.len(), |back| {
            labels.len().checked_sub(back).map(|i| labels[i])
        })
    };
    assert_eq!(among("&_ | count"), Ok(Some("&9 | count".into())));
    assert_eq!(among("&-3"), Ok(Some("&2".into())));
    assert_eq!(
        among("&-4").unwrap_err(),
        "'&-4': no line 4 back (3 ran before this one)"
    );
    let e = resolve_previous_line("&-0", 3).unwrap_err();
    assert!(e.starts_with("'&-0' names this line itself"), "{e}");
    // a notebook is told to name its cell
    let e = quarb::previous_line_in_a_file("&-2 @| count").unwrap();
    assert!(e.starts_with("'&-2' is the interactive form"), "{e}");
    assert_eq!(quarb::previous_line_in_a_file("&2 @| count"), None);
}
