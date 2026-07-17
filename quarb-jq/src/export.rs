//! The reverse direction: Quarb → jq.
//!
//! Walks the query's *reflection arbor* — the locked vocabulary is
//! the stable surface for query-rewriting tooling, and this is
//! tooling — and emits an equivalent jq filter, refusing what jq
//! cannot express. Fragments and pure macros expand before
//! reflection, so they translate for free.
//!
//! The translatable subset: child hops (`/name` → `.name`, `/*` →
//! `.[]`), terminal value projections (bare `::`), predicates
//! (comparisons, `and`/`or`/`not`, truthiness, `=~` → `test()`,
//! `*=` → `contains()`, indexes and ranges), unions (`,`),
//! `rec(...)` → object construction, and the aggregate family via
//! the array-collect idiom (`count` → `length`, `sum` → `add`, …).
//!
//! Notes report the standing divergences (1-based vs 0-based
//! indexing is converted; Quarb positions within the whole hop
//! result; `0`/`""` truthiness).

use crate::{JqError, Translation};
use quarb::reflect::QueryArbor;
use quarb::{AstAdapter, NodeId, Value};

/// Translate a Quarb query to a jq filter.
pub fn export(quarb: &str) -> Result<Translation, JqError> {
    let arbor =
        QueryArbor::parse(quarb).map_err(|e| JqError::Syntax(0, format!("parsing Quarb: {e}")))?;
    if has_group(&arbor) {
        return Err(JqError::Syntax(0, "path patterns (groups and quantifiers) have no jq translation".into()));
    }
    let mut ex = Exporter {
        arbor,
        notes: Vec::new(),
    };
    let query = ex.query()?;
    Ok(Translation {
        query,
        notes: ex.notes,
    })
}

/// Whether the reflected query carries any path-pattern group; the
/// walkers below count only `step` children, so an unguarded group
/// would silently vanish from the translation.
fn has_group(arbor: &QueryArbor) -> bool {
    let mut stack = vec![arbor.root()];
    while let Some(n) = stack.pop() {
        if arbor.name(n).as_deref() == Some("group") {
            return true;
        }
        stack.extend(arbor.children(n));
    }
    false
}

struct Exporter {
    arbor: QueryArbor,
    notes: Vec<String>,
}

impl Exporter {
    fn kids(&self, n: NodeId, kind: &str) -> Vec<NodeId> {
        self.arbor
            .children(n)
            .into_iter()
            .filter(|&c| self.arbor.name(c).as_deref() == Some(kind))
            .collect()
    }

    fn kid(&self, n: NodeId, kind: &str) -> Option<NodeId> {
        self.kids(n, kind).into_iter().next()
    }

    fn prop(&self, n: NodeId, key: &str) -> Option<Value> {
        self.arbor.property(n, key)
    }

    fn prop_s(&self, n: NodeId, key: &str) -> String {
        self.prop(n, key).map(|v| v.to_string()).unwrap_or_default()
    }

    fn kind(&self, n: NodeId) -> String {
        self.arbor.name(n).unwrap_or_default()
    }

    fn query(&mut self) -> Result<String, JqError> {
        let root = self.arbor.root();
        let q = self
            .kid(root, "query")
            .ok_or_else(|| JqError::Unsupported("empty query".into()))?;
        if self.kid(q, "query").is_some() {
            return Err(JqError::Unsupported(
                "correlation (<=>) has no jq equivalent".into(),
            ));
        }
        let branches = self.kids(q, "branch");
        let paths: Vec<String> = branches
            .iter()
            .map(|&b| self.branch(b))
            .collect::<Result<_, _>>()?;
        let mut out = paths.join(", ");

        if let Some(pipe) = self.kid(q, "pipeline") {
            for stage in self.arbor.children(pipe) {
                out = self.stage(stage, out)?;
            }
        }
        Ok(out)
    }

    fn branch(&mut self, b: NodeId) -> Result<String, JqError> {
        let mut out = String::new();
        for step in self.kids(b, "step") {
            out.push_str(&self.step(step)?);
        }
        if let Some(p) = self.kid(b, "projection") {
            match self.prop_s(p, "kind").as_str() {
                // The bare value projection: jq values are already
                // the node's value.
                "property" if self.prop(p, "key").is_none() => {}
                other => {
                    return Err(JqError::Unsupported(format!(
                        "the {other} projection '::{}' (JSON fields are child hops: '/name::')",
                        self.prop_s(p, "key")
                    )));
                }
            }
        }
        if out.is_empty() {
            out.push('.');
        }
        Ok(out)
    }

