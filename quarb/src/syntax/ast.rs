//! The query AST.
//!
//! A query is a root-anchored sequence of navigation steps. Each step
//! is an axis (which direction to move), a matcher (which name to
//! keep), and an optional leaf anchor.

pub mod visit;

use crate::value::Value;
use globset::GlobMatcher;
use regex::Regex;

/// A parsed query: a union of navigation branches, then a pipeline of
/// commands applied to the union.
#[derive(Debug, Clone)]
pub struct Query {
    /// Prior expressions joined by `<=>`, evaluated into the context
    /// trace before this query's body; empty for a plain query.
    pub correlations: Vec<Query>,
    pub branches: Vec<Branch>,
    pub pipeline: Vec<Command>,
    /// Where the correlations bind: the number of the driver's
    /// pipeline commands written before the first `<=>`. Those commands
    /// run first, the join binds against the capsae as they then
    /// stand (register, marks, node), and the rest of the pipeline
    /// follows — a join forks the capsa it leaves, as a hop does.
    pub join_at: usize,
    /// Meaningful on a correlation entry: `true` when the marker
    /// following it was `<=>?` — the outer join. The entry's
    /// context may then bind null: a current-side row that no
    /// tuple admits survives with this witness slot null, its
    /// trace-referencing predicates vacuous (the ON clause of a
    /// LEFT JOIN).
    pub outer: bool,
}

/// Where a navigation (a branch, a navigation command, or an operand
/// path) starts. The parenthesized mark anchors mirror the
/// register's sigil law on the node side: `(name)` one mark by
/// name, `(N)` one by 1-based position, `(.)` the latest, `(@)`
/// ALL of the thread's marks — forking into one thread per marked
/// node — and `(@name)` every node marked under one (possibly
/// shadowed) name.
/// Every anchor resolves to a LIST of start nodes; the singular
/// forms yield at most one, and a miss yields nothing (never an
/// error).
#[derive(Debug, Clone)]
pub enum Anchor {
    /// Navigate from the current node (the default).
    Current,
    /// `^` — navigate from the arbor root. At the top level the
    /// two coincide; inside a subcontext body the anchor reaches
    /// back to the root (`.t(^/row @| count)` — the whole-table
    /// total from any capsa).
    Root,
    /// `(name)` — the most recent mark under `name` in scope.
    Mark(String),
    /// `(N)` — the mark at 1-based position `N` in the thread's
    /// mark array (names never shift positions; they are
    /// references layered on top).
    MarkIndex(usize),
    /// `(.)` — the most recent mark, named or not (the top of
    /// the mark array, mirroring the register's `$.`).
    MarkTop,
    /// `$$.-N` / `((.-N))` — the mark N places back from the latest
    /// (`$$.-2` the penultimate, also `$$..` / `((..))`).
    MarkBack(usize),
    /// `(@)` — every mark in the thread's array, in push order:
    /// the navigation forks into one thread per marked node,
    /// registers and marks carried (the sigil law's plural — it
    /// yields THREADS, not a list value; node identity never
    /// enters the value space).
    MarksAll,
    /// `(@name)` — every mark pushed under `name`, shadowed ones
    /// included, in push order; forks like `(@)`.
    MarksNamed(String),
    /// `((!expr))` — the node-id anchor: the node
    /// whose id the expression yields — a literal (`((!7))`), a
    /// value's origin (`((!$.v:%origin:id))`), a coordinate record
    /// (`:%origin`, its `id` field), or a list of them, which
    /// forks like `((@))`. The coordinate stood on again — in this
    /// run or the next, since adapters mint ids deterministically.
    /// An id the adapter never minted yields no node.
    Id(Box<Operand>),
    /// `$-:source` / `$-:target` — an end of the arrived-by edge, in
    /// the declaration's orientation (spec: Edge facts): a node
    /// operand in its own right, so a path, a projection or a node
    /// read may follow it (`$-:target::code`, `$-:source:.d`).
    /// Nothing where no edge has been walked.
    EdgeEnd(End),
}

/// Which end of an edge: the node the declaration points from, or
/// the one it points to — whichever way the hop walked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Source,
    Target,
}

impl End {
    pub fn name(self) -> &'static str {
        match self {
            End::Source => "source",
            End::Target => "target",
        }
    }
}

impl PartialEq for Anchor {
    fn eq(&self, other: &Anchor) -> bool {
        match (self, other) {
            (Anchor::Current, Anchor::Current)
            | (Anchor::Root, Anchor::Root)
            | (Anchor::MarkTop, Anchor::MarkTop)
            | (Anchor::MarksAll, Anchor::MarksAll) => true,
            (Anchor::Mark(a), Anchor::Mark(b)) | (Anchor::MarksNamed(a), Anchor::MarksNamed(b)) => {
                a == b
            }
            (Anchor::MarkIndex(a), Anchor::MarkIndex(b)) => a == b,
            (Anchor::EdgeEnd(a), Anchor::EdgeEnd(b)) => a == b,
            // Two id anchors are equal when they are spelled alike.
            (Anchor::Id(a), Anchor::Id(b)) => {
                crate::syntax::unparse::operand_text(a) == crate::syntax::unparse::operand_text(b)
            }
            _ => false,
        }
    }
}

impl Eq for Anchor {}

/// One `||` branch: navigation and an optional projection.
#[derive(Debug, Clone)]
pub struct Branch {
    pub steps: Vec<PathElem>,
    pub projection: Option<Projection>,
    /// Where the branch starts navigating (see [`Anchor`]).
    pub anchor: Anchor,
}

