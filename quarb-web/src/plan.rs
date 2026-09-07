//! The planner: what of a query an index-backed store can answer
//! before the engine runs. Three rungs, each with its own gate:
//!
//! - **Full** — the store's answer *is* the answer: `//page[…] @|
//!   count` with every conjunct translated exactly.
//! - **Prefilter** — the store names a superset of the pages the
//!   enumerating step would keep; the engine then runs the
//!   original query over the level *scoped* to those candidates
//!   and re-verifies every predicate. Sound whenever no later
//!   step can re-enter the tree from above (a parent, an
//!   ancestor, a sibling, an anchored path), because a scope
//!   narrows enumeration only — crosslinks, keyed rows, and the
//!   grafts are untouched.
//! - **Scan** — the engine alone, with the reason stated.
//!
//! The planner reads the query through reflection (the query
//! arbor), so it depends on the locked vocabulary and nothing
//! private to the parser.

use crate::db::{Dialect, Param};
use quarb::reflect::QueryArbor;
use quarb::{AstAdapter, NodeId, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rung {
    Full,
    Prefilter,
    Scan,
}

/// The planner's decision.
#[derive(Clone, Debug)]
pub struct Plan {
    pub rung: Rung,
    /// The WHERE clause over `pages p` (Full and Prefilter).
    pub where_sql: String,
    pub params: Vec<Param>,
    /// Why this rung: the construct that stopped a higher one, or
    /// what the clause covers.
    pub reason: String,
    /// The conjuncts the engine must still verify (Prefilter).
    pub unverified: Vec<String>,
    /// `@| top(n; ::col)` / `bottom`: the candidates are the first
    /// `n` by `col` (descending for top), ties in document order —
    /// exactly the engine's stable sort — when every conjunct is
    /// exact, so the store may cut the sweep to `n` rows.
    pub limit: Option<Limit>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub column: String,
    pub descending: bool,
    pub n: i64,
}

impl Plan {
    fn scan(reason: impl Into<String>) -> Plan {
        Plan { rung: Rung::Scan, where_sql: String::new(), params: Vec::new(), reason: reason.into(), unverified: Vec::new(), limit: None }
    }
}

/// A SQL fragment under construction, with its parameters.
struct Sql {
    dialect: Dialect,
    params: Vec<Param>,
}

impl Sql {
    fn param(&mut self, p: Param) -> String {
        self.params.push(p);
        match self.dialect {
            Dialect::Sqlite => "?".to_string(),
            Dialect::Postgres => format!("${}", self.params.len()),
        }
    }

    /// `col` contains `lit` (case-sensitive, exact).
    fn contains(&mut self, col: &str, lit: &str) -> String {
        match self.dialect {
            Dialect::Sqlite => format!("instr({col}, {}) > 0", self.param(Param::Str(lit.to_string()))),
            Dialect::Postgres => format!("{col} LIKE {} ESCAPE '\\'", self.param(Param::Str(format!("%{}%", like_escape(lit))))),
        }
    }

    /// `col` matches the LIKE pattern (already escaped).
    fn like(&mut self, col: &str, pattern: String) -> String {
        match self.dialect {
            Dialect::Sqlite => format!("{col} LIKE {} ESCAPE '\\'", self.param(Param::Str(pattern))),
            Dialect::Postgres => format!("{col} LIKE {} ESCAPE '\\'", self.param(Param::Str(pattern))),
        }
    }
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// One translated conjunct.
struct Conj {
    sql: String,
    exact: bool,
    /// What the engine still verifies when not exact.
    note: String,
}

/// The arbor with its reading helpers.
struct Q<'a> {
    a: &'a QueryArbor,
}

impl Q<'_> {
    fn kids(&self, n: NodeId) -> Vec<NodeId> {
        self.a.children(n)
    }
    fn name(&self, n: NodeId) -> String {
        self.a.name(n).unwrap_or_default()
    }
    fn prop(&self, n: NodeId, key: &str) -> Option<Value> {
        self.a.property(n, key)
    }
    fn text(&self, n: NodeId, key: &str) -> Option<String> {
        match self.prop(n, key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }
    fn kids_named(&self, n: NodeId, name: &str) -> Vec<NodeId> {
        self.kids(n).into_iter().filter(|c| self.name(*c) == name).collect()
    }
    /// Every node below `n` (depth-first).
    fn all_below(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = self.kids(n);
        stack.reverse();
        while let Some(c) = stack.pop() {
            out.push(c);
            let mut ks = self.kids(c);
            ks.reverse();
            stack.extend(ks);
        }
        out
    }
}

/// Axes a step may use after the enumerating step (and inside
/// its predicates) without re-entering the tree from above.
const SAFE_AXES: &[&str] = &["/", "//", "//?", "//!", "->", "<-", "--", "-->"];
/// Ascent is safe only as a path's tail (a parent chain reads
/// keyed rows, never an enumeration).
const ASCENT_AXES: &[&str] = &["\\", "\\\\", "\\\\?", "\\\\!"];
/// Node kinds that carry navigation the planner does not follow.
const UNSAFE_KINDS: &[&str] = &["mark", "subcontext", "push", "expr-push", "context", "ordinal", "topic", "recall", "match"];

fn anchored(q: &Q, path: NodeId) -> bool {
    ["anchored", "mark", "mark-index", "mark-top", "marks-all", "marks-name"]
        .iter()
        .any(|k| q.prop(path, k).is_some())
}

/// Whether a path (a `path` operand or a branch tail) stays off
/// the tree's enumeration: safe axes throughout, ascent only at
/// the tail, nothing anchored, nothing unsafe below.
fn path_is_safe(q: &Q, path: NodeId, steps: &[NodeId]) -> bool {
    if anchored(q, path) {
        return false;
    }
    let mut ascended = false;
    for &s in steps {
        let axis = q.text(s, "axis").unwrap_or_default();
        if ASCENT_AXES.contains(&axis.as_str()) {
            ascended = true;
            continue;
        }
        if ascended || !SAFE_AXES.contains(&axis.as_str()) {
            return false;
        }
        // Predicates on the step: their paths must be safe too.
        for p in q.kids_named(s, "predicate") {
            if !pred_is_safe(q, p) {
                return false;
            }
        }
    }
    true
}

fn pred_is_safe(q: &Q, pred: NodeId) -> bool {
    for n in q.all_below(pred) {
        let name = q.name(n);
        if UNSAFE_KINDS.contains(&name.as_str()) {
            return false;
        }
        if name == "path" {
            let steps: Vec<NodeId> = q.kids(n).into_iter().filter(|c| q.name(*c) == "step" || q.name(*c) == "group").collect();
            for &s in &steps {
                if q.name(s) == "group" {
                    // A group's alternatives are steps of their own.
                    for alt in q.kids_named(s, "alt") {
                        let inner: Vec<NodeId> = q.kids(alt);
                        if !path_is_safe(q, n, &inner) {
                            return false;
                        }
                    }
                }
            }
            let plain: Vec<NodeId> = steps.iter().copied().filter(|s| q.name(*s) == "step").collect();
            if !path_is_safe(q, n, &plain) {
                return false;
            }
        }
    }
    true
}

/// The column a page property maps to, and whether it is numeric.
fn column(key: &str) -> Option<(&'static str, bool)> {
    Some(match key {
        "title" => ("p.title", false),
        "path" => ("p.path", false),
        "url" | "href" => ("p.url", false),
        "name" => ("p.name", false),
        "category" => ("p.category", false),
        "description" => ("p.description", false),
        "depth" => ("p.depth", true),
        "in_degree" | "in-degree" => ("p.in_degree", true),
        "out_degree" | "out-degree" => ("p.out_degree", true),
        "mutual_degree" | "mutual-degree" => ("p.mutual_degree", true),
        "pagerank" => ("p.pagerank", true),
        "redlinks" => ("p.redlinks", true),
        _ => return None,
    })
}

/// The required literal factors of a regex: every substring a
/// match must contain, from the top-level concatenation.
pub fn regex_factors(re: &str) -> Vec<String> {
    use regex_syntax::hir::{Hir, HirKind};
    fn walk(h: &Hir, out: &mut Vec<String>) {
        match h.kind() {
            HirKind::Literal(l) => {
                if let Ok(s) = std::str::from_utf8(&l.0) {
                    out.push(s.to_string());
                }
            }
            HirKind::Concat(parts) => {
                let mut run = String::new();
                for p in parts {
                    match p.kind() {
                        HirKind::Literal(l) => {
                            if let Ok(s) = std::str::from_utf8(&l.0) {
                                run.push_str(s);
                            }
                        }
                        _ => {
                            if !run.is_empty() {
                                out.push(std::mem::take(&mut run));
                            }
                            walk(p, out);
                        }
                    }
                }
                if !run.is_empty() {
                    out.push(run);
                }
            }
            HirKind::Capture(c) => walk(&c.sub, out),
            HirKind::Repetition(r) if r.min >= 1 => walk(&r.sub, out),
            _ => {}
        }
    }
    let Ok(hir) = regex_syntax::parse(re) else { return Vec::new() };
    let mut out = Vec::new();
    walk(&hir, &mut out);
    // A one-character factor is sound but names nearly everything;
    // it is dropped (dropping a factor only widens the superset).
    out.retain(|f| f.chars().count() >= 2 && !f.trim().is_empty());
    out
}

/// Translate one predicate expression node into a conjunct, when
/// the store can say something about it.
fn translate(q: &Q, n: NodeId, sql: &mut Sql) -> Option<Conj> {
    match q.name(n).as_str() {
        "and" => {
            let parts: Vec<Option<Conj>> = q.kids(n).into_iter().map(|c| translate(q, c, sql)).collect();
            let some: Vec<Conj> = parts.into_iter().flatten().collect();
            if some.is_empty() {
                return None;
            }
            let exact = some.len() == 2 && some.iter().all(|c| c.exact);
            let note = some.iter().filter(|c| !c.exact).map(|c| c.note.clone()).collect::<Vec<_>>().join(" and ");
            Some(Conj {
                sql: format!("({})", some.iter().map(|c| c.sql.clone()).collect::<Vec<_>>().join(" AND ")),
                exact,
                note: if exact { String::new() } else if some.len() == 2 { note } else { "one side of an and".to_string() },
            })
        }
        "or" => {
            let parts: Vec<Conj> = q.kids(n).into_iter().map(|c| translate(q, c, sql)).collect::<Option<Vec<_>>>()?;
            if parts.len() != 2 {
                return None;
            }
            let exact = parts.iter().all(|c| c.exact);
            Some(Conj {
                sql: format!("({})", parts.iter().map(|c| c.sql.clone()).collect::<Vec<_>>().join(" OR ")),
                exact,
                note: parts.iter().filter(|c| !c.exact).map(|c| c.note.clone()).collect::<Vec<_>>().join(" or "),
            })
        }
        "not" => {
            let inner = q.kids(n).into_iter().next()?;
            // Only an exact inner clause negates soundly.
            let c = translate(q, inner, sql)?;
            if !c.exact {
                return None;
            }
            Some(Conj { sql: format!("NOT {}", c.sql), exact: true, note: String::new() })
        }
        "compare" => translate_compare(q, n, sql),
        "path" => translate_truthy_path(q, n, sql),
        _ => None,
    }
}

/// A path as a truthy predicate: `[<-link]`, `[->link]`.
fn translate_truthy_path(q: &Q, path: NodeId, _sql: &mut Sql) -> Option<Conj> {
    if anchored(q, path) {
        return None;
    }
    let steps = q.kids_named(path, "step");
    if steps.len() != 1 || !q.kids_named(path, "projection").is_empty() {
        return None;
    }
    let s = steps[0];
    if !q.kids_named(s, "predicate").is_empty() || !q.kids_named(s, "trait").is_empty() {
        return None;
    }
    let col = degree_of(q, s)?;
    Some(Conj { sql: format!("{col} > 0"), exact: true, note: String::new() })
}

/// `->link` / `<-link` as a degree column.
fn degree_of(q: &Q, step: NodeId) -> Option<&'static str> {
    let axis = q.text(step, "axis")?;
    let m = q.text(step, "matcher")?;
    if m != "link" {
        return None;
    }
    match axis.as_str() {
        "->" => Some("p.out_degree"),
        "<-" => Some("p.in_degree"),
        _ => None,
    }
}

