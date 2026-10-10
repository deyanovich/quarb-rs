//! The static plan: what a query will touch, read off the syntax
//! tree before anything runs — per branch, per step, the plan the
//! executor would hand the adapter ([`StepPlan`]), the literal
//! path from the root to the step's base where one is known, and
//! what the adapter says it would apply from its catalog
//! ([`AstAdapter::select_plan`]). A host projects the touch set
//! from it: a root-anchored branch names its first child, which
//! under a mount table is the mount; a step with no known scope
//! (an unanchored search, a step past a predicate or a group)
//! lands anywhere.

use crate::adapter::{AstAdapter, Conjunct, Estimate, PlanAxis, StepPlan};
use crate::adapter::{Cap, Cmp};
use crate::ast::{Anchor, Axis, Matcher, PathElem, Predicate, Query, Reach};
use crate::value::Value;
use std::fmt::Write as _;

/// A bound the static plan is held to before a run: the most nodes,
/// bytes and requests the estimate may reach (each `None` is
/// unbounded), and whether an unknown estimate — a step with no
/// estimator, or an upper end of `u64::MAX` (a listing that
/// stopped early, a file whose node count no header says) — is
/// itself over the budget (`strict`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Budget {
    pub nodes: Option<u64>,
    pub bytes: Option<u64>,
    pub requests: Option<u32>,
    pub strict: bool,
}

impl Budget {
    /// Where the estimators may stop counting under this budget.
    pub fn cap(&self) -> Cap {
        Cap {
            nodes: self.nodes,
            bytes: self.bytes,
        }
    }

    /// Why `plan` exceeds this budget, if it does: the refusal's
    /// message, naming the bound and the estimate's end that
    /// passes it (the lower end when even the least already does).
    pub fn exceeded_by(&self, plan: &Plan) -> Option<String> {
        let Some(e) = plan.estimate() else {
            return self.strict.then(|| {
                "the plan's cost is unknown (no estimator answers) and the budget is strict"
                    .to_string()
            });
        };
        if let Some(max) = self.nodes {
            if e.nodes.0 > max {
                return Some(if e.nodes.0 == e.nodes.1 {
                    format!(
                        "the plan estimates {} nodes; the budget allows {max}",
                        e.nodes.0
                    )
                } else {
                    format!(
                        "the plan reaches at least {} nodes; the budget allows {max}",
                        e.nodes.0
                    )
                });
            }
            if e.nodes.1 == u64::MAX {
                if self.strict {
                    return Some(format!(
                        "the plan's nodes are unbounded (at least {}) and the budget is strict",
                        e.nodes.0
                    ));
                }
            } else if e.nodes.1 > max {
                return Some(format!(
                    "the plan estimates up to {} nodes; the budget allows {max}",
                    e.nodes.1
                ));
            }
        }
        if let Some(max) = self.bytes
            && let Some(bytes) = e.bytes
        {
            if bytes.0 > max {
                return Some(if bytes.0 == bytes.1 {
                    format!(
                        "the plan estimates {} bytes; the budget allows {max}",
                        bytes.0
                    )
                } else {
                    format!(
                        "the plan reaches at least {} bytes; the budget allows {max}",
                        bytes.0
                    )
                });
            }
            if bytes.1 == u64::MAX {
                if self.strict {
                    return Some(format!(
                        "the plan's bytes are unbounded (at least {}) and the budget is strict",
                        bytes.0
                    ));
                }
            } else if bytes.1 > max {
                return Some(format!(
                    "the plan estimates up to {} bytes; the budget allows {max}",
                    bytes.1
                ));
            }
        }
        if let Some(max) = self.requests
            && let Some(n) = e.requests
            && n > max
        {
            return Some(format!(
                "the plan makes {n} requests; the budget allows {max}"
            ));
        }
        None
    }
}