/// A pipeline command.
///
/// The execution unit here is the *capsa* — a node with a register
/// (its breadcrumbs) and a current topic (the scalar being worked on).
#[derive(Debug, Clone)]
pub enum Command {
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
    /// `| .:name(body)` / `| .:name` / `| .:%` — the attaching push
    /// (spec: Attached Regulae): the push it wraps files in the
    /// capsa exactly as `.name(body)` or `.%` does, and the same
    /// regulae are attached to the node the thread stands at, where
    /// `X:.name` reads them by the node's identity — the one named,
    /// or every field the fields push filed (`name` is then `None`).
    /// Takes the quantifier like any push.
    Attach {
        name: Option<String>,
        command: Box<Command>,
    },
    /// `@| f` — reduce the whole context's topics to a new context.
    /// Keyed aggregates (`sort_by`, `unique_by`, `min_by`, `max_by`,
    /// `top`, `bottom`) instead reorder or filter the capsae
    /// themselves, preserving nodes and registers.
    Agg(FnCall),
    /// `@| .name(body)` — the aggregate push: the body is a
    /// subcontext fed the context as a whole (its seed topic the
    /// list of the peers' topics, its register the regulae every
    /// capsa shares), and its value is filed under the name on EVERY
    /// capsa; the context passes on unchanged, so a later body
    /// reads the same stream and the regula just filed.
    AggPush { name: Option<String>, body: Operand },
    /// `@| %(…)` / `@| %%(…)` — the aggregate record: the
    /// constructor's fields are bodies fed the context as a whole,
    /// and the result is one capsa whose topic is the record (the
    /// enriched form opens with the shared named regulae).
    AggRecord { call: FnCall, enriched: bool },
    /// `@| @(0; $_)`, `@| 0`, `@| %(…) ++ %(…)` — a literal or a
    /// constructor after the aggregation pipe opens an expression
    /// over the context as a whole (spec: Literals and
    /// constructors): `$_` is the list of the topics, and the
    /// result is one capsa holding the expression's value.
    AggExpr(Operand),
    /// `@| %.`, `@| $.name`, `@| @.` — a register accessor after the
    /// aggregation pipe reads once: the regulae every capsa shares,
    /// as one capsa.
    AggRecall(RegRef),
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
    /// `| &$.p` — the quarq a register holds, spliced at the recall
    /// (spec: Quarqs): its body runs here as the next commands, late
    /// bound, where `&name` splices a def at parse time. A register
    /// holding no quarq refuses.
    Splice(RegRef),
    /// `| ,name` — the SESSION PUSH (spec: Capsa Operations, "Session
    /// memory", draft 166): the topic filed under the name for the
    /// rest of the session; it gathers — one thread files its topic,
    /// several file the list of their topics — and the threads pass
    /// on untouched.
    SessionPush(String),
    /// `| %%(...)` — the named register view (`%.`) enriched with
    /// the given fields: register fields first in the `%.` layout,
    /// the args after in written order, an arg overriding a
    /// register field that shares its name. The call carries the
    /// record convention, normalized to `rec`.
    RecordWith(FnCall),
    /// `| .%(...)` / `| .name%(...)` — build the record and push it
    /// in one step, anonymous or named, the topic set to the record
    ///; `.%%(...)` builds the enriched register view
    /// instead. The operand spelling `.name(%(...))` cannot exist: a
    /// record constructor in operand position would ride its first
    /// field as the topic, so the push carries the
    /// constructor itself.
    RecordPush {
        name: Option<String>,
        call: FnCall,
        enriched: bool,
    },
    /// `| ...` — the spread: fork a list topic into one thread per
    /// element (Cypher's UNWIND). Core syntax, not a command — it
    /// computes nothing, it changes how many threads exist. Null
    /// forks to nothing; a non-list scalar passes through; a record
    /// passes whole. The outer form `| ...?` differs in exactly one
    /// case: where the plain spread would fork to nothing (null or
    /// an empty list), it emits ONE thread with a null topic — the
    /// row-multiplying half of OPTIONAL MATCH / LEFT JOIN.
    Spread { outer: bool },
    /// `| .%` — the fields push: the topic is a record, and each of
    /// its fields is pushed as a named regula — the mirror image of
    /// `%.`, which reads the named regulae as a record. (`| .`
    /// files the record as ONE regula; the bare `.%` spreads it.)
    FieldsPush,
    /// `| /path…` — a navigation command: resume navigation from each
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
    /// `$| command` — the iteration pipe, the scope-dual of `@|`: where
    /// `@|` hands its command ALL topics at once (the context), `$|`
    /// hands it ONE element at a time, within the topic — per
    /// capsa, capsae unchanged. `$| f` maps, `$| [cond]` filters
    /// elements (`$_` = the element; a comprehension's WHERE),
    /// `$| [a..b]` slices. Null maps to null; a non-list topic is
    /// a singleton; an expanding inner command flattens.
    Map(Box<Command>),
    /// `.v(body){N}` — a quantified push: the push
    /// applied to the context again and again, each round reading
    /// the previous round's values (the snapshot rule) and filing
    /// one more regula on the same capsa — the iteration loop.
    /// `{N}` runs exactly N rounds; `{m;n}` at least m, then stops
    /// at a fixpoint (a round that repointed no name and changed
    /// no topic) or at n; `{m;}` likewise up to the quantifier
    /// bound.
    Repeat {
        command: Box<Command>,
        min: usize,
        max: Option<usize>,
    },
}

/// A row literal's key as `record_row` carries it: the text, and
/// whether it was quoted (a quoted key stays text where a bare one
/// reads as the literal it spells, `%(1 = …)`, `%(2013-01-01 = …)`).
pub(crate) fn row_key(name: &str) -> (&str, bool) {
    match name.strip_prefix('\u{0}') {
        Some(text) => (text, true),
        None => (name, false),
    }
}

/// The core property names (spec: Core Properties), reserved on the
/// adapter level: `:name` refuses with a pointer to `:name`.
pub const CORE_KEYS: &[&str] = &[
    "name",
    "id",
    "index",
    "depth",
    "parent-name",
    "parent-id",
    "parent-index",
    "n-children",
    "n-descendants",
    "n-siblings",
    "traits",
    "is-leaf",
    "is-root",
    "is-first-child",
    "is-last-child",
    "is-top-level",
    "is-non-tree",
    "name-path",
    "index-path",
    "id-path",
    "path",
    "source",
    "instant",
    "dpid",
    "provenance",
    "origin",
    "elided",
    "%provenance",
    "@provenance",
    "%origin",
    "@origin",
];

