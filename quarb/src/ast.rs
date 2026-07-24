//! The query AST.
//!
//! A query is a root-anchored sequence of navigation steps. Each step
//! is an axis (which direction to move), a matcher (which name to
//! keep), and an optional leaf anchor.

use crate::value::Value;
use globset::GlobMatcher;
use regex::Regex;

/// A parsed query: a union of navigation branches, then a pipeline of
/// stages applied to the union.
#[derive(Debug, Clone)]
pub struct Query {
    /// Prior expressions joined by `<=>`, evaluated into the context
    /// trace before this query's body; empty for a plain query.
    pub correlations: Vec<Query>,
    pub branches: Vec<Branch>,
    pub pipeline: Vec<Stage>,
    /// Meaningful on a correlation entry: `true` when the marker
    /// following it was `<=>?` — the outer join. The entry's
    /// context may then bind null: a current-side row that no
    /// tuple admits survives with this witness slot null, its
    /// trace-referencing predicates vacuous (the ON clause of a
    /// LEFT JOIN).
    pub outer: bool,
}

/// One `||` branch: navigation and an optional projection.
#[derive(Debug, Clone)]
pub struct Branch {
    pub steps: Vec<PathElem>,
    pub projection: Option<Projection>,
    /// `^` — navigate from the arbor root rather than the current
    /// node. At the top level the two coincide; inside a subcontext
    /// body the anchor reaches back to the root (`.t(^/row @|
    /// count)` — the whole-table total from any capsa).
    pub anchored: bool,
    /// `(name)` — navigate from the node marked `name` (most
    /// recent mark under that name in scope). Exclusive with
    /// `anchored`.
    pub mark: Option<String>,
}

/// A pipeline stage.
///
/// The execution unit here is the *capsa* — a node with a register
/// (its breadcrumbs) and a current topic (the scalar being worked on).
#[derive(Debug, Clone)]
pub enum Stage {
    /// `| f` — a per-capsa scalar transform of the topic.
    Func(FnCall),
    /// `| .` or `| .name` — push the topic onto the register.
    Push(Option<String>),
    /// `| .(expr)` or `| .name(expr)` — evaluate `expr` from the
    /// current node, reduce it to a scalar, set it as the topic, and
    /// push it (grouped aggregation).
    Subcontext {
        name: Option<String>,
        body: Box<Query>,
    },
    /// `| ::a * ::b` — a value expression evaluated against the
    /// capsa's node, set as the topic.
    Expr(Operand),
    /// `| .(::a * ::b)` or `| .name(::a * ::b)` — a value expression
    /// evaluated against the capsa's node, set as the topic and
    /// pushed (a computed column).
    ExprPush { name: Option<String>, expr: Operand },
    /// `@| f` — reduce the whole context's topics to a new context.
    /// Keyed aggregates (`sort_by`, `unique_by`, `min_by`, `max_by`,
    /// `top`, `bottom`) instead reorder or filter the capsae
    /// themselves, preserving nodes and registers.
    Agg(FnCall),
    /// `@| [n]` / `@| [a..b]` — positional selection of capsae from
    /// the whole context, with the same numbering as the
    /// navigation-side index and range predicates.
    Select(Predicate),
    /// `| [cond]` — a per-capsa filter: each capsa is kept iff the
    /// condition holds against its node.
    Filter(PredExpr),
    /// `| $.`, `| $.name`, `| @.` — recall a register value as the
    /// topic.
    Recall(RegRef),
    /// `| ...` — the spread: fork a list topic into one thread per
    /// element (Cypher's UNWIND). Core syntax, not a function — it
    /// computes nothing, it changes how many threads exist. Null
    /// forks to nothing; a non-list scalar passes through; a record
    /// passes whole. The outer form `| ...?` differs in exactly one
    /// case: where the plain spread would fork to nothing (null or
    /// an empty list), it emits ONE thread with a null topic — the
    /// row-multiplying half of OPTIONAL MATCH / LEFT JOIN.
    Spread { outer: bool },
    /// `| /path…` — a navigation stage: resume navigation from each
    /// capsa's node (or from `^` / a `(name)` mark), fanning out
    /// into one capsa per result with registers and marks carried
    /// forward — the pipeline spelling of a path continuation. The
    /// steps behave in every respect (predicates, marks, groups,
    /// the navigation-predicate scope law) as if written in the
    /// path. Legal only in navigation mode — no live topic; after a
    /// projection, a push (`| .`) files the topic and navigation
    /// resumes. A trailing projection moves the thread to scalar
    /// mode exactly like a branch projection. Refused under `@|`
    /// and `$|`: hops are per-thread.
    Nav(Branch),
    /// `$| stage` — the map pipe, the scope-dual of `@|`: where
    /// `@|` hands its stage ALL topics at once (the context), `$|`
    /// hands it ONE element at a time, within the topic — per
    /// capsa, capsae unchanged. `$| f` maps, `$| [cond]` filters
    /// elements (`$_` = the element; a comprehension's WHERE),
    /// `$| [a..b]` slices. Null maps to null; a non-list topic is
    /// a singleton; an expanding inner stage flattens.
    Map(Box<Stage>),
}

