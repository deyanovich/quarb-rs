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
    Arg, ArithOp, Axis, Branch, CmpOp, Group, InterpSeg, Matcher, Operand, PathElem, PredExpr,
    Predicate, Projection, PushBody, Query, Reach, RegRef, Stage, Step,
};
use crate::value::Value;

/// Render `query` as canonical query text.
pub fn unparse(query: &Query) -> String {
    let mut out = String::new();
    for corr in &query.correlations {
        out.push_str(&unparse(corr));
        out.push_str(if corr.outer { " <=>? " } else { " <=> " });
    }
    out.push_str(&query_body(query));
    out
}

fn query_body(q: &Query) -> String {
    let mut parts: Vec<String> = q.branches.iter().map(branch).collect();
    // A trailing leaf anchor before a plain pipe would reparse as
    // the map pipe (`/a$ | f` relexes as `/a $| f` — the token
    // stream cannot spell them apart), so such a branch reprints
    // as a single-alternative group: `(/a$) | f`.
    if let (Some(last), Some(first)) = (q.branches.last(), q.pipeline.first())
        && branch_ends_bare_leaf(last)
        && stage(first).starts_with('|')
    {
        let text = parts.pop().expect("branches and parts zip");
        parts.push(format!("({text})"));
    }
    let mut out = parts.join(" || ");
    for stage_ast in &q.pipeline {
        out.push(' ');
        out.push_str(&stage(stage_ast));
    }
    out
}

/// Whether a branch's emitted text ends with the bare leaf anchor
/// `$` (a trailing leaf-anchored step with no projection after it).
fn branch_ends_bare_leaf(b: &Branch) -> bool {
    b.projection.is_none() && matches!(b.steps.last(), Some(PathElem::Step(s)) if s.leaf)
}

fn branch(b: &Branch) -> String {
    // The `^` anchor is semantic (navigate from the root, which
    // differs from the current node inside a subcontext body), so
    // it reprints whenever set.
    let mut out = String::new();
    if b.anchored {
        out.push('^');
    }
    if let Some(m) = &b.mark {
        out.push_str(&format!("({m})"));
    }
    for e in &b.steps {
        out.push_str(&elem(e));
    }
    if let Some(p) = &b.projection {
        out.push_str(&projection(p));
    }
    out
}

fn elem(e: &PathElem) -> String {
    match e {
        // A mark prints spaced for the same lexing reason as the
        // named push below.
        PathElem::Mark(name) => format!(" .{name} "),
        PathElem::Step(s) => step(s),
        PathElem::Group(g) => group(g),
        PathElem::Push { name, body } => {
            // A named push prints spaced: glued, `.name(` would lex
            // back into the preceding hop's name (the `/x.rs(...)`
            // rule). The bare `.(` re-lexes safely glued.
            let lead = if name.is_some() { " ." } else { "." };
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
        (m, Some(n)) => format!("{{{m},{n}}}"),
        (m, None) => format!("{{{m},}}"),
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
        Axis::Resolve { property, hint } => {
            out.push_str("::");
            out.push_str(&name_text(property));
            out.push_str("~>");
            if let Some(h) = hint {
                out.push_str(&name_text(h));
            }
            // A resolution step carries no matcher of its own.
            return out + &step_suffix(s);
        }
        Axis::ReverseResolve { property, hint } => {
            out.push_str("::");
            out.push_str(&name_text(property));
            out.push_str("<~");
            if let Some(h) = hint {
                out.push_str(&name_text(h));
            }
            return out + &step_suffix(s);
        }
    }
    let m = matcher(&s.matcher);
    // The lexer reads `<-3` as less-than-minus-three and `<-x` as
    // an incoming crosslink, so a digit-leading matcher after `<-`
    // (and a dash-leading one after `<`) needs a separating space
    // to keep its axis on reparse. Quoting would corrupt globs;
    // the space is meaning-preserving everywhere.
    let glue_breaks = match &s.axis {
        Axis::InLink => m.starts_with(|c: char| c.is_ascii_digit()),
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

fn matcher(m: &Matcher) -> String {
    match m {
        Matcher::Name(n) => name_text(n),
        Matcher::Glob(g) => g.glob().glob().to_string(),
        Matcher::Regex(r) => format!("~({})", r.as_str()),
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
            format!("'{n}'")
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
        format!("'{n}'")
    }
}

/// A projection key. Like [`name_text`], but also quotes the
/// predicate keywords `and`/`or`/`not`: `opt_projection_name` refuses
/// them unquoted (a bare `::and` is the default projection followed by
/// the connective keyword), so a field with one of those names must
/// reprint quoted (`::'and'`) to round-trip.
fn proj_name(k: &str) -> String {
    if matches!(k, "and" | "or" | "not") {
        format!("'{k}'")
    } else {
        name_text(k)
    }
}

fn projection(p: &Projection) -> String {
    match p {
        Projection::Property(None) => "::".to_string(),
        Projection::Property(Some(k)) => format!("::{}", proj_name(k)),
        Projection::CoreMeta(k) => format!(":::{}", proj_name(k)),
        Projection::AdapterMeta(k) => format!(";;;{}", proj_name(k)),
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
        PredExpr::Not(a) => format!("!{}", pred_term(a)),
        PredExpr::Compare(l, op, r) => {
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
        CmpOp::Match => "=~",
        CmpOp::NotMatch => "!~",
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
                    let Operand::Lit(pat) = test else {
                        unreachable!("regex arms hold their pattern literal");
                    };
                    out.push_str(&format!(" ~({pat})"));
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
            anchored,
            mark,
        } => {
            let mut out = String::new();
            if *anchored {
                out.push('^');
            }
            if let Some(m) = mark {
                out.push_str(&format!("({m})"));
            }
            out.extend(steps.iter().map(elem));
            if let Some(p) = p {
                out.push_str(&projection(p));
            }
            out
        }
        Operand::Lit(v) => literal(v),
        Operand::Arith { op, left, right } => {
            format!("{} {} {}", arith_term(left), arith(*op), arith_term(right))
        }
        Operand::Neg(inner) => format!("- {}", arith_term(inner)),
        Operand::Group(e) => format!("({})", pred_expr(e)),
        Operand::Recall(r) => reg(r),
        Operand::Topic => "$_".to_string(),
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
        Operand::Outer(inner) => format!("${}", operand(inner)),
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
                Some(k) => format!("$*{k}"),
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
        Value::Str(s) => {
            // Single quotes are verbatim; fall back to double when
            // the text itself holds one. A double-quoted string is
            // interpolated, so `"`, `\`, and `$` must be escaped — the
            // same escaping the `Operand::Interp` arm applies — or the
            // result fails to reparse (a bare `"` closes the string)
            // or silently grows a live `${…}` hole.
            if s.contains('\'') {
                let mut out = String::from("\"");
                for c in s.chars() {
                    if matches!(c, '"' | '\\' | '$') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
                out
            } else {
                format!("'{s}'")
            }
        }
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
                        }
                    }
                }
                _ => unreachable!("guarded above"),
            }
            out.push('`');
            out
        }
        Stage::Func(call) => format!("| {}", fn_call(call)),
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
        Stage::Push(None) => "| .".to_string(),
        Stage::Push(Some(n)) => format!("| .{n}"),
        Stage::Subcontext { name, body } => match name {
            Some(n) => format!("| .{n}({})", unparse(body)),
            None => format!("| .({})", unparse(body)),
        },
        // Only some operand spellings are self-delimiting after `|`
        // (the starts pipe_item's expression arms accept: a paren,
        // a quoted string, a path or projection, the `$` family,
        // `@-`); everything else — `(3)`, `(now())`, `(@*)`,
        // `(^/a)` — must keep parens, or the reparse reads it as a
        // function name.
        Stage::Expr(e) => {
            let text = operand(e);
            let delimited =
                text.starts_with(['(', '\'', '"', '/', ':', ';', '$']) || text.starts_with("@-");
            if delimited {
                format!("| {text}")
            } else {
                format!("| ({text})")
            }
        }
        Stage::ExprPush { name, expr } => match name {
            Some(n) => format!("| .{n}({})", operand(expr)),
            None => format!("| .({})", operand(expr)),
        },
        Stage::Select(p) => format!("@| {}", predicate(p)),
        Stage::Filter(e) => format!("| [{}]", pred_expr(e)),
        Stage::Recall(r) => format!("| {}", reg(r)),
    }
}