/// A reference into a capsa's register (spec: The Capsa). The
/// register is one sequence of regulae, values and node references
/// alike, under one name pool; the reads below number and view the
/// two kinds apart: the value side (`$.`, `@.`, `%.`, `%..`) sees
/// the regulae that are not references, the node side (`$$.`,
/// `@@.`, `%%.`, `%%..`) the references. A name reads whatever was
/// filed under it last.
#[derive(Debug, Clone)]
pub enum RegRef {
    /// `$,name` — the session memory's value under the name (spec:
    /// Capsa Operations, "Session memory", draft 166).
    Session(String),
    /// `%,` — the whole session memory as a record.
    SessionRecord,
    /// `$.` — the latest value regula.
    Top,
    /// `$.N` — the N-th value regula (1-based, references not
    /// counted).
    Index(usize),
    /// `$.-N` — the N-th value regula counted back from the latest
    /// (`$.-1` is `$.`, `$.-2` the penultimate, also `$..`).
    Back(usize),
    /// `$.name` — the latest regula under the name, whatever its
    /// kind (a reference reads as its locator).
    Named(String),
    /// `@.` — the value regulae in filing order, as a list.
    Whole,
    /// `%.` — the *named* view of the values, as a record: one
    /// field per name, in first-push order, carrying the latest
    /// value pushed under that name. Unnamed regulae are invisible
    /// to it (they stay reachable positionally).
    Record,
    /// `%.*` — the FULL value view, as a record: the named regulae
    /// exactly as `%.` renders them, plus every anonymous value
    /// regula keyed `.N` by its value position (the same number
    /// `$.N` recalls it by). `.` cannot head a push name, so the
    /// keys cannot collide — names themselves MAY be numeric (the
    /// pivot macro writes data-valued column names). Spelled `%%.`
    /// before draft 153 and `%..` before draft 163.
    FullRecord,
    /// `@@.` — the node references in filing order, as a list.
    NodeWhole,
    /// `%%.` — the named view of the references: the latest
    /// reference per name, as a record.
    NodeRecord,
    /// `%%.*` — the full node view: the named references plus the
    /// anonymous ones keyed `.N` by their reference position
    /// (`%%..` before draft 163).
    NodeFullRecord,
}

/// A simple command: `name` or `name(operand; …)`.
#[derive(Debug, Clone)]
pub struct FnCall {
    pub name: String,
    pub args: Vec<Arg>,
}

impl FnCall {
    /// The `--of=expr` / `-o=expr` option of a reducing aggregate
    /// (spec: Aggregating Pipe, draft 164): the expression read on
    /// each item of the input in turn before the reduction.
    pub fn of_expr(&self) -> Option<&Operand> {
        self.args.iter().find_map(|a| match a {
            Arg::Opt {
                name,
                value: Some(e),
            } if name == "--of" || name == "-o" => Some(e),
            _ => None,
        })
    }

    /// The call without its `--of` option, for the reduction itself.
    pub fn without_of(&self) -> FnCall {
        FnCall {
            name: self.name.clone(),
            args: self
                .args
                .iter()
                .filter(|a| !matches!(a, Arg::Opt { name, .. } if name == "--of" || name == "-o"))
                .cloned()
                .collect(),
        }
    }
}

/// One command operand: a literal, a value expression evaluated
/// per capsa against its node (the key of a keyed aggregate like
/// `sort_by(::age)`), or an offset range (`window(-2..0)`, either
/// end optional).
#[derive(Debug, Clone)]
pub enum Arg {
    /// A record field's key that is no text (spec: Record, draft
    /// 166): `%(1 = "a"; 2013-01-01 = 5)`, the value of the field
    /// following it, as a text key's `Lit(Str)` does.
    Key(Value),
    Lit(Value),
    Expr(Operand),
    Range(Option<i64>, Option<i64>),
    /// A named option of the call (`csv(";")` carries its
    /// delimiter as `delimiter`), typed rather than a marker
    /// string a consumer has to recognise.
    Named {
        name: String,
        value: Value,
    },
    /// An option of a simple command, spelled as in the shell in
    /// the one option segment that closes the parentheses
    /// (`upper(-i)`, `sort(::name; --locale=ru)`): the name as
    /// written with its dashes, and the value `=` bound to it (a
    /// switch has none). The void option `-=` opens a segment and
    /// is not kept.
    Opt {
        name: String,
        value: Option<Operand>,
    },
    /// `%(@(a; b) = @(1; 2))`, `%($.k = $.v)` — the pairwise fields
    /// (spec: the record constructor): a list of names on the left
    /// of `=` takes a list of values pairwise, in order, among the
    /// ordinary fields in the order written.
    Pairs {
        keys: Operand,
        values: Operand,
    },
}

/// The field name an expression carries on its own, for `record(...)`
/// auto-naming: the projection's key (`::href` → `href`,
/// `/a/b::c` → `c`, `:name` → `name`, `:::tag` → `tag`). Computed
/// expressions and bare paths carry none and need an explicit name.
pub fn auto_field_name(op: &Operand) -> Option<&str> {
    if let Operand::Invoking(inner) | Operand::Position(inner) | Operand::Driver(inner) = op {
        return auto_field_name(inner);
    }
    if let Operand::Recall(RegRef::Named(n)) = op {
        return Some(n);
    }
    // `:title` — a record's field derives its name as a projection
    // does: `group(:title)`, `%(:title, 'n', 1)`; a projector
    // (the head's `= %(:A; :B)`) by its bare key likewise.
    if let Operand::Field { name, .. } = op {
        return Some(name);
    }
    if let Operand::Typed { inner, .. } = op {
        return auto_field_name(inner);
    }
    if let Operand::Lit(crate::Value::Projection { key, .. }) = op {
        return Some(key);
    }
    if let Operand::ValueMeta { key, .. } = op {
        return Some(key);
    }
    // `<-link:.v` — an attached regula derives its name as a
    // recall does.
    if let Operand::PeerReg {
        reg: RegRef::Named(n),
        ..
    } = op
    {
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
    /// `:key` — a core field, the fields the engine gives every node.
    CoreMeta(String),
    /// `:::key` — adapter-defined metadata.
    AdapterMeta(String),
}

/// One element of a navigation path: a plain hop, a parenthesized
/// path-pattern group (alternation + quantifier), or — inside a
/// group — a breadcrumb push.
#[derive(Debug, Clone)]
pub enum PathElem {
    /// `.name` (bare, node context) — mark the current node in the
    /// thread's mark array, under `name` when one is given; a bare
    /// `.` marks anonymously (the slot is still `((.N))`-addressable,
    /// and `((.))`/`((@))` see it). A name is an identifier or a
    /// quoted string — positions number themselves. Not a hop:
    /// navigation continues from the same node.
    Mark(Option<String>),
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

/// `( alt | alt | … ) quant reach` — a path-pattern group. A set
/// walk (an open quantifier with a floor of at most one, under all
/// matches or the proximal reach, no push or mark inside) reaches
/// each node once, at the fewest rounds, and runs to its fixpoint;
/// every other group is an exact walk over acyclic walks: within one
/// expansion path no node is visited twice (the start node
/// included), the rounds capped by the adapter's quantifier bound
/// and the paths in flight by the frontier bound.
#[derive(Debug, Clone)]
pub struct Group {
    /// Alternative subpaths (at least one, each non-empty). Each
    /// repetition takes the union over the alternatives.
    pub alts: Vec<Vec<PathElem>>,
    pub quant: Quant,
    /// `[...]` expression predicates on the group's matches,
    /// applied per repetition count BEFORE reach — so
    /// `(...)+[P]?` is "the proximal satisfying P" (a shortest
    /// path search, stopping at the first tier with a survivor).
    /// Positional predicates are refused (no ordering across
    /// tiers). Mirrors the axis rule: matcher before reach.
    pub predicates: Vec<Predicate>,
    /// `<...>` trait filters on the group's matches, tested with
    /// the predicates (before reach): `(>token){2}<word>` is the
    /// second next token when it is a word. Multiple clauses are
    /// ANDed together.
    pub traits: Vec<TraitClause>,
    /// `?` / `!` after the quantifier: keep only the matches at the
    /// smallest / largest repetition count. Default keeps all.
    pub reach: Reach,
}

/// `{m,n}`-style repetition. `max` is `None` for the open-ended
/// forms, clamped to the adapter's quantifier bound at execution.
/// A plain group is `{1,1}`; `+` is `{1,}`; `*` is `{0,}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quant {
    pub min: usize,
    pub max: Option<usize>,
    /// A budget instead of a count (spec: The budget quantifier):
    /// `{;100_km}`, `{10_km;100_km}`, `{2_h;}` — the floor and the
    /// ceiling as written, quantities the walk sums an edge weight
    /// against. With a budget `min` is 0 and `max` is `None`.
    pub budget: Option<Budget>,
}

/// A budget quantifier's bounds, as written: each a quantity
/// literal (`100_km`, `2_h`), resolved at the walk under the run's
/// declared units; an absent floor includes the start at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub floor: Option<String>,
    pub ceiling: Option<String>,
}

