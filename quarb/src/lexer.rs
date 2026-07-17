//! Lexer for the supported query subset.
//!
//! Recognizes navigation, sibling, and crosslink operators, the
//! proximal/distal suffixes, anchors, projections, predicate
//! operators, pipeline syntax, `~(...)` regex names, and bare or
//! quoted names. Any other operator belongs to a Quarb feature the
//! engine does not implement yet and is rejected with a clear "not
//! yet supported" message.
//!
//! Scanning is index-based so a `-` can serve both as a filename
//! character (`foo-bar`) and as the start of the `->` crosslink.

use crate::error::{QuarbError, Result};

/// A lexical token.
#[derive(Debug, PartialEq, Eq)]
pub enum Token {
    /// `/`
    Slash,
    /// `//`
    SlashSlash,
    /// `\`
    Backslash,
    /// `\\`
    BackslashBackslash,
    /// `>`
    Gt,
    /// `>>`, `>>?`, `>>!` — all / nearest / farthest following
    /// siblings.
    FollowingSiblings(char),
    /// `<`
    Lt,
    /// `<<`, `<<?`, `<<!` — all / nearest / farthest preceding
    /// siblings. The payload is the reach mark (' ', '?', '!').
    PrecedingSiblings(char),
    /// `?` — proximal suffix.
    Question,
    /// `?=` — the value-match marker inside a parenthesized
    /// conditional: `(x ?= k ? r : else)`.
    QuestionEq,
    /// `!` — distal suffix.
    Bang,
    /// `^` — root anchor.
    Caret,
    /// `$` — leaf anchor.
    Dollar,
    /// `::` — property projection.
    ColonColon,
    /// `:::` — core-metadata projection.
    ColonColonColon,
    /// `::;` — adapter-metadata projection.
    ColonColonSemi,
    /// `|` — pipe / trait alternation.
    Pipe,
    /// `||` — union.
    PipePipe,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `,`
    Comma,
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `<=`
    Le,
    /// `>=`
    Ge,
    /// `=~` — regex match.
    Match,
    /// `!~` — regex non-match.
    NotMatch,
    /// `*=` — substring containment.
    Contains,
    /// `->` — outgoing crosslink.
    ArrowOut,
    /// `<-` — incoming crosslink.
    ArrowIn,
    /// `@` — register/aggregation sigil.
    At,
    /// `%` — the record sigil (`%.`, the named register view).
    Percent,
    /// `&` — the fragment sigil (`&name` invokes a `def`).
    Amp,
    /// `&&` — trait conjunction.
    AmpAmp,
    /// `:` — the definition separator (`def &name: body;`).
    Colon,
    /// `;` — the statement terminator (after a `def` body).
    Semi,
    /// `<=>` — correlation operator.
    Correlate,
    /// `~>` — cross-reference resolution.
    Resolve,
    /// `<~` — reverse cross-reference resolution.
    ReverseResolve,
    /// `{n}`, `{m,n}`, `{m,}` — a path-pattern repetition
    /// quantifier. `{n}` carries `(n, Some(n))`; `{m,}` carries
    /// `(m, None)` (open-ended, clamped to the adapter's quantifier
    /// bound at execution).
    Quant { min: usize, max: Option<usize> },
    /// `~(...)` — the inner regex pattern.
    Regex(String),
    /// `s/pat/repl/mods` — a substitution, lexed only in pipeline
    /// position (directly after `|`), like the `/.../` regex literal
    /// after `=~`. Elsewhere `s` is an ordinary name character run.
    Subst {
        pattern: String,
        replacement: String,
        mods: String,
    },
    /// A name, with quotes stripped and marked as quoted-literal.
    /// `glued` records that no whitespace separated the name from
    /// the preceding token — a projection's property name must be
    /// glued (`::price`), which is what lets a spaced name act as an
    /// arithmetic operator (`/price:: * /qty::`).
    Name {
        text: String,
        quoted: bool,
        glued: bool,
    },
    /// A double-quoted string with `${...}` holes — an interpolation.
    /// Hole expressions are lexed and parsed at parse time.
    Interp(Vec<InterpPart>),
    /// A backtick shell literal — sugar for the `sh(...)` stage,
    /// interpolated like a double-quoted string (Perl's `qx`).
    Shell(Vec<InterpPart>),
}

