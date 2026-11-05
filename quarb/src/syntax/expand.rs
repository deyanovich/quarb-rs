//! What the parser asks of the layer that can evaluate: a macro's
//! expansion (its body run over its bindings, and over the dataset
//! when it is data-aware), and the register usage of a fragment,
//! which the graph reads off the query's reflection. The parser
//! holds an expander and never evaluates anything itself.
use crate::syntax::ast::{
    Anchor, Arg, ArithOp, Axis, Branch, CmpOp, Command, ConcatMode, Operand, PathElem, PredExpr,
    Predicate, Projection, Query, Reach, RegRef, TraitClause,
};
use crate::value::Value;

/// A macro parameter binding for the expansion arbor: one argument
/// form, or the collected remainder (`@rest`).
pub(crate) enum MacroBinding {
    One(Operand),
    Rest(Vec<Operand>),
}

/// A reflected node's own construct: the AST subtree the node
/// stands for, which a structural answer grafts as a copy (design
/// paper: a handle from each reflected node to its AST node, so a
/// graft is a clone and not a walk back from properties).
#[derive(Debug, Clone)]
pub enum Form {
    Query(Query),
    Pipeline(Vec<Command>),
    Branch(Branch),
    Elem(PathElem),
    Predicate(Predicate),
    PredExpr(PredExpr),
    Operand(Operand),
    Command(Command),
    Projection(Projection),
    Arg(Arg),
    /// A trait block on a hop (`<link>`, `<block || inline>`).
    Trait(TraitClause),
}

impl Form {
    /// The construct's canonical text.
    pub(crate) fn text(&self) -> String {
        use crate::syntax::unparse as u;
        match self {
            Form::Query(q) => u::unparse(q),
            Form::Pipeline(cmds) => cmds.iter().map(u::command).collect::<Vec<_>>().join(" "),
            Form::Branch(b) => u::branch(b),
            Form::Elem(e) => u::elem(e),
            Form::Predicate(p) => u::predicate(p),
            Form::PredExpr(e) => u::pred_expr(e),
            Form::Operand(o) => u::operand_text(o),
            Form::Command(c) => u::command(c),
            Form::Projection(p) => u::projection(p),
            Form::Arg(a) => u::arg_text(a),
            Form::Trait(t) => u::trait_suffix(std::slice::from_ref(t)),
        }
    }

    /// What the form is, for a refusal.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Form::Query(_) => "a query",
            Form::Pipeline(_) => "a pipeline",
            Form::Branch(_) => "a branch",
            Form::Elem(PathElem::Step(_)) => "a step",
            Form::Elem(PathElem::Group(_)) => "a group",
            Form::Elem(PathElem::Mark(_)) => "a mark",
            Form::Elem(PathElem::Push { .. }) => "a push",
            Form::Elem(PathElem::Hole(_)) => "an expansion",
            Form::Elem(PathElem::Param(_)) => "a parameter",
            Form::Predicate(_) | Form::PredExpr(_) => "a predicate",
            Form::Operand(_) => "an operand",
            Form::Command(_) => "a command",
            Form::Projection(_) => "a projection",
            Form::Arg(_) => "an argument",
            Form::Trait(_) => "a trait",
        }
    }
}

/// One value of a macro body's final context: a reflected node or
/// a quarq, grafted as structure, or a text, parsed as today
/// (design paper: a body may answer in structure).
pub(crate) enum Answer {
    Form(Form),
    Text(String),
}

/// What a form grafts where a path element stands: the elements it
/// continues the walk with, the predicates refining the element
/// before it, the projection ending the branch.
pub(crate) struct Walk {
    pub(crate) elems: Vec<PathElem>,
    pub(crate) predicates: Vec<Predicate>,
    pub(crate) projection: Option<Projection>,
}

