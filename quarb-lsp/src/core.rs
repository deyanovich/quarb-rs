//! The protocol-agnostic core: one set of operations, two codecs.
//!
//! Both transports (LSP JSON-RPC over stdio, kaivrpc over a Unix
//! socket) decode into these calls and encode the same answers.
//! The engine is the single source of truth: diagnostics come
//! from the real parser (what refuses here refuses in `qua`),
//! completions from `quarb::complete` — the same registry the
//! highlighter and quai read.
//!
//! Positions: the engine's parse errors are messages, not spans
//! (`QuarbError::Parse(String)`). Until the parser carries
//! offsets, the diagnostic range comes from a heuristic — the
//! engine consistently backticks the offending token, and the
//! first occurrence of that token in the text anchors the range.
//! When nothing anchors, the whole first line is the range.

use serde::Serialize;

/// One diagnostic, zero-based positions, single line.
#[derive(Debug, Clone, Serialize)]
pub struct Diag {
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
    pub message: String,
}

/// One completion candidate with its kind, as the wire sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub label: String,
    /// "function" | "aggregate" | "register" | "child" |
    /// "property" | "trait"
    pub kind: &'static str,
}

/// A model/defs file starts with a statement keyword; v1
/// diagnostics cover query files only (the defs grammar has its
/// own statement-level errors the engine reports per statement).
fn is_defs(text: &str) -> bool {
    text.lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .is_some_and(|l| {
            ["node ", "ref ", "edge ", "rel ", "mount "]
                .iter()
                .any(|k| l.starts_with(k))
        })
}

/// Parse `text` as a query; on refusal, one anchored diagnostic.
pub fn diagnostics(text: &str) -> Vec<Diag> {
    let query = text.trim_end();
    if query.is_empty() || is_defs(query) {
        return Vec::new();
    }
    match quarb::reflect::QueryArbor::parse(query) {
        Ok(_) => Vec::new(),
        Err(e) => {
            let message = e.to_string();
            let (line, col_start, col_end) = anchor(query, &message);
            vec![Diag { line, col_start, col_end, message }]
        }
    }
}

/// Locate the error's backticked token in the text, if any.
fn anchor(text: &str, message: &str) -> (u32, u32, u32) {
    if let Some(tok) = backticked(message) {
        if let Some(byte) = text.find(tok) {
            let (line, col) = line_col(text, byte);
            return (line, col, col + tok.chars().count() as u32);
        }
    }
    let first_len = text.lines().next().unwrap_or("").chars().count() as u32;
    (0, 0, first_len.max(1))
}

fn backticked(message: &str) -> Option<&str> {
    let start = message.find('`')? + 1;
    let end = start + message[start..].find('`')?;
    let tok = &message[start..end];
    (!tok.is_empty()).then_some(tok)
}

fn line_col(text: &str, byte: usize) -> (u32, u32) {
    let head = &text[..byte];
    let line = head.matches('\n').count() as u32;
    let col = head.rsplit('\n').next().unwrap_or("").chars().count() as u32;
    (line, col)
}

/// Completion at a zero-based line/character position.
pub fn completions(text: &str, line: u32, character: u32) -> Vec<Item> {
    let cursor = byte_at(text, line, character);
    quarb::complete::complete(text, cursor)
        .into_iter()
        .map(|c| Item {
            label: c.text,
            kind: match c.kind {
                quarb::complete::Kind::Function => "function",
                quarb::complete::Kind::Aggregate => "aggregate",
                quarb::complete::Kind::Register => "register",
                quarb::complete::Kind::Child => "child",
                quarb::complete::Kind::Property => "property",
                quarb::complete::Kind::Trait => "trait",
            },
        })
        .collect()
}

fn byte_at(text: &str, line: u32, character: u32) -> usize {
    let mut offset = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i as u32 == line {
            let col: usize = l
                .chars()
                .take(character as usize)
                .map(char::len_utf8)
                .sum();
            return offset + col.min(l.len());
        }
        offset += l.len();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_query_is_clean() {
        assert!(diagnostics("/books/*[::price > 20]::title").is_empty());
    }

    #[test]
    fn refusal_is_anchored() {
        // The bare ::: needs a key; the diagnostic lands on it.
        let d = diagnostics("/tags/* | rec(:::)");
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains(":::"), "{}", d[0].message);
    }

    #[test]
    fn defs_files_pass_through() {
        assert!(diagnostics("node /people/person: /teams/*;").is_empty());
    }

    #[test]
    fn completion_positions_are_utf16_agnostic_enough() {
        let items = completions("/cf/entry @| co", 0, 15);
        assert!(items.iter().any(|i| i.label == "count"));
    }
}