/// One segment of an interpolated string: literal text, or the raw
/// source of a `${...}` hole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpPart {
    Text(String),
    Hole(String),
}

/// Characters allowed in a bare name.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | '+')
}

/// Tokenize `input` into the subset's token stream.
pub fn lex(input: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    let at = |j: usize| chars.get(j).copied();

    let mut spaced = true;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            spaced = true;
            continue;
        }
        let glued = !spaced;
        spaced = false;
        match c {
            // A `/.../` regex literal on the right of `=~` / `!~`,
            // where `/` cannot be a hop. Elsewhere `/` is navigation.
            // `s/pat/repl/mods` directly after a pipe — the
            // substitution stage. `\/` escapes a literal slash.
            's' if at(i + 1) == Some('/') && matches!(tokens.last(), Some(Token::Pipe)) => {
                i += 2;
                let mut parts: Vec<String> = vec![String::new()];
                loop {
                    match at(i) {
                        Some('\\') if at(i + 1) == Some('/') => {
                            parts.last_mut().expect("nonempty").push('/');
                            i += 2;
                        }
                        Some('/') => {
                            i += 1;
                            // The third `/` terminates the form.
                            if parts.len() == 2 {
                                break;
                            }
                            parts.push(String::new());
                        }
                        Some(ch) => {
                            parts.last_mut().expect("nonempty").push(ch);
                            i += 1;
                        }
                        None => {
                            return Err(QuarbError::Lex(
                                "unterminated substitution 's/pat/repl/'".into(),
                            ));
                        }
                    }
                }
                let mut mods = String::new();
                while let Some(ch) = at(i) {
                    if ch.is_ascii_alphabetic() {
                        mods.push(ch);
                        i += 1;
                    } else {
                        break;
                    }
                }
                let replacement = parts.pop().expect("two parts");
                let pattern = parts.pop().expect("two parts");
                tokens.push(Token::Subst {
                    pattern,
                    replacement,
                    mods,
                });
            }
            '/' if matches!(tokens.last(), Some(Token::Match | Token::NotMatch)) => {
                let mut body = String::new();
                i += 1;
                loop {
                    match at(i) {
                        Some('\\') if at(i + 1) == Some('/') => {
                            body.push('/');
                            i += 2;
                        }
                        Some('/') => {
                            i += 1;
                            break;
                        }
                        Some(ch) => {
                            body.push(ch);
                            i += 1;
                        }
                        None => {
                            return Err(QuarbError::Lex("unterminated regex '/…'".into()));
                        }
                    }
                }
                // Trailing modifier letters (`/pat/imsx`): case-insensitive
                // (i), multi-line (m), dot-matches-newline (s), extended (x).
                // Folded into the pattern as an inline flag group so every
                // regex flavor (base `regex`, opt-in fancy-regex/PCRE2)
                // honors them without a separate build path.
                let mut flags = String::new();
                while let Some(m @ ('i' | 'm' | 's' | 'x')) = at(i) {
                    flags.push(m);
                    i += 1;
                }
                if flags.is_empty() {
                    tokens.push(Token::Regex(body));
                } else {
                    tokens.push(Token::Regex(format!("(?{flags}){body}")));
                }
            }
            '/' if at(i + 1) == Some('/') => {
                tokens.push(Token::SlashSlash);
                i += 2;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            '\\' if at(i + 1) == Some('\\') => {
                tokens.push(Token::BackslashBackslash);
                i += 2;
            }
            '\\' => {
                tokens.push(Token::Backslash);
                i += 1;
            }
            // `->` before `-` as a name char.
            '-' if at(i + 1) == Some('>') => {
                tokens.push(Token::ArrowOut);
                i += 2;
            }
            '<' if at(i + 1) == Some('<') => {
                let mark = match at(i + 2) {
                    Some(m @ ('?' | '!')) => {
                        i += 1;
                        m
                    }
                    _ => ' ',
                };
                tokens.push(Token::PrecedingSiblings(mark));
                i += 2;
            }
            // `<-` is the incoming crosslink, but `<-<digit>` is a
            // less-than against a negative literal (`::a<-3`),
            // matching the spaced `< -3`. A digit is never a
            // crosslink target, so only a non-digit keeps `<-`.
            '<' if at(i + 1) == Some('-') && !at(i + 2).is_some_and(|c| c.is_ascii_digit()) => {
                tokens.push(Token::ArrowIn);
                i += 2;
            }
            '<' if at(i + 1) == Some('~') => {
                tokens.push(Token::ReverseResolve);
                i += 2;
            }
            '<' if at(i + 1) == Some('=') && at(i + 2) == Some('>') => {
                tokens.push(Token::Correlate);
                i += 3;
            }
            '<' if at(i + 1) == Some('=') => {
                tokens.push(Token::Le);
                i += 2;
            }
            '<' => {
                tokens.push(Token::Lt);
                i += 1;
            }
            '>' if at(i + 1) == Some('>') => {
                let mark = match at(i + 2) {
                    Some(m @ ('?' | '!')) => {
                        i += 1;
                        m
                    }
                    _ => ' ',
                };
                tokens.push(Token::FollowingSiblings(mark));
                i += 2;
            }
            '>' if at(i + 1) == Some('=') => {
                tokens.push(Token::Ge);
                i += 2;
            }
            '>' => {
                tokens.push(Token::Gt);
                i += 1;
            }
            '?' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Token::QuestionEq);
                    i += 2;
                } else {
                    tokens.push(Token::Question);
                    i += 1;
                }
            }
            '!' if at(i + 1) == Some('=') => {
                tokens.push(Token::Ne);
                i += 2;
            }
            '!' if at(i + 1) == Some('~') => {
                tokens.push(Token::NotMatch);
                i += 2;
            }
            '!' => {
                tokens.push(Token::Bang);
                i += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
                i += 1;
            }
            '$' => {
                tokens.push(Token::Dollar);
                i += 1;
            }
            ':' => {
                // A single ':' is the definition separator
                // (`def &name: body;`); '::'/':::'/'::;' are the
                // projection family.
                if at(i + 1) != Some(':') {
                    tokens.push(Token::Colon);
                    i += 1;
                    continue;
                }
                match at(i + 2) {
                    Some(':') => {
                        tokens.push(Token::ColonColonColon);
                        i += 3;
                    }
                    Some(';') => {
                        tokens.push(Token::ColonColonSemi);
                        i += 3;
                    }
                    _ => {
                        tokens.push(Token::ColonColon);
                        i += 2;
                    }
                }
            }
            '|' if at(i + 1) == Some('|') => {
                tokens.push(Token::PipePipe);
                i += 2;
            }
            '|' => {
                tokens.push(Token::Pipe);
                i += 1;
            }
            // `*=` is the substring operator. A `*` beginning a name or
            // glob is handled by the name-char arm below; `=` cannot
            // appear unescaped in a name, so this never steals one.
            '*' if at(i + 1) == Some('=') => {
                tokens.push(Token::Contains);
                i += 2;
            }
            '=' if at(i + 1) == Some('~') => {
                tokens.push(Token::Match);
                i += 2;
            }
            // `=>` — the scalar pattern-search hop (spec: Search
            // Operators). Not implemented yet; recognized here so it
            // yields the honest message rather than lexing as `=` `>`.
            '=' if at(i + 1) == Some('>') => {
                return Err(QuarbError::Unsupported(
                    "the pattern-search operator '=>' is not implemented yet".into(),
                ));
            }
            '=' => {
                tokens.push(Token::Eq);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                i += 1;
            }
            // An `@` directly after a navigation operator, before a
            // name character, is part of the hop name (kaiv array
            // namespaces like `/@servers`). The register/context
            // sigils (`@.`, `@|`, `@*`) never follow a navigation
            // operator, so there is no clash.
            '@' if matches!(
                tokens.last(),
                Some(
                    Token::Slash | Token::SlashSlash | Token::Backslash | Token::BackslashBackslash
                )
            ) && at(i + 1).is_some_and(is_name_char) =>
            {
                let mut text = String::from("@");
                i += 1;
                while let Some(ch) = at(i) {
                    if ch == '-' && at(i + 1) == Some('>') {
                        break;
                    }
                    if is_name_char(ch) {
                        text.push(ch);
                        i += 1;
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Name {
                    text,
                    quoted: false,
                    glued,
                });
            }
            '@' => {
                tokens.push(Token::At);
                i += 1;
            }
            '%' => {
                tokens.push(Token::Percent);
                i += 1;
            }
            '&' if at(i + 1) == Some('&') => {
                tokens.push(Token::AmpAmp);
                i += 2;
            }
            '&' => {
                tokens.push(Token::Amp);
                i += 1;
            }
            ';' => {
                tokens.push(Token::Semi);
                i += 1;
            }
            '~' if at(i + 1) == Some('>') => {
                tokens.push(Token::Resolve);
                i += 2;
            }
            '~' => {
                if at(i + 1) != Some('(') {
                    return Err(QuarbError::Lex(
                        "'~' must introduce a regex '~(...)' or a resolution '~>'".into(),
                    ));
                }
                let (body, next) = read_balanced(&chars, i + 2)?;
                tokens.push(Token::Regex(body));
                i = next;
            }
            '\'' => {
                let (text, next) = read_quoted(&chars, i + 1, c)?;
                tokens.push(Token::Name {
                    text,
                    quoted: true,
                    glued,
                });
                i = next;
            }
            // Double quotes are interpolated: `${expr}` holes evaluate
            // in the current scope. A hole-free string stays a plain
            // quoted name.
            '`' => {
                let (parts, next) = read_interpolated_until(&chars, i + 1, '`')?;
                tokens.push(Token::Shell(parts));
                i = next;
            }
            '"' => {
                let (parts, next) = read_interpolated(&chars, i + 1)?;
                match parts.as_slice() {
                    [] => tokens.push(Token::Name {
                        text: String::new(),
                        quoted: true,
                        glued,
                    }),
                    [InterpPart::Text(text)] => tokens.push(Token::Name {
                        text: text.clone(),
                        quoted: true,
                        glued,
                    }),
                    _ => tokens.push(Token::Interp(parts)),
                }
                i = next;
            }
            c if is_name_char(c) => {
                let mut text = String::new();
                while let Some(ch) = at(i) {
                    // `-` ends the name if it starts a `->` crosslink.
                    if ch == '-' && at(i + 1) == Some('>') {
                        break;
                    }
                    // `*` ends the name if it starts a `*=` contains
                    // operator, so a glued `::Name*='x'` reads as key
                    // `Name` + Contains rather than key `Name*` + `=`.
                    // A leading `*=` is caught by the top-level arm.
                    if ch == '*' && at(i + 1) == Some('=') {
                        break;
                    }
                    // `.` ends a non-empty name if `(` follows: the
                    // dot opens a push/subcontext, not a name
                    // character (`CONTAINS.(::qty)` is the hop
                    // `CONTAINS` then the push `.(::qty)`). A
                    // leading dot stays (`.(expr)` itself, `.name(`).
                    // A NAMED push after a named hop needs a space
                    // (`->e .q(...)`): glued `e.q(` must stay one
                    // name, or `/x.rs(...)` would lose its filename.
                    if ch == '.' && at(i + 1) == Some('(') && !text.is_empty() {
                        break;
                    }
                    if is_name_char(ch) {
                        text.push(ch);
                        i += 1;
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Name {
                    text,
                    quoted: false,
                    glued,
                });
            }
            '{' => {
                let (min, max, next) = read_quantifier(&chars, i + 1)?;
                tokens.push(Token::Quant { min, max });
                i = next;
            }
            other => {
                return Err(QuarbError::Unsupported(format!(
                    "operator '{other}' is not implemented yet"
                )));
            }
        }
    }

    Ok(tokens)
}

