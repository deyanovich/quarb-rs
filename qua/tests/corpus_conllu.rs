//! CoNLL-U in full (ruling #63): a treebank read as a document
//! (`text:`) and as a corpus (`corpus:`), every column and key on
//! the tokens, the basic and enhanced graphs, empty nodes,
//! multiword ranges, CoNLL-U Plus columns, the sidecar over prose,
//! and the `| conllu` round trip.

use quarb::koine::{Render, render_nodes};
use quarb::{AstAdapter, QueryResult, Value};

/// Two documents: `d1` holds a paragraph of two sentences — one
/// with a multiword range at ids 9–10 (the numeric-compare case)
/// and one with an elided predicate as an empty node — and `d2`
/// one sentence without `# text`, reconstructed from `SpaceAfter`.
const TREEBANK: &str = "# newdoc id = d1
# newpar
# sent_id = d1-1
# text = The old lady looked over them and she couldn't see.
1	The	the	DET	DT	Definite=Def|PronType=Art	3	det	3:det	_
2	old	old	ADJ	JJ	Degree=Pos	3	amod	3:amod	_
3	lady	lady	NOUN	NN	Number=Sing	4	nsubj	4:nsubj|11:nsubj	_
4	looked	look	VERB	VBD	Tense=Past	0	root	0:root	_
5	over	over	ADP	IN	_	6	case	6:case	_
6	them	they	PRON	PRP	Case=Acc|Number=Plur|Person=3	4	obl	4:obl:over	_
7	and	and	CCONJ	CC	_	11	cc	11:cc	_
8	she	she	PRON	PRP	Case=Nom|Gender=Fem|Number=Sing|Person=3	11	nsubj	11:nsubj	NER=B-PERSON
9-10	couldn't	_	_	_	_	_	_	_	SpaceAfter=No
9	could	could	AUX	MD	VerbForm=Fin	11	aux	11:aux	_
10	n't	not	PART	RB	Polarity=Neg	11	advmod	11:advmod	_
11	see	see	VERB	VB	VerbForm=Inf	4	conj	4:conj:and	SpaceAfter=No
12	.	.	PUNCT	.	_	4	punct	4:punct	_

# sent_id = d1-2
# text = Tom read the Iliad and Huck the Odyssey.
1	Tom	Tom	PROPN	NNP	Number=Sing	2	nsubj	2:nsubj	NER=B-PERSON
2	read	read	VERB	VBD	Tense=Past	0	root	0:root	_
3	the	the	DET	DT	Definite=Def|PronType=Art	4	det	4:det	_
4	Iliad	Iliad	PROPN	NNP	Number=Sing	2	obj	2:obj	NER=B-WORK_OF_ART
5	and	and	CCONJ	CC	_	6	cc	6.1:cc	_
6	Huck	Huck	PROPN	NNP	Number=Sing	2	conj	6.1:nsubj	NER=B-PERSON
6.1	read	read	VERB	VBD	Tense=Past	_	_	2:conj:and	_
7	the	the	DET	DT	Definite=Def|PronType=Art	8	det	8:det	_
8	Odyssey	Odyssey	PROPN	NNP	Number=Sing	6	orphan	6.1:obj	NER=B-WORK_OF_ART|SpaceAfter=No
9	.	.	PUNCT	.	_	2	punct	2:punct	_

# newdoc id = d2
# sent_id = d2-1
1	No	no	DET	DT	_	2	det	_	_
2	answer	answer	NOUN	NN	Number=Sing	0	root	_	SpaceAfter=No
3	.	.	PUNCT	.	_	2	punct	_	SpaceAfter=No
";

/// CoNLL-U Plus: the columns reordered and one named column added.
const PLUS: &str = "# global.columns = ID FORM UPOS PARSEME:MWE
# text = He took the bull by the horns.
1	He	PRON	*
2	took	VERB	1:VID
3	the	DET	*
4	bull	NOUN	1
5	by	ADP	1
6	the	DET	*
7	horns	NOUN	1
8	.	PUNCT	*
";

fn values(m: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, m).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns
            .iter()
            .map(|&n| m.property(n, "").map(|v| v.to_string()).unwrap_or_default())
            .collect(),
    }
}

