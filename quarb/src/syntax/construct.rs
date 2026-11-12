//! The syntax library (design paper, draft 5: the constructors): a
//! node built by vocabulary. A template constructs by spelling and
//! lets values fill only the places a value can stand; a node whose
//! kind or operator is computed — a step whose axis comes from data,
//! a comparison whose operator was read off another query — is the
//! constructor's. One command per kind, the locked vocabulary tables
//! as the signatures: the properties as operands in the tables'
//! order, the children as quarq operands. The generic
//! `syntax:node(KIND; %(props); children…)` serves the case where
//! even the kind is computed; `syntax:node(LIKE; children…)` copies
//! a node's (or a quarq's) kind and properties over the children
//! given, which is what the bottom-up rebuild uses. Every answer is a
//! quarq that stands for the construct exactly, checked against the
//! vocabulary as it is built: a property the kind does not know, a
//! child of a kind the parent cannot hold, or a count of children
//! the kind does not admit is refused naming the kind. `++!` and
//! `++` over a node are the same rewrite applied to the node itself.

use crate::pipeline::library::{
    CommandLibrary, InputSlot, LibCall, LibFailure, OperandSpec, Pipe, Synopsis,
};
use crate::syntax::ast::{
    Anchor, Arg, ArithOp, Branch, CmpOp, Command, ConcatMode, FnCall, Group, Matcher, Operand,
    PathElem, PredExpr, Predicate, Projection, PushBody, Quant, Query, Reach, RegRef, Step,
};
use crate::syntax::expand::Form;
use crate::value::Value;

pub struct SyntaxLibrary;

pub static SYNTAX: SyntaxLibrary = SyntaxLibrary;

const fn op(name: &'static str, doc: &'static str, required: bool) -> OperandSpec {
    OperandSpec {
        name,
        doc,
        values: &[],
        required,
    }
}

const CHILDREN: OperandSpec = op(
    "...",
    "the children, each a quarq (a node answered, a constructor's answer, a q(…) literal)",
    false,
);

macro_rules! kind {
    ($name:literal, $doc:literal, [$($operand:expr),*]) => {
        Synopsis {
            name: $name,
            doc: $doc,
            pipes: Pipe::Both,
            operands: &[$($operand,)* CHILDREN],
            options: &[],
            input: InputSlot::Context,
            returns: "quarq",
        }
    };
}

