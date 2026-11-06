//! Line-atom adapter for Quarb.
//!
//! Every line of a file is a node — the reading `wc`, `grep -c`,
//! and `cloc` all quietly assume, given an arbor. The higher
//! levels (the syntax level's AST, the text level's prose) know what
//! a line *means*; this adapter only knows that it *is*, which is
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
//! - Annotations: `:::n` (1-based ordinal), `:::blank`;
//!   root carries `:::n-lines` and `:::n-blank`.
//! - Lines have no properties at all — a closed surface — so
//!   the annotations alias at `::` under the alias rule: `::n` and
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
    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
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

    /// The kind first (spec: Traits): the root is the `text`, every
    /// other node a `line`, a blank one `blank` too.
    fn traits(&self, node: NodeId) -> Vec<String> {
        if node.0 == 0 {
            return vec!["text".to_string()];
        }
        let mut out = vec!["line".to_string()];
        if self.is_blank(node) {
            out.push("blank".to_string());
        }
        out
    }

    /// The line's text, verbatim.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.line(node).map(|l| Value::Str(l.to_string()))
    }

    /// `:::n-lines` / `:::n-blank` (root); `:::n` / `:::blank`
    /// (line).
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["n-lines", "n-blank", "n", "blank"]
    }

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
    /// annotations answer at `::` too.
    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        if node.0 == 0 {
            &["n-lines", "n-blank"]
        } else {
            &["n", "blank"]
        }
    }
}

/// The lines of a file streamed (hash (d) of the columnar round,
/// 2026-11-06): one pass over the bytes records where each line
/// begins and which are blank; a line's text is read from the file
/// when a step reaches it, the sequential read seeking never. The
/// nodes, the names, the traits and the metadata are the whole
/// reading's.
pub struct LinesStream {
    starts: Vec<u64>,
    /// Each line's length in bytes, its ending excluded.
    lens: Vec<u32>,
    blank: Vec<bool>,
    n_blank: usize,
    reader: std::cell::RefCell<(std::io::BufReader<std::fs::File>, u64)>,
    last: std::cell::RefCell<Option<(u64, String)>>,
}

impl LinesStream {
    pub fn open(path: &std::path::Path) -> std::io::Result<Self> {
        use std::io::{BufRead, Seek};
        let mut reader = std::io::BufReader::with_capacity(1 << 16, std::fs::File::open(path)?);
        let mut starts = Vec::new();
        let mut lens = Vec::new();
        let mut blank = Vec::new();
        let mut n_blank = 0usize;
        let mut line = Vec::new();
        let mut at = 0u64;
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            let start = at;
            at += n as u64;
            let body = line.strip_suffix(b"\n").unwrap_or(&line);
            let body = body.strip_suffix(b"\r").unwrap_or(body);
            // a BOM opens no line
            let bom = if start == 0 && body.starts_with(b"\xEF\xBB\xBF") {
                3
            } else {
                0
            };
            let body = &body[bom..];
            let is_blank = body.iter().all(|c| c.is_ascii_whitespace());
            if is_blank {
                n_blank += 1;
            }
            starts.push(start + bom as u64);
            lens.push(body.len() as u32);
            blank.push(is_blank);
        }
        let mut file = std::io::BufReader::with_capacity(1 << 16, std::fs::File::open(path)?);
        file.seek(std::io::SeekFrom::Start(0))?;
        Ok(LinesStream {
            starts,
            lens,
            blank,
            n_blank,
            reader: std::cell::RefCell::new((file, 0)),
            last: std::cell::RefCell::new(None),
        })
    }

    pub fn n_lines(&self) -> usize {
        self.starts.len()
    }

    pub fn locator(&self, node: NodeId) -> String {
        if node.0 == 0 {
            "/".to_string()
        } else {
            format!("/line[{}]", node.0)
        }
    }

    /// Line `node` (1-based) read from the file, or the one last read.
    fn line(&self, node: NodeId) -> Option<String> {
        let i = (node.0 as usize).checked_sub(1)?;
        {
            let last = self.last.borrow();
            if let Some((k, l)) = &*last
                && *k == node.0
            {
                return Some(l.clone());
            }
        }
        use std::io::{Read, Seek};
        let (&start, &len) = (self.starts.get(i)?, self.lens.get(i)?);
        let text = {
            let mut r = self.reader.borrow_mut();
            if r.1 != start {
                r.0.seek(std::io::SeekFrom::Start(start)).ok()?;
            }
            let mut buf = vec![0u8; len as usize];
            r.0.read_exact(&mut buf).ok()?;
            // past the line's ending to the next line's start
            let next = self.starts.get(i + 1).copied();
            match next {
                Some(n) => {
                    let gap = n - (start + len as u64);
                    if gap > 0 && gap < 16 {
                        let mut skip = vec![0u8; gap as usize];
                        r.0.read_exact(&mut skip).ok()?;
                    }
                    r.1 = n;
                }
                None => r.1 = u64::MAX,
            }
            String::from_utf8_lossy(&buf).into_owned()
        };
        *self.last.borrow_mut() = Some((node.0, text.clone()));
        Some(text)
    }

    fn is_blank(&self, node: NodeId) -> bool {
        (node.0 as usize)
            .checked_sub(1)
            .and_then(|i| self.blank.get(i))
            .copied()
            .unwrap_or(false)
    }

    /// The lines as the plan reaches them from the root: a child step
    /// testing nothing, or the line name alone.
    fn lines_plan(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> bool {
        from.0 == 0
            && plan.conjuncts.is_empty()
            && plan.traits.is_empty()
            && matches!(plan.name, None | Some("line"))
            && matches!(plan.axis, quarb::PlanAxis::Child)
    }
}

