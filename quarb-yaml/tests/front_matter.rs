//! A YAML header at the top of a text (a Markdown note's front
//! matter): what counts as one, what it holds, where the text
//! begins after it.

use quarb::Value;
use quarb_yaml::front_matter;

fn s(t: &str) -> Value {
    Value::Str(t.to_string())
}

#[test]
fn a_header_is_a_mapping_between_two_lines() {
    let (entries, body) = front_matter(
        "---\ntitle: The Iliad\nbooks: 24\nscore: 4.5\ndraft: false\nnote:\n\
         tags: [epic, greek]\nsource:\n  edition: Monro and Allen\n  year: 1920\n---\n\n# Book I\n",
    )
    .unwrap();
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    // in the order written
    assert_eq!(
        keys,
        ["title", "books", "score", "draft", "note", "tags", "source"]
    );
    let get = |k: &str| entries.iter().find(|(n, _)| n == k).unwrap().1.clone();
    // the YAML types kept
    assert_eq!(get("title").to_string(), s("The Iliad").to_string());
    assert!(matches!(get("books"), Value::Int(24)));
    assert!(matches!(get("score"), Value::Float(f) if f == 4.5));
    assert!(matches!(get("draft"), Value::Bool(false)));
    assert!(matches!(get("note"), Value::Null));
    let Value::List(tags) = get("tags") else {
        panic!("tags: no list")
    };
    assert_eq!(tags.len(), 2);
    assert!(matches!(&tags[1], Value::Str(t) if t == "greek"));
    assert_eq!(
        get("source").to_string(),
        r#"%(edition = "Monro and Allen"; year = 1920)"#
    );
    // the text begins after the closing line
    assert_eq!(body, "\n# Book I\n");
}

#[test]
fn the_closing_line_may_be_three_dots_and_lines_may_end_in_crlf() {
    let (entries, body) = front_matter("---\ntitle: The Odyssey\n...\n# Book I\n").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(body, "# Book I\n");
    let (entries, body) =
        front_matter("\u{feff}---\r\ntitle: The Odyssey\r\n---\r\nText.\r\n").unwrap();
    assert_eq!(entries[0].0, "title");
    assert_eq!(body, "Text.\r\n");
    // a header and nothing after it
    let (entries, body) = front_matter("---\ntitle: The Odyssey\n---").unwrap();
    assert_eq!((entries.len(), body), (1, ""));
}

#[test]
fn what_is_no_header_is_left_to_the_reader() {
    for text in [
        // no opening line
        "# Book I\n\n---\ntitle: x\n---\n",
        "\n---\ntitle: x\n---\n",
        // a rule that opens a document
        "---\n\nA paragraph.\n\n---\n\nAnother.\n",
        // never closed
        "---\ntitle: The Iliad\n\n# Book I\n",
        // a list, a sentence, nothing at all: no mapping
        "---\n- epic\n- greek\n---\n",
        "---\nSing, goddess\n---\n",
        "---\n---\n# Book I\n",
        // four dashes, or dashes with text after them
        "----\ntitle: x\n---\n",
        "--- title: x\n---\n",
        // not YAML
        "---\ntitle: [unclosed\n---\n",
        "",
    ] {
        assert!(front_matter(text).is_none(), "{text:?}");
    }
    // a closing line inside a block is no closing line
    let (entries, body) =
        front_matter("---\nabstract: |\n  one\n  ---\n  two\n---\nText.\n").unwrap();
    assert_eq!(entries[0].1.to_string(), s("one\n---\ntwo\n").to_string());
    assert_eq!(body, "Text.\n");
}
