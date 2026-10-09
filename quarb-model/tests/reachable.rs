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
         rel /e/* -> /d/*[::x = $$::x];\n\
         tree x: /d;\n",
    )
    .unwrap();
    assert_eq!(model.reachable_roots(&["a"]), ["a", "b", "c", "d"]);
    assert_eq!(model.reachable_roots(&["e"]), ["e", "d"]);
    assert_eq!(model.reachable_roots(&[]), ["d"]);
}
