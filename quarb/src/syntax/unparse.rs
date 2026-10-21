//! The unparser: render a parsed (and expanded) [`Query`] back to
//! canonical query text.
//!
//! This is the synthesis half of reflection — the parser reads
//! queries into arbors, the unparser writes them back out — and the
//! engine of `qua --expand` (LISP's *macroexpand*: show the query
//! the fragments wrote). The output round-trips: parsing it yields a
//! query with the same meaning, and unparsing is a fixpoint
//! (`unparse(parse(unparse(q))) == unparse(q)`).

use crate::syntax::ast::{
    Arg, ArithOp, Axis, Branch, CmpOp, Command, Group, InterpSeg, Matcher, Operand, PatSeg,
    PathElem, PredExpr, Predicate, Projection, PushBody, Query, Reach, RefProp, RegRef, Step,
    TraitClause,
};
use crate::value::Value;

/// Render `query` as canonical query text. Driver-first: the
/// query's own branches lead, each joined expression follows its
/// `<=>` (the outer marker glued to the operator), and the driving
/// pipeline closes the text.
pub fn unparse(query: &Query) -> String {
    // The expression head: `= expr` parses as `^ | (expr)`, and
    // prints as it was written, since `| (3)` would reparse as the
    // capture `$3`.
    if let [branch] = query.branches.as_slice()
        && branch.steps.is_empty()
        && branch.projection.is_none()
        && matches!(branch.anchor, crate::syntax::ast::Anchor::Root)
        && query.correlations.is_empty()
        && let Some((Command::Expr(head), rest)) = query.pipeline.split_first()
    {
        let mut out = format!("= {}", operand(head));
        for command_ast in rest {
            out.push(' ');
            out.push_str(&command(command_ast));
        }
        return out;
    }
    let n = query.correlations.len();
    // The join prints where it binds: the driver's commands before
    // `join_at` precede the `<=>` entries, the rest follow them.
    let at = if n == 0 {
        0
    } else {
        query.join_at.min(query.pipeline.len())
    };
    let (pre, post) = query.pipeline.split_at(at);
    let main_pipes = post.first().is_some_and(|s| command(s).starts_with('|'));
    let pre_pipes = pre.first().is_some_and(|s| command(s).starts_with('|'));
    let mut out = branches_join(
        query,
        if at > 0 {
            pre_pipes
        } else {
            n == 0 && main_pipes
        },
    );
    for command_ast in pre {
        out.push(' ');
        out.push_str(&command(command_ast));
    }
    for (i, corr) in query.correlations.iter().enumerate() {
        out.push_str(if corr.outer { " <=>? " } else { " <=> " });
        let followed_by_main_pipe = (i + 1 == n) && main_pipes;
        let own_pipes = corr
            .pipeline
            .first()
            .is_some_and(|s| command(s).starts_with('|'));
        out.push_str(&branches_join(corr, followed_by_main_pipe || own_pipes));
        // A non-final entry may retain its own pre-join pipeline.
        for command_ast in &corr.pipeline {
            out.push(' ');
            out.push_str(&command(command_ast));
        }
    }
    for command_ast in post {
        out.push(' ');
        out.push_str(&command(command_ast));
    }
    out
}

/// The union of a query's branches. A trailing leaf anchor before a
/// plain pipe would reparse as the map pipe (`/a$ | f` relexes as
/// `/a $| f` — the token stream cannot spell them apart), so such a
/// branch reprints as a single-alternative group: `(/a$) | f`.
fn branches_join(q: &Query, followed_by_plain_pipe: bool) -> String {
    let mut parts: Vec<String> = q.branches.iter().map(branch).collect();
    if followed_by_plain_pipe
        && let Some(last) = q.branches.last()
        && branch_ends_bare_leaf(last)
    {
        let text = parts.pop().expect("branches and parts zip");
        parts.push(format!("({text})"));
    }
    parts.join(" || ")
}

/// Whether a branch's emitted text ends with the bare leaf anchor
/// `$` (a trailing leaf-anchored step with no projection after it).
fn branch_ends_bare_leaf(b: &Branch) -> bool {
    b.projection.is_none() && matches!(b.steps.last(), Some(PathElem::Step(s)) if s.leaf)
}

fn branch(b: &Branch) -> String {
    // Anchors are semantic (the root differs from the current
    // node inside a subcontext body; marks are the thread's own),
    // so they reprint whenever set.
    let mut out = String::new();
    if !universal_head(&b.anchor, &b.steps) {
        out.push_str(&anchor(&b.anchor));
    }
    for e in &b.steps {
        out.push_str(&elem(e));
    }
    if let Some(p) = &b.projection {
        out.push_str(&projection(p));
    }
    // A final mark (or named push) prints with a trailing space
    // for lexing; at the branch end the joiner supplies its own
    // spacing.
    out.truncate(out.trim_end().len());
    out
}

/// `*//` is root-anchored by construction and spells its own
/// head: the `^` is not reprinted before it.
fn universal_head(a: &crate::syntax::ast::Anchor, steps: &[PathElem]) -> bool {
    matches!(a, crate::syntax::ast::Anchor::Root)
        && matches!(
            steps.first(),
            Some(PathElem::Step(s)) if matches!(s.axis, crate::syntax::ast::Axis::Universal)
        )
}

/// An anchor's canonical spelling; empty for the default.
fn anchor(a: &crate::syntax::ast::Anchor) -> String {
    use crate::syntax::ast::Anchor;
    match a {
        Anchor::Current => String::new(),
        Anchor::Root => "^".to_string(),
        // Double parentheses are the node side.
        // `$$.name` / `$$.N` / `$$.` — the marks, node-side; the
        // rounded spellings are the double-paren anchors.
        Anchor::Mark(m) => format!("$$.{m}"),
        Anchor::MarkIndex(n) => format!("$$.{n}"),
        Anchor::MarkTop => "$$.".to_string(),
        Anchor::MarkBack(n) => format!("$$.-{n}"),
        Anchor::MarksAll => "((@))".to_string(),
        Anchor::MarksNamed(m) => format!("((@{m}))"),
        Anchor::Id(op) => format!("((!{}))", operand(op)),
    }
}

pub(crate) fn elem(e: &PathElem) -> String {
    match e {
        // A mark prints spaced for the same lexing reason as the
        // named push below; the anonymous mark is a lone dot.
        PathElem::Mark(Some(name)) => format!(" .{} ", push_name(name)),
        PathElem::Mark(None) => " . ".to_string(),
        PathElem::Step(s) => step(s),
        PathElem::Group(g) => group(g),
        PathElem::Push { name, body } => {
            // A push prints spaced: the push dot stands after
            // whitespace; glued, it would be a name
            // character.
            let lead = " .";
            let n = name.as_deref().unwrap_or("");
            match body {
                PushBody::Query(q) => format!("{lead}{n}({})", unparse(q)),
                PushBody::Expr(e) => format!("{lead}{n}({})", operand(e)),
            }
        }
    }
}