impl Form {
    /// The form where one operand stands (design paper: Grafting).
    pub(crate) fn into_operand(self) -> Result<Operand, String> {
        let rel = |b: Branch| Operand::Rel {
            steps: b.steps,
            projection: b.projection,
            anchor: b.anchor,
        };
        Ok(match self {
            Form::Operand(op) | Form::Arg(Arg::Expr(op)) => op,
            Form::Arg(Arg::Lit(v)) | Form::Arg(Arg::Key(v)) => Operand::Lit(v),
            Form::Predicate(Predicate::Expr(e)) | Form::PredExpr(e) => Operand::Group(Box::new(e)),
            Form::Branch(b) => rel(b),
            Form::Query(mut q)
                if q.correlations.is_empty() && q.pipeline.is_empty() && q.branches.len() == 1 =>
            {
                rel(q.branches.pop().expect("one"))
            }
            Form::Elem(e) => Operand::Rel {
                steps: vec![e],
                projection: None,
                anchor: Anchor::Current,
            },
            Form::Projection(p) => Operand::Rel {
                steps: Vec::new(),
                projection: Some(p),
                anchor: Anchor::Current,
            },
            Form::Pipeline(mut cmds) => match cmds.as_slice() {
                [Command::Expr(_)] => match cmds.pop() {
                    Some(Command::Expr(op)) => op,
                    _ => unreachable!("matched"),
                },
                [Command::Nav(_)] => match cmds.pop() {
                    Some(Command::Nav(b)) => rel(b),
                    _ => unreachable!("matched"),
                },
                _ => Operand::Piped {
                    expr: Box::new(Operand::Seed),
                    commands: cmds,
                },
            },
            Form::Command(Command::Expr(op)) => op,
            Form::Command(Command::Nav(b)) => rel(b),
            Form::Command(c) => Operand::Piped {
                expr: Box::new(Operand::Seed),
                commands: vec![c],
            },
            other => {
                return Err(format!(
                    "{} ('{}') cannot stand as an operand",
                    other.kind(),
                    other.text()
                ));
            }
        })
    }

    /// The form after a pipe: the commands it is (design paper:
    /// Grafting); `agg` for the aggregate pipe, where a scalar
    /// command reads as its aggregate.
    pub(crate) fn into_commands(self, agg: bool) -> Result<Vec<Command>, String> {
        let nav = |b: Branch| Command::Nav(b);
        Ok(match self {
            Form::Command(c) => vec![c],
            Form::Pipeline(cmds) => cmds,
            Form::Query(q) if q.correlations.is_empty() => {
                let mut out = Vec::new();
                match q.branches.len() {
                    0 => {}
                    1 => out.push(nav(q.branches.into_iter().next().expect("one"))),
                    _ => {
                        return Err(
                            "a union cannot stand after a pipe; a hop walks one path per thread"
                                .to_string(),
                        );
                    }
                }
                out.extend(q.pipeline);
                out
            }
            Form::Branch(b) => vec![nav(b)],
            Form::Projection(p) => vec![nav(Branch {
                steps: Vec::new(),
                projection: Some(p),
                anchor: Anchor::Current,
            })],
            Form::Operand(Operand::Rel {
                steps,
                projection,
                anchor,
            })
            | Form::Arg(Arg::Expr(Operand::Rel {
                steps,
                projection,
                anchor,
            })) => vec![nav(Branch {
                steps,
                projection,
                anchor,
            })],
            Form::Operand(op) | Form::Arg(Arg::Expr(op)) => {
                vec![if agg {
                    Command::AggExpr(op)
                } else {
                    Command::Expr(op)
                }]
            }
            Form::Arg(Arg::Lit(v)) | Form::Arg(Arg::Key(v)) => vec![Command::Expr(Operand::Lit(v))],
            Form::Predicate(Predicate::Expr(e)) | Form::PredExpr(e) => {
                if agg {
                    return Err(
                        "a predicate filters per capsa; it follows '|', not '@|'".to_string()
                    );
                }
                vec![Command::Filter(e)]
            }
            Form::Predicate(p) => vec![Command::Select(p)],
            other => {
                return Err(format!(
                    "{} ('{}') cannot stand after a pipe; a walk continues at path position",
                    other.kind(),
                    other.text()
                ));
            }
        })
    }