/// A reference into a capsa's register.
#[derive(Debug, Clone)]
pub enum RegRef {
    /// `$.` — the top (most recently pushed) regula.
    Top,
    /// `$.N` — the regula at 1-based index `N`.
    Index(usize),
    /// `$.name` — a named regula.
    Named(String),
    /// `@.` — the whole register, as a list.
    Whole,
    /// `%.` — the *named* view of the register, as a record: one
    /// field per named regula, in first-push order, carrying the
    /// latest value pushed under that name. Unnamed regulae are
    /// invisible to it (they stay reachable positionally).
    Record,
}

/// A pipeline-stage function call: `name` or `name(arg, …)`.
#[derive(Debug, Clone)]
pub struct FnCall {
    pub name: String,
    pub args: Vec<Arg>,
}

/// One function argument: a literal, a value expression evaluated
/// per capsa against its node (the key of a keyed aggregate like
/// `sort_by(::age)`), or an offset range (`window(-2..0)`, either
/// end optional).
#[derive(Debug, Clone)]
pub enum Arg {
    Lit(Value),
    Expr(Operand),
    Range(Option<i64>, Option<i64>),
}

/// The field name an expression carries on its own, for `record(...)`
/// auto-naming: the projection's key (`::href` → `href`,
/// `/a/b::c` → `c`, `:::name` → `name`, `;;;tag` → `tag`). Computed
/// expressions and bare paths carry none and need an explicit name.
pub fn auto_field_name(op: &Operand) -> Option<&str> {
    if let Operand::Outer(inner) = op {
        return auto_field_name(inner);
    }
    if let Operand::Recall(RegRef::Named(n)) = op {
        return Some(n);
    }
    if matches!(op, Operand::Ordinal) {
        return Some("ordinal");
    }
    if let Operand::Edge { projection } | Operand::Edges { projection } = op {
        return Some(match projection {
            Some(Projection::Property(Some(k))) => k,
            _ => {
                if matches!(op, Operand::Edge { .. }) {
                    "edge"
                } else {
                    "edges"
                }
            }
        });
    }
    let projection = match op {
        Operand::Rel { projection, .. } | Operand::Ctx { projection, .. } => projection.as_ref(),
        _ => None,
    }?;
    match projection {
        Projection::Property(Some(k)) => Some(k),
        Projection::CoreMeta(k) | Projection::AdapterMeta(k) => Some(k),
        Projection::Property(None) => None,
    }
}

/// A projection turning the node context into scalar values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// `::name` (a named property) or `::` (the default projection).
    Property(Option<String>),
    /// `:::key` — engine-computed core metadata.
    CoreMeta(String),
    /// `;;;key` — adapter-defined metadata.
    AdapterMeta(String),
}

/// One element of a navigation path: a plain hop, a parenthesized
/// path-pattern group (alternation + quantifier), or — inside a
/// group — a breadcrumb push.
#[derive(Debug, Clone)]
pub enum PathElem {
    /// `.name` (bare, node context) — mark the current node under
    /// `name` in the thread's mark store; `(name)` anchors on it
    /// later. Not a hop: navigation continues from the same node.
    Mark(String),
    Step(Step),
    Group(Group),
    /// `.(body)` / `.name(body)` inside a path pattern: evaluate the
    /// body from the path's current node (with `$-` in scope after
    /// an edge hop), reduce to a scalar, and push it onto the
    /// expansion path's register. A pattern that pushes yields one
    /// result per path, the register carried into the result capsa.
    Push {
        name: Option<String>,
        body: PushBody,
    },
}

/// A pattern push's body: a navigating sub-query (reduced like a
/// subcontext), or a plain value expression.
#[derive(Debug, Clone)]
pub enum PushBody {
    Query(Box<Query>),
    Expr(Operand),
}

