//! Typed projectors and column states (spec: Projectors; The Table,
//! "Columns"; "Mapping over the cells", draft 166): `:A<int>` declares
//! the field it names and stands only where a field is declared; a
//! declared column is fixed, casting what arrives or refusing by
//! name; an undeclared one types by its cells and widens; `map`'s
//! bare trait body redeclares.
use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"x":"1","y":"two"}}"#).unwrap()
}

fn rows(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        other => panic!("{q}: {other:?}"),
    }
}

fn one(q: &str) -> String {
    let v = rows(q);
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

const T: &str = r#"#(@(name; age); %("Braund"; 22); %("Allen"; 35))"#;

#[test]
fn a_typed_projector_is_a_projector_with_its_declaration() {
    // it prints as written, is a projector, and `:::type` reads the
    // declared type, null under the star or on a bare projector
    assert_eq!(one("= :A<int>"), ":A<int>");
    assert_eq!(one("= ::price<float>"), "::price<float>");
    assert_eq!(
        one("= @(:zip<text>; :amount<*; float>; :paid<*>)"),
        "@(:zip<text>; :amount<*; float>; :paid<*>)"
    );
    assert_eq!(one("= :A<int> | type"), "projector");
    assert_eq!(one("= :A<int> | :::type"), "int");
    assert_eq!(one("= :A<*; int> | :::type"), "null");
    assert_eq!(one("= :A | :::type"), "null");
    // the type words are checked when the query is read
    let e = refuse("= :A<nope>");
    assert!(
        e.contains("'nope' is none") && e.contains("'<*>' declares nothing"),
        "{e}"
    );
}

#[test]
fn a_typed_projector_is_refused_as_a_value() {
    // right of `=` the pointer names the declared key and the cast
    let e = refuse("= %(a = :x<int>)");
    assert!(
        e.contains("declares nothing")
            && e.contains("'a<int> = :x'")
            && e.contains("'a = int(:x)'"),
        "{e}"
    );
    // as an operand it is refused at run time
    let e = refuse(&format!("= {T} | :age<int>"));
    assert!(
        e.contains("declares a field, and stands where a value does") && e.contains("int(:age)"),
        "{e}"
    );
    // in a table row literal likewise
    let e = refuse(r#"= #(@(a); %(a = :x<int>))"#);
    assert!(e.contains("declares nothing"), "{e}");
}

#[test]
fn a_record_key_with_a_trait_casts_its_value() {
    // the declared key: the value cast on build, a bare typed
    // projector keying its own field
    assert_eq!(
        one(r#"= %(price<float> = "2.5"; n = 1)"#),
        "%(price = 2.5; n = 1)"
    );
    assert_eq!(one(r#"/a | %(::x<int>; ::y)"#), r#"%(x = 1; y = "two")"#);
    assert_eq!(one(r#"= %(x = "3") | %(:x<int>)"#), "%(x = 3)");
    let e = refuse(r#"= %(x<int> = "4.5")"#);
    assert!(e.contains("'x' is declared int, and holds \"4.5\""), "{e}");
    // `map(<float>; --to=:price)` casts the field on a record
    assert_eq!(
        one(r#"= %(a = "2"; b = "x") | map(<int>; --to=:a)"#),
        r#"%(a = 2; b = "x")"#
    );
    let e = refuse(r#"= %(a = "2"; b = "x") | map(<int>)"#);
    assert!(e.contains("field 'b' is declared int"), "{e}");
}

#[test]
fn a_declared_column_is_fixed_and_an_undeclared_one_widens() {
    let d = format!("= {T} | #(--header=@(:name; :age<int>)) | .t");
    // appended rows cast into the declaration or refuse by column
    assert_eq!(
        one(&format!(r#"{d} | $.t ++ %("Zed"; "7") | header"#)),
        "@(:name<*; text>; :age<int>)"
    );
    assert_eq!(
        one(&format!(r#"{d} | $.t ++ %("Zed"; "7") | :age"#)),
        "@(22; 35; 7)"
    );
    let e = refuse(&format!(r#"{d} | $.t ++ %("Zed"; "x")"#));
    assert!(
        e.contains("column 'age' is declared int, and row 3 holds \"x\""),
        "{e}"
    );
    let e = refuse(&format!(r#"{d} | $.t ++ #(@(name; age); %("Zed"; "x"))"#));
    assert!(e.contains("column 'age' is declared int"), "{e}");
    // an undeclared column widens: text at the top
    assert_eq!(
        one(&format!(r#"= {T} ++ %("Zed"; "x") | header"#)),
        "@(:name<*; text>; :age<*; text>)"
    );
    // mapped cells cast or refuse; an undeclared column re-types
    assert_eq!(
        one(&format!("{d} | $.t | map(* 2; --to=:age) | header")),
        "@(:name<*; text>; :age<int>)"
    );
    let e = refuse(&format!("{d} | $.t | map(* 1.5; --to=:age)"));
    assert!(
        e.contains("column 'age' is declared int, and row 2 holds 52.5"),
        "{e}"
    );
    assert_eq!(
        one(&format!("= {T} | map(* 1.5; --to=:age) | header")),
        "@(:name<*; text>; :age<*; float>)"
    );
    // arithmetic over the table likewise
    assert_eq!(one(&format!("{d} | $.t + 1 | :age")), "@(23; 36)");
    let e = refuse(&format!("{d} | $.t * 1.5"));
    assert!(e.contains("column 'age' is declared int"), "{e}");
    // a row selection keeps the declarations; `values` keeps the
    // columns and their types; `type` is the effective type either way
    assert_eq!(
        one(&format!("{d} | $.t | [1..1] | header")),
        "@(:name<*; text>; :age<int>)"
    );
    assert_eq!(
        one(&format!("{d} | $.t | values | header")),
        "@(:1<*; text>; :2<int>)"
    );
    assert_eq!(one(&format!("{d} | $.t | type")), r#"@("text"; "int")"#);
}

#[test]
fn a_trait_as_the_body_of_map_redeclares() {
    // `map(<int>; --to=:D)` declares and casts, `map(<*>)` undeclares,
    // no --to every column, --to naming rows refuses, a node refuses
    assert_eq!(
        one(&format!("= {T} | map(<float>; --to=:age) | header")),
        "@(:name<*; text>; :age<float>)"
    );
    assert_eq!(
        one(&format!(
            "= {T} | map(<float>; --to=:age) | :age | [1] | type"
        )),
        "float"
    );
    assert_eq!(
        one(&format!(
            "= {T} | map(<float>; --to=:age) | map(<*>) | header"
        )),
        "@(:name<*; text>; :age<*; float>)"
    );
    assert_eq!(
        one(&format!("= {T} | map(<text>) | header")),
        "@(:name<text>; :age<text>)"
    );
    let e = refuse(&format!("= {T} | map(<int>; --to=:name)"));
    assert!(
        e.contains("column 'name' is declared int, and row 1 holds \"Braund\""),
        "{e}"
    );
    let e = refuse(&format!("= {T} | map(<int>; --to=[1])"));
    assert!(e.contains("rows have no type"), "{e}");
    let e = refuse(&format!("= {T} | map(<int>; --to=:zip)"));
    assert!(e.contains("no column 'zip'"), "{e}");
    let e = refuse("/a | map(+ 1)");
    assert!(e.contains("a node is none"), "{e}");
}

#[test]
fn a_typed_structured_projector_casts_what_it_reads() {
    // the table shape declares its columns; the list shape stays a
    // list of cast columns; a cell that does not cast refuses by name
    assert_eq!(
        one(&format!("= {T} | .t | $.t:#(:age<float>; :name) | header")),
        "@(:age<float>; :name<*; text>)"
    );
    assert_eq!(
        one(&format!(
            "= {T} | .t | $.t:@(:age<float>) | [1] | [2] | type"
        )),
        "float"
    );
    assert_eq!(
        one(&format!(
            "= {T} | .t | $.t:%(:age<float>) | :age | [1] | type"
        )),
        "float"
    );
    let e = refuse(&format!("= {T} | .t | $.t:@(:name<int>)"));
    assert!(
        e.contains("'name' is declared int, and item 1 holds \"Braund\""),
        "{e}"
    );
    let e = refuse(&format!("= {T} | .t | $.t:#(:name<int>)"));
    assert!(e.contains("column 'name' is declared int"), "{e}");
    // the spread of a held header narrows and casts another table
    assert_eq!(
        one(&format!(
            r#"= @(:age<float>) | .h | {T} | .t | $.t:#($.h...) | header"#
        )),
        "@(:age<float>)"
    );
}
