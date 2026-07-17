//! XPath 1.0 importer for Quarb.
//!
//! Translates an XPath 1.0 expression into an equivalent Quarb
//! query, following the mapping in the specification's comparative
//! guide. The translatable subset covers the axes and predicate
//! forms Quarb models directly; everything else is refused with an
//! [`XPathError::Unsupported`] naming the construct, never silently
//! approximated.
//!
//! - Axes: `child`, `descendant`, `parent` (`..`), `ancestor`,
//!   `self` (`.`), `attribute` (`@`), `following-sibling` (`>>`),
//!   `preceding-sibling` (`<<`). No `following`/`preceding`
//!   (document-order) or `namespace` axes.
//! - Node tests: names (namespace-prefixed names become quoted
//!   Quarb segments), `*`, and terminal `text()`. No `node()`,
//!   `comment()`, or `processing-instruction()`: Quarb navigates
//!   elements only.
//! - Predicates: positional `[n]`, `[last()]` → `[-1]` (and
//!   `[last() - k]` → `[-(k+1)]`), `position()` comparisons →
//!   index / range predicates (`[position() > 1]` → `[2..]`),
//!   comparisons over attributes, `text()`, `.` and relative
//!   descending paths, existence tests, `and`/`or`/`not()`,
//!   `contains()`, `starts-with()`.
//! - Top level: union `|` becomes `||`; `count(...)` and `sum(...)`
//!   become `@| count` / `@| sum` aggregations.
//!
//! Known semantic divergences are reported as [`Translation::notes`]
//! rather than errors:
//!
//! - `[n]` on an abbreviated `//` step: XPath expands `//name[n]`
//!   to a per-parent `child::` step, positioning within each parent,
//!   while Quarb positions within the hop's whole per-source result
//!   list. The two agree when all matches share one parent. On the
//!   child axis (`name[n]`) and the explicit descendant axis
//!   (`descendant::name[n]`) the translation is exact.
//! - Quarb's `::text` is the concatenated descendant text, while
//!   XPath's `text()` selects immediate text-node children. The two
//!   agree on leaf elements.

mod export;
pub use export::export;

use std::fmt::Write as _;

/// An error translating an XPath expression.
#[derive(Debug, thiserror::Error)]
pub enum XPathError {
    #[error("XPath syntax error at offset {0}: {1}")]
    Syntax(usize, String),
    #[error("unsupported XPath construct: {0}")]
    Unsupported(String),
}

/// A successful translation: the Quarb query, plus notes on known
/// semantic divergences that apply to it.
#[derive(Debug)]
pub struct Translation {
    pub query: String,
    pub notes: Vec<String>,
}

/// Translate an XPath 1.0 expression to a Quarb query.
pub fn translate(xpath: &str) -> Result<Translation, XPathError> {
    let tokens = lex(xpath)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        notes: Vec::new(),
    };
    let query = parser.top()?;
    if parser.pos < parser.tokens.len() {
        return Err(XPathError::Syntax(
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
    Slash,
    SlashSlash,
    Union,
    LBracket,
    RBracket,
    LParen,
    RParen,
    At,
    Comma,
    Dot,
    DotDot,
    Star,
    Minus,
    Cmp(&'static str),
    Number(String),
    Literal(String),
    /// A name or QName; `true` when immediately followed by `(`
    /// (function call) and `false` otherwise. The paired axis test
    /// (`name::`) is a separate token.
    Name(String, bool),
    Axis(String),
}

impl std::fmt::Display for Tok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tok::Slash => write!(f, "/"),
            Tok::SlashSlash => write!(f, "//"),
            Tok::Union => write!(f, "|"),
            Tok::LBracket => write!(f, "["),
            Tok::RBracket => write!(f, "]"),
            Tok::LParen => write!(f, "("),
            Tok::RParen => write!(f, ")"),
            Tok::At => write!(f, "@"),
            Tok::Comma => write!(f, ","),
            Tok::Dot => write!(f, "."),
            Tok::DotDot => write!(f, ".."),
            Tok::Star => write!(f, "*"),
            Tok::Minus => write!(f, "-"),
            Tok::Cmp(op) => write!(f, "{op}"),
            Tok::Number(n) => write!(f, "{n}"),
            Tok::Literal(s) => write!(f, "'{s}'"),
            Tok::Name(n, _) => write!(f, "{n}"),
            Tok::Axis(a) => write!(f, "{a}::"),
        }
    }
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '_')
}

