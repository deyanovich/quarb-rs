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
//!
//! Typing-friendly aliases — the rounded syntax — resolve here, so
//! the parser sees one syntax: the tail-colon arrows `:-` / `:--` /
//! `-:` / `--:` lex as `->` / `-->` / `<-` / `<--`, the semicolon
//! sibling hops `;-` / `-;` (reach forms `;;-` / `-;;`) as `>` / `<`
//! (`>>` / `<<`), the file-system ascent `../` / `..//` as `\` / `\\`,
//! the quantifier `(;m,n)` as `{m,n}`, and the rounded predicate
//! `(?…)` and trait `(:…)` as `[…]` and `<…>` — the lexer keeps a
//! stack of open brackets so the `)` that balances a `(?` or `(:`
//! comes out as `]` or `>`.

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
    /// `;-` — the next-sibling hop's rounded spelling. A token of
    /// its own, not `Gt`: the alias names the hop only, never the
    /// comparison `>` (which gets its own rounded form).
    NextSibling,
    /// `-;` — the previous-sibling hop's rounded spelling (see
    /// [`Token::NextSibling`]).
    PrevSibling,
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
    /// `^` — root anchor; or its rounded spelling, the empty double
    /// pair `(())`.
    Caret,
    /// `$` — leaf anchor.
    Dollar,
    /// `::` — property projection.
    ColonColon,
    /// `:::` — core-metadata projection.
    ColonColonColon,
    /// `::::` — adapter-metadata projection (canonical: the
    /// projection ladder by colon count). The historical spellings
    /// `;;;` and `::;` are accepted as deprecated aliases and
    /// canonicalize on unparse.
    SemiSemiSemi,
    /// `|` — pipe / trait alternation.
    Pipe,
    /// `||` — union.
    PipePipe,
    /// `[` — or the rounded predicate opener `(?`, its permanent
    /// typing-friendly alias (canonicalizes on unparse).
    LBracket,
    /// `]` — or the `)` that balances a `(?`.
    RBracket,
    // `Lt` / `Gt` likewise stand for the rounded trait `(:…)`: the
    // opener lexes as `<`, the `)` that balances it as `>`.
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
    /// `--` — either-direction crosslink (the headless arrow).
    DashDash,
    /// `@` — register/aggregation sigil.
    At,
    /// `%` — the record sigil (`%.`, the named register view).
    Percent,
    /// `&` — the fragment sigil (`&name` invokes a `def`).
    Amp,
    /// `&&` — trait conjunction.
    AmpAmp,
    /// `:` — the definition separator (`def &name: body;`), or the
    /// conditional's else; spaced on its right.
    Colon,
    /// `:name` — a record's field (ruling #48): the single colon
    /// glued to the field name, the bottom rung of the colon ladder.
    Field,
    /// `%+` — the named-captures record.
    PercentPlus,
    /// `;` — the statement terminator (after a `def` body).
    Semi,
    /// `<=>` — correlation operator.
    Correlate,
    /// `-->` — cross-reference resolution (`-->` is the deprecated
    /// alias; both lex to this token and unparse canonically).
    Resolve,
    /// `<--` — reverse cross-reference resolution (`<--` is the
    /// deprecated alias).
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

/// Whether `text` is the date-and-time head of an ISO instant —
/// `YYYY-MM-DDT` followed only by time characters. The gate that
/// lets the word lexer keep a time's colons.
fn is_iso_datetime_prefix(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
        && b[11..]
            .iter()
            .all(|c| c.is_ascii_digit() || matches!(c, b':' | b'.' | b'+' | b'-' | b'Z'))
}

/// An open bracket the lexer is inside, so a `)` knows whether it
/// balances a plain paren or a rounded predicate `(?`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opener {
    /// `(`
    Paren,
    /// `(?` — closes as `]`.
    RoundedPredicate,
    /// `(:` — closes as `>`.
    RoundedTrait,
    /// `[`
    Bracket,
}

/// Whether a dash at this point is the edge accessor's own name —
/// glued to a preceding `$` or `@` (`$-`, `@-`) — rather than the
/// start of a rounded arrow or sibling hop.
fn after_accessor(tokens: &[Token], glued: bool) -> bool {
    glued && matches!(tokens.last(), Some(Token::Dollar | Token::At))
}

/// Scan a rounded register or anchor unit opening at `i` (ruling
/// #43). Single parentheses hold the value side, double the node
/// side; the content is a run of name characters (or `@`) closed by
/// the matching parens, and any other content leaves the parens
/// ordinary. Returns the pointy token shape and the index past the
/// unit — the parser sees exactly what the sigil spelling lexes to.
fn paren_unit(chars: &[char], i: usize) -> Option<(Vec<Token>, usize)> {
    let name = |t: &str| Token::Name {
        text: t.to_string(),
        quoted: false,
        glued: true,
    };
    let double = chars.get(i + 1) == Some(&'(');
    let start = if double { i + 2 } else { i + 1 };
    let mut j = start;
    while chars
        .get(j)
        .is_some_and(|&c| is_name_char(c) || c == '@')
    {
        j += 1;
    }
    let body: String = chars[start..j].iter().collect();
    if double {
        if chars.get(j) != Some(&')') || chars.get(j + 1) != Some(&')') {
            return None;
        }
        let toks = match body.as_str() {
            "" => vec![Token::Caret],
            "_" => vec![Token::Dollar, Token::Dollar],
            "*" | "@" => vec![Token::LParen, Token::At, Token::RParen],
            b if b.starts_with('*') || b.starts_with('@') => {
                vec![Token::LParen, Token::At, name(&b[1..]), Token::RParen]
            }
            b => vec![Token::LParen, name(b), Token::RParen],
        };
        return Some((toks, j + 2));
    }
    if chars.get(j) != Some(&')') {
        return None;
    }
    let toks = match body.as_str() {
        "_" | "-" | "." => vec![Token::Dollar, name(&body)],
        "*" => vec![Token::At, name("*")],
        "*." => vec![Token::At, name(".")],
        "*-" => vec![Token::At, name("-")],
        // `(.name)` — the register pushed by `.name`; `(.N)` the
        // N-th register (a float is written `0.5`, never `(.5)`).
        b if b.starts_with('.') => vec![Token::Dollar, name(b)],
        // `(*N)` — the N-th witness.
        b if b.starts_with('*') && b[1..].bytes().all(|c| c.is_ascii_digit()) => {
            vec![Token::Dollar, name(b)]
        }
        // `(N)` — the N-th match capture `$N` (a value; the mark at
        // position N is `((N))`).
        b if !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit()) => {
            vec![Token::Dollar, name(b)]
        }
        // A bare name is a mark anchor's deprecated single-paren
        // spelling, or a group — the parser's call.
        _ => return None,
    };
    Some((toks, j + 1))
}