/// Read a `{n}` / `{m,n}` / `{m,}` repetition quantifier after the
/// opening brace at `start`. Returns `(min, max, index past '}')`;
/// `max` is `None` for the open-ended `{m,}` form. Interior spaces
/// are tolerated (`{ 1 , 3 }`).
fn read_quantifier(chars: &[char], start: usize) -> Result<(usize, Option<usize>, usize)> {
    let malformed =
        || QuarbError::Lex("malformed quantifier '{…}': write {n}, {m,n}, or {m,}".into());
    let mut i = start;
    // Optional whitespace, optional digit run, optional whitespace.
    // Returns `None` when no digits were present.
    let number = |i: &mut usize| -> Option<usize> {
        while chars.get(*i).is_some_and(|c| c.is_whitespace()) {
            *i += 1;
        }
        let mut digits = String::new();
        while let Some(c) = chars.get(*i).filter(|c| c.is_ascii_digit()) {
            digits.push(*c);
            *i += 1;
        }
        while chars.get(*i).is_some_and(|c| c.is_whitespace()) {
            *i += 1;
        }
        digits.parse().ok()
    };
    let min = number(&mut i).ok_or_else(malformed)?;
    match chars.get(i) {
        Some('}') => Ok((min, Some(min), i + 1)),
        Some(',') => {
            i += 1;
            let max = number(&mut i);
            match chars.get(i) {
                // `{m,}` (max `None`) is the open-ended form.
                Some('}') => Ok((min, max, i + 1)),
                _ => Err(malformed()),
            }
        }
        _ => Err(malformed()),
    }
}