/// A path-pattern group in canonical (strict) form: every
/// alternative spells its nav-ops, `{2,2}` reprints as `{2}`, and
/// the open-ended forms reprint as `+` / `*`.
pub(crate) fn group(g: &Group) -> String {
    let alts: Vec<String> = g
        .alts
        .iter()
        .map(|alt| alt.iter().map(elem).collect())
        .collect();
    let quant = match (g.quant.min, g.quant.max) {
        (1, Some(1)) => String::new(),
        (1, None) => "+".to_string(),
        (0, None) => "*".to_string(),
        (m, Some(n)) if m == n => format!("{{{m}}}"),
        // the semicolon separates the bounds; the regex comma is sugar
        (m, Some(n)) => format!("{{{m};{n}}}"),
        (m, None) => format!("{{{m};}}"),
    };
    let preds: String = g.predicates.iter().map(predicate).collect();
    format!(
        "({}){}{}{}{}",
        alts.join("|"),
        quant,
        trait_suffix(&g.traits),
        preds,
        reach_mark(&g.reach)
    )
}

pub(crate) fn step(s: &Step) -> String {
    let mut out = String::new();
    match &s.axis {
        Axis::Child => out.push('/'),
        Axis::Descendant(Reach::All) => out.push_str("//"),
        Axis::Descendant(Reach::Proximal) => out.push_str("//?"),
        Axis::Descendant(Reach::Distal) => out.push_str("//!"),
        Axis::Universal => out.push_str("*//"),
        Axis::Parent => out.push('\\'),
        Axis::Ancestor(Reach::All) => out.push_str("\\\\"),
        Axis::Ancestor(Reach::Proximal) => out.push_str("\\\\?"),
        Axis::Ancestor(Reach::Distal) => out.push_str("\\\\!"),
        Axis::NextSibling => out.push('>'),
        Axis::PrevSibling => out.push('<'),
        Axis::FollowingSiblings(r) => out.push_str(&format!(">>{}", reach_mark(r))),
        Axis::PrecedingSiblings(r) => out.push_str(&format!("<<{}", reach_mark(r))),
        Axis::OutLink => out.push_str("->"),
        Axis::InLink => out.push_str("<-"),
        Axis::BothLink => out.push_str("--"),
        Axis::Resolve {
            property,
            label: hint,
        } => {
            if let Some(p) = property {
                out.push_str("::");
                out.push_str(&name_text(p));
            }
            out.push_str("-->");
            if let Some(h) = hint {
                out.push_str(&name_text(h));
            }
            // A resolution step carries no matcher of its own.
            return out + &step_suffix(s);
        }
        Axis::ReverseResolve {
            label,
            scope,
            property,
        } => {
            out.push_str("<--");
            if let Some(h) = label {
                out.push_str(&name_text(h));
            }
            if scope.is_some() || *property != RefProp::Default {
                out.push('(');
                if let Some(sp) = scope {
                    for e in &sp.0 {
                        out.push_str(&elem(e));
                    }
                }
                match property {
                    RefProp::Named(p) => {
                        out.push_str("::");
                        out.push_str(&name_text(p));
                    }
                    RefProp::Any => out.push_str("::*"),
                    RefProp::Default => {}
                }
                out.push(')');
            }
            return out + &step_suffix(s);
        }
        Axis::ValueMatch {
            property,
            scope,
            target,
        } => {
            out.push_str("::");
            out.push_str(&name_text(property));
            out.push_str("=>(");
            if let Some(sp) = scope {
                for e in &sp.0 {
                    out.push_str(&elem(e));
                }
            }
            out.push_str("::");
            out.push_str(&name_text(target));
            out.push(')');
            return out + &step_suffix(s);
        }
    }
    // The parent and the ancestors take no name: the wildcard is
    // implied, and the bare axis is the canonical spelling.
    let m = if !s.labels.is_empty() {
        labels(&s.labels)
    } else if matches!(s.axis, Axis::Parent | Axis::Ancestor(_))
        && matches!(s.matcher, Matcher::Any)
    {
        String::new()
    } else {
        matcher(&s.matcher)
    };
    // The lexer reads `<-3` as less-than-minus-three and `<-x` as
    // an incoming crosslink, so a digit-leading matcher after `<-`
    // needs a separating space to keep its axis on reparse; the
    // space is meaning-preserving there. A dash-leading name after
    // the sibling hop `<` cannot take a space (spaced, `<` is the
    // comparison) and reprints quoted instead: `<"-x"`. A glob led
    // by a dash has no spelling after `<` at all (quoting would make
    // it a literal name); it prints as it is and does not reparse.
    let m = if matches!(s.axis, Axis::PrevSibling)
        && matches!(s.matcher, Matcher::Name(_))
        && m.starts_with('-')
    {
        crate::value::quarb_string(&m)
    } else {
        m
    };
    let glue_breaks = match &s.axis {
        Axis::InLink => m.starts_with(|c: char| c.is_ascii_digit()),
        // `--` then a dash-leading matcher would fuse into a longer
        // dash run on reparse.
        Axis::BothLink => m.starts_with('-'),
        _ => false,
    };
    if glue_breaks {
        out.push(' ');
    }
    out.push_str(&m);
    out + &step_suffix(s)
}

fn reach_mark(r: &Reach) -> &'static str {
    match r {
        Reach::All => "",
        Reach::Proximal => "?",
        Reach::Distal => "!",
    }
}

/// `<...>` trait clauses, CNF-shaped: clauses joined by `&&`,
/// each a `||`-disjunction, parenthesized when both dimensions are
/// in play. Empty for no traits.
fn trait_suffix(traits: &[TraitClause]) -> String {
    let mut out = String::new();
    if !traits.is_empty() {
        let many = traits.len() > 1;
        let clauses: Vec<String> = traits
            .iter()
            .map(|t| {
                let body = t
                    .alts
                    .iter()
                    .map(|a| trait_lit(a))
                    .collect::<Vec<_>>()
                    .join(" || ");
                if many && t.alts.len() > 1 {
                    format!("({body})")
                } else {
                    body
                }
            })
            .collect();
        out.push('<');
        out.push_str(&clauses.join(" && "));
        out.push('>');
    }
    out
}

fn step_suffix(s: &Step) -> String {
    let mut out = trait_suffix(&s.traits);
    for p in &s.predicates {
        out.push_str(&predicate(p));
    }
    if s.leaf {
        out.push('$');
    }
    out
}

/// The canonical regex literal: `(/body/mods)`. In
/// operand position a leading inline-flag group splits out as
/// trailing modifiers; in name position (`mods` false) it stays
/// inline, the closer there being strictly `/)`. A slash in the body
/// escapes.
fn regex_literal(pat: &str, mods: bool) -> String {
    let split = || {
        let rest = pat.strip_prefix("(?")?;
        let k = rest.find(')')?;
        let flags = &rest[..k];
        (!flags.is_empty() && flags.chars().all(|c| matches!(c, 'i' | 'm' | 's' | 'x')))
            .then(|| (flags.to_string(), rest[k + 1..].to_string()))
    };
    let (flags, body) = if mods { split() } else { None }.unwrap_or_default_with(pat);
    format!("(/{}/{})", body.replace('/', "\\/"), flags)
}

trait OrPat {
    fn unwrap_or_default_with(self, pat: &str) -> (String, String);
}
impl OrPat for Option<(String, String)> {
    fn unwrap_or_default_with(self, pat: &str) -> (String, String) {
        self.unwrap_or_else(|| (String::new(), pat.to_string()))
    }
}