/// One navigation step.
#[derive(Debug, Clone)]
pub struct Step {
    pub axis: Axis,
    pub matcher: Matcher,
    /// The name algebra (spec: Name algebra): designators joined
    /// by `&&`, `||` and a leading `!`, in CNF. On a crosslink hop
    /// it is the label selection, `->(a && !b)`, read over the
    /// labels; on every other axis it reads the node's name,
    /// `/(*.rs && !test_*)`, the complement among the hop's own
    /// candidates. Empty when the matcher alone names the hop; the
    /// matcher is `Any` when it is set.
    pub labels: Vec<LabelClause>,
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
    /// that is spelled `[:index = n]`. Out of range selects
    /// nothing.
    Index(i64),
    /// `[a..b]` — keep the inclusive positional range of the hop's
    /// result list, under the same rules as `Index`. Either end may
    /// be omitted (`[2..]`, `[..3]`) or negative (`[2..-1]`); ends
    /// clamp to the list.
    Range(Option<i64>, Option<i64>),
    /// `[expr]` — keep nodes for which `expr` is truthy.
    Expr(PredExpr),
    /// `[2; 4; 5]`, `[1; 3..4; -1]` — a position list (spec: Index
    /// Predicates): positions and ranges, each read as `Index` /
    /// `Range`, the selection kept in the hop's order, a position
    /// named twice kept once. The items are `Index` and `Range`.
    Positions(Vec<Predicate>),
    /// `[@(2; 4; 5)]`, `[$.pos]`, `[$.n]` — brackets that look at no
    /// node: an expression of literals and registers alone, read at
    /// run time as a position (an integer) or a position list (a
    /// list of integers); any other value is taken for its
    /// truthiness.
    Computed(Operand),
}

/// A boolean predicate expression.
#[derive(Debug, Clone)]
pub enum PredExpr {
    Or(Box<PredExpr>, Box<PredExpr>),
    And(Box<PredExpr>, Box<PredExpr>),
    Not(Box<PredExpr>),
    /// A comparison between two operands.
    Compare(Operand, CmpOp, Operand),
    /// A comparison chain `a R b S c …` (spec: Chains in
    /// Predicates): the conjunction of the adjacent comparisons,
    /// every operand evaluated once. Two or more operators.
    Chain(Vec<Operand>, Vec<CmpOp>),
    /// A bare operand, taken for its truthiness (a structural path is
    /// truthy when it selects any node; a projection when its value is
    /// truthy).
    Truthy(Operand),
}

/// One segment of a pattern literal on `=` / `!=`:
/// quoted literal text and the glob star. The stars live outside
/// the quotes, so the quoted text is always literal and no escape
/// mechanism exists or is needed; strict alternation is enforced
/// by the parser (stars never touch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatSeg {
    Star,
    Lit(String),
}

/// An operand: a value relative to the current node, or a literal.
/// One index of a value (spec: Indexing a Value): a position
/// (1-based, negative from the end), a range with either end open,
/// or a column's name.
#[derive(Debug, Clone)]
pub enum IndexSpec {
    At(i64),
    Range(Option<i64>, Option<i64>),
    Name(String),
    /// A position list: `v[2; 4; 5]`, the rows `t[@(2; 4)]`, read
    /// in the value's order, a position named twice once.
    List(Vec<IndexSpec>),
    /// `[@(2; 4)]`, `[$.pos]`: an expression read when the value is
    /// indexed, an integer the position, a list of integers the
    /// position list; any other value a condition, judged once.
    Expr(Box<Operand>),
    /// `t[:A > 0]`, `v[$_ > 2]`, `t[nulls = 0]` — a condition on the
    /// rows of a table or the items of a list (spec: Indexing a
    /// Value, draft 166), each judged as the topic, as `| [cond]`
    /// judges them; the table keeps its header and index.
    Cond(Box<PredExpr>),
    /// Three or more written coordinates, `v[2; 4; 5]`: a position
    /// list on a list or a record; a table has two coordinates and
    /// refuses (its listed rows are `t[@(2; 4; 5)]`).
    Coords(Vec<IndexSpec>),
}

/// The shape after the projection operator's colon: a constructor or
/// an expression (`@(…)`, `%(…)`, `(…)`, `$.proj`) whose value is
/// read on the base, or the `#(…)` form, the table of the named
/// columns (each item a bare projection or `name = expr`).
#[derive(Debug, Clone)]
pub enum ProjShape {
    Value(Box<Operand>),
    /// `#(:A; :B)`, `#(:A; :B; -i=:A)`: the columns, and the index
    /// option naming one of them (spec: The Table, "The index").
    Columns {
        items: Vec<(Option<String>, Operand)>,
        index: Option<Box<Operand>>,
    },
}

/// One row of a table literal: a list row, or a record row whose
/// items are named or positional.
#[derive(Debug, Clone)]
pub enum TableRow {
    List(Vec<Operand>),
    Record(Vec<(Option<String>, Operand)>),
}