    /// The form where a path element stands (design paper:
    /// Grafting): elements, a refining predicate, or a projection.
    pub(crate) fn into_walk(self) -> Result<Walk, String> {
        let walk = |elems, predicates, projection| Walk {
            elems,
            predicates,
            projection,
        };
        Ok(match self {
            Form::Elem(e) => walk(vec![e], Vec::new(), None),
            Form::Predicate(p) => walk(Vec::new(), vec![p], None),
            Form::PredExpr(e) => walk(Vec::new(), vec![Predicate::Expr(e)], None),
            Form::Projection(p) => walk(Vec::new(), Vec::new(), Some(p)),
            Form::Branch(b) if b.anchor == Anchor::Current => {
                walk(b.steps, Vec::new(), b.projection)
            }
            Form::Query(mut q)
                if q.correlations.is_empty()
                    && q.pipeline.is_empty()
                    && q.branches.len() == 1
                    && q.branches[0].anchor == Anchor::Current =>
            {
                let b = q.branches.pop().expect("one");
                walk(b.steps, Vec::new(), b.projection)
            }
            Form::Operand(Operand::Rel {
                steps,
                projection,
                anchor: Anchor::Current,
            })
            | Form::Arg(Arg::Expr(Operand::Rel {
                steps,
                projection,
                anchor: Anchor::Current,
            })) => walk(steps, Vec::new(), projection),
            Form::Pipeline(mut cmds) if cmds.len() == 1 => match cmds.pop() {
                Some(Command::Nav(b)) if b.anchor == Anchor::Current => {
                    walk(b.steps, Vec::new(), b.projection)
                }
                Some(Command::Filter(e)) => walk(Vec::new(), vec![Predicate::Expr(e)], None),
                Some(Command::Select(p)) => walk(Vec::new(), vec![p], None),
                Some(c) => {
                    return Err(format!(
                        "a command ('{}') cannot continue a walk; open the template with the pipe",
                        crate::syntax::unparse::command(&c)
                    ));
                }
                None => unreachable!("one"),
            },
            Form::Command(Command::Nav(b)) if b.anchor == Anchor::Current => {
                walk(b.steps, Vec::new(), b.projection)
            }
            other => {
                return Err(format!(
                    "{} ('{}') cannot continue a walk",
                    other.kind(),
                    other.text()
                ));
            }
        })
    }

    /// The form among a call's arguments or a record's fields.
    pub(crate) fn into_arg(self) -> Result<Arg, String> {
        Ok(match self {
            Form::Arg(a) => a,
            Form::Operand(Operand::Lit(v)) => Arg::Lit(v),
            Form::Operand(op) => Arg::Expr(op),
            Form::Projection(p) => Arg::Expr(Operand::Rel {
                steps: Vec::new(),
                projection: Some(p),
                anchor: Anchor::Current,
            }),
            other => Arg::Expr(other.into_operand()?),
        })
    }

    /// The form as a name — a push's, a hop's, a property's.
    pub(crate) fn into_name(self) -> Result<String, String> {
        Ok(match self {
            Form::Projection(Projection::Property(Some(k)))
            | Form::Projection(Projection::CoreMeta(k))
            | Form::Projection(Projection::AdapterMeta(k)) => k,
            Form::Operand(Operand::Lit(crate::value::Value::Str(s)))
            | Form::Arg(Arg::Lit(crate::value::Value::Str(s))) => s,
            Form::Operand(Operand::Lit(v)) | Form::Arg(Arg::Lit(v)) => v.to_string(),
            Form::Elem(PathElem::Step(crate::syntax::ast::Step {
                matcher: crate::syntax::ast::Matcher::Name(n),
                ..
            })) => n,
            other => {
                return Err(format!(
                    "{} ('{}') spells no name",
                    other.kind(),
                    other.text()
                ));
            }
        })
    }
}

pub(crate) struct RegUsage {
    pub(crate) pushes: std::collections::BTreeMap<String, usize>,
    pub(crate) recalls: std::collections::BTreeMap<String, usize>,
}

pub(crate) trait Expander {
    /// The unit table a quantity literal is read with: the source's
    /// declared units over the built-in table.
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        crate::value::quantity::scale_expr(expr)
    }
    /// The unit names the dataset adds to the built-in table (a
    /// model's declarations, a document's own units), for the
    /// operand of `convert(U)` and `unit(U)`; None when no dataset
    /// is at hand to ask, and the operand is judged at the splice
    /// site instead (a definitions file parses without one).
    fn unit_names(&self) -> Option<Vec<String>> {
        None
    }
    fn usage_of_query(&self, q: &Query) -> RegUsage;
    fn usage_of_operand(&self, op: &Operand) -> RegUsage;
    fn usage_of_commands(&self, commands: &[Command]) -> RegUsage;
    /// The register names a macro's arguments invite.
    fn operand_invites(&self, op: &Operand) -> std::collections::BTreeSet<String>;
    /// A macro's expansion: what its body yields over the bindings
    /// (and the dataset, when data-aware) — reflected nodes and
    /// quarqs as forms, every other value as its text. An error
    /// says why it could not expand, in the parser's words.
    fn expand(
        &self,
        name: &str,
        body: &Query,
        bindings: &[(String, MacroBinding)],
        data_aware: bool,
    ) -> crate::Result<Vec<Answer>>;
}

