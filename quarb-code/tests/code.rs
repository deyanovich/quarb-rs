use quarb_code::CodeAdapter;

#[test]
fn rust_functions_fields_and_lines() {
    let src = "fn main() { greet(\"hi\"); }\nfn greet(s: &str) {}\n";
    let a = CodeAdapter::parse(src, "rs").unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("//function_item::name"), ["main", "greet"]);
    assert_eq!(v("//function_item[::name = \"greet\"]::;start-line"), ["2"]);
    assert_eq!(v("//call_expression::function"), ["greet"]);
    assert_eq!(v("//string_literal:: @| count"), ["1"]);
    assert_eq!(v("/function_item[1]::name"), ["main"]);
}

#[test]
fn python_and_unknown_ext() {
    let a = CodeAdapter::parse("def f():\n    return 1\n", "py").unwrap();
    match quarb::run("//function_definition::name", &a).unwrap() {
        quarb::QueryResult::Values(vs) => assert_eq!(vs[0].to_string(), "f"),
        _ => panic!(),
    }
    assert!(CodeAdapter::parse("x", "zig").is_err());
}

#[test]
fn deeply_nested_does_not_overflow() {
    use quarb::AstAdapter;
    // Thousands of nesting levels: interning the tree by recursion
    // would overflow the call stack. The iterative walk handles it,
    // and tree-sitter parses the nesting itself. `[[[…]]]` is a valid
    // JS array nested `depth` deep.
    let depth = 20_000;
    let src = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    let a = CodeAdapter::parse(&src, "js").unwrap();
    // Parsing returned (no stack overflow) and the tree is well-formed:
    // the top-level expression is the single named child of the root.
    // (Checked shallowly, so the test exercises `build`, not a deep
    // engine traversal.)
    assert_eq!(a.children(a.root()).len(), 1);
}

#[test]
fn c_grammar() {
    let src = r#"
static int helper(int a, int b) { return a + b; }
int main(void) {
    for (int i = 0; i < 3; i++) {
        helper(i, 1);
    }
    return 0;
}
"#;
    let a = quarb_code::CodeAdapter::parse(src, "c").unwrap();
    let got = quarb::run("//function_definition::declarator", &a).unwrap();
    match got {
        quarb::QueryResult::Values(vs) => {
            let names: Vec<String> = vs.iter().map(|v| v.to_string()).collect();
            assert_eq!(names, ["helper(int a, int b)", "main(void)"]);
        }
        _ => panic!("expected values"),
    }
    let got = quarb::run("//for_statement//call_expression", &a).unwrap();
    match got {
        quarb::QueryResult::Nodes(ns) => assert_eq!(ns.len(), 1),
        _ => panic!("expected nodes"),
    }
    assert!(quarb_code::supported("c"));
    assert!(quarb_code::supported("h"));
}