/// `( alt | alt | … ) quant reach` — a path-pattern group. Expansion
/// follows simple-path semantics: within one expansion path no node
/// is visited twice (the start node included), and open-ended
/// quantifiers stop at the adapter's quantifier bound.
#[derive(Debug, Clone)]
pub struct Group {
    /// Alternative subpaths (at least one, each non-empty). Each
    /// repetition takes the union over the alternatives.
    pub alts: Vec<Vec<PathElem>>,
    pub quant: Quant,
    /// `[...]` expression predicates on the group's matches,
    /// applied per repetition count BEFORE reach — so
    /// `(...)+[P]?` is "the nearest satisfying P" (a shortest
    /// path search, stopping at the first tier with a survivor).
    /// Positional predicates are refused (no ordering across
    /// tiers). Mirrors the axis rule: matcher before reach.
    pub predicates: Vec<Predicate>,
    /// `?` / `!` after the quantifier: keep only the matches at the
    /// smallest / largest repetition count. Default keeps all.
    pub reach: Reach,
}

/// `{m,n}`-style repetition. `max` is `None` for the open-ended
/// forms, clamped to the adapter's quantifier bound at execution.
/// A plain group is `{1,1}`; `+` is `{1,}`; `*` is `{0,}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quant {
    pub min: usize,
    pub max: Option<usize>,
}

/// One navigation step.
#[derive(Debug, Clone)]
pub struct Step {
    pub axis: Axis,
    pub matcher: Matcher,
    /// `<...>` trait filters. Multiple clauses are ANDed together.
    pub traits: Vec<TraitClause>,
    /// `[...]` predicates. All must pass.
    pub predicates: Vec<Predicate>,
    /// Trailing `$`: keep the match only if it is a leaf (no children).
    pub leaf: bool,
}

/// A `[...]` filter on a step.
#[derive(Debug, Clone)]
pub enum Predicate {
    /// `[n]` — keep the `n`-th node (1-based; negative counts from
    /// the end, `[-1]` is the last) of the hop's result list from
    /// one source node, as filtered by the predicates to its left.
    /// Position among the hop's results, *not* sibling position —
    /// that is spelled `[:::index = n]`. Out of range selects
    /// nothing.
    Index(i64),
    /// `[a..b]` — keep the inclusive positional range of the hop's
    /// result list, under the same rules as `Index`. Either end may
    /// be omitted (`[2..]`, `[..3]`) or negative (`[2..-1]`); ends
    /// clamp to the list.
    Range(Option<i64>, Option<i64>),
    /// `[expr]` — keep nodes for which `expr` is truthy.
    Expr(PredExpr),
}

/// A boolean predicate expression.
#[derive(Debug, Clone)]
pub enum PredExpr {
    Or(Box<PredExpr>, Box<PredExpr>),
    And(Box<PredExpr>, Box<PredExpr>),
    Not(Box<PredExpr>),
    /// A comparison between two operands.
    Compare(Operand, CmpOp, Operand),
    /// A bare operand, taken for its truthiness (a structural path is
    /// truthy when it selects any node; a projection when its value is
    /// truthy).
    Truthy(Operand),
}

