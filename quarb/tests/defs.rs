//! Fragment definitions (`def`) and the unparser: expansion is
//! observable through `expand` (the `qua --expand` engine), and
//! canonical text must round-trip.

use quarb::{Defs, expand, parse_defs};

fn exp(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

#[test]
fn expansion() {
    // a query fragment splices its branches
    assert_eq!(
        exp("def &adults: /row[::Age >= 18]; &adults @| count"),
        "/row[::Age >= 18] @| count"
    );
    // parameters substitute as forms (call-by-name)
    assert_eq!(
        exp("def &above($col, $min): /row[$col >= $min]; &above(::Fare, 500) | ::Name"),
        "/row[::Fare >= 500] | ::Name"
    );
    // a pipeline fragment splices its stages, pipes intact
    assert_eq!(
        exp("def &cols: | .n(::Name) | %.; /row | &cols"),
        "/row | .n(::Name) | %."
    );
    // a def may invoke an earlier def
    assert_eq!(
        exp("def &adults: /row[::Age >= 18]; \
             def &rich: &adults | [::Fare > 200]; &rich @| count"),
        "/row[::Age >= 18] | [::Fare > 200] @| count"
    );
    // a fragment with a pipeline stands alone as a branch
    assert_eq!(
        exp("def &stats: /row::Age @| mean; &stats"),
        "/row::Age @| mean"
    );
}

#[test]
fn expansion_errors() {
    let err = |q: &str| expand(q, &Defs::default()).unwrap_err().to_string();
    assert!(err("&nope | count").contains("unknown fragment '&nope'"));
    assert!(err("def &a: /x; def &a: /y; /z").contains("already defined"));
    assert!(err("def &a: /x; /row | &a").contains("query fragment"));
    assert!(err("def &p: | upper; &p").contains("pipeline fragment"));
    assert!(err("def &p: | upper; /row @| &p").contains("invoked with '@|'"));
    assert!(err("def &f($x): /row[$x > 1]; &f(1, 2)").contains("takes 1 argument"));
    // recursion is impossible: a def sees only earlier defs
    assert!(err("def &r: &r; /x").contains("unknown fragment '&r'"));
    // params are scoped to their def body
    assert!(err("def &f($x): /row[$x > 1]; /row[$x > 1]").contains("after '$'"));
    // fragments take pipeline filters, not trailing predicates
    assert!(err("def &a: /row; &a[::x > 1]").contains("pipeline filter"));
    // ... and pipe projections, not trailing ones
    assert!(err("def &a: /row; &a::name").contains("through the pipe"));
}

/// Procedural macros: the body is a query evaluated at expansion
/// time against the invocation's expansion arbor; its text results,
/// joined, are reparsed as the expansion.
#[test]
fn macros() {
    // a computed splice: the hole's arithmetic runs at expansion
    assert_eq!(
        exp(
            r#"macro &sample($pct): ^ | "| [\$ord mod ${100 idiv $pct} = 1]";
             /row | &sample(25) @| count"#
        ),
        "/row | [$ord mod 4 = 1] @| count"
    );
    // literal args bind by value; form args splice as their text
    assert_eq!(
        exp(r#"macro &above($col, $min): ^ | "/row[${$col} > ${$min}]";
             &above(::fare, 500) | ::name"#),
        "/row[::fare > 500] | ::name"
    );
    // a rest parameter (@cols) iterates: one stanza per argument,
    // auto-joined; ::form is each argument's unparsed text
    assert_eq!(
        exp(r#"macro &describe(@cols):
               /cols/* | .k(/projection::key)
                       | "| .${$.k}-mean(/row${::form} @| mean)";
             ^ | &describe(::fare, ::age) | %."#),
        "^ | .fare-mean(/row::fare @| mean) | .age-mean(/row::age @| mean) | %."
    );
    // the body reads the argument's *structure* through the locked
    // reflection vocabulary (projections inside predicates included)
    assert_eq!(
        exp(
            r#"macro &cols($q): /q//projection::key | "| .${$_}(::${$_})";
             /row | &cols(/row[::age > 30][::fare < 10]::name) | %."#
        ),
        "/row | .age(::age) | .fare(::fare) | .name(::name) | %."
    );
    // generated text may invoke earlier fragments
    assert_eq!(
        exp(r#"def &adults: /row[::age >= 18];
             macro &counted: ^ | "&adults @| count";
             &counted"#),
        "/row[::age >= 18] @| count"
    );
}

#[test]
fn macro_errors() {
    let err = |q: &str| expand(q, &Defs::default()).unwrap_err().to_string();
    // category mismatches, both directions
    assert!(err(r#"macro &m: ^ | "| upper"; &m"#).contains("expanded to a pipeline fragment"));
    assert!(err(r#"macro &m: ^ | "/row"; /x | &m"#).contains("expanded to a query fragment"));
    // arity (a rest parameter makes it a minimum)
    assert!(err(r#"macro &m($a, @r): ^ | "/x"; &m()"#).contains("takes 1+ argument(s)"));
    // an empty expansion is an error, not a silent no-op
    assert!(err(r#"macro &m: /nope | "| x"; &m"#).contains("expanded to nothing"));
    // the expansion is reparsed: bad generated text names the macro
    assert!(err(r#"macro &m: ^ | "/row[oops"; &m"#).contains("in expansion of '&m'"));
    // a macro body is a query; a bare pipeline body gets the hint
    assert!(err(r#"macro &m: | upper; &m"#).contains("anchor a non-navigating body"));
    // macros see only earlier definitions: recursion is impossible
    assert!(err(r#"macro &r: ^ | "&r"; &r"#).contains("unknown fragment '&r'"));
    // one namespace: a macro cannot shadow a def
    assert!(err(r#"def &a: /x; macro &a: ^ | "/y"; /z"#).contains("already defined"));
}

/// Data-aware macros (`&name!`): the `!` is enforced both ways, and
/// pure expansion (no dataset) refuses them honestly.
#[test]
fn data_aware_errors() {
    let err = |q: &str| expand(q, &Defs::default()).unwrap_err().to_string();
    // a data-aware macro must be invoked with the bang
    assert!(
        err(r#"macro &p!($c): /data/row | $c @| unique | "| x"; ^ | &p(::k)"#)
            .contains("invoke it as '&p!(...)'")
    );
    // ... and nothing else may carry it
    assert!(err(r#"def &a: /x; &a! @| count"#).contains("'&a' is pure"));
    assert!(err(r#"macro &m: ^ | "/y"; &m! @| count"#).contains("'&m' is pure"));
    // pure expansion has no dataset to read
    assert!(
        err(r#"macro &p!($c): /data/row | $c @| unique | "| x"; ^ | &p!(::k)"#)
            .contains("needs an input")
    );
    // `/data` is the dataset's mount: not a parameter name
    assert!(err(r#"macro &p!($data): ^ | "/x"; ^ | &p!(::k)"#).contains("'/data'"));
}

#[test]
fn defs_files() {
    let defs = parse_defs(
        "def &adults: /row[::Age >= 18];\n\
         def &fare-stats: | .name(::Name) | .fare(::Fare) | %.;\n",
    )
    .unwrap();
    assert_eq!(
        expand("&adults | &fare-stats", &defs).unwrap(),
        "/row[::Age >= 18] | .name(::Name) | .fare(::Fare) | %."
    );
    // a defs file holds only definitions
    assert!(parse_defs("def &a: /x; /row").is_err());
}

/// Unparsing is a fixpoint: parse → unparse → parse → unparse is
/// stable, across the syntax surface.
#[test]
fn unparse_fixpoint() {
    let queries = [
        "/row[::Age >= 18] @| count",
        "//book[::pages > 200];;;id",
        "//*<block>[:::index = 2][1..3]$",
        "/items/*[/price:: * /qty:: > 15]/name::",
        "/a || /b::x | upper @| join(', ')",
        "/row @| group(::Pclass) | top(2, ::Fare) @| ungroup | rec($.Pclass, ::Name)",
        "/row | .who | [$ord mod 2 = 1] | $.who",
        "/row | .a(::x) | %. @| [2..-1]",
        "/users/* | .total(/orders/*/amt:: @| sum) | $.total",
        "//user <=> //order[::uid = $*1::id and ::amt > $*1::limit]::",
        "//p[not (::a = 1 or ::b = 2)]",
        "/x | s/foo/bar/g | trim",
        "/x | (:::index + 1) * 3",
        "/x[- :::index = -2]",
        ";;;n-rows",
        "//~(^ch[0-9]+$)/*.rs",
        "/row | [::name =~ /^(\\w+), (\\w+)/] | rec('surname', $1, 'title', $2)",
        "/row | ::fare @| window(-2..0) | mean",
        "/row | ::fare @| window(..0, ::class) | sum",
        "/row | ::fare | .now @| shift(1, ::class) | $.now - $_",
        "/row | ::fare @| window(3) | mean",
        "/row | \"${::name} (${::age})\" @| join(', ')",
        "/row | ::fare | .f | \"fare \\$${$.f}, doubled ${$_ * 2}\"",
        "//h2>>p::text @| join(' ')",
        "//aside<<?*;;;tag || //a[1]>>!p",
        "/row | .d(::dept) | .m(^/row[::dept = $$.d]::pay @| mean) | $.m - $$_",
        "/users/* <=> /orders/*[/uid:: = $*1/id::] | rec('who', $*1/name::, 'amt', /amt::)",
        "/tracks/* | rec(::title, 'artist', ::album_id~>::artist_id~>::name)",
        "/invoices/* | ::qty * ::track_id~>::price @| group(::customer) | sum",
    ];
    for q in queries {
        let once = exp(q);
        let twice = exp(&once);
        assert_eq!(once, twice, "not a fixpoint for {q}");
    }
}
