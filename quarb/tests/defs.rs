//! Fragment definitions (`def`) and the unparser: expansion is
//! observable through `expand` (the `qua --expand` engine), and
//! canonical text must round-trip.

use quarb::{Defs, expand, expand_first, parse_defs};

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
}

/// The record convention breaks the call-operand duality — a
/// leading field name would ride as the topic and silently re-key
/// the record — so `rec`/`record` refuse operand position (found
/// porting `aif`: `&f(rec("seen", ::Fare))` reflected and ran as
/// `('seen' | rec(::Fare))`, keying the record `Fare`).
#[test]
fn rec_refuses_call_operand_position() {
    let err = |q: &str| expand(q, &Defs::default()).unwrap_err().to_string();
    assert!(
        err("/row[rec('seen', ::Fare) = 1]").contains("re-keying"),
        "rec as predicate operand must refuse"
    );
    assert!(
        err("macro &f($t): /t | \"| ${::form}\"; /row | &f(rec('seen', ::Fare))")
            .contains("re-keying"),
        "rec as macro argument must refuse"
    );
    // the honest spelling still parses and round-trips
    assert_eq!(exp("/row | ('seen' | %(::Fare))"), "/row | ('seen' | %(::Fare))");
}

/// Ruling #22: capture must be invited through an argument. A
/// macro-pushed register recalled by the surrounding query is the
/// classic accidental-capture pitfall (LISP's broken `swap`) and
/// refuses; passing the name — or a `$.name` reference — through
/// the parentheses is deliberate anaphora (the `aif` shape) and
/// stays legal. `def` fragments are exempt: their bodies are
/// literal text the author wrote in plain sight.
#[test]
fn invited_capture() {
    let err = |q: &str| expand(q, &Defs::default()).unwrap_err().to_string();
    // The v=9 accident: uninvited push, outside recall — refused.
    assert!(
        err("macro &m($n): //step[1] | \"| .t(9)\"; \
             ^ | .t(1) | &m(/x) | rec(\"v\", $.t)")
            .contains("invited through an argument"),
        "uninvited capture must refuse"
    );
    // aif: the caller's branch arrives as an argument carrying
    // `$.it`, so the emitted `.it` push is invited.
    assert_eq!(
        exp("macro &aif($c, $t): /c | \"| .it(${::form}) | ${^/t::form}\"; \
             /row | &aif(::Fare, ($.it))"),
        "/row | .it(::Fare) | $.it"
    );
    // Pivot shape: uninvited pushes swept up by `%.` — no outside
    // recall names them, so the lint stays quiet.
    assert_eq!(
        exp("macro &m($n): //step[1] | \"| .z(1)\"; /row | &m(/x) | %."),
        "/row | .z(1) | %."
    );
    // Inviting by bare name (the explicit-binding escape): the
    // argument hands the register name through the parentheses.
    assert_eq!(
        exp("macro &m($r): //step[1] | \"| .${::matcher}(9)\"; \
             ^ | &m(/t) | rec(\"v\", $.t)"),
        "^ | .t(9) | %(v = $.t)"
    );
    // A def doing the same thing stays legal — bodies are literal.
    assert_eq!(
        exp("def &load: | .t(9); ^ | &load | rec(\"v\", $.t)"),
        "^ | .t(9) | %(v = $.t)"
    );
}

/// `--expand-1` (macroexpand-1): each directly-invoked macro's
/// generated text, before re-expansion — the `&wrap` chain takes
/// two visible steps where `--expand` shows only the fixed point.
#[test]
fn expand_first_steps() {
    let q = "def &adults: /row[::Age >= 18]; \
             macro &wrap($agg): //step[1] | \"&adults @| ${::matcher}\"; \
             &wrap(/count)";
    assert_eq!(
        expand_first(q, &Defs::default()).unwrap(),
        vec!["&adults @| count".to_string()]
    );
    assert_eq!(exp(q), "/row[::Age >= 18] @| count");
    // No macro invocations: nothing to report (defs are textual).
    assert_eq!(
        expand_first("def &a: /x; &a @| count", &Defs::default()).unwrap(),
        Vec::<String>::new()
    );
    // Diagnostics still fire in the -1 lens: the full parse runs.
    assert!(
        expand_first("macro &m($n): //step[1] | \"| .t(9)\"; \
                      ^ | .t(1) | &m(/x) | rec(\"v\", $.t)", &Defs::default())
            .unwrap_err()
            .to_string()
            .contains("invited through an argument")
    );
}