/// An operand: a value relative to the current node, or a literal.
#[derive(Debug, Clone)]
pub enum Operand {
    /// A descending path from the current node, with an optional
    /// projection. Empty `steps` + a projection is a projection of the
    /// current node (`::size`); non-empty `steps` navigate first
    /// (`/address::city`).
    Rel {
        steps: Vec<PathElem>,
        projection: Option<Projection>,
        /// `^` — the operand navigates from the arbor root rather
        /// than the current node, mirroring the branch anchor:
        /// `[;;;short = ^/tags/*;;;short]` compares against a set
        /// gathered elsewhere in the arbor (existentially, like any
        /// multi-valued operand).
        anchored: bool,
        /// `(name)` — the operand navigates from the node marked
        /// `name`. Exclusive with `anchored`; an unset mark yields
        /// no values.
        mark: Option<String>,
    },
    /// A literal string or number.
    Lit(Value),
    /// An arithmetic combination of two operands, each contributing
    /// its first value; operands without a numeric reading make the
    /// result null (spec: Value Expressions and Arithmetic).
    Arith {
        op: ArithOp,
        left: Box<Operand>,
        right: Box<Operand>,
    },
    /// Unary minus.
    Neg(Box<Operand>),
    /// A parenthesized sub-expression used as a value; a boolean
    /// group in operand position is its truth value.
    Group(Box<PredExpr>),
    /// `$.name` / `$.` — a register recall as an operand. Null when
    /// no capsa scope is in reach (e.g. navigation predicates).
    Recall(RegRef),
    /// `$_` — the topic: the current pipeline value.
    Topic,
    /// `$ordinal` / `$ord` — the capsa's 1-based position in the
    /// current context. Ephemeral order (it changes with every
    /// sort, filter, and group), unlike the stable tree fact
    /// `:::index`.
    Ordinal,
    /// `$name` — a fragment parameter, legal only inside a `def`
    /// body; expansion replaces it with the invocation's argument
    /// form. Never reaches the executor.
    Param(String),
    /// `$1` … `$9` — a regex capture: the group captured by the last
    /// successful `=~` match in a per-capsa filter stage. Null until
    /// a filter has matched (and in navigation predicates, where no
    /// capsa exists).
    Capture(usize),
    /// `$$.name`, `$$_`, `$$ord`, `$$1` — the same capsa-scope
    /// operand, one scope *out*: the invoking capsa of the enclosing
    /// subcontext body. Each extra `$` steps out one more level;
    /// null where no enclosing scope exists.
    Outer(Box<Operand>),
    /// `$-` — the arrived-by edge: the crosslink hop the current
    /// thread most recently walked. Bare, it reads as the edge's
    /// label; projected (`$-::prop`), as the edge's own property
    /// (adapter-provided). Null where no edge has been walked — an
    /// edge-hop's own predicates and pattern stages after an edge
    /// hop are the defined scopes.
    Edge { projection: Option<Projection> },
    /// `@*` — the current context: every capsa of the stage's
    /// INPUT (the snapshot rule — a stage is the transition, so
    /// "the context" during its evaluation is what it received).
    /// Bare, the peers' topics as a list; projected (`@*::prop`,
    /// `:::`, `;;;`), the projection mapped over the peers' nodes.
    /// Null where no context exists (navigation predicates).
    Capsae { projection: Option<Projection> },
    /// `(expr | f @| g ...)` — an operand with a pipe tail. Stage
    /// semantics mirror the pipeline exactly: each value rides a
    /// pseudo-capsa (the enclosing node and register, the value as
    /// topic) through the ordinary stage machinery. `@|` treats a
    /// single list value as a context of its elements. Pushes and
    /// subcontexts refuse (a pipe inside an expression transforms a
    /// value; pushes belong to real capsae).
    Piped {
        expr: Box<Operand>,
        stages: Vec<Stage>,
    },
    /// `now()` — the invocation instant (spec: The Temporal
    /// Fragment, Determinism): one timeline point bound by the
    /// runner BEFORE evaluation begins, denoted identically by
    /// every occurrence in the query, displayed as UTC. The only
    /// nullary call operand; evaluation itself never reads a
    /// clock, and the runner lets the caller pin it (`qua --now`).
    Now,
    /// `(cond ? then : else)` — the conditional, parenthesized
    /// only (like every boolean-bearing operand). The condition is
    /// a full predicate expression, decided by truthiness; only the
    /// taken branch evaluates. Branches parse the conditional rule
    /// themselves, so chains need no inner parens:
    /// `(a < 1 ? 'low' : a < 10 ? 'mid' : 'high')`.
    Cond {
        cond: Box<PredExpr>,
        then: Box<Operand>,
        other: Box<Operand>,
    },
    /// `@-` — ALL the edges that reached this capsa on the walk's
    /// final hop, as a list (the sigil law: `$` one, `@` all). Bare,
    /// the labels; projected (`@-::prop`), each edge's property.
    /// Aggregated, never forking: a node reached by three crossings
    /// keeps one capsa whose `@-` has three elements. Empty where
    /// the walk's last element was not a hop.
    Edges { projection: Option<Projection> },
    /// `"text ${expr} text"` — an interpolated string: text segments
    /// and holes, each hole an expression evaluated in the current
    /// scope and spliced as text (null splices as empty).
    Interp(Vec<InterpSeg>),
    /// `(x ?= k1 ? r1 : k2 ? r2 : else)` — the value match: the
    /// scrutinee is evaluated once and compared against each arm's
    /// test in order (equality by the standard coercion; a regex
    /// arm tests `=~` instead); the first hit's result is the
    /// value, only that branch evaluating. The final expression —
    /// the first not followed by `?` — is the required else.
    Match {
        scrutinee: Box<Operand>,
        /// (test, is-regex, result) per arm, in document order.
        arms: Vec<(Operand, bool, Operand)>,
        other: Box<Operand>,
    },
    /// A correlation context reference, optionally navigated and
    /// projected: `$*N/child::proj` descends from the `N`-th prior
    /// context's bound node before projecting, exactly as `Rel` does
    /// from the current node. `index` is `None` for the current node
    /// (`$*`); empty `steps` projects the bound node in place
    /// (`$*1::limit`).
    Ctx {
        index: Option<usize>,
        steps: Vec<PathElem>,
        projection: Option<Projection>,
    },
}