fn fn_call(call: &crate::ast::FnCall) -> String {
    if call.args.is_empty() {
        return call.name.clone();
    }
    let args: Vec<String> = call
        .args
        .iter()
        .map(|a| match a {
            Arg::Lit(v) => literal(v),
            Arg::Expr(e) => operand(e),
            Arg::Range(a, b) => format!(
                "{}..{}",
                a.map(|n| n.to_string()).unwrap_or_default(),
                b.map(|n| n.to_string()).unwrap_or_default()
            ),
        })
        .collect();
    format!("{}({})", call.name, args.join(", "))
}

fn reg(r: &RegRef) -> String {
    match r {
        RegRef::Top => "$.".to_string(),
        RegRef::Index(n) => format!("$.{n}"),
        RegRef::Named(n) => format!("$.{n}"),
        RegRef::Whole => "@.".to_string(),
        RegRef::Record => "%.".to_string(),
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
        assert_eq!(rt(&once), once, "unparse is not a fixpoint for {q:?}");
        once
    }

    // Finding: `;;;` is the canonical adapter-metadata spelling; the
    // deprecated `::;` alias must parse to the same AST and reprint
    // as `;;;`.
    #[test]
    fn adapter_meta_alias_canonicalizes() {
        assert_eq!(rt("//*.txt::;size"), "//*.txt;;;size");
        assert_eq!(rt("//*.txt;;;size"), "//*.txt;;;size");
        assert_eq!(
            rt("/commits/*[::;short = ^/tags/*::;short]"),
            "/commits/*[;;;short = ^/tags/*;;;short]"
        );
        // The def statement terminator still lexes as a single `;`.
        assert_eq!(rt("def &f: //x;;;size; &f"), rt("def &f: //x::;size; &f"));
    }

    // Finding: a projection key named `and`/`or`/`not` must reprint
    // quoted, or the bare `::and` relexes as the default projection
    // followed by the connective keyword.
    #[test]
    fn projection_keyword_key_is_quoted() {
        assert_eq!(assert_fixpoint("/x::'and'"), "/x::'and'");
        assert_eq!(assert_fixpoint("/x::'or'"), "/x::'or'");
        assert_eq!(assert_fixpoint("/x:::'not'"), "/x:::'not'");
        assert_eq!(assert_fixpoint("/x;;;'and'"), "/x;;;'and'");
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