/// Whether the next token sits in name position — right after a
/// navigation axis (or its reach mark), where a regex literal names
/// the hop and takes no trailing modifiers.
/// Whether char position `i` follows whitespace (or is the start
/// of the input): the constructor sigils `%(`, `@(`, `*(` stand
/// only there, or after a push dot (ruling #53). Char-indexed, as
/// the lexer is — a byte slice would misread after any non-ASCII
/// character.
fn after_whitespace(chars: &[char], i: usize) -> bool {
    i == 0 || chars[i - 1].is_whitespace()
}

/// Whether char position `i` follows a push dot — `.` or `.name` —
/// the record push's glue (`.%(…)`, `.r%(…)`).
fn after_push_dot(chars: &[char], i: usize) -> bool {
    let mut k = i;
    while k > 0 && is_name_char(chars[k - 1]) && chars[k - 1] != '.' {
        k -= 1;
    }
    k > 0 && chars[k - 1] == '.' && !(k > 1 && chars[k - 2] == '.')
}

fn after_axis(tokens: &[Token]) -> bool {
    let axis = |t: &Token| {
        matches!(
            t,
            Token::Slash
                | Token::SlashSlash
                | Token::Backslash
                | Token::BackslashBackslash
                | Token::Gt
                | Token::Lt
                | Token::NextSibling
                | Token::PrevSibling
                | Token::FollowingSiblings(_)
                | Token::PrecedingSiblings(_)
                | Token::ArrowOut
                | Token::ArrowIn
                | Token::DashDash
                | Token::Resolve
                | Token::ReverseResolve
        )
    };
    match tokens.last() {
        Some(t) if axis(t) => true,
        Some(Token::Question | Token::Bang) => tokens.len() >= 2 && axis(&tokens[tokens.len() - 2]),
        _ => false,
    }
}

/// Read a `(/…/)` regex literal opening at `i`: the body up to a
/// closing slash that is followed (when `mods` is allowed) by Perl
/// modifiers and the closing paren; any other slash is literal, and
/// `\/` is a literal slash. Modifiers fold into an inline flag
/// group, as after `=~`. Returns the body and the index past the
/// paren, or `None` when no such closer exists (the parens are then
/// ordinary).
fn regex_paren(chars: &[char], i: usize, mods: bool) -> Option<(String, usize)> {
    let mut body = String::new();
    let mut j = i + 2;
    while let Some(&c) = chars.get(j) {
        match c {
            '\\' if chars.get(j + 1) == Some(&'/') => {
                body.push('/');
                j += 2;
            }
            '\\' => {
                body.push('\\');
                if let Some(&n) = chars.get(j + 1) {
                    body.push(n);
                }
                j += 2;
            }
            '/' => {
                let mut k = j + 1;
                let mut flags = String::new();
                if mods {
                    while let Some(&m @ ('i' | 'm' | 's' | 'x')) = chars.get(k) {
                        flags.push(m);
                        k += 1;
                    }
                }
                if chars.get(k) == Some(&')') {
                    let body = if flags.is_empty() {
                        body
                    } else {
                        format!("(?{flags}){body}")
                    };
                    return Some((body, k + 1));
                }
                body.push('/');
                j += 1;
            }
            _ => {
                body.push(c);
                j += 1;
            }
        }
    }
    None
}

/// Whether an opening paren here belongs to what precedes it — a
/// glued name (a call: `now()`, `sh()`) or a glued sigil (`%()`,
/// `&f()`) — rather than opening something of its own.
fn after_call_head(tokens: &[Token], glued: bool) -> bool {
    glued
        && matches!(
            tokens.last(),
            Some(Token::Name { .. } | Token::Percent | Token::Amp | Token::Dollar | Token::At)
        )
}

