//! The descriptors (spec: Field Projections): on a record, a table
//! or a list of records `:::key` reads a descriptor of the value;
//! `::key` is the position's property whatever the topic, and a
//! record's field has the single colon alone.
use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"x":1,"y":"two"}}"#).unwrap()
}

fn rows(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        other => panic!("{q}: {other:?}"),
    }
}

#[test]
fn the_rungs_on_a_record_a_list_and_a_table() {
    // `::key` after a record projection is still the position's
    // property (the reach-back idiom), the field the single colon's
    assert_eq!(rows("/a | % | ::x"), ["1"]);
    assert_eq!(rows("/a | % | :x"), ["1"]);
    assert_eq!(
        rows("/a | %(::y) | ++ %(x = ::x)"),
        [r#"%(y = "two"; x = 1)"#]
    );
    assert_eq!(rows(r#"/a | %(::y) | [::x = 1] | :y"#), ["two"]);
    assert_eq!(
        rows("/a | % | $_:%(n = ::x; t = :::type)"),
        [r#"%(n = 1; t = "record")"#]
    );
    // the descriptors
    assert_eq!(rows("/a | % | :::fields"), ["@(:x; :y)"]);
    assert_eq!(rows("/a | % | :::keys"), [r#"@("x"; "y")"#]);
    assert_eq!(rows("/a | % | :::type"), ["record"]);
    assert_eq!(rows("/a | % | :::dim"), ["@(1; 2)"]);
    assert_eq!(rows("/a | % | :::zzz"), ["null"]);
    // a list of records reads through for `:` and `:::`; `::a` is
    // the position's, and a value capsa has none
    let l = "= @(%(a = 1); %(a = 2))";
    assert_eq!(rows(&format!("{l} | ::a")), ["null"]);
    assert_eq!(rows(&format!("{l} | :a")), ["@(1; 2)"]);
    assert_eq!(rows(&format!("{l} | :::fields")), ["@(:a)"]);
    assert_eq!(rows(&format!("{l} | :::nrows")), ["2"]);
    // a table: the column as a list, the descriptors
    let t = r#"= #(@(name; age); %("Braund"; 22); %("Allen"; 35))"#;
    assert_eq!(rows(&format!("{t} | :age")), ["@(22; 35)"]);
    assert_eq!(
        rows(&format!("{t} | :::header")),
        ["@(:name<*; text>; :age<*; int>)"]
    );
    assert_eq!(rows(&format!("{t} | :::type")), [r#"@("text"; "int")"#]);
    assert_eq!(rows(&format!("{t} | :::dim")), ["@(2; 2)"]);
    assert_eq!(rows(&format!("{t} | :::nrows")), ["2"]);
    assert_eq!(rows(&format!("{t} | :::ncols")), ["2"]);
    assert_eq!(rows(&format!("{t} | :::keys")), [r#"@("name"; "age")"#]);
    // a node keeps its rungs: `::` the property, `:::` the adapter's
    assert_eq!(rows("/a | ::x"), ["1"]);
    assert_eq!(rows("/a | :::type"), ["object"]);
    assert_eq!(rows("/a | :name | ::x"), ["1"]);
}