impl Form {
    /// The commands the form reads as after a pipe, for a quarq
    /// that stands for it: a step as the hop `| /x`, a predicate as
    /// the filter, a projection as `| ::x`, an operand as `| expr`.
    pub(crate) fn as_commands(&self) -> Vec<Command> {
        match self {
            // a step or a group prints as the hop it is after a pipe
            Form::Elem(e) => vec![Command::Nav(Branch {
                steps: vec![e.clone()],
                projection: None,
                anchor: Anchor::Current,
            })],
            other => match other.clone().into_commands(false) {
                Ok(cmds) => cmds,
                Err(_) => Vec::new(),
            },
        }
    }

    /// The reflected kind the form stands for, as the vocabulary
    /// names it.
    pub(crate) fn node_kind(&self) -> &'static str {
        match self {
            Form::Query(_) => "query",
            Form::Pipeline(_) => "pipeline",
            Form::Branch(_) => "branch",
            Form::Elem(PathElem::Step(_)) => "step",
            Form::Elem(PathElem::Group(_)) => "group",
            Form::Elem(PathElem::Mark(_)) => "mark",
            Form::Elem(PathElem::Push { .. }) => "subcontext",
            Form::Elem(PathElem::Hole(_)) => "expansion",
            Form::Elem(PathElem::Param(_)) => "param",
            Form::Predicate(_) => "predicate",
            Form::PredExpr(PredExpr::Compare(..)) => "compare",
            Form::PredExpr(PredExpr::And(..)) => "and",
            Form::PredExpr(PredExpr::Or(..)) => "or",
            Form::PredExpr(PredExpr::Not(..)) => "not",
            Form::PredExpr(PredExpr::Chain(..)) => "chain",
            Form::PredExpr(PredExpr::Truthy(_)) => "expr",
            Form::Operand(Operand::Lit(_)) | Form::Arg(Arg::Lit(_)) | Form::Arg(Arg::Key(_)) => {
                "literal"
            }
            Form::Operand(Operand::Rel {
                steps,
                projection: Some(_),
                ..
            }) if steps.is_empty() => "path",
            Form::Operand(_) | Form::Arg(_) => "operand",
            Form::Command(Command::Func(_)) => "cmd",
            Form::Command(Command::Agg(_)) => "agg",
            Form::Command(Command::Push(_)) => "push",
            Form::Command(Command::Subcontext { .. }) => "subcontext",
            Form::Command(Command::ExprPush { .. }) => "expr-push",
            Form::Command(Command::RecordPush { .. }) => "record-push",
            Form::Command(Command::AggPush { .. }) => "agg-push",
            Form::Command(Command::Attach { .. }) => "attach",
            Form::Command(Command::SessionPush(_)) => "session-push",
            Form::Command(Command::Nav(_)) => "nav",
            Form::Command(_) => "command",
            Form::Projection(_) => "projection",
            Form::Trait(_) => "trait",
        }
    }

    /// The node copied with the named properties replaced (`++!`)
    /// or added (`++`), children kept (design paper: the node
    /// overwrite): the overwrite is the constructor
    /// `syntax:node(LIKE; …)` applied to the node itself. A property
    /// the kind does not know is refused naming the kind; `++`
    /// refuses a property the node carries.
    pub(crate) fn rewrite(
        mut self,
        mode: ConcatMode,
        fields: &[(Option<String>, Value)],
    ) -> Result<Form, String> {
        use crate::syntax::ast::{Group, Step};
        if mode == ConcatMode::Item {
            return Err("'+@+' appends one item to a list; a node takes '++!' or '++'".to_string());
        }
        let overwrite = mode == ConcatMode::Overwrite;
        let kind = self.node_kind();
        let text = |v: &Value| match v {
            Value::Str(s) => Ok(s.clone()),
            other => Err(format!("'{}' is no text", other)),
        };
        let int = |v: &Value| match v {
            Value::Int(n) if *n >= 0 => Ok(*n as usize),
            other => Err(format!("'{}' is no count", other)),
        };
        let carries = |prop: &str, present: bool| -> Result<(), String> {
            if present && !overwrite {
                Err(format!(
                    "a {kind} node carries '{prop}' already; '++!' overwrites it"
                ))
            } else {
                Ok(())
            }
        };
        let unknown = |prop: &str| -> Result<(), String> {
            Err(format!("a {kind} node has no property '{prop}'"))
        };
        for (name, value) in fields {
            let Some(prop) = name.as_deref() else {
                return Err("every field of the overwrite names a property".to_string());
            };
            match (&mut self, prop) {
                (Form::Elem(PathElem::Step(Step { axis, .. })), "axis") => {
                    carries(prop, true)?;
                    *axis = axis_from_spelling(&text(value)?)?;
                }
                (Form::Elem(PathElem::Step(Step { matcher, .. })), "matcher") => {
                    carries(prop, true)?;
                    *matcher = crate::syntax::parser::matcher_for(&text(value)?, false)
                        .map_err(|e| e.to_string())?;
                }
                (Form::Elem(PathElem::Step(Step { leaf, .. })), "leaf") => {
                    carries(prop, true)?;
                    *leaf = value.is_truthy();
                }
                (Form::Elem(PathElem::Group(Group { quant, .. })), "min") => {
                    carries(prop, true)?;
                    quant.min = int(value)?;
                }
                (Form::Elem(PathElem::Group(Group { quant, .. })), "max") => {
                    carries(prop, quant.max.is_some())?;
                    quant.max = match value {
                        Value::Null => None,
                        v => Some(int(v)?),
                    };
                }
                (Form::Elem(PathElem::Mark(n)), "name")
                | (Form::Elem(PathElem::Push { name: n, .. }), "name")
                | (Form::Command(Command::Push(n)), "name")
                | (Form::Command(Command::Subcontext { name: n, .. }), "name")
                | (Form::Command(Command::ExprPush { name: n, .. }), "name")
                | (Form::Command(Command::RecordPush { name: n, .. }), "name")
                | (Form::Command(Command::AggPush { name: n, .. }), "name")
                | (Form::Command(Command::Attach { name: n, .. }), "name") => {
                    carries(prop, n.is_some())?;
                    *n = Some(text(value)?);
                }
                (Form::Command(Command::SessionPush(n)), "name") => {
                    carries(prop, true)?;
                    *n = text(value)?;
                }
                (Form::Command(Command::Func(call)), "name")
                | (Form::Command(Command::Agg(call)), "name") => {
                    carries(prop, true)?;
                    call.name = text(value)?;
                }
                (Form::Operand(Operand::Lit(v)), "value") | (Form::Arg(Arg::Lit(v)), "value") => {
                    carries(prop, true)?;
                    *v = value.clone();
                }
                (Form::Projection(Projection::Property(k)), "key") => {
                    carries(prop, k.is_some())?;
                    *k = Some(text(value)?);
                }
                (Form::Projection(Projection::CoreMeta(k)), "key")
                | (Form::Projection(Projection::AdapterMeta(k)), "key") => {
                    carries(prop, true)?;
                    *k = text(value)?;
                }
                (Form::PredExpr(PredExpr::Compare(_, op, _)), "op") => {
                    carries(prop, true)?;
                    *op = cmp_from_spelling(&text(value)?)?;
                }
                // a trait block's alternatives: one name, or a list
                (Form::Trait(t), "alts") => {
                    carries(prop, !t.alts.is_empty())?;
                    let items = match value {
                        Value::List(items) => items.to_vec(),
                        v => vec![v.clone()],
                    };
                    t.alts = items.iter().map(text).collect::<Result<Vec<_>, _>>()?;
                    if t.alts.is_empty() {
                        return Err("a trait block names at least one trait".to_string());
                    }
                }
                (Form::PredExpr(PredExpr::Chain(_, ops)), "ops") => {
                    carries(prop, !ops.is_empty())?;
                    let items = match value {
                        Value::List(items) => items.to_vec(),
                        v => vec![v.clone()],
                    };
                    *ops = items
                        .iter()
                        .map(|v| cmp_from_spelling(&text(v)?))
                        .collect::<Result<Vec<_>, _>>()?;
                }
                (Form::Elem(PathElem::Group(Group { reach, .. })), "reach") => {
                    carries(prop, *reach != Reach::All)?;
                    *reach = match text(value)?.as_str() {
                        "" => Reach::All,
                        "?" => Reach::Proximal,
                        "!" => Reach::Distal,
                        other => return Err(format!("'{other}' is no reach mark (? or !)")),
                    };
                }
                // a predicate's position: its kind, the index, the range
                (Form::Predicate(p), "kind") => {
                    carries(prop, true)?;
                    *p = match text(value)?.as_str() {
                        "expr" => {
                            Predicate::Expr(PredExpr::Truthy(Operand::Lit(Value::Bool(true))))
                        }
                        "index" => Predicate::Index(1),
                        "range" => Predicate::Range(None, None),
                        "positions" => Predicate::Positions(Vec::new()),
                        "computed" => Predicate::Computed(Operand::Lit(Value::Null)),
                        other => {
                            return Err(format!(
                                "'{other}' is no predicate kind (expr, index, range, positions, computed)"
                            ));
                        }
                    };
                }
                (Form::Predicate(Predicate::Index(n)), "value") => {
                    carries(prop, true)?;
                    *n = match value {
                        Value::Int(i) => *i,
                        other => return Err(format!("'{other}' is no index")),
                    };
                }
                (Form::Predicate(Predicate::Range(from, _)), "from") => {
                    carries(prop, from.is_some())?;
                    *from = match value {
                        Value::Null => None,
                        Value::Int(i) => Some(*i),
                        other => return Err(format!("'{other}' is no position")),
                    };
                }
                (Form::Predicate(Predicate::Range(_, to)), "to") => {
                    carries(prop, to.is_some())?;
                    *to = match value {
                        Value::Null => None,
                        Value::Int(i) => Some(*i),
                        other => return Err(format!("'{other}' is no position")),
                    };
                }
                // a projection's rung
                (Form::Projection(p), "kind") => {
                    carries(prop, true)?;
                    let key = match p {
                        Projection::Property(k) => k.clone().unwrap_or_default(),
                        Projection::CoreMeta(k) | Projection::AdapterMeta(k) => k.clone(),
                    };
                    *p = match text(value)?.as_str() {
                        "property" => {
                            Projection::Property(if key.is_empty() { None } else { Some(key) })
                        }
                        "core" => Projection::CoreMeta(key),
                        "adapter" => Projection::AdapterMeta(key),
                        other => {
                            return Err(format!(
                                "'{other}' is no projection kind (property, core, adapter)"
                            ));
                        }
                    };
                }
                (Form::Operand(Operand::Arith { op, .. }), "op") => {
                    carries(prop, true)?;
                    *op = match text(value)?.as_str() {
                        "+" => ArithOp::Add,
                        "-" => ArithOp::Sub,
                        "*" => ArithOp::Mul,
                        "/" => ArithOp::Div,
                        "idiv" => ArithOp::IDiv,
                        "mod" => ArithOp::Mod,
                        other => {
                            return Err(format!(
                                "'{other}' is no arithmetic operator (+, -, *, /, idiv, mod)"
                            ));
                        }
                    };
                }
                (Form::Operand(Operand::Concat { mode, .. }), "item") => {
                    carries(prop, *mode == ConcatMode::Item)?;
                    if value.is_truthy() {
                        *mode = ConcatMode::Item;
                    }
                }
                (Form::Operand(Operand::Concat { mode, .. }), "overwrite") => {
                    carries(prop, *mode == ConcatMode::Overwrite)?;
                    if value.is_truthy() {
                        *mode = ConcatMode::Overwrite;
                    }
                }
                (Form::Command(Command::Recall(r)), "ref")
                | (Form::Command(Command::Splice(r)), "ref")
                | (Form::Command(Command::AggRecall(r)), "ref")
                | (Form::Operand(Operand::Recall(r)), "ref") => {
                    carries(prop, true)?;
                    *r = reg_from_spelling(&text(value)?)?;
                }
                (Form::Command(Command::Repeat { min, .. }), "min") => {
                    carries(prop, true)?;
                    *min = int(value)?;
                }
                (Form::Command(Command::Repeat { max, .. }), "max") => {
                    carries(prop, max.is_some())?;
                    *max = match value {
                        Value::Null => None,
                        v => Some(int(v)?),
                    };
                }
                (Form::Operand(Operand::Field { name, .. }), "name") => {
                    carries(prop, !name.is_empty())?;
                    *name = text(value)?;
                }
                (Form::Command(Command::Func(call)), "lib")
                | (Form::Command(Command::Agg(call)), "lib") => {
                    carries(prop, call.name.contains(':'))?;
                    let bare = call.name.rsplit(':').next().unwrap_or("").to_string();
                    call.name = format!("{}:{bare}", text(value)?);
                }
                _ => unknown(prop)?,
            }
        }
        Ok(self)
    }
}