#[derive(Debug, Clone)]
pub enum Operand {
    /// A pattern literal — `*"sub"*`, `"app"*`, `*".gz"`,
    /// `*"a"*"b"*` — legal only as the right operand of `=` / `!=`
    /// (the parser assembles it from a glued star/string chain and
    /// refuses it anywhere else). The stars are syntax, not data: a
    /// value containing `*` never globs.
    Pattern(Vec<PatSeg>),
    /// A path from the current node — descending, ascending, sideways,
    /// or across a crosslink — with an optional projection. Empty
    /// `steps` + a projection is a projection of the
    /// current node (`::size`); non-empty `steps` navigate first
    /// (`/address::city`). The anchor mirrors the branch anchor:
    /// `[:::short = ^/tags/*:::short]` compares against a set
    /// gathered elsewhere (existentially, like any multi-valued
    /// operand); `(name)`/`(N)`/`(.)` start from one marked node;
    /// `(@)`/`(@name)` union the values gathered from every
    /// matching mark. An unset anchor yields no values.
    Rel {
        steps: Vec<PathElem>,
        projection: Option<Projection>,
        anchor: Anchor,
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
    /// `a ++ b`, `a ++! b`, `a +@+ b` — concatenation (spec: Value
    /// Expressions, Concatenation): a binary operator at the
    /// additive level, the left side giving the shape — a list, a
    /// record, a table or a text; null on either side is null.
    Concat {
        mode: ConcatMode,
        left: Box<Operand>,
        right: Box<Operand>,
    },
    /// `%(…)` on the right of a concatenation: a row under the table
    /// literal's rules, its items positional by the left side's
    /// header or named, resolved at run time; on a record or a list
    /// a named row is the record it spells. Nowhere else an operand.
    Row(Vec<(Option<String>, Operand)>),
    /// Unary minus.
    Neg(Box<Operand>),
    /// A glued minus before a projector (`-::fare`): the
    /// descending marker on a sort key (spec: Keyed Aggregates) —
    /// the key descends whatever its type, and ascends under
    /// `--desc`. Anywhere else the negation, as `Neg`.
    Desc(Box<Operand>),
    /// `v:@(:id; 1)`, `v:%(n = :name)`, `t:#(:A; :B)`, `v:($.cols)`,
    /// `v:$.proj` — the projection operator (spec: The Projection
    /// Operator, draft 166): the constructor after a glued colon is
    /// applied to the value on its left — every projector in
    /// it, at any depth, replaced by its reading on that value (on
    /// each node, for a path), every other value kept. The `#`
    /// shape is the table of the named columns.
    Projected {
        base: Box<Operand>,
        shape: ProjShape,
    },
    /// `:props...` — the spread suffix on a record item: a list of
    /// projectors spread into the record's fields, each keyed
    /// by its bare key and read on the cursor (`%(:props...)` is what
    /// `| %` abbreviates).
    Spread(Box<Operand>),
    /// The first argument of a reducing callee (`sum(/*::n)`): the
    /// values it reaches, gathered into one list as the command's
    /// input would be (spec: Value Expressions, draft 164); one
    /// value passes as it is. Transparent in reflection and unparse.
    Gathered(Box<Operand>),
    /// A parenthesized sub-expression used as a value; a boolean
    /// group in operand position is its truth value.
    Group(Box<PredExpr>),
    /// `$.name` / `$.` — a register recall as an operand. Null when
    /// no capsa scope is in reach (e.g. navigation predicates).
    Recall(RegRef),
    /// `$_` — the topic: the current pipeline value.
    Topic,
    /// `_` — the cursor (spec: The Cursor): the record of the two
    /// cursor operands, `%(topic = $_; position = $$_)`; `_:topic`
    /// and `_:position` read as the sigils do, and `_::k` is refused
    /// (the position's property is `_:position::k` or `$$_::k`).
    Cursor,
    /// `base:name` — a field of a record value: the
    /// bottom rung of the colon ladder, a value's field below a
    /// node's property. A leading `:name` reads the topic's field.
    Field { base: Box<Operand>, name: String },
    /// `t[[label]]`, `t[[x; y]]`, `t[[a..b]]`, `r[[k]]` — the lookup
    /// by label (spec: Indexing a Value, draft 166): on an indexed
    /// table the rows carrying the label, on a record the field
    /// under the key, compared as values; a range takes both ends
    /// in. Where the single bracket counts positions.
    LabelIndex {
        base: Box<Operand>,
        labels: Vec<Operand>,
    },
    /// `:A<int>`, `::price<float>`, `<int>` — a typed projector
    /// (spec: Projectors, draft 166): a projection with its trait
    /// brackets, declaring the type of the field it names; `inner`
    /// is the projection as written (the topic alone under `map`'s
    /// bare trait body, `map(<int>; --to=:A)`). It stands only where
    /// a field is declared — a header item, a record key, a
    /// structured projector's item, `map`'s body — and is refused as
    /// a value.
    Typed {
        inner: Box<Operand>,
        typing: crate::value::Typing,
    },
    /// `base:key` — a value's provenance: the core-metadata rung
    /// after a register value or the topic, defined over the
    /// origins the value was read from (`source`, `instant`,
    /// `dpid`, `provenance`, `origins`, `elided`).
    ValueMeta { base: Box<Operand>, key: String },
    /// `X:.name`, `:.name`, `X:%.` — the node read (spec: Attached
    /// Regulae): the regula attached to the node `at` reaches under
    /// the name (`RegRef::Named`), or the record of everything
    /// attached there (`RegRef::Record`); `at` is a bare node
    /// operand (a path, a mark, the invoking position, a bound
    /// node), or `None` for the current node (`:.name` alone, also
    /// inside a predicate). One value per node reached, in context
    /// order, as the previous command left the attachments; a node
    /// with nothing attached under the name yields no value (null
    /// as a scalar, nothing to an aggregate).
    PeerReg {
        at: Option<Box<Operand>>,
        reg: RegRef,
    },
    /// `%+` — the record of the last match's named groups (Perl's
    /// `%+`); the empty record outside any match.
    NamedCaptures,
    /// `@(a; b; c)` — the list literal: each item
    /// contributes every value it yields, so `@(/tags/*::)` gathers
    /// a path's values into one list. `@()` is the empty list.
    List(Vec<Operand>),
    /// `#(...)` — the table literal (spec: The Table Kind): an
    /// optional header list of names, then rows, each a list
    /// (`@(1; 2)`, an array) or a record (`%("Braund"; 22)`, a
    /// frame; an unnamed item is positional in header order, a
    /// named one matches the header by name).
    Table {
        header: Option<Vec<String>>,
        rows: Vec<TableRow>,
    },
    /// `@%(...)` — the record array (spec: List Construction):
    /// sugar over a list of records in the table literal's shape, a
    /// header list then one `%(...)` row per record, positional by
    /// the header or named; a name the header lacks extends it, null
    /// in every other row. A cell holds any value: no width is
    /// promised, the value is a plain list.
    RecordArray {
        header: Vec<String>,
        rows: Vec<Vec<(Option<String>, Operand)>>,
    },
    /// `a..b`, `a..b..step` — the range (spec: the calculator's
    /// range): every integer from `a` to `b`, one value at a time,
    /// as a path yields its matches; `5..1` counts down; a step
    /// strides, the values always running from the first bound
    /// toward the second, the step's sign saying where the stride
    /// is anchored (`a..b..-n` strides from `b`). Bounds are integer
    /// literals or parenthesized expressions.
    Range {
        from: Box<Operand>,
        to: Box<Operand>,
        step: Option<Box<Operand>>,
    },
    /// `value[i]`, `value[a..b]`, `table[i; j]` — brackets glued to a
    /// value expression index the value (spec: Indexing a Value): on
    /// a list the item or the sublist, on a table the row, the rows,
    /// or with a second index the cell, a column slice, a row slice
    /// or a sub-table. Brackets on a path step stay the predicate.
    Index {
        base: Box<Operand>,
        rows: IndexSpec,
        cols: Option<IndexSpec>,
    },
    /// `$ordinal` / `$ord` — the capsa's 1-based position in the
    /// current context. Ephemeral order (it changes with every
    /// sort, filter, and group), unlike the stable tree fact
    /// `:index`.
    Ordinal,
    /// `$name` — a fragment parameter, legal only inside a `def`
    /// body; expansion replaces it with the invocation's argument
    /// form. Never reaches the executor.
    Param(String),
    /// `$1` … `$9` — a regex capture: the group captured by the last
    /// successful `=~` match in a per-capsa predicate command. Null until
    /// a filter has matched (and in navigation predicates, where no
    /// capsa exists).
    Capture(usize),
    /// `$$__` — the invoking position (spec: The Cursor): the
    /// position at the last query-body invocation, the opener's
    /// node, held through the body's hops and rebound by a deeper
    /// query body; with an optional relative path and projection
    /// (`$$__::prop`, `$$__/child::x`); null at the top level. The
    /// only reach out of a scope: there is no scope stack and no
    /// stepping out per doubled sigil. Spelled `_` before draft 153.
    Invoking(Box<Operand>),
    /// `$$_` — the position (spec: The Cursor): the node the thread
    /// stands on, the candidate inside a predicate or a join
    /// condition, with an optional relative path and projection
    /// (`$$_::prop`, `$$_/child`); bare, a reference to the node.
    Position(Box<Operand>),
    /// `$$0` — the driver of the enclosing correlation, inside its
    /// join condition (a parse-time refusal anywhere else), with an
    /// optional relative path and projection; bare, a reference.
    Driver(Box<Operand>),
    /// `$-` — the arrived-by edge: the crosslink hop the current
    /// thread most recently walked. Bare, it reads as the edge's
    /// label; projected (`$-::prop`), as the edge's own property
    /// (adapter-provided). Null where no edge has been walked — an
    /// edge-hop's own predicates and pattern commands after an edge
    /// hop are the defined scopes.
    Edge { projection: Option<Projection> },
    /// `@_` — the context's topics as a list, the plural of `$_`:
    /// every capsa of the command's INPUT (the snapshot rule — a
    /// command is the transition, so "the context" during its
    /// evaluation is what it received). Null where no context
    /// exists (navigation predicates). Rounded `(*_)`.
    Topics,
    /// `@@_` — the context's positions as node references, the
    /// plural of `$$_`; a glued projection (`@@_::prop`) maps over
    /// them. Rounded `((*_))`.
    Positions { projection: Option<Projection> },
    /// `#.` / `#.*` / `##.` / `##.*` — the context as a table: one
    /// row per thread in context order, one column per named value
    /// regula (`#.`, as `%.` names them) or per every regula (`#.*`,
    /// the anonymous ones as `.N`, as `%.*` names them); `node` the
    /// reference face. A view built when read, columns typed by the
    /// table rule (spec: Context Accessors). `#*` and `##*` before
    /// draft 163.
    Matrix { node: bool, full: bool },
    /// `#_` — the context table (spec: The Cursor): one row per
    /// thread in context order, the columns `topic` and `position`,
    /// so `#_:topic` is `@_` and `#_:position` is `@@_`; the
    /// aggregate input, beside `_` the cursor record.
    CtxTable,
    /// The seed of a subcontext body: the enclosing capsa as the
    /// control operator that feeds the body hands it to a command
    /// — the topic (`$_`) on its node, the register readable. It
    /// stands only as the head of a headless body pipeline:
    /// `.n(cc)` is `Piped { Seed, [cc] }`, a command applied to
    /// the seed exactly as `| cc` applies to its input.
    Seed,
    /// `(expr | f @| g ...)` — an operand with a pipe tail. Command
    /// semantics mirror the pipeline exactly: each value rides a
    /// pseudo-capsa (the enclosing node and register, the value as
    /// topic) through the ordinary command machinery. `@|` treats a
    /// single list value as a context of its elements. Pushes and
    /// subcontexts refuse (a pipe inside an expression transforms a
    /// value; pushes belong to real capsae).
    Piped {
        expr: Box<Operand>,
        commands: Vec<Command>,
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
    /// `a ?? b` — the null default (spec: The Conditional, "The null
    /// default"): `a` unless it is null, then `b`; null alone is
    /// tested, so false, 0 and "" stay. The left side evaluates once,
    /// the right only when needed; `a ?? b ?? c` groups to the right.
    /// Below arithmetic and comparison, above the conditional.
    Coalesce {
        left: Box<Operand>,
        right: Box<Operand>,
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
    /// `${expr:?}` / `${expr:?message}` — the strict expansion (ruling
    /// #34): refuses when the hole would splice nothing, with the
    /// optional message. Bash's own spelling for "error if unset".
    Strict(Operand, Option<String>),
    /// `${expr:-fallback}` — the default hole:
    /// `default(expr, fallback)`, bash-faithfully spelled. The
    /// fallback is a full value expression — quote a literal.
    Default(Operand, Operand),
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
    /// `x` — the matrix product (spec: The matrix): between a matrix
    /// (an array of one numeric type) and a vector (a list of
    /// numbers), and nowhere else; a reserved word of the value
    /// grammar.
    MatMul,
}

impl ArithOp {
    /// The operator as written.
    pub fn symbol(self) -> &'static str {
        match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "div",
            ArithOp::IDiv => "idiv",
            ArithOp::Mod => "mod",
            ArithOp::MatMul => "x",
        }
    }
}

/// The concatenation operators (spec: Concatenation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcatMode {
    /// `++` — a right side of the left's own shape merges, anything
    /// else is appended as one element; a text takes the right
    /// side's text; a record's duplicate field is refused.
    Merge,
    /// `++!` — records only: the right side's field replaces the
    /// left's.
    Overwrite,
    /// `+@+` — lists only: the right side goes in as one item
    /// whatever it is.
    Item,
}