/// One navigation step as the plan sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStep {
    /// The axis the fast path covers; `None` for one it does not
    /// (parent, sibling, links): the step walks.
    pub axis: Option<PlanAxis>,
    /// The literal name the step matches, when it is one.
    pub name: Option<String>,
    /// The conjuncts lowered, in order (today the leading
    /// `[::prop OP literal]`): the property, the comparison, the
    /// literal.
    pub conjuncts: Vec<(String, Cmp, Value)>,
    /// The step's `<trait>` filters, lowered when each is one name.
    pub traits: Vec<String>,
    /// How many predicates the step carries in all; those beyond
    /// the lowered conjuncts run engine-side.
    pub predicates: usize,
    /// The literal names from the root to the step's base, when
    /// every step before it was a bare child hop by name; `None`
    /// when the base is not known statically.
    pub scope: Option<Vec<String>>,
    /// Which conjuncts the adapter would apply from its catalog
    /// ([`AstAdapter::select_plan`]); `None` when it cannot say.
    pub pushed: Option<Vec<bool>>,
    /// What reaching the step would cost ([`AstAdapter::estimate`]);
    /// `None` where no estimator answers.
    pub estimate: Option<Estimate>,
}

impl PlannedStep {
    /// A bare hop: one child by its literal name, nothing tested —
    /// the way into a container, which costs nothing to price and
    /// which no store estimates on its own.
    pub fn is_bare_hop(&self) -> bool {
        self.axis == Some(PlanAxis::Child)
            && self.name.is_some()
            && self.predicates == 0
            && self.traits.is_empty()
    }

    /// The conjuncts the engine would run itself: those the
    /// adapter did not claim, plus the predicates not lowered.
    pub fn residual(&self) -> usize {
        let unclaimed = match &self.pushed {
            Some(p) => p.iter().filter(|&&b| !b).count(),
            None => self.conjuncts.len(),
        };
        unclaimed + self.predicates.saturating_sub(self.conjuncts.len())
    }
}

/// One branch of a query.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchPlan {
    /// Anchored at the root (or at the node a run starts from).
    pub anchored: bool,
    pub steps: Vec<PlannedStep>,
}

impl BranchPlan {
    /// The branch's cost: its steps' estimates composed in order
    /// ([`Estimate::then`]); unknown when a step that reads has no
    /// estimate. A bare hop ([`PlannedStep::is_bare_hop`]) with
    /// none is free: the way into a container is not a read.
    pub fn estimate(&self) -> Option<Estimate> {
        let mut acc: Option<Estimate> = None;
        for s in &self.steps {
            let e = match &s.estimate {
                Some(e) => e,
                None if s.is_bare_hop() => continue,
                None => return None,
            };
            acc = Some(match acc {
                Some(a) => a.then(e),
                None => e.clone(),
            });
        }
        acc
    }
}

/// The plan of a query: its correlations (each a query of its own),
/// its branches, and how many pipeline stages follow.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub correlations: Vec<Plan>,
    pub branches: Vec<BranchPlan>,
    pub stages: usize,
}

impl Plan {
    /// The query's cost: its correlations' and branches' estimates
    /// side by side ([`Estimate::beside`]); unknown when any is.
    pub fn estimate(&self) -> Option<Estimate> {
        let mut acc: Option<Estimate> = None;
        let parts = self
            .correlations
            .iter()
            .map(Plan::estimate)
            .chain(self.branches.iter().map(BranchPlan::estimate));
        for e in parts {
            let e = e?;
            acc = Some(match acc {
                Some(a) => a.beside(&e),
                None => e,
            });
        }
        acc
    }

    /// The plan as a kaiv document (contract `quarb/plan`), one
    /// entry per step under `/plan/@steps` with its branch and
    /// position, the estimate's fields beside it, and the query's
    /// composed estimate under `/plan/estimate` — queryable and
    /// renderable like the trace payload.
    pub fn to_kaiv(&self, query: &str) -> String {
        let mut w = String::new();
        let _ = writeln!(
            w,
            ".!kaiv\n.!verbatim\n\n# quarb/plan payload — engine-emitted.\n(/meta)"
        );
        let _ = writeln!(w, "contract={}", kaiv_str("quarb/plan"));
        let _ = writeln!(w, "version={}", kaiv_str("1"));
        let _ = writeln!(w, "query={}", kaiv_str(query));
        let _ = writeln!(w, "()\n");
        let mut n = 0;
        self.write_steps(&mut w, "", &mut n);
        if n > 0 {
            let _ = writeln!(w, "[]\n");
        }
        if let Some(e) = self.estimate() {
            let _ = writeln!(w, "(/plan/estimate)");
            write_estimate(&mut w, &e);
            let _ = writeln!(w, "()");
        }
        w
    }

