//! The unparser: render a parsed (and expanded) [`Query`] back to
//! canonical query text.
//!
//! This is the synthesis half of reflection — the parser reads
//! queries into arbors, the unparser writes them back out — and the
//! engine of `qua --expand` (LISP's *macroexpand*: show the query
//! the fragments wrote). The output round-trips: parsing it yields a
//! query with the same meaning, and unparsing is a fixpoint
//! (`unparse(parse(unparse(q))) == unparse(q)`).

use crate::ast::{
    Arg, ArithOp, Axis, Branch, CmpOp, Group, InterpSeg, Matcher, Operand, PatSeg, PathElem,
    PredExpr,
    Predicate, Projection, PushBody, Query, Reach, RegRef, Stage, Step,
};
use crate::value::Value;

/// Render `query` as canonical query text. Driver-first: the
/// query's own branches lead, each joined expression follows its
/// `<=>` (the outer marker glued to the operator), and the driving
/// pipeline closes the text.
pub fn unparse(query: &Query) -> String {
    let n = query.correlations.len();
    // The join prints where it binds: the driver's stages before
    // `join_at` precede the `<=>` entries, the rest follow them.
    let at = if n == 0 { 0 } else { query.join_at.min(query.pipeline.len()) };
    let (pre, post) = query.pipeline.split_at(at);
    let main_pipes = post.first().is_some_and(|s| stage(s).starts_with('|'));
    let pre_pipes = pre.first().is_some_and(|s| stage(s).starts_with('|'));
    let mut out = branches_join(query, if at > 0 { pre_pipes } else { n == 0 && main_pipes });
    for stage_ast in pre {
        out.push(' ');
        out.push_str(&stage(stage_ast));
    }
    for (i, corr) in query.correlations.iter().enumerate() {
        out.push_str(if corr.outer { " <=>? " } else { " <=> " });
        let followed_by_main_pipe = (i + 1 == n) && main_pipes;
        let own_pipes = corr
            .pipeline
            .first()
            .is_some_and(|s| stage(s).starts_with('|'));
        out.push_str(&branches_join(corr, followed_by_main_pipe || own_pipes));
        // A non-final entry may retain its own pre-join pipeline.
        for stage_ast in &corr.pipeline {
            out.push(' ');
            out.push_str(&stage(stage_ast));
        }
    }
    for stage_ast in post {
        out.push(' ');
        out.push_str(&stage(stage_ast));
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
    out.push_str(&anchor(&b.anchor));
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

/// An anchor's canonical spelling; empty for the default.
fn anchor(a: &crate::ast::Anchor) -> String {
    use crate::ast::Anchor;
    match a {
        Anchor::Current => String::new(),
        Anchor::Root => "^".to_string(),
        // Double parentheses are the node side (ruling #43).
        // `$$.name` / `$$.N` / `$$.` — the marks, node-side; the
        // rounded spellings are the double-paren anchors.
        Anchor::Mark(m) => format!("$$.{m}"),
        Anchor::MarkIndex(n) => format!("$$.{n}"),
        Anchor::MarkTop => "$$.".to_string(),
        Anchor::MarksAll => "((@))".to_string(),
        Anchor::MarksNamed(m) => format!("((@{m}))"),
    }
}

fn elem(e: &PathElem) -> String {
    match e {
        // A mark prints spaced for the same lexing reason as the
        // named push below; the anonymous mark is a lone dot.
        PathElem::Mark(Some(name)) => format!(" .{} ", push_name(name)),
        PathElem::Mark(None) => " . ".to_string(),
        PathElem::Step(s) => step(s),
        PathElem::Group(g) => group(g),
        PathElem::Push { name, body } => {
            // A push prints spaced: the push dot stands after
            // whitespace (ruling #45); glued, it would be a name
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
fn group(g: &Group) -> String {
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
        "({}){}{}{}",
        alts.join("|"),
        quant,
        preds,
        reach_mark(&g.reach)
    )
}

fn step(s: &Step) -> String {
    let mut out = String::new();
    match &s.axis {
        Axis::Child => out.push('/'),
        Axis::Descendant(Reach::All) => out.push_str("//"),
        Axis::Descendant(Reach::Proximal) => out.push_str("//?"),
        Axis::Descendant(Reach::Distal) => out.push_str("//!"),
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
        Axis::Resolve { property, hint } => {
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
        Axis::ReverseResolve { property, hint } => {
            if let Some(p) = property {
                out.push_str("::");
                out.push_str(&name_text(p));
            }
            out.push_str("<--");
            if let Some(h) = hint {
                out.push_str(&name_text(h));
            }
            return out + &step_suffix(s);
        }
    }
    // The parent and the ancestors take no name: the wildcard is
    // implied, and the bare axis is the canonical spelling.
    let m = if matches!(s.axis, Axis::Parent | Axis::Ancestor(_)) && matches!(s.matcher, Matcher::Any) {
        String::new()
    } else {
        matcher(&s.matcher)
    };
    // The lexer reads `<-3` as less-than-minus-three and `<-x` as
    // an incoming crosslink, so a digit-leading matcher after `<-`
    // (and a dash-leading one after `<`) needs a separating space
    // to keep its axis on reparse. Quoting would corrupt globs;
    // the space is meaning-preserving everywhere.
    let glue_breaks = match &s.axis {
        Axis::InLink => m.starts_with(|c: char| c.is_ascii_digit()),
        // `--` then a dash-leading matcher would fuse into a longer
        // dash run on reparse.
        Axis::BothLink => m.starts_with('-'),
        Axis::PrevSibling => m.starts_with('-'),
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

fn step_suffix(s: &Step) -> String {
    let mut out = String::new();
    if !s.traits.is_empty() {
        // One bracket, CNF-shaped: clauses joined by `&&`, each a
        // `||`-disjunction, parenthesized when both dimensions are
        // in play.
        let many = s.traits.len() > 1;
        let clauses: Vec<String> = s
            .traits
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
    for p in &s.predicates {
        out.push_str(&predicate(p));
    }
    if s.leaf {
        out.push('$');
    }
    out
}

/// The canonical regex literal (ruling #44): `(/body/mods)`. In
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

fn matcher(m: &Matcher) -> String {
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
    let quote = |n: &str| {
        let bare = !n.is_empty()
            && n.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | '+'));
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

fn projection(p: &Projection) -> String {
    match p {
        Projection::Property(None) => "::".to_string(),
        Projection::Property(Some(k)) => format!("::{}", proj_name(k)),
        Projection::CoreMeta(k) => format!(":::{}", proj_name(k)),
        Projection::AdapterMeta(k) => format!("::::{}", proj_name(k)),
    }
}

fn predicate(p: &Predicate) -> String {
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

fn pred_expr(e: &PredExpr) -> String {
    match e {
        PredExpr::Or(a, b) => format!("{} || {}", pred_term(a), pred_term(b)),
        PredExpr::And(a, b) => format!("{} && {}", pred_term(a), pred_term(b)),
        // `!` binds tightest: a negated comparison or connective
        // prints with its parentheses so the text re-parses as
        // written (`.not. ::x > 1` is `!(::x > 1)`).
        PredExpr::Not(a) => match a.as_ref() {
            PredExpr::Truthy(_) | PredExpr::Not(_) => format!("!{}", pred_expr(a)),
            _ => format!("!({})", pred_expr(a)),
        },
        PredExpr::Compare(l, op, r) => {
            // Ruling #33: the canonical spelling of a literal
            // substring test is the pattern form — `*= "x"` prints
            // as `== *"x"*`. A dynamic right operand keeps `*=`
            // (patterns are literal syntax only).
            if matches!(op, CmpOp::Contains)
                && let Operand::Lit(Value::Str(t)) = r
            {
                let pat = Operand::Pattern(vec![
                    PatSeg::Star,
                    PatSeg::Lit(t.clone()),
                    PatSeg::Star,
                ]);
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
        PredExpr::Truthy(o) => operand(o),
    }
}

/// A sub-expression of a connective: parenthesized when it is itself
/// a binary connective, so precedence survives the round-trip.
/// `!` binds tightest, so a negation never needs wrapping.
fn pred_term(e: &PredExpr) -> String {
    match e {
        PredExpr::Or(..) | PredExpr::And(..) => {
            format!("({})", pred_expr(e))
        }
        _ => pred_expr(e),
    }
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
    }
}

/// The canonical text of one operand form (macro argument splicing
/// and the expansion arbor's `::form` property).
pub(crate) fn operand_text(o: &Operand) -> String {
    operand(o)
}

fn operand(o: &Operand) -> String {
    match o {
        Operand::Match {
            scrutinee,
            arms,
            other,
        } => {
            let mut out = format!("({} ?=", operand(scrutinee));
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
            out.push_str(&anchor(a));
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
            format!("{} {} {}", arith_term(left), arith(*op), arith_term(right))
        }
        Operand::Neg(inner) => format!("- {}", arith_term(inner)),
        Operand::Group(e) => format!("({})", pred_expr(e)),
        Operand::Recall(r) => reg(r),
        Operand::Topic => "$_".to_string(),
        Operand::Field { base, name } => match **base {
            Operand::Topic => format!(":{name}"),
            _ => format!("{}:{name}", operand(base)),
        },
        Operand::NamedCaptures => "%+".to_string(),
        Operand::List(items) => {
            let inner: Vec<String> = items.iter().map(operand).collect();
            format!("@({})", inner.join("; "))
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
        Operand::Capsae { projection: p } => match p {
            Some(p) => format!("@*{}", projection(p)),
            None => "@*".to_string(),
        },
        Operand::Piped { expr, stages } => {
            let tail: Vec<String> = stages.iter().map(stage).collect();
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
        // `_` — the served node, projected or navigated.
        Operand::Outer(inner) => format!("_{}", operand(inner)),
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

/// An arithmetic sub-expression: parenthesized when it is itself
/// arithmetic, so grouping survives the round-trip regardless of
/// precedence.
fn arith_term(o: &Operand) -> String {
    match o {
        Operand::Arith { .. } => format!("({})", operand(o)),
        _ => operand(o),
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

fn stage(st: &Stage) -> String {
    match st {
        // `s/pat/repl/mods` keeps its substitution spelling.
        Stage::Func(call)
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
        // The sh stage canonicalizes to its backtick sugar when it
        // has the sugarable shape (one literal or interpolated
        // command).
        Stage::Func(call)
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
        // (ruling #38); `rec` / `record` are its aliases.
        Stage::Func(call) if is_record_call(&call.name) => format!("| %{}", record_args(call, "; ")),
        Stage::Func(call) => format!("| {}", fn_call(call)),
        // `%%(...)`: the record sigil's enriched register view.
        Stage::RecordWith(call) => format!("| %%{}", record_args(call, "; ")),
        // `.%(...)` / `.name%(...)`: the record push, sigil-spelled.
        Stage::RecordPush {
            name,
            call,
            enriched,
        } => {
            let sigil = if *enriched { "%%" } else { "%" };
            format!("| .{}{sigil}{}", name.as_deref().map(push_name).unwrap_or_default(), record_args(call, "; "))
        }
        // `group(...)` follows the record convention: its keys print
        // as a record's do.
        Stage::Agg(call) if call.name == "group" => format!("@| group{}", record_args(call, "; ")),
        Stage::Agg(call) => format!("@| {}", fn_call(call)),
        Stage::Spread { outer: false } => "| ...".to_string(),
        Stage::Spread { outer: true } => "| ...?".to_string(),
        Stage::Map(inner) => {
            let body = stage(inner);
            let body = body
                .strip_prefix("@| ")
                .or_else(|| body.strip_prefix("| "))
                .unwrap_or(&body)
                .to_string();
            format!("$| {body}")
        }
        Stage::Nav(b) => format!("| {}", branch(b)),
        Stage::Push(None) => "| .".to_string(),
        Stage::Push(Some(n)) => format!("| .{}", push_name(n)),
        Stage::FieldsPush => "| .%".to_string(),
        Stage::Subcontext { name, body } => match name {
            Some(n) => format!("| .{}({})", push_name(n), unparse(body)),
            None => format!("| .({})", unparse(body)),
        },
        // Only some operand spellings are self-delimiting after `|`
        // (the starts pipe_item's expression arms accept: a paren,
        // a quoted string, a projection, the `$` family, `@-`);
        // everything else — `(3)`, `(now())`, `(@*)`, `(^/a)` —
        // must keep parens, or the reparse reads it as a function
        // name. A bare PATH must keep parens too: unparenthesized
        // it would reparse as a navigation stage.
        Stage::Expr(e) => {
            let text = operand(e);
            let delimited = text.starts_with(['(', '\'', '"', ':', ';', '$', '%', '_'])
                || text.starts_with("@-")
                || text.starts_with("@(");
            if delimited {
                format!("| {text}")
            } else {
                format!("| ({text})")
            }
        }
        Stage::ExprPush { name, expr } => match name {
            Some(n) => format!("| .{}({})", push_name(n), operand(expr)),
            None => format!("| .({})", operand(expr)),
        },
        Stage::Select(p) => format!("@| {}", predicate(p)),
        Stage::Filter(e) => format!("| [{}]", pred_expr(e)),
        Stage::Recall(r) => format!("| {}", reg(r)),
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
        && n.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
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
        && !crate::parser::is_bool_word(k)
}

/// The record convention's argument list (ruling #50): a literal
/// string is a key and prints as `key = value` — bare when it is an
/// identifier, quoted otherwise — with `sep` between the items; an
/// auto-named value prints alone. The kaiv form, which the record
/// is emitted as.
fn record_args(call: &crate::ast::FnCall, sep: &str) -> String {
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
                out.push(arg_text(a));
                i += 1;
            }
        }
    }
    format!("({})", out.join(sep))
}

fn arg_text(a: &Arg) -> String {
    match a {
        Arg::Lit(v) => literal(v),
        Arg::Expr(e) => operand(e),
        Arg::Range(a, b) => format!(
            "{}..{}",
            a.map(|n| n.to_string()).unwrap_or_default(),
            b.map(|n| n.to_string()).unwrap_or_default()
        ),
    }
}

fn fn_call(call: &crate::ast::FnCall) -> String {
    if call.args.is_empty() {
        return call.name.clone();
    }
    let args: Vec<String> = call.args.iter().map(arg_text).collect();
    format!("{}({})", call.name, args.join("; "))
}

fn reg(r: &RegRef) -> String {
    match r {
        RegRef::Top => "$.".to_string(),
        RegRef::Index(n) => format!("$.{n}"),
        RegRef::Named(n) => format!("$.{n}"),
        RegRef::Whole => "@.".to_string(),
        RegRef::Record => "%.".to_string(),
        RegRef::FullRecord => "%%.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::unparse;

    /// Parse `q`, unparse it, and return the canonical text.
    fn rt(q: &str) -> String {
        let toks = crate::lexer::lex(q).expect("lex");
        let ast = crate::parser::parse(&toks).expect("parse");
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

    // Finding: `::::` is the canonical adapter-metadata spelling; the
    // deprecated `::;` alias must parse to the same AST and reprint
    // as `::::`.
    #[test]
    fn adapter_meta_alias_canonicalizes() {
        assert_eq!(rt("//*.txt::::size"), "//*.txt::::size");
        assert_eq!(rt("//*.txt::::size"), "//*.txt::::size");
        assert_eq!(rt("//*.txt::::size"), "//*.txt::::size");
        assert_eq!(
            rt("/commits/*[::::short = ^/tags/*::::short]"),
            "/commits/*[::::short = ^/tags/*::::short]"
        );
        // The def statement terminator still lexes as a single `;`.
        assert_eq!(rt("def &f: //x::::size; &f"), rt("def &f: //x::::size; &f"));
    }

    // Finding: a projection key named `and`/`or`/`not` must reprint
    // quoted, or the bare `::and` relexes as the default projection
    // followed by the connective keyword.
    #[test]
    fn projection_keyword_key_is_quoted() {
        assert_eq!(assert_fixpoint("/x::\"and\""), "/x::\"and\"");
        assert_eq!(assert_fixpoint("/x::\"or\""), "/x::\"or\"");
        assert_eq!(assert_fixpoint("/x:::\"not\""), "/x:::\"not\"");
        assert_eq!(assert_fixpoint("/x::::\"and\""), "/x::::\"and\"");
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
