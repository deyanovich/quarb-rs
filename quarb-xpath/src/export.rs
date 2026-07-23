//! The reverse direction: Quarb → XPath 1.0.
//!
//! Walks the query's *reflection arbor* — the locked vocabulary is
//! the stable surface for query-rewriting tooling — and emits an
//! equivalent XPath 1.0 expression, refusing what XPath cannot
//! express. Fragments and pure macros expand before reflection, so
//! they translate for free.
//!
//! The translatable subset: downward navigation (`/`, `//`),
//! parents and ancestors (`\` → `parent::`, `\\` → `ancestor::`),
//! the sibling reach family (`>>` → `following-sibling::`, `<<` →
//! `preceding-sibling::`), predicates (comparisons over attributes
//! and child text, `and`/`or`/`not`, existence, indexes → `[n]` /
//! `[last()]`, ranges → `position()`), the leaf anchor →
//! `[not(*)]`, unions (`|`), terminal projections (`::a` → `/@a`,
//! `::text` → `/text()`), and the `count` / `sum` aggregates as
//! function wrappers.
//!
//! Notes carry the standing divergences (Quarb's `//` is proper
//! descendants where XPath's is descendant-or-self; `::text`
//! concatenates where `text()` selects immediate text nodes).

use crate::{Translation, XPathError};
use quarb::reflect::QueryArbor;
use quarb::{AstAdapter, NodeId, Value};