/// Tokenize `input` into the subset's token stream.
pub fn lex(input: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut openers: Vec<Opener> = Vec::new();
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
            // `-->` — resolution, canonical spelling: one dash
            // more than the direct edge, for the extra step through
            // the value. Longest match: before `--`.
            '-' if at(i + 1) == Some('-') && at(i + 2) == Some('>') => {
                tokens.push(Token::Resolve);
                i += 3;
            }
            // `__` — the pipe, lying down (ruling #43): a standalone
            // double underscore is `|`; glued to name characters it
            // stays a name (`x__y`, `__init__`). `*__` and `,__` are
            // the aggregation and map pipes — all, then; each, then
            // — whole spellings, since `@` and `$` are not on the
            // layouts that lack `|`.
            '_' if at(i + 1) == Some('_') && !at(i + 2).is_some_and(is_name_char) => {
                tokens.push(Token::Pipe);
                i += 2;
            }
            '*' if at(i + 1) == Some('_')
                && at(i + 2) == Some('_')
                && !at(i + 3).is_some_and(is_name_char) =>
            {
                tokens.push(Token::At);
                tokens.push(Token::Pipe);
                i += 3;
            }
            // `*(a; b)` — the rounded list literal (`*` stands in for
            // `@`, as in `*__` and `(*)`). A list literal follows
            // whitespace (rulings #52/#53), so after an axis the star
            // is the wildcard (`/*(?…)`) and glued to anything it is
            // what it was.
            '*' if at(i + 1) == Some('(') && after_whitespace(&chars, i) => {
                tokens.push(Token::At);
                i += 1;
            }
            // `@(a; b)` — the list literal — follows whitespace.
            '@' if at(i + 1) == Some('(') => {
                if !after_whitespace(&chars, i) {
                    return Err(QuarbError::Parse(
                        "a list literal follows whitespace: write ' @(…)'".into(),
                    ));
                }
                tokens.push(Token::At);
                i += 1;
            }
            // `%(…)` / `%%(…)` — the record sigil — follows whitespace
            // or the push dot (`.%(…)`, `.name%(…)`); glued to anything
            // else it is refused (ruling #53). The paren lexes on its
            // own, as for any call.
            '%' if matches!((at(i + 1), at(i + 2)), (Some('('), _) | (Some('%'), Some('('))) => {
                if !after_whitespace(&chars, i) && !after_push_dot(&chars, i) {
                    return Err(QuarbError::Parse(
                        "a record sigil follows whitespace or a push dot: write ' %(…)' or '.name%(…)'".into(),
                    ));
                }
                tokens.push(Token::Percent);
                i += 1;
                if at(i) == Some('%') {
                    tokens.push(Token::Percent);
                    i += 1;
                }
            }
            ',' if at(i + 1) == Some('_')
                && at(i + 2) == Some('_')
                && !at(i + 3).is_some_and(is_name_char) =>
            {
                tokens.push(Token::Dollar);
                tokens.push(Token::Pipe);
                i += 3;
            }
            // `:=:` — the correlation operator's rounded spelling,
            // mirrored like the arrows (`:=:?` for the outer form:
            // the mark follows as its own token, as after `<=>`).
            ':' if at(i + 1) == Some('=') && at(i + 2) == Some(':') => {
                tokens.push(Token::Correlate);
                i += 3;
            }
            // `--:` / `-:` — the tail-colon spellings of `<--` and
            // `<-` (the colon marks the arrow's tail, mirrored):
            // permanent typing-friendly aliases. Two guards: after
            // the edge accessors `$-` / `@-` the dash is the
            // accessor's own name (`$-::prop`, `$-;`), and a colon
            // run (`-::`) is a projection after a name that ends in
            // a dash — an arrow's label is never a colon.
            '-' if at(i + 1) == Some('-')
                && at(i + 2) == Some(':')
                && at(i + 3) != Some(':')
                && !after_accessor(&tokens, glued) =>
            {
                tokens.push(Token::ReverseResolve);
                i += 3;
            }
            // `--` — the headless arrow — carved out of the dash
            // run like `->`; the name lexer breaks on it too, so a
            // glued `/posts/5--ip` reads as row 5 then the hop.
            '-' if at(i + 1) == Some('-') => {
                tokens.push(Token::DashDash);
                i += 2;
            }
            '-' if at(i + 1) == Some(':')
                && at(i + 2) != Some(':')
                && !after_accessor(&tokens, glued) =>
            {
                tokens.push(Token::ArrowIn);
                i += 2;
            }
            // `-;;` / `-;` — the preceding-siblings reach and the
            // previous-sibling hop, rounded: the sibling hops live
            // in the semicolon family (the dash points at the
            // sibling, the semicolon marks the axis). Same accessor
            // guard as the arrows.
            '-' if at(i + 1) == Some(';')
                && at(i + 2) == Some(';')
                && !after_accessor(&tokens, glued) =>
            {
                let mark = match at(i + 3) {
                    Some(m @ ('?' | '!')) => {
                        i += 1;
                        m
                    }
                    _ => ' ',
                };
                tokens.push(Token::PrecedingSiblings(mark));
                i += 3;
            }
            '-' if at(i + 1) == Some(';') && !after_accessor(&tokens, glued) => {
                tokens.push(Token::PrevSibling);
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
            // `<--` — reverse resolution, canonical spelling.
            // Longest match: before `<-`; the digit guard mirrors
            // `<-`'s so `<--3` stays a comparison shape, not a hop.
            '<' if at(i + 1) == Some('-')
                && at(i + 2) == Some('-')
                && !at(i + 3).is_some_and(|c| c.is_ascii_digit()) =>
            {
                tokens.push(Token::ReverseResolve);
                i += 3;
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
                if matches!(openers.last(), Some(Opener::RoundedTrait)) {
                    return Err(QuarbError::Lex(
                        "a '(:' trait closes with ')', not '>'".into(),
                    ));
                }
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
            // ':-' / ':--' — the tail-colon spellings of '->' and
            // '-->' (the colon marks the arrow's tail): permanent
            // typing-friendly aliases, canonicalizing on unparse.
            // The digit guard mirrors '<-': a conditional's glued
            // else-branch negative (': -3') stays a colon + number.
            ':' if at(i + 1) == Some('-')
                && at(i + 2) == Some('-')
                && !at(i + 3).is_some_and(|c| c.is_ascii_digit()) =>
            {
                tokens.push(Token::Resolve);
                i += 3;
            }
            ':' if at(i + 1) == Some('-')
                && at(i + 2) != Some('-')
                && !at(i + 2).is_some_and(|c| c.is_ascii_digit()) =>
            {
                tokens.push(Token::ArrowOut);
                i += 2;
            }
            // `:name` — a record's field (ruling #48): a single
            // colon glued to a name on its right. Spaced on the
            // right, a colon is the else of a conditional or a
            // definition's separator, as ever (`? 2 : 0`, `: -3`).
            ':' if at(i + 1).is_some_and(|c| c.is_alphabetic() || c == '_') => {
                tokens.push(Token::Field);
                i += 1;
            }
            ':' => {
                // A single ':' is the definition separator
                // (`def &name: body;`); '::'/':::'/'::::' are the
                // projection ladder ('::;' and ';;;' lex as the
                // deprecated aliases of '::::').
                if at(i + 1) != Some(':') {
                    tokens.push(Token::Colon);
                    i += 1;
                    continue;
                }
                match at(i + 2) {
                    // '::::' — adapter metadata, canonical: the
                    // projection ladder by colon count (more colons,
                    // more meta). Longest match before ':::'.
                    Some(':') if at(i + 3) == Some(':') => {
                        tokens.push(Token::SemiSemiSemi);
                        i += 4;
                    }
                    Some(':') => {
                        tokens.push(Token::ColonColonColon);
                        i += 3;
                    }
                    // The deprecated alias claims '::;' only when a
                    // metadata key follows; otherwise the ';' is a
                    // def separator after a bare '::' projection
                    // (`def &f: /x::; &f`).
                    Some(';')
                        if at(i + 3).is_some_and(|c| {
                            c.is_alphanumeric() || c == '_' || c == '\'' || c == '"'
                        }) =>
                    {
                        tokens.push(Token::SemiSemiSemi);
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
                openers.push(Opener::Bracket);
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                match openers.last() {
                    Some(Opener::Bracket) => {
                        openers.pop();
                    }
                    // `(?…]` would otherwise pass the parser as a
                    // well-formed predicate; the two spellings do
                    // not mix within one pair.
                    Some(Opener::RoundedPredicate) => {
                        return Err(QuarbError::Lex(
                            "a '(?' predicate closes with ')', not ']'".into(),
                        ));
                    }
                    Some(Opener::RoundedTrait) => {
                        return Err(QuarbError::Lex(
                            "a '(:' trait closes with ')', not ']'".into(),
                        ));
                    }
                    _ => {}
                }
                tokens.push(Token::RBracket);
                i += 1;
            }
            // `(?` — the rounded predicate: a permanent typing-
            // friendly alias of `[` for keyboard layouts where the
            // square brackets are costly (the Russian layout has
            // none). The digraph lexes as `[` and the `)` that
            // balances it as `]`, so the parser sees one predicate
            // syntax and unparse prints the canonical brackets —
            // the index forms included: an index is a positional
            // predicate, one bracket family. The digraph is never
            // two tokens today: no expression
            // begins with `?`, and the conditional's `?=` always
            // follows an operand. Regex bodies (`~(?i)…`) never
            // reach this arm — `read_balanced` takes them raw.
            // The rounded register and anchor families (ruling
            // #43): a parenthesized symbol is the value side —
            // `(_)` `(.)` `(.name)` `(*)` `(*.)` `(*-)` `(*N)` `(-)`
            // for `$_` `$.` `$.name` `@*` `@.` `@-` `$*N` `$-` —
            // and double parentheses the node side — `(())` the
            // root, `((_))` the driver `$$`, `((name))` `((N))`
            // `((.))` `((@))` `((@name))` the mark anchors (also
            // `((*))` `((*name))`). Glued to a call head the outer
            // paren is the call's (`f((_))` passes the topic).
            '(' if !after_call_head(&tokens, glued)
                && let Some((unit, next)) = paren_unit(&chars, i) =>
            {
                tokens.extend(unit);
                i = next;
            }
            // `(/…/)` — the regex literal (ruling #44), canonical in
            // both positions. In name position it closes strictly
            // with `/)` (flags inline: `(/(?i)^foo/)`), so a pattern
            // group ending in a hop named i/m/s/x is never misread;
            // in operand position the Perl modifiers follow the
            // closing slash (`(/^bob/i)`). `\/` is a literal slash.
            // Glued to a call head the paren is the call's (`f(/x/)`
            // passes a path). The bare `/…/` after `=~` / `!~` and
            // the tilde wrapper `~(…)` remain as sugar and alias.
            '(' if at(i + 1) == Some('/')
                && !after_call_head(&tokens, glued)
                && let Some((body, next)) = regex_paren(&chars, i, !after_axis(&tokens)) =>
            {
                tokens.push(Token::Regex(body));
                i = next;
            }
            '(' if at(i + 1) == Some('?') => {
                openers.push(Opener::RoundedPredicate);
                tokens.push(Token::LBracket);
                i += 2;
            }
            // `(:` — the rounded trait, `<…>`'s alias on the same
            // model: the digraph lexes as `<`, the balancing `)` as
            // `>`. A trait body never begins with a colon or a dash,
            // so `(::a = 1)` stays a group opening on a projection
            // and `(:-p|…)` a group opening on the `:-` arrow. A
            // trait sits on a hop name — one that follows an axis —
            // so glued to any other call head (`sort_by(:size)`,
            // `%(:name)`) the paren is the call's, opening on a
            // field of the topic record.
            '(' if at(i + 1) == Some(':')
                && !matches!(at(i + 2), Some(':' | '-'))
                && !(after_call_head(&tokens, glued) && !after_axis(&tokens[..tokens.len() - 1])) =>
            {
                openers.push(Opener::RoundedTrait);
                tokens.push(Token::Lt);
                i += 2;
            }
            // `(;m,n)` — the rounded quantifier, `{m,n}`'s alias:
            // read whole, like the braces.
            '(' if at(i + 1) == Some(';') => {
                let (min, max, next) = read_quantifier(&chars, i + 2, ')')?;
                tokens.push(Token::Quant { min, max });
                i = next;
            }
            '(' => {
                openers.push(Opener::Paren);
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                // Balance the innermost paren-family opener; a
                // rounded predicate closes as `]`. A `)` with no
                // paren open (or a `[` on top) stays a plain `)`
                // for the parser to report.
                match openers.last() {
                    Some(Opener::RoundedPredicate) => {
                        openers.pop();
                        tokens.push(Token::RBracket);
                    }
                    Some(Opener::RoundedTrait) => {
                        openers.pop();
                        tokens.push(Token::Gt);
                    }
                    Some(Opener::Paren) => {
                        openers.pop();
                        tokens.push(Token::RParen);
                    }
                    _ => tokens.push(Token::RParen),
                }
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
                    if ch == '-' && matches!(at(i + 1), Some('>') | Some('-')) {
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
            '%' if at(i + 1) == Some('+') => {
                tokens.push(Token::PercentPlus);
                i += 2;
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
            ';' if at(i + 1) == Some(';') && at(i + 2) == Some(';') => {
                tokens.push(Token::SemiSemiSemi);
                i += 3;
            }
            // `;;-` / `;-` — the following-siblings reach and the
            // next-sibling hop, rounded (`>>` / `>`).
            ';' if at(i + 1) == Some(';') && at(i + 2) == Some('-') => {
                let mark = match at(i + 3) {
                    Some(m @ ('?' | '!')) => {
                        i += 1;
                        m
                    }
                    _ => ' ',
                };
                tokens.push(Token::FollowingSiblings(mark));
                i += 3;
            }
            ';' if at(i + 1) == Some('-') => {
                tokens.push(Token::NextSibling);
                i += 2;
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
                        "'~' must introduce a regex '~(...)' or a resolution '-->'".into(),
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
            // `../` / `..//` — the file-system idiom for the
            // ascending hops `\` / `\\` (the reach marks `?` / `!`
            // follow as their own tokens, as after `\\`). A dot run
            // is otherwise a name (`..3` in a range, `...` the
            // spread), so only `..` directly before `/` is the hop;
            // the name lexer breaks on the same shape.
            '.' if at(i + 1) == Some('.') && at(i + 2) == Some('/') => {
                if at(i + 3) == Some('/') {
                    tokens.push(Token::BackslashBackslash);
                    i += 4;
                } else {
                    tokens.push(Token::Backslash);
                    i += 3;
                }
            }
            c if is_name_char(c) => {
                let mut text = String::new();
                while let Some(ch) = at(i) {
                    // `-` ends the name if it starts a `->` or `--`
                    // crosslink. A name with a literal double hyphen
                    // stays reachable quoted, as with `->`.
                    if ch == '-' && matches!(at(i + 1), Some('>') | Some('-')) {
                        break;
                    }
                    // Likewise the rounded `-:` (arrow) and `-;`
                    // (sibling hop) — but a dash before a colon run
                    // stays a name character (`x-::prop`), and a
                    // leading dash is the accessor case the arms
                    // above already settled (`$-::prop`).
                    if ch == '-'
                        && !text.is_empty()
                        && (at(i + 1) == Some(';')
                            || (at(i + 1) == Some(':') && at(i + 2) != Some(':')))
                    {
                        break;
                    }
                    // `..` before `/` ends a name: the ascending
                    // hop `../` (`a../b` is `a\b`). A longer dot run
                    // (`.../`) is the spread, kept whole.
                    if ch == '.'
                        && !text.is_empty()
                        && !text.ends_with('.')
                        && at(i + 1) == Some('.')
                        && at(i + 2) == Some('/')
                    {
                        break;
                    }
                    // `*` ends the name if it starts a `*=` contains
                    // operator, so a glued `::Name*='x'` reads as key
                    // `Name` + Contains rather than key `Name*` + `=`.
                    // A leading `*=` is caught by the top-level arm.
                    if ch == '*' && at(i + 1) == Some('=') {
                        break;
                    }
                    // A glued dot is a name character, always
                    // (`/x.rs(...)` keeps its filename, `b.m` is a
                    // name): the push dot stands after whitespace
                    // (ruling #45), so no `.(` or `.name(` ever
                    // splits a name.
                    // A full ISO instant keeps its colons: once the
                    // text reads `YYYY-MM-DDT…`, a `:` followed by a
                    // digit is a time separator, not a projection —
                    // so `[::at > 2026-07-25T14:16:10Z]` lexes as one
                    // literal (the temporal reading does the rest).
                    if ch == ':'
                        && is_iso_datetime_prefix(&text)
                        && at(i + 1).is_some_and(|c| c.is_ascii_digit())
                    {
                        text.push(ch);
                        i += 1;
                        continue;
                    }
                    if is_name_char(ch) {
                        text.push(ch);
                        i += 1;
                    } else {
                        break;
                    }
                }
                // A push dot is never glued to a name character
                // (ruling #45): `x.(…)` reads as the name `x.` and
                // a paren, which is never meant — refuse it with the
                // fix. (`x.m` and `x.rs(` are names, as ever.)
                if text.len() > 1
                    && text.ends_with('.')
                    && !text.ends_with("..")
                    && at(i) == Some('(')
                {
                    return Err(QuarbError::Lex(format!(
                        "a push dot is never glued to a name: write `{} .(…)` (glued, the dot is part of the name)",
                        &text[..text.len() - 1]
                    )));
                }
                tokens.push(Token::Name {
                    text,
                    quoted: false,
                    glued,
                });
            }
            '{' => {
                let (min, max, next) = read_quantifier(&chars, i + 1, '}')?;
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
/// opening brace at `start`, up to `close` (`}` — or `)` for the
/// rounded `(;m,n)`). Returns `(min, max, index past the closer)`;
/// `max` is `None` for the open-ended `{m,}` form. Interior spaces
/// are tolerated (`{ 1 , 3 }`).
fn read_quantifier(
    chars: &[char],
    start: usize,
    close: char,
) -> Result<(usize, Option<usize>, usize)> {
    let malformed = || {
        QuarbError::Lex(if close == ')' {
            "malformed quantifier '(;…)': write (;n), (;m,n), or (;m,)".into()
        } else {
            "malformed quantifier '{…}': write {n}, {m,n}, or {m,}".into()
        })
    };
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
        Some(&c) if c == close => Ok((min, Some(min), i + 1)),
        Some(',') => {
            i += 1;
            let max = number(&mut i);
            match chars.get(i) {
                // `{m,}` (max `None`) is the open-ended form.
                Some(&c) if c == close => Ok((min, max, i + 1)),
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
                                            "unterminated interpolation '${…' (missing '}')".into(),
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
            vec![
                Token::Slash,
                Token::Quant {
                    min: 2,
                    max: Some(2)
                }
            ]
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
    fn rounded_predicate_lexes_as_brackets() {
        // `(?…)` is `[…]`: the digraph opens, the balancing `)`
        // closes as `]`, and inner parens stay parens.
        let toks = lex("/x(?(::a > 1 || ::b) && ::c)").unwrap();
        assert_eq!(
            toks.iter()
                .filter(|t| matches!(
                    t,
                    Token::LBracket | Token::RBracket | Token::LParen | Token::RParen
                ))
                .collect::<Vec<_>>(),
            vec![
                &Token::LBracket,
                &Token::LParen,
                &Token::RParen,
                &Token::RBracket
            ],
            "got {toks:?}"
        );
        // Nested the other way round: a square predicate inside a
        // paren group inside a rounded predicate.
        let toks = lex("(?(/a[1]))").unwrap();
        assert_eq!(
            toks,
            vec![
                Token::LBracket,
                Token::LParen,
                Token::Slash,
                Token::Name {
                    text: "a".into(),
                    quoted: false,
                    glued: true,
                },
                Token::LBracket,
                Token::Name {
                    text: "1".into(),
                    quoted: false,
                    glued: true,
                },
                Token::RBracket,
                Token::RParen,
                Token::RBracket,
            ]
        );
        // The spellings do not mix within one pair.
        assert!(lex("/x(?::a = 1]").is_err());
        // An index is a positional predicate: the alias covers it.
        assert_eq!(lex("(?1)").unwrap().len(), 3);
        // A regex body is raw: `(?i)` is an inline flag, not a
        // predicate.
        assert!(matches!(
            lex("~(?i)bob").unwrap().first(),
            Some(Token::Regex(r)) if r == "?i"
        ));
        // A spaced `( ?` is still two tokens.
        assert!(lex("( ?").unwrap().contains(&Token::LParen));
    }

    #[test]
    fn rounded_family_lexes() {
        let name = |t: &str| Token::Name {
            text: t.into(),
            quoted: false,
            glued: true,
        };
        // reverse arrows: `-:` / `--:` are `<-` / `<--`
        assert!(lex("/a-:b").unwrap().contains(&Token::ArrowIn));
        assert!(lex("/a--:b").unwrap().contains(&Token::ReverseResolve));
        // the edge accessors keep their dash
        assert_eq!(
            lex("$-::w").unwrap(),
            vec![Token::Dollar, name("-"), Token::ColonColon, name("w")]
        );
        assert_eq!(lex("@-;").unwrap(), vec![Token::At, name("-"), Token::Semi]);
        // a name ending in a dash before a colon run is a projection
        assert_eq!(
            lex("/x-::p").unwrap(),
            vec![Token::Slash, name("x-"), Token::ColonColon, name("p")]
        );
        // sibling hops and reaches
        assert!(lex("/a;-b").unwrap().contains(&Token::NextSibling));
        assert!(lex("/a-;b").unwrap().contains(&Token::PrevSibling));
        assert!(lex("/a;;-?b").unwrap().contains(&Token::FollowingSiblings('?')));
        assert!(lex("/a-;;!b").unwrap().contains(&Token::PrecedingSiblings('!')));
        // ascent
        assert_eq!(lex("../x").unwrap(), vec![Token::Backslash, name("x")]);
        assert_eq!(
            lex("/a/b..//?c").unwrap(),
            vec![
                Token::Slash,
                name("a"),
                Token::Slash,
                name("b"),
                Token::BackslashBackslash,
                Token::Question,
                name("c")
            ]
        );
        // dot runs that are not the hop
        assert_eq!(lex("| ...").unwrap(), vec![Token::Pipe, Token::Name { text: "...".into(), quoted: false, glued: false }]);
        assert_eq!(lex("[2..3]").unwrap(), vec![Token::LBracket, name("2..3"), Token::RBracket]);
        // traits: `(:` opens as `<`, the balancing `)` closes as `>`
        assert_eq!(
            lex("/x(:admin && !banned)").unwrap(),
            vec![
                Token::Slash,
                name("x"),
                Token::Lt,
                name("admin"),
                Token::AmpAmp,
                Token::Bang,
                name("banned"),
                Token::Gt
            ]
        );
        assert!(matches!(lex("(::a = 1)").unwrap().as_slice(), [Token::LParen, Token::ColonColon, ..]));
        assert!(matches!(lex("(:-p)").unwrap().as_slice(), [Token::LParen, Token::ArrowOut, ..]));
        assert!(lex("(:a>").is_err());
        assert!(lex("(:a]").is_err());
        // root anchor: `(())` is `^`; a glued `()` stays a call's parens
        assert_eq!(lex("(())/x").unwrap(), vec![Token::Caret, Token::Slash, name("x")]);
        assert_eq!(lex("| (())").unwrap(), vec![Token::Pipe, Token::Caret]);
        assert_eq!(
            lex("now()").unwrap(),
            vec![
                Token::Name {
                    text: "now".into(),
                    quoted: false,
                    glued: false
                },
                Token::LParen,
                Token::RParen
            ]
        );
        assert_eq!(lex("%()").unwrap(), vec![Token::Percent, Token::LParen, Token::RParen]);
        // quantifier
        assert_eq!(lex("(;2,3)").unwrap(), vec![Token::Quant { min: 2, max: Some(3) }]);
        assert_eq!(lex("(;2)").unwrap(), vec![Token::Quant { min: 2, max: Some(2) }]);
        assert_eq!(lex("(;2,)").unwrap(), vec![Token::Quant { min: 2, max: None }]);
        assert!(lex("(;2,3}").is_err());
    }

    #[test]
    fn rounded_registers_pipes_and_correlation_lex() {
        let name = |t: &str| Token::Name {
            text: t.into(),
            quoted: false,
            glued: true,
        };
        // the value side, single parens: exactly the sigil's tokens
        for (rounded, pointy) in [
            ("(_)", "$_"),
            ("(.)", "$."),
            ("(.name)", "$.name"),
            ("(.my-reg)", "$.my-reg"),
            ("(*)", "@*"),
            ("(*.)", "@."),
            ("(*-)", "@-"),
            ("(*2)", "$*2"),
            ("(1)", "$1"),
            ("(12)", "$12"),
            ("(-)", "$-"),
            ("(_)::name", "$_::name"),
        ] {
            assert_eq!(lex(rounded).unwrap(), lex(pointy).unwrap(), "{rounded}");
        }
        // the node side, double parens: the mark anchors and the driver
        for (rounded, pointy) in [
            ("((m))/x", "(m)/x"),
            ("((2))::x", "((2))::x"),
            ("((@))/e", "(@)/e"),
            ("((*))/e", "(@)/e"),
            ("((@m))/f", "(@m)/f"),
            ("((*m))/f", "(@m)/f"),
            ("((_))::id", "$$::id"),
            ("(())//x", "^//x"),
        ] {
            assert_eq!(
                lex(rounded).unwrap(),
                lex(pointy).unwrap(),
                "{rounded} vs {pointy}"
            );
        }
        // `((.))` is the most recent mark — `(.)` itself is now `$.`
        assert_eq!(
            lex("((.))/d").unwrap(),
            vec![Token::LParen, name("."), Token::RParen, Token::Slash, name("d")]
        );
        // ordinary parens are untouched
        assert!(matches!(lex("((::a = 1 || ::b = 2) && ::c)").unwrap().as_slice(), [Token::LParen, Token::LParen, Token::ColonColon, ..]));
        assert!(matches!(lex("(-3)").unwrap().as_slice(), [Token::LParen, Token::Name { .. }, Token::RParen]));
        assert_eq!(lex("(.5)").unwrap(), lex("$.5").unwrap());
        assert_eq!(lex("f((_))").unwrap()[1..], lex("f($_)").unwrap()[1..]);
        // pipes: standalone `__`, `*__`, `,__`
        assert_eq!(lex("/a __ count").unwrap(), lex("/a | count").unwrap());
        assert_eq!(lex("/a *__ max").unwrap(), lex("/a @| max").unwrap());
        assert_eq!(lex("/a ,__ upper").unwrap(), lex("/a $| upper").unwrap());
        assert_eq!(lex("__ ...").unwrap(), lex("| ...").unwrap());
        assert!(matches!(lex("x__y").unwrap().as_slice(), [Token::Name { text, .. }] if text == "x__y"));
        assert!(matches!(lex("__init__").unwrap().as_slice(), [Token::Name { text, .. }] if text == "__init__"));
        // correlation
        assert_eq!(lex("/a :=: /b").unwrap(), lex("/a <=> /b").unwrap());
        assert_eq!(lex("/a :=:? /b").unwrap(), lex("/a <=>? /b").unwrap());
    }

    #[test]
    fn regex_literal_in_parens_lexes() {
        // name position: strict `/)` closer, flags inline
        assert_eq!(lex("//(/^foo/)").unwrap(), lex("//~(^foo)").unwrap());
        assert_eq!(lex("//(/(?i)^foo/)").unwrap(), lex("//~((?i)^foo)").unwrap());
        // a group ending in a hop named x stays a group
        assert!(matches!(
            lex("/a(/src/x)+").unwrap().as_slice(),
            [Token::Slash, Token::Name { .. }, Token::LParen, Token::Slash, Token::Name { .. }, Token::Slash, Token::Name { .. }, Token::RParen, ..]
        ));
        assert!(matches!(lex("/(/a|/b)").unwrap().as_slice(), [Token::Slash, Token::LParen, ..]));
        // operand position: Perl modifiers, same token as the sugar
        assert_eq!(lex("[::a =~ (/^bob/i)]").unwrap(), lex("[::a =~ /^bob/i]").unwrap());
        assert!(matches!(lex("[::a = (/x/)]").unwrap().as_slice(), [Token::LBracket, Token::ColonColon, Token::Name { .. }, Token::Eq, Token::Regex(r), Token::RBracket] if r == "x"));
        // an escaped slash is literal; an unescaped one before non-closer text too
        assert!(matches!(lex("//(/a\\/b/)").unwrap().as_slice(), [Token::SlashSlash, Token::Regex(r)] if r == "a/b"));
        assert!(matches!(lex("[::p =~ (/a/b/)]").unwrap().as_slice(), [.., Token::Regex(r), Token::RBracket] if r == "a/b"));
        // glued to a call head the paren is the call's
        assert!(matches!(lex("f(/x/)").unwrap().as_slice(), [Token::Name { .. }, Token::LParen, Token::Slash, ..]));
    }

    #[test]
    fn push_dot_stands_after_whitespace() {
        // glued, a dot is a name character
        assert!(matches!(lex("/x.rs(?::n = 1)").unwrap().as_slice(), [Token::Slash, Token::Name { text, .. }, Token::LBracket, ..] if text == "x.rs"));
        assert!(matches!(lex("/a/b.m").unwrap().as_slice(), [.., Token::Name { text, .. }] if text == "b.m"));
        assert!(matches!(lex("/e.q(::x)").unwrap().as_slice(), [Token::Slash, Token::Name { text, .. }, Token::LParen, ..] if text == "e.q"));
        // spaced, it is the push / mark / subcontext
        assert!(matches!(lex("/a/b .m").unwrap().as_slice(), [.., Token::Name { text, glued: false, .. }] if text == ".m"));
        assert!(matches!(lex("/x .(::y)").unwrap().as_slice(), [Token::Slash, Token::Name { .. }, Token::Name { text, .. }, Token::LParen, ..] if text == "."));
        // a name ending in a dot before a paren is refused, with the fix
        for q in ["CONTAINS.(::qty)", "/a(->e.($-::qty))+"] {
            assert!(lex(q).unwrap_err().to_string().contains("never glued"), "{q}");
        }
        // after a bracket, paren, or pipe there is nothing to glue to
        for q in ["/a[1].m", "(.(::q))+", "/a | .m", "$.name", "@.", "%.", "::.hidden", "/.git", "//?.git", "[::x = .5]", "[..3]", "| ...", "/a/b../c"] {
            assert!(lex(q).is_ok(), "{q}: {:?}", lex(q));
        }
    }

    #[test]
    fn field_colon_and_named_captures_lex() {
        assert!(matches!(lex("$.r:a").unwrap().as_slice(), [Token::Dollar, Token::Name { .. }, Token::Field, Token::Name { text, .. }] if text == "a"));
        assert!(matches!(lex("| :a").unwrap().as_slice(), [Token::Pipe, Token::Field, Token::Name { .. }]));
        assert!(matches!(lex("%+:year").unwrap().as_slice(), [Token::PercentPlus, Token::Field, Token::Name { .. }]));
        // spaced on the right, a colon is the else / the separator
        assert!(matches!(lex("? 2 : 0").unwrap().as_slice(), [Token::Question, Token::Name { .. }, Token::Colon, Token::Name { .. }]));
        assert!(matches!(lex("def &f: /x").unwrap().as_slice(), [Token::Name { .. }, Token::Amp, Token::Name { .. }, Token::Colon, ..]));
        // the ladder and the tail-colon arrows are untouched
        assert!(lex("::a").unwrap().contains(&Token::ColonColon));
        assert!(lex("/a:-b").unwrap().contains(&Token::ArrowOut));
    }

    #[test]
    fn constructor_sigils_follow_whitespace() {
        // Ruling #53: `%(`, `@(`, `*(` stand after whitespace (or the
        // push dot for the record); glued, `*` is the wildcard and
        // `%(` / `@(` refuse.
        assert!(lex("/a | %(::x)").is_ok());
        assert!(lex("/a | .r%(::x)").is_ok());
        assert!(lex("/a | .%%(::x)").is_ok());
        assert!(lex("/a | f(%(::x))").unwrap_err().to_string().contains("whitespace"));
        assert!(lex("/a | @(1; 2)").is_ok());
        assert!(lex("/a | f(@(1))").unwrap_err().to_string().contains("whitespace"));
        assert!(lex("/a | *(1; 2)").unwrap().contains(&Token::At));
        // char-indexed: a non-ASCII character earlier in the text
        // must not shift the whitespace check (article 18's `·`)
        assert!(lex("/a[:: =~ /x · y/] | %(::n)").is_ok());
        assert!(lex("/a[:: = 'é'] | @(1)").is_ok());
        // glued after an axis: the wildcard, then a rounded predicate
        let toks = lex("/*(?::x = 1)").unwrap();
        assert!(!toks.contains(&Token::At) && toks.contains(&Token::LBracket), "{toks:?}");
    }

    #[test]
    fn call_head_paren_is_not_a_trait() {
        // `f(:x)` — a call whose first argument is a field of the
        // topic record; the trait opener needs a hop name.
        assert!(matches!(
            lex("| sort_by(:size)").unwrap().as_slice(),
            [Token::Pipe, Token::Name { .. }, Token::LParen, Token::Field, Token::Name { .. }, Token::RParen]
        ));
        assert!(matches!(lex("@| group(:name)").unwrap().as_slice(), [Token::At, Token::Pipe, Token::Name { .. }, Token::LParen, Token::Field, ..]));
        assert!(matches!(lex("| %(:name, 'x')").unwrap().as_slice(), [Token::Pipe, Token::Percent, Token::LParen, Token::Field, ..]));
        // a hop name keeps its trait, on every axis
        for q in ["//user(:admin)", "/*(:admin)", "//?user(:admin)", "/a/'my node'(:t)"] {
            assert!(lex(q).unwrap().contains(&Token::Lt), "{q}");
        }
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

#[cfg(test)]
mod iso_instant_tests {
    use super::*;

    /// Full ISO instants lex as one name token — the colons are
    /// time separators once the `YYYY-MM-DDT` head is seen — and
    /// nothing else's colons are affected.
    #[test]
    fn full_iso_instants_lex_whole() {
        let toks = lex("[::at > 2026-07-25T14:16:10Z]").unwrap();
        assert!(toks.iter().any(|t| matches!(
            t, Token::Name { text, .. } if text == "2026-07-25T14:16:10Z")));
        // Offsets and fractional seconds ride along.
        let toks = lex("[::at > 2026-07-25T14:16:10.500+02:00]").unwrap();
        assert!(toks.iter().any(|t| matches!(
            t, Token::Name { text, .. } if text == "2026-07-25T14:16:10.500+02:00")));
        // Bare dates unchanged; projections unchanged.
        let toks = lex("/x[::born > 2026-01-01]::name").unwrap();
        assert!(toks.iter().any(|t| matches!(
            t, Token::Name { text, .. } if text == "2026-01-01")));
        assert!(toks.iter().filter(|t| matches!(t, Token::ColonColon)).count() >= 2);
        // A name that merely starts with digits keeps ending at ':'.
        assert!(lex("/entry[::k = 'a:b']").is_ok());
    }
}