const COMMANDS: &[Synopsis] = &[
    Synopsis {
        name: "node",
        doc: "a node built by vocabulary: of the kind named, over a record of its properties and the children given; or like the node (or quarq) given, its kind and properties copied over the children given",
        pipes: Pipe::Both,
        operands: &[
            op(
                "like",
                "the kind's name, or a node or a quarq whose kind and properties are copied",
                true,
            ),
            op(
                "props",
                "with a kind's name: a record of the node's properties",
                false,
            ),
            CHILDREN,
        ],
        options: &[],
        input: InputSlot::Context,
        returns: "quarq",
    },
    kind!(
        "step",
        "a hop: its axis, its matcher, whether it is a leaf anchor; the traits and the predicates beneath",
        [
            op(
                "axis",
                "the axis as spelled: /, //, \\, \\\\, >, <, ->, <-, --, ~, ~~",
                true
            ),
            op("matcher", "the name, a glob, or *", true),
            op("leaf", "true for a leaf anchor", false)
        ]
    ),
    kind!(
        "trait",
        "a trait block on a hop: its alternatives, one name or a list, joined by ||",
        [op(
            "alts",
            "the trait names: one, or a list for alternatives",
            true
        )]
    ),
    kind!(
        "group",
        "a path-pattern group: its quantifier's min and max, its reach selector; the alternatives beneath, each a walk",
        [
            op("min", "the least repetitions", false),
            op("max", "the most, null for open", false),
            op("reach", "? or !, nothing for keep-all", false)
        ]
    ),
    kind!(
        "mark",
        "a mark on the current node, named or not",
        [op("name", "the mark's name", false)]
    ),
    kind!(
        "push",
        "a bare push of the topic, named or not",
        [op("name", "the regula's name", false)]
    ),
    kind!(
        "subcontext",
        "a subcontext push: the body beneath, a query",
        [op("name", "the regula's name", false)]
    ),
    kind!(
        "expr-push",
        "a computed push: the expression beneath",
        [op("name", "the regula's name", false)]
    ),
    kind!(
        "agg-push",
        "an aggregate push: the body beneath",
        [op("name", "the regula's name", false)]
    ),
    kind!(
        "attach",
        "an attaching push: the push beneath",
        [op("name", "the regula's name", false)]
    ),
    kind!(
        "session-push",
        "a session push",
        [op("name", "the session value's name", true)]
    ),
    kind!(
        "predicate",
        "a predicate: the condition beneath, or a position by its kind and values",
        [
            op(
                "kind",
                "expr (the default), index, range, positions or computed",
                false
            ),
            op("value", "the index, for an index predicate", false),
            op("from", "the range's start", false),
            op("to", "the range's end", false)
        ]
    ),
    kind!(
        "compare",
        "a comparison: its operator; the two operands beneath",
        [op("op", "=, !=, <, <=, >, >=, ==, !==, *=, <<=, >>=", true)]
    ),
    kind!("and", "a conjunction of the two conditions beneath", []),
    kind!("or", "a disjunction of the two conditions beneath", []),
    kind!("not", "the negation of the condition beneath", []),
    kind!(
        "chain",
        "a comparison chain: its operators; the operands beneath",
        [op("ops", "the operators, a list", true)]
    ),
    kind!(
        "literal",
        "a literal of the value given",
        [op("value", "the value", true)]
    ),
    kind!(
        "projection",
        "a projection by its key, on the ladder's rung",
        [
            op("key", "the key", false),
            op("kind", "property (the default), core or adapter", false)
        ]
    ),
    kind!(
        "path",
        "a relative path from the current node: the elements beneath, a projection last",
        []
    ),
    kind!(
        "branch",
        "a branch from the current node: the elements beneath, a projection last",
        []
    ),
    kind!("query", "a query: branches beneath, then a pipeline", []),
    kind!("pipeline", "a pipeline: the commands beneath", []),
    kind!("nav", "a navigation command: the branch beneath", []),
    kind!(
        "cmd",
        "a command by name: the operands beneath",
        [
            op("name", "the command's name", true),
            op("lib", "its library's prefix", false)
        ]
    ),
    kind!(
        "agg",
        "a command after the aggregation pipe, by name: the operands beneath",
        [
            op("name", "the command's name", true),
            op("lib", "its library's prefix", false)
        ]
    ),
    kind!("expr", "an expression command: the operand beneath", []),
    kind!("filter", "a per-capsa filter: the condition beneath", []),
    kind!(
        "select",
        "a positional selection: the predicate beneath",
        []
    ),
    kind!(
        "recall",
        "a capsa read",
        [op(
            "ref",
            "the reference as spelled: $., $.N, $.-N, $.name, $,name, @., %., %.*",
            true
        )]
    ),
    kind!(
        "arith",
        "an arithmetic expression: its operator; the two operands beneath",
        [op("op", "+, -, *, /, idiv, mod", true)]
    ),
    kind!(
        "concat",
        "a concatenation: the two operands beneath",
        [
            op("item", "true for +@+", false),
            op("overwrite", "true for ++!", false)
        ]
    ),
    kind!(
        "cond",
        "a conditional: the test, the then and the else beneath",
        []
    ),
    kind!("neg", "a negation: the operand beneath", []),
    kind!(
        "parens",
        "a parenthesised condition as a value: the condition beneath",
        []
    ),
    kind!("list", "a list constructor: the items beneath", []),
    kind!(
        "piped",
        "a piped operand: the expression, then the commands beneath",
        []
    ),
    kind!(
        "map",
        "the iteration pipe's command: the command beneath",
        []
    ),
    kind!(
        "repeat",
        "a quantified command: the command beneath",
        [
            op("min", "the least repetitions", false),
            op("max", "the most, null for open", false)
        ]
    ),
    kind!(
        "field",
        "a field read of the operand beneath",
        [op("name", "the field's name", true)]
    ),
];

impl CommandLibrary for SyntaxLibrary {
    fn prefix(&self) -> &str {
        "syntax"
    }

    fn commands(&self) -> &[Synopsis] {
        COMMANDS
    }

    fn call(&self, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        let form = match name {
            "node" => node(call.operands),
            kind => built(kind, call.operands),
        }
        .map_err(LibFailure::Failed)?;
        Ok(Value::quarq_form(form))
    }
}

