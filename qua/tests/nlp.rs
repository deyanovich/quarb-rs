//! NLP functions, tier 1 (lang design/nlp-stdlib.md): tokens,
//! casefold, deaccent, stem, ttr, hapax, entropy over values,
//! skipgrams, dice / logdice / tscore, jaro_winkler, jaccard.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"t": "The cat sat, the cat ran; 3 cats.",
            "de": "Straße GROSS", "el": "ὀδυσσεύς", "ru": "ёлка подъём",
            "fr": "élève", "words": ["the", "cat", "sat", "the", "cat", "ran"],
            "counts": [4, 4]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

#[test]
fn tokens_are_the_corpus_tiers_segments() {
    // One value per token, as `words` spreads: the aggregates follow.
    assert_eq!(values("/t | :: | tokens @| count"), ["11"]);
    assert_eq!(
        values("/t | :: | tokens @| [..4]"),
        ["The", "cat", "sat", ","]
    );
    assert_eq!(values("/t | :: | tokens(word) @| count"), ["7"]);
    assert_eq!(values("/t | :: | tokens(number)"), ["3"]);
    assert_eq!(values("/t | :: | tokens(punct) @| count"), ["3"]);
    assert_eq!(
        values("/t | :: | tokens(word) | casefold @| ttr | round(3)"),
        ["0.714"]
    );
}

#[test]
fn folding_and_stripping() {
    assert_eq!(values("/de | :: | casefold"), ["strasse gross"]);
    assert_eq!(values("/el | :: | deaccent(grc)"), ["οδυσσευς"]);
    assert_eq!(values("/fr | :: | deaccent(fr)"), ["eleve"]);
    // the bare form is the blunt one, every mark off
    assert_eq!(values("/fr | :: | deaccent"), ["eleve"]);
    assert_eq!(values("/ru | :: | deaccent"), ["елка подъем"]);
    // ё is a letter: the stress mark goes, the diaeresis stays
    assert_eq!(values("/ru | :: | deaccent(ru)"), ["ёлка подъём"]);
    assert_eq!(values("/ru | :: | strip"), ["елка подъем"]);
    // the hyphenated table name is a bare word
    assert_eq!(values("= 'Љубљана' | translit(sr-latn)"), ["Ljubljana"]);
    assert_eq!(values("= 'Ljubljana' | translit('sr-cyrl')"), ["Љубљана"]);
}

#[test]
fn stemming_by_language() {
    assert_eq!(values("/words/* | :: @| first | stem(en)"), ["the"]);
    assert_eq!(values("= \"running\" | stem(en)"), ["run"]);
    // Word by word, the rest of the text untouched.
    assert_eq!(
        values("= \"running quickly, cats!\" | stem(en)"),
        ["run quick, cat!"]
    );
    assert_eq!(values("= \"подъёмами\" | stem(ru)"), ["подъём"]);
    // A language the stemmer does not ship is refused, naming them.
    let err = quarb::run("= \"word\" | stem(xx)", &doc())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("stem takes a language") && err.contains("ru"),
        "{err}"
    );
}

#[test]
fn vocabulary_statistics() {
    assert_eq!(values("/words/* | :: @| ttr | round(4)"), ["0.6667"]);
    assert_eq!(values("/words/* | :: @| hapax"), ["sat", "ran"]);
    // Over values: the distribution's entropy; over counts: as before.
    assert_eq!(values("/words/* | :: @| entropy | round(4)"), ["1.9183"]);
    assert_eq!(values("/counts/* | :: @| entropy"), ["1"]);
}

#[test]
fn skipgrams_and_jaccard() {
    assert_eq!(
        values("/words/*[..4] | :: @| skipgrams(2; 1)"),
        ["the, cat", "the, sat", "cat, sat", "cat, the", "sat, the"]
    );
    assert_eq!(values("/words/* | :: @| skipgrams(3; 0) @| count"), ["4"]);
    assert_eq!(
        values("/words/* | :: @| jaccard(@(\"the\"; \"cat\"; \"dog\"))"),
        ["0.4"]
    );
    // the other sample is one list operand; a flat spelling is a
    // surplus of operands (the adjacency rule)
    let e = quarb::run("/words/* | :: @| jaccard(\"the\"; \"cat\"; \"dog\")", &doc())
        .unwrap_err()
        .to_string();
    assert!(e.contains("'jaccard' takes 1 operand at most, 3 given"), "{e}");
}

#[test]
fn collocation_measures_and_similarity() {
    assert_eq!(values("= 10 | dice(20; 30)"), ["0.4"]);
    assert_eq!(
        values("= 10 | logdice(20; 30)"),
        values("= 10 | log_dice(20; 30)")
    );
    assert_eq!(
        values("= 10 | tscore(20; 30; 1000)"),
        values("= 10 | t_score(20; 30; 1000)")
    );
    assert_eq!(
        values("= \"martha\" | jaro_winkler(\"marhta\") | round(4)"),
        ["0.9611"]
    );
}
