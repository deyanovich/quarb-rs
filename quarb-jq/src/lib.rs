//! jq importer for Quarb.
//!
//! Translates a jq filter into an equivalent Quarb query over the
//! JSON adapter, following the mapping in the specification's
//! comparative guide. The translatable subset covers jq's
//! navigation, `select` filtering, and array functions; everything
//! else is refused with a [`JqError::Unsupported`] naming the
//! construct, never silently approximated.
//!
//! - Navigation: `.key` (also `."quoted key"` and `.key?`), `.[]`,
//!   `.[n]` (0-based; negative from the end), slices `.[a:b]`, and
//!   pipes between navigation commands (`.a | .b` ≡ `.a.b`).
//! - Filtering: `select(...)` with comparisons over relative paths,
//!   `has("key")`, `and`/`or`, and parenthesized groups.
//! - Functions: `length`, `keys`, `add`, `min`, `max`, `unique`,
//!   `sort`, `reverse`, `first`, `last`, `join(s)`, `map(path)`,
//!   and the array-collect idiom `[EXPR] | fn`.
//! - Not translated, and refused with the hand-written Quarb form
//!   in the refusal: `sort_by`, `min_by`, `max_by`, `group_by`,
//!   `unique_by` (the thread commands `@| sort_by`, `@| min_by`,
//!   `@| max_by`, `@| unique_by`, and a fold by a pushed key).
//!
//! jq emits *values*, so [`translate`] projects the final
//! navigation with `::`; [`translate_nodes`] leaves the result as
//! nodes (locations) instead, which pairs with `jq 'path(...)'`.
//!
//! Known semantic divergences are reported as [`Translation::notes`]
//! rather than errors:
//!
//! - Quarb streams results; jq's `map(...)`, `keys`, and slices box
//!   theirs into arrays.
//! - A missing key is an empty result in Quarb, where jq produces
//!   `null` (and a type error without `?`).
//! - Container results project as empty values (Quarb's JSON
//!   subtree serializes with a following | json); scalar leaves agree.
//! - `select(.key)` truthiness: `0` and `""` are falsy in Quarb but
//!   truthy in jq.

mod export;
pub use export::export;

use quarb::diagnostic as d;
use std::fmt::Write as _;

/// An error translating a jq filter.
#[derive(Debug, thiserror::Error)]
pub enum JqError {
    #[error("jq syntax error at offset {0}: {1}")]
    Syntax(usize, String),
    #[error("unsupported jq construct: {0}")]
    Unsupported(String),
    /// A construct of jq with no Quarb translation: the diagnostic
    /// code of the reason (`SLICE_NOT_TRANSLATED`; ruling 2026-11-17,
    /// one word per reason) and the reason.
    #[error("unsupported jq construct: {1}")]
    NotTranslated(&'static str, String),
    /// The same, for a construct the strict translator refuses and
    /// the lenient one takes: the refusal's tail may name the flag.
    #[error("unsupported jq construct: {1}")]
    Lenient(&'static str, String),
}

impl JqError {
    /// The code and the reason of a refusal that has a word, and
    /// whether `--lenient` translates what was refused.
    pub fn not_translated(&self) -> Option<(&'static str, &str, bool)> {
        match self {
            JqError::NotTranslated(code, reason) => Some((code, reason, false)),
            JqError::Lenient(code, reason) => Some((code, reason, true)),
            _ => None,
        }
    }
}

/// A successful translation: the Quarb query, plus notes on known
/// semantic divergences that apply to it.
#[derive(Debug)]
pub struct Translation {
    pub query: String,
    pub notes: Vec<String>,
    /// What a lenient translation gives up: one line for each place
    /// where the Quarb construct chosen answers differently from
    /// jq. Empty for a strict translation, and for a lenient one of
    /// a filter that needed no leniency.
    pub warnings: Vec<String>,
}

/// Translate a jq filter to a value-projecting Quarb query (the
/// final navigation gets `::`, matching jq's value output).
pub fn translate(jq: &str) -> Result<Translation, JqError> {
    translate_inner(jq, true, false)
}

/// [`translate`] under `--lenient` (the ruling of 2026-11-17): a
/// construct with no exact Quarb counterpart is translated to the
/// nearest one, and each divergence is named in
/// [`Translation::warnings`]. Today that is the by-key functions:
/// `sort_by`, `min_by`, `max_by`, `unique_by`, and `group_by(KEY) |
/// map(length)`. A filter that needs no leniency translates as it
/// does strictly.
pub fn translate_lenient(jq: &str) -> Result<Translation, JqError> {
    translate_inner(jq, true, true)
}

/// [`translate_nodes`] under `--lenient`.
pub fn translate_nodes_lenient(jq: &str) -> Result<Translation, JqError> {
    translate_inner(jq, false, true)
}

/// Translate a jq filter to a node query (no final projection); the
/// results are node locations, comparable with `jq 'path(...)'`.
pub fn translate_nodes(jq: &str) -> Result<Translation, JqError> {
    translate_inner(jq, false, false)
}

fn translate_inner(jq: &str, project: bool, lenient: bool) -> Result<Translation, JqError> {
    let tokens = lex(jq)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        notes: Vec::new(),
        lenient,
        project,
        warnings: Vec::new(),
    };
    let query = parser.program(project)?;
    if parser.pos < parser.tokens.len() {
        return Err(JqError::Syntax(
            parser.tokens[parser.pos].1,
            format!("unexpected trailing '{}'", parser.tokens[parser.pos].0),
        ));
    }
    let mut notes = parser.notes;
    notes.dedup();
    let mut warnings = parser.warnings;
    warnings.dedup();
    Ok(Translation {
        query,
        notes,
        warnings,
    })
}

// ---------------------------------------------------------------- lexer

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Dot,
    DotDot,
    Pipe,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Colon,
    Comma,
    Question,
    Minus,
    Cmp(&'static str),
    And,
    Or,
    Not,
    Number(String),
    Str(String),
    /// An identifier; `true` when immediately followed by `(`.
    Ident(String, bool),
    Var(String),
}

impl std::fmt::Display for Tok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tok::Dot => write!(f, "."),
            Tok::DotDot => write!(f, ".."),
            Tok::Pipe => write!(f, "|"),
            Tok::LBracket => write!(f, "["),
            Tok::RBracket => write!(f, "]"),
            Tok::LParen => write!(f, "("),
            Tok::RParen => write!(f, ")"),
            Tok::Colon => write!(f, ":"),
            Tok::Comma => write!(f, ","),
            Tok::Question => write!(f, "?"),
            Tok::Minus => write!(f, "-"),
            Tok::Cmp(op) => write!(f, "{op}"),
            Tok::And => write!(f, "and"),
            Tok::Or => write!(f, "or"),
            Tok::Not => write!(f, "not"),
            Tok::Number(n) => write!(f, "{n}"),
            Tok::Str(s) => write!(f, "\"{s}\""),
            Tok::Ident(n, _) => write!(f, "{n}"),
            Tok::Var(v) => write!(f, "${v}"),
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lex(input: &str) -> Result<Vec<(Tok, usize)>, JqError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at = i;
        match c {
            c if c.is_whitespace() => i += 1,
            '.' => {
                if chars.get(i + 1) == Some(&'.') {
                    tokens.push((Tok::DotDot, at));
                    i += 2;
                } else {
                    tokens.push((Tok::Dot, at));
                    i += 1;
                }
            }
            '|' => {
                tokens.push((Tok::Pipe, at));
                i += 1;
            }
            '[' => {
                tokens.push((Tok::LBracket, at));
                i += 1;
            }
            ']' => {
                tokens.push((Tok::RBracket, at));
                i += 1;
            }
            '(' => {
                tokens.push((Tok::LParen, at));
                i += 1;
            }
            ')' => {
                tokens.push((Tok::RParen, at));
                i += 1;
            }
            '{' => {
                return Err(JqError::NotTranslated(
                    d::CONSTRUCT_NOT_TRANSLATED,
                    "object construction {...}".into(),
                ));
            }
            ':' => {
                tokens.push((Tok::Colon, at));
                i += 1;
            }
            ',' => {
                tokens.push((Tok::Comma, at));
                i += 1;
            }
            '?' => {
                tokens.push((Tok::Question, at));
                i += 1;
            }
            '-' => {
                tokens.push((Tok::Minus, at));
                i += 1;
            }
            '=' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push((Tok::Cmp("=="), at));
                    i += 2;
                } else {
                    return Err(JqError::NotTranslated(
                        d::CONSTRUCT_NOT_TRANSLATED,
                        "assignment (=)".into(),
                    ));
                }
            }
            '!' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push((Tok::Cmp("!="), at));
                    i += 2;
                } else {
                    return Err(JqError::Syntax(at, "lone '!'".into()));
                }
            }
            '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push((Tok::Cmp("<="), at));
                    i += 2;
                } else {
                    tokens.push((Tok::Cmp("<"), at));
                    i += 1;
                }
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push((Tok::Cmp(">="), at));
                    i += 2;
                } else {
                    tokens.push((Tok::Cmp(">"), at));
                    i += 1;
                }
            }
            '"' => {
                let mut text = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                        match chars.get(i) {
                            Some('"') => text.push('"'),
                            Some('\\') => text.push('\\'),
                            Some('n') => text.push('\n'),
                            Some('t') => text.push('\t'),
                            Some('(') => {
                                return Err(JqError::NotTranslated(
                                    d::CONSTRUCT_NOT_TRANSLATED,
                                    "string interpolation".into(),
                                ));
                            }
                            Some(other) => {
                                return Err(JqError::NotTranslated(
                                    d::LITERAL_NOT_TRANSLATED,
                                    format!("the string escape \\{other}"),
                                ));
                            }
                            None => break,
                        }
                        i += 1;
                    } else {
                        text.push(chars[i]);
                        i += 1;
                    }
                }
                if i == chars.len() {
                    return Err(JqError::Syntax(at, "unterminated string".into()));
                }
                i += 1;
                tokens.push((Tok::Str(text), at));
            }
            '$' => {
                let mut name = String::new();
                i += 1;
                while chars.get(i).is_some_and(|&c| is_ident_char(c)) {
                    name.push(chars[i]);
                    i += 1;
                }
                tokens.push((Tok::Var(name), at));
            }
            c if c.is_ascii_digit() => {
                let mut text = String::new();
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    text.push(chars[i]);
                    i += 1;
                }
                if chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(char::is_ascii_digit)
                {
                    text.push('.');
                    i += 1;
                    while chars.get(i).is_some_and(char::is_ascii_digit) {
                        text.push(chars[i]);
                        i += 1;
                    }
                }
                tokens.push((Tok::Number(text), at));
            }
            c if is_ident_start(c) => {
                let mut text = String::new();
                while chars.get(i).is_some_and(|&c| is_ident_char(c)) {
                    text.push(chars[i]);
                    i += 1;
                }
                let tok = match text.as_str() {
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    "reduce" | "foreach" | "as" | "if" | "then" | "elif" | "else" | "end"
                    | "def" | "try" | "catch" | "import" | "include" | "label" => {
                        return Err(JqError::NotTranslated(
                            d::CONSTRUCT_NOT_TRANSLATED,
                            format!("the {text} construct"),
                        ));
                    }
                    _ => Tok::Ident(text, chars.get(i) == Some(&'(')),
                };
                tokens.push((tok, at));
            }
            other => {
                return Err(JqError::Syntax(at, format!("unexpected '{other}'")));
            }
        }
    }
    Ok(tokens)
}