impl ConcatMode {
    /// The operator as written.
    pub fn symbol(self) -> &'static str {
        match self {
            ConcatMode::Merge => "++",
            ConcatMode::Overwrite => "++!",
            ConcatMode::Item => "+@+",
        }
    }
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
    /// `<<=` — the left value lies within the right (spec:
    /// Containment): a member of a list or a range, an instant or a
    /// period inside a period.
    Within,
    /// `>>=` — the mirror: the left value contains the right.
    Encloses,
}

/// One `<...>` trait filter. Alternatives inside it are ORed; a
/// node passes if it has at least one.
///
/// A trait is atomic: a sequence of segments joined by colons
/// (`tag:pandas`, `category:Guides`), tested by exact name. The
/// only wildcards are whole segments (spec: Traits): `.` stands
/// for one segment and `*` for a run of one or more, so `<tag:.>`
/// is a two-segment trait of the tag namespace, `<tag:*>` any
/// trait under it at any depth, `<*:draft>` draft under any
/// namespace, `<.>` any unnamespaced trait, `<*>` any trait at
/// all. Neither sign is ever part of a segment: `head*` and
/// `v1.2` stay literal names.
#[derive(Debug, Clone)]
pub struct TraitClause {
    pub alts: Vec<String>,
}

/// Whether a trait literal carries a segment wildcard — a whole
/// segment that is `.` or `*`. Such an alternative is tested
/// against the node's trait list, never pushed to an index.
pub fn trait_has_wildcard(alt: &str) -> bool {
    alt.trim_start_matches('!')
        .split(':')
        .any(|seg| seg == "." || seg == "*")
}