/// A crosslink label selection in its parentheses: the conjuncts
/// joined by `&&`, a conjunct of several alternatives by `||` in
/// its own parentheses, a negated alternative led by `!`.
pub(crate) fn labels(clauses: &[crate::syntax::ast::LabelClause]) -> String {
    let clause = |c: &crate::syntax::ast::LabelClause| -> String {
        c.alts
            .iter()
            .map(|a| format!("{}{}", if a.neg { "!" } else { "" }, matcher(&a.matcher)))
            .collect::<Vec<_>>()
            .join(" || ")
    };
    let body = if clauses.len() == 1 {
        clause(&clauses[0])
    } else {
        clauses
            .iter()
            .map(|c| {
                if c.alts.len() > 1 {
                    format!("({})", clause(c))
                } else {
                    clause(c)
                }
            })
            .collect::<Vec<_>>()
            .join(" && ")
    };
    format!("({body})")
}

pub(crate) fn matcher(m: &Matcher) -> String {
    match m {
        Matcher::Name(n) => name_text(n),
        Matcher::Glob(g) => g.glob().glob().to_string(),
        Matcher::Regex(r) => regex_literal(r.as_str(), false),
        Matcher::Any => "*".to_string(),
        Matcher::Dot => ".".to_string(),
    }
}

/// A CNF trait literal (`name` / `!name`): the negation mark stays
/// bare; the name quotes when it would not lex back as one name
/// token (a trait named "my trait" must reprint as `<'my trait'>`).
/// The bare set is the lexer's name characters — wider than
/// [`name_text`]'s, since `<*>`-style wildcards are bare traits.
fn trait_lit(lit: &str) -> String {
    // bare when every colon-joined segment is a bare name (the
    // segment wildcards `.` and `*` included): `tag:pandas`,
    // `tag:.`, `*:draft`; an empty segment or any other sign
    // quotes the whole literal
    let quote = |n: &str| {
        let bare = !n.is_empty()
            && n.split(':').all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | '+'))
            });
        if bare {
            n.to_string()
        } else {
            crate::value::quarb_string(n)
        }
    };
    match lit.strip_prefix('!') {
        Some(name) => format!("!{}", quote(name)),
        None => quote(lit),
    }
}

/// A name, quoted when it does not lex as a bare name.
fn name_text(n: &str) -> String {
    let bare = !n.is_empty()
        && n.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        && !n.starts_with('.');
    if bare {
        n.to_string()
    } else {
        crate::value::quarb_string(n)
    }
}

/// A projection key. Like [`name_text`], but also quotes the
/// predicate keywords `and`/`or`/`not`: `opt_projection_name` refuses
/// them unquoted (a bare `::and` is the default projection followed by
/// the connective keyword), so a field with one of those names must
/// reprint quoted (`::'and'`) to round-trip.
fn proj_name(k: &str) -> String {
    if matches!(k, "and" | "or" | "not") {
        crate::value::quarb_string(k)
    } else {
        name_text(k)
    }
}

pub(crate) fn projection(p: &Projection) -> String {
    match p {
        Projection::Property(None) => "::".to_string(),
        Projection::Property(Some(k)) => format!("::{}", proj_name(k)),
        Projection::CoreMeta(k) => format!(":{}", proj_name(k)),
        Projection::AdapterMeta(k) => format!(":::{}", proj_name(k)),
    }
}

pub(crate) fn predicate(p: &Predicate) -> String {
    match p {
        Predicate::Index(n) => format!("[{n}]"),
        Predicate::Range(from, to) => {
            let f = from.map(|v| v.to_string()).unwrap_or_default();
            let t = to.map(|v| v.to_string()).unwrap_or_default();
            format!("[{f}..{t}]")
        }
        Predicate::Expr(e) => format!("[{}]", pred_expr(e)),
    }
}

pub(crate) fn pred_expr(e: &PredExpr) -> String {
    match e {
        // A connective chain is left-associative, so a left operand
        // of the same connective needs no parentheses: `a || b || c`
        // prints as written.
        PredExpr::Or(a, b) => {
            let left = match a.as_ref() {
                PredExpr::Or(..) => pred_expr(a),
                _ => pred_term(a),
            };
            format!("{left} || {}", pred_term(b))
        }
        PredExpr::And(a, b) => {
            let left = match a.as_ref() {
                PredExpr::And(..) => pred_expr(a),
                _ => pred_term(a),
            };
            format!("{left} && {}", pred_term(b))
        }
        // `!` binds tightest: a negated comparison or connective
        // prints with its parentheses so the text re-parses as
        // written (`.not. ::x > 1` is `!(::x > 1)`).
        PredExpr::Not(a) => match a.as_ref() {
            PredExpr::Truthy(_) | PredExpr::Not(_) => format!("!{}", pred_expr(a)),
            _ => format!("!{}", parens(&pred_expr(a))),
        },
        PredExpr::Compare(l, op, r) => {
            // Ruling #33: the canonical spelling of a literal
            // substring test is the pattern form — `*= "x"` prints
            // as `== *"x"*`. A dynamic right operand keeps `*=`
            // (patterns are literal syntax only).
            if matches!(op, CmpOp::Contains)
                && let Operand::Lit(Value::Str(t)) = r
            {
                let pat =
                    Operand::Pattern(vec![PatSeg::Star, PatSeg::Lit(t.clone()), PatSeg::Star]);
                return format!("{} == {}", operand(l), operand(&pat));
            }
            // A pattern literal is a pattern comparison however it
            // was spelled (`= *"x"*` is heritage): `==` / `!==`.
            if let Operand::Pattern(_) = r
                && matches!(op, CmpOp::Eq | CmpOp::Ne)
            {
                let eq = if matches!(op, CmpOp::Eq) { "==" } else { "!==" };
                return format!("{} {} {}", operand(l), eq, operand(r));
            }
            // A match's literal pattern prints as `== (/x/i)` /
            // `!== (/x/i)`; a dynamic pattern (a non-literal right
            // operand) prints its operand.
            if matches!(op, CmpOp::Match | CmpOp::NotMatch)
                && let Operand::Lit(Value::Str(t)) = r
            {
                return format!("{} {} {}", operand(l), cmp(*op), regex_literal(t, true));
            }
            format!("{} {} {}", operand(l), cmp(*op), operand(r))
        }
        PredExpr::Chain(operands, ops) => {
            let mut out = operand(&operands[0]);
            for (op, o) in ops.iter().zip(&operands[1..]) {
                out.push(' ');
                out.push_str(cmp(*op));
                out.push(' ');
                out.push_str(&operand(o));
            }
            out
        }
        PredExpr::Truthy(o) => operand(o),
    }
}

/// A sub-expression of a connective: parenthesized when it is itself
/// a binary connective, so precedence survives the round-trip.
/// `!` binds tightest, so a negation never needs wrapping.
fn pred_term(e: &PredExpr) -> String {
    match e {
        PredExpr::Or(..) | PredExpr::And(..) => parens(&pred_expr(e)),
        _ => pred_expr(e),
    }
}

/// `inner` in parentheses — with a space after the opening one
/// when the text starts with a lone `:` or `;` (a register field
/// `:name`, a shell word), since `(:name` is the rounded sibling
/// hop and `(;…)` the shell literal to the lexer; `(::` lexes as
/// itself.
fn parens(inner: &str) -> String {
    if needs_gap(inner) {
        format!("( {inner})")
    } else {
        format!("({inner})")
    }
}

fn needs_gap(inner: &str) -> bool {
    (inner.starts_with(':') && !inner.starts_with("::"))
        || (inner.starts_with(';') && !inner.starts_with(";;"))
}