// --------------------------------------------------------------- parser

/// What the pipeline has produced so far.
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    /// A node set; navigation may continue.
    Nodes,
    /// The element nodes of a slice. jq boxes a slice into a new
    /// array, so the commands that would consume that array (`map`,
    /// array functions, `length`) must not iterate again.
    Elements,
    /// A value stream still awaiting its `::` projection (after
    /// `map(...)` or an array collect).
    PendingValues,
    /// Projected values (after an aggregate); only further
    /// aggregates may follow.
    Values,
    /// The elements of an array as threads in the order a by-key
    /// function gave them (`--lenient`), their nodes kept: an index,
    /// a slice, `[]`, `length`, `first`, `last` or `map(path)` may
    /// follow.
    Ordered,
    /// One element picked out of an ordered array (the several that
    /// tie, for `min_by` and `max_by`), or its elements iterated: a
    /// path may follow, read as a value.
    Picked,
}

/// jq functions that consume an array and reduce or reorder it,
/// with their Quarb aggregate counterparts (jq's `unique` sorts;
/// Quarb's preserves first-seen order, so it composes with `sort`).
fn array_fn(name: &str) -> Option<&'static str> {
    Some(match name {
        "add" => "sum",
        "unique" => "unique @| sort",
        "sort" => "sort",
        "min" => "min",
        "max" => "max",
        "first" => "first",
        "last" => "last",
        "reverse" => "reverse",
        "join" => "join",
        _ => return None,
    })
}

/// jq's functions that sort, pick, group or dedup an array by a
/// key.
fn by_key_fn(name: &str) -> bool {
    matches!(
        name,
        "sort_by" | "min_by" | "max_by" | "group_by" | "unique_by"
    )
}

/// The refusal of a by-key function. None of them is translated
/// (ruled 2026-11-17): the refusal names what a hand-written query
/// says and where its answer differs from jq's, over the filter's
/// own array (`array`, the path so far) and key (`key`, the
/// argument as a Quarb key) when they are plain, with the places
/// named otherwise.
fn by_key_refusal(f: &str, array: Option<&str>, key: Option<&str>, flag: bool) -> String {
    let a = array.unwrap_or("ARRAY");
    let k = key.unwrap_or("KEY");
    let way = match f {
        "sort_by" => format!(
            "Quarb sorts the threads and keeps the nodes: {a}/* @| sort_by({k}), then @| [1] \
             for the first element or @| [..3] for the first three; a null key sorts last \
             there, first in jq"
        ),
        "min_by" => format!(
            "{a}/* @| min_by({k}): every thread that ties for the least key answers, where \
             jq answers the first, and a null key does not compete, where jq takes it for \
             the least"
        ),
        "max_by" => format!(
            "{a}/* @| max_by({k}): every thread that ties for the greatest key answers, \
             where jq answers the last"
        ),
        "group_by" => format!(
            "Quarb pushes the key and folds by it: {a}/* | .k({k}) @|{{.k}} count, or \
             another fold in the place of count; one answer a group, the groups in the \
             order of first appearance, where jq sorts them by the key"
        ),
        _ => format!(
            "{a}/* @| unique_by({k}): the first thread of each key, in the order of first \
             appearance; jq answers them sorted by the key, which @| sort_by({k}) before \
             it gives"
        ),
    };
    let places = match (array, key) {
        (Some(_), Some(_)) => "",
        (None, Some(_)) => "; ARRAY is the path to the array",
        (Some(_), None) => "; KEY is a value read from the element, as ::price",
        (None, None) => {
            "; ARRAY is the path to the array, KEY a value read from the element, as ::price"
        }
    };
    // What `--lenient` does with it, where the refusal is the
    // strict translator's.
    let lenient = match (flag, f) {
        (false, _) => "",
        (true, "group_by") => {
            "; --lenient translates group_by(KEY) | map(length), the size of each group, \
             with a warning where the answer differs from jq's"
        }
        // the refusal's tail names the flag
        (true, _) => "",
    };
    format!("the {f} function (in a hand-written query: {way}{places}){lenient}")
}

