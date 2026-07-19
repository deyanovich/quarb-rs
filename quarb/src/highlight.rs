//! ANSI syntax highlighting for query text — the terminal twin of
//! the JupyterLab (CodeMirror) highlighter.
//!
//! Both color the same token model — paths and axes, the projection
//! sigils, correlation and pipe operators, register references,
//! strings, numbers, and unit/span literals — but each host needs
//! its own tokenizer (CodeMirror's StreamLanguage in the browser,
//! this scanner in the terminal). The keyword set here is not a
//! hand-kept copy: it is the engine's own stdlib registry
//! ([`crate::stdlib`]), so a new built-in colors without a second
//! edit.
//!
//! The scanner walks the source bytes directly, so whitespace and
//! layout are preserved exactly — the output is the input with SGR
//! escapes woven in. Callers should gate on a TTY.

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
    ":::", "::;", "::", "<=>?", "<=>", "~>", "<~", "->", "<-", "@|", "&&", "||", "=~", "?=", ">=",
    "<=", "!=", "*=", "|", "!", "=", "<", ">", "+", "{", "}", "?", "(", ")", "[", "]", ",",
];

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The query with ANSI SGR escapes woven in. Whitespace and every
/// unrecognized byte pass through verbatim, so the result printed to
/// a terminal reads as the original text, colored.
pub fn highlight_ansi(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 32);
    let mut i = 0;
    let paint = |out: &mut String, color: &str, s: &str| {
        out.push_str(color);
        out.push_str(s);
        out.push_str(RESET);
    };
    while i < b.len() {
        let c = b[i] as char;

        // Whitespace and bytes we don't classify: pass through.
        if c.is_ascii_whitespace() {
            out.push(c);
            i += 1;
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
            paint(&mut out, STRING, &src[start..i.min(b.len())]);
            continue;
        }

        // Register / record / context references: $. $.name $*1 $$
        // @. %. — the whole run colors as a register reference.
        if c == '$' || c == '@' || c == '%' {
            let start = i;
            i += 1;
            while i < b.len() && matches!(b[i] as char, '.' | '*' | '$' | '-') {
                i += 1;
            }
            while i < b.len() && is_name_char(b[i] as char) {
                i += 1;
            }
            paint(&mut out, REGISTER, &src[start..i]);
            continue;
        }

        // Numbers, with an optional unit/span suffix (5km, 90min,
        // 1.5h, 100kB) — the whole literal colors as a number.
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i] as char).is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] as char == '.' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit() {
                i += 1;
                while i < b.len() && (b[i] as char).is_ascii_digit() {
                    i += 1;
                }
            }
            // Unit suffix: a letter followed by unit-expression chars.
            if i < b.len() && (b[i] as char).is_ascii_alphabetic() {
                while i < b.len()
                    && matches!(b[i] as char, 'a'..='z' | 'A'..='Z' | '0'..='9' | '^' | '*' | '/' | '%' | '-')
                {
                    i += 1;
                }
            }
            paint(&mut out, NUMBER, &src[start..i]);
            continue;
        }

        // A leading-dot push/recall name (`.total(`, `.n`): the dot
        // and name read as a register reference.
        if c == '.' && i + 1 < b.len() && is_name_start(b[i + 1] as char) {
            let start = i;
            i += 1;
            while i < b.len() && is_name_char(b[i] as char) {
                i += 1;
            }
            paint(&mut out, REGISTER, &src[start..i]);
            continue;
        }

        // Identifiers: a stdlib built-in is a keyword; anything else
        // is a plain name (property / edge / matcher) — left default.
        if is_name_start(c) {
            let start = i;
            while i < b.len() && is_name_char(b[i] as char) {
                i += 1;
            }
            let word = &src[start..i];
            if is_keyword(word) {
                paint(&mut out, KEYWORD, word);
            } else {
                out.push_str(word);
            }
            continue;
        }

        // Path axes: // then /.
        if c == '/' {
            if i + 1 < b.len() && b[i + 1] as char == '/' {
                paint(&mut out, PATH, "//");
                i += 2;
            } else {
                paint(&mut out, PATH, "/");
                i += 1;
            }
            continue;
        }

        // Operators and sigils (longest match).
        if let Some(op) = OPERATORS.iter().find(|op| src[i..].starts_with(**op)) {
            paint(&mut out, OPERATOR, op);
            i += op.len();
            continue;
        }

        // Anything else: verbatim.
        out.push(c);
        i += 1;
    }
    out
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
}