fn cmp(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        // the pattern comparisons; `=~` / `!~` parse as heritage sugar
        CmpOp::Match => "==",
        CmpOp::NotMatch => "!==",
        CmpOp::Contains => "*=",
        CmpOp::Within => "<<=",
        CmpOp::Encloses => ">>=",
    }
}

/// The canonical text of one operand form (macro argument splicing
/// and the expansion arbor's `::form` property).
/// `#(--width=3)` / `#(--header=@(a; b; c))` — the reshape's
/// option, long, the header's names bare.
fn reshape_text(call: &crate::syntax::ast::FnCall) -> String {
    match &call.args[0] {
        Arg::Lit(Value::List(names)) => format!(
            "#(--header=@({}))",
            names
                .iter()
                .map(|n| name_text(&n.to_string()))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Arg::Lit(v) => format!("#(--width={})", v.to_quarb()),
        _ => fn_call(call),
    }
}

pub(crate) fn operand_text(o: &Operand) -> String {
    operand(o)
}

pub(crate) fn operand(o: &Operand) -> String {
    match o {
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            let head = operand(scrutinee);
            let gap = if needs_gap(&head) { " " } else { "" };
            let mut out = format!("({gap}{head} ?=");
            for (test, regex, result) in arms {
                if *regex {
                    let Operand::Lit(Value::Str(pat)) = test else {
                        unreachable!("regex arms hold their pattern literal");
                    };
                    out.push(' ');
                    out.push_str(&regex_literal(pat, true));
                } else {
                    out.push_str(&format!(" {}", operand(test)));
                }
                out.push_str(&format!(" ? {} :", operand(result)));
            }
            out.push_str(&format!(" {})", operand(other)));
            out
        }
        Operand::Rel {
            steps,
            projection: p,
            anchor: a,
        } => {
            let mut out = String::new();
            if !universal_head(a, steps) {
                out.push_str(&anchor(a));
            }
            out.extend(steps.iter().map(elem));
            if let Some(p) = p {
                out.push_str(&projection(p));
            }
            out
        }
        Operand::Lit(v) => literal(v),
        // The canonical tight spelling: stars outside the quotes,
        // no whitespace inside the chain.
        Operand::Pattern(segs) => segs
            .iter()
            .map(|seg| match seg {
                PatSeg::Star => "*".to_string(),
                PatSeg::Lit(t) => literal(&Value::Str(t.clone())),
            })
            .collect::<String>(),
        Operand::Arith { op, left, right } => {
            format!(
                "{} {} {}",
                arith_term(left, *op, false),
                arith(*op),
                arith_term(right, *op, true)
            )
        }
        // The concatenation stands at the additive level: a looser
        // operand under it is parenthesized, and so is one of the
        // same level on its right (the parse is left-associative).
        Operand::Concat { mode, left, right } => format!(
            "{} {} {}",
            arith_term(left, ArithOp::Add, false),
            mode.symbol(),
            arith_term(right, ArithOp::Add, true)
        ),
        Operand::Row(items) => {
            let inner: Vec<String> = items
                .iter()
                .map(|(n, e)| match n {
                    Some(n) => format!("{n} = {}", body_text(e)),
                    None => body_text(e),
                })
                .collect();
            format!("%({})", inner.join("; "))
        }
        Operand::Neg(inner) => format!(
            "- {}",
            match **inner {
                Operand::Arith { .. } | Operand::Concat { .. } => parens(&operand(inner)),
                _ => operand(inner),
            }
        ),
        Operand::Gathered(inner) => operand(inner),
        Operand::Group(e) => parens(&pred_expr(e)),
        Operand::Recall(r) => reg(r),
        Operand::Topic => "$_".to_string(),
        Operand::Cursor => "_".to_string(),
        // The seed never stands alone in parsed text; a headless
        // body pipeline prints without it (see `body_text`).
        Operand::Seed => "$_".to_string(),
        Operand::Field { base, name } => match **base {
            Operand::Topic => format!(":{name}"),
            _ => format!("{}:{name}", operand(base)),
        },
        Operand::ValueMeta { base, key } => format!("{}:{key}", operand(base)),
        Operand::PeerReg { at, reg: r } => {
            format!("{}:{}", operand(at), reg(r).trim_start_matches('$'))
        }
        Operand::NamedCaptures => "%+".to_string(),
        Operand::List(items) => {
            let inner: Vec<String> = items.iter().map(|i| grouped(body_text(i))).collect();
            format!("@({})", inner.join("; "))
        }
        Operand::Coalesce { left, right } => {
            format!("{} ?? {}", body_text(left), body_text(right))
        }
        Operand::Index { base, rows, cols } => {
            let spec = |s: &crate::syntax::ast::IndexSpec| match s {
                crate::syntax::ast::IndexSpec::At(n) => n.to_string(),
                crate::syntax::ast::IndexSpec::Range(a, b) => format!(
                    "{}..{}",
                    a.map(|x| x.to_string()).unwrap_or_default(),
                    b.map(|x| x.to_string()).unwrap_or_default()
                ),
                crate::syntax::ast::IndexSpec::Name(n) => literal(&Value::Str(n.clone())),
            };
            match cols {
                Some(c) => format!("{}[{}; {}]", body_text(base), spec(rows), spec(c)),
                None => format!("{}[{}]", body_text(base), spec(rows)),
            }
        }
        Operand::Range { from, to, step } => {
            let bound = |e: &Operand| match e {
                Operand::Lit(Value::Int(n)) => n.to_string(),
                other => format!("({})", body_text(other)),
            };
            match step {
                Some(s) => format!("{}..{}..{}", bound(from), bound(to), bound(s)),
                None => format!("{}..{}", bound(from), bound(to)),
            }
        }
        Operand::RecordArray { header, rows } => {
            let mut parts: Vec<String> = vec![format!("@({})", header.join("; "))];
            for row in rows {
                let inner: Vec<String> = row
                    .iter()
                    .map(|(n, e)| match n {
                        Some(n) => format!("{n} = {}", body_text(e)),
                        None => body_text(e),
                    })
                    .collect();
                parts.push(format!("%({})", inner.join("; ")));
            }
            format!("@%({})", parts.join("; "))
        }
        Operand::Table { header, rows } => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(h) = header {
                parts.push(format!("@({})", h.join("; ")));
            }
            for row in rows {
                parts.push(match row {
                    crate::syntax::ast::TableRow::List(items) => {
                        let inner: Vec<String> = items.iter().map(body_text).collect();
                        format!("@({})", inner.join("; "))
                    }
                    crate::syntax::ast::TableRow::Record(items) => {
                        let inner: Vec<String> = items
                            .iter()
                            .map(|(n, e)| match n {
                                Some(n) => format!("{n} = {}", body_text(e)),
                                None => body_text(e),
                            })
                            .collect();
                        format!("%({})", inner.join("; "))
                    }
                });
            }
            format!("#({})", parts.join("; "))
        }
        Operand::Now => "now()".to_string(),
        Operand::Edge { projection: p } => match p {
            Some(p) => format!("$-{}", projection(p)),
            None => "$-".to_string(),
        },
        Operand::Edges { projection: p } => match p {
            Some(p) => format!("@-{}", projection(p)),
            None => "@-".to_string(),
        },
        Operand::Topics => "@_".to_string(),
        Operand::Positions { projection: p } => match p {
            Some(p) => format!("@@_{}", projection(p)),
            None => "@@_".to_string(),
        },
        Operand::Matrix {
            node: false,
            full: false,
        } => "#.".to_string(),
        Operand::Matrix {
            node: false,
            full: true,
        } => "#.*".to_string(),
        Operand::Matrix {
            node: true,
            full: false,
        } => "##.".to_string(),
        Operand::Matrix {
            node: true,
            full: true,
        } => "##.*".to_string(),
        Operand::CtxTable => "#_".to_string(),
        Operand::Piped { expr, commands } if matches!(**expr, Operand::Seed) => {
            parens(&headless(commands))
        }
        Operand::Piped { expr, commands } => {
            let tail: Vec<String> = commands.iter().map(command).collect();
            let mut head = operand(expr);
            // The same `$`-then-`|` hazard as at query level: a
            // path operand ending in a bare leaf anchor regroups
            // before a plain pipe.
            if let Operand::Rel {
                steps,
                projection: None,
                ..
            } = expr.as_ref()
                && matches!(steps.last(), Some(PathElem::Step(s)) if s.leaf)
                && tail.first().is_some_and(|t| t.starts_with('|'))
            {
                head = format!("({head})");
            }
            format!("({} {})", head, tail.join(" "))
        }
        Operand::Cond { cond, then, other } => format!(
            "({} ? {} : {})",
            pred_expr(cond),
            operand(then),
            operand(other)
        ),
        Operand::Ordinal => "$ord".to_string(),
        Operand::Param(name) => format!("${name}"),
        Operand::Capture(n) => format!("${n}"),
        // The outer-scope wrapper prefixes one more `$` to the inner
        // spelling (`$.x` → `$$.x`).
        // Each `Outer` wrap adds one `$`. Capsa-scope inners
        // (`$.name`, `$_`) already start with a dollar; the node
        // form (`::prop`, `/child::x`) needs the full `$$` spelled.
        // `$$_` is the driver; its register `$$_.name`, its node
        // form `$$_::prop` / `$$_/kid`; one `$` more per scope out.
        // `_` — the invoking position, projected or navigated.
        Operand::Invoking(inner) => format!("$$__{}", operand(inner)),
        Operand::Position(inner) => format!("$$_{}", operand(inner)),
        Operand::Driver(inner) => format!("$$0{}", operand(inner)),
        // An interpolated string reprints double-quoted, with its
        // escapes restored and each hole's expression unparsed.
        Operand::Interp(segs) => {
            let mut out = String::from("\"");
            for seg in segs {
                match seg {
                    InterpSeg::Text(t) => {
                        for c in t.chars() {
                            if matches!(c, '"' | '\\' | '$') {
                                out.push('\\');
                            }
                            out.push(c);
                        }
                    }
                    InterpSeg::Expr(e) => {
                        out.push_str("${");
                        out.push_str(&operand(e));
                        out.push('}');
                    }
                    InterpSeg::Strict(e, msg) => {
                        out.push_str("${");
                        out.push_str(&operand(e));
                        out.push_str(":?");
                        if let Some(m) = msg {
                            out.push_str(m);
                        }
                        out.push('}');
                    }
                    InterpSeg::Default(e, f) => {
                        out.push_str("${");
                        out.push_str(&operand(e));
                        out.push_str(":-");
                        out.push_str(&operand(f));
                        out.push('}');
                    }
                }
            }
            out.push('"');
            out
        }
        Operand::Ctx {
            index,
            steps,
            projection: p,
        } => {
            let mut out = match index {
                Some(k) => format!("$${k}"),
                None => "$*".to_string(),
            };
            for e in steps {
                out.push_str(&elem(e));
            }
            if let Some(p) = p {
                out.push_str(&projection(p));
            }
            out
        }
    }
}