#[test]
fn the_treebank_reads_as_a_document() {
    let m = quarb_text::TextModel::parse_conllu_text(TREEBANK).unwrap();
    // Two documents as sections, a paragraph where `# newpar`
    // said so, sentences under the paragraph or the section.
    assert_eq!(values(&m, "/section::lemma"), ["d1", "d2"]);
    assert_eq!(values(&m, "//paragraph @| count"), ["1"]);
    assert_eq!(values(&m, "//sentence @| count"), ["3"]);
    assert_eq!(values(&m, "/section[1]/paragraph/sentence @| count"), ["2"]);
    assert_eq!(values(&m, "/section[2]/sentence @| count"), ["1"]);
    // `::` is `# text`, else the forms joined per SpaceAfter.
    assert_eq!(
        values(&m, "//sentence[1]::"),
        ["The old lady looked over them and she couldn't see."]
    );
    assert_eq!(values(&m, "//sentence[3]::"), ["No answer."]);
    // The paragraph's prose runs its sentences together.
    assert_eq!(
        values(&m, "//paragraph::"),
        [
            "The old lady looked over them and she couldn't see. Tom read the Iliad and Huck the Odyssey."
        ]
    );
    // `::id` is the sent_id; the comments answer under their keys.
    assert_eq!(values(&m, "//sentence::id"), ["d1-1", "d1-2", "d2-1"]);
    assert_eq!(values(&m, "//sentence[1]::sent_id"), ["d1-1"]);
    assert_eq!(values(&m, "//sentence[1]::Sent_Id"), ["d1-1"]);
    // No tokens in the document reading.
    assert_eq!(values(&m, "//token @| count"), ["0"]);
}

#[test]
fn the_treebank_reads_as_a_corpus() {
    let m = quarb_text::TextModel::parse_conllu_corpus(TREEBANK).unwrap();
    // 12 + 9 words and one empty node.
    assert_eq!(values(&m, "//token @| count"), ["25"]);
    assert_eq!(values(&m, "//token<empty> @| count"), ["1"]);
    assert_eq!(values(&m, "//sentence[2]/token @| count"), ["10"]);
    // Positions count the text's tokens only; the empty node has
    // none.
    assert_eq!(values(&m, "//token[::::n = 24]::"), ["."]);
    assert_eq!(values(&m, "//token[::::n] @| count"), ["24"]);
    assert_eq!(values(&m, "//token<empty>[::::n] @| count"), ["0"]);
    assert_eq!(values(&m, "//token<empty>::id"), ["6.1"]);
    // Sentence text and ordinal still answer on the token.
    assert_eq!(
        values(&m, "//token[::id = \"2\"][::::sentence = 3]::sentence"),
        ["No answer."]
    );
    // Every column.
    assert_eq!(values(&m, "//token[::lower = \"lady\"]::lemma"), ["lady"]);
    assert_eq!(values(&m, "//token[::lower = \"lady\"]::upos"), ["NOUN"]);
    assert_eq!(values(&m, "//token[::lower = \"lady\"]::xpos"), ["NN"]);
    assert_eq!(
        values(&m, "//token[::lower = \"lady\"]::feats"),
        ["Number=Sing"]
    );
    assert_eq!(values(&m, "//token[::lower = \"lady\"]::deprel"), ["nsubj"]);
    assert_eq!(
        values(&m, "//token[::lower = \"lady\"]::deps"),
        ["4:nsubj|11:nsubj"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"she\"]::misc"),
        ["NER=B-PERSON"]
    );
    // Every key, in the annotation's spelling and case-folded.
    assert_eq!(values(&m, "//token[::lower = \"she\"]::Case"), ["Nom"]);
    assert_eq!(values(&m, "//token[::lower = \"she\"]::case"), ["Nom"]);
    assert_eq!(values(&m, "//token[::Gender = \"Fem\"]::"), ["she"]);
    assert_eq!(
        values(&m, "//token[::ner = \"B-PERSON\"]::"),
        ["she", "Tom", "Huck"]
    );
    assert_eq!(
        values(&m, "//token[::NER = \"B-WORK_OF_ART\"]::"),
        ["Iliad", "Odyssey"]
    );
    assert_eq!(values(&m, "//token[::SpaceAfter = \"No\"] @| count"), ["4"]);
    // The multiword range at 9–10: its words share the surface
    // form; the range id is metadata.
    assert_eq!(values(&m, "//token[::mwt]::"), ["could", "n't"]);
    assert_eq!(
        values(&m, "//sentence[1]/token[::id = \"9\"]::mwt"),
        ["couldn't"]
    );
    assert_eq!(values(&m, "//token[::id = \"10\"]::::mwt"), ["9-10"]);
    assert_eq!(values(&m, "//token[::::mwt] @| count"), ["2"]);
}

