//! The canonical text is a fixpoint over the published corpus: every
//! query of the recipes and articles that parses on its own (no
//! defs file, no model) unparses to text that parses back and
//! unparses to itself.

use quarb::Defs;

#[test]
fn the_corpus_unparses_to_a_fixpoint() {
    let corpus = include_str!("corpus/queries.txt");
    let defs = Defs::default();
    let (mut parsed, mut drift) = (0usize, Vec::new());
    for q in corpus.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(once) = quarb::expand(q, &defs) else {
            continue;
        };
        parsed += 1;
        match quarb::expand(&once, &defs) {
            Ok(twice) if twice == once => {}
            Ok(twice) => drift.push(format!("{q}\n  once:  {once}\n  twice: {twice}")),
            Err(e) => drift.push(format!("{q}\n  once:  {once}\n  reparse: {e}")),
        }
    }
    let total = corpus.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(
        parsed * 10 >= total * 7,
        "only {parsed} of {total} corpus queries parse standalone"
    );
    assert!(
        drift.is_empty(),
        "{} of {parsed} parsed queries are not a fixpoint:\n{}",
        drift.len(),
        drift.join("\n")
    );
}