/// Path-position splicing (ruling #17): invocations are legal
/// mid-path, in groups, and as predicate operands; trailing
/// refinement group-wraps.
#[test]
fn path_splice() {
    // mid-path: the walk continues through the fragment
    assert_eq!(
        exp("def &card: //div[::kind = 'card']; //section&card/h3::"),
        "//section//div[::kind = 'card']/h3::"
    );
    // the article-17 guard: a group body under a written quantifier
    assert_eq!(
        exp("def &clean: (<-cookie->ip<-ip->cookie); /cookies/C&clean+ @| count"),
        "/cookies/C(<-cookie->ip<-ip->cookie)+ @| count"
    );
    // trailing predicate group-wraps (expression predicates only)
    assert_eq!(
        exp("def &a: /row; &a[::x > 1]"),
        "(/row)[::x > 1]"
    );
    // trailing projection: the body ends the branch
    assert_eq!(exp("def &a: /row; &a::name"), "/row::name");
    // a projected body ends the branch where it stands
    assert_eq!(
        exp("def &price: /span[::kind = 'p']::; //div&price @| max"),
        "//div/span[::kind = 'p']:: @| max"
    );
    // quantifier + reach on a plain body wrap it as a group
    assert_eq!(
        exp("def &down: /wrap; /root&down{2,3}?/leaf::"),
        "/root(/wrap){2,3}?/leaf::"
    );
    // two fragments splice in sequence
    assert_eq!(
        exp("def &a: /x; def &b: /y[1]; /root&a&b::"),
        "/root/x/y[1]::"
    );
}

#[test]
fn union_body_as_group() {
    // a union body mid-path becomes a path-pattern group
    assert_eq!(
        exp("def &either: /a || /b; /x&either/c::"),
        "/x(/a|/b)/c::"
    );
    // ... adopting a written quantifier directly
    assert_eq!(exp("def &either: /a || /b; /x&either{2}"), "/x(/a|/b){2}");
    // at branch head a bare union splices whole (the historical
    // behavior) — the group form appears only under refinement
    assert_eq!(exp("def &either: /a || /b; &either @| count"), "/a || /b @| count");
    assert_eq!(exp("def &either: /a || /b; &either+ @| count"), "(/a|/b)+ @| count");
    // a projected union splices whole at head, as before
    assert_eq!(
        exp("def &two: /a::x || /b::y; &two @| count"),
        "/a::x || /b::y @| count"
    );
}

#[test]
fn group_splice() {
    // an invocation inside a group alternative
    assert_eq!(
        exp("def &hop: <-cookie; /x(&hop|->ip)+ @| count"),
        "/x(<-cookie|->ip)+ @| count"
    );
    // a group-bodied fragment nests
    assert_eq!(
        exp("def &pair: (/a/b); /x(&pair|/c){1,2}"),
        "/x((/a/b)|/c){1,2}"
    );
}

/// Predicate fragments (`def &vis: [cond];`) and operand splices.
#[test]
fn predicate_splice() {
    // a guard refines the step before it
    assert_eq!(
        exp("def &vis: [not ::style =~ /none/]; //div&vis/h3::"),
        "//div[!::style =~ (/none/)]/h3::"
    );
    // bracket predicates after a guard join the same predicate run
    assert_eq!(
        exp("def &vis: [::s]; //span&vis[::k = 'p']::"),
        "//span[::s][::k = 'p']::"
    );
    // ... and a group's match set
    assert_eq!(
        exp("def &deep: [::d > 2]; /x(/wrap)+&deep"),
        "/x(/wrap)+[::d > 2]"
    );
    // ... and rides the plain pipe as a per-capsa filter
    assert_eq!(
        exp("def &vis: [not ::style =~ /none/]; //div | &vis | ::id"),
        "//div | [!::style =~ (/none/)] | ::id"
    );
    // as an operand it reads as a boolean
    assert_eq!(
        exp("def &vis: [::x > 1]; //div[&vis && ::y = 2]::"),
        "//div[(::x > 1) && ::y = 2]::"
    );
    // a bare navigation body in a predicate is an existence test
    assert_eq!(
        exp("def &guard: /flag; //row[&guard]::"),
        "//row[/flag]::"
    );
    // a projected body compares by value
    assert_eq!(
        exp("def &price: /span::; //div[&price > 10]::"),
        "//div[/span:: > 10]::"
    );
    // an anchored body is the `^`-operand pattern
    assert_eq!(
        exp("def &top: ^/set/*::x; /row[::x = &top]::"),
        "/row[::x = ^/set/*::x]::"
    );
    // parameters substitute inside a guard
    assert_eq!(
        exp("def &min($n): [::price > $n]; //div&min(10)::"),
        "//div[::price > 10]::"
    );
}

#[test]
fn path_splice_errors() {
    let err = |q: &str| expand(q, &Defs::default()).unwrap_err().to_string();
    // a pipeline-carrying body cannot enter a walk
    assert!(err("def &s: /row | upper; /x&s").contains("carries a pipeline"));
    // ... nor a correlation
    assert!(
        err("def &j: /a <=> /b[::x = $$::x]; /x&j").contains("carries a correlation")
    );
    // ... nor a re-anchoring body
    assert!(err("def &t: ^/a; /x&t").contains("re-anchors"));
    // a projected body cannot continue the walk
    assert!(err("def &p: /a::x; /root&p/child").contains("cannot continue past it"));
    // ... nor stand in a group alternative
    assert!(err("def &p: /a::x; /root(&p|/b)").contains("group alternative walks on"));
    // positional refinement keeps the group teaching error
    assert!(err("def &a: /row; &a[2]").contains("expression predicates only"));
    // the trait refusal is unchanged (a step carries traits; a
    // splice has no single step)
    assert!(err("def &a: /row; &a<block>").contains("trailing trait selector"));
    // a guard needs something to refine
    assert!(err("def &vis: [::x]; &vis").contains("nothing precedes"));
    // reverse resolution stays out of predicates, spliced or not
    assert!(
        err("def &back: ::id<--; /row[&back]::").contains("reverse resolution")
    );
    // trailing-refinement refusals survive for pipeline-carrying
    // bodies, where refine-through-the-pipe is still the truth
    assert!(
        err("def &s: /row | upper; &s[::x > 1]").contains("pipeline filter")
    );
    assert!(err("def &s: /row | upper; &s::name").contains("through the pipe"));
}

