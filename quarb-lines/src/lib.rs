//! Line-atom adapter for Quarb.
//!
//! Every line of a file is a node — the reading `wc`, `grep -c`,
//! and `cloc` all quietly assume, given an arbor. The syntax
//! levels (quarb-code's AST, the text level's prose) know what a
//! line *means*; this adapter only knows that it *is*, which is
//! exactly the half of line-classification that syntax discards:
//! blank lines and totals are not syntax, so no grammar-driven
//! reading can count them.
//!
//! - The unnamed root has one `line` child per line, in file
//!   order: `/line` iterates, `/line[5]` is the fifth,
//!   `//line @| count` is the total.
//! - `::` (bare) is the line's text, verbatim — leading
//!   whitespace preserved; trimming is a query decision.
//! - `<blank>` traits whitespace-only lines, so the trait
//!   algebra reads the way the question is asked:
//!   `//line<!blank> @| count`.
//! - Annotations: `::::n` (1-based ordinal), `::::blank`;
//!   root carries `::::n-lines` and `::::n-blank`.
//! - Lines have no properties at all — a closed surface — so
//!   the annotations alias at `::` per ruling #29: `::n` and
//!   `::blank` just work, and can never shadow anything.
//!
//! With the `code:` reading of the same file mounted beside
//! this one, `cloc` becomes a join: comments from the grammar,
//! totals and blanks from here.

use quarb::{AstAdapter, NodeId, Value};

/// A Quarb adapter over the lines of a text.
pub struct LinesAdapter {
    lines: Vec<String>,
}

impl LinesAdapter {
    /// Split `text` into line nodes (`str::lines` semantics: no
    /// phantom line after a trailing newline; `\r\n` folds).
    pub fn parse(text: &str) -> Self {
        LinesAdapter {
            lines: text.lines().map(str::to_string).collect(),
        }
    }

    /// A locator path to `node`, like `/line[3]`, for rendering.
    pub fn locator(&self, node: NodeId) -> String {
        if node.0 == 0 {
            "/".to_string()
        } else {
            format!("/line[{}]", node.0)
        }
    }

    fn line(&self, node: NodeId) -> Option<&str> {
        self.lines
            .get((node.0 as usize).checked_sub(1)?)
            .map(String::as_str)
    }

    fn is_blank(&self, node: NodeId) -> bool {
        self.line(node).is_some_and(|l| l.trim().is_empty())
    }
}

impl AstAdapter for LinesAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            (1..=self.lines.len() as u64).map(NodeId).collect()
        } else {
            Vec::new()
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        if node.0 == 0 {
            None
        } else {
            Some("line".to_string())
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        if node.0 == 0 { None } else { Some(NodeId(0)) }
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        if node.0 != 0 && self.is_blank(node) {
            vec!["blank".to_string()]
        } else {
            Vec::new()
        }
    }

    /// The line's text, verbatim.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.line(node).map(|l| Value::Str(l.to_string()))
    }

    /// `::::n-lines` / `::::n-blank` (root); `::::n` / `::::blank`
    /// (line).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            return match key {
                "n-lines" => Some(Value::Int(self.lines.len() as i64)),
                "n-blank" => Some(Value::Int(
                    (1..=self.lines.len() as u64)
                        .filter(|&i| self.is_blank(NodeId(i)))
                        .count() as i64,
                )),
                _ => None,
            };
        }
        match key {
            "n" => Some(Value::Int(node.0 as i64)),
            "blank" => Some(Value::Bool(self.is_blank(node))),
            _ => None,
        }
    }

    /// Lines have no properties — a closed surface — so the
    /// annotations answer at `::` too (ruling #29).
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        if node.0 == 0 {
            &["n-lines", "n-blank"]
        } else {
            &["n", "blank"]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "fn main() {\n\n    // hi\n\t\n}\n";

    #[test]
    fn counts() {
        let a = LinesAdapter::parse(DOC);
        assert_eq!(a.metadata(NodeId(0), "n-lines"), Some(Value::Int(5)));
        assert_eq!(a.metadata(NodeId(0), "n-blank"), Some(Value::Int(2)));
    }

    #[test]
    fn blank_is_whitespace_only() {
        let a = LinesAdapter::parse(DOC);
        assert_eq!(a.traits(NodeId(2)), vec!["blank"]);
        assert_eq!(a.traits(NodeId(4)), vec!["blank"]); // tab-only
        assert!(a.traits(NodeId(3)).is_empty());
    }

    #[test]
    fn queries_run() {
        let a = LinesAdapter::parse(DOC);
        let out = quarb::run("//line<!blank> @| count", &a).unwrap();
        match out {
            quarb::QueryResult::Values(v) => {
                assert_eq!(v[0].to_string(), "3");
            }
            _ => panic!("expected values"),
        }
    }

    #[test]
    fn aliases_answer_at_two_colons() {
        let a = LinesAdapter::parse(DOC);
        let out = quarb::run("/line[::blank] @| count", &a).unwrap();
        match out {
            quarb::QueryResult::Values(v) => assert_eq!(v[0].to_string(), "2"),
            _ => panic!("expected values"),
        }
    }

    #[test]
    fn no_phantom_trailing_line() {
        let a = LinesAdapter::parse("a\nb\n");
        assert_eq!(a.metadata(NodeId(0), "n-lines"), Some(Value::Int(2)));
    }
}
