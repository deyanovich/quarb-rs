//! Syntax highlighting for query text — one tokenizer, three emits.
//!
//! The [`scan`] tokenizer classifies a query into paths and axes, the
//! projection sigils, correlation and pipe operators, register
//! references, strings, numbers, and unit/span literals. Two thin
//! renderers consume it: [`highlight_ansi`] for the terminal (SGR
//! escapes) and [`highlight_html`] for the browser playground
//! (`<span class="qh-…">`). The JupyterLab extension's CodeMirror
//! tokenizer mirrors the same model in TypeScript.
//!
//! The keyword set is not a hand-kept copy: it is the engine's own
//! stdlib registry ([`crate::stdlib`]), so a new built-in colors
//! without a second edit. The scanner covers every byte in order, so
//! the spans concatenate back to the exact source.

const RESET: &str = "\x1b[0m";
const KEYWORD: &str = "\x1b[1;36m"; // bold cyan — stdlib functions, def/macro
const STRING: &str = "\x1b[32m"; // green
const NUMBER: &str = "\x1b[35m"; // magenta — numbers and unit/span literals
const OPERATOR: &str = "\x1b[33m"; // yellow — sigils, comparisons, pipes
const PATH: &str = "\x1b[34m"; // blue — the / and // axes
const REGISTER: &str = "\x1b[36m"; // cyan — $. $* @. %. register refs

/// Non-alphanumeric operator spellings, longest first so a prefix
/// never wins over the sigil it opens (`:::` before `::`, `<=>?`
/// before `<=>`).
const OPERATORS: &[&str] = &[
    ";;;", ":::", "::;", "::", "<=>?", "<=>", "~>", "<~", "->", "<-", "@|", "&&", "||", "=~", "?=",
    ">=", "<=", "!=", "*=", "|", "!", "=", "<", ">", "+", "{", "}", "?", "(", ")", "[", "]", ",",
];

/// A token category — the shared classification both renderers map.
#[derive(Clone, Copy)]
enum Class {
    Keyword,
    Str,
    Number,
    Operator,
    Path,
    Register,
}

impl Class {
    fn ansi(self) -> &'static str {
        match self {
            Class::Keyword => KEYWORD,
            Class::Str => STRING,
            Class::Number => NUMBER,
            Class::Operator => OPERATOR,
            Class::Path => PATH,
            Class::Register => REGISTER,
        }
    }
    fn css(self) -> &'static str {
        match self {
            Class::Keyword => "qh-keyword",
            Class::Str => "qh-string",
            Class::Number => "qh-number",
            Class::Operator => "qh-operator",
            Class::Path => "qh-path",
            Class::Register => "qh-register",
        }
    }
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Tokenize `src` into ordered `(class, slice)` spans that partition
/// it exactly. `None` is verbatim — whitespace, plain names
/// (properties/edges), and unclassified punctuation.
fn scan(src: &str) -> Vec<(Option<Class>, &str)> {
    let b = src.as_bytes();
    let mut spans: Vec<(Option<Class>, &str)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;

        // Whitespace runs: verbatim.
        if c.is_ascii_whitespace() {
            let start = i;
            i += 1;
            while i < b.len() && (b[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            spans.push((None, &src[start..i]));
            continue;
        }

        // Strings: "..." or '...', with backslash escapes.
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < b.len() {
                let ch = b[i] as char;
                i += 1;
                if ch == '\\' {
                    i += 1;
                } else if ch == quote {
                    break;
                }
            }
            spans.push((Some(Class::Str), &src[start..i.min(b.len())]));
            continue;
        }

        // Register / record / context references: $. $.name $*1 $$
        // @. %. — the whole run colors as a register reference.
        // `@|` is the aggregation pipe, not a register: leave it
        // for the operator table.
        if (c == '$' || c == '@' || c == '%') && !(c == '@' && b.get(i + 1) == Some(&b'|')) {
            let start = i;
            i += 1;
            while i < b.len() && matches!(b[i] as char, '.' | '*' | '$' | '-') {
                i += 1;
            }
            while i < b.len() && is_name_char(b[i] as char) {
                i += 1;
            }
            spans.push((Some(Class::Register), &src[start..i]));
            continue;
        }

        // Numbers, with an optional unit/span suffix (5km, 90min,
        // 1.5h, 100kB) — the whole literal colors as a number.
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i] as char).is_ascii_digit() {
                i += 1;
            }
            if i < b.len()
                && b[i] as char == '.'
                && i + 1 < b.len()
                && (b[i + 1] as char).is_ascii_digit()
            {
                i += 1;
                while i < b.len() && (b[i] as char).is_ascii_digit() {
                    i += 1;
                }
            }
            if i < b.len() && (b[i] as char).is_ascii_alphabetic() {
                while i < b.len()
                    && matches!(b[i] as char, 'a'..='z' | 'A'..='Z' | '0'..='9' | '^' | '*' | '/' | '%' | '-')
                {
                    i += 1;
                }
            }
            spans.push((Some(Class::Number), &src[start..i]));
            continue;
        }

        // A leading-dot push/recall name (`.total`, `.n`).
        if c == '.' && i + 1 < b.len() && is_name_start(b[i + 1] as char) {
            let start = i;
            i += 1;
            while i < b.len() && is_name_char(b[i] as char) {
                i += 1;
            }
            spans.push((Some(Class::Register), &src[start..i]));
            continue;
        }

        // Identifiers: a stdlib built-in is a keyword; anything else is
        // a plain name (property / edge / matcher), left verbatim.
        if is_name_start(c) {
            let start = i;
            while i < b.len() && is_name_char(b[i] as char) {
                i += 1;
            }
            let word = &src[start..i];
            spans.push((is_keyword(word).then_some(Class::Keyword), word));
            continue;
        }

        // Path axes: // then /.
        if c == '/' {
            if i + 1 < b.len() && b[i + 1] as char == '/' {
                spans.push((Some(Class::Path), &src[i..i + 2]));
                i += 2;
            } else {
                spans.push((Some(Class::Path), &src[i..i + 1]));
                i += 1;
            }
            continue;
        }

        // Operators and sigils (longest match).
        if let Some(op) = OPERATORS.iter().find(|op| src[i..].starts_with(**op)) {
            spans.push((Some(Class::Operator), &src[i..i + op.len()]));
            i += op.len();
            continue;
        }

        // Anything else: verbatim, one full char (UTF-8 safe).
        let start = i;
        i += 1;
        while i < b.len() && (b[i] & 0xC0) == 0x80 {
            i += 1;
        }
        spans.push((None, &src[start..i]));
    }
    spans
}