/// Read a double-quoted, interpolated string after the opening quote
/// at `start`: `${...}` opens a hole holding an expression (lexed and
/// parsed later, at parse time); `\$`, `\"`, and `\\` escape. Returns
/// the alternating text/hole parts and the index past the closing
/// quote.
fn read_interpolated(chars: &[char], start: usize) -> Result<(Vec<InterpPart>, usize)> {
    read_interpolated_until(chars, start, '"')
}

fn read_interpolated_until(
    chars: &[char],
    start: usize,
    close: char,
) -> Result<(Vec<InterpPart>, usize)> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        match ch {
            c if c == close => {
                if !text.is_empty() {
                    parts.push(InterpPart::Text(text));
                }
                return Ok((parts, i + 1));
            }
            '\\' if matches!(chars.get(i + 1), Some('$' | '"' | '`' | '\\')) => {
                text.push(chars[i + 1]);
                i += 2;
            }
            '$' if chars.get(i + 1) == Some(&'{') => {
                if !text.is_empty() {
                    parts.push(InterpPart::Text(std::mem::take(&mut text)));
                }
                let mut hole = String::new();
                i += 2;
                // Scan to the matching `}`, honoring nested braces (a
                // `{n}` quantifier inside the hole) and single-quoted
                // strings (where `{`, `}`, and `'` are literal), so a
                // legal value expression is not truncated at the first
                // inner brace or quoted `}`.
                let mut depth = 0usize;
                loop {
                    match chars.get(i) {
                        Some('}') if depth == 0 => break,
                        Some('}') => {
                            depth -= 1;
                            hole.push('}');
                            i += 1;
                        }
                        Some('{') => {
                            depth += 1;
                            hole.push('{');
                            i += 1;
                        }
                        Some('\'') => {
                            hole.push('\'');
                            i += 1;
                            loop {
                                match chars.get(i) {
                                    Some('\'') => {
                                        hole.push('\'');
                                        i += 1;
                                        break;
                                    }
                                    Some(&c) => {
                                        hole.push(c);
                                        i += 1;
                                    }
                                    None => {
                                        return Err(QuarbError::Lex(
                                            "unterminated interpolation '${…' (missing '}')"
                                                .into(),
                                        ));
                                    }
                                }
                            }
                        }
                        Some(&c) => {
                            hole.push(c);
                            i += 1;
                        }
                        None => {
                            return Err(QuarbError::Lex(
                                "unterminated interpolation '${…' (missing '}')".into(),
                            ));
                        }
                    }
                }
                i += 1;
                if hole.trim().is_empty() {
                    return Err(QuarbError::Lex("empty interpolation '${}'".into()));
                }
                parts.push(InterpPart::Hole(hole));
            }
            _ => {
                text.push(ch);
                i += 1;
            }
        }
    }
    Err(QuarbError::Lex("unterminated quoted name (\"…)".into()))
}