/// Whether a trait pattern matches a trait name segment by
/// segment: `.` takes exactly one segment, `*` one or more,
/// anything else its own text.
pub fn trait_pattern_matches(pattern: &str, name: &str) -> bool {
    fn go(pat: &[&str], name: &[&str]) -> bool {
        match (pat.split_first(), name.split_first()) {
            (None, None) => true,
            (None, Some(_)) | (Some(_), None) => false,
            (Some((&"*", rest)), Some(_)) => (1..=name.len()).any(|k| go(rest, &name[k..])),
            (Some((&".", rest)), Some((_, tail))) => go(rest, tail),
            (Some((p, rest)), Some((n, tail))) => p == n && go(rest, tail),
        }
    }
    let pat: Vec<&str> = pattern.split(':').collect();
    let segs: Vec<&str> = name.split(':').collect();
    go(&pat, &segs)
}

impl TraitClause {
    /// Whether a node satisfies this clause. A leading `!` negates
    /// a literal (impossible in a real trait name, so the marker
    /// cannot collide). A literal runs through the adapter's
    /// `has_trait`, which may honour aliases; a segment wildcard
    /// (`*`, `tag:.`, `*:draft`) runs over `traits`, the node's
    /// own list — so `*` matches any node with a trait and `!*` a
    /// traitless one.
    pub fn matches_with(&self, traits: &[String], has: impl Fn(&str) -> bool) -> bool {
        self.alts.iter().any(|alt| {
            let (neg, lit) = match alt.strip_prefix('!') {
                Some(lit) => (true, lit),
                None => (false, alt.as_str()),
            };
            let hit = if trait_has_wildcard(lit) {
                traits.iter().any(|t| trait_pattern_matches(lit, t))
            } else {
                has(lit)
            };
            hit != neg
        })
    }
}

/// One conjunct of a name algebra — `/(*.rs && !test_*)`,
/// `->(a && !b)`, spec: Name algebra — its alternatives OR'd, each
/// a designator (a name, a glob, a regex), negated or not. A step's
/// clauses are ANDed, as a trait's are. On a crosslink hop the
/// selection reads the labels a crosslink carries together:
/// `(a && b)` is one crosslink labeled both ways. `*` is any
/// label or name, so `!*` selects nothing.
#[derive(Debug, Clone)]
pub struct LabelClause {
    pub alts: Vec<LabelAlt>,
}

/// One alternative of a [`LabelClause`].
#[derive(Debug, Clone)]
pub struct LabelAlt {
    pub neg: bool,
    pub matcher: Matcher,
}

impl LabelClause {
    /// Whether a crosslink carrying `labels` satisfies the clause.
    pub fn matches(&self, labels: &[String]) -> bool {
        self.alts.iter().any(|a| {
            let hit = labels.iter().any(|l| a.matcher.matches(l));
            hit != a.neg
        })
    }
}

impl Axis {
    /// A crosslink axis: the step's matcher and name algebra read
    /// the edge labels, not the node's name.
    pub fn is_link(&self) -> bool {
        matches!(self, Axis::OutLink | Axis::InLink | Axis::BothLink)
    }
}