struct Parser {
    tokens: Vec<(Tok, usize)>,
    pos: usize,
    notes: Vec<String>,
    /// `--lenient`: translate to the nearest construct and warn.
    lenient: bool,
    /// Whether the result is projected to values (`translate`) or
    /// left as nodes (`translate_nodes`).
    project: bool,
    warnings: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).map(|(t, _)| t.clone());
        self.pos += 1;
        t
    }

    fn expect(&mut self, tok: Tok, what: &str) -> Result<(), JqError> {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            Ok(())
        } else {
            let at = self
                .tokens
                .get(self.pos)
                .map(|(_, at)| *at)
                .unwrap_or_default();
            Err(JqError::Syntax(at, format!("expected {what}")))
        }
    }

    /// The whole program: a pipeline, or the array-collect idiom
    /// `[PIPELINE] | fn | ...`.
    fn program(&mut self, project: bool) -> Result<String, JqError> {
        // `[...]` at the very start is an array collect.
        if self.peek() == Some(&Tok::LBracket) {
            self.pos += 1;
            let mut out = String::new();
            let mut state = self.pipeline(&mut out)?;
            self.expect(Tok::RBracket, "']' to close the array collect")?;
            // The collected values are the projected stream.
            match state {
                State::Nodes | State::Elements | State::PendingValues => out.push_str("::"),
                State::Ordered | State::Picked => out.push_str(" | ::"),
                State::Values => {}
            }
            state = State::Values;
            self.notes.push(
                "array collect: Quarb streams the values; jq boxes them into one array".into(),
            );
            while self.peek() == Some(&Tok::Pipe) {
                self.pos += 1;
                state = self.aggregate_command(&mut out, state)?;
            }
            return Ok(out);
        }

        let mut out = String::new();
        let state = self.pipeline(&mut out)?;
        // A bare `.` navigates nowhere: it is the document root.
        if out.is_empty() && state == State::Nodes {
            out.push('^');
        }
        match state {
            State::Nodes | State::Elements => {
                if project {
                    out.push_str("::");
                    self.notes.push(
                        "value projection: scalar leaves agree with jq; a container \
                         projects as empty here \u{2014} pipe it through | json to \
                         serialize the subtree"
                            .into(),
                    );
                }
            }
            State::PendingValues => out.push_str("::"),
            State::Values => {}
            // The ordered elements are nodes still: their values
            // through a pipe, as a step's `::` would give them.
            State::Ordered | State::Picked => {
                if project {
                    out.push_str(" | ::");
                    self.notes.push(
                        "value projection: scalar leaves agree with jq; a container \
                         projects as empty here \u{2014} pipe it through | json to \
                         serialize the subtree"
                            .into(),
                    );
                }
            }
        }
        Ok(out)
    }

    /// A pipeline of commands, stopping at `]`, `)`, or end.
    fn pipeline(&mut self, out: &mut String) -> Result<State, JqError> {
        let mut state = self.command(out, State::Nodes)?;
        while self.peek() == Some(&Tok::Pipe) {
            self.pos += 1;
            state = self.command(out, state)?;
        }
        Ok(state)
    }

    /// One pipeline command.
    fn command(&mut self, out: &mut String, state: State) -> Result<State, JqError> {
        match self.peek() {
            Some(Tok::Dot) if matches!(state, State::Ordered | State::Picked) => {
                self.ordered_path(out, state)
            }
            Some(Tok::Dot) => {
                if state == State::Elements {
                    return Err(JqError::NotTranslated(
                        d::SLICE_NOT_TRANSLATED,
                        "navigation on a sliced array (iterate the slice with [] first)".into(),
                    ));
                }
                if state != State::Nodes {
                    return Err(JqError::NotTranslated(
                        d::VALUES_NOT_TRANSLATED,
                        "navigation after a value-producing command".into(),
                    ));
                }
                let sliced = self.nav_path(out)?;
                Ok(if sliced {
                    State::Elements
                } else {
                    State::Nodes
                })
            }
            Some(Tok::DotDot) => Err(JqError::NotTranslated(
                d::DESCENT_NOT_TRANSLATED,
                "recursive descent .. (use Quarb's // in a hand-written query)".into(),
            )),
            Some(Tok::Ident(f, true)) if f == "select" => {
                if state == State::Elements {
                    return Err(JqError::NotTranslated(
                        d::SLICE_NOT_TRANSLATED,
                        "select on a sliced array (jq tests the whole array; iterate \
                         the slice with [] first)"
                            .into(),
                    ));
                }
                if state != State::Nodes {
                    return Err(JqError::NotTranslated(
                        d::VALUES_NOT_TRANSLATED,
                        "select after a value-producing command".into(),
                    ));
                }
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after select")?;
                let cond = self.cond_or()?;
                self.expect(Tok::RParen, "')' to close select")?;
                write!(out, "[{cond}]").unwrap();
                Ok(State::Nodes)
            }
            Some(Tok::Ident(f, true)) if f == "map" && state == State::Ordered => {
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after map")?;
                if self.peek() != Some(&Tok::Dot) {
                    return Err(JqError::NotTranslated(
                        d::MAP_NOT_TRANSLATED,
                        "map with a non-path body".into(),
                    ));
                }
                let mut path = String::new();
                if self.nav_path(&mut path)? {
                    return Err(JqError::NotTranslated(
                        d::SLICE_NOT_TRANSLATED,
                        "a slice inside map".into(),
                    ));
                }
                self.expect(Tok::RParen, "')' to close map")?;
                self.value_below(out, &path)?;
                self.notes.push(
                    "map: Quarb streams the mapped values; jq boxes them into one array".into(),
                );
                Ok(State::Values)
            }
            Some(Tok::Ident(f, true)) if f == "map" => {
                if !matches!(state, State::Nodes | State::Elements) {
                    return Err(JqError::NotTranslated(
                        d::VALUES_NOT_TRANSLATED,
                        "map after a value-producing command".into(),
                    ));
                }
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after map")?;
                // A slice already streams its elements; an array node
                // still needs iterating.
                if state == State::Nodes {
                    out.push_str("/*");
                }
                if self.peek() == Some(&Tok::Dot) {
                    if self.nav_path(out)? {
                        return Err(JqError::NotTranslated(
                            d::SLICE_NOT_TRANSLATED,
                            "a slice inside map".into(),
                        ));
                    }
                } else {
                    return Err(JqError::NotTranslated(
                        d::MAP_NOT_TRANSLATED,
                        "map with a non-path body".into(),
                    ));
                }
                self.expect(Tok::RParen, "')' to close map")?;
                self.notes.push(
                    "map: Quarb streams the mapped values; jq boxes them into one array".into(),
                );
                Ok(State::PendingValues)
            }
            Some(Tok::Ident(f, false)) if f == "length" => {
                self.pos += 1;
                match state {
                    State::Nodes => {
                        out.push_str(":::length");
                        Ok(State::Values)
                    }
                    // A slice's length is its element count.
                    State::Elements => {
                        out.push_str(":: @| count");
                        Ok(State::Values)
                    }
                    // Counting a projected stream.
                    State::PendingValues => {
                        out.push_str(":: @| count");
                        Ok(State::Values)
                    }
                    State::Values | State::Ordered => {
                        out.push_str(" @| count");
                        Ok(State::Values)
                    }
                    State::Picked => Err(JqError::NotTranslated(
                        d::VALUES_NOT_TRANSLATED,
                        "length of an element picked after a by-key function".into(),
                    )),
                }
            }
            Some(Tok::Ident(f, false)) if f == "keys" => {
                if state != State::Nodes {
                    return Err(JqError::NotTranslated(
                        d::VALUES_NOT_TRANSLATED,
                        "keys after a value-producing command".into(),
                    ));
                }
                self.pos += 1;
                // Child names, sorted — jq's keys sorts.
                out.push_str("/*:name @| sort");
                self.notes
                    .push("keys: Quarb streams the names; jq boxes them into one array".into());
                Ok(State::Values)
            }
            Some(Tok::Ident(f, calls)) if array_fn(f).is_some() => {
                let f = f.clone();
                let fname = array_fn(&f).unwrap();
                let calls = *calls;
                self.pos += 1;
                let arg = if calls {
                    self.expect(Tok::LParen, "'('")?;
                    let Some(Tok::Str(s)) = self.bump() else {
                        return Err(JqError::NotTranslated(
                            d::FUNCTION_NOT_TRANSLATED,
                            format!("{f} with a non-literal argument"),
                        ));
                    };
                    self.expect(Tok::RParen, "')'")?;
                    Some(s)
                } else {
                    None
                };
                let call = match arg {
                    Some(s) => format!("{fname}({})", quarb_literal(&s)?),
                    None => fname.to_string(),
                };
                match state {
                    // jq array functions consume an array node:
                    // iterate it and aggregate the values. A slice
                    // already streams its elements.
                    State::Nodes => write!(out, "/*:: @| {call}").unwrap(),
                    State::Elements | State::PendingValues => write!(out, ":: @| {call}").unwrap(),
                    State::Values => write!(out, " @| {call}").unwrap(),
                    // An ordered array keeps its nodes: its first or
                    // its last element is a thread index.
                    State::Ordered if f == "first" => {
                        out.push_str(" @| [1]");
                        return Ok(State::Picked);
                    }
                    State::Ordered if f == "last" => {
                        out.push_str(" @| [-1]");
                        return Ok(State::Picked);
                    }
                    State::Ordered | State::Picked => {
                        return Err(JqError::NotTranslated(
                            d::FUNCTION_NOT_TRANSLATED,
                            format!(
                                "{f} after a by-key function (index the ordered array, slice it, \
                             iterate it with [] or take its length)"
                            ),
                        ));
                    }
                }
                Ok(State::Values)
            }
            // The by-key functions are not translated (ruled
            // 2026-11-17): the refusal names the Quarb way, over the
            // filter's own array and key where both are plain.
            Some(Tok::Ident(f, _)) if by_key_fn(f) => {
                let f = f.clone();
                if self.lenient {
                    return self.by_key_lenient(out, state, &f);
                }
                let key = self.plain_path_key();
                let array = (state == State::Nodes).then(|| out.clone());
                Err(JqError::Lenient(
                    d::SORT_NOT_TRANSLATED,
                    by_key_refusal(&f, array.as_deref(), key.as_deref(), true),
                ))
            }
            Some(Tok::Ident(f, _)) => Err(JqError::NotTranslated(
                d::FUNCTION_NOT_TRANSLATED,
                format!("the {f} function"),
            )),
            Some(Tok::Var(v)) => Err(JqError::NotTranslated(
                d::CONSTRUCT_NOT_TRANSLATED,
                format!("the variable ${v}"),
            )),
            Some(Tok::LBracket) => Err(JqError::NotTranslated(
                d::CONSTRUCT_NOT_TRANSLATED,
                "array construction inside a pipeline (only supported at the start)".into(),
            )),
            other => {
                let what = other.map(|t| t.to_string()).unwrap_or_else(|| "end".into());
                Err(JqError::Syntax(
                    0,
                    format!("expected a command, found {what}"),
                ))
            }
        }
    }

    /// A navigation path starting at `.`: `.a.b[0]["k"][]?[1:3]…`,
    /// emitted as Quarb steps. Returns whether the path ends in a
    /// slice (whose elements Quarb streams where jq boxes an array).
    fn nav_path(&mut self, out: &mut String) -> Result<bool, JqError> {
        self.expect(Tok::Dot, "'.'")?;
        self.nav_segments(out)
    }

    /// The segments of a path after its leading `.`.
    fn nav_segments(&mut self, out: &mut String) -> Result<bool, JqError> {
        let mut sliced = false;
        loop {
            // After a slice, only `[]` (which iterates the boxed
            // array back to the same elements — a no-op here) or `?`
            // may follow within the path.
            if sliced {
                match self.peek() {
                    Some(Tok::LBracket)
                        if self.tokens.get(self.pos + 1).map(|(t, _)| t)
                            == Some(&Tok::RBracket) =>
                    {
                        self.pos += 2;
                        sliced = false;
                        continue;
                    }
                    Some(Tok::Ident(_, false) | Tok::Str(_) | Tok::LBracket | Tok::Dot) => {
                        return Err(JqError::NotTranslated(
                            d::SLICE_NOT_TRANSLATED,
                            "indexing into a slice (iterate it with [] first)".into(),
                        ));
                    }
                    _ => break,
                }
            }
            match self.peek() {
                Some(Tok::Ident(name, false)) => {
                    let name = name.clone();
                    self.pos += 1;
                    write!(out, "/{}", quarb_name(&name)).unwrap();
                }
                Some(Tok::Str(key)) => {
                    // `."quoted key"`
                    let key = key.clone();
                    self.pos += 1;
                    write!(out, "/{}", quarb_name(&key)).unwrap();
                }
                Some(Tok::LBracket) => {
                    self.pos += 1;
                    sliced = self.bracket_segment(out)?;
                }
                Some(Tok::Dot) => {
                    self.pos += 1;
                    continue;
                }
                Some(Tok::Question) => {
                    self.pos += 1;
                    self.notes.push(
                        "?: a missing key is an empty result in Quarb (jq yields null, \
                         or a type error without ?)"
                            .into(),
                    );
                }
                _ => break,
            }
        }
        Ok(sliced)
    }

    /// The inside of a `[...]` path segment (after the `[`):
    /// iterate `[]`, index `[n]` / `[-n]`, slice `[a:b]`, or string
    /// key `["k"]`. Returns whether the segment was a slice.
    fn bracket_segment(&mut self, out: &mut String) -> Result<bool, JqError> {
        // `[]` — iterate.
        if self.peek() == Some(&Tok::RBracket) {
            self.pos += 1;
            out.push_str("/*");
            return Ok(false);
        }
        // `["key"]`.
        if let Some(Tok::Str(key)) = self.peek() {
            let key = key.clone();
            self.pos += 1;
            self.expect(Tok::RBracket, "']'")?;
            write!(out, "/{}", quarb_name(&key)).unwrap();
            return Ok(false);
        }
        // Index or slice, either side possibly negative or absent.
        let start = self.signed_number()?;
        if self.peek() == Some(&Tok::RBracket) {
            self.pos += 1;
            let n = start.ok_or_else(|| JqError::Syntax(0, "empty [] index".into()))?;
            if n >= 0 {
                // An array's items are named by their position from
                // one, so jq's `.[0]` is the name step `/1`.
                write!(out, "/{}", n + 1).unwrap();
            } else {
                write!(out, "/*[{n}]").unwrap();
            }
            return Ok(false);
        }
        self.expect(Tok::Colon, "':' or ']' in a bracket segment")?;
        let end = self.signed_number()?;
        self.expect(Tok::RBracket, "']' to close the slice")?;
        // jq slices are 0-based with an exclusive end; Quarb ranges
        // are 1-based inclusive.
        write!(out, "/*[{}]", quarb_range(start, end)).unwrap();
        self.notes.push(
            "slice: Quarb streams the selected elements; jq boxes them into one array".into(),
        );
        Ok(true)
    }

    /// An optional (possibly negative) integer.
    fn signed_number(&mut self) -> Result<Option<i64>, JqError> {
        let neg = if self.peek() == Some(&Tok::Minus) {
            self.pos += 1;
            true
        } else {
            false
        };
        match self.peek() {
            Some(Tok::Number(n)) => {
                let n = n.clone();
                if n.contains('.') {
                    return Err(JqError::NotTranslated(
                        d::LITERAL_NOT_TRANSLATED,
                        format!("the non-integer index {n}"),
                    ));
                }
                self.pos += 1;
                let v: i64 = n
                    .parse()
                    .map_err(|_| JqError::Syntax(0, format!("bad number {n}")))?;
                Ok(Some(if neg { -v } else { v }))
            }
            _ if neg => Err(JqError::Syntax(0, "expected a number after '-'".into())),
            _ => Ok(None),
        }
    }

    // Conditions inside select(...).

    fn cond_or(&mut self) -> Result<String, JqError> {
        let mut left = self.cond_and()?;
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            let right = self.cond_and()?;
            left = format!("{left} or {right}");
        }
        Ok(left)
    }

    fn cond_and(&mut self) -> Result<String, JqError> {
        let mut left = self.cond_cmp()?;
        while self.peek() == Some(&Tok::And) {
            self.pos += 1;
            let right = self.cond_cmp()?;
            left = format!("{left} && {right}");
        }
        Ok(left)
    }

    fn cond_cmp(&mut self) -> Result<String, JqError> {
        let (left, left_bare) = self.cond_primary()?;
        if let Some(Tok::Cmp(op)) = self.peek() {
            let op = *op;
            self.pos += 1;
            let (right, right_bare) = self.cond_primary()?;
            let left = if left_bare { format!("{left}::") } else { left };
            let right = if right_bare {
                format!("{right}::")
            } else {
                right
            };
            let qop = if op == "==" { "=" } else { op };
            return Ok(format!("{left} {qop} {right}"));
        }
        if left_bare {
            // jq truthiness on a bare path.
            self.notes
                .push("select(.key): 0 and \"\" are falsy in Quarb but truthy in jq".into());
            return Ok(format!("{left}::"));
        }
        Ok(left)
    }

    /// A condition operand. The flag marks a bare relative path
    /// (projected with `::` when its value is needed).
    fn cond_primary(&mut self) -> Result<(String, bool), JqError> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let inner = self.cond_or()?;
                self.expect(Tok::RParen, "')' to close the group")?;
                Ok((format!("({inner})"), false))
            }
            Some(Tok::Ident(f, true)) if f == "has" => {
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after has")?;
                let Some(Tok::Str(key)) = self.bump() else {
                    return Err(JqError::NotTranslated(
                        d::FUNCTION_NOT_TRANSLATED,
                        "has with a non-literal key".into(),
                    ));
                };
                self.expect(Tok::RParen, "')' to close has")?;
                Ok((format!("/{}", quarb_name(&key)), false))
            }
            Some(Tok::Ident(f, _)) => Err(JqError::NotTranslated(
                d::FUNCTION_NOT_TRANSLATED,
                format!("the {f} function in a condition"),
            )),
            Some(Tok::Dot) => {
                let mut path = String::new();
                if self.nav_path(&mut path)? {
                    return Err(JqError::NotTranslated(
                        d::SLICE_NOT_TRANSLATED,
                        "a slice inside a condition".into(),
                    ));
                }
                if path.is_empty() {
                    // A bare `.` — the candidate's own value.
                    return Ok(("::".into(), false));
                }
                Ok((path, true))
            }
            Some(Tok::Str(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok((quarb_literal(&s)?, false))
            }
            Some(Tok::Number(n)) => {
                let n = n.clone();
                self.pos += 1;
                Ok((n, false))
            }
            Some(Tok::Minus) => {
                self.pos += 1;
                let Some(Tok::Number(n)) = self.bump() else {
                    return Err(JqError::Syntax(0, "expected a number after '-'".into()));
                };
                Ok((format!("-{n}"), false))
            }
            other => {
                let what = other.map(|t| t.to_string()).unwrap_or_else(|| "end".into());
                Err(JqError::Syntax(
                    0,
                    format!("expected a condition, found {what}"),
                ))
            }
        }
    }

    /// A by-key function under `--lenient`: the nearest Quarb
    /// construct, and a warning for each place its answer differs
    /// from jq's. `sort_by` and `unique_by` leave an ordered array,
    /// `min_by` and `max_by` an element; `group_by` is translated
    /// with the `map(length)` that follows it, an array of arrays
    /// having no thread shape. Anything else is refused as the
    /// strict translator refuses it, without the flag's mention.
    fn by_key_lenient(
        &mut self,
        out: &mut String,
        state: State,
        f: &str,
    ) -> Result<State, JqError> {
        // What the function reads: an array node (iterated here), a
        // slice's elements or an ordered array (streamed already).
        let iter = match state {
            State::Nodes => "/*",
            State::Elements | State::Ordered => "",
            _ => {
                let key = self.plain_path_key();
                return Err(JqError::NotTranslated(
                    d::SORT_NOT_TRANSLATED,
                    by_key_refusal(f, None, key.as_deref(), false),
                ));
            }
        };
        let array = (state == State::Nodes).then(|| out.clone());
        let keys = match self.key_arguments() {
            Some(keys) if f == "sort_by" || keys.len() == 1 => keys,
            _ => {
                return Err(JqError::NotTranslated(
                    d::SORT_NOT_TRANSLATED,
                    by_key_refusal(f, array.as_deref(), None, false),
                ));
            }
        };
        let k = keys.join("; ");
        match f {
            "sort_by" => {
                write!(out, "{iter} @| sort_by({k})").unwrap();
                self.warnings
                    .push("sort_by: a null key sorts last, where jq sorts it first".into());
                Ok(State::Ordered)
            }
            "min_by" => {
                write!(out, "{iter} @| min_by({k})").unwrap();
                self.warnings.push(
                    "min_by: every element that ties for the least key answers, where jq \
                     answers the first; a null key does not compete, where jq takes it for \
                     the least"
                        .into(),
                );
                Ok(State::Picked)
            }
            "max_by" => {
                write!(out, "{iter} @| max_by({k})").unwrap();
                self.warnings.push(
                    "max_by: every element that ties for the greatest key answers, where jq \
                     answers the last"
                        .into(),
                );
                Ok(State::Picked)
            }
            "unique_by" => {
                // jq answers the first element of each key, the keys
                // sorted: the stable sort, then the first per key.
                write!(out, "{iter} @| sort_by({k}) @| unique_by({k})").unwrap();
                self.warnings
                    .push("unique_by: a null key sorts last, where jq sorts it first".into());
                Ok(State::Ordered)
            }
            _ => {
                // `group_by(KEY) | map(length)` alone: the size of
                // each group, the groups in the keys' order.
                let follows = [
                    Tok::Pipe,
                    Tok::Ident("map".into(), true),
                    Tok::LParen,
                    Tok::Ident("length".into(), false),
                    Tok::RParen,
                ];
                let here = self
                    .tokens
                    .get(self.pos..self.pos + follows.len())
                    .is_some_and(|ts| ts.iter().map(|(t, _)| t).eq(follows.iter()));
                if !here {
                    return Err(JqError::NotTranslated(
                        d::SORT_NOT_TRANSLATED,
                        "the group_by function without map(length) after it (an array of \
                         arrays has no thread shape; --lenient translates group_by(KEY) | \
                         map(length), and a hand-written query folds by the pushed key: \
                         ARRAY/* | .k(KEY) @|{.k} COMMAND)"
                            .into(),
                    ));
                }
                self.pos += follows.len();
                write!(out, "{iter} @| sort_by({k}) | .k({k}) @|{{.k}} count").unwrap();
                self.warnings.push(
                    "group_by: the group of a null key comes last, where jq puts it first".into(),
                );
                self.notes.push(
                    "group_by | map(length): Quarb streams the sizes; jq boxes them into \
                     one array"
                        .into(),
                );
                Ok(State::Values)
            }
        }
    }

    /// The arguments of a by-key function as Quarb keys, when each
    /// is one plain path (`sort_by(.a, .b.c)` gives `/a::` and
    /// `/b/c::`), the function's name and parentheses consumed;
    /// `None`, the position and the notes as they were, for anything
    /// else.
    fn key_arguments(&mut self) -> Option<Vec<String>> {
        let (at, notes) = (self.pos, self.notes.len());
        let keys = self.read_key_arguments();
        if keys.is_none() {
            self.pos = at;
            self.notes.truncate(notes);
        }
        keys
    }

    fn read_key_arguments(&mut self) -> Option<Vec<String>> {
        self.pos += 1; // the function's name
        if self.bump()? != Tok::LParen {
            return None;
        }
        let mut keys = Vec::new();
        loop {
            if self.peek() != Some(&Tok::Dot) {
                return None;
            }
            let mut path = String::new();
            if self.nav_path(&mut path).ok()? {
                return None;
            }
            keys.push(format!("{path}::"));
            match self.bump()? {
                Tok::RParen => return Some(keys),
                Tok::Comma => {}
                _ => return None,
            }
        }
    }

    /// A path after a by-key function (`--lenient`). On the ordered
    /// array: an index (the element, a thread index), a slice (the
    /// array still), `[]` (its elements). On an element: a relative
    /// path, read as a value.
    fn ordered_path(&mut self, out: &mut String, mut state: State) -> Result<State, JqError> {
        self.expect(Tok::Dot, "'.'")?;
        while state == State::Ordered {
            match self.peek() {
                Some(Tok::LBracket) => {
                    self.pos += 1;
                    if self.peek() == Some(&Tok::RBracket) {
                        self.pos += 1;
                        state = State::Picked;
                        continue;
                    }
                    let start = self.signed_number()?;
                    if self.peek() == Some(&Tok::RBracket) {
                        self.pos += 1;
                        let n = start.ok_or_else(|| JqError::Syntax(0, "empty [] index".into()))?;
                        // jq counts from zero, a thread index from
                        // one; from the end both count alike
                        let at = if n >= 0 { n + 1 } else { n };
                        write!(out, " @| [{at}]").unwrap();
                        state = State::Picked;
                    } else {
                        self.expect(Tok::Colon, "':' or ']' in a bracket segment")?;
                        let end = self.signed_number()?;
                        self.expect(Tok::RBracket, "']' to close the slice")?;
                        write!(out, " @| [{}]", quarb_range(start, end)).unwrap();
                    }
                }
                Some(Tok::Dot | Tok::Question) => self.pos += 1,
                Some(Tok::Ident(_, false) | Tok::Str(_)) => {
                    return Err(JqError::NotTranslated(
                        d::SORT_NOT_TRANSLATED,
                        "a key on an ordered array (index it, slice it or iterate it with [] \
                         first)"
                            .into(),
                    ));
                }
                // a bare `.`, or the path's end after a slice
                _ => return Ok(State::Ordered),
            }
        }
        let mut path = String::new();
        if self.nav_segments(&mut path)? {
            return Err(JqError::NotTranslated(
                d::SLICE_NOT_TRANSLATED,
                "a slice of an element picked after a by-key function".into(),
            ));
        }
        if path.is_empty() {
            return Ok(State::Picked);
        }
        self.value_below(out, &path)?;
        Ok(State::Values)
    }

    /// The value at `path` below each ordered element: after a
    /// thread command a step would be refused (the topic is live),
    /// so the path is read as a value, parenthesized. A result
    /// asked for as nodes cannot go below the elements.
    fn value_below(&mut self, out: &mut String, path: &str) -> Result<(), JqError> {
        if !self.project {
            return Err(JqError::NotTranslated(
                d::SORT_NOT_TRANSLATED,
                "a path below an element after a by-key function, where nodes are asked for \
                 (the ordered elements are the nodes; a path below them is read as a value)"
                    .into(),
            ));
        }
        if path.is_empty() {
            out.push_str(" | ::");
        } else {
            write!(out, " | ({path}::)").unwrap();
        }
        Ok(())
    }

    /// The argument of a by-key function as the key a hand-written
    /// query names, when it is one plain path (`sort_by(.a.b)` gives
    /// `/a/b::`, `sort_by(.)` gives `::`); `None` for anything else.
    /// Read for the refusal's hint only: the position and the notes
    /// are left as they were.
    fn plain_path_key(&mut self) -> Option<String> {
        let (at, notes) = (self.pos, self.notes.len());
        let key = self.path_argument();
        self.pos = at;
        self.notes.truncate(notes);
        key
    }

    fn path_argument(&mut self) -> Option<String> {
        self.pos += 1; // the function's name
        if self.bump()? != Tok::LParen || self.peek() != Some(&Tok::Dot) {
            return None;
        }
        let mut path = String::new();
        let sliced = self.nav_path(&mut path).ok()?;
        (!sliced && self.peek() == Some(&Tok::RParen)).then(|| format!("{path}::"))
    }

    /// A `@|` aggregate command after an array collect.
    fn aggregate_command(&mut self, out: &mut String, state: State) -> Result<State, JqError> {
        debug_assert_eq!(state, State::Values);
        match self.peek() {
            Some(Tok::Ident(f, false)) if f == "length" => {
                self.pos += 1;
                out.push_str(" @| count");
                Ok(State::Values)
            }
            Some(Tok::Ident(f, calls)) if array_fn(f).is_some() => {
                let f = f.clone();
                let fname = array_fn(&f).unwrap();
                let calls = *calls;
                self.pos += 1;
                if calls {
                    self.expect(Tok::LParen, "'('")?;
                    let Some(Tok::Str(s)) = self.bump() else {
                        return Err(JqError::NotTranslated(
                            d::FUNCTION_NOT_TRANSLATED,
                            format!("{f} with a non-literal argument"),
                        ));
                    };
                    self.expect(Tok::RParen, "')'")?;
                    write!(out, " @| {fname}({})", quarb_literal(&s)?).unwrap();
                } else {
                    write!(out, " @| {fname}").unwrap();
                }
                Ok(State::Values)
            }
            other => {
                let what = other.map(|t| t.to_string()).unwrap_or_else(|| "end".into());
                Err(JqError::NotTranslated(
                    d::CONSTRUCT_NOT_TRANSLATED,
                    format!("'{what}' after an array collect (only array functions follow)"),
                ))
            }
        }
    }
}