/// The children among a constructor's operands: every quarq, a list
/// of quarqs as its items; anything else is refused.
fn children(ops: Vec<Value>) -> Result<Vec<Form>, String> {
    let mut out = Vec::new();
    for v in ops {
        match v {
            Value::Quarq(q) => out.push(q.into_form()),
            Value::List(items) => {
                for item in items.iter() {
                    match item {
                        Value::Quarq(q) => out.push(q.clone().into_form()),
                        Value::Null => {}
                        other => {
                            return Err(format!(
                                "a child is a quarq; {} given",
                                crate::stdlib::kind_of(other)
                            ));
                        }
                    }
                }
            }
            Value::Null => {}
            other => {
                return Err(format!(
                    "a child is a quarq; {} given",
                    crate::stdlib::kind_of(&other)
                ));
            }
        }
    }
    Ok(out)
}

/// `syntax:node(KIND; %(props); children…)` and `syntax:node(LIKE;
/// children…)`.
fn node(mut ops: Vec<Value>) -> Result<Form, String> {
    if ops.is_empty() {
        return Err("syntax:node takes a kind's name or a node to build like".to_string());
    }
    let like = ops.remove(0);
    if matches!(like, Value::Null) {
        return Err(
            "nothing given to build like: a node of the expansion root, a rest parameter or \
             the data has no form, and a value that is no node or quarq builds nothing"
                .to_string(),
        );
    }
    match like {
        Value::Str(kind) => {
            let props: Vec<(String, Value)> = match ops.first() {
                Some(Value::Record(r)) => {
                    let fields = r.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    ops.remove(0);
                    fields
                }
                _ => Vec::new(),
            };
            let form = Form::from_kind(&kind)?;
            let fields: Vec<(Option<String>, Value)> =
                props.into_iter().map(|(k, v)| (Some(k), v)).collect();
            let form = form.rewrite(ConcatMode::Overwrite, &fields)?;
            form.with_children(children(ops)?)
        }
        Value::Quarq(q) => q.into_form().with_children(children(ops)?),
        other => Err(format!(
            "syntax:node builds like a node or a quarq, or by a kind's name; {} given",
            crate::stdlib::kind_of(&other)
        )),
    }
}

/// A kind's own constructor: the properties positional, in the
/// table's order, then the children.
fn built(kind: &str, ops: Vec<Value>) -> Result<Form, String> {
    let syn = COMMANDS
        .iter()
        .find(|s| s.name == kind)
        .ok_or_else(|| format!("no constructor builds a {kind} node"))?;
    let props: Vec<&OperandSpec> = syn.operands.iter().filter(|o| o.name != "...").collect();
    let mut fields: Vec<(Option<String>, Value)> = Vec::new();
    let mut rest: Vec<Value> = Vec::new();
    let mut i = 0;
    // a quarq, or a list of quarqs, is a child; any other value fills
    // the next property (a list of names is a trait's alternatives)
    let is_child = |v: &Value| match v {
        Value::Quarq(_) => true,
        Value::List(items) => items.iter().any(|i| matches!(i, Value::Quarq(_))),
        _ => false,
    };
    for v in ops {
        if i < props.len() && !is_child(&v) {
            if !matches!(v, Value::Null) {
                fields.push((Some(props[i].name.to_string()), v));
            }
            i += 1;
        } else {
            rest.push(v);
        }
    }
    for p in props.iter().filter(|p| p.required) {
        if !fields.iter().any(|(n, _)| n.as_deref() == Some(p.name)) {
            return Err(format!(
                "syntax:{kind} needs its {} (the signature: {})",
                p.name,
                syn.render("syntax")
            ));
        }
    }
    Form::from_kind(kind)?
        .rewrite(ConcatMode::Overwrite, &fields)?
        .with_children(children(rest)?)
}

fn empty_query() -> Query {
    Query {
        correlations: Vec::new(),
        branches: Vec::new(),
        pipeline: Vec::new(),
        join_at: 0,
        outer: false,
        every: false,
    }
}

fn current_branch() -> Branch {
    Branch {
        steps: Vec::new(),
        projection: None,
        anchor: Anchor::Current,
    }
}

fn null() -> Operand {
    Operand::Lit(Value::Null)
}

fn truthy() -> PredExpr {
    PredExpr::Truthy(Operand::Lit(Value::Bool(true)))
}

fn call_named(name: &str) -> FnCall {
    FnCall {
        name: name.to_string(),
        args: Vec::new(),
        headless: false,
    }
}