/// An arithmetic sub-expression under `parent`: parenthesized
/// where the parse would group it otherwise — a looser operator
/// below a tighter one, or, on the right, the same precedence
/// (the parse is left-associative). A long left-leaning chain
/// `1 + 1 + 1 + …` therefore prints flat, within the nesting the
/// parser accepts.
fn arith_term(o: &Operand, parent: ArithOp, right: bool) -> String {
    let inner = match o {
        Operand::Arith { op, .. } => arith_precedence(*op),
        Operand::Concat { .. } => arith_precedence(ArithOp::Add),
        _ => return operand(o),
    };
    let outer = arith_precedence(parent);
    if inner < outer || (right && inner == outer) {
        parens(&operand(o))
    } else {
        operand(o)
    }
}

fn arith_precedence(op: ArithOp) -> u8 {
    match op {
        ArithOp::Add | ArithOp::Sub => 1,
        _ => 2,
    }
}

fn arith(op: ArithOp) -> &'static str {
    match op {
        ArithOp::Add => "+",
        ArithOp::Sub => "-",
        ArithOp::Mul => "*",
        ArithOp::Div => "div",
        ArithOp::IDiv => "idiv",
        ArithOp::Mod => "mod",
        ArithOp::MatMul => "x",
    }
}

fn literal(v: &Value) -> String {
    match v {
        // The one string-literal rule, shared with the display form.
        Value::Str(s) => crate::value::quarb_string(s),
        // Null displays as empty text; as a literal it must spell
        // its keyword or the round-trip drops it.
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn command(st: &Command) -> String {
    match st {
        // `s/pat/repl/mods` keeps its substitution spelling.
        Command::Func(call)
            if call.name == "s"
                && call.args.len() == 3
                && call.args.iter().all(|a| matches!(a, Arg::Lit(_))) =>
        {
            let lit = |a: &Arg| match a {
                Arg::Lit(v) => v.to_string(),
                _ => unreachable!("matched literals"),
            };
            // A literal `/` in the pattern or replacement must be
            // re-escaped as `\/`: the lexer unescaped `\/` to `/` when
            // it read the substitution, and a bare `/` would relex as a
            // section delimiter. The mods are `[a-z]` only — no escaping.
            format!(
                "| s/{}/{}/{}",
                lit(&call.args[0]).replace('/', "\\/"),
                lit(&call.args[1]).replace('/', "\\/"),
                lit(&call.args[2])
            )
        }
        // The sh command canonicalizes to its backtick sugar when it
        // has the sugarable shape (one literal or interpolated
        // command).
        Command::Func(call)
            if call.name == "sh"
                && call.args.len() == 1
                && matches!(
                    call.args[0],
                    Arg::Lit(Value::Str(_)) | Arg::Expr(Operand::Interp(_))
                ) =>
        {
            let mut out = String::from("| `");
            let escape = |out: &mut String, t: &str| {
                for c in t.chars() {
                    if matches!(c, '`' | '\\' | '$') {
                        out.push('\\');
                    }
                    out.push(c);
                }
            };
            match &call.args[0] {
                Arg::Lit(Value::Str(t)) => escape(&mut out, t),
                Arg::Expr(Operand::Interp(segs)) => {
                    for seg in segs {
                        match seg {
                            InterpSeg::Text(t) => escape(&mut out, t),
                            InterpSeg::Expr(e) => {
                                out.push_str("${");
                                out.push_str(&operand(e));
                                out.push('}');
                            }
                            InterpSeg::Strict(e, msg) => {
                                out.push_str("${");
                                out.push_str(&operand(e));
                                out.push_str(":?");
                                if let Some(m) = msg {
                                    out.push_str(m);
                                }
                                out.push('}');
                            }
                            InterpSeg::Default(e, f) => {
                                out.push_str("${");
                                out.push_str(&operand(e));
                                out.push_str(":-");
                                out.push_str(&operand(f));
                                out.push('}');
                            }
                        }
                    }
                }
                _ => unreachable!("guarded above"),
            }
            out.push('`');
            out
        }
        // `%(...)` is the record constructor's canonical spelling
        //; `rec` / `record` are its aliases.
        Command::Func(call) if is_record_call(&call.name) => {
            format!("| %{}", record_args(call, "; "))
        }
        // `| #(--width=3)` / `| #(--header=@(a; b; c))` — the
        // reshape, its option long, its header bare.
        Command::Func(call) if call.name == "#" && !call.args.is_empty() => {
            format!("| {}", reshape_text(call))
        }
        Command::Func(call) => format!("| {}", fn_call(call)),
        // `%%(...)`: the record sigil's enriched register view.
        Command::RecordWith(call) => format!("| %%{}", record_args(call, "; ")),
        // `.%(...)` / `.name%(...)`: the record push, sigil-spelled.
        Command::RecordPush {
            name,
            call,
            enriched,
        } => {
            let sigil = if *enriched { "%%" } else { "%" };
            format!(
                "| .{}{sigil}{}",
                name.as_deref().map(push_name).unwrap_or_default(),
                record_args(call, "; ")
            )
        }
        // `group(...)` follows the record convention: its keys print
        // as a record's do.
        Command::Agg(call) if call.name == "group" => {
            format!("@| group{}", record_args(call, "; "))
        }
        Command::Agg(call) if call.name == "#" && !call.args.is_empty() => {
            format!("@| {}", reshape_text(call))
        }
        Command::Agg(call) => format!("@| {}", fn_call(call)),
        Command::Spread { outer: false } => "| ...".to_string(),
        Command::Spread { outer: true } => "| ...?".to_string(),
        Command::Repeat {
            command: inner,
            min,
            max,
        } => {
            let quant = match max {
                Some(n) if n == min => format!("{{{min}}}"),
                Some(n) => format!("{{{min};{n}}}"),
                None => format!("{{{min};}}"),
            };
            format!("{}{quant}", command(inner))
        }
        Command::Map(inner) => {
            let body = command(inner);
            let body = body
                .strip_prefix("@| ")
                .or_else(|| body.strip_prefix("| "))
                .unwrap_or(&body)
                .to_string();
            format!("$| {body}")
        }
        Command::Nav(b) => format!("| {}", branch(b)),
        Command::Push(None) => "| .".to_string(),
        Command::Push(Some(n)) => format!("| .{}", push_name(n)),
        Command::FieldsPush => "| .%".to_string(),
        Command::Subcontext { name, body } => match name {
            Some(n) => format!("| .{}({})", push_name(n), unparse(body)),
            None => format!("| .({})", unparse(body)),
        },
        // Only some operand spellings are self-delimiting after `|`
        // (the starts pipe_item's expression arms accept: a paren,
        // a quoted string, a projection, the `$` family, `@-`);
        // everything else — `(3)`, `(now())`, `(@*)`, `(^/a)` —
        // must keep parens, or the reparse reads it as a command
        // name. A bare PATH must keep parens too: unparenthesized
        // it would reparse as a navigation command.
        // A literal stands bare after a pipe (draft 164): `| 0`,
        // never `| (0)`, which is the rounded register.
        Command::Expr(e) => {
            let text = operand(e);
            let delimited = text.starts_with(['(', '\'', '"', ':', ';', '$', '%', '_'])
                || text.starts_with("@-")
                || text.starts_with("@(")
                || matches!(e, Operand::Lit(_));
            if delimited {
                format!("| {text}")
            } else {
                format!("| ({text})")
            }
        }
        Command::ExprPush { name, expr } => match name {
            Some(n) => format!("| .{}({})", push_name(n), body_text(expr)),
            None => format!("| .({})", body_text(expr)),
        },
        Command::AggPush { name, body } => match name {
            Some(n) => format!("@| .{}({})", push_name(n), body_text(body)),
            None => format!("@| .({})", body_text(body)),
        },
        Command::AggRecord { call, enriched } => format!(
            "@| {}{}",
            if *enriched { "%%" } else { "%" },
            record_args(call, "; ")
        ),
        Command::AggExpr(e) => {
            let text = operand(e);
            let delimited = text.starts_with(['(', '\'', '"', '$', '%', '#'])
                || text.starts_with("@(")
                || text.starts_with("@%(")
                || matches!(e, Operand::Lit(_));
            if delimited {
                format!("@| {text}")
            } else {
                format!("@| ({text})")
            }
        }
        Command::AggRecall(r) => format!("@| {}", reg(r)),
        Command::Select(p) => format!("@| {}", predicate(p)),
        Command::Filter(e) => format!("| [{}]", pred_expr(e)),
        Command::Recall(r) => format!("| {}", reg(r)),
    }
}

fn is_record_call(name: &str) -> bool {
    matches!(name, "rec" | "record")
}

/// A key of the record convention prints bare when it is an
/// identifier (`[A-Za-z_][A-Za-z0-9_]*`, and not a boolean word),
/// quoted otherwise — the canonical display of a record.
/// A push name prints bare when it is an identifier, quoted
/// otherwise (`."3"`, a pivot's data-driven column).
fn push_name(n: &str) -> String {
    if n.chars().next().is_some_and(|c| c.is_alphabetic())
        && n.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        n.to_string()
    } else {
        crate::value::quarb_string(n)
    }
}

fn is_bare_key(k: &str) -> bool {
    // An identifier in any script (names are Unicode alphanumerics
    // and `_`), never a boolean word; and `.N`, the key of an
    // anonymous regula in `%%.`.
    if k.len() > 1 && k.starts_with('.') && k[1..].bytes().all(|c| c.is_ascii_digit()) {
        return true;
    }
    let mut cs = k.chars();
    matches!(cs.next(), Some(c) if c.is_alphabetic() || c == '_')
        && cs.all(|c| c.is_alphanumeric() || c == '_')
        && !crate::value::is_bool_word(k)
}

/// The record convention's argument list: a literal
/// string is a key and prints as `key = value` — bare when it is an
/// identifier, quoted otherwise — with `sep` between the items; an
/// auto-named value prints alone. The kaiv form, which the record
/// is emitted as.
fn record_args(call: &crate::syntax::ast::FnCall, sep: &str) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < call.args.len() {
        match &call.args[i] {
            Arg::Lit(Value::Str(k)) if i + 1 < call.args.len() => {
                let key = if is_bare_key(k) {
                    k.clone()
                } else {
                    literal(&Value::Str(k.clone()))
                };
                out.push(format!("{key} = {}", arg_text(&call.args[i + 1])));
                i += 2;
            }
            a => {
                out.push(grouped(arg_text(a)));
                i += 1;
            }
        }
    }
    format!("({})", out.join(sep))
}