    fn step(&mut self, s: NodeId) -> Result<String, JqError> {
        let axis = self.prop_s(s, "axis");
        if axis != "/" {
            return Err(JqError::Unsupported(format!(
                "the '{axis}' axis (jq navigates down one level at a time)"
            )));
        }
        if self.prop(s, "leaf") == Some(Value::Bool(true)) {
            return Err(JqError::Unsupported("the leaf anchor '$'".into()));
        }
        if self.kid(s, "trait").is_some() {
            return Err(JqError::Unsupported("trait filters (<...>)".into()));
        }
        // Positional predicates fold into the step's own indexing;
        // expression predicates become select() after iteration.
        let mut positional = String::new();
        let mut selects = Vec::new();
        for p in self.kids(s, "predicate") {
            match self.prop_s(p, "kind").as_str() {
                "index" | "range" => {
                    if !positional.is_empty() {
                        return Err(JqError::Unsupported("stacked positional predicates".into()));
                    }
                    positional = self.positional(p)?;
                }
                _ => selects.push(self.pred_expr_children(p)?),
            }
        }
        let mut out = match self.prop_s(s, "matcher-kind").as_str() {
            "name" => {
                let field = format!(".{}", quote_jq_field(&self.prop_s(s, "matcher")));
                if !positional.is_empty() {
                    return Err(JqError::Unsupported(
                        "a positional predicate on a named hop".into(),
                    ));
                }
                field
            }
            "any" => {
                if positional.is_empty() {
                    "[]".to_string()
                } else {
                    positional
                }
            }
            other => {
                return Err(JqError::Unsupported(format!(
                    "{other} name matching (jq fields are literal)"
                )));
            }
        };
        for cond in selects {
            out.push_str(&format!(" | select({cond})"));
        }
        Ok(out)
    }

    /// A positional predicate as jq indexing/slicing on the array
    /// itself (replacing the `[]` iteration).
    fn positional(&mut self, p: NodeId) -> Result<String, JqError> {
        self.notes.push(
            "positional: Quarb is 1-based and positions within the whole \
             hop result; jq is 0-based per array"
                .to_string(),
        );
        match self.prop_s(p, "kind").as_str() {
            "index" => {
                let n = match self.prop(p, "value") {
                    Some(Value::Int(n)) => n,
                    _ => return Err(JqError::Unsupported("a non-integer index".into())),
                };
                Ok(if n > 0 {
                    format!("[{}]", n - 1)
                } else {
                    format!("[{n}]")
                })
            }
            _ => {
                let from = match self.prop(p, "from") {
                    Some(Value::Int(n)) => Some(n),
                    _ => None,
                };
                let to = match self.prop(p, "to") {
                    Some(Value::Int(n)) => Some(n),
                    _ => None,
                };
                if from.is_some_and(|n| n < 0) || to.is_some_and(|n| n < 0) {
                    return Err(JqError::Unsupported("negative range ends".into()));
                }
                let lo = from.map(|n| (n - 1).to_string()).unwrap_or_default();
                let hi = to.map(|n| n.to_string()).unwrap_or_default();
                Ok(format!("[{lo}:{hi}][]"))
            }
        }
    }

    /// The conjunction of a predicate node's expression children.
    fn pred_expr_children(&mut self, p: NodeId) -> Result<String, JqError> {
        let parts: Vec<String> = self
            .arbor
            .children(p)
            .into_iter()
            .map(|c| self.pred_expr(c))
            .collect::<Result<_, _>>()?;
        Ok(parts.join(" and "))
    }