/// A jq slice `[start:end]` (0-based, the end exclusive) as a Quarb
/// range (1-based, inclusive).
fn quarb_range(start: Option<i64>, end: Option<i64>) -> String {
    let quarb_start = match start {
        None => String::new(),
        Some(a) if a >= 0 => (a + 1).to_string(),
        Some(a) => a.to_string(),
    };
    let quarb_end = match end {
        None => String::new(),
        Some(0) => "0".to_string(), // empty slice: [start..0]
        Some(b) if b > 0 => b.to_string(),
        Some(b) => (b - 1).to_string(),
    };
    format!("{quarb_start}..{quarb_end}")
}

/// Render a key as a Quarb name segment, quoting when it contains
/// characters outside Quarb's bare-name set.
fn quarb_name(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if bare {
        name.to_string()
    } else {
        format!("'{name}'")
    }
}

/// Render a string literal as a Quarb quoted literal.
fn quarb_literal(s: &str) -> Result<String, JqError> {
    if !s.contains('"') {
        Ok(format!("\"{s}\""))
    } else if !s.contains('\'') {
        Ok(format!("'{s}'"))
    } else {
        Err(JqError::NotTranslated(
            d::LITERAL_NOT_TRANSLATED,
            "a literal containing both quote characters".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(jq: &str) -> String {
        translate(jq).unwrap().query
    }

    fn tn(jq: &str) -> String {
        translate_nodes(jq).unwrap().query
    }

    fn unsupported(jq: &str) -> String {
        match translate(jq) {
            Err(JqError::NotTranslated(_, msg) | JqError::Lenient(_, msg)) => msg,
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn navigation() {
        assert_eq!(t("."), "^::");
        assert_eq!(tn("."), "^");
        assert_eq!(t(".users"), "/users::");
        assert_eq!(tn(".users"), "/users");
        assert_eq!(t(".users[0].name"), "/users/1/name::");
        assert_eq!(t(".a.b.c"), "/a/b/c::");
        assert_eq!(t(".a | .b"), "/a/b::");
        assert_eq!(t(".users[]"), "/users/*::");
        assert_eq!(t(".[]"), "/*::");
        assert_eq!(t(".[2]"), "/3::");
        assert_eq!(t(".[-1]"), "/*[-1]::");
        assert_eq!(t(".users[-2]"), "/users/*[-2]::");
        assert_eq!(t(r#"."weird key""#), "/'weird key'::");
        assert_eq!(t(r#".["k"]"#), "/k::");
        assert_eq!(t(".users[0].name?"), "/users/1/name::");
    }

    #[test]
    fn slices() {
        assert_eq!(t(".items[2:4]"), "/items/*[3..4]::");
        assert_eq!(t(".items[:3]"), "/items/*[..3]::");
        assert_eq!(t(".items[2:]"), "/items/*[3..]::");
        assert_eq!(t(".items[-2:]"), "/items/*[-2..]::");
        assert_eq!(t(".items[0:-1]"), "/items/*[1..-2]::");
        // A zero end is an empty slice; the range must stay valid
        // (`..0` end, never a doubled `....0`).
        assert_eq!(t(".items[:0]"), "/items/*[..0]::");
        assert_eq!(t(".items[0:0]"), "/items/*[1..0]::");
    }

    #[test]
    fn select_conditions() {
        assert_eq!(t(".users[] | select(.age > 30)"), "/users/*[/age:: > 30]::");
        assert_eq!(
            t(r#".users[] | select(.name == "Ada")"#),
            "/users/*[/name:: = \"Ada\"]::"
        );
        assert_eq!(
            t(".users[] | select(.age >= 18 and .age < 65)"),
            "/users/*[/age:: >= 18 && /age:: < 65]::"
        );
        assert_eq!(
            t(r#".users[] | select(has("email"))"#),
            "/users/*[/email]::"
        );
        assert_eq!(t(".users[] | select(.active)"), "/users/*[/active::]::");
        assert_eq!(
            t(".users[] | select(.age > 30) | .name"),
            "/users/*[/age:: > 30]/name::"
        );
    }

    #[test]
    fn functions() {
        assert_eq!(t(".users | length"), "/users:::length");
        assert_eq!(t(".users[0] | keys"), "/users/1/*:name @| sort");
        assert_eq!(t(".nums | add"), "/nums/*:: @| sum");
        assert_eq!(t(".nums | min"), "/nums/*:: @| min");
        assert_eq!(t(".nums | unique"), "/nums/*:: @| unique @| sort");
        assert_eq!(t(".nums | sort | first"), "/nums/*:: @| sort @| first");
        assert_eq!(t(r#".tags | join(", ")"#), "/tags/*:: @| join(\", \")");
        assert_eq!(t(".users | map(.age)"), "/users/*/age::");
        assert_eq!(t(".users | map(.age) | add"), "/users/*/age:: @| sum");
    }

    #[test]
    fn array_collect() {
        assert_eq!(t("[.users[].age] | length"), "/users/*/age:: @| count");
        assert_eq!(t("[.users[].age] | add"), "/users/*/age:: @| sum");
        assert_eq!(
            t("[.users[] | select(.age > 30)] | length"),
            "/users/*[/age:: > 30]:: @| count"
        );
    }

    #[test]
    fn unsupported_constructs() {
        assert!(unsupported("..").contains("recursive descent"));
        assert!(unsupported(".x = 1").contains("assignment"));
        assert!(unsupported("{a: .b}").contains("object construction"));
        assert!(unsupported(".users | to_entries").contains("to_entries"));
        assert!(unsupported("reduce .[] as $x (0; .+$x)").contains("reduce"));
        assert!(unsupported(".a | map(length)").contains("non-path"));
        assert!(unsupported(r#""hi \(.name)""#).contains("interpolation"));
    }

    /// The by-key functions are refused, and the refusal names the
    /// Quarb way over the filter's own array and key (ruled
    /// 2026-11-17). Every query form printed here answers on the
    /// engine (probed on 0.67.0).
    #[test]
    fn by_key_functions_name_the_quarb_way() {
        let m = unsupported(".events | sort_by(.payload.duration_ms) | .[0].id");
        assert!(
            m.starts_with("the sort_by function (in a hand-written query: "),
            "{m}"
        );
        assert!(
            m.contains("/events/* @| sort_by(/payload/duration_ms::), then @| [1]"),
            "{m}"
        );
        // the text ends with the Quarb way; the flag is named by the
        // line's tail, since the lenient translator takes the form
        assert!(
            m.ends_with("a null key sorts last there, first in jq)"),
            "{m}"
        );
        assert!(matches!(
            translate(".events | sort_by(.payload.duration_ms) | .[0].id"),
            Err(JqError::Lenient(code, _)) if code == d::SORT_NOT_TRANSLATED
        ));
        // a refusal the flag does not lift has its own word and
        // variant
        assert!(matches!(
            translate(".."),
            Err(JqError::NotTranslated(code, _)) if code == d::DESCENT_NOT_TRANSLATED
        ));

        let m = unsupported(".events | min_by(.ms) | .id");
        assert!(
            m.contains("/events/* @| min_by(/ms::): every thread"),
            "{m}"
        );
        assert!(m.contains("a null key does not compete"), "{m}");
        let m = unsupported(".events | max_by(.ms)");
        assert!(
            m.contains("/events/* @| max_by(/ms::): every thread"),
            "{m}"
        );
        let m = unsupported(".events | group_by(.kind)");
        assert!(
            m.contains("/events/* | .k(/kind::) @|{.k} count, or another fold"),
            "{m}"
        );
        let m = unsupported(".events | unique_by(.kind)");
        assert!(
            m.contains("/events/* @| unique_by(/kind::): the first"),
            "{m}"
        );
        assert!(m.contains("@| sort_by(/kind::) before it gives"), "{m}");

        // after a select the array is the selected elements' path;
        // the root array; the element itself as the key
        let m = unsupported(".events[] | select(.id > 2) | sort_by(.ms)");
        assert!(
            m.contains("/events/*[/id:: > 2]/* @| sort_by(/ms::)"),
            "{m}"
        );
        assert!(unsupported("sort_by(.)").contains(": /* @| sort_by(::), then"));

        // a key that is no plain path: the place is named
        let m = unsupported(".events | sort_by(.a, .b)");
        assert!(m.contains("/events/* @| sort_by(KEY), then"), "{m}");
        assert!(
            m.ends_with("; KEY is a value read from the element, as ::price)"),
            "{m}"
        );
        // an input that is no node set: the array's place is named
        let m = unsupported(".events | map(.ms) | sort_by(.)");
        assert!(m.contains("ARRAY/* @| sort_by(::), then"), "{m}");
        assert!(m.ends_with("; ARRAY is the path to the array)"), "{m}");
        let m = unsupported(".events | map(.ms) | group_by(length)");
        assert!(
            m.ends_with(
                "ARRAY is the path to the array, KEY a value read from the element, as \
                 ::price); --lenient translates group_by(KEY) | map(length), the size of each \
                 group, with a warning where the answer differs from jq's"
            ),
            "{m}"
        );

        // elsewhere the function is refused as before
        assert_eq!(
            unsupported(".events[] | select(sort_by(.a))"),
            "the sort_by function in a condition"
        );
    }

    fn lenient(jq: &str) -> (String, Vec<String>) {
        let t = translate_lenient(jq).unwrap();
        (t.query, t.warnings)
    }

    fn lenient_unsupported(jq: &str) -> String {
        match translate_lenient(jq) {
            Err(JqError::NotTranslated(_, msg) | JqError::Lenient(_, msg)) => msg,
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    /// `--lenient` (the ruling of 2026-11-17): a by-key function is
    /// translated to the nearest Quarb construct and each divergence
    /// from jq is a warning. Every query here answers on the engine
    /// (probed on 0.67.0 against jq's answers for the same filters).
    #[test]
    fn lenient_translates_the_by_key_functions() {
        // the article's filter: the sort, an index, a key
        let (q, w) = lenient(".events | sort_by(.payload.duration_ms) | .[0].id");
        assert_eq!(
            q,
            "/events/* @| sort_by(/payload/duration_ms::) @| [1] | (/id::)"
        );
        assert_eq!(
            w,
            ["sort_by: a null key sorts last, where jq sorts it first"]
        );
        // several keys; the whole ordered array, its values
        let (q, _) = lenient(".events | sort_by(.kind, .id)");
        assert_eq!(q, "/events/* @| sort_by(/kind::; /id::) | ::");
        // nodes asked for: the ordered elements themselves
        assert_eq!(
            translate_nodes_lenient(".events | sort_by(.id) | .[-1]")
                .unwrap()
                .query,
            "/events/* @| sort_by(/id::) @| [-1]"
        );
        // slices keep the array; `[]` and map read below each element
        assert_eq!(
            lenient(".events | sort_by(.id) | .[1:3] | .[].kind").0,
            "/events/* @| sort_by(/id::) @| [2..3] | (/kind::)"
        );
        assert_eq!(
            lenient(".events | sort_by(.id) | .[:3] | map(.kind)").0,
            "/events/* @| sort_by(/id::) @| [..3] | (/kind::)"
        );
        assert_eq!(
            lenient(".events | sort_by(.id) | length").0,
            "/events/* @| sort_by(/id::) @| count"
        );
        assert_eq!(
            lenient(".events | sort_by(.id) | first | .kind").0,
            "/events/* @| sort_by(/id::) @| [1] | (/kind::)"
        );
        assert_eq!(
            lenient(".events | sort_by(.id) | last").0,
            "/events/* @| sort_by(/id::) @| [-1] | ::"
        );
        // a slice's elements and the root array are read as they stand
        assert_eq!(
            lenient(".events[1:4] | sort_by(.id) | .[0].id").0,
            "/events/*[2..4] @| sort_by(/id::) @| [1] | (/id::)"
        );
        assert_eq!(lenient("sort_by(.)").0, "/* @| sort_by(::) | ::");

        // min_by, max_by: the thread commands of the same names
        let (q, w) = lenient(".events | min_by(.payload.duration_ms) | .id");
        assert_eq!(q, "/events/* @| min_by(/payload/duration_ms::) | (/id::)");
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("min_by: every element that ties"), "{w:?}");
        assert!(w[0].contains("a null key does not compete"), "{w:?}");
        let (q, w) = lenient(".events | max_by(.id)");
        assert_eq!(q, "/events/* @| max_by(/id::) | ::");
        assert!(w[0].starts_with("max_by: every element that ties"), "{w:?}");

        // unique_by: jq's order is the keys', the first of each
        let (q, w) = lenient(".events | unique_by(.kind) | map(.id)");
        assert_eq!(
            q,
            "/events/* @| sort_by(/kind::) @| unique_by(/kind::) | (/id::)"
        );
        assert_eq!(
            w,
            ["unique_by: a null key sorts last, where jq sorts it first"]
        );

        // group_by with the sizes of its groups
        let (q, w) = lenient(".events | group_by(.kind) | map(length)");
        assert_eq!(
            q,
            "/events/* @| sort_by(/kind::) | .k(/kind::) @|{.k} count"
        );
        assert_eq!(
            w,
            ["group_by: the group of a null key comes last, where jq puts it first"]
        );

        // a filter that needs no leniency is the strict translation,
        // and warns of nothing
        let (q, w) = lenient(".users[] | select(.age > 30) | .name");
        assert_eq!(q, t(".users[] | select(.age > 30) | .name"));
        assert!(w.is_empty());
        assert!(translate(".users | length").unwrap().warnings.is_empty());
    }

    /// What stays refused under `--lenient`, each by name.
    #[test]
    fn lenient_refuses_what_has_no_near_construct() {
        // a bare group_by: an array of arrays
        let m = lenient_unsupported(".events | group_by(.kind)");
        assert!(
            m.starts_with("the group_by function without map(length) after it"),
            "{m}"
        );
        // a key that is no plain path; several keys where one is taken
        let m = lenient_unsupported(".events | sort_by(-.id)");
        assert!(m.contains("/events/* @| sort_by(KEY)"), "{m}");
        assert!(!m.contains("--lenient"), "{m}");
        let m = lenient_unsupported(".events | min_by(.a, .b)");
        assert!(m.contains("/events/* @| min_by(KEY)"), "{m}");
        // an input that is no node set
        let m = lenient_unsupported(".events | map(.ms) | sort_by(.)");
        assert!(m.contains("ARRAY/* @| sort_by(::)"), "{m}");
        // a key on the ordered array; a function over it
        assert_eq!(
            lenient_unsupported(".events | sort_by(.id) | .kind"),
            "a key on an ordered array (index it, slice it or iterate it with [] first)"
        );
        assert!(
            lenient_unsupported(".events | sort_by(.id) | add")
                .starts_with("add after a by-key function"),
        );
        assert_eq!(
            lenient_unsupported(".events | min_by(.id) | length"),
            "length of an element picked after a by-key function"
        );
        // nodes asked for, and a path below the element
        match translate_nodes_lenient(".events | sort_by(.id) | .[0].kind") {
            Err(JqError::NotTranslated(_, m)) => {
                assert!(m.starts_with("a path below an element"), "{m}")
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn notes_flag_divergences() {
        assert!(
            translate(".a?")
                .unwrap()
                .notes
                .iter()
                .any(|n| n.contains("missing key"))
        );
        assert!(
            translate(".a | map(.b)")
                .unwrap()
                .notes
                .iter()
                .any(|n| n.contains("boxes"))
        );
        assert!(
            translate(".users[] | select(.active)")
                .unwrap()
                .notes
                .iter()
                .any(|n| n.contains("falsy"))
        );
        // node mode adds no projection note
        assert!(translate_nodes(".users").unwrap().notes.is_empty());
    }
}