/// The left side of a comparison: a page column (and whether it
/// is numeric) — `::key`, or a degree count (`<-link @| count`).
struct Lhs(&'static str, bool);

fn lhs_of(q: &Q, path: NodeId) -> Option<Lhs> {
    // `(<-link @| count)`: a piped operand — the path and its
    // stages side by side.
    if q.name(path) == "piped" {
        let kids = q.kids(path);
        if kids.len() != 2 || q.name(kids[0]) != "path" || anchored(q, kids[0]) {
            return None;
        }
        let steps = q.kids_named(kids[0], "step");
        if steps.len() != 1 || !q.kids_named(kids[0], "projection").is_empty() {
            return None;
        }
        if q.name(kids[1]) != "agg" || q.text(kids[1], "name").as_deref() != Some("count") || !q.kids(kids[1]).is_empty() {
            return None;
        }
        let col = degree_of(q, steps[0])?;
        return Some(Lhs(col, true));
    }
    if anchored(q, path) {
        return None;
    }
    let steps = q.kids_named(path, "step");
    let proj = q.kids_named(path, "projection");
    if steps.is_empty() {
        // `::key` on the page itself.
        let p = *proj.first()?;
        if q.text(p, "kind").as_deref() != Some("property") {
            return None;
        }
        let key = q.text(p, "key")?;
        let (col, num) = column(&key)?;
        return Some(Lhs(col, num));
    }
    None
}

fn translate_compare(q: &Q, n: NodeId, sql: &mut Sql) -> Option<Conj> {
    let op = q.text(n, "op")?;
    let kids = q.kids(n);
    if kids.len() != 2 {
        return None;
    }
    let (l, r) = (kids[0], kids[1]);
    if q.name(l) != "path" && q.name(l) != "piped" {
        return None;
    }
    // A text-level reach on the left: `[//paragraph[:: *= "x"]]`
    // reflects as a path whose steps carry the predicate; that is
    // handled where the path is the whole predicate. Here the
    // path must project a page column.
    let Lhs(col, numeric) = lhs_of(q, l)?;
    match q.name(r).as_str() {
        "literal" => {
            let v = q.prop(r, "value")?;
            match (&v, numeric) {
                (Value::Int(i), true) => {
                    let p = sql.param(Param::Int(*i));
                    let sop = sql_op(&op)?;
                    Some(Conj { sql: format!("{col} {sop} {p}"), exact: true, note: String::new() })
                }
                (Value::Float(f), true) => {
                    let p = sql.param(Param::Float(*f));
                    let sop = sql_op(&op)?;
                    Some(Conj { sql: format!("{col} {sop} {p}"), exact: true, note: String::new() })
                }
                (Value::Str(s), false) => match op.as_str() {
                    "=" => {
                        let p = sql.param(Param::Str(s.clone()));
                        Some(Conj { sql: format!("{col} = {p}"), exact: true, note: String::new() })
                    }
                    "!=" => {
                        let p = sql.param(Param::Str(s.clone()));
                        Some(Conj { sql: format!("({col} IS NULL OR {col} != {p})"), exact: false, note: format!("{col} != …") })
                    }
                    "*=" => Some(Conj { sql: sql.contains(col, s), exact: true, note: String::new() }),
                    "==" => {
                        // A regex: its required literal factors.
                        let factors = regex_factors(s);
                        if factors.is_empty() {
                            return None;
                        }
                        let parts: Vec<String> = factors.iter().map(|f| sql.contains(col, f)).collect();
                        Some(Conj { sql: format!("({})", parts.join(" AND ")), exact: false, note: format!("{col} == regex") })
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        "pattern" if op == "==" || op == "=" => {
            // `"pre"*"suf"`: a LIKE pattern, a superset (case rules
            // differ per engine).
            let mut pat = String::new();
            for seg in q.kids(r) {
                match q.name(seg).as_str() {
                    "star" => pat.push('%'),
                    "literal" => pat.push_str(&like_escape(&q.text(seg, "value")?)),
                    _ => return None,
                }
            }
            Some(Conj { sql: sql.like(col, pat), exact: false, note: format!("{col} pattern") })
        }
        _ => None,
    }
}

fn sql_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "=" => "=",
        "!=" => "<>",
        "<" => "<",
        "<=" => "<=",
        ">" => ">",
        ">=" => ">=",
        _ => return None,
    })
}

/// A text-level reach as a whole predicate: a relative path of
/// descents into the page whose steps carry `[:: *= "x"]`,
/// `[:: == /re/]`, or `[::lemma = "x"]` predicates. Every literal
/// the prose must contain is a substring of the page's plain
/// text — the lowering invariant — so each becomes a substring
/// test on `page_text.plain`, a superset the engine re-verifies.
fn translate_text_reach(q: &Q, path: NodeId, sql: &mut Sql) -> Option<Conj> {
    if anchored(q, path) {
        return None;
    }
    let steps = q.kids_named(path, "step");
    if steps.is_empty() {
        return None;
    }
    let mut lits: Vec<String> = Vec::new();
    for &s in &steps {
        let axis = q.text(s, "axis")?;
        if !matches!(axis.as_str(), "/" | "//" | "//?" | "//!") {
            return None;
        }
        for pred in q.kids_named(s, "predicate") {
            for cmp in q.kids(pred) {
                if q.name(cmp) != "compare" {
                    continue;
                }
                let op = q.text(cmp, "op").unwrap_or_default();
                let kids = q.kids(cmp);
                if kids.len() != 2 || q.name(kids[0]) != "path" || q.name(kids[1]) != "literal" {
                    continue;
                }
                // The left side: bare `::` or `::lemma` on the node.
                if !q.kids_named(kids[0], "step").is_empty() {
                    continue;
                }
                let proj = q.kids_named(kids[0], "projection");
                let key = proj.first().and_then(|p| q.text(*p, "key"));
                if !matches!(key.as_deref(), None | Some("lemma")) {
                    continue;
                }
                let Some(Value::Str(lit)) = q.prop(kids[1], "value") else { continue };
                match op.as_str() {
                    "*=" => lits.push(lit),
                    "=" if key.is_some() => lits.push(lit),
                    "==" => lits.extend(regex_factors(&lit)),
                    _ => {}
                }
            }
        }
    }
    if lits.is_empty() {
        return None;
    }
    let tests: Vec<String> = lits.iter().map(|l| sql.contains("t.plain", l)).collect();
    Some(Conj {
        sql: format!("EXISTS (SELECT 1 FROM page_text t WHERE t.page_id = p.id AND {})", tests.join(" AND ")),
        exact: false,
        note: "the text-level reach".to_string(),
    })
}

/// A trait clause on the enumerating step.
fn translate_trait(q: &Q, t: NodeId, sql: &mut Sql) -> Option<Conj> {
    let Some(Value::List(alts)) = q.prop(t, "alts") else { return None };
    let mut parts = Vec::new();
    for a in alts {
        let Value::Str(a) = a else { return None };
        let part = if a == "page" || a == "category" {
            "1 = 1".to_string()
        } else if a == "hiddencat" || a == "disambiguation" || a == "noindex" {
            // A page property rides in the tags column, quoted.
            sql.contains("p.tags", &format!("\"{a}\""))
        } else if a == "orphan" {
            "p.in_degree = 0".to_string()
        } else if let Some(x) = a.strip_prefix("tag:") {
            let p = sql.param(Param::Str(x.to_string()));
            format!("EXISTS (SELECT 1 FROM page_terms pt JOIN pages r ON r.id = pt.term_id WHERE pt.page_id = p.id AND r.kind = 'tag' AND r.path = {p})")
        } else if let Some(x) = a.strip_prefix("category:") {
            let p = sql.param(Param::Str(x.to_string()));
            format!("EXISTS (SELECT 1 FROM page_terms pt JOIN pages r ON r.id = pt.term_id WHERE pt.page_id = p.id AND r.kind = 'category' AND r.path = {p})")
        } else {
            // A trait the store does not index (a text-level trait
            // on the document, an alias): no constraint.
            return None;
        };
        parts.push(part);
    }
    Some(Conj { sql: format!("({})", parts.join(" OR ")), exact: true, note: String::new() })
}

/// Plan `query` for a store speaking `dialect`. `model_in_force`
/// (a `--model` over the level) keeps the scan: a model may
/// rename what the planner reads.
pub fn plan(query: &str, dialect: Dialect, model_in_force: bool) -> Plan {
    if model_in_force {
        return Plan::scan("a model file is in force");
    }
    let arbor = match QueryArbor::parse(query) {
        Ok(a) => a,
        Err(e) => return Plan::scan(format!("parse: {e}")),
    };
    let q = Q { a: &arbor };
    let root = q.a.root();
    let queries = q.kids_named(root, "query");
    if queries.len() != 1 {
        return Plan::scan("not one query");
    }
    let qn = queries[0];
    if !q.kids_named(qn, "query").is_empty() {
        return Plan::scan("a correlation (<=>) is present");
    }
    let branches = q.kids_named(qn, "branch");
    if branches.len() != 1 {
        return Plan::scan("more than one branch");
    }
    let branch = branches[0];
    let elems: Vec<NodeId> = q.kids(branch).into_iter().filter(|c| q.name(*c) != "projection").collect();
    // The enumerating step: `//page` with a prefix of child steps
    // (`/sites/<host>/pages`, a directory) before it.
    let mut host: Option<String> = None;
    let mut enumerating: Option<usize> = None;
    let mut prev_matcher = String::new();
    // The prefix steps' matchers: the Full rung needs the whole
    // container (nothing, or exactly sites/<host>/pages).
    let mut prefix: Vec<String> = Vec::new();
    for (i, &e) in elems.iter().enumerate() {
        if q.name(e) != "step" {
            return Plan::scan(format!("a {} before the enumerating step", q.name(e)));
        }
        let axis = q.text(e, "axis").unwrap_or_default();
        let matcher = q.text(e, "matcher").unwrap_or_default();
        let mkind = q.text(e, "matcher-kind").unwrap_or_default();
        if (axis == "//" || axis == "//?" || axis == "//!") && (matcher == "page" || matcher == "category") && mkind == "name" {
            enumerating = Some(i);
            break;
        }
        if !matches!(axis.as_str(), "/" | "//" | "//?" | "//!") || !q.kids_named(e, "predicate").is_empty() || !q.kids_named(e, "trait").is_empty() {
            return Plan::scan("a prefix step with tests before //page");
        }
        if prev_matcher == "sites" && mkind == "name" {
            host = Some(matcher.clone());
        }
        prefix.push(matcher.clone());
        prev_matcher = matcher;
    }
    let whole_container = prefix.is_empty()
        || (prefix.len() == 3 && prefix[0] == "sites" && prefix[2] == "pages")
        || (prefix.len() == 2 && prefix[0] == "sites")
        || (prefix.len() == 1 && prefix[0] == "sites");
    let Some(ei) = enumerating else {
        return Plan::scan("no //page enumeration");
    };
    let estep = elems[ei];
    let kind = q.text(estep, "matcher").unwrap_or_else(|| "page".to_string());
    // Everything after: safe axes only, nothing that re-enters.
    let tail: Vec<NodeId> = elems[ei + 1..].to_vec();
    for &e in &tail {
        let name = q.name(e);
        if name == "group" {
            for alt in q.kids_named(e, "alt") {
                let inner: Vec<NodeId> = q.kids(alt);
                if inner.iter().any(|s| q.name(*s) != "step") || !path_is_safe(&q, e, &inner) {
                    return Plan::scan("a group after //page with an unsafe step");
                }
            }
            for p in q.kids_named(e, "predicate") {
                if !pred_is_safe(&q, p) {
                    return Plan::scan("a group predicate that re-enters the tree");
                }
            }
            continue;
        }
        if name != "step" {
            return Plan::scan(format!("a {name} after //page"));
        }
        let axis = q.text(e, "axis").unwrap_or_default();
        if !SAFE_AXES.contains(&axis.as_str()) {
            return Plan::scan(format!("the {axis} axis after //page re-enters the tree"));
        }
        for p in q.kids_named(e, "predicate") {
            if !pred_is_safe(&q, p) {
                return Plan::scan("a later predicate that re-enters the tree");
            }
        }
    }
    // The pipeline: no stage may navigate from the root.
    let pipeline = q.kids_named(qn, "pipeline").into_iter().next();
    let mut stages: Vec<NodeId> = Vec::new();
    if let Some(p) = pipeline {
        stages = q.kids(p);
        for s in &stages {
            let name = q.name(*s);
            if matches!(name.as_str(), "subcontext" | "push" | "expr-push") {
                return Plan::scan(format!("a {name} stage"));
            }
            for n in q.all_below(*s) {
                if q.name(n) == "path" && anchored(&q, n) {
                    return Plan::scan("an anchored path in the pipeline");
                }
                if UNSAFE_KINDS.contains(&q.name(n).as_str()) {
                    return Plan::scan(format!("a {} in the pipeline", q.name(n)));
                }
            }
        }
    }
    // The enumerating step's own tests.
    let preds = q.kids_named(estep, "predicate");
    if preds.iter().any(|p| q.text(*p, "kind").as_deref() != Some("expr")) {
        return Plan::scan("a positional predicate on //page");
    }
    for &p in &preds {
        if !pred_is_safe(&q, p) {
            return Plan::scan("a predicate on //page that re-enters the tree");
        }
    }
    let mut sql = Sql { dialect, params: Vec::new() };
    // The site clause first: parameters are positional, and the
    // clause text leads.
    let mut where_parts = vec![format!("p.kind = '{kind}'")];
    if let Some(h) = &host
        && h != "*"
    {
        let p = sql.param(Param::Str(h.clone()));
        where_parts.push(format!("p.site_id IN (SELECT id FROM sites WHERE host = {p})"));
    }
    let mut conjs: Vec<Conj> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for t in q.kids_named(estep, "trait") {
        match translate_trait(&q, t, &mut sql) {
            Some(c) => conjs.push(c),
            None => dropped.push("a trait the store does not index".to_string()),
        }
    }
    for &p in &preds {
        let Some(expr) = q.kids(p).into_iter().next() else { continue };
        let c = match q.name(expr).as_str() {
            "path" => translate_text_reach(&q, expr, &mut sql).or_else(|| translate_truthy_path(&q, expr, &mut sql)),
            _ => translate(&q, expr, &mut sql),
        };
        match c {
            Some(c) => conjs.push(c),
            None => dropped.push("a predicate the store cannot express".to_string()),
        }
    }
    let translated = conjs.len();
    for c in &conjs {
        where_parts.push(c.sql.clone());
    }
    let where_sql = where_parts.join(" AND ");
    let all_exact = dropped.is_empty() && conjs.iter().all(|c| c.exact);
    let unverified: Vec<String> = conjs.iter().filter(|c| !c.exact).map(|c| c.note.clone()).chain(dropped.iter().cloned()).collect();
    // Full: a bare count over the enumeration, everything exact.
    let bare_count = tail.is_empty()
        && q.kids_named(branch, "projection").is_empty()
        && stages.len() == 1
        && q.name(stages[0]) == "agg"
        && q.text(stages[0], "name").as_deref() == Some("count")
        && q.kids(stages[0]).is_empty();
    if bare_count && all_exact && whole_container {
        return Plan {
            rung: Rung::Full,
            where_sql,
            params: sql.params,
            reason: format!("count over {translated} exact conjunct(s)"),
            unverified: Vec::new(),
            limit: None,
        };
    }
    // `@| top(n; ::col)` first in the pipeline, over the whole
    // container with exact tests only: the store names the n rows
    // the engine's stable sort would keep.
    let limit = (tail.is_empty() && q.kids_named(branch, "projection").is_empty() && all_exact && whole_container)
        .then(|| stages.first().and_then(|s| limit_of(&q, *s)))
        .flatten();
    if let Some(l) = limit {
        return Plan {
            rung: Rung::Prefilter,
            where_sql,
            params: sql.params,
            reason: format!(
                "the {} {} row(s) by {} over {translated} exact conjunct(s)",
                l.n,
                if l.descending { "top" } else { "bottom" },
                l.column
            ),
            unverified: Vec::new(),
            limit: Some(l),
        };
    }
    // Below a directory the walk is already narrow; a candidate
    // set pays only when it spares the text level.
    let text_reach = conjs.iter().any(|c| c.note == "the text-level reach");
    if !whole_container && !text_reach {
        return Plan::scan("a subtree prefix: the walk is narrow already");
    }
    if translated == 0 {
        return Plan::scan(if dropped.is_empty() {
            if bare_count && !whole_container { "a count below a directory: the tree walk".to_string() } else { "no predicate to push".to_string() }
        } else {
            dropped.join("; ")
        });
    }
    Plan {
        rung: Rung::Prefilter,
        where_sql,
        params: sql.params,
        reason: format!(
            "{translated} conjunct(s) name the candidates{}",
            if unverified.is_empty() { String::new() } else { format!("; the engine verifies {}", unverified.join(", ")) }
        ),
        unverified,
        limit: None,
    }
}

/// `top(n; ::col)` / `bottom(n; ::col)` on a numeric page column.
fn limit_of(q: &Q, stage: NodeId) -> Option<Limit> {
    if q.name(stage) != "agg" {
        return None;
    }
    let name = q.text(stage, "name")?;
    let descending = match name.as_str() {
        "top" => true,
        "bottom" => false,
        _ => return None,
    };
    let kids = q.kids(stage);
    if kids.len() != 2 || q.name(kids[0]) != "literal" || q.name(kids[1]) != "path" {
        return None;
    }
    let Some(Value::Int(n)) = q.prop(kids[0], "value") else { return None };
    if n < 0 {
        return None;
    }
    let Lhs(col, numeric) = lhs_of(q, kids[1])?;
    if !numeric {
        return None;
    }
    Some(Limit { column: col.to_string(), descending, n })
}