/// Read a quoted name after the opening quote at `start`, up to the
/// matching `quote`. Returns the text and the index past the quote.
fn read_quoted(chars: &[char], start: usize, quote: char) -> Result<(String, usize)> {
    let mut text = String::new();
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        i += 1;
        if ch == quote {
            return Ok((text, i));
        }
        text.push(ch);
    }
    Err(QuarbError::Lex(format!(
        "unterminated quoted name ({quote}…)"
    )))
}

/// Read a regex body after `~(` starting at `start`, up to the
/// matching `)`, honoring nested parentheses, backslash escapes, and
/// `[...]` character classes (where parens are literal). Returns the
/// body and the index past the closing `)`.
fn read_balanced(chars: &[char], start: usize) -> Result<(String, usize)> {
    let mut body = String::new();
    let mut depth = 1usize;
    let mut in_class = false;
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        i += 1;
        match ch {
            // A backslash escapes the next character verbatim, so an
            // escaped paren or bracket (`\)`, `\(`, `\]`) neither
            // closes the group nor toggles a character class.
            '\\' => {
                body.push('\\');
                if let Some(&next) = chars.get(i) {
                    body.push(next);
                    i += 1;
                }
            }
            // Inside a `[...]` character class, parens are literal;
            // only an unescaped `]` closes the class.
            '[' if !in_class => {
                in_class = true;
                body.push('[');
            }
            ']' if in_class => {
                in_class = false;
                body.push(']');
            }
            '(' if !in_class => {
                depth += 1;
                body.push('(');
            }
            ')' if !in_class => {
                depth -= 1;
                if depth == 0 {
                    return Ok((body, i));
                }
                body.push(')');
            }
            _ => body.push(ch),
        }
    }
    Err(QuarbError::Lex("unterminated regex '~(…'".into()))
}