/// A field's text, in the path group when it is dash-led (`-x`,
/// `--author`, `--`): the group keeps the operand reading, where the
/// bare field would open the option segment (draft 164). A negative
/// number is no dash-led word.
fn grouped(text: String) -> String {
    let head = text.split([' ', '=', '/', ':', '[']).next().unwrap_or("");
    if text.starts_with("--") || crate::syntax::lexer::is_option_word(head) {
        format!("({text})")
    } else {
        text
    }
}

/// A headless body pipeline's text: the commands with the first
/// one's pipe dropped (`cc`, `unique @| count`).
fn headless(commands: &[crate::syntax::ast::Command]) -> String {
    let tail: Vec<String> = commands.iter().map(command).collect();
    let text = tail.join(" ");
    text.trim_start_matches("| ").to_string()
}

/// An operand standing as a whole body — a push's, a record
/// field's, a list item's: a headless pipeline prints bare, since
/// the body's own parentheses delimit it (`.n(cc)`, not `.n((cc))`,
/// which the reparse would read as a path group).
pub(crate) fn body_text(op: &Operand) -> String {
    match op {
        Operand::Piped { expr, commands } if matches!(**expr, Operand::Seed) => headless(commands),
        _ => operand(op),
    }
}

fn arg_text(a: &Arg) -> String {
    match a {
        Arg::Lit(v) => literal(v),
        Arg::Expr(e) => body_text(e),
        Arg::Range(a, b) => format!(
            "{}..{}",
            a.map(|n| n.to_string()).unwrap_or_default(),
            b.map(|n| n.to_string()).unwrap_or_default()
        ),
        Arg::Named { name, value } => format!("{name} = {}", literal(value)),
        Arg::Pairs { keys, values } => format!("{} = {}", body_text(keys), body_text(values)),
        // An option's string value prints bare when the shell
        // spelling admits it (`--locale=ru`), quoted otherwise.
        Arg::Opt { name, value } => match value {
            Some(Operand::Lit(Value::Str(s)))
                if !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    && !s.starts_with(|c: char| c.is_ascii_digit() || c == '-')
                    && !matches!(s.as_str(), "true" | "false" | "null") =>
            {
                format!("{name}={s}")
            }
            Some(v) => format!("{name}={}", operand(v)),
            None => name.clone(),
        },
    }
}