fn lex(input: &str) -> Result<Vec<(Tok, usize)>, XPathError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at = i;
        match c {
            c if c.is_whitespace() => i += 1,
            '/' => {
                if chars.get(i + 1) == Some(&'/') {
                    tokens.push((Tok::SlashSlash, at));
                    i += 2;
                } else {
                    tokens.push((Tok::Slash, at));
                    i += 1;
                }
            }
            '|' => {
                tokens.push((Tok::Union, at));
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
            '@' => {
                tokens.push((Tok::At, at));
                i += 1;
            }
            ',' => {
                tokens.push((Tok::Comma, at));
                i += 1;
            }
            '*' => {
                tokens.push((Tok::Star, at));
                i += 1;
            }
            '-' => {
                tokens.push((Tok::Minus, at));
                i += 1;
            }
            '.' => {
                if chars.get(i + 1) == Some(&'.') {
                    tokens.push((Tok::DotDot, at));
                    i += 2;
                } else if chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
                    // A number like `.5`.
                    let mut text = String::from("0.");
                    i += 1;
                    while chars.get(i).is_some_and(char::is_ascii_digit) {
                        text.push(chars[i]);
                        i += 1;
                    }
                    tokens.push((Tok::Number(text), at));
                } else {
                    tokens.push((Tok::Dot, at));
                    i += 1;
                }
            }
            '=' => {
                tokens.push((Tok::Cmp("="), at));
                i += 1;
            }
            '!' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push((Tok::Cmp("!="), at));
                    i += 2;
                } else {
                    return Err(XPathError::Syntax(at, "lone '!'".into()));
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
            '\'' | '"' => {
                let mut text = String::new();
                i += 1;
                while i < chars.len() && chars[i] != c {
                    text.push(chars[i]);
                    i += 1;
                }
                if i == chars.len() {
                    return Err(XPathError::Syntax(at, "unterminated string literal".into()));
                }
                i += 1;
                tokens.push((Tok::Literal(text), at));
            }
            c if c.is_ascii_digit() => {
                let mut text = String::new();
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    text.push(chars[i]);
                    i += 1;
                }
                if chars.get(i) == Some(&'.') {
                    text.push('.');
                    i += 1;
                    while chars.get(i).is_some_and(char::is_ascii_digit) {
                        text.push(chars[i]);
                        i += 1;
                    }
                }
                tokens.push((Tok::Number(text), at));
            }
            c if is_name_start(c) => {
                let mut text = String::new();
                while chars.get(i).is_some_and(|&c| is_name_char(c)) {
                    text.push(chars[i]);
                    i += 1;
                }
                // A QName: one `name:name` (but not `name::`).
                if chars.get(i) == Some(&':')
                    && chars.get(i + 1) != Some(&':')
                    && chars.get(i + 1).is_some_and(|&c| is_name_start(c))
                {
                    text.push(':');
                    i += 1;
                    while chars.get(i).is_some_and(|&c| is_name_char(c)) {
                        text.push(chars[i]);
                        i += 1;
                    }
                }
                if chars.get(i) == Some(&':') && chars.get(i + 1) == Some(&':') {
                    tokens.push((Tok::Axis(text), at));
                    i += 2;
                } else {
                    let calls = chars.get(i) == Some(&'(');
                    tokens.push((Tok::Name(text, calls), at));
                }
            }
            other => {
                return Err(XPathError::Syntax(at, format!("unexpected '{other}'")));
            }
        }
    }
    Ok(tokens)
}

// --------------------------------------------------------------- parser

/// The divergence note for a positional predicate on an abbreviated
/// `//` step.
const ABBREV_INDEX_NOTE: &str = "[n] on //: XPath's abbreviated // positions within each \
     parent; Quarb positions within the hop's whole result list \
     (equal when all matches share one parent)";