#[test]
fn the_basic_and_enhanced_graphs() {
    let m = quarb_text::TextModel::parse_conllu_corpus(TREEBANK).unwrap();
    // The basic tree: one head, the relation on the dependent and
    // on the edge.
    assert_eq!(values(&m, "//token[::lower = \"old\"]->head::"), ["lady"]);
    assert_eq!(
        values(
            &m,
            "//token[::deprel = \"amod\"][->head::lower = \"lady\"]::"
        ),
        ["old"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"looked\"]<-head::"),
        ["lady", "them", "see", "."]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"old\"]->head[$-::rel = \"amod\"]::"),
        ["lady"]
    );
    assert_eq!(
        values(
            &m,
            "//token[::lower = \"old\"]->head[$-::rel = \"det\"] @| count"
        ),
        ["0"]
    );
    // The enhanced graph: two heads for the shared subject, the
    // relation as edge data.
    assert_eq!(
        values(&m, "//token[::lower = \"lady\"]->ehead::"),
        ["looked", "see"]
    );
    assert_eq!(
        values(
            &m,
            "//token[::lower = \"lady\"]->ehead[$-::rel = \"nsubj\"]::"
        ),
        ["looked", "see"]
    );
    assert_eq!(
        values(
            &m,
            "//token[::lower = \"see\"]->ehead[$-::rel = \"conj:and\"]::"
        ),
        ["looked"]
    );
    assert_eq!(
        values(
            &m,
            "//token[::lower = \"see\"]->ehead[$-::rel = \"conj\"] @| count"
        ),
        ["0"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"see\"]<-ehead @| count"),
        ["5"]
    );
    // The empty node takes and gives enhanced edges, and no basic
    // ones.
    assert_eq!(values(&m, "//token<empty>->ehead::"), ["read"]);
    assert_eq!(
        values(&m, "//token<empty><-ehead::"),
        ["and", "Huck", "Odyssey"]
    );
    assert_eq!(values(&m, "//token<empty>->head @| count"), ["0"]);
    assert_eq!(values(&m, "//token<empty><-head @| count"), ["0"]);
    // A root carries no head edge.
    assert_eq!(
        values(&m, "//token[::deprel = \"root\"]->head @| count"),
        ["0"]
    );
}

#[test]
fn conllu_plus_columns() {
    let m = quarb_text::TextModel::parse_conllu_corpus(PLUS).unwrap();
    assert_eq!(values(&m, "//token @| count"), ["8"]);
    assert_eq!(values(&m, "//token[::lower = \"took\"]::upos"), ["VERB"]);
    // The reordered ten and the extra column under its declared
    // name, `:` read as `-`, both casings; `*` is a value.
    assert_eq!(
        values(&m, "//token[::lower = \"took\"]::parseme-mwe"),
        ["1:VID"]
    );
    assert_eq!(
        values(&m, "//token[::PARSEME-MWE = \"1\"]::"),
        ["bull", "by", "horns"]
    );
    // Columns the file did not declare are absent.
    assert_eq!(values(&m, "//token[::lemma] @| count"), ["0"]);
}

