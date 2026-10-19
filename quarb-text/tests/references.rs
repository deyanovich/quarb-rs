//! The reference-list lowering (`?references=STANDARD`): a
//! document whose bibliography is prose paragraphs — numbered as a
//! list, or numbered in the text, or author-year — becomes `bib`
//! nodes with their fields, and the citations in the body become
//! `cit` nodes resolved against them; a paragraph that is no
//! reference stays prose.

use quarb::{AstAdapter, QueryResult};
use quarb_text::{Block, Container, TextModel};

fn values(m: &TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, m).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| m.name(*n).unwrap_or_default()).collect(),
    }
}

fn p(text: &str) -> Block {
    Block::Paragraph { text: text.into() }
}

#[test]
fn a_numbered_list_lowers_to_bib_nodes_and_cits_resolve() {
    let blocks = vec![
        Block::Heading { level: 1, lemma: "On the fall".into() },
        p("Gibbon's account (Gibbon 1776, 45) remains the frame; the Loeb Herodotus [2] and the Quarterly piece (Austen 1816) add the rest. See also [1, 2]."),
        Block::Heading { level: 2, lemma: "References".into() },
        Block::Open { kind: Container::OrderedList { start: 1 }, lemma: None },
        Block::Open { kind: Container::Item, lemma: None },
        Block::Text { text: "Gibbon, Edward. The History of the Decline and Fall of the Roman Empire. Vol. 1. London: Strahan and Cadell, 1776.".into() },
        Block::Close { hypograph: None },
        Block::Open { kind: Container::Item, lemma: None },
        Block::Text { text: "Herodotus. The Histories. Translated by A. D. Godley. Cambridge, MA: Harvard University Press, 1920.".into() },
        Block::Close { hypograph: None },
        Block::Open { kind: Container::Item, lemma: None },
        Block::Text { text: "Austen, Jane. \"On the Reading of Novels.\" The Quarterly Review 12, no. 3 (1816): 45–67.".into() },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        p("This closing line is prose, not a reference."),
    ];
    let mut m = TextModel::build(blocks);
    assert_eq!(values(&m, "//bib @| count"), ["0"]);
    let n = m.lower_references("cmos").unwrap();
    assert_eq!(n, 3);
    assert_eq!(values(&m, "//bib::onym"), ["1", "2", "3"]);
    assert_eq!(
        values(&m, "//bib::author"),
        ["Gibbon, Edward", "Herodotus", "Austen, Jane"]
    );
    assert_eq!(
        values(&m, "//bib::auctor"),
        ["Gibbon, Edward", "Herodotus", "Austen, Jane"]
    );
    assert_eq!(values(&m, "//bib::year"), ["1776", "1920", "1816"]);
    assert_eq!(values(&m, "//bib:::genus"), ["book", "book", "article"]);
    assert_eq!(
        values(&m, "//bib<article>::journal"),
        ["The Quarterly Review"]
    );
    assert_eq!(values(&m, "//cit::target"), ["1", "2", "3", "1", "2"]);
    assert_eq!(values(&m, "//cit[1]::locator"), ["45"]);
    assert_eq!(values(&m, "//cit[!:::resolved] @| count"), ["0"]);
    assert_eq!(values(&m, "//bib[!<-cit] @| count"), ["0"]);
    // The prose stays prose; the entries' text is their projection.
    assert_eq!(values(&m, "//paragraph @| count"), ["2"]);
    assert_eq!(
        values(&m, "//bib[1]::"),
        [
            "Gibbon, Edward. The History of the Decline and Fall of the Roman Empire. Vol. 1. London: Strahan and Cadell, 1776."
        ]
    );
    assert!(m.lower_references("apa").is_err());
}

#[test]
fn numbered_in_the_text_and_a_gost_list() {
    let blocks = vec![
        p("Как показано в [2, с. 45], а также [1]."),
        p(
            "1. Гиббон, Э. История упадка и разрушения Римской империи : в 7 т. / Э. Гиббон. – Москва : Терра, 1997. – Т. 1. – 624 с.",
        ),
        p(
            "2. Иванов, И. И. Геродот и его источники / И. И. Иванов // Вестник древней истории. – 2019. – Т. 79, № 3. – С. 45–52.",
        ),
        p("Заключительная фраза без ссылки."),
    ];
    let mut m = TextModel::build(blocks);
    assert_eq!(m.lower_references("gost").unwrap(), 2);
    assert_eq!(values(&m, "//bib::onym"), ["1", "2"]);
    assert_eq!(values(&m, "//bib:::genus"), ["book", "article"]);
    assert_eq!(values(&m, "//bib[2]::journal"), ["Вестник древней истории"]);
    assert_eq!(values(&m, "//bib[2]::pages"), ["45–52"]);
    assert_eq!(
        values(&m, "//cit | %(t = ::target; l = ::locator)"),
        ["%(t = \"2\"; l = \"с. 45\")", "%(t = \"1\"; l = null)"]
    );
    assert_eq!(values(&m, "//cit[!:::resolved] @| count"), ["0"]);
}
