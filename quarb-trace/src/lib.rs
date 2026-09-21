//! quarb-trace — the Rust mirror of the `quarb/trace` kaiv schema:
//! the viz contract's payload types and their kaiv serialization.
//!
//! The contract is the kaiv schema (`schema/trace.saiv` in the
//! quarbopsis repo, typed from the hand-written corpus); this crate
//! is its Rust embodiment — the types the engine fills and the
//! writer that emits the interchange document. No engine
//! dependency and no reader: renderers consume the kaiv document.
//!
//! Field order and typing follow the corpus payloads line for line,
//! so an emitted document validates against the schema and diffs
//! cleanly against a hand-written one.

use std::fmt::Write as _;

/// A scalar as the payload carries it: adapter properties, topics,
/// results, and stub measures.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl Scalar {
    pub fn text(&self) -> String {
        match self {
            Scalar::Null => "∅".to_string(),
            Scalar::Bool(b) => b.to_string(),
            Scalar::Int(i) => i.to_string(),
            Scalar::Float(f) => f.to_string(),
            Scalar::Str(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub contract: String,
    pub version: String,
    pub query: String,
    pub adapter: String,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct Arbor {
    pub root: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub node: String,
    pub kind: String,
    /// Adapter-open properties, in emission order.
    pub props: Vec<(String, Scalar)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Parent,
    Crosslink,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub edge: String,
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    /// Tree edges: the address name and the child's position among
    /// all its parent's children (1-based).
    pub name: Option<String>,
    pub index: Option<i64>,
    /// Crosslink edges: the label.
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Step {
    pub step: i64,
    /// hop | projection | agg | group | filter | stage
    pub kind: String,
    pub expr: String,
    pub axis: Option<String>,
    pub matcher: Option<String>,
    pub matcherkind: Option<String>,
    pub predicate: Option<String>,
    pub projection: Option<String>,
    pub projectionkind: Option<String>,
    pub name: Option<String>,
    pub body: Option<String>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// Previewed but not committed (the staging area).
    pub staged: bool,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub thread: String,
    pub parent: Option<String>,
    pub born: i64,
    pub ended: Option<i64>,
    /// live | forked | filtered | exhausted | aggregated | merged
    pub fate: String,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub group: String,
    pub at: i64,
    pub kind: String,
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub at: i64,
    pub iter: Option<i64>,
    /// navigation | scalar
    pub mode: String,
    pub size: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Window {
    pub at: i64,
    pub first: i64,
    pub last: i64,
    pub of: i64,
}

#[derive(Debug, Clone)]
pub struct Stub {
    pub at: i64,
    /// before | after
    pub side: String,
    pub first: i64,
    pub last: i64,
    pub count: i64,
    pub nodes: Option<i64>,
    pub children: Option<i64>,
    pub size: Option<i64>,
    pub kinds: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Position {
    pub at: i64,
    pub iter: Option<i64>,
    pub thread: String,
    pub node: Option<String>,
    pub topic: Option<Scalar>,
    pub index: Option<i64>,
    pub locator: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Ribbon {
    pub at: i64,
    pub iter: Option<i64>,
    pub edge: String,
    pub threads: i64,
    /// against — walked from the edge's `to` end.
    pub dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Lookahead {
    pub at: i64,
    pub iter: Option<i64>,
    pub thread: String,
    pub node: String,
    /// match | fail
    pub verdict: String,
    pub index: Option<i64>,
    pub locator: Option<String>,
}

/// One `quarb/trace` payload.
#[derive(Debug, Clone, Default)]
pub struct Payload {
    pub meta: Meta,
    pub arbor: Arbor,
    pub steps: Vec<Step>,
    pub threads: Vec<Thread>,
    pub groups: Vec<Group>,
    pub snapshots: Vec<Snapshot>,
    pub window: Window,
    pub stubs: Vec<Stub>,
    pub positions: Vec<Position>,
    pub ribbons: Vec<Ribbon>,
    pub lookahead: Vec<Lookahead>,
    /// Absent in windowed payloads: the positions are the results.
    pub results: Option<Vec<Scalar>>,
}

pub const CONTRACT: &str = "quarb/trace";
pub const VERSION: &str = "0.0-draft.1";

// ---------------------------------------------------------------
// kaiv writer

/// A value line's text: kaiv reads `$name` as a reference, so a
/// literal `$` doubles; a value is one line.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '$' => out.push_str("$$"),
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A key as kaiv reads it: bare when it is an identifier, double-
/// quoted otherwise (an html `data-here` or `aria-label`).
fn key(k: &str) -> String {
    let bare = !k.is_empty()
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if bare || k.starts_with('/') || k.starts_with('@') {
        k.to_string()
    } else {
        format!("\"{}\"", k.replace('"', ""))
    }
}

struct W(String);

impl W {
    fn line(&mut self, s: &str) {
        self.0.push_str(s);
        self.0.push('\n');
    }
    fn str(&mut self, k: &str, v: &str) {
        let _ = writeln!(self.0, "{}={}", key(k), escape(v));
    }
    fn int(&mut self, k: &str, v: i64) {
        let _ = writeln!(self.0, "!int\n{}={v}", key(k));
    }
    fn null(&mut self, k: &str) {
        let _ = writeln!(self.0, "!null\n{}=", key(k));
    }
    fn scalar(&mut self, k: &str, v: &Scalar) {
        match v {
            Scalar::Null => self.null(k),
            Scalar::Bool(b) => {
                let _ = writeln!(self.0, "!bool\n{}={b}", key(k));
            }
            Scalar::Int(i) => self.int(k, *i),
            Scalar::Float(f) => {
                let _ = writeln!(self.0, "!float\n{}={f}", key(k));
            }
            Scalar::Str(s) => self.str(k, s),
        }
    }
    fn opt_str(&mut self, key: &str, v: &Option<String>) {
        if let Some(s) = v {
            self.str(key, s);
        }
    }
    fn opt_int(&mut self, key: &str, v: &Option<i64>) {
        if let Some(i) = v {
            self.int(key, *i);
        }
    }
    fn iter(&mut self, v: &Option<i64>) {
        self.opt_int("iter", v);
    }
}

impl Payload {
    /// The kaiv document, in the corpus's line order.
    pub fn to_kaiv(&self) -> String {
        let mut w = W(String::new());
        w.line(".!kaiv");
        w.line("");
        w.line("# quarb/trace payload — engine-emitted.");
        w.line("(/meta)");
        w.str("contract", &self.meta.contract);
        w.str("version", &self.meta.version);
        w.str("query", &self.meta.query);
        w.str("adapter", &self.meta.adapter);
        w.str("source", &self.meta.source);
        w.line("()");
        w.line("");
        w.str("/arbor::root", &self.arbor.root);
        w.line("");
        for n in &self.arbor.nodes {
            w.line("[/arbor/@nodes]");
            w.str("node", &n.node);
            w.str("kind", &n.kind);
            for (k, v) in &n.props {
                w.scalar(k, v);
            }
        }
        if !self.arbor.nodes.is_empty() {
            w.line("[]");
            w.line("");
        }
        for e in &self.arbor.edges {
            w.line("[/arbor/@edges]");
            w.str("edge", &e.edge);
            w.str("from", &e.from);
            w.str("to", &e.to);
            w.str(
                "kind",
                match e.kind {
                    EdgeKind::Parent => "parent",
                    EdgeKind::Crosslink => "crosslink",
                },
            );
            w.opt_str("name", &e.name);
            w.opt_int("index", &e.index);
            w.opt_str("label", &e.label);
        }
        if !self.arbor.edges.is_empty() {
            w.line("[]");
            w.line("");
        }
        for s in &self.steps {
            w.line("[/query/@steps]");
            w.int("step", s.step);
            w.str("kind", &s.kind);
            w.str("expr", &s.expr);
            w.opt_str("axis", &s.axis);
            w.opt_str("matcher", &s.matcher);
            w.opt_str("matcherkind", &s.matcherkind);
            w.opt_str("predicate", &s.predicate);
            w.opt_str("projection", &s.projection);
            w.opt_str("projectionkind", &s.projectionkind);
            w.opt_str("name", &s.name);
            w.opt_str("body", &s.body);
            if s.kind == "group" {
                w.opt_int("min", &s.min);
                match s.max {
                    Some(m) => w.int("max", m),
                    None => w.null("max"),
                }
            }
            if s.staged {
                w.line("!bool\nstaged=true");
            }
        }
        if !self.steps.is_empty() {
            w.line("[]");
            w.line("");
        }
        for t in &self.threads {
            w.line("[/trace/@threads]");
            w.str("thread", &t.thread);
            match &t.parent {
                Some(p) => w.str("parent", p),
                None => w.null("parent"),
            }
            w.int("born", t.born);
            match t.ended {
                Some(e) => w.int("ended", e),
                None => w.null("ended"),
            }
            w.str("fate", &t.fate);
        }
        if !self.threads.is_empty() {
            w.line("[]");
            w.line("");
        }
        for g in &self.groups {
            w.line("[/trace/@groups]");
            w.str("group", &g.group);
            w.int("at", g.at);
            w.str("kind", &g.kind);
            w.str("name", &g.name);
            for m in &g.members {
                w.str("@members+", m);
            }
        }
        if !self.groups.is_empty() {
            w.line("[]");
            w.line("");
        }
        for s in &self.snapshots {
            w.line("[/trace/@snapshots]");
            w.int("at", s.at);
            w.iter(&s.iter);
            w.str("mode", &s.mode);
            w.int("size", s.size);
        }
        if !self.snapshots.is_empty() {
            w.line("[]");
            w.line("");
        }
        w.line("(/trace/window)");
        w.int("at", self.window.at);
        w.int("first", self.window.first);
        w.int("last", self.window.last);
        w.int("of", self.window.of);
        w.line("()");
        w.line("");
        for s in &self.stubs {
            w.line("[/trace/@stubs]");
            w.int("at", s.at);
            w.str("side", &s.side);
            w.int("first", s.first);
            w.int("last", s.last);
            w.int("count", s.count);
            w.opt_int("nodes", &s.nodes);
            w.opt_int("children", &s.children);
            w.opt_int("size", &s.size);
            w.opt_str("kinds", &s.kinds);
        }
        if !self.stubs.is_empty() {
            w.line("[]");
            w.line("");
        }
        for p in &self.positions {
            w.line("[/trace/@positions]");
            w.int("at", p.at);
            w.iter(&p.iter);
            w.str("thread", &p.thread);
            match &p.node {
                Some(n) => w.str("node", n),
                None => w.null("node"),
            }
            if let Some(t) = &p.topic {
                w.scalar("topic", t);
            }
            w.opt_int("index", &p.index);
            w.opt_str("locator", &p.locator);
        }
        if !self.positions.is_empty() {
            w.line("[]");
            w.line("");
        }
        for r in &self.ribbons {
            w.line("[/trace/@ribbons]");
            w.int("at", r.at);
            w.iter(&r.iter);
            w.str("edge", &r.edge);
            w.opt_str("dir", &r.dir);
            w.int("threads", r.threads);
        }
        if !self.ribbons.is_empty() {
            w.line("[]");
            w.line("");
        }
        for l in &self.lookahead {
            w.line("[/trace/@lookahead]");
            w.int("at", l.at);
            w.iter(&l.iter);
            w.str("thread", &l.thread);
            w.str("node", &l.node);
            w.str("verdict", &l.verdict);
            w.opt_int("index", &l.index);
            w.opt_str("locator", &l.locator);
        }
        if !self.lookahead.is_empty() {
            w.line("[]");
            w.line("");
        }
        if let Some(results) = &self.results {
            for r in results {
                match r {
                    Scalar::Str(s) => w.str("/@results+", s),
                    other => w.scalar("/@results+", other),
                }
            }
        }
        w.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_dollar_and_newlines() {
        assert_eq!(escape("a$b\nc"), "a$$b c");
    }

    #[test]
    fn keys_quote_when_not_bare() {
        assert_eq!(key("href"), "href");
        assert_eq!(key("data-here"), "\"data-here\"");
        assert_eq!(key("0"), "\"0\"");
        assert_eq!(key("/@results+"), "/@results+");
    }

    #[test]
    fn empty_tables_are_omitted() {
        let p = Payload::default();
        let k = p.to_kaiv();
        assert!(k.contains("(/trace/window)"));
        assert!(!k.contains("[/trace/@stubs]"));
        assert!(!k.contains("[/@results"));
    }
}