    fn pred_expr(&mut self, e: NodeId) -> Result<String, JqError> {
        match self.kind(e).as_str() {
            "and" | "or" => {
                let op = self.kind(e);
                let kids: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c))
                    .collect::<Result<_, _>>()?;
                Ok(format!("({})", kids.join(&format!(" {op} "))))
            }
            "not" => {
                let inner = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("({} | not)", inner.join(" and ")))
            }
            "parens" => {
                let kids: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c))
                    .collect::<Result<_, _>>()?;
                Ok(format!("({})", kids.join(" and ")))
            }
            "compare" => {
                let op = self.prop_s(e, "op");
                let kids = self.arbor.children(e);
                if kids.len() != 2 {
                    return Err(JqError::Unsupported("a malformed comparison".into()));
                }
                let l = self.operand(kids[0])?;
                let r = self.operand(kids[1])?;
                Ok(match op.as_str() {
                    "=" => format!("{l} == {r}"),
                    "!=" => format!("{l} != {r}"),
                    "<" | "<=" | ">" | ">=" => format!("{l} {op} {r}"),
                    "=~" => format!("({l} | test({r}))"),
                    "!~" => format!("({l} | test({r}) | not)"),
                    "*=" => format!("({l} | contains({r}))"),
                    other => {
                        return Err(JqError::Unsupported(format!("the '{other}' comparison")));
                    }
                })
            }
            // A bare truthy operand.
            _ => {
                self.notes
                    .push("truthiness: 0 and \"\" are falsy in Quarb but truthy in jq".to_string());
                self.operand(e)
            }
        }
    }

    fn operand(&mut self, o: NodeId) -> Result<String, JqError> {
        match self.kind(o).as_str() {
            "literal" => {
                let v = self.prop(o, "value").unwrap_or(Value::Null);
                Ok(match self.prop_s(o, "type").as_str() {
                    "text" => format!("\"{}\"", v),
                    _ => v.to_string(),
                })
            }
            "path" => {
                let steps = self.kids(o, "step");
                let mut out = String::new();
                for s in steps {
                    out.push_str(&self.step(s)?);
                }
                if let Some(p) = self.kid(o, "projection")
                    && !(self.prop_s(p, "kind") == "property" && self.prop(p, "key").is_none())
                {
                    return Err(JqError::Unsupported(
                        "a named projection in a predicate (JSON fields are child hops)".into(),
                    ));
                }
                Ok(if out.is_empty() { ".".into() } else { out })
            }
            "arith" => {
                let op = self.prop_s(o, "op");
                let kids = self.arbor.children(o);
                let l = self.operand(kids[0])?;
                let r = self.operand(kids[1])?;
                Ok(match op.as_str() {
                    "+" | "-" | "*" => format!("({l} {op} {r})"),
                    "div" => format!("({l} / {r})"),
                    "mod" => format!("({l} % {r})"),
                    other => return Err(JqError::Unsupported(format!("'{other}' arithmetic"))),
                })
            }
            other => Err(JqError::Unsupported(format!(
                "the '{other}' operand (registers, topics, and captures are Quarb-side state)"
            ))),
        }
    }

    fn stage(&mut self, s: NodeId, input: String) -> Result<String, JqError> {
        match self.kind(s).as_str() {
            "filter" => {
                let cond = self.pred_expr_children(s)?;
                Ok(format!("{input} | select({cond})"))
            }
            "expr" => {
                let kids = self.arbor.children(s);
                let e = self.operand(kids[0])?;
                Ok(format!("{input} | {e}"))
            }
            "func" => {
                let name = self.prop_s(s, "name");
                match name.as_str() {
                    "upper" => Ok(format!("{input} | ascii_upcase")),
                    "lower" => Ok(format!("{input} | ascii_downcase")),
                    "rec" | "record" => {
                        let fields = self.record_fields(s)?;
                        Ok(format!("{input} | {{{fields}}}"))
                    }
                    other => Err(JqError::Unsupported(format!(
                        "the '{other}' pipeline function"
                    ))),
                }
            }
            "agg" => {
                let name = self.prop_s(s, "name");
                self.notes.push(
                    "array collect: jq boxes the stream into one array for the aggregate"
                        .to_string(),
                );
                Ok(match name.as_str() {
                    "count" => format!("[{input}] | length"),
                    "sum" => format!("[{input}] | add"),
                    "min" => format!("[{input}] | min"),
                    "max" => format!("[{input}] | max"),
                    "unique" => {
                        self.notes.push(
                            "unique: jq sorts; Quarb keeps first-appearance order".to_string(),
                        );
                        format!("[{input}] | unique[]")
                    }
                    "sort" => format!("[{input}] | sort[]"),
                    "reverse" => format!("[{input}] | reverse[]"),
                    "first" => format!("[{input}] | first"),
                    "last" => format!("[{input}] | last"),
                    "join" => {
                        let sep = self
                            .kid(s, "literal")
                            .map(|l| self.prop_s(l, "value"))
                            .unwrap_or_default();
                        format!("[{input}] | join(\"{sep}\")")
                    }
                    other => {
                        return Err(JqError::Unsupported(format!("the '{other}' aggregate")));
                    }
                })
            }
            "select" => {
                let p = self.arbor.children(s)[0];
                let pos = self.positional(p)?;
                // Positional selection over the whole stream: collect.
                Ok(format!("[{input}] | .{pos}"))
            }
            other => Err(JqError::Unsupported(format!(
                "the '{other}' stage (registers and grouping are Quarb-side state)"
            ))),
        }
    }

    /// `rec(...)` fields: auto-named path projections and
    /// literal-name/value pairs.
    fn record_fields(&mut self, s: NodeId) -> Result<String, JqError> {
        let kids = self.arbor.children(s);
        let mut fields = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let k = kids[i];
            if self.kind(k) == "literal" {
                let name = self.prop_s(k, "value");
                let value = self.operand(kids[i + 1])?;
                fields.push(format!("{}: {value}", quote_jq_field(&name)));
                i += 2;
            } else {
                // Auto-named: the path's last field name.
                let expr = self.operand(k)?;
                let name = expr
                    .rsplit('.')
                    .next()
                    .unwrap_or("value")
                    .trim_end_matches(['[', ']'])
                    .to_string();
                fields.push(format!("{}: {expr}", quote_jq_field(&name)));
                i += 1;
            }
        }
        Ok(fields.join(", "))
    }
}

/// jq field spelling: bare where legal, quoted otherwise.
fn quote_jq_field(name: &str) -> String {
    let bare = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().unwrap().is_ascii_digit();
    if bare {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}