/// One segment of an interpolated string.
#[derive(Debug, Clone)]
pub enum InterpSeg {
    Text(String),
    Expr(Operand),
}

/// An arithmetic operator (spec: Value Expressions and Arithmetic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `div` — always a float.
    Div,
    /// `idiv` — integer division, truncating toward zero.
    IDiv,
    /// `mod` — remainder; the sign follows the dividend.
    Mod,
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Match,
    NotMatch,
    /// `*=` — the left value contains the right as a substring.
    Contains,
}

/// One `<...>` trait filter. Alternatives inside it are ORed; a
/// node passes if it has at least one.
#[derive(Debug, Clone)]
pub struct TraitClause {
    pub alts: Vec<String>,
}

impl TraitClause {
    /// Whether a node carrying `node_traits` satisfies this clause.
    /// A leading `!` negates a literal (impossible in a real trait
    /// name, so the marker cannot collide); `*` matches any node
    /// that has at least one trait, `!*` a traitless one.
    pub fn matches(&self, node_traits: &[String]) -> bool {
        self.alts.iter().any(|alt| match alt.strip_prefix('!') {
            Some("*") => node_traits.is_empty(),
            Some(name) => !node_traits.iter().any(|t| t == name),
            None if alt == "*" => !node_traits.is_empty(),
            None => node_traits.iter().any(|t| t == alt),
        })
    }
}

/// The direction a step moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Axis {
    /// `/` — immediate children.
    Child,
    /// `//`, `//?`, `//!` — descendants at any depth.
    Descendant(Reach),
    /// `\` — the immediate parent.
    Parent,
    /// `\\`, `\\?`, `\\!` — ancestors at any distance.
    Ancestor(Reach),
    /// `>` — the next sibling.
    NextSibling,
    /// `<` — the previous sibling.
    PrevSibling,
    /// `>>`, `>>?`, `>>!` — following siblings at any distance
    /// (all matches / nearest match / farthest match), in document
    /// order.
    FollowingSiblings(Reach),
    /// `<<`, `<<?`, `<<!` — preceding siblings at any distance,
    /// in document order; the nearest (`?`) is the latest one
    /// before the node.
    PrecedingSiblings(Reach),
    /// `->` — outgoing crosslink; the step's matcher matches the edge
    /// label, not the target's name.
    OutLink,
    /// `<-` — incoming crosslink; the matcher matches the edge label.
    InLink,
    /// `::prop~>hint` — resolve a cross-reference: the adapter maps
    /// `(node, property, hint)` to a target node.
    Resolve {
        property: String,
        hint: Option<String>,
    },
    /// `::prop<~hint` — reverse resolution: find every node whose
    /// `prop` resolves to the current node ("what points here?").
    ReverseResolve {
        property: String,
        hint: Option<String>,
    },
}

/// For descendant/ancestor axes, which matches to keep by distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// All matches at any distance.
    All,
    /// `?` — only the nearest match(es) (minimum distance).
    Proximal,
    /// `!` — only the farthest match(es) (maximum distance).
    Distal,
}

/// How a step selects among candidate nodes by name.
#[derive(Clone)]
pub enum Matcher {
    /// A literal name (`src`, `main.rs`).
    Name(String),
    /// A glob over the name (`*.rs`, `data.*`).
    Glob(GlobMatcher),
    /// A `~(...)` regex over the name.
    Regex(Regex),
    /// `*` — any name.
    Any,
    /// `.` inside a path pattern — matches any hop name (the pattern
    /// dot wildcard; unnamed nodes included, like `*`).
    Dot,
}

impl Matcher {
    /// Whether `name` satisfies this matcher.
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Matcher::Name(n) => n == name,
            Matcher::Glob(g) => g.is_match(name),
            Matcher::Regex(r) => r.is_match(name),
            Matcher::Any | Matcher::Dot => true,
        }
    }
}

impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Matcher::Name(n) => write!(f, "Name({n:?})"),
            Matcher::Glob(g) => write!(f, "Glob({:?})", g.glob().glob()),
            Matcher::Regex(r) => write!(f, "Regex({:?})", r.as_str()),
            Matcher::Any => write!(f, "Any"),
            Matcher::Dot => write!(f, "Dot"),
        }
    }
}