#[cfg(test)]
mod quant_tests {
    use super::*;

    #[test]
    fn quantifier_forms() {
        let quant = |src: &str| match lex(src).unwrap().as_slice() {
            [Token::Quant { min, max }] => (*min, *max),
            other => panic!("expected a single Quant, got {other:?}"),
        };
        assert_eq!(quant("{2}"), (2, Some(2)));
        assert_eq!(quant("{1,3}"), (1, Some(3)));
        assert_eq!(quant("{2,}"), (2, None));
        assert_eq!(quant("{ 1 , 3 }"), (1, Some(3)));
        assert_eq!(quant("{0,4}"), (0, Some(4)));
    }

    #[test]
    fn quantifier_in_context() {
        let toks = lex("/{2}").unwrap();
        assert_eq!(
            toks,
            vec![Token::Slash, Token::Quant { min: 2, max: Some(2) }]
        );
        // `+` and `*` after `)` stay name characters — the parser
        // reads them as quantifier suffixes by position, not the
        // lexer.
        let toks = lex("(/a)+?").unwrap();
        assert!(matches!(
            toks.as_slice(),
            [
                Token::LParen,
                Token::Slash,
                Token::Name { .. },
                Token::RParen,
                Token::Name { text, glued: true, .. },
                Token::Question,
            ] if text == "+"
        ));
    }

    #[test]
    fn malformed_quantifiers() {
        for src in ["{}", "{,3}", "{a}", "{1,2", "{1;2}"] {
            assert!(lex(src).is_err(), "{src} should not lex");
        }
        // A stray closing brace stays unsupported.
        assert!(lex("}").is_err());
    }
}

#[cfg(test)]
mod subst_tests {
    use super::*;

    #[test]
    fn subst_lexes_in_pipeline_position() {
        let toks = lex("| s/foo/bar/g").unwrap();
        assert!(
            matches!(
                &toks[1],
                Token::Subst { pattern, replacement, mods }
                    if pattern == "foo" && replacement == "bar" && mods == "g"
            ),
            "got {toks:?}"
        );
        // escaped slash
        let toks = lex(r"| s/a\/b/x/").unwrap();
        assert!(matches!(&toks[1], Token::Subst { pattern, .. } if pattern == "a/b"));
        // not in path position
        let toks = lex("/s/x").unwrap();
        assert!(toks.iter().all(|t| !matches!(t, Token::Subst { .. })));
    }
}