/// The axis an overwrite spells, as the reflection spells it.
fn axis_from_spelling(s: &str) -> Result<Axis, String> {
    Ok(match s {
        "/" => Axis::Child,
        "//" => Axis::Descendant(Reach::All),
        "//?" => Axis::Descendant(Reach::Proximal),
        "//!" => Axis::Descendant(Reach::Distal),
        "*//" => Axis::Universal,
        "\\" => Axis::Parent,
        "\\\\" => Axis::Ancestor(Reach::All),
        "\\\\?" => Axis::Ancestor(Reach::Proximal),
        "\\\\!" => Axis::Ancestor(Reach::Distal),
        ">" => Axis::NextSibling,
        "<" => Axis::PrevSibling,
        ">>" => Axis::FollowingSiblings(Reach::All),
        ">>?" => Axis::FollowingSiblings(Reach::Proximal),
        ">>!" => Axis::FollowingSiblings(Reach::Distal),
        "<<" => Axis::PrecedingSiblings(Reach::All),
        "<<?" => Axis::PrecedingSiblings(Reach::Proximal),
        "<<!" => Axis::PrecedingSiblings(Reach::Distal),
        "->" => Axis::OutLink,
        "<-" => Axis::InLink,
        "--" => Axis::BothLink,
        "~" => Axis::Any,
        "~~" => Axis::AnyDeep(Reach::All),
        "~~?" => Axis::AnyDeep(Reach::Proximal),
        "~~!" => Axis::AnyDeep(Reach::Distal),
        other => {
            return Err(format!(
                "'{other}' is no axis a step's overwrite spells (the plain axes: /, //, \\, \\\\, >, <, >>, <<, ->, <-, --, ~, ~~)"
            ));
        }
    })
}