/// A group's quantifier is a range: its most at least its least, as
/// the parser refuses it written (`{9;3}`).
pub(crate) fn group_shape(quant: &crate::syntax::ast::Quant) -> Result<(), String> {
    match quant.max {
        Some(max) if max < quant.min => Err(format!(
            "quantifier {{{};{max}}} has max below min",
            quant.min
        )),
        _ => Ok(()),
    }
}

impl Form {
    /// A bare node of the kind, its properties at their defaults,
    /// no children: what a constructor rewrites and fills.
    pub(crate) fn from_kind(kind: &str) -> Result<Form, String> {
        Ok(match kind {
            "step" => Form::Elem(PathElem::Step(Step {
                axis: crate::syntax::ast::Axis::Child,
                matcher: Matcher::Any,
                labels: Vec::new(),
                traits: Vec::new(),
                predicates: Vec::new(),
                leaf: false,
            })),
            "group" => Form::Elem(PathElem::Group(Group {
                alts: Vec::new(),
                quant: Quant {
                    min: 1,
                    max: Some(1),
                    budget: None,
                    rule: None,
                },
                more: Vec::new(),
                predicates: Vec::new(),
                traits: Vec::new(),
                reach: Reach::All,
                marks: Vec::new(),
            })),
            "mark" => Form::Elem(PathElem::Mark(None)),
            "push" => Form::Command(Command::Push(None)),
            "subcontext" => Form::Command(Command::Subcontext {
                name: None,
                body: Box::new(empty_query()),
            }),
            "expr-push" => Form::Command(Command::ExprPush {
                name: None,
                expr: Operand::Topic,
            }),
            "agg-push" => Form::Command(Command::AggPush {
                name: None,
                body: Operand::Topic,
                regs: Vec::new(),
                keep: false,
            }),
            "attach" => Form::Command(Command::Attach {
                name: None,
                command: Box::new(Command::Push(None)),
            }),
            "session-push" => Form::Command(Command::SessionPush(String::new())),
            "trait" => Form::Trait(crate::syntax::ast::TraitClause { alts: Vec::new() }),
            "predicate" => Form::Predicate(Predicate::Expr(truthy())),
            "compare" => Form::PredExpr(PredExpr::Compare(null(), CmpOp::Eq, null())),
            "and" => Form::PredExpr(PredExpr::And(Box::new(truthy()), Box::new(truthy()))),
            "or" => Form::PredExpr(PredExpr::Or(Box::new(truthy()), Box::new(truthy()))),
            "not" => Form::PredExpr(PredExpr::Not(Box::new(truthy()))),
            "chain" => Form::PredExpr(PredExpr::Chain(Vec::new(), Vec::new())),
            "literal" => Form::Operand(null()),
            "projection" => Form::Projection(Projection::Property(None)),
            "path" => Form::Operand(Operand::Rel {
                steps: Vec::new(),
                projection: None,
                anchor: Anchor::Current,
            }),
            "branch" => Form::Branch(current_branch()),
            "query" => Form::Query(empty_query()),
            "pipeline" => Form::Pipeline(Vec::new()),
            "nav" => Form::Command(Command::Nav(current_branch())),
            "cmd" => Form::Command(Command::Func(call_named(""))),
            "agg" => Form::Command(Command::Agg(call_named(""))),
            "expr" => Form::Command(Command::Expr(Operand::Topic)),
            "filter" => Form::Command(Command::Filter(truthy())),
            "select" => Form::Command(Command::Select(Predicate::Index(1))),
            "recall" => Form::Command(Command::Recall(RegRef::Top)),
            "arith" => Form::Operand(Operand::Arith {
                op: ArithOp::Add,
                left: Box::new(null()),
                right: Box::new(null()),
            }),
            "concat" => Form::Operand(Operand::Concat {
                mode: ConcatMode::Merge,
                left: Box::new(null()),
                right: Box::new(null()),
            }),
            "cond" => Form::Operand(Operand::Cond {
                cond: Box::new(truthy()),
                then: Box::new(null()),
                other: Box::new(null()),
            }),
            "neg" => Form::Operand(Operand::Neg(Box::new(null()))),
            "parens" => Form::Operand(Operand::Group(Box::new(truthy()))),
            "list" => Form::Operand(Operand::List(Vec::new())),
            "piped" => Form::Operand(Operand::Piped {
                expr: Box::new(Operand::Topic),
                commands: Vec::new(),
            }),
            "map" => Form::Command(Command::Map(Box::new(Command::Push(None)))),
            "repeat" => Form::Command(Command::Repeat {
                command: Box::new(Command::Push(None)),
                min: 1,
                max: Some(1),
            }),
            "field" => Form::Operand(Operand::Field {
                base: Box::new(Operand::Topic),
                name: String::new(),
                bare: true,
            }),
            other => {
                return Err(format!(
                    "a {other} node is not built by a constructor; it is answered from a query's own reflection"
                ));
            }
        })
    }