impl AstAdapter for LinesStream {
    fn estimate(&self, _scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let n = self.starts.len() as u64;
        let nodes = match plan.limit {
            Some(k) if self.lines_plan(NodeId(0), plan) => (n.min(k as u64), n.min(k as u64)),
            _ => (n, n),
        };
        Some(quarb::Estimate {
            nodes,
            bytes: None,
            requests: None,
            latency: quarb::Latency::Local,
            confidence: quarb::Confidence::Bound,
        })
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            (1..=self.starts.len() as u64).map(NodeId).collect()
        } else {
            Vec::new()
        }
    }

    fn count(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<usize> {
        self.lines_plan(from, plan).then_some(self.starts.len())
    }

    fn select(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<quarb::Selection> {
        let k = plan.limit?;
        if !self.lines_plan(from, plan) {
            return None;
        }
        let n = self.starts.len().min(k);
        Some(quarb::Selection {
            nodes: (1..=n as u64).map(|i| (NodeId(i), 1)).collect(),
            applied: Vec::new(),
        })
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
        if node.0 == 0 {
            return vec!["text".to_string()];
        }
        let mut out = vec!["line".to_string()];
        if self.is_blank(node) {
            out.push("blank".to_string());
        }
        out
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.line(node).map(Value::Str)
    }

    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["n-lines", "n-blank", "n", "blank"]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            return match key {
                "n-lines" => Some(Value::Int(self.starts.len() as i64)),
                "n-blank" => Some(Value::Int(self.n_blank as i64)),
                _ => None,
            };
        }
        match key {
            "n" => Some(Value::Int(node.0 as i64)),
            "blank" => Some(Value::Bool(self.is_blank(node))),
            _ => None,
        }
    }

    fn aliased_metadata(&self, node: NodeId) -> &'static [&'static str] {
        if node.0 == 0 {
            &["n-lines", "n-blank"]
        } else {
            &["n", "blank"]
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn the_stream_reads_as_the_whole_file_does() {
        let doc = "fn main() {\n\n    // hi\r\n\t\n}\nlast without ending";
        let dir = std::env::temp_dir().join(format!("quarb-lines-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.txt");
        std::fs::write(&p, doc).unwrap();
        let s = LinesStream::open(&p).unwrap();
        let w = LinesAdapter::parse(doc);
        assert_eq!(s.children(s.root()).len(), w.children(w.root()).len());
        for k in ["n-lines", "n-blank"] {
            assert_eq!(s.metadata(NodeId(0), k), w.metadata(NodeId(0), k), "{k}");
        }
        for (a, b) in s.children(s.root()).into_iter().zip(w.children(w.root())) {
            assert_eq!(s.name(a), w.name(b));
            assert_eq!(s.traits(a), w.traits(b), "traits of {}", a.0);
            assert_eq!(s.default_value(a), w.default_value(b), "text of {}", a.0);
            assert_eq!(s.locator(a), w.locator(b));
            for k in ["n", "blank"] {
                assert_eq!(s.metadata(a, k), w.metadata(b, k), "{k} of {}", a.0);
            }
        }
        // out of order, then in order again
        assert_eq!(s.default_value(NodeId(5)), w.default_value(NodeId(5)));
        assert_eq!(s.default_value(NodeId(2)), w.default_value(NodeId(2)));
        assert_eq!(s.default_value(NodeId(3)), w.default_value(NodeId(3)));
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
        // the kind first (spec: Traits): a line, blank or not
        assert_eq!(a.traits(NodeId(2)), vec!["line", "blank"]);
        assert_eq!(a.traits(NodeId(4)), vec!["line", "blank"]); // tab-only
        assert_eq!(a.traits(NodeId(3)), vec!["line"]);
        assert_eq!(a.traits(NodeId(0)), vec!["text"]);
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
