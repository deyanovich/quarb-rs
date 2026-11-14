//! A YAML header on the plain Markdown mount: the document's head,
//! as a page's is, and no part of the body.

use quarb::QueryResult;

fn values(text: &str, q: &str) -> Vec<String> {
    let a = quarb_markdown::parse(text);
    match quarb::run(q, &a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
    }
}

const NOTE: &str = "---\n\
title: The Iliad & its \"wrath\"\n\
author: Homer\n\
tags: [epic, greek]\n\
books: 24\n\
edition:\n  editor: Monro <and> Allen\n  year: 1920\n\
draft: false\n\
note:\n\
---\n\n\
# Book I\n\nSing, goddess.\n";

#[test]
fn the_header_is_the_head() {
    // a title element, text escaped
    assert_eq!(
        values(NOTE, "/html/head/title::"),
        [r#"The Iliad & its "wrath""#]
    );
    // a meta element per key, per item of a list, per key of a
    // nested mapping; a null declares nothing
    assert_eq!(
        values(NOTE, "/html/head/meta | (::name ++ '=' ++ ::content)"),
        [
            "author=Homer",
            "tags=epic",
            "tags=greek",
            "books=24",
            "edition.editor=Monro <and> Allen",
            "edition.year=1920",
            "draft=false",
        ]
    );
    assert_eq!(
        values(NOTE, "/html/head/meta[::name = 'tags'] | ::content"),
        ["epic", "greek"]
    );
}

#[test]
fn the_header_is_no_part_of_the_body() {
    assert_eq!(values(NOTE, "/html/body/* | :name"), ["h1", "p"]);
    assert_eq!(values(NOTE, "//h1::"), ["Book I"]);
    // it read as a rule, a paragraph, and a heading
    assert_eq!(values(NOTE, "//hr @| count"), ["0"]);
    assert_eq!(values(NOTE, "//h2 @| count"), ["0"]);
}

#[test]
fn a_file_with_no_header_reads_as_ever() {
    let plain = "# Book I\n\n---\n\nSing, goddess.\n";
    assert_eq!(values(plain, "/html/head/* @| count"), ["0"]);
    assert_eq!(values(plain, "/html/body/* | :name"), ["h1", "hr", "p"]);
    // a rule that opens the file is a rule
    let rule = "---\n\nA paragraph.\n";
    assert_eq!(values(rule, "/html/body/* | :name"), ["hr", "p"]);
}