    /// The node over the children given, its own children replaced
    /// (design paper: the constructors): each child placed where its
    /// kind stands under the parent; a child the kind cannot hold, or
    /// a count the kind does not admit, is refused naming the kind.
    pub(crate) fn with_children(self, children: Vec<Form>) -> Result<Form, String> {
        let kind = self.node_kind();
        let count = |n: usize, what: &str| -> Result<(), String> {
            if children.len() != n {
                Err(format!(
                    "a {kind} node holds {what}; {} given",
                    children.len()
                ))
            } else {
                Ok(())
            }
        };
        let cannot = |child: &Form| -> String {
            format!(
                "a {kind} node cannot hold {} ('{}')",
                child.kind(),
                child.text()
            )
        };
        Ok(match self {
            Form::Elem(PathElem::Step(mut s)) => {
                // traits and predicates beneath, in the table's order
                s.traits.clear();
                s.predicates.clear();
                for c in children {
                    match c {
                        Form::Trait(t) => s.traits.push(t),
                        other => s
                            .predicates
                            .push(as_predicate(other).map_err(|c| cannot(&c))?),
                    }
                }
                Form::Elem(PathElem::Step(s))
            }
            Form::Elem(PathElem::Group(mut g)) => {
                let mut alts = Vec::new();
                for c in children {
                    let w = c.clone().into_walk().map_err(|_| cannot(&c))?;
                    if w.projection.is_some() || !w.predicates.is_empty() {
                        return Err(cannot(&c));
                    }
                    alts.push(w.elems);
                }
                // the shape checks a written group gets (the review
                // of 2026-11-10, syntax B18: a group built with no
                // alternative expanded to `()`, one built `{9;3}` to
                // that brace, and both answered nothing)
                if alts.is_empty() {
                    return Err(
                        "a group node holds one alternative or more, each a walk of at least \
                         one hop; none given"
                            .to_string(),
                    );
                }
                group_shape(&g.quant)?;
                g.alts = alts;
                Form::Elem(PathElem::Group(g))
            }
            Form::Elem(PathElem::Mark(n)) => {
                count(0, "no children")?;
                Form::Elem(PathElem::Mark(n))
            }
            Form::Elem(PathElem::Push { name, body }) => {
                count(1, "one body")?;
                let c = children.into_iter().next().expect("one");
                let body = match body {
                    PushBody::Query(_) => {
                        PushBody::Query(Box::new(as_query(c).map_err(|c| cannot(&c))?))
                    }
                    PushBody::Expr(_) => PushBody::Expr(c.into_operand()?),
                };
                Form::Elem(PathElem::Push { name, body })
            }
            Form::Command(Command::Push(n)) => {
                count(0, "no children")?;
                Form::Command(Command::Push(n))
            }
            Form::Command(Command::SessionPush(n)) => {
                count(0, "no children")?;
                Form::Command(Command::SessionPush(n))
            }
            Form::Command(Command::Subcontext { name, .. }) => {
                count(1, "one body, a query")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Subcontext {
                    name,
                    body: Box::new(as_query(c).map_err(|c| cannot(&c))?),
                })
            }
            Form::Command(Command::ExprPush { name, .. }) => {
                count(1, "one expression")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::ExprPush {
                    name,
                    expr: c.into_operand()?,
                })
            }
            Form::Command(Command::AggPush {
                name, regs, keep, ..
            }) => {
                count(1, "one body")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::AggPush {
                    name,
                    body: c.into_operand()?,
                    regs,
                    keep,
                })
            }
            Form::Command(Command::Attach { name, .. }) => {
                count(1, "one push")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Attach {
                    name,
                    command: Box::new(as_command(c).map_err(|c| cannot(&c))?),
                })
            }
            Form::Predicate(p) => match p {
                Predicate::Expr(_) => {
                    count(1, "one condition")?;
                    let c = children.into_iter().next().expect("one");
                    Form::Predicate(Predicate::Expr(as_pred_expr(c).map_err(|c| cannot(&c))?))
                }
                Predicate::Positions(_) => Form::Predicate(Predicate::Positions(
                    children
                        .into_iter()
                        .map(|c| as_predicate(c).map_err(|c| cannot(&c)))
                        .collect::<Result<Vec<_>, _>>()?,
                )),
                Predicate::Computed(_) => {
                    count(1, "one operand")?;
                    let c = children.into_iter().next().expect("one");
                    Form::Predicate(Predicate::Computed(c.into_operand()?))
                }
                positional => {
                    count(0, "no children")?;
                    Form::Predicate(positional)
                }
            },
            Form::PredExpr(PredExpr::Compare(_, op, _)) => {
                count(2, "two operands")?;
                let mut it = children.into_iter();
                let l = it.next().expect("two").into_operand()?;
                let r = it.next().expect("two").into_operand()?;
                Form::PredExpr(PredExpr::Compare(l, op, r))
            }
            Form::PredExpr(PredExpr::And(..)) | Form::PredExpr(PredExpr::Or(..)) => {
                count(2, "two conditions")?;
                let mut it = children.into_iter();
                let a = as_pred_expr(it.next().expect("two")).map_err(|c| cannot(&c))?;
                let b = as_pred_expr(it.next().expect("two")).map_err(|c| cannot(&c))?;
                Form::PredExpr(match self_kind_and(kind) {
                    true => PredExpr::And(Box::new(a), Box::new(b)),
                    false => PredExpr::Or(Box::new(a), Box::new(b)),
                })
            }
            Form::PredExpr(PredExpr::Not(_)) => {
                count(1, "one condition")?;
                let c = children.into_iter().next().expect("one");
                Form::PredExpr(PredExpr::Not(Box::new(
                    as_pred_expr(c).map_err(|c| cannot(&c))?,
                )))
            }
            Form::PredExpr(PredExpr::Chain(_, ops)) => {
                if children.len() != ops.len() + 1 {
                    return Err(format!(
                        "a chain node over {} operators holds {} operands; {} given",
                        ops.len(),
                        ops.len() + 1,
                        children.len()
                    ));
                }
                let operands = children
                    .into_iter()
                    .map(Form::into_operand)
                    .collect::<Result<Vec<_>, _>>()?;
                Form::PredExpr(PredExpr::Chain(operands, ops))
            }
            Form::PredExpr(PredExpr::Truthy(_)) => {
                count(1, "one operand")?;
                let c = children.into_iter().next().expect("one");
                Form::PredExpr(PredExpr::Truthy(c.into_operand()?))
            }
            Form::Operand(Operand::Lit(v)) | Form::Arg(Arg::Lit(v)) | Form::Arg(Arg::Key(v)) => {
                count(0, "no children")?;
                Form::Operand(Operand::Lit(v))
            }
            Form::Projection(p) => {
                count(0, "no children")?;
                Form::Projection(p)
            }
            Form::Trait(t) => {
                count(0, "no children")?;
                Form::Trait(t)
            }
            Form::Operand(Operand::Rel { anchor, .. }) => {
                let (steps, projection) = as_walk(children, &cannot)?;
                Form::Operand(Operand::Rel {
                    steps,
                    projection,
                    anchor,
                })
            }
            Form::Branch(b) => {
                let (steps, projection) = as_walk(children, &cannot)?;
                Form::Branch(Branch {
                    steps,
                    projection,
                    anchor: b.anchor,
                })
            }
            Form::Query(mut q) => {
                q.branches.clear();
                q.pipeline.clear();
                for c in children {
                    match c {
                        Form::Branch(b) => q.branches.push(b),
                        Form::Pipeline(cmds) => q.pipeline.extend(cmds),
                        Form::Command(c) => q.pipeline.push(c),
                        other => {
                            let w = other.clone().into_walk().map_err(|_| cannot(&other))?;
                            if !w.predicates.is_empty() {
                                return Err(cannot(&other));
                            }
                            q.branches.push(Branch {
                                steps: w.elems,
                                projection: w.projection,
                                anchor: Anchor::Current,
                            });
                        }
                    }
                }
                Form::Query(q)
            }
            Form::Pipeline(_) => {
                let mut cmds = Vec::new();
                for c in children {
                    cmds.extend(c.clone().into_commands(false).map_err(|_| cannot(&c))?);
                }
                Form::Pipeline(cmds)
            }
            Form::Command(Command::Nav(_)) => {
                count(1, "one branch")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Nav(as_branch(c).map_err(|c| cannot(&c))?))
            }
            Form::Command(Command::Func(mut call)) => {
                call.args = children
                    .into_iter()
                    .map(Form::into_arg)
                    .collect::<Result<Vec<_>, _>>()?;
                Form::Command(Command::Func(call))
            }
            Form::Command(Command::Agg(mut call)) => {
                call.args = children
                    .into_iter()
                    .map(Form::into_arg)
                    .collect::<Result<Vec<_>, _>>()?;
                Form::Command(Command::Agg(call))
            }
            Form::Command(Command::Expr(_)) => {
                count(1, "one operand")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Expr(c.into_operand()?))
            }
            Form::Command(Command::AggExpr(_)) => {
                count(1, "one operand")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::AggExpr(c.into_operand()?))
            }
            Form::Command(Command::Filter(_)) => {
                count(1, "one condition")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Filter(as_pred_expr(c).map_err(|c| cannot(&c))?))
            }
            Form::Command(Command::Select(_)) => {
                count(1, "one predicate")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Select(as_predicate(c).map_err(|c| cannot(&c))?))
            }
            Form::Command(Command::Recall(r)) => {
                count(0, "no children")?;
                Form::Command(Command::Recall(r))
            }
            Form::Command(Command::Map(_)) => {
                count(1, "one command")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Map(Box::new(
                    as_command(c).map_err(|c| cannot(&c))?,
                )))
            }
            Form::Command(Command::Repeat { min, max, .. }) => {
                count(1, "one command")?;
                let c = children.into_iter().next().expect("one");
                Form::Command(Command::Repeat {
                    command: Box::new(as_command(c).map_err(|c| cannot(&c))?),
                    min,
                    max,
                })
            }
            Form::Operand(Operand::Arith { op, .. }) => {
                count(2, "two operands")?;
                let mut it = children.into_iter();
                Form::Operand(Operand::Arith {
                    op,
                    left: Box::new(it.next().expect("two").into_operand()?),
                    right: Box::new(it.next().expect("two").into_operand()?),
                })
            }
            Form::Operand(Operand::Concat { mode, .. }) => {
                count(2, "two operands")?;
                let mut it = children.into_iter();
                Form::Operand(Operand::Concat {
                    mode,
                    left: Box::new(it.next().expect("two").into_operand()?),
                    right: Box::new(it.next().expect("two").into_operand()?),
                })
            }
            Form::Operand(Operand::Cond { .. }) => {
                count(3, "a test, a then and an else")?;
                let mut it = children.into_iter();
                let cond = as_pred_expr(it.next().expect("three")).map_err(|c| cannot(&c))?;
                Form::Operand(Operand::Cond {
                    cond: Box::new(cond),
                    then: Box::new(it.next().expect("three").into_operand()?),
                    other: Box::new(it.next().expect("three").into_operand()?),
                })
            }
            Form::Operand(Operand::Neg(_)) => {
                count(1, "one operand")?;
                let c = children.into_iter().next().expect("one");
                Form::Operand(Operand::Neg(Box::new(c.into_operand()?)))
            }
            Form::Operand(Operand::Group(_)) => {
                count(1, "one condition")?;
                let c = children.into_iter().next().expect("one");
                Form::Operand(Operand::Group(Box::new(
                    as_pred_expr(c).map_err(|c| cannot(&c))?,
                )))
            }
            Form::Operand(Operand::List(_)) => Form::Operand(Operand::List(
                children
                    .into_iter()
                    .map(Form::into_operand)
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Form::Operand(Operand::Piped { .. }) => {
                let mut it = children.into_iter();
                let expr = it
                    .next()
                    .ok_or_else(|| {
                        format!("a {kind} node holds an expression, then commands; none given")
                    })?
                    .into_operand()?;
                let mut commands = Vec::new();
                for c in it {
                    commands.extend(c.clone().into_commands(false).map_err(|_| cannot(&c))?);
                }
                Form::Operand(Operand::Piped {
                    expr: Box::new(expr),
                    commands,
                })
            }
            Form::Operand(Operand::Field { name, .. }) => {
                count(1, "one operand")?;
                let c = children.into_iter().next().expect("one");
                let base = c.into_operand()?;
                let bare = matches!(base, Operand::Topic);
                Form::Operand(Operand::Field {
                    base: Box::new(base),
                    name,
                    bare,
                })
            }
            other => {
                if children.is_empty() {
                    other
                } else {
                    return Err(format!(
                        "a {kind} node takes no children from a constructor"
                    ));
                }
            }
        })
    }
}