/// The query with ANSI SGR escapes woven in. Whitespace and every
/// unclassified byte pass through verbatim, so the result printed to a
/// terminal reads as the original text, colored. Callers should gate
/// on a TTY.
pub fn highlight_ansi(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 32);
    for (class, s) in scan(src) {
        match class {
            Some(c) => {
                out.push_str(c.ansi());
                out.push_str(s);
                out.push_str(RESET);
            }
            None => out.push_str(s),
        }
    }
    out
}

/// The query with HTML `<span class="qh-…">` markup woven in, for the
/// browser playground. The token model is identical to
/// [`highlight_ansi`]; the text is HTML-escaped.
pub fn highlight_html(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    for (class, s) in scan(src) {
        match class {
            Some(c) => {
                out.push_str("<span class=\"");
                out.push_str(c.css());
                out.push_str("\">");
                escape_into(&mut out, s);
                out.push_str("</span>");
            }
            None => escape_into(&mut out, s),
        }
    }
    out
}

/// Append `s` to `out` with HTML metacharacters escaped.
fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

/// Whether `word` colors as a keyword: a stdlib function (scalar,
/// aggregate, or keyed) or a reuse/logic word.
fn is_keyword(word: &str) -> bool {
    crate::stdlib::known_scalar(word)
        || crate::stdlib::known_agg(word)
        || crate::stdlib::known_keyed(word)
        || matches!(word, "def" | "macro" | "not" | "and" | "or")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        // Remove SGR escapes to check the text round-trips exactly.
        let mut out = String::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == 0x1b {
                while i < b.len() && b[i] != b'm' {
                    i += 1;
                }
                i += 1;
            } else {
                out.push(b[i] as char);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn preserves_source_exactly() {
        for q in [
            "/books/*[/price:: > 20]/title::",
            "/@hosts/*[::draw > 0.2kW]::name",
            "/a/* <=> /b/*[::k = $*1::k] | rec(\"x\", ::v)",
            "//function_item @| count | .n | %.",
        ] {
            assert_eq!(strip(&highlight_ansi(q)), q, "round-trip: {q}");
        }
    }

    #[test]
    fn colors_the_right_tokens() {
        let h = highlight_ansi("/x/* | rec(\"a\", ::v) @| count");
        assert!(h.contains(&format!("{KEYWORD}rec{RESET}")));
        assert!(h.contains(&format!("{KEYWORD}count{RESET}")));
        assert!(h.contains(&format!("{STRING}\"a\"{RESET}")));
        assert!(h.contains(&format!("{PATH}/{RESET}")));
        // A unit literal colors whole.
        let u = highlight_ansi("[::draw > 0.2kW]");
        assert!(u.contains(&format!("{NUMBER}0.2kW{RESET}")));
        // A property name stays uncolored (no keyword escape on it).
        assert!(!highlight_ansi("::draw").contains(&format!("{KEYWORD}draw")));
    }

    #[test]
    fn html_escapes_and_wraps() {
        let h = highlight_html("/a/*[::v > 1] | rec(\"x\", ::v)");
        assert!(h.contains("<span class=\"qh-path\">/</span>"));
        assert!(h.contains("<span class=\"qh-keyword\">rec</span>"));
        assert!(h.contains("<span class=\"qh-operator\">&gt;</span>"));
        // Strip tags and unescape → the original source.
        assert!(h.contains("<span class=\"qh-string\">&quot;x&quot;</span>"));
    }
}