/// The direction a step moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Axis {
    /// `/` — immediate children.
    Child,
    /// `//`, `//?`, `//!` — descendants at any depth.
    Descendant(Reach),
    /// `*//` — the universal search: every node of the arbor,
    /// the root and its descendants in document order, then the
    /// nodes outside the tree in the adapter's order. Written
    /// only as the head of a branch; it takes no reach.
    Universal,
    /// `\` — the immediate parent.
    Parent,
    /// `\\`, `\\?`, `\\!` — ancestors at any distance.
    Ancestor(Reach),
    /// `>` — the next sibling.
    NextSibling,
    /// `<` — the previous sibling.
    PrevSibling,
    /// `>>`, `>>?`, `>>!` — following siblings at any distance
    /// (all matches / proximal / distal match), in document
    /// order.
    FollowingSiblings(Reach),
    /// `<<`, `<<?`, `<<!` — preceding siblings at any distance,
    /// in document order; the proximal (`?`) is the latest one
    /// before the node.
    PrecedingSiblings(Reach),
    /// `->` — outgoing crosslink; the step's matcher matches the edge
    /// label, not the target's name.
    OutLink,
    /// `<-` — incoming crosslink; the matcher matches the edge label.
    InLink,
    /// `--` — either-direction crosslink: the union of `->` and
    /// `<-` under one matcher. The headless member of the arrow
    /// family — an edge followed without asserting its direction;
    /// on typed data (a value node vs. the rows referencing it)
    /// only one direction usually exists, so the spelling drops
    /// the direction claim the data never needed.
    BothLink,
    /// `~` — the navop star, the wildcard of the operator slot: every
    /// edge touching the position, a tree edge or a crosslink, either
    /// direction, keeping the far ends whose edge matches the name —
    /// a child's or the parent's name, a crosslink's label: `~x` ≡
    /// `(/x|\x|->x|<-x)`, `~*` every edge. Siblings are an order, not
    /// edges, so it never walks `>` or `<`.
    Any,
    /// `~~`, `~~?`, `~~!` — the deep navop star: the walk over every
    /// edge ending at a matching one, as `//name` is to `/name`.
    AnyDeep(Reach),
    /// `::prop-->label` — resolve a cross-reference (the glued name
    /// is the reference label): the adapter maps
    /// `(node, property, label)` to a target node. With no property
    /// (`//a-->`, the bare arrow) the adapter names the node's own
    /// reference property — an html anchor's `href`.
    Resolve {
        property: Option<String>,
        label: Option<String>,
    },
    /// `<--label(SCOPE::prop)` — reverse resolution, the inverse of
    /// `-->`: every node whose `prop` resolves to the current node
    /// ("what points here?"). The argument names the referring
    /// nodes' property and, by its scope path, where to look; with
    /// no property, each candidate's own reference property; `::*`
    /// any property (a scope is then required). Bare `<--` takes the
    /// whole arbor, which an adapter must open to the walk.
    ReverseResolve {
        label: Option<String>,
        /// The scope path, root-anchored; `None` is the whole arbor.
        scope: Option<ScopePath>,
        property: RefProp,
    },
    /// `::a=>(SCOPE::b)` — the value match: the nodes of SCOPE whose
    /// `b` equals the current node's `a`, plain equality of two named
    /// properties, no resolution function, neither a key. One
    /// spelling in both directions; a 1-to-many hop.
    ValueMatch {
        property: String,
        scope: Option<ScopePath>,
        target: String,
    },
}

/// A root-anchored scope path inside an arrow's argument. Two
/// scopes are equal when they are spelled alike.
#[derive(Debug, Clone)]
pub struct ScopePath(pub Vec<PathElem>);

impl ScopePath {
    /// The scope as the canonical query text — what a model's
    /// `index SCOPE::prop;` declaration is matched against.
    pub fn text(&self) -> String {
        self.0.iter().map(crate::syntax::unparse::elem).collect()
    }
}
impl PartialEq for ScopePath {
    fn eq(&self, other: &ScopePath) -> bool {
        self.text() == other.text()
    }
}
impl Eq for ScopePath {}

/// Which property of a candidate a reverse resolution consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefProp {
    /// `(::y)` — the named property.
    Named(String),
    /// `(SCOPE::*)` — any property the candidate has.
    Any,
    /// Omitted — the candidate's own reference property.
    Default,
}

/// For descendant/ancestor axes, which matches to keep by distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// All matches at any distance.
    All,
    /// `?` — only the proximal (nearest) match(es) (minimum distance).
    Proximal,
    /// `!` — only the distal (farthest) match(es) (maximum distance).
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

#[cfg(test)]
mod trait_segment_tests {
    use super::{TraitClause, trait_has_wildcard, trait_pattern_matches};

    #[test]
    fn whole_segments_are_the_only_wildcards() {
        assert!(trait_has_wildcard("*"));
        assert!(trait_has_wildcard("tag:."));
        assert!(trait_has_wildcard("*:draft"));
        assert!(trait_has_wildcard("!tag:*"));
        assert!(!trait_has_wildcard("head*"));
        assert!(!trait_has_wildcard("v1.2"));
        assert!(!trait_has_wildcard(".draft"));
        assert!(!trait_has_wildcard("tag:pandas"));
    }

    #[test]
    fn dot_is_one_segment_star_a_run() {
        assert!(trait_pattern_matches("tag:.", "tag:capsa"));
        assert!(!trait_pattern_matches("tag:.", "tag"));
        assert!(!trait_pattern_matches("tag:.", "tag:a:b"));
        assert!(trait_pattern_matches("tag:*", "tag:capsa"));
        assert!(trait_pattern_matches("tag:*", "tag:a:b"));
        assert!(!trait_pattern_matches("tag:*", "tag"));
        assert!(trait_pattern_matches(".:draft", "label:draft"));
        assert!(!trait_pattern_matches(".:draft", "draft"));
        assert!(trait_pattern_matches("*:draft", "a:b:draft"));
        assert!(!trait_pattern_matches("*:draft", "draft"));
        assert!(trait_pattern_matches(".", "draft"));
        assert!(!trait_pattern_matches(".", "tag:draft"));
        assert!(trait_pattern_matches("*", "draft"));
        assert!(trait_pattern_matches("*", "tag:draft"));
        assert!(trait_pattern_matches("a:*:b", "a:x:y:b"));
        assert!(!trait_pattern_matches("a:*:b", "a:b"));
    }

    #[test]
    fn clause_runs_literals_through_has_and_wildcards_over_the_list() {
        let traits = vec!["tag:capsa".to_string(), "block".to_string()];
        let has = |n: &str| traits.iter().any(|t| t == n) || n == "alias";
        let c = |alts: &[&str]| TraitClause {
            alts: alts.iter().map(|s| s.to_string()).collect(),
        };
        assert!(c(&["alias"]).matches_with(&traits, has));
        assert!(c(&["tag:."]).matches_with(&traits, has));
        assert!(c(&["."]).matches_with(&traits, has));
        assert!(!c(&["*:draft"]).matches_with(&traits, has));
        assert!(c(&["!tag:*"]).matches_with(&[], has));
        assert!(!c(&["!tag:*"]).matches_with(&traits, has));
        assert!(c(&["*"]).matches_with(&traits, has));
        assert!(!c(&["*"]).matches_with(&[], has));
        assert!(c(&["!*"]).matches_with(&[], has));
        assert!(!c(&["!*"]).matches_with(&traits, has));
    }
}
