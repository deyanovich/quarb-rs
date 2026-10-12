//! `morph:` — pymorphy3, the Russian morphological analyser, as a
//! Quarb command library: the pilot of a library that wraps another
//! language's library through an embedded interpreter (spec: Command
//! Libraries). The commands are declared here, in Rust, with the
//! synopsis the engine checks before the run; a page of Python glue
//! (compiled once, the analyser built once) adapts pymorphy3's
//! objects to the call shape, and the values cross through
//! `quarb-pyvalue`, the same mapping `quarb-py` uses the other way.
//!
//! What the pilot proves of the contract: the synopsis per command
//! (operands, options with short and long names, pipe, input slot,
//! return); the required prefix; no mount (the library is pure and
//! linked); the input slot (first positional unless `$_` places it;
//! after `@|` the stream is the value and the call runs once over
//! it); options as keyword arguments; value conversion both ways;
//! failure as null after `|` and a refusal after `@|`, the module's
//! stdout redirected to stderr during a call; a run-time requirement
//! (the `pymorphy3` package and `pymorphy3-dicts-ru`) refused by name
//! before the run where it is not importable.
//!
//! What a node becomes: the engine hands a node to a library as the
//! record of its name, its text (`::`) and its properties, a
//! snapshot; this library reads a word from the `text` field.

use quarb::Value;
use quarb::library::{
    CommandLibrary, InputSlot, LibCall, LibFailure, OperandSpec, OptionSpec, Pipe, Synopsis,
};
use quarb_pyvalue::PyGlue;

/// The glue: pymorphy3 in the call shape. One analyser per
/// language, built on first use; a list input (the stream after
/// `@|`) maps the command over its items.
const GLUE: &str = r#"
import contextlib, sys
import pymorphy3

_analysers = {}

def _morph(lang):
    lang = lang or "ru"
    if lang not in _analysers:
        _analysers[lang] = pymorphy3.MorphAnalyzer(lang=lang)
    return _analysers[lang]

def _word(x):
    if isinstance(x, dict):
        x = x.get("text") or x.get("name") or ""
    return "" if x is None else str(x)

def _best(word, lang):
    parses = _morph(lang).parse(_word(word))
    return parses[0] if parses else None

def lemma(word, lang="ru"):
    p = _best(word, lang)
    return None if p is None else p.normal_form

def tag(word, lang="ru"):
    p = _best(word, lang)
    return None if p is None else str(p.tag)

def pos(word, lang="ru"):
    p = _best(word, lang)
    return None if p is None else p.tag.POS

def parse(word, n=None, lang="ru"):
    parses = _morph(lang).parse(_word(word))
    if n is not None:
        parses = parses[: int(n)]
    return [
        {
            "lemma": p.normal_form,
            "tag": str(p.tag),
            "pos": p.tag.POS,
            "score": round(p.score, 6),
        }
        for p in parses
    ]

_GRAMMEMES = ("case", "number", "gender", "person", "tense", "mood", "voice", "aspect", "animacy", "transitivity")

def inflect(word, lang="ru", **grammemes):
    p = _best(word, lang)
    if p is None:
        return None
    wanted = {str(v) for k, v in grammemes.items() if k in _GRAMMEMES and v is not None}
    if not wanted:
        return p.word
    q = p.inflect(wanted)
    return None if q is None else q.word

def _call(name, args, kwargs):
    f = globals()[name]
    with contextlib.redirect_stdout(sys.stderr):
        if args and isinstance(args[0], list):
            head, rest = args[0], args[1:]
            return [f(item, *rest, **kwargs) for item in head]
        return f(*args, **kwargs)
"#;

/// The library.
pub struct Morph;

const LANG: OptionSpec = OptionSpec {
    short: Some("-l"),
    long: "lang",
    takes_value: true,
    doc: "the dictionary: ru (the default), uk where its dictionaries are installed",
};

const WORD: OperandSpec = OperandSpec {
    name: "word",
    doc: "the word form (the input; a node by its text)",
    values: &[],
    required: true,
};

const GRAMMEME_OPTIONS: &[OptionSpec] = &[
    LANG,
    OptionSpec {
        short: None,
        long: "case",
        takes_value: true,
        doc: "nomn gent datv accs ablt loct voct gen2 acc2 loc2",
    },
    OptionSpec {
        short: None,
        long: "number",
        takes_value: true,
        doc: "sing plur",
    },
    OptionSpec {
        short: None,
        long: "gender",
        takes_value: true,
        doc: "masc femn neut",
    },
    OptionSpec {
        short: None,
        long: "person",
        takes_value: true,
        doc: "1per 2per 3per",
    },
    OptionSpec {
        short: None,
        long: "tense",
        takes_value: true,
        doc: "pres past futr",
    },
    OptionSpec {
        short: None,
        long: "mood",
        takes_value: true,
        doc: "indc impr",
    },
    OptionSpec {
        short: None,
        long: "voice",
        takes_value: true,
        doc: "actv pssv",
    },
    OptionSpec {
        short: None,
        long: "aspect",
        takes_value: true,
        doc: "perf impf",
    },
    OptionSpec {
        short: None,
        long: "animacy",
        takes_value: true,
        doc: "anim inan",
    },
    OptionSpec {
        short: None,
        long: "transitivity",
        takes_value: true,
        doc: "tran intr",
    },
];

