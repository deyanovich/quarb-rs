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
//!   pipes between navigation stages (`.a | .b` ≡ `.a.b`).
//! - Filtering: `select(...)` with comparisons over relative paths,
//!   `has("key")`, `and`/`or`, and parenthesized groups.
//! - Functions: `length`, `keys`, `add`, `min`, `max`, `unique`,
//!   `sort`, `reverse`, `first`, `last`, `join(s)`, `map(path)`,
//!   and the array-collect idiom `[EXPR] | fn`.
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

use std::fmt::Write as _;

/// An error translating a jq filter.
#[derive(Debug, thiserror::Error)]
pub enum JqError {
    #[error("jq syntax error at offset {0}: {1}")]
    Syntax(usize, String),
    #[error("unsupported jq construct: {0}")]
    Unsupported(String),
}

/// A successful translation: the Quarb query, plus notes on known
/// semantic divergences that apply to it.
#[derive(Debug)]
pub struct Translation {
    pub query: String,
    pub notes: Vec<String>,
}

/// Translate a jq filter to a value-projecting Quarb query (the
/// final navigation gets `::`, matching jq's value output).
pub fn translate(jq: &str) -> Result<Translation, JqError> {
    translate_inner(jq, true)
}

/// Translate a jq filter to a node query (no final projection); the
/// results are node locations, comparable with `jq 'path(...)'`.
pub fn translate_nodes(jq: &str) -> Result<Translation, JqError> {
    translate_inner(jq, false)
}