struct Parser {
    tokens: Vec<(Tok, usize)>,
    pos: usize,
    notes: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    fn peek2(&self) -> Option<&Tok> {
        self.tokens.get(self.pos + 1).map(|(t, _)| t)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).map(|(t, _)| t.clone());
        self.pos += 1;
        t
    }

    fn expect(&mut self, tok: Tok, what: &str) -> Result<(), XPathError> {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            Ok(())
        } else {
            let at = self
                .tokens
                .get(self.pos)
                .map(|(_, at)| *at)
                .unwrap_or_default();
            Err(XPathError::Syntax(at, format!("expected {what}")))
        }
    }

    /// The whole expression: a union of paths, or a top-level
    /// `count(...)` / `sum(...)` aggregation.
    fn top(&mut self) -> Result<String, XPathError> {
        if let Some(Tok::Name(name, true)) = self.peek() {
            let func = name.clone();
            match func.as_str() {
                "count" => {
                    self.pos += 1;
                    self.expect(Tok::LParen, "'(' after count")?;
                    let inner = self.union()?;
                    self.expect(Tok::RParen, "')' to close count")?;
                    return Ok(format!("{inner} @| count"));
                }
                "sum" => {
                    self.pos += 1;
                    self.expect(Tok::LParen, "'(' after sum")?;
                    let (inner, projected) = self.path()?;
                    self.expect(Tok::RParen, "')' to close sum")?;
                    // `sum` needs values; project the node set if the
                    // path did not already end in a projection.
                    let projected = if projected {
                        inner
                    } else {
                        format!("{inner}::")
                    };
                    return Ok(format!("{projected} @| sum"));
                }
                _ => {}
            }
        }
        self.union()
    }

    fn union(&mut self) -> Result<String, XPathError> {
        let mut parts = vec![self.path()?.0];
        while self.peek() == Some(&Tok::Union) {
            self.pos += 1;
            parts.push(self.path()?.0);
        }
        Ok(parts.join(" || "))
    }

    /// An absolute location path. The flag reports whether the path
    /// ends in a projection (a terminal attribute or `text()` step).
    fn path(&mut self) -> Result<(String, bool), XPathError> {
        let mut out = String::new();
        let mut projection: Option<String> = None;

        let mut sep = match self.peek() {
            Some(Tok::Slash) => "/",
            Some(Tok::SlashSlash) => "//",
            _ => {
                return Err(XPathError::Unsupported(
                    "relative paths (Quarb queries are root-anchored; start with / or //)".into(),
                ));
            }
        };
        self.pos += 1;

        // A bare `/` selects the document root.
        if self.peek().is_none() || self.peek() == Some(&Tok::Union) {
            if sep == "/" {
                return Ok(("^".into(), false));
            }
            return Err(XPathError::Syntax(0, "'//' needs a step".into()));
        }

        loop {
            if projection.is_some() {
                return Err(XPathError::Unsupported(
                    "steps after an attribute or text() step".into(),
                ));
            }
            match self.step(sep, &mut out)? {
                StepOut::Element => {}
                StepOut::Projection(p) => projection = Some(p),
            }
            sep = match self.peek() {
                Some(Tok::Slash) => "/",
                Some(Tok::SlashSlash) => "//",
                _ => break,
            };
            self.pos += 1;
        }

        let projected = projection.is_some();
        if let Some(p) = projection {
            out.push_str(&p);
        }
        Ok((out, projected))
    }

    /// One location step. Writes navigation into `out`; a terminal
    /// attribute / `text()` step is returned as a projection instead.
    fn step(&mut self, sep: &str, out: &mut String) -> Result<StepOut, XPathError> {
        // Resolve an explicit axis to the Quarb hop syntax. XPath's
        // abbreviated `//` expands to a per-parent `child::` step, so
        // positional predicates on it diverge from Quarb's per-source
        // list — an *explicit* descendant:: axis does not.
        let mut abbreviated_descendant = sep == "//";
        let (axis, sep) = match self.peek() {
            Some(Tok::Axis(a)) => {
                let a = a.clone();
                self.pos += 1;
                abbreviated_descendant = false;
                match a.as_str() {
                    "child" => ("child", sep),
                    "descendant" | "descendant-or-self" => ("child", "//"),
                    "parent" => ("parent", sep),
                    "ancestor" | "ancestor-or-self" => ("ancestor", sep),
                    "self" => ("self", sep),
                    "attribute" => ("attribute", sep),
                    "following-sibling" => ("following-sibling", sep),
                    "preceding-sibling" => ("preceding-sibling", sep),
                    other => {
                        return Err(XPathError::Unsupported(format!(
                            "the {other}:: axis (no Quarb equivalent)"
                        )));
                    }
                }
            }
            Some(Tok::At) => {
                self.pos += 1;
                ("attribute", sep)
            }
            _ => ("child", sep),
        };

        match axis {
            "attribute" => {
                let name = match self.bump() {
                    Some(Tok::Name(n, false)) => n,
                    Some(Tok::Star) => {
                        return Err(XPathError::Unsupported(
                            "@* (Quarb projects one named property at a time)".into(),
                        ));
                    }
                    _ => return Err(XPathError::Syntax(0, "expected an attribute name".into())),
                };
                return Ok(StepOut::Projection(format!("::{}", quarb_name(&name))));
            }
            "self" | "parent" | "ancestor" | "following-sibling" | "preceding-sibling" => {
                let hop = match axis {
                    "parent" => "\\",
                    "ancestor" => "\\\\",
                    "following-sibling" => ">>",
                    "preceding-sibling" => "<<",
                    _ => "",
                };
                match self.bump() {
                    // `self::name` filters the current node by name.
                    Some(Tok::Name(n, false)) if axis == "self" => {
                        write!(out, "[:::name = {}]", quarb_literal(&n)?).unwrap();
                    }
                    Some(Tok::Star) if axis == "self" => {}
                    Some(Tok::Name(n, false)) => {
                        write!(out, "{hop}{}", quarb_name(&n)).unwrap();
                    }
                    Some(Tok::Star) => {
                        write!(out, "{hop}*").unwrap();
                    }
                    Some(Tok::Name(n, true)) if n == "node" => {
                        self.expect(Tok::LParen, "'('")?;
                        self.expect(Tok::RParen, "')'")?;
                        if axis != "self" {
                            write!(out, "{hop}*").unwrap();
                        }
                    }
                    _ => {
                        return Err(XPathError::Syntax(0, format!("bad {axis}:: node test")));
                    }
                }
                return Ok(StepOut::Element);
            }
            _ => {}
        }

        // The child/descendant axis.
        match self.bump() {
            Some(Tok::Name(n, false)) => {
                write!(out, "{sep}{}", quarb_name(&n)).unwrap();
            }
            Some(Tok::Star) => {
                write!(out, "{sep}*").unwrap();
            }
            Some(Tok::Dot) => return Ok(StepOut::Element),
            Some(Tok::DotDot) => {
                write!(out, "\\*").unwrap();
                return Ok(StepOut::Element);
            }
            Some(Tok::Name(n, true)) if n == "text" => {
                self.expect(Tok::LParen, "'('")?;
                self.expect(Tok::RParen, "')'")?;
                self.notes.push(
                    "text(): Quarb ::text is the concatenated descendant text; \
                     XPath text() selects immediate text nodes (equal on leaf elements)"
                        .into(),
                );
                return Ok(StepOut::Projection("::text".into()));
            }
            Some(Tok::Name(n, true)) if n == "node" => {
                return Err(XPathError::Unsupported(
                    "node() (Quarb navigates elements only)".into(),
                ));
            }
            Some(Tok::Name(n, true)) => {
                return Err(XPathError::Unsupported(format!("the {n}() node test")));
            }
            other => {
                return Err(XPathError::Syntax(
                    0,
                    format!(
                        "expected a step, found {}",
                        other.map(|t| t.to_string()).unwrap_or_else(|| "end".into())
                    ),
                ));
            }
        }

        // Predicates.
        while self.peek() == Some(&Tok::LBracket) {
            self.pos += 1;
            self.predicate(abbreviated_descendant, out)?;
            self.expect(Tok::RBracket, "']' to close the predicate")?;
        }
        Ok(StepOut::Element)
    }

    /// One `[...]` predicate on an element step. Quarb's predicate
    /// model matches XPath's (sequential, positions among the step's
    /// results) except on an abbreviated `//` step, where XPath
    /// positions within each parent.
    fn predicate(
        &mut self,
        abbreviated_descendant: bool,
        out: &mut String,
    ) -> Result<(), XPathError> {
        // Positional forms first.
        match (self.peek(), self.peek2()) {
            (Some(Tok::Number(n)), Some(Tok::RBracket)) => {
                let n = n.clone();
                if n.contains('.') {
                    return Err(XPathError::Unsupported(format!(
                        "the non-integer position [{n}]"
                    )));
                }
                if abbreviated_descendant {
                    self.notes.push(ABBREV_INDEX_NOTE.into());
                }
                self.pos += 1;
                write!(out, "[{n}]").unwrap();
                return Ok(());
            }
            // `[last()]` → `[-1]`; `[last() - k]` → `[-(k+1)]`.
            (Some(Tok::Name(f, true)), _) if f == "last" => {
                self.pos += 1;
                self.expect(Tok::LParen, "'('")?;
                self.expect(Tok::RParen, "')'")?;
                let back: i64 = if self.peek() == Some(&Tok::Minus) {
                    self.pos += 1;
                    match self.bump() {
                        Some(Tok::Number(k)) => k.parse().map_err(|_| {
                            XPathError::Unsupported(format!("last() - {k} (non-integer)"))
                        })?,
                        _ => {
                            return Err(XPathError::Syntax(
                                0,
                                "expected a number after last() -".into(),
                            ));
                        }
                    }
                } else {
                    0
                };
                if abbreviated_descendant {
                    self.notes.push(ABBREV_INDEX_NOTE.into());
                }
                write!(out, "[-{}]", back + 1).unwrap();
                return Ok(());
            }
            // `position()` comparisons → index / range predicates.
            (Some(Tok::Name(f, true)), _) if f == "position" => {
                self.pos += 1;
                self.expect(Tok::LParen, "'('")?;
                self.expect(Tok::RParen, "')'")?;
                let op = match self.bump() {
                    Some(Tok::Cmp(op)) => op,
                    _ => {
                        return Err(XPathError::Syntax(
                            0,
                            "expected a comparison after position()".into(),
                        ));
                    }
                };
                let n: u64 = match self.bump() {
                    Some(Tok::Number(n)) => n.parse().map_err(|_| {
                        XPathError::Unsupported(format!("position() {op} {n} (non-integer)"))
                    })?,
                    _ => {
                        return Err(XPathError::Syntax(
                            0,
                            "expected a number after position() comparison".into(),
                        ));
                    }
                };
                if abbreviated_descendant {
                    self.notes.push(ABBREV_INDEX_NOTE.into());
                }
                match op {
                    "=" => write!(out, "[{n}]").unwrap(),
                    ">" => write!(out, "[{}..]", n + 1).unwrap(),
                    ">=" => write!(out, "[{n}..]").unwrap(),
                    "<" => write!(out, "[..{}]", n.saturating_sub(1)).unwrap(),
                    "<=" => write!(out, "[..{n}]").unwrap(),
                    other => {
                        return Err(XPathError::Unsupported(format!("position() {other} {n}")));
                    }
                }
                return Ok(());
            }
            _ => {}
        }

        let expr = self.pred_or()?;
        write!(out, "[{expr}]").unwrap();
        Ok(())
    }

    fn pred_or(&mut self) -> Result<String, XPathError> {
        let mut left = self.pred_and()?;
        while matches!(self.peek(), Some(Tok::Name(n, false)) if n == "or") {
            self.pos += 1;
            let right = self.pred_and()?;
            left = format!("{left} or {right}");
        }
        Ok(left)
    }

    fn pred_and(&mut self) -> Result<String, XPathError> {
        let mut left = self.pred_cmp()?;
        while matches!(self.peek(), Some(Tok::Name(n, false)) if n == "and") {
            self.pos += 1;
            let right = self.pred_cmp()?;
            left = format!("{left} and {right}");
        }
        Ok(left)
    }

    fn pred_cmp(&mut self) -> Result<String, XPathError> {
        let (left, left_bare) = self.pred_primary()?;
        if let Some(Tok::Cmp(op)) = self.peek() {
            let op = *op;
            self.pos += 1;
            let (right, right_bare) = self.pred_primary()?;
            // A compared element path is compared by value: project it.
            let left = if left_bare { format!("{left}::") } else { left };
            let right = if right_bare {
                format!("{right}::")
            } else {
                right
            };
            return Ok(format!("{left} {op} {right}"));
        }
        // A bare path stays structural: an existence test.
        Ok(left)
    }

    /// A predicate operand or boolean atom. The flag reports a bare
    /// element path (one that needs `::` appended when its *value* is
    /// wanted rather than its existence).
    fn pred_primary(&mut self) -> Result<(String, bool), XPathError> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let inner = self.pred_or()?;
                self.expect(Tok::RParen, "')' to close the group")?;
                Ok((format!("({inner})"), false))
            }
            Some(Tok::Name(f, true)) if f == "not" => {
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after not")?;
                let inner = self.pred_or()?;
                self.expect(Tok::RParen, "')' to close not")?;
                Ok((format!("not ({inner})"), false))
            }
            Some(Tok::Name(f, true)) if f == "contains" => {
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after contains")?;
                let (hay, hay_bare) = self.pred_primary()?;
                let hay = if hay_bare { format!("{hay}::") } else { hay };
                self.expect(Tok::Comma, "',' between contains arguments")?;
                let (needle, needle_bare) = self.pred_primary()?;
                let needle = if needle_bare {
                    format!("{needle}::")
                } else {
                    needle
                };
                self.expect(Tok::RParen, "')' to close contains")?;
                Ok((format!("{hay} *= {needle}"), false))
            }
            Some(Tok::Name(f, true)) if f == "starts-with" => {
                self.pos += 1;
                self.expect(Tok::LParen, "'(' after starts-with")?;
                let (subject, bare) = self.pred_primary()?;
                let subject = if bare {
                    format!("{subject}::")
                } else {
                    subject
                };
                self.expect(Tok::Comma, "',' between starts-with arguments")?;
                let Some(Tok::Literal(prefix)) = self.bump() else {
                    return Err(XPathError::Unsupported(
                        "starts-with with a non-literal prefix".into(),
                    ));
                };
                self.expect(Tok::RParen, "')' to close starts-with")?;
                if prefix.contains(['(', ')']) {
                    return Err(XPathError::Unsupported(
                        "starts-with prefix containing parentheses".into(),
                    ));
                }
                self.expect_nothing_weird(&prefix)?;
                Ok((format!("{subject} =~ ~(^{})", regex_escape(&prefix)), false))
            }
            Some(Tok::Name(f, true)) if f == "text" => self.pred_path(),
            Some(Tok::Name(f, true)) => Err(XPathError::Unsupported(format!(
                "the {f}() function in a predicate"
            ))),
            Some(Tok::Literal(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok((quarb_literal(&s)?, false))
            }
            Some(Tok::Number(n)) => {
                let n = n.clone();
                self.pos += 1;
                Ok((n, false))
            }
            _ => self.pred_path(),
        }
    }

    /// A relative path operand inside a predicate: `@attr`, `text()`,
    /// `.`, or a descending element path with an optional terminal
    /// `@attr` / `text()`. Bare (projection-less) paths are flagged.
    fn pred_path(&mut self) -> Result<(String, bool), XPathError> {
        let mut out = String::new();
        let mut sep = "/";
        // Leading `.` / `./` / `.//`.
        if self.peek() == Some(&Tok::Dot) {
            self.pos += 1;
            match self.peek() {
                Some(Tok::Slash) => {
                    self.pos += 1;
                }
                Some(Tok::SlashSlash) => {
                    sep = "//";
                    self.pos += 1;
                }
                // A bare `.` is the candidate's own value.
                _ => return Ok(("::".into(), false)),
            }
        } else if self.peek() == Some(&Tok::SlashSlash) || self.peek() == Some(&Tok::Slash) {
            return Err(XPathError::Unsupported(
                "absolute paths inside predicates (Quarb predicate paths are \
                 relative to the candidate node)"
                    .into(),
            ));
        }

        loop {
            match self.peek() {
                Some(Tok::At) => {
                    self.pos += 1;
                    let Some(Tok::Name(name, false)) = self.bump() else {
                        return Err(XPathError::Syntax(0, "expected an attribute name".into()));
                    };
                    write!(out, "::{}", quarb_name(&name)).unwrap();
                    return Ok((out, false));
                }
                Some(Tok::Name(f, true)) if f == "text" => {
                    self.pos += 1;
                    self.expect(Tok::LParen, "'('")?;
                    self.expect(Tok::RParen, "')'")?;
                    out.push_str("::text");
                    return Ok((out, false));
                }
                Some(Tok::Name(n, false)) => {
                    let n = n.clone();
                    self.pos += 1;
                    write!(out, "{sep}{}", quarb_name(&n)).unwrap();
                }
                Some(Tok::Star) => {
                    self.pos += 1;
                    write!(out, "{sep}*").unwrap();
                }
                Some(Tok::DotDot) | Some(Tok::Axis(_)) => {
                    return Err(XPathError::Unsupported(
                        "upward navigation inside predicates (Quarb predicate \
                         paths descend from the candidate node)"
                            .into(),
                    ));
                }
                other => {
                    return Err(XPathError::Syntax(
                        0,
                        format!(
                            "expected a predicate operand, found {}",
                            other.map(|t| t.to_string()).unwrap_or_else(|| "end".into())
                        ),
                    ));
                }
            }
            sep = match self.peek() {
                Some(Tok::Slash) => "/",
                Some(Tok::SlashSlash) => "//",
                _ => break,
            };
            self.pos += 1;
        }
        Ok((out, true))
    }

    fn expect_nothing_weird(&self, literal: &str) -> Result<(), XPathError> {
        if literal.contains(['\'', '"']) {
            return Err(XPathError::Unsupported(
                "a literal containing quote characters".into(),
            ));
        }
        Ok(())
    }
}