    fn write_steps(&self, w: &mut String, branch: &str, n: &mut usize) {
        for (ci, c) in self.correlations.iter().enumerate() {
            c.write_steps(w, &format!("{branch}{ci}."), n);
        }
        for (bi, b) in self.branches.iter().enumerate() {
            for (si, s) in b.steps.iter().enumerate() {
                *n += 1;
                let _ = writeln!(w, "[/plan/@steps]");
                let _ = writeln!(w, "branch={}", kaiv_str(&format!("{branch}{bi}")));
                let _ = writeln!(w, "!int\nstep={si}");
                let _ = writeln!(w, "anchored={}", if b.anchored { "true" } else { "false" });
                let _ = writeln!(
                    w,
                    "axis={}",
                    kaiv_str(match s.axis {
                        Some(PlanAxis::Child) => "child",
                        Some(PlanAxis::Descendant) => "descendant",
                        None => "walk",
                    })
                );
                if let Some(name) = &s.name {
                    let _ = writeln!(w, "name={}", kaiv_str(name));
                }
                if let Some(scope) = &s.scope {
                    let _ = writeln!(w, "scope={}", kaiv_str(&format!("/{}", scope.join("/"))));
                }
                for t in &s.traits {
                    let _ = writeln!(w, "trait={}", kaiv_str(t));
                }
                for (i, (p, c, v)) in s.conjuncts.iter().enumerate() {
                    let pushed = s.pushed.as_ref().and_then(|p| p.get(i)).copied();
                    let _ = writeln!(
                        w,
                        "conjunct={}",
                        kaiv_str(&format!(
                            "::{p} {} {} ({})",
                            c.text(),
                            crate::unparse::operand_text(&crate::ast::Operand::Lit(v.clone())),
                            match pushed {
                                Some(true) => "pushed",
                                Some(false) => "residual",
                                None => "unknown",
                            }
                        ))
                    );
                }
                let _ = writeln!(w, "!int\nresidual={}", s.residual());
                if let Some(e) = &s.estimate {
                    write_estimate(w, e);
                }
            }
        }
    }
}

/// An unbounded upper end (`u64::MAX`) has no `-max` line: the
/// minimum alone says "at least".
fn write_estimate(w: &mut String, e: &Estimate) {
    let _ = writeln!(w, "!int\nnodes-min={}", e.nodes.0);
    if e.nodes.1 != u64::MAX {
        let _ = writeln!(w, "!int\nnodes-max={}", e.nodes.1);
    }
    if let Some((lo, hi)) = e.bytes {
        let _ = writeln!(w, "!int\nbytes-min={lo}");
        if hi != u64::MAX {
            let _ = writeln!(w, "!int\nbytes-max={hi}");
        }
    }
    if let Some(n) = e.requests {
        let _ = writeln!(w, "!int\nrequests={n}");
    }
    let _ = writeln!(
        w,
        "latency={}",
        kaiv_str(match e.latency {
            crate::adapter::Latency::Memory => "memory",
            crate::adapter::Latency::Local => "local",
            crate::adapter::Latency::Remote => "remote",
        })
    );
    let _ = writeln!(
        w,
        "confidence={}",
        kaiv_str(match e.confidence {
            crate::adapter::Confidence::Guess => "guess",
            crate::adapter::Confidence::Bound => "bound",
            crate::adapter::Confidence::Exact => "exact",
        })
    );
}

/// A kaiv string literal: quoted when it holds anything a bare
/// value would misread.
/// A value line's text. The payload is a verbatim document
/// (`.!verbatim`): what stands between `=` and end-of-line is the
/// value, no quoting, no escapes, `$` a plain character. A value is
/// one line, so a query written one stage per line is folded, a
/// space per line break.
fn kaiv_str(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            c => c,
        })
        .collect()
}