#[test]
fn macro_path_splice() {
    // a macro expansion splices mid-path (the brace quantifier's
    // canonical form is the explicit group)
    assert_eq!(
        exp(r#"macro &hop($n): ^ | "/w{${$n}}"; /x&hop(2)/leaf::"#),
        "/x(/w){2}/leaf::"
    );
    // ... and under a written quantifier
    assert_eq!(
        exp(r#"macro &pair: ^ | "(/a/b)"; /x&pair+ @| count"#),
        "/x(/a/b)+ @| count"
    );
    // ... and as an operand
    assert_eq!(
        exp(r#"macro &lim: ^ | "10"; //div[::price > &lim]::"#),
        "//div[::price > 10]::"
    );
    // a pipeline expansion refuses at path position
    assert!(
        expand(r#"macro &m: ^ | "| upper"; /x&m"#, &Defs::default())
            .unwrap_err()
            .to_string()
            .contains("at path position it must expand to navigation steps")
    );
    // several branch-shaped values would silently concatenate into
    // one path; the expansion lint refuses instead
    assert!(
        expand(
            r#"macro &m(@xs): /xs/* | "//div[::k = ${::form}]"; &m(1, 2) @| count"#,
            &Defs::default()
        )
        .unwrap_err()
        .to_string()
        .contains("branch-shaped values")
    );
    // the ledger holds at the new sites: generated text sees only
    // earlier definitions
    assert!(
        expand(r#"macro &r: ^ | "/x&r"; /a&r"#, &Defs::default())
            .unwrap_err()
            .to_string()
            .contains("unknown fragment '&r'")
    );
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

/// A defs file is a library, and a library wants a header: lines
/// whose first non-blank character is `#` are comments.
#[test]
fn defs_file_comments() {
    let defs = parse_defs(
        "# birth-record fragments\n\
         #   (verified against titanic-births.json)\n\
         def &adults: /row[::Age >= 18];\n\
         \n\
         # Julian to Gregorian, 1900-1917\n\
         def &gregorian: | tp(\"%Y-%m-%d\") | ($_ + 12d);\n",
    )
    .unwrap();
    assert_eq!(
        expand("&adults", &defs).unwrap(),
        "/row[::Age >= 18]"
    );
    assert_eq!(
        expand("/x | &gregorian", &defs).unwrap(),
        "/x | tp('%Y-%m-%d') | $_ + '12d'"
    );
    // only a line-leading `#` comments; `#` has no meaning inside a
    // definition body, so a mid-line `#` still errors as query text
    assert!(parse_defs("def &a: /x # trailing;\n").is_err());
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
        "/row | .(::x) | .b(::y) | %%.",
        "/users/* | .total(/orders/*/amt:: @| sum) | $.total",
        "//user <=> //order[::uid = $$::id and ::amt > $$::limit]",
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
        "//h2>>p:: @| join(' ')",
        "//aside<<?*;;;tag || //a[1]>>!p",
        "/row | .d(::dept) | .m(^/row[::dept = $$.d]::pay @| mean) | $.m - $$_",
        "/users/* <=> /orders/*[/uid:: = $$/id::] | rec(::name, 'amt', $*1/amt::)",
        "/tracks/* | rec(::title, 'artist', ::album_id~>::artist_id~>::name)",
        "/invoices/* | ::qty * ::track_id~>::price @| group(::customer) | sum",
        // Round-trip regressions (2026-07-21 review): constant and
        // operand stages keep their parens; a quoted dot-leading
        // string stays a constant topic; quoted trait names reprint
        // quoted; `<-`/`<` keep their axis before digit-/dash-led
        // matchers; a trailing leaf anchor regroups before a plain
        // pipe instead of reparsing as the map pipe.
        "/a | (3)",
        "/a | (now())",
        "/a | (- 3)",
        "/a | (@*)",
        "/files | '.gitignore'",
        "//x<'my trait'>",
        "//a <- 3",
        "//a < -x",
        "def &f: /a$; &f | upper",
        // Path-position splices (ruling #17): the expansions must
        // themselves round-trip.
        "def &clean: (<-a->b); /x&clean+ @| count",
        "def &g: /a || /b; /x&g{2}",
        "def &p: /price::; //row[&p > 10]::name",
        "def &vis: [not ::style =~ /none/]; //div&vis/h3::",
    ];
    for q in queries {
        let once = exp(q);
        let twice = exp(&once);
        assert_eq!(once, twice, "not a fixpoint for {q}");
    }
}