fn self_kind_and(kind: &str) -> bool {
    kind == "and"
}

/// A walk's elements and projection from children in order: steps
/// and groups continue it, predicates refine the element before,
/// a projection ends it.
fn as_walk(
    children: Vec<Form>,
    cannot: &dyn Fn(&Form) -> String,
) -> Result<(Vec<PathElem>, Option<Projection>), String> {
    let mut steps: Vec<PathElem> = Vec::new();
    let mut projection: Option<Projection> = None;
    for c in children {
        if projection.is_some() {
            return Err(format!(
                "a projection ends the walk; '{}' follows it",
                c.text()
            ));
        }
        let w = c.clone().into_walk().map_err(|_| cannot(&c))?;
        steps.extend(w.elems);
        for p in w.predicates {
            match steps.last_mut() {
                Some(PathElem::Step(s)) => s.predicates.push(p),
                Some(PathElem::Group(g)) => g.predicates.push(p),
                _ => {
                    return Err(
                        "a predicate refines the element before it; none precedes it".to_string(),
                    );
                }
            }
        }
        projection = w.projection;
    }
    Ok((steps, projection))
}

fn as_predicate(c: Form) -> Result<Predicate, Form> {
    match c {
        Form::Predicate(p) => Ok(p),
        Form::PredExpr(e) => Ok(Predicate::Expr(e)),
        Form::Command(Command::Filter(e)) => Ok(Predicate::Expr(e)),
        Form::Command(Command::Select(p)) => Ok(p),
        // a quarq of one command: a filter or a selection as such,
        // an expression or a bare projection as the truth of its value
        // (`syntax:predicate(q(::x))`)
        Form::Pipeline(mut cmds) if cmds.len() == 1 => match cmds.pop() {
            Some(Command::Filter(e)) => Ok(Predicate::Expr(e)),
            Some(Command::Select(p)) => Ok(p),
            Some(Command::Expr(Operand::Group(e))) => Ok(Predicate::Expr(*e)),
            Some(Command::Expr(op)) => Ok(Predicate::Expr(PredExpr::Truthy(op))),
            Some(Command::Nav(b)) => Ok(Predicate::Expr(PredExpr::Truthy(Operand::Rel {
                steps: b.steps,
                projection: b.projection,
                anchor: b.anchor,
            }))),
            Some(c) => Err(Form::Command(c)),
            None => unreachable!("one"),
        },
        Form::Operand(op) => Ok(Predicate::Expr(PredExpr::Truthy(op))),
        other => Err(other),
    }
}

fn as_pred_expr(c: Form) -> Result<PredExpr, Form> {
    match as_predicate(c)? {
        Predicate::Expr(e) => Ok(e),
        other => Err(Form::Predicate(other)),
    }
}

fn as_command(c: Form) -> Result<Command, Form> {
    match c.clone().into_commands(false) {
        Ok(mut cmds) if cmds.len() == 1 => Ok(cmds.pop().expect("one")),
        _ => Err(c),
    }
}

fn as_branch(c: Form) -> Result<Branch, Form> {
    match c {
        Form::Branch(b) => Ok(b),
        Form::Command(Command::Nav(b)) => Ok(b),
        other => match other.clone().into_walk() {
            Ok(w) if w.predicates.is_empty() => Ok(Branch {
                steps: w.elems,
                projection: w.projection,
                anchor: Anchor::Current,
            }),
            _ => Err(other),
        },
    }
}

fn as_query(c: Form) -> Result<Query, Form> {
    match c {
        Form::Query(q) => Ok(q),
        Form::Pipeline(cmds) => Ok(Query {
            pipeline: cmds,
            ..empty_query()
        }),
        other => {
            let b = as_branch(other)?;
            Ok(Query {
                branches: vec![b],
                ..empty_query()
            })
        }
    }
}