/// [`fn_call`] for the parser's tests.
#[cfg(test)]
pub(crate) fn fn_call_text(call: &crate::syntax::ast::FnCall) -> String {
    fn_call(call)
}

fn fn_call(call: &crate::syntax::ast::FnCall) -> String {
    if call.args.is_empty() {
        return call.name.clone();
    }
    let args: Vec<String> = call
        .args
        .iter()
        .map(|a| match a {
            // `csv(";"; …)`: the delimiter the parser carried as
            // `delimiter=;` reads back as the quoted character.
            Arg::Named {
                name,
                value: Value::Str(s),
            } if name == "delimiter" => match s.as_str() {
                "\t" => "\"tab\"".to_string(),
                "\"" => "'\"'".to_string(),
                d => format!("\"{d}\""),
            },
            other => arg_text(other),
        })
        .collect();
    // The operands first, then the one option segment; a segment
    // whose first option would not open one (a dash before a
    // digit) opens with the void option.
    let (operands, options): (Vec<_>, Vec<_>) = args
        .into_iter()
        .zip(call.args.iter())
        .partition(|(_, a)| !matches!(a, Arg::Opt { .. }));
    // A hop as an operand prints as the path group, `f((--author))`
    // — the canonical spelling (draft 164), a dash-led field being
    // the option segment on the reparse; the trailing separator,
    // `f(--author;)`, is an input spelling only.
    let mut fields: Vec<String> = operands.into_iter().map(|(t, _)| grouped(t)).collect();
    if !options.is_empty() {
        let mut segment: Vec<String> = options.into_iter().map(|(t, _)| t).collect();
        let opens = segment[0] == "--"
            || segment[0].starts_with("--")
            || crate::syntax::lexer::is_option_word(segment[0].split('=').next().unwrap_or(""));
        if !opens {
            segment.insert(0, "-=".to_string());
        }
        fields.push(segment.join(" "));
    }
    format!("{}({})", call.name, fields.join("; "))
}