/// Translate a Quarb query to an XPath 1.0 expression.
pub fn export(quarb: &str) -> Result<Translation, XPathError> {
    let arbor = QueryArbor::parse(quarb)
        .map_err(|e| XPathError::Syntax(0, format!("parsing Quarb: {e}")))?;
    if has_group(&arbor) {
        return Err(XPathError::Syntax(0, "path patterns (groups and quantifiers) have no XPath 1.0 translation".into()));
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

    fn query(&mut self) -> Result<String, XPathError> {
        let root = self.arbor.root();
        let q = self
            .kid(root, "query")
            .ok_or_else(|| XPathError::Unsupported("empty query".into()))?;
        if self.kid(q, "query").is_some() {
            return Err(XPathError::Unsupported(
                "correlation (<=>) has no XPath equivalent".into(),
            ));
        }
        let branches = self.kids(q, "branch");
        let paths: Vec<String> = branches
            .iter()
            .map(|&b| self.branch(b))
            .collect::<Result<_, _>>()?;
        let mut out = paths.join(" | ");

        if let Some(pipe) = self.kid(q, "pipeline") {
            let stages = self.arbor.children(pipe);
            if stages.len() != 1 {
                return Err(XPathError::Unsupported(
                    "a multi-stage pipeline (XPath 1.0 has no pipeline)".into(),
                ));
            }
            let s = stages[0];
            if self.kind(s) != "agg" {
                return Err(XPathError::Unsupported(format!(
                    "the '{}' stage (XPath 1.0 has no pipeline)",
                    self.kind(s)
                )));
            }
            out = match self.prop_s(s, "name").as_str() {
                "count" => format!("count({out})"),
                "sum" => format!("sum({out})"),
                other => {
                    return Err(XPathError::Unsupported(format!(
                        "the '{other}' aggregate (XPath 1.0 has count() and sum())"
                    )));
                }
            };
        }
        Ok(out)
    }

    fn branch(&mut self, b: NodeId) -> Result<String, XPathError> {
        let mut out = String::new();
        for step in self.kids(b, "step") {
            out.push_str(&self.step(step)?);
        }
        if let Some(p) = self.kid(b, "projection") {
            out.push_str(&self.projection(p)?);
        }
        if out.is_empty() {
            out.push('.');
        }
        Ok(out)
    }

    fn projection(&mut self, p: NodeId) -> Result<String, XPathError> {
        match self.prop_s(p, "kind").as_str() {
            "property" => match self.prop(p, "key") {
                Some(k) => Ok(format!("/@{k}")),
                None => {
                    self.notes.push(
                        "text(): Quarb's bare :: is the concatenated descendant text; \
                         XPath text() selects immediate text nodes (equal on leaf elements)"
                            .to_string(),
                    );
                    Ok("/text()".to_string())
                }
            },
            other => Err(XPathError::Unsupported(format!(
                "the {other} metadata projection"
            ))),
        }
    }

    fn step(&mut self, s: NodeId) -> Result<String, XPathError> {
        let name = match self.prop_s(s, "matcher-kind").as_str() {
            "name" => self.prop_s(s, "matcher"),
            "any" => "*".to_string(),
            other => {
                return Err(XPathError::Unsupported(format!(
                    "{other} name matching (XPath names are literal)"
                )));
            }
        };
        let mut out = match self.prop_s(s, "axis").as_str() {
            "/" => format!("/{name}"),
            "//" => {
                self.notes.push(
                    "//: Quarb selects proper descendants; XPath's // is \
                     descendant-or-self"
                        .to_string(),
                );
                format!("//{name}")
            }
            "\\" => format!("/parent::{name}"),
            "\\\\" => format!("/ancestor::{name}"),
            ">>" => format!("/following-sibling::{name}"),
            "<<" => format!("/preceding-sibling::{name}"),
            other => {
                return Err(XPathError::Unsupported(format!(
                    "the '{other}' axis (XPath 1.0 lacks it, or the reach variant)"
                )));
            }
        };
        if self.kid(s, "trait").is_some() {
            return Err(XPathError::Unsupported("trait filters (<...>)".into()));
        }
        for p in self.kids(s, "predicate") {
            out.push_str(&self.predicate(p)?);
        }
        if self.prop(s, "leaf") == Some(Value::Bool(true)) {
            out.push_str("[not(*)]");
        }
        Ok(out)
    }

    fn predicate(&mut self, p: NodeId) -> Result<String, XPathError> {
        match self.prop_s(p, "kind").as_str() {
            "index" => match self.prop(p, "value") {
                Some(Value::Int(n)) if n > 0 => Ok(format!("[{n}]")),
                Some(Value::Int(-1)) => Ok("[last()]".to_string()),
                Some(Value::Int(n)) => Ok(format!("[last() - {}]", -n - 1)),
                _ => Err(XPathError::Unsupported("a non-integer index".into())),
            },
            "range" => {
                let from = match self.prop(p, "from") {
                    Some(Value::Int(n)) => Some(n),
                    _ => None,
                };
                let to = match self.prop(p, "to") {
                    Some(Value::Int(n)) => Some(n),
                    _ => None,
                };
                if from.is_some_and(|n| n < 0) || to.is_some_and(|n| n < 0) {
                    return Err(XPathError::Unsupported("negative range ends".into()));
                }
                Ok(match (from, to) {
                    (Some(a), Some(b)) => {
                        format!("[position() >= {a} and position() <= {b}]")
                    }
                    (Some(a), None) => format!("[position() >= {a}]"),
                    (None, Some(b)) => format!("[position() <= {b}]"),
                    (None, None) => String::new(),
                })
            }
            _ => {
                let parts: Vec<String> = self
                    .arbor
                    .children(p)
                    .into_iter()
                    .map(|c| self.pred_expr(c))
                    .collect::<Result<_, _>>()?;
                Ok(format!("[{}]", parts.join(" and ")))
            }
        }
    }

    fn pred_expr(&mut self, e: NodeId) -> Result<String, XPathError> {
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
                let inner: Vec<String> = self
                    .arbor
                    .children(e)
                    .into_iter()
                    .map(|c| self.pred_expr(c))
                    .collect::<Result<_, _>>()?;
                Ok(format!("not({})", inner.join(" and ")))
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
                let l = self.operand(kids[0])?;
                let r = self.operand(kids[1])?;
                Ok(match op.as_str() {
                    "=" | "<" | "<=" | ">" | ">=" => format!("{l} {op} {r}"),
                    "!=" => format!("{l} != {r}"),
                    "*=" => format!("contains({l}, {r})"),
                    "=~" => {
                        return Err(XPathError::Unsupported(
                            "regex matching (XPath 1.0 has contains/starts-with)".into(),
                        ));
                    }
                    other => {
                        return Err(XPathError::Unsupported(format!("the '{other}' comparison")));
                    }
                })
            }
            _ => self.operand(e),
        }
    }

    fn operand(&mut self, o: NodeId) -> Result<String, XPathError> {
        match self.kind(o).as_str() {
            "literal" => {
                let v = self.prop(o, "value").unwrap_or(Value::Null);
                Ok(match self.prop_s(o, "type").as_str() {
                    "text" => format!("\"{v}\""),
                    _ => v.to_string(),
                })
            }
            "path" => {
                let steps = self.kids(o, "step");
                let mut out = String::new();
                for s in &steps {
                    out.push_str(&self.step(*s)?);
                }
                let out = out.trim_start_matches('/').to_string();
                match self.kid(o, "projection") {
                    Some(p) => {
                        let proj = self.projection(p)?;
                        Ok(if out.is_empty() {
                            proj.trim_start_matches('/').to_string()
                        } else {
                            format!("{out}{proj}")
                        })
                    }
                    None => Ok(if out.is_empty() { ".".into() } else { out }),
                }
            }
            other => Err(XPathError::Unsupported(format!(
                "the '{other}' operand (registers and topics are Quarb-side state)"
            ))),
        }
    }
}