impl Plan {
    /// The first literal name of every anchored branch — under a
    /// mount table, the mounts the query names; a branch with an
    /// unanchored search contributes `None` (every mount).
    pub fn roots(&self) -> Vec<Option<String>> {
        let mut out = Vec::new();
        for b in self
            .branches
            .iter()
            .chain(self.correlations.iter().flat_map(|c| c.branches.iter()))
        {
            let first = b.steps.first();
            let root = match first {
                Some(s) if b.anchored && s.axis == Some(PlanAxis::Child) => s.name.clone(),
                _ => None,
            };
            if !out.contains(&root) {
                out.push(root);
            }
        }
        out
    }
}

/// The plan of `query` over `adapter`, which answers
/// [`AstAdapter::select_plan`] from its catalog and is never asked
/// for a node.
pub fn of(query: &Query, adapter: &dyn AstAdapter) -> Plan {
    of_within(query, adapter, Cap::NONE)
}

/// [`of`] with every estimator told where it may stop counting
/// ([`Budget::cap`]).
pub fn of_within(query: &Query, adapter: &dyn AstAdapter, cap: Cap) -> Plan {
    Plan {
        correlations: query
            .correlations
            .iter()
            .map(|c| of_within(c, adapter, cap))
            .collect(),
        branches: query
            .branches
            .iter()
            .map(|b| {
                let anchored = matches!(b.anchor, Anchor::Root | Anchor::Current);
                BranchPlan {
                    anchored,
                    steps: plan_steps(&b.steps, anchored, adapter, cap),
                }
            })
            .collect(),
        stages: query.pipeline.len(),
    }
}

fn plan_steps(
    elems: &[PathElem],
    anchored: bool,
    adapter: &dyn AstAdapter,
    cap: Cap,
) -> Vec<PlannedStep> {
    let mut scope: Option<Vec<String>> = anchored.then(Vec::new);
    let mut out = Vec::new();
    for elem in elems {
        let PathElem::Step(step) = elem else {
            // A group, a mark or a push: the base past it is not a
            // literal path.
            scope = None;
            continue;
        };
        let axis = match step.axis {
            Axis::Child => Some(PlanAxis::Child),
            Axis::Descendant(Reach::All) => Some(PlanAxis::Descendant),
            _ => None,
        };
        let name = match &step.matcher {
            Matcher::Name(n) => Some(n.clone()),
            _ => None,
        };
        let lowered = match step.predicates.first() {
            Some(Predicate::Expr(e)) => crate::exec::indexable_comparison(e),
            _ => None,
        };
        let conjuncts: Vec<(String, Cmp, Value)> = lowered
            .map(|(p, c, v)| vec![(p.to_string(), c, v.clone())])
            .unwrap_or_default();
        let traits: Vec<String> = crate::exec::literal_traits(step)
            .into_iter()
            .map(str::to_string)
            .collect();
        let (pushed, estimate) = match (axis, &scope) {
            (Some(axis), Some(scope)) => {
                let plan = StepPlan {
                    traits: traits.iter().map(String::as_str).collect(),
                    cap,
                    axis,
                    name: name.as_deref(),
                    conjuncts: conjuncts
                        .iter()
                        .map(|(p, c, v)| match (c, v) {
                            (Cmp::Eq, Value::Str(_)) => Conjunct::Equals(p, v),
                            _ => Conjunct::Compare(p, *c, v),
                        })
                        .collect(),
                    limit: None,
                    order: None,
                };
                let scope: Vec<&str> = scope.iter().map(String::as_str).collect();
                (
                    adapter.select_plan(&scope, &plan),
                    adapter.estimate(&scope, &plan),
                )
            }
            _ => (None, None),
        };
        out.push(PlannedStep {
            axis,
            name: name.clone(),
            conjuncts,
            traits,
            predicates: step.predicates.len(),
            scope: scope.clone(),
            pushed,
            estimate,
        });
        // The base stays literal only through a bare child hop by
        // name.
        scope = match (
            axis,
            name,
            step.predicates.is_empty() && step.traits.is_empty(),
            scope,
        ) {
            (Some(PlanAxis::Child), Some(n), true, Some(mut s)) => {
                s.push(n);
                Some(s)
            }
            _ => None,
        };
    }
    out
}