fn translate_inner(jq: &str, project: bool) -> Result<Translation, JqError> {
    let tokens = lex(jq)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        notes: Vec::new(),
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
    Ok(Translation { query, notes })
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
                return Err(JqError::Unsupported("object construction {...}".into()));
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
                    return Err(JqError::Unsupported("assignment (=)".into()));
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
                                return Err(JqError::Unsupported("string interpolation".into()));
                            }
                            Some(other) => {
                                return Err(JqError::Unsupported(format!(
                                    "the string escape \\{other}"
                                )));
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
                        return Err(JqError::Unsupported(format!("the {text} construct")));
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
    /// array, so the stages that would consume that array (`map`,
    /// array functions, `length`) must not iterate again.
    Elements,
    /// A value stream still awaiting its `::` projection (after
    /// `map(...)` or an array collect).
    PendingValues,
    /// Projected values (after an aggregate); only further
    /// aggregates may follow.
    Values,
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

struct Parser {
    tokens: Vec<(Tok, usize)>,
    pos: usize,
    notes: Vec<String>,
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
                State::Values => {}
            }
            state = State::Values;
            self.notes.push(
                "array collect: Quarb streams the values; jq boxes them into one array".into(),
            );
            while self.peek() == Some(&Tok::Pipe) {
                self.pos += 1;
                state = self.aggregate_stage(&mut out, state)?;
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
        }
        Ok(out)
    }

    /// A pipeline of stages, stopping at `]`, `)`, or end.
    fn pipeline(&mut self, out: &mut String) -> Result<State, JqError> {
        let mut state = self.stage(out, State::Nodes)?;
        while self.peek() == Some(&Tok::Pipe) {
            self.pos += 1;
            state = self.stage(out, state)?;
        }
        Ok(state)
    }

    /// One pipeline stage.
    fn stage(&mut self, out: &mut String, state: State) -> Result<State, JqError> {
        match self.peek() {
            Some(Tok::Dot) => {
                if state == State::Elements {
                    return Err(JqError::Unsupported(
                        "navigation on a sliced array (iterate the slice with [] first)".into(),
                    ));
                }
                if state != State::Nodes {
                    return Err(JqError::Unsupported(
                        "navigation after a value-producing stage".into(),
                    ));
                }
                let sliced = self.nav_path(out)?;
                Ok(if sliced {
                    State::Elements
                } else {
                    State::Nodes
                })
            }
            Some(Tok::DotDot) => Err(JqError::Unsupported(
                "recursive descent .. (use Quarb's // in a hand-written query)".into(),
            )),
            Some(Tok::Ident(f, true)) if f == "select" => {
                if state == State::Elements {
                    return Err(JqError::Unsupported(
                        "select on a sliced array (jq tests the whole array; iterate \
                         the slice with [] first)"
                            .into(),
                    ));
                }
                if state != State::Nodes {
                    return Err(JqError::Unsupported(
                        "select after a value-producing stage".into(),
                    ));
                }
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after select")?;
                let cond = self.cond_or()?;
                self.expect(Tok::RParen, "')' to close select")?;
                write!(out, "[{cond}]").unwrap();
                Ok(State::Nodes)
            }
            Some(Tok::Ident(f, true)) if f == "map" => {
                if !matches!(state, State::Nodes | State::Elements) {
                    return Err(JqError::Unsupported(
                        "map after a value-producing stage".into(),
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
                        return Err(JqError::Unsupported("a slice inside map".into()));
                    }
                } else {
                    return Err(JqError::Unsupported("map with a non-path body".into()));
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
                        out.push_str("::;length");
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
                    State::Values => {
                        out.push_str(" @| count");
                        Ok(State::Values)
                    }
                }
            }
            Some(Tok::Ident(f, false)) if f == "keys" => {
                if state != State::Nodes {
                    return Err(JqError::Unsupported(
                        "keys after a value-producing stage".into(),
                    ));
                }
                self.pos += 1;
                // Child names, sorted — jq's keys sorts.
                out.push_str("/*:::name @| sort");
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
                        return Err(JqError::Unsupported(format!(
                            "{f} with a non-literal argument"
                        )));
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
                }
                Ok(State::Values)
            }
            Some(Tok::Ident(f, _)) => Err(JqError::Unsupported(format!("the {f} function"))),
            Some(Tok::Var(v)) => Err(JqError::Unsupported(format!("the variable ${v}"))),
            Some(Tok::LBracket) => Err(JqError::Unsupported(
                "array construction inside a pipeline (only supported at the start)".into(),
            )),
            other => {
                let what = other.map(|t| t.to_string()).unwrap_or_else(|| "end".into());
                Err(JqError::Syntax(
                    0,
                    format!("expected a stage, found {what}"),
                ))
            }
        }
    }

    /// A navigation path starting at `.`: `.a.b[0]["k"][]?[1:3]…`,
    /// emitted as Quarb hops. Returns whether the path ends in a
    /// slice (whose elements Quarb streams where jq boxes an array).
    fn nav_path(&mut self, out: &mut String) -> Result<bool, JqError> {
        self.expect(Tok::Dot, "'.'")?;
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
                        return Err(JqError::Unsupported(
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
                // Array elements are named by index in the JSON
                // adapter, so a plain name hop addresses them.
                write!(out, "/{n}").unwrap();
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
        write!(out, "/*[{quarb_start}..{quarb_end}]").unwrap();
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
                    return Err(JqError::Unsupported(format!("the non-integer index {n}")));
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
            left = format!("{left} and {right}");
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
                    return Err(JqError::Unsupported("has with a non-literal key".into()));
                };
                self.expect(Tok::RParen, "')' to close has")?;
                Ok((format!("/{}", quarb_name(&key)), false))
            }
            Some(Tok::Ident(f, _)) => Err(JqError::Unsupported(format!(
                "the {f} function in a condition"
            ))),
            Some(Tok::Dot) => {
                let mut path = String::new();
                if self.nav_path(&mut path)? {
                    return Err(JqError::Unsupported("a slice inside a condition".into()));
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

    /// A `@|` aggregate stage after an array collect.
    fn aggregate_stage(&mut self, out: &mut String, state: State) -> Result<State, JqError> {
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
                        return Err(JqError::Unsupported(format!(
                            "{f} with a non-literal argument"
                        )));
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
                Err(JqError::Unsupported(format!(
                    "'{what}' after an array collect (only array functions follow)"
                )))
            }
        }
    }
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
        Err(JqError::Unsupported(
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
            Err(JqError::Unsupported(msg)) => msg,
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn navigation() {
        assert_eq!(t("."), "^::");
        assert_eq!(tn("."), "^");
        assert_eq!(t(".users"), "/users::");
        assert_eq!(tn(".users"), "/users");
        assert_eq!(t(".users[0].name"), "/users/0/name::");
        assert_eq!(t(".a.b.c"), "/a/b/c::");
        assert_eq!(t(".a | .b"), "/a/b::");
        assert_eq!(t(".users[]"), "/users/*::");
        assert_eq!(t(".[]"), "/*::");
        assert_eq!(t(".[2]"), "/2::");
        assert_eq!(t(".[-1]"), "/*[-1]::");
        assert_eq!(t(".users[-2]"), "/users/*[-2]::");
        assert_eq!(t(r#"."weird key""#), "/'weird key'::");
        assert_eq!(t(r#".["k"]"#), "/k::");
        assert_eq!(t(".users[0].name?"), "/users/0/name::");
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
            "/users/*[/age:: >= 18 and /age:: < 65]::"
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
        assert_eq!(t(".users | length"), "/users::;length");
        assert_eq!(t(".users[0] | keys"), "/users/0/*:::name @| sort");
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
