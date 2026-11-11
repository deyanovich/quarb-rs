//! The closure of mounts a query may reach through the model's
//! declarations, for a replay plan.

use quarb_model::parse_model;

#[test]
fn refs_rels_and_trees_close_over_the_named_roots() {
    let model = parse_model(
        "mount a: a.json;\n\
         mount b: b.json;\n\
         mount c: c.json;\n\
         mount d: d.json;\n\
         mount e: e.json;\n\
         ref /a/*::k --> /b/items;\n\
         ref /b/items/*::k --> /c/rows;\n\
         rel /e/* -> /d/*[::x = $$0::x];\n\
         tree x: /d;\n",
    )
    .unwrap();
    assert_eq!(model.reachable_roots(&["a"]), ["a", "b", "c", "d"]);
    assert_eq!(model.reachable_roots(&["e"]), ["e", "d"]);
    assert_eq!(model.reachable_roots(&[]), ["d"]);
}

/// A relation's condition names its source `$$0`; a bare `$$` does
/// not lex, and the relation it declared held nowhere, silently.
#[test]
fn a_bare_position_in_a_relation_is_refused() {
    let e = parse_model("rel /authors/* -> /books/* [::author = $$::id];").unwrap_err();
    assert!(e.contains("the relation's source is '$$0'"), "{e}");
    // the operands the engine knows pass, and text is left alone
    for cond in [
        "::author = $$0::id",
        "$$_::author = $$0::id",
        "::title = '$$'",
    ] {
        parse_model(&format!("rel /authors/* -> /books/* [{cond}];")).unwrap();
    }
}

/// `use FILE;` brings the used file's statements, not its `graph`
/// declarations (spec: Model Files, Composition): a graph is
/// declared by the model that is opened. Its `property` statements
/// join with the rest.
#[test]
fn a_used_models_graph_declarations_stay_out() {
    let dir = std::env::temp_dir().join(format!("quarb-model-use-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("base.model"),
        "graph /airports/*(->*) { pagerank; };\n\
         property /edges/row::double<int> = ::km * 2;\n",
    )
    .unwrap();
    std::fs::write(dir.join("user.model"), "use base.model;\n").unwrap();
    let model = quarb_model::parse_model_file(&dir.join("user.model")).unwrap();
    assert!(model.graphs.is_empty());
    assert_eq!(model.properties.len(), 1);
}

/// A later model cell adds every kind of statement to the cells
/// before it: the notebook merged the fields by hand and lost the
/// three newest (`property`, `graph`, `weight-potential`).
#[test]
fn a_model_absorbs_every_statement_of_another() {
    let mut base = parse_model("ref /a/*::k --> /b/items;").unwrap();
    let cell = parse_model(
        "property /edges/row::double<int> = ::km * 2;\n\
         graph /airports/*(->*) { pagerank; };\n\
         weight-potential /stations/*::opened;\n\
         rel /e/* -> /d/* [::x = $$0::x];",
    )
    .unwrap();
    base.absorb(&cell);
    assert_eq!(base.refs.len(), 1);
    assert_eq!(base.properties.len(), 1);
    assert_eq!(base.graphs.len(), 1);
    assert_eq!(base.potentials.len(), 1);
    assert_eq!(base.rels.len(), 1);
}

/// A quoted name after `as` (ruling 2026-11-10): the quotes are
/// delimiters, and a quote inside the name is escaped. The quotes
/// were kept in the label, so `->e` matched nothing.
#[test]
fn a_quoted_label_is_the_name_between_its_quotes() {
    use quarb_model::Label;
    let label = |statement: &str| {
        let model = parse_model(&format!(
            "ref /e/*::a --> /n/*::id;\nref /e/*::b --> /n/*::id;\n{statement}"
        ))
        .unwrap();
        match model.edges[0].label.clone() {
            Some(Label::Name(n)) => n,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(label("edge /e/*: ::a -- ::b as e;"), "e");
    assert_eq!(label("edge /e/*: ::a -- ::b as \"e\";"), "e");
    assert_eq!(label("edge /e/*: ::a -- ::b as 'two words';"), "two words");
    assert_eq!(
        label("edge /e/*: ::a -- ::b as \"say \\\"hi\\\"\";"),
        "say \"hi\""
    );
}