/// The comparison an overwrite spells.
fn cmp_from_spelling(s: &str) -> Result<CmpOp, String> {
    Ok(match s {
        "=" => CmpOp::Eq,
        "!=" => CmpOp::Ne,
        "<" => CmpOp::Lt,
        "<=" => CmpOp::Le,
        ">" => CmpOp::Gt,
        ">=" => CmpOp::Ge,
        "==" => CmpOp::Match,
        "!==" => CmpOp::NotMatch,
        "*=" => CmpOp::Contains,
        "<<=" => CmpOp::Within,
        ">>=" => CmpOp::Encloses,
        other => return Err(format!("'{other}' is no comparison operator")),
    })
}

/// The register reference an overwrite or a constructor spells.
fn reg_from_spelling(s: &str) -> Result<RegRef, String> {
    Ok(match s {
        "$." => RegRef::Top,
        "$.." => RegRef::Back(2),
        "@." => RegRef::Whole,
        "%." => RegRef::Record,
        "%.*" => RegRef::FullRecord,
        "@@." => RegRef::NodeWhole,
        "%%." => RegRef::NodeRecord,
        "%%.*" => RegRef::NodeFullRecord,
        "%," => RegRef::SessionRecord,
        other => {
            if let Some(name) = other.strip_prefix("$,") {
                RegRef::Session(name.to_string())
            } else if let Some(rest) = other.strip_prefix("$.") {
                if let Some(back) = rest.strip_prefix('-') {
                    RegRef::Back(
                        back.parse()
                            .map_err(|_| format!("'{other}' is no register reference"))?,
                    )
                } else if rest.bytes().all(|b| b.is_ascii_digit()) {
                    RegRef::Index(
                        rest.parse()
                            .map_err(|_| format!("'{other}' is no register reference"))?,
                    )
                } else {
                    RegRef::Named(rest.to_string())
                }
            } else {
                return Err(format!(
                    "'{other}' is no register reference ($., $.N, $.-N, $.name, $,name, @., %., %.*)"
                ));
            }
        }
    })
}