pub(crate) fn reg(r: &RegRef) -> String {
    match r {
        RegRef::Top => "$.".to_string(),
        RegRef::Index(n) => format!("$.{n}"),
        RegRef::Back(n) => format!("$.-{n}"),
        RegRef::Named(n) => format!("$.{n}"),
        RegRef::Whole => "@.".to_string(),
        RegRef::Record => "%.".to_string(),
        RegRef::FullRecord => "%.*".to_string(),
        RegRef::NodeWhole => "@@.".to_string(),
        RegRef::NodeRecord => "%%.".to_string(),
        RegRef::NodeFullRecord => "%%.*".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::unparse;

    /// Parse `q`, unparse it, and return the canonical text.
    fn rt(q: &str) -> String {
        let toks = crate::syntax::lexer::lex(q).expect("lex");
        let ast = crate::syntax::parser::parse(&toks).expect("parse");
        unparse(&ast)
    }

    /// Assert the unparse output reparses and is a fixpoint
    /// (`unparse(parse(text)) == unparse(parse(unparse(parse(text))))`),
    /// returning the canonical text for further assertions.
    fn assert_fixpoint(q: &str) -> String {
        let once = rt(q);
        assert_eq!(rt(&once), once, "unparse is !a fixpoint for {q:?}");
        once
    }

    // Finding: `:::` is the canonical adapter-metadata spelling; the
    // deprecated `::;` alias must parse to the same AST and reprint
    // as `:::`.
    #[test]
    fn connective_chains_print_as_written() {
        // Left-associative: no parentheses on a same-connective
        // chain, and none that would open `(:` — the rounded
        // sibling hop to the lexer.
        assert_eq!(
            assert_fixpoint("/a | %(fi = 1) | [:fi || :ss || :pp || :mp]"),
            "/a | %(fi = 1) | [:fi || :ss || :pp || :mp]"
        );
        assert_eq!(
            assert_fixpoint("/a | %(fi = 1) | [:pp && ( :fi || :ss)]"),
            "/a | %(fi = 1) | [:pp && ( :fi || :ss)]"
        );
        assert_fixpoint("/a | %(fi = 1) | [!( :fi || :ss)]");
    }

    #[test]
    fn calculator_round_trips() {
        for q in [
            "= @(1..5)",
            "= 0..8..2",
            "= @(1..(2 + 1))",
            "= @((1 + 1)..4..(1 + 0))",
            "= @(1..12) | #(--width=3)",
            "= @(1..6) | #(--header=@(a; b; c)) | :b",
            "= @(1..6) | #(%(1; 2); %(3; 4)) ++ %(5; 6)",
            "/row | 0 + $_ | \"x\" | %(v = $_) ++ %(w = 1) @| @(0; $_) @| (10 + $_)",
            "/row @| %(v = $_) ++ %(w = 1) | 1",
            "= @(1; @(2)) | flatten(1)",
            "= %(@(a; b) = @(1; 2))",
            "= %(id = 7; $.k = $.v) | keys",
            "/a | %($.k = $_) | values",
            "= 1..6 @| #(--width=2) | dim",
        ] {
            assert_fixpoint(q);
        }
    }

    #[test]
    fn structure_arithmetic_round_trips() {
        for q in [
            "= @(1; 2) * 2 | $_ * 3",
            "= #(%(1; 2)) x @(1; 1)",
            "/a | @(::x) | $_ + 1 * 2 | count",
            "= 5 | $_ mod 3",
        ] {
            assert_fixpoint(q);
        }
        // the operator after a pipe is the topic's expression
        assert_eq!(rt("= 1 | * 2"), "= 1 | $_ * 2");
        assert_eq!(rt("= 1 | + 2 * 3"), "= 1 | $_ + 2 * 3");
    }

    #[test]
    fn concatenation_round_trips() {
        for q in [
            "= @(1; 2) ++ 3",
            "/a | %(a = ::x) | ++! %(a = 2)",
            "= #(@(a; b); %(1; 2)) ++ %(3; b = 4)",
            "/a | @(::x) ++ ::y | count",
            "/a | @(::x) | ++ ::y | count",
            "= @(0) ++ (/rows/*::x)",
            "= @(1; 2) +@+ @(3; 4)",
            "= @(1) ++ (2 + 3)",
            "= @(1) ++ 2 * 3",
            "= (1 + 2) ++ 3",
        ] {
            assert_fixpoint(q);
        }
        // the concatenation sits at the additive level, left-associative
        assert_eq!(rt("= @(1) ++ (@(2) ++ 3)"), "= @(1) ++ (@(2) ++ 3)");
        assert_eq!(rt("= (@(1) ++ @(2)) ++ 3"), "= @(1) ++ @(2) ++ 3");
        assert_eq!(rt("= (@(1) ++ 2) * 3"), "= (@(1) ++ 2) * 3");
    }

    #[test]
    fn record_array_round_trips() {
        for q in [
            "= @%(@(a; b); %(1; 2); %(b = 3))",
            "/x | @%(@(name); %(::name))",
        ] {
            assert_fixpoint(q);
        }
    }

    #[test]
    fn label_selections_round_trip() {
        for q in [
            "/a->(ref && !doc)",
            "/a<-((ref || doc) && !x*)",
            "/a--(!(ref || doc))",
            "/a->(!*)",
            "(/a->(ref && doc)){2}",
            "/a->(ref|doc)",
        ] {
            assert_fixpoint(q);
        }
        // CNF puts the conjunction outermost; the disjunction of
        // two conjunctions distributes.
        assert_eq!(rt("/a->(!(ref || doc))"), "/a->(!ref && !doc)");
        assert_eq!(rt("/a->(ref .and. .not. doc)"), "/a->(ref && !doc)");
    }

    #[test]
    fn peer_reads_round_trip() {
        for q in [
            "//x | .v(::a) | <-link:.v",
            "//x | .v(::a) | (<-link:.v @| sum)",
            "/a | /*:.",
            "/a | /*:.2",
            "/a | \\/*:@.",
            "/a | \\/*:%.",
            "/a | \\/*:%%.",
            "/a | \\/*:%.:v",
            "/a | <-link:.v:%provenance:text",
            "/a | ((!3))::name",
            "/a | .v(1) | ((!$.v:%origin:id)):name",
            "/a | ((!@(2; 3)))/*::x",
            "/a | $.v:%provenance",
            "/a | $.v:@provenance",
        ] {
            // A path-headed expression command prints parenthesized,
            // as every such command does; the spelling inside is the
            // written one.
            let canon = assert_fixpoint(q);
            assert!(
                canon.contains(&q[q.rfind("| ").unwrap() + 2..]),
                "{q:?} -> {canon:?}"
            );
        }
        // The rounded twin prints pointy.
        assert_eq!(rt("/a | \\/*:(*.)"), rt("/a | \\/*:@."));
    }

    #[test]
    fn expression_heads_strings_and_chains_round_trip() {
        // `= 3` is not `| (3)`, which is the capture `$3`
        assert_eq!(rt("= 3"), "= 3");
        assert_eq!(rt("= 1 + 2 | $_ * 2"), "= 1 + 2 | $_ * 2");
        // the rounded hole's opener is escaped inside a string
        assert_eq!(rt("= 'a(,b'"), r#"= "a\(,b""#);
        assert_fixpoint(r#"= "a\(,b""#);
        // grouping prints only where the parse needs it
        assert_eq!(rt("= (1 + 2) * 3"), "= (1 + 2) * 3");
        assert_eq!(rt("= 1 + 2 * 3"), "= 1 + 2 * 3");
        assert_eq!(rt("= 1 - (2 - 3)"), "= 1 - (2 - 3)");
        assert_eq!(rt("= 1 - 2 - 3"), "= 1 - 2 - 3");
        assert_eq!(rt("= - (1 + 2)"), "= - (1 + 2)");
        let chain = format!("= {}", vec!["1"; 70].join(" + "));
        assert_eq!(rt(&chain), chain);
    }

    #[test]
    fn adapter_meta_alias_canonicalizes() {
        assert_eq!(rt("//*.txt:::size"), "//*.txt:::size");
        assert_eq!(rt("//*.txt:::size"), "//*.txt:::size");
        assert_eq!(rt("//*.txt:::size"), "//*.txt:::size");
        assert_eq!(
            rt("/commits/*[:::short = ^/tags/*:::short]"),
            "/commits/*[:::short = ^/tags/*:::short]"
        );
        // The def statement terminator still lexes as a single `;`.
        assert_eq!(rt("def &f: //x:::size; &f"), rt("def &f: //x:::size; &f"));
    }

    // Finding: a projection key named `and`/`or`/`not` must reprint
    // quoted, or the bare `::and` relexes as the default projection
    // followed by the connective keyword.
    #[test]
    fn projection_keyword_key_is_quoted() {
        assert_eq!(assert_fixpoint("/x::\"and\""), "/x::\"and\"");
        assert_eq!(assert_fixpoint("/x::\"or\""), "/x::\"or\"");
        assert_eq!(assert_fixpoint("/x:\"not\""), "/x:\"not\"");
        assert_eq!(assert_fixpoint("/x:::\"and\""), "/x:::\"and\"");
        // A near-miss name is not a keyword and stays bare.
        assert_eq!(assert_fixpoint("/x::android"), "/x::android");
    }

    // Finding: the double-quote fallback for a string holding a single
    // quote must escape `"`, `\`, and `$`.
    #[test]
    fn double_quote_fallback_escapes() {
        // Embedded double quotes.
        assert_eq!(
            assert_fixpoint("/x[::msg = \"it's \\\"fine\\\"\"]"),
            "/x[::msg = \"it's \\\"fine\\\"\"]"
        );
        // A `${…}` that must NOT become a live interpolation hole.
        assert_eq!(
            assert_fixpoint("/x[::msg = \"don't pay \\${fee}\"]"),
            "/x[::msg = \"don't pay \\${fee}\"]"
        );
        // A literal backslash beside the single quote.
        assert_eq!(
            assert_fixpoint("/x[::msg = \"it's a\\\\b\"]"),
            "/x[::msg = \"it's a\\\\b\"]"
        );
    }

    // Finding: a substitution pattern/replacement holding a literal
    // slash must reprint it escaped as `\/`.
    #[test]
    fn substitution_reescapes_slash() {
        assert_eq!(assert_fixpoint("/x | s/a\\/b/x/"), "/x | s/a\\/b/x/");
        assert_eq!(assert_fixpoint("/x | s/a/c\\/d/"), "/x | s/a/c\\/d/");
        // No slash in the parts — output is unchanged from before.
        assert_eq!(assert_fixpoint("/x | s/a/b/g"), "/x | s/a/b/g");
    }
}