const COMMANDS: &[Synopsis] = &[
    Synopsis {
        name: "lemma",
        doc: "the normal form of the word's most likely parse",
        pipes: Pipe::Both,
        operands: &[WORD],
        options: &[LANG],
        input: InputSlot::Operand(0),
        returns: "string (null where the word has no parse)",
    },
    Synopsis {
        name: "tag",
        doc: "the grammemes of the most likely parse, as OpenCorpora writes them",
        pipes: Pipe::Both,
        operands: &[WORD],
        options: &[LANG],
        input: InputSlot::Operand(0),
        returns: "string",
    },
    Synopsis {
        name: "pos",
        doc: "the part of speech of the most likely parse (NOUN, VERB, ADJF, …)",
        pipes: Pipe::Both,
        operands: &[WORD],
        options: &[LANG],
        input: InputSlot::Operand(0),
        returns: "string",
    },
    Synopsis {
        name: "parse",
        doc: "every parse of the word, most likely first: lemma, tag, pos, score",
        pipes: Pipe::Both,
        operands: &[WORD],
        options: &[
            OptionSpec {
                short: Some("-n"),
                long: "n",
                takes_value: true,
                doc: "at most this many parses (all by default; -n=1 the best)",
            },
            LANG,
        ],
        input: InputSlot::Operand(0),
        returns: "list of records %(lemma; tag; pos; score)",
    },
    Synopsis {
        name: "inflect",
        doc: "the word's most likely parse inflected to the grammemes given",
        pipes: Pipe::Both,
        operands: &[WORD],
        options: GRAMMEME_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "string (null where no form exists)",
    },
];

static PY: PyGlue = PyGlue::new(GLUE, "quarb_morph.py", "quarb_morph");

impl CommandLibrary for Morph {
    fn prefix(&self) -> &str {
        "morph"
    }

    fn commands(&self) -> &[Synopsis] {
        COMMANDS
    }

    fn requirements(&self) -> &[&str] {
        &["pymorphy3", "pymorphy3-dicts-ru"]
    }

    fn available(&self) -> Result<(), String> {
        quarb_pyvalue::importable(
            &["pymorphy3", "pymorphy3_dicts_ru"],
            "pymorphy3 pymorphy3-dicts-ru",
        )
    }

    fn call(&self, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        let Some(syn) = self.synopsis(name) else {
            return Err(LibFailure::Unknown(format!(
                "library 'morph' has no command '{name}'"
            )));
        };
        PY.call(syn, name, call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(name: &str, input: Value, options: Vec<(&str, Value)>) -> Value {
        Morph
            .call(
                name,
                LibCall {
                    agg: false,
                    input: Some(input),
                    operands: vec![],
                    options: options
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), Some(v)))
                        .collect(),
                },
            )
            .unwrap()
    }

    #[test]
    fn lemma_tag_and_parse_read_a_russian_form() {
        if Morph.available().is_err() {
            eprintln!("pymorphy3 not installed; skipping");
            return;
        }
        assert_eq!(
            plain("lemma", Value::Str("книги".into()), vec![]),
            Value::Str("книга".into())
        );
        let Value::Str(tag) = plain("tag", Value::Str("книги".into()), vec![]) else {
            panic!("tag")
        };
        assert!(tag.starts_with("NOUN"), "{tag}");
        let Value::List(parses) = plain("parse", Value::Str("стали".into()), vec![]) else {
            panic!("parse")
        };
        assert!(parses.len() >= 2, "{parses:?}");
        let best = plain(
            "parse",
            Value::Str("стали".into()),
            vec![("n", Value::Int(1))],
        );
        assert_eq!(best.into_items().map(|v| v.len()), Some(1));
        assert_eq!(
            plain(
                "inflect",
                Value::Str("книга".into()),
                vec![
                    ("case", Value::Str("gent".into())),
                    ("number", Value::Str("plur".into()))
                ]
            ),
            Value::Str("книг".into())
        );
    }

    #[test]
    fn the_stream_maps_once_after_the_aggregation_pipe() {
        if Morph.available().is_err() {
            return;
        }
        let out = Morph
            .call(
                "lemma",
                LibCall {
                    agg: true,
                    input: Some(Value::list(vec![
                        Value::Str("книги".into()),
                        Value::Str("стали".into()),
                    ])),
                    operands: vec![],
                    options: vec![],
                },
            )
            .unwrap();
        assert_eq!(
            out,
            Value::list(vec![Value::Str("книга".into()), Value::Str("стать".into())])
        );
    }
}
