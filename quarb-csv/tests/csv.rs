//! End-to-end tests: queries run through the engine against parsed
//! CSV tables.

use quarb_csv::CsvAdapter;

const DOC: &str = "\
name,dept,age,salary
Ada,eng,36,120000
Bo,sales,17,45000
Cy,eng,64,95000
Dee,\"ops, misc\",41,
";

fn nodes(query: &str) -> Vec<String> {
    let adapter = CsvAdapter::parse(DOC).unwrap();
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| adapter.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}

fn values(query: &str) -> Vec<String> {
    let adapter = CsvAdapter::parse(DOC).unwrap();
    match quarb::run(query, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
fn rows_and_columns() {
    assert_eq!(nodes("/row").len(), 4);
    assert_eq!(values("//row @| count"), vec!["4"]);
    assert_eq!(nodes("/row[2]"), vec!["/row[2]"]);
    assert_eq!(nodes("/row[-1]"), vec!["/row[4]"]);
    assert_eq!(values("/row[1]::name"), vec!["Ada"]);
    // a quoted cell keeps its embedded comma
    assert_eq!(values("/row[4]::dept"), vec!["ops, misc"]);
    // table shape
    assert_eq!(values("^::;n-rows"), vec!["4"]);
    assert_eq!(values("^::;columns"), vec!["name, dept, age, salary"]);
}

#[test]
fn filtering_and_arithmetic() {
    assert_eq!(values("/row[::age > 35]::name"), vec!["Ada", "Cy", "Dee"]);
    assert_eq!(
        values(r#"/row[::dept = "eng" and ::age < 40]::name"#),
        vec!["Ada"]
    );
    // computed comparison over cells
    assert_eq!(values("/row[::salary div ::age > 3000]::name"), vec!["Ada"]);
    // computed column, aggregated
    assert_eq!(
        values("/row[::dept = \"eng\"] | .(::salary + 0) @| sum"),
        vec!["215000"]
    );
}

#[test]
fn missing_values() {
    // Dee's salary cell is empty: the property is null
    assert_eq!(values("/row[::name = \"Dee\"]::salary"), vec![""]);
    // ... which numeric aggregates skip (pandas' NaN behavior)
    assert_eq!(values("/row::salary @| mean"), vec!["86666.66666666667"]);
    assert_eq!(values("/row::salary @| count"), vec!["4"]);
    // ... and truthiness filters (dropna)
    assert_eq!(values("/row[::salary]::name @| count"), vec!["3"]);
    // ... and null propagates through arithmetic
    assert_eq!(values("/row[::name = \"Dee\"] | ::salary * 2"), vec![""]);
    // default(v) fills the hole (pandas fillna); non-nulls pass
    assert_eq!(
        values("/row[::name = \"Dee\"] | ::salary | default(0)"),
        vec!["0"]
    );
    assert_eq!(values("/row[1] | ::salary | default(0)"), vec!["120000"]);
}

#[test]
fn aggregation() {
    assert_eq!(values("/row::age @| median"), vec!["38.5"]);
    assert_eq!(values("/row::age @| max"), vec!["64"]);
    assert_eq!(values("/row::name @| sort @| first"), vec!["Ada"]);
    assert_eq!(values("/row[::dept = \"eng\"]::age @| mean"), vec!["50"]);
}

/// Keyed aggregates over rows: node-preserving, with missing keys
/// not competing.
#[test]
fn keyed_aggregates() {
    assert_eq!(values("/row @| max_by(::salary) | ::name"), vec!["Ada"]);
    // Dee's missing salary does not compete for the minimum
    assert_eq!(values("/row @| min_by(::salary) | ::name"), vec!["Bo"]);
    assert_eq!(values("/row @| top(2, ::age) | ::name"), vec!["Cy", "Dee"]);
    assert_eq!(values("/row @| bottom(1, ::age) | ::name"), vec!["Bo"]);
    // sort_by sends missing keys last
    assert_eq!(
        values("/row @| sort_by(::salary) | ::name"),
        vec!["Bo", "Cy", "Ada", "Dee"]
    );
    assert_eq!(
        values("/row @| unique_by(::dept) | ::dept"),
        vec!["eng", "sales", "ops, misc"]
    );
    // keyed stages compose with positional selection and projection
    assert_eq!(
        values("/row @| sort_by(::age) @| [..2] | ::name"),
        vec!["Bo", "Ada"]
    );
    // ... and preserve node identity: a node result stays one
    assert_eq!(nodes("/row @| max_by(::age)"), vec!["/row[3]"]);
}

/// record(...) builds a record per row; records display as strict
/// JSON, so a stream of them is JSONL.
#[test]
fn records_and_json() {
    // auto-named projections plus a named computed field
    assert_eq!(
        values(r#"/row[::dept = "eng"] | record(::name, "monthly", ::salary div 12)"#),
        vec![
            r#"{"name": "Ada", "monthly": 10000}"#,
            r#"{"name": "Cy", "monthly": 7916.666666666667}"#
        ]
    );
    // | json serializes any topic: strings quote, null is literal
    assert_eq!(values("/row[1]::name | json"), vec![r#""Ada""#]);
    assert_eq!(values("/row[-1]::salary | json"), vec!["null"]);
    // composes with keyed aggregates: top earner as a record
    assert_eq!(
        values("/row @| max_by(::salary) | record(::name, ::salary)"),
        vec![r#"{"name": "Ada", "salary": "120000"}"#]
    );
}

/// Value-keyed grouping: list topics, keys as recallable regs,
/// reductions as separate per-capsa stages.
#[test]
fn grouping() {
    // value_counts
    assert_eq!(
        values("/row @| group(::dept) | count | rec($.dept, \"n\", $_)"),
        vec![
            r#"{"dept": "eng", "n": 2}"#,
            r#"{"dept": "sales", "n": 1}"#,
            r#"{"dept": "ops, misc", "n": 1}"#
        ]
    );
    // groupby().mean() — and groups sort by their aggregate (the
    // null mean, per the missing-key rule, sorts last)
    assert_eq!(
        values(
            "/row::salary @| group(::dept) | mean | .m | rec($.dept, \"mean\", $.m) \
             @| sort_by($.m)"
        ),
        vec![
            r#"{"dept": "sales", "mean": 45000}"#,
            r#"{"dept": "eng", "mean": 107500}"#,
            r#"{"dept": "ops, misc", "mean": null}"#
        ]
    );
    // computed group keys, record-convention named
    assert_eq!(
        values("/row @| group(\"senior\", ::age idiv 40) | count | rec($.senior, \"n\", $_)"),
        vec![r#"{"senior": 0, "n": 2}"#, r#"{"senior": 1, "n": 2}"#]
    );
    // null keys form no group (pandas dropna): Dee's empty salary
    assert_eq!(
        values("/row @| group(::salary) | count @| count"),
        vec!["3"]
    );
}

/// A group carries its member capsae alongside the list topic:
/// keyed aggregates on `|` work per group, `@| ungroup` flattens
/// back with the group's key registers inherited.
#[test]
fn group_members() {
    // per-group top-1 by salary, flattened to rows
    assert_eq!(
        values(
            "/row @| group(::dept) | top(1, ::salary) @| ungroup \
             | rec($.dept, ::name, ::salary)"
        ),
        vec![
            r#"{"dept": "eng", "name": "Ada", "salary": "120000"}"#,
            r#"{"dept": "sales", "name": "Bo", "salary": "45000"}"#,
            r#"{"dept": "ops, misc", "name": "Dee", "salary": null}"#
        ]
    );
    // a keyed stage on `|` rebuilds the list topic from the
    // surviving members, so reductions see the trimmed group
    assert_eq!(
        values("/row::salary @| group(::dept) | top(1, $_) | sum | rec($.dept, \"max\", $_)"),
        vec![
            r#"{"dept": "eng", "max": 120000}"#,
            r#"{"dept": "sales", "max": 45000}"#,
            r#"{"dept": "ops, misc", "max": null}"#
        ]
    );
    // ungroup restores the pre-group typing: an unprojected
    // context comes back as nodes
    assert_eq!(
        nodes("/row @| group(::dept) | top(1, ::age) @| ungroup"),
        vec!["/row[3]", "/row[2]", "/row[4]"]
    );
    // ... and a projected one as its values
    assert_eq!(
        values("/row::name @| group(::dept) | top(1, ::age) @| ungroup"),
        vec!["Cy", "Bo", "Dee"]
    );
    // memberless capsae pass through keyed `|` stages and ungroup
    assert_eq!(
        values("/row | top(1, ::age) @| ungroup @| count"),
        vec!["4"]
    );
}

/// `$ordinal` / `$ord` — the capsa's 1-based position in the
/// current context: ephemeral order, unlike the tree fact
/// `:::index`.
#[test]
fn ordinal() {
    // a computed rank column after a sort
    assert_eq!(
        values("/row @| sort_by(::age) | rec($ord, ::name)"),
        vec![
            r#"{"ordinal": 1, "name": "Bo"}"#,
            r#"{"ordinal": 2, "name": "Ada"}"#,
            r#"{"ordinal": 3, "name": "Dee"}"#,
            r#"{"ordinal": 4, "name": "Cy"}"#
        ]
    );
    // positional sampling in a per-capsa filter
    assert_eq!(
        values("/row | [$ord mod 2 = 1] | ::name"),
        vec!["Ada", "Cy"]
    );
    // the ordinal is the position in the context at that stage:
    // after a filter, survivors renumber
    assert_eq!(
        values("/row | [::dept = \"eng\"] | $ord + 0"),
        vec!["1", "2"]
    );
    // outside a capsa context (navigation predicates) it is null
    assert_eq!(values("/row[$ord = 1] @| count"), vec!["0"]);
}

/// `@| window(span[, key])` — each capsa gains its neighbors at
/// ordinal offsets within the span as members (partial near the
/// edges), so the per-capsa aggregate machinery rolls. Ages in
/// context order: 36, 17, 64, 41.
#[test]
fn windows() {
    // rolling sum of 2 (self + predecessor); partial at row 1
    assert_eq!(
        values("/row | ::age @| window(-1..0) | sum"),
        vec!["36", "53", "81", "105"]
    );
    // `window(n)` is the trailing-count sugar
    assert_eq!(
        values("/row | ::age @| window(2) | sum"),
        vec!["36", "53", "81", "105"]
    );
    // expanding window: cumulative sum
    assert_eq!(
        values("/row | ::age @| window(..0) | sum"),
        vec!["36", "53", "117", "158"]
    );
    // centered window: partial at both edges
    assert_eq!(
        values("/row @| window(-1..1) | count"),
        vec!["2", "3", "3", "2"]
    );
    // leading window
    assert_eq!(
        values("/row | ::age @| window(0..1) | max"),
        vec!["36", "64", "64", "41"]
    );
    // a span past the edge leaves an empty window
    assert_eq!(
        values("/row @| window(..-1) | count"),
        vec!["0", "1", "2", "3"]
    );
    // keyed: neighbors are the nearest same-key capsae, original
    // order preserved (eng, sales, eng, ops)
    assert_eq!(
        values("/row | ::age @| window(-1..0, ::dept) | sum"),
        vec!["36", "17", "100", "41"]
    );
    // members ride the keyed `|` machinery: rolling max
    assert_eq!(
        values("/row | ::age @| window(-1..0) | top(1, $_) | sum"),
        vec!["36", "36", "64", "64"]
    );
    // window is capsa-preserving: registers stay the capsa's own
    assert_eq!(
        values("/row | ::age | .a @| window(-1..0) | sum | $_ - $.a"),
        vec!["0", "36", "17", "64"]
    );
}

/// `@| shift(n[, key])` — the topic becomes the effective topic of
/// the capsa `n` positions back in its partition (negative looks
/// forward; null past the edge).
#[test]
fn shift() {
    assert_eq!(
        values("/row | ::age @| shift(1)"),
        vec!["", "36", "17", "64"]
    );
    assert_eq!(
        values("/row | ::age @| shift(-1)"),
        vec!["17", "64", "41", ""]
    );
    // the diff idiom: row-over-row delta (null at the edge)
    assert_eq!(
        values("/row | ::age | .now @| shift(1) | $.now - $_"),
        vec!["", "-19", "47", "-23"]
    );
    // keyed: the previous same-key capsa (eng, sales, eng, ops)
    assert_eq!(
        values("/row | ::age @| shift(1, ::dept)"),
        vec!["", "", "36", ""]
    );
}

/// `"text ${expr} text"` — interpolated strings: holes evaluate in
/// the current scope and splice as text; null splices as empty; a
/// hole-free double-quoted string stays a plain literal.
#[test]
fn interpolation() {
    assert_eq!(
        values(r#"/row[..2] | "${::name} is ${::age}""#),
        vec!["Ada is 36", "Bo is 17"]
    );
    // full expressions in holes; register recall; \$ escapes
    assert_eq!(
        values(r#"/row[1] | ::salary | .s | "\$${$.s}, monthly \$${$_ idiv 12}""#),
        vec!["$120000, monthly $10000"]
    );
    // null splices as empty (Dee's salary is missing)
    assert_eq!(values(r#"/row[-1] | "[${::salary}]""#), vec!["[]"]);
    // a hole-free double-quoted string is an ordinary literal
    assert_eq!(values(r#"/row[::dept = "eng"] @| count"#), vec!["2"]);
    // interpolations work as function arguments
    assert_eq!(
        values(r#"/row[1] | rec("who", "${::name}/${::dept}")"#),
        vec![r#"{"who": "Ada/eng"}"#]
    );
}

/// The `^` anchor is semantic: inside a subcontext body it reaches
/// back to the arbor root, so a per-capsa computation can consult
/// the whole table (a share-of-total column).
#[test]
fn root_anchor_in_subcontexts() {
    assert_eq!(
        values("/row @| group(::dept) | count | .n | .t(^/row @| count) | $.n * 100 idiv $.t"),
        vec!["50", "25", "25"]
    );
    // at the top level the anchor is inert (root is already the
    // start), and it round-trips through the unparser
    assert_eq!(values("^/row @| count"), vec!["4"]);
}

/// Procedural macros end to end: the body computes over argument
/// forms at expansion time, and the spliced text runs against the
/// table.
#[test]
fn macros_end_to_end() {
    // describe: one stanza per column argument, auto-joined
    assert_eq!(
        values(
            r#"macro &describe(@cols):
                 /cols/* | .k(/projection::key)
                         | "| .${$.k}-mean(/row${::form} @| mean)";
               ^ | &describe(::age, ::salary) | %."#
        ),
        vec![r#"{"age-mean": 39.5, "salary-mean": 86666.66666666667}"#]
    );
    // a computed splice parameterizes a filter at expansion time
    assert_eq!(
        values(r#"macro &odd($n): ^ | "| [\$ord mod ${$n} = 1]"; /row | &odd(2) | ::name"#),
        vec!["Ada", "Cy"]
    );
}

/// Data-aware macros (`&name!`): the body reads the dataset —
/// mounted as `/data` — at expansion time. Outside a hole a
/// parameter is the argument form (`| $col` projects the data by
/// it); inside a hole it splices as text.
#[test]
fn data_aware_macros() {
    // the pivot: distinct key values, read from the data, become
    // one generated count column each
    assert_eq!(
        values(
            r#"macro &pivot!($col):
                 /data/row[::age < 40] | $col @| unique
                   | "| .n-${$_}(/row[${$col} = '${$_}'] @| count)";
               ^ | &pivot!(::dept) | %."#
        ),
        vec![r#"{"n-eng": 2, "n-sales": 1}"#]
    );
    // the form binding also filters the data at expansion time
    assert_eq!(
        values(
            r#"macro &max!($col):
                 /data/row | $col @| max | "| '${$_}'";
               ^ | &max!(::age)"#
        ),
        vec!["64"]
    );
}

/// `window` / `shift` argument and pipe shapes are checked at parse
/// time.
#[test]
fn window_shapes() {
    let err = |q: &str| {
        let adapter = CsvAdapter::parse(DOC).unwrap();
        quarb::run(q, &adapter).unwrap_err().to_string()
    };
    // whole-context only: they read neighbors
    assert!(err("/row | ::age | window(-1..0)").contains("'window' uses '@|'"));
    assert!(err("/row | ::age | shift(1)").contains("'shift' uses '@|'"));
    // an empty span, a zero count, a missing distance
    assert!(err("/row @| window(2..1)").contains("start <= end"));
    assert!(err("/row @| window(0)").contains("offset range or a count"));
    assert!(err("/row @| shift()").contains("integer distance"));
    // a range argument belongs to window alone
    assert!(err("/row @| top(1..2, ::age)").contains("no range argument"));
}

/// `%.` — the register as a record: the named view, one field per
/// name in first-push order, latest value per name; unnamed regs
/// are invisible to it.
#[test]
fn register_record_view() {
    // multi-column output: pushes become fields
    assert_eq!(
        values("/row[::age > 35] | .name(::name) | .age(::age) | %."),
        vec![
            r#"{"name": "Ada", "age": "36"}"#,
            r#"{"name": "Cy", "age": "64"}"#,
            r#"{"name": "Dee", "age": "41"}"#
        ]
    );
    // a group's key regs are already named: %. after a reduction
    // labels the row without rec()
    assert_eq!(
        values("/row @| group(::dept) | count | .n | %."),
        vec![
            r#"{"dept": "eng", "n": 2}"#,
            r#"{"dept": "sales", "n": 1}"#,
            r#"{"dept": "ops, misc", "n": 1}"#
        ]
    );
    // unnamed regs are skipped; a repointed name keeps its place
    // and carries the latest value
    assert_eq!(
        values("/row[1] | .a(::name) | ::dept | . | .b(::age) | .a(::salary) | %."),
        vec![r#"{"a": "120000", "b": "36"}"#]
    );
    // an empty register is an empty record
    assert_eq!(values("/row[1] | %."), vec!["{}"]);
}

/// Fragment definitions expand at parse time: query fragments,
/// parameterized fragments, pipeline fragments, and defs files.
#[test]
fn fragments() {
    assert_eq!(
        values("def &grown: /row[::age >= 18]; &grown @| count"),
        vec!["3"]
    );
    assert_eq!(
        values("def &above($col, $min): /row[$col >= $min]; &above(::salary, 100000) | ::name"),
        vec!["Ada"]
    );
    assert_eq!(
        values(
            "def &cols: | .name(::name) | .dept(::dept) | %.; \
             /row @| top(1, ::salary) | &cols"
        ),
        vec![r#"{"name": "Ada", "dept": "eng"}"#]
    );
    // a def building on an earlier def
    assert_eq!(
        values(
            "def &grown: /row[::age >= 18]; \
             def &grown-eng: &grown | [::dept = \"eng\"]; \
             &grown-eng | ::name"
        ),
        vec!["Ada", "Cy"]
    );
    // arguments are forms, evaluated per capsa at the splice site
    assert_eq!(
        values("def &col($c): | .v($c) | $.v; /row | &col(::age * 2) @| max"),
        vec!["128"]
    );
    // a preloaded defs table (the --defs path)
    let defs = quarb::parse_defs("def &grown: /row[::age >= 18];").unwrap();
    let adapter = CsvAdapter::parse(DOC).unwrap();
    match quarb::run_with_defs("&grown @| count", &defs, &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => assert_eq!(vs[0].to_string(), "3"),
        _ => panic!("expected values"),
    }
}

/// `$1` … `$9` — regex captures: a successful `=~` in a filter
/// stage binds its groups for the rest of the pipeline.
#[test]
fn captures() {
    // extract, then use as fields (Perl's match-then-harvest)
    assert_eq!(
        values(r#"/row | [::dept =~ /^(\w+)s$/] | rec("stem", $1, ::name)"#),
        vec![r#"{"stem": "sale", "name": "Bo"}"#]
    );
    // captures reach pushes, %., and later filters
    assert_eq!(
        values(r#"/row | [::name =~ /^(\w)/] | .initial($1) | .age(::age) | %. @| [1..2]"#),
        vec![
            r#"{"initial": "A", "age": "36"}"#,
            r#"{"initial": "B", "age": "17"}"#
        ]
    );
    // ... and group keys
    assert_eq!(
        values(r#"/row | [::dept =~ /^(\w)/] @| group("initial", $1) | count | .n | %."#),
        vec![
            r#"{"initial": "e", "n": 2}"#,
            r#"{"initial": "s", "n": 1}"#,
            r#"{"initial": "o", "n": 1}"#
        ]
    );
    // a later filter without =~ keeps earlier captures; an
    // unreferenced group index is null
    assert_eq!(
        values(r#"/row | [::name =~ /^(\w+)$/] | [::age > 30] | rec("who", $1, "g2", $2)"#),
        vec![
            r#"{"who": "Ada", "g2": null}"#,
            r#"{"who": "Cy", "g2": null}"#,
            r#"{"who": "Dee", "g2": null}"#
        ]
    );
    // outside any match, captures are null (and falsy)
    assert_eq!(values("/row[1] | rec(\"c\", $1)"), vec![r#"{"c": null}"#]);
}

/// Regression: a property projection evaluated at the arbor root
/// (NodeId 0) must not underflow the 1-based cell index. The root
/// has no cells, so the property is simply null.
#[test]
fn property_at_root_is_null() {
    use quarb::{AstAdapter, NodeId};
    let adapter = CsvAdapter::parse(DOC).unwrap();
    // Used to panic with 'attempt to subtract with overflow'
    // (debug) / silently wrap (release) in CsvAdapter::cell.
    assert!(adapter.property(NodeId(0), "age").is_none());
}

#[test]
fn tsv_and_errors() {
    let tsv = "a\tb\n1\t2\n";
    let adapter = CsvAdapter::parse_with_delimiter(tsv, b'\t').unwrap();
    match quarb::run("/row::b", &adapter).unwrap() {
        quarb::QueryResult::Values(vs) => assert_eq!(vs.len(), 1),
        _ => panic!("expected values"),
    }
    // a ragged record is an error
    assert!(CsvAdapter::parse("a,b\n1,2,3\n").is_err());
}

/// Correlated subqueries: `$$` steps a capsa-scope operand out to
/// the invoking capsa, so a subcontext body can mention "this
/// row's" values — the wide crosstab, groupwise transforms, and
/// share-of-group columns.
#[test]
fn correlated_subqueries() {
    // wide crosstab: one row per dept, one generated cell per
    // condition, each cell counting with THIS group's key
    assert_eq!(
        values(
            "/row @| group(::dept) \
             | .adults(^/row[::dept = $$.dept][::age >= 18] @| count) | %."
        ),
        vec![
            r#"{"dept": "eng", "adults": 2}"#,
            r#"{"dept": "sales", "adults": 0}"#,
            r#"{"dept": "ops, misc", "adults": 1}"#
        ]
    );
    // groupwise transform: each row paired with its group's mean
    assert_eq!(
        values(
            "/row @| [..2] | .d(::dept) \
             | .m(^/row[::dept = $$.d]::salary @| mean) | $.m"
        ),
        vec!["107500", "45000"]
    );
    // $$ord and $$_ reach out too
    assert_eq!(
        values("/row @| [..2] | ::age | .t(^/row[:::index = $$ord]::name) | $.t"),
        vec!["Ada", "Bo"]
    );
    // at the top level there is no enclosing scope: null
    assert_eq!(values("/row[1] | $$.x"), vec![""]);
    // each extra $ steps one more level out
    assert_eq!(
        values(
            "/row[1] | .a(::age) | .x(^/row[1] | .b(::dept) | .y(^/row[::age = $$$.a] @| count) | $.y) | $.x"
        ),
        vec!["1"]
    );
}