#[cfg(test)]
mod glued_operator_tests {
    use super::*;

    #[test]
    fn lt_before_negative_number_is_not_crosslink() {
        // Glued `<-3` is `< -3` (a less-than against a negative
        // literal), matching the spaced form, not the incoming
        // crosslink `<-`.
        let toks = lex("[::a<-3]").unwrap();
        assert!(
            matches!(
                toks.as_slice(),
                [
                    Token::LBracket,
                    Token::ColonColon,
                    Token::Name { text: a, .. },
                    Token::Lt,
                    Token::Name { text: n, .. },
                    Token::RBracket,
                ] if a == "a" && n == "-3"
            ),
            "got {toks:?}"
        );
        // A non-digit target still lexes as the crosslink.
        assert!(lex("a<-b").unwrap().contains(&Token::ArrowIn));
    }

    #[test]
    fn glued_contains_after_projection_key() {
        // `::Name*='x'` is key `Name` + Contains, not key `Name*` + Eq.
        let toks = lex("[::Name*='Countess']").unwrap();
        assert!(
            matches!(
                toks.as_slice(),
                [
                    Token::LBracket,
                    Token::ColonColon,
                    Token::Name { text: k, .. },
                    Token::Contains,
                    Token::Name { text: v, .. },
                    Token::RBracket,
                ] if k == "Name" && v == "Countess"
            ),
            "got {toks:?}"
        );
        // A `*` mid-name that is not part of `*=` stays a name char.
        let toks = lex("a*b").unwrap();
        assert!(matches!(toks.as_slice(), [Token::Name { text, .. }] if text == "a*b"));
    }

    #[test]
    fn interpolation_hole_honors_braces_and_quotes() {
        let hole = |src: &str| match lex(src).unwrap().as_slice() {
            [Token::Interp(parts)] => match parts.as_slice() {
                [InterpPart::Hole(h)] => h.clone(),
                other => panic!("expected a single hole, got {other:?}"),
            },
            other => panic!("expected a single Interp, got {other:?}"),
        };
        // A `{2}` quantifier inside the hole must not close it early.
        assert_eq!(hole("\"${(/a){2}::v}\""), "(/a){2}::v");
        // A `}` inside a single-quoted string is literal, not a close.
        assert_eq!(hole("\"${(::t = '}' ? 1 : 0)}\""), "(::t = '}' ? 1 : 0)");
    }

    #[test]
    fn regex_honors_escapes_and_char_classes() {
        let body = |src: &str| match lex(src).unwrap().as_slice() {
            [Token::Regex(b)] => b.clone(),
            other => panic!("expected a regex, got {other:?}"),
        };
        // An escaped `)` does not close the group early.
        assert_eq!(body("~(.*\\))"), ".*\\)");
        // A `)` inside a character class is literal.
        assert_eq!(body("~([)])"), "[)]");
        // A plain nested group still balances as before.
        assert_eq!(body("~((ab)+)"), "(ab)+");
    }
}

#[cfg(test)]
mod regex_and_search_tests {
    use super::*;

    #[test]
    fn regex_modifiers_fold_into_inline_flags() {
        // `/pat/imsx` after `=~` folds the trailing modifier letters
        // into an inline flag group, so every regex flavor honors them
        // without a separate build path.
        let toks = lex("[::x =~ /admin/i]").unwrap();
        assert!(
            toks.contains(&Token::Regex("(?i)admin".into())),
            "got {toks:?}"
        );
        let toks = lex("[::x =~ /a.b/ms]").unwrap();
        assert!(
            toks.contains(&Token::Regex("(?ms)a.b".into())),
            "got {toks:?}"
        );
        // No modifiers: the body is left bare.
        let toks = lex("[::x =~ /plain/]").unwrap();
        assert!(toks.contains(&Token::Regex("plain".into())), "got {toks:?}");
    }

    #[test]
    fn arrow_search_operator_is_unsupported_not_eq_gt() {
        // `=>` is the (unimplemented) pattern-search hop; it reports
        // that honestly rather than lexing as `=` then `>`.
        let err = lex("//a => b").unwrap_err();
        assert!(matches!(err, QuarbError::Unsupported(_)), "got {err:?}");
    }
}