enum StepOut {
    Element,
    Projection(String),
}

/// Render a name as a Quarb name segment, quoting it when it
/// contains characters outside Quarb's bare-name set (e.g. the `:`
/// of a namespace-prefixed name).
fn quarb_name(name: &str) -> String {
    let bare = name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if bare {
        name.to_string()
    } else {
        format!("'{name}'")
    }
}

/// Render a string literal as a Quarb quoted literal.
fn quarb_literal(s: &str) -> Result<String, XPathError> {
    if !s.contains('"') {
        Ok(format!("\"{s}\""))
    } else if !s.contains('\'') {
        Ok(format!("'{s}'"))
    } else {
        Err(XPathError::Unsupported(
            "a literal containing both quote characters".into(),
        ))
    }
}

/// Escape regex metacharacters in a literal (for `starts-with`).
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\^$.|?*+[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(xpath: &str) -> String {
        translate(xpath).unwrap().query
    }

    fn unsupported(xpath: &str) -> String {
        match translate(xpath) {
            Err(XPathError::Unsupported(msg)) => msg,
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn plain_paths() {
        assert_eq!(t("/EXAMPLE"), "/EXAMPLE");
        assert_eq!(t("/EXAMPLE/head/title"), "/EXAMPLE/head/title");
        assert_eq!(t("//p"), "//p");
        assert_eq!(t("/a//b/c"), "/a//b/c");
        assert_eq!(t("/*"), "/*");
        assert_eq!(t("/"), "^");
    }

    #[test]
    fn explicit_axes() {
        assert_eq!(t("/child::EXAMPLE/child::head"), "/EXAMPLE/head");
        assert_eq!(t("/descendant::title"), "//title");
        assert_eq!(t("/descendant::p/ancestor::chapter"), "//p\\\\chapter");
        assert_eq!(t("//image/parent::chapter"), "//image\\chapter");
        assert_eq!(t("//image/.."), "//image\\*");
        assert_eq!(t("//image/../title"), "//image\\*/title");
        assert_eq!(t("//p/self::p"), "//p[:::name = \"p\"]");
    }

    #[test]
    fn attributes_and_text() {
        assert_eq!(t("//chapter/@id"), "//chapter::id");
        assert_eq!(t("/EXAMPLE/attribute::prop1"), "/EXAMPLE::prop1");
        assert_eq!(t("//title/text()"), "//title::text");
    }

    #[test]
    fn predicates() {
        assert_eq!(t("//chapter[2]"), "//chapter[2]");
        assert_eq!(t("//*[2]"), "//*[2]");
        assert_eq!(
            t("//chapter[@id='chapter2']"),
            "//chapter[::id = \"chapter2\"]"
        );
        assert_eq!(t("//chapter[title]"), "//chapter[/title]");
        assert_eq!(
            t("//chapter[title='Chapter 2']"),
            "//chapter[/title:: = \"Chapter 2\"]"
        );
        assert_eq!(t("//chapter[@id][image]"), "//chapter[::id][/image]");
        assert_eq!(
            t("//chapter[image/@href='linus.gif']/title"),
            "//chapter[/image::href = \"linus.gif\"]/title"
        );
        assert_eq!(t("//chapter[.//image]"), "//chapter[//image]");
        assert_eq!(t("//p[.='...']"), "//p[:: = \"...\"]");
        assert_eq!(t("//chapter[text()='x']"), "//chapter[::text = \"x\"]");
        assert_eq!(
            t("//chapter[@id='a' or @id='b']"),
            "//chapter[::id = \"a\" or ::id = \"b\"]"
        );
        assert_eq!(
            t("//chapter[not(image) and title]"),
            "//chapter[not (/image) and /title]"
        );
        assert_eq!(
            t("//chapter[contains(@id, 'apt')]"),
            "//chapter[::id *= \"apt\"]"
        );
        assert_eq!(
            t("//chapter[starts-with(@id, 'chap')]"),
            "//chapter[::id =~ ~(^chap)]"
        );
        assert_eq!(
            t("//city[population > 100000]"),
            "//city[/population:: > 100000]"
        );
    }

    #[test]
    fn positional_predicates() {
        assert_eq!(t("//chapter[position() = 2]"), "//chapter[2]");
        assert_eq!(t("//chapter[last()]"), "//chapter[-1]");
        assert_eq!(t("//chapter[last()]/@id"), "//chapter[-1]::id");
        assert_eq!(t("//chapter[last()]/title"), "//chapter[-1]/title");
        assert_eq!(t("//chapter[last()-1]"), "//chapter[-2]");
        assert_eq!(t("//chapter[position() > 2]"), "//chapter[3..]");
        assert_eq!(t("//chapter[position() >= 2]"), "//chapter[2..]");
        assert_eq!(t("//chapter[position() < 3]"), "//chapter[..2]");
        assert_eq!(t("//chapter[position() <= 3]"), "//chapter[..3]");
        // positional predicates mid-path are ordinary predicates now
        assert_eq!(t("//chapter[1]/p[last()]/img"), "//chapter[1]/p[-1]/img");
    }

    #[test]
    fn unions_and_aggregates() {
        assert_eq!(t("//title | //p"), "//title || //p");
        assert_eq!(t("count(//chapter)"), "//chapter @| count");
        assert_eq!(t("sum(//book/@pages)"), "//book::pages @| sum");
        assert_eq!(t("sum(//population)"), "//population:: @| sum");
    }

    #[test]
    fn qualified_names_quote() {
        assert_eq!(t("//dc:title"), "//'dc:title'");
        assert_eq!(t("//dc:title/text()"), "//'dc:title'::text");
    }

    #[test]
    fn unsupported_constructs() {
        assert!(unsupported("chapter/title").contains("relative"));
        assert!(unsupported("/following::p").contains("following"));
        assert!(unsupported("//node()").contains("elements only"));
        assert!(unsupported("//p[../title]").contains("upward"));
        assert!(unsupported("//chapter/@id/x").contains("after an attribute"));
    }

    #[test]
    fn notes_flag_divergences() {
        // [n] on an abbreviated // step: XPath positions per parent.
        let tr = translate("//p[1]").unwrap();
        assert_eq!(tr.query, "//p[1]");
        assert_eq!(tr.notes.len(), 1);
        assert!(tr.notes[0].contains("within each parent"));
        assert_eq!(translate("//*[2]").unwrap().notes.len(), 1);
        // ... but child-axis and explicit descendant:: [n] are exact.
        assert!(translate("/a/b[1]").unwrap().notes.is_empty());
        assert!(translate("/a/*[2]").unwrap().notes.is_empty());
        assert!(translate("/descendant::p[1]").unwrap().notes.is_empty());
        assert_eq!(translate("/descendant::p[1]").unwrap().query, "//p[1]");
        assert!(!translate("//title/text()").unwrap().notes.is_empty());
    }
}