/// The sidecar over prose: the same annotation aligned to the
/// document's text, tokens under the prose blocks, sentences an
/// annotation on the token.
#[test]
fn the_sidecar_aligns_ranges_past_nine() {
    let prose = "The old lady looked over them and she couldn't see.\n\nNo answer.\n";
    let sidecar: String = TREEBANK
        .split("\n\n")
        .filter(|block| !block.contains("d1-2"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut m = quarb_text::TextModel::parse_plain(prose);
    m.annotate_conllu(&sidecar).unwrap();
    // The sidecar's sentences are the tier (ruling #64): derived,
    // one per block here, the tokens beneath.
    assert_eq!(values(&m, "//sentence @| count"), ["2"]);
    assert_eq!(values(&m, "//sentence<derived> @| count"), ["2"]);
    assert_eq!(values(&m, "//paragraph[1]/sentence/token @| count"), ["12"]);
    assert_eq!(values(&m, "//paragraph[2]/sentence[1]::"), ["No answer."]);
    assert_eq!(values(&m, "//paragraph[2]/sentence/token @| count"), ["3"]);
    assert_eq!(values(&m, "//token[::mwt]::"), ["could", "n't"]);
    assert_eq!(values(&m, "//token[::id = \"10\"]::::mwt"), ["9-10"]);
    assert_eq!(values(&m, "//token[::lower = \"old\"]->head::"), ["lady"]);
    assert_eq!(
        values(
            &m,
            "//token[::lower = \"lady\"]->ehead[$-::rel = \"nsubj\"]::"
        ),
        ["looked", "see"]
    );
    assert_eq!(values(&m, "//token[::case = \"Nom\"]::"), ["she"]);
    assert_eq!(
        values(&m, "//token[::::sentence = 2]::sentence"),
        ["No answer.", "No answer.", "No answer."]
    );
    // A form that is not in the text names the offset.
    let mut bad = quarb_text::TextModel::parse_plain("No reply.\n");
    let err = bad
        .annotate_conllu("1\tNo\t_\t_\t_\t_\t0\troot\t_\t_\n2\tanswer\t_\t_\t_\t_\t1\tdep\t_\t_\n")
        .unwrap_err();
    assert!(err.contains("token 2") && err.contains("offset 3"), "{err}");
}

#[test]
fn the_round_trip() {
    let m = quarb_text::TextModel::parse_conllu_corpus(TREEBANK).unwrap();
    let out = render_nodes(&m, &[m.root()], Render::Conllu);
    // The rendering re-reads to the same tokens and properties.
    let again = quarb_text::TextModel::parse_conllu_corpus(&out).unwrap();
    for q in [
        "//token @| count",
        "//sentence::id",
        "//sentence::",
        "//token[::mwt]::",
        "//token<empty>::id",
        "//token[::lower = \"lady\"]->ehead[$-::rel = \"nsubj\"]::",
        "//token[::ner = \"B-PERSON\"]::",
        "//token::feats",
    ] {
        assert_eq!(values(&again, q), values(&m, q), "{q}");
    }
    // The range line precedes its words; the empty node keeps its
    // id; the sentence comments are written.
    assert!(
        out.contains("# sent_id = d1-1\n# text = The old lady"),
        "{out}"
    );
    assert!(
        out.contains("9-10\tcouldn't\t_\t_\t_\t_\t_\t_\t_\t_\n9\tcould\tcould\tAUX"),
        "{out}"
    );
    assert!(
        out.contains("\n6.1\tread\tread\tVERB\tVBD\tTense=Past\t_\t_\t2:conj:and\t_\n"),
        "{out}"
    );
    // Through the pipeline stage, on one sentence.
    let one = values(&m, "//sentence[3] | conllu");
    assert_eq!(one.len(), 1);
    assert!(
        one[0].starts_with("# sent_id = d2-1\n# text = No answer.\n1\tNo\tno\tDET"),
        "{}",
        one[0]
    );
    // A plain corpus exports `_` columns a tagger can fill.
    let mut plain = quarb_text::TextModel::parse_plain("No answer.\n");
    plain.tokenize();
    let out = render_nodes(&plain, &[plain.root()], Render::Conllu);
    assert_eq!(
        out,
        "# text = No answer.\n1\tNo\t_\t_\t_\t_\t_\t_\t_\t_\n2\tanswer\t_\t_\t_\t_\t_\t_\t_\t_\n3\t.\t_\t_\t_\t_\t_\t_\t_\t_\n"
    );
}

/// CorefUD's bracket notation: a chain across sentences, a
/// single-token mention, and two nested mentions on one token.
const COREF: &str = "# global.Entity = eid-etype-head-other
# text = Tom saw Huck and he waved.
1	Tom	Tom	PROPN	_	_	2	nsubj	_	Entity=(e1-person-1)
2	saw	see	VERB	_	_	0	root	_	_
3	Huck	Huck	PROPN	_	_	2	obj	_	Entity=(e2-person-1)
4	and	and	CCONJ	_	_	6	cc	_	_
5	he	he	PRON	_	_	6	nsubj	_	Entity=(e1-person-1)
6	waved	wave	VERB	_	_	2	conj	_	SpaceAfter=No
7	.	.	PUNCT	_	_	2	punct	_	_

# text = The old lady's spectacles gleamed.
1	The	the	DET	_	_	3	det	_	Entity=(e4-object-5-(e3-person-3-
2	old	old	ADJ	_	_	3	amod	_	_
3	lady	lady	NOUN	_	_	5	nmod:poss	_	Entity=e3)|SpaceAfter=No
4	's	's	PART	_	_	3	case	_	_
5	spectacles	spectacle	NOUN	_	_	6	nsubj	_	Entity=e4)
6	gleamed	gleam	VERB	_	_	0	root	_	SpaceAfter=No
7	.	.	PUNCT	_	_	6	punct	_	_
";

#[test]
fn mentions_from_ner_tags() {
    let m = quarb_text::TextModel::parse_conllu_corpus(TREEBANK).unwrap();
    // Five single-token mentions in document order; the type from
    // the tag, no cluster from an NER-only source.
    assert_eq!(values(&m, "//token[::entity] @| count"), ["5"]);
    assert_eq!(
        values(&m, "//token[::entity = \"PERSON\"]::mention"),
        ["she", "Tom", "Huck"]
    );
    assert_eq!(
        values(&m, "//token[::entity = \"WORK_OF_ART\"]::::mention"),
        ["3", "5"]
    );
    assert_eq!(values(&m, "//token[::::mention = 2]::"), ["Tom"]);
    assert_eq!(values(&m, "//token[::::entity] @| count"), ["0"]);
    // BIO continuation joins tokens within a sentence.
    let mut two = quarb_text::TextModel::parse_plain("Aunt Polly waited.\n");
    two.annotate_conllu(
        "1\tAunt\t_\t_\t_\t_\t3\tnsubj\t_\tner=B-PERSON\n2\tPolly\t_\t_\t_\t_\t1\tflat\t_\tner=I-PERSON\n3\twaited\t_\t_\t_\t_\t0\troot\t_\tner=O|SpaceAfter=No\n4\t.\t_\t_\t_\t_\t3\tpunct\t_\tner=O\n",
    )
    .unwrap();
    assert_eq!(
        values(&two, "//token[::::mention = 1]::"),
        ["Aunt", "Polly"]
    );
    assert_eq!(
        values(&two, "//token[::lower = \"polly\"]::mention"),
        ["Aunt Polly"]
    );
    assert_eq!(
        values(&two, "//token[::lower = \"polly\"]::entity"),
        ["PERSON"]
    );
}

#[test]
fn mentions_from_corefud_brackets() {
    let m = quarb_text::TextModel::parse_conllu_corpus(COREF).unwrap();
    // A chain is the tokens sharing a cluster.
    assert_eq!(values(&m, "//token[::::entity = \"e1\"]::"), ["Tom", "he"]);
    assert_eq!(
        values(&m, "//token[::::entity = \"e1\"]::::mention"),
        ["1", "3"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"huck\"]::entity"),
        ["person"]
    );
    // Nested mentions: the token in both answers lists, outer first
    // as the brackets opened; the mention text follows SpaceAfter.
    assert_eq!(
        values(&m, "//token[::lower = \"lady\"]::mention"),
        ["The old lady's spectacles, The old lady"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"lady\"]::::entity"),
        ["e4, e3"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"spectacles\"]::mention"),
        ["The old lady's spectacles"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"spectacles\"]::entity"),
        ["object"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"gleamed\"][::mention] @| count"),
        ["0"]
    );
    // The raw bracket spec is still there under its own key.
    assert_eq!(values(&m, "//token[::lower = \"lady\"]::Entity"), ["e3)"]);
}

/// A second treebank file for the set reading: one sentence, its
/// own `# newdoc`, a coreference bracket under the shared header.
const SECOND: &str = "# global.Entity = eid-etype-head-other
# newdoc id = d3
# sent_id = d3-1
# text = Huck laughed.
1	Huck	Huck	PROPN	NNP	Number=Sing	2	nsubj	_	Entity=(e7-person-1)
2	laughed	laugh	VERB	VBD	Tense=Past	0	root	_	SpaceAfter=No
3	.	.	PUNCT	.	_	2	punct	_	_
";

#[test]
fn the_set_reading() {
    // A UD treebank's files read as one document: a level-1 section
    // per file, named as given, the files' own newdoc sections
    // beneath; positions and ordinals run across the set.
    let files = [("train.conllu", TREEBANK), ("dev.conllu", SECOND)];
    let m = quarb_text::TextModel::parse_conllu_text_set(&files).unwrap();
    assert_eq!(
        values(&m, "/section::lemma"),
        vec!["train.conllu", "dev.conllu"]
    );
    assert_eq!(
        values(&m, "/section/section::lemma"),
        vec!["d1", "d2", "d3"]
    );
    assert_eq!(values(&m, "//sentence @| count"), vec!["4"]);
    assert_eq!(values(&m, "//token @| count"), vec!["0"]);
    let m = quarb_text::TextModel::parse_conllu_corpus_set(&files).unwrap();
    // 28 tokens: the first file's 25 (its empty node among them)
    // and the second's 3.
    assert_eq!(values(&m, "//token @| count"), vec!["28"]);
    assert_eq!(
        values(&m, "//token[::lower = \"huck\"]::::n"),
        vec!["18", "25"]
    );
    assert_eq!(
        values(&m, "//token[::lower = \"huck\"]::::sentence"),
        vec!["2", "4"]
    );
    // Mentions decode once over the set, under the header the
    // second file declares.
    assert_eq!(values(&m, "//token[::::entity = \"e7\"]::"), vec!["Huck"]);
    assert_eq!(
        values(&m, "//token[::entity = \"PERSON\"] @| count"),
        vec!["3"]
    );
    // An error names the file.
    let bad = [("train.conllu", TREEBANK), ("dev.conllu", "x\ty\n")];
    let err = match quarb_text::TextModel::parse_conllu_text_set(&bad) {
        Ok(_) => panic!("a malformed file must refuse"),
        Err(e) => e,
    };
    assert!(err.starts_with("dev.conllu: "), "{err}");
}

#[test]
fn the_sniff() {
    // A pipe or an extensionless file: CoNLL-U by its first token
    // line, comments and blank lines skipped, a Plus header
    // setting the column count.
    assert!(quarb_text::looks_like_conllu(TREEBANK));
    assert!(quarb_text::looks_like_conllu(PLUS));
    assert!(quarb_text::looks_like_conllu(
        "\n\n# sent_id = 1\n9-10\tcouldn't\t_\t_\t_\t_\t_\t_\t_\t_\n"
    ));
    assert!(!quarb_text::looks_like_conllu(
        "Tom said nothing.\n\nHe went out.\n"
    ));
    assert!(!quarb_text::looks_like_conllu("1\ta\tb\n2\tc\td\n"));
    assert!(!quarb_text::looks_like_conllu("# only a comment\n"));
    assert!(!quarb_text::looks_like_conllu(""));
}

#[test]
fn a_directory_of_treebank_files() {
    // `corpus:DIR` and `text:DIR` through the binary: the files
    // beneath, in path order, nested directories included.
    let dir = std::env::temp_dir().join(format!("quarb-treebank-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ud")).unwrap();
    std::fs::write(dir.join("ud/a-train.conllu"), TREEBANK).unwrap();
    std::fs::write(dir.join("ud/b-dev.conllu"), SECOND).unwrap();
    std::fs::write(dir.join("README.md"), "# not a treebank\n").unwrap();
    let qua = |query: &str, target: &str| -> String {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
            .args([query, target])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "qua {query} {target}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let corpus = format!("corpus:{}", dir.display());
    let text = format!("text:{}", dir.display());
    assert_eq!(qua("//sentence @| count", &corpus), "4\n");
    assert_eq!(qua("//token @| count", &corpus), "28\n");
    assert_eq!(
        qua("/section::lemma", &text),
        "ud/a-train.conllu\nud/b-dev.conllu\n"
    );
    assert_eq!(qua("//token @| count", &text), "0\n");
    // An empty directory is refused, naming the reason.
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .args([
            "//sentence @| count",
            &format!("corpus:{}", empty.display()),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("holds no .conllu file"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
