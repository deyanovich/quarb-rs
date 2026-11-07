//! The model-file grammar: a definitions file
//! (Section~\ref{sec:fragments}) extended with `node`, `ref`,
//! `rel`, `edge`, and `mount` statements. Statements terminate with `;`;
//! `#` line comments and `def`/`macro` statements are collected as
//! defs text (prepended to every constructor query so a model's own
//! fragments are visible to its `node` queries).

/// One parsed model file.
#[derive(Debug, Default, Clone)]
pub struct Model {
    /// `mount NAME: target;` — sources the model opens itself.
    pub mounts: Vec<Mount>,
    /// `node /container/role: query;` — derived containers.
    pub nodes: Vec<NodeDecl>,
    /// `ref PATH::field --> container;` — scoped references.
    pub refs: Vec<RefDecl>,
    /// `rel SOURCE -> TARGET[cond];` — node-to-node relations.
    pub rels: Vec<RelDecl>,
    /// `edge PATH: ::a -- ::b;` — pair edges between containers.
    pub edges: Vec<EdgeDecl>,
    /// `def`/`macro` statements, verbatim, joined with `;` — seeded
    /// into every constructor query's fragment table.
    pub defs_text: String,
    /// `alias` statements: other spellings a node's name or trait
    /// answers to (an alias is a way in, never a way
    /// out; locators and reflection keep the canonical spelling).
    pub aliases: Vec<AliasDecl>,
    /// `trait <name>: STEP;` — declared traits: a node carries
    /// `name` exactly when it would match STEP as a step of a path,
    /// decided at the node. In declaration order; a declaration may
    /// use the traits declared before it.
    pub traits: Vec<TraitDecl>,
    /// `index SCOPE::prop;` — properties the model keeps an index
    /// of over the scoped nodes, for the searches by value (`=>`,
    /// `<--(SCOPE::prop)`) whose scope is the declared one. Cost
    /// only: the answer is the walk's, with or without it.
    pub indexes: Vec<IndexDecl>,
    /// `graph SCOPE(hop) { measure; … };` — the measures of the
    /// graph library the source maintains over the whole graph of
    /// the hop on the scope (spec: Graph Declarations). Cost only:
    /// a library command is answered from it when its hop, options
    /// and context are the declared ones.
    pub graphs: Vec<GraphDecl>,
    /// `weight-potential SCOPE-PATH::prop;` — the properties that are
    /// potentials: an edge within the scope weighs their difference
    /// (spec: Weight Potentials).
    pub potentials: Vec<PotentialDecl>,
    /// `tree IDENTITY: PATH;` — the string other trees refer to this
    /// one by, and the path (a mount root or a node inside one) the
    /// tree stands at. An identity ending in a separator is a base.
    pub trees: Vec<TreeDecl>,
    /// `unit NAME[, ALIAS...] = FACTOR EXPRESSION;` — a unit added
    /// to the unit table the quantity coercion consults, for every
    /// source the model mounts (spec: Unit Libraries): one of the
    /// unit is FACTOR of EXPRESSION, the expression naming built-in
    /// units, units declared earlier, or opaque units. The first
    /// name is the primary spelling, the rest aliases. Also what
    /// `units PATH;` imports: a file of unit declarations only.
    pub units: Vec<UnitDecl>,
    /// The unit files imported (`units PATH;`), each once.
    pub unit_files: Vec<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnitDecl {
    /// The spellings, the first primary.
    pub names: Vec<String>,
    /// The factor, exact where a decimal would round (`1/40`).
    pub factor: f64,
    /// The unit expression the factor scales.
    pub expr: String,
    /// The declaration as written, for messages.
    pub stmt: String,
}

/// One item of a `weights(…)` clause: the property, and under the
/// typed form `::km<_km>` the unit a bare number in it is read in
/// (spec: Weights, the typed clause; the unit spelled with its
/// underscore as a quantity writes it, ruling 2026-11-07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeightSpec {
    /// The property name, `::` stripped.
    pub prop: String,
    /// The unit of a bare number, as written between `<_` and `>`.
    pub unit: Option<String>,
}

/// A declared trait: `trait <heading>: (h1|h2|h3);`.
#[derive(Debug, Clone)]
pub struct TreeDecl {
    pub identity: String,
    pub path: String,
}

/// A declared graph: `graph SCOPE(hop) { measure; measure(options); };`
/// (spec: Graph Declarations) — the source maintains the named
/// measures of the graph library over the whole graph of the hop on
/// the scope, each as a query writes it.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphDecl {
    /// The node-selecting path.
    pub scope: String,
    /// The hop as written (`->link`, `--*`).
    pub hop: String,
    /// Each measure as written (`pagerank`, `betweenness(--normalized)`).
    pub measures: Vec<String>,
    /// The declaration as written, for messages.
    pub stmt: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexDecl {
    /// The node-selecting path (the `::prop` suffix stripped).
    pub scope: String,
    pub property: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitDecl {
    pub name: String,
    /// The step body, as written — a name, a pattern, an
    /// alternation, a trait expression, predicates, in any
    /// combination, without the navigation operator.
    pub step: String,
}

/// What an alias applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasKind {
    /// A node's name: `alias page document;`, `alias s/\.html$//;`.
    Name,
    /// A node's traits: `alias <chunk> <block>;`,
    /// `alias <s/^tag://i>;`.
    Trait,
}

/// How a written spelling is matched against the canonical one.
#[derive(Debug, Clone)]
pub enum AliasRule {
    /// The written `alias` stands for the canonical `original`.
    Exact { alias: String, original: String },
    /// A Perl-style substitution `s/pattern/replacement/flags` applied
    /// to each canonical spelling: the node also answers to what it
    /// produces. `i` compares case-insensitively; `g` replaces every
    /// match rather than the first.
    Subst {
        pattern: regex::Regex,
        replacement: String,
        ignore_case: bool,
        global: bool,
    },
}

#[derive(Debug, Clone)]
pub struct AliasDecl {
    pub kind: AliasKind,
    pub rule: AliasRule,
}

impl AliasDecl {
    /// Whether `written` reaches a node whose canonical spelling is
    /// `canonical` under this alias.
    pub fn admits(&self, written: &str, canonical: &str) -> bool {
        match &self.rule {
            AliasRule::Exact { alias, original } => written == alias && canonical == original,
            AliasRule::Subst {
                pattern,
                replacement,
                ignore_case,
                global,
            } => {
                // Perl's rule: a substitution that matched nothing
                // substituted nothing, and admits nothing.
                if !pattern.is_match(canonical) {
                    return false;
                }
                let produced = if *global {
                    pattern.replace_all(canonical, replacement.as_str())
                } else {
                    pattern.replace(canonical, replacement.as_str())
                };
                if *ignore_case {
                    produced.eq_ignore_ascii_case(written)
                        || produced.to_lowercase() == written.to_lowercase()
                } else {
                    produced == written
                }
            }
        }
    }
}

/// Parse one alias body: `NEW ORIGINAL`, `s/pat/rep/flags`, or the
/// same two forms inside angle brackets for traits.
/// `trait <NAME>: STEP` — a plain name in angle brackets (no
/// period prefix: a declared trait is arbor structure, named like
/// the adapter's own), then the body of one path step.
fn parse_trait(body: &str, stmt: &str) -> Result<TraitDecl, String> {
    let body = body.trim();
    let Some(rest) = body.strip_prefix('<') else {
        return Err(format!(
            "trait needs '<name>: STEP' (the name in angle brackets): '{stmt}'"
        ));
    };
    let Some((name, step)) = rest.split_once('>') else {
        return Err(format!("trait <name> is not closed: '{stmt}'"));
    };
    let name = name.trim();
    if let Some(bare) = name.strip_prefix('.') {
        return Err(format!(
            "a declared trait has a plain name — write 'trait <{bare}>', not '<.{bare}>': '{stmt}'"
        ));
    }
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'));
    if !plain {
        return Err(format!(
            "trait name '<{name}>' is not a plain name: '{stmt}'"
        ));
    }
    let Some(step) = step.trim().strip_prefix(':') else {
        return Err(format!(
            "trait <{name}> needs ': STEP' after the name: '{stmt}'"
        ));
    };
    let step = step.trim();
    if step.is_empty() {
        return Err(format!("trait <{name}> needs a step body: '{stmt}'"));
    }
    quarb::validate_step_body(step).map_err(|e| format!("trait <{name}>: {e}"))?;
    Ok(TraitDecl {
        name: name.to_string(),
        step: step.to_string(),
    })
}

fn parse_alias(body: &str, stmt: &str) -> Result<AliasDecl, String> {
    let body = body.trim();
    let (kind, body) = match body.strip_prefix('<') {
        Some(inner) => {
            let inner = inner.trim_end();
            let inner = inner
                .strip_suffix('>')
                .ok_or_else(|| format!("a trait alias closes its angle bracket: '{stmt}'"))?;
            // `<chunk> <block>` — two bracketed spellings.
            let inner = inner.trim();
            let inner = match inner.split_once("> <") {
                Some((a, b)) => format!("{} {}", a.trim(), b.trim()),
                None => inner.to_string(),
            };
            (AliasKind::Trait, inner)
        }
        None => (AliasKind::Name, body.to_string()),
    };
    let body = body.trim();
    if let Some(rest) = body.strip_prefix("s/") {
        // s/pattern/replacement/flags, `\/` escaping a slash.
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek() == Some(&'/') {
                cur.push('/');
                chars.next();
            } else if c == '/' {
                parts.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        parts.push(cur);
        if parts.len() != 3 {
            return Err(format!(
                "a substitution alias is 's/pattern/replacement/flags': '{stmt}'"
            ));
        }
        let flags = parts[2].trim();
        for f in flags.chars() {
            if !matches!(f, 'i' | 'g') {
                return Err(format!("unknown substitution flag '{f}' (i, g): '{stmt}'"));
            }
        }
        let pattern = regex::Regex::new(&parts[0])
            .map_err(|e| format!("alias pattern does not parse: {e}: '{stmt}'"))?;
        return Ok(AliasDecl {
            kind,
            rule: AliasRule::Subst {
                pattern,
                replacement: parts[1].clone(),
                ignore_case: flags.contains('i'),
                global: flags.contains('g'),
            },
        });
    }
    let (alias, original) = body.split_once(char::is_whitespace).ok_or_else(|| {
        format!("alias needs 'NEW ORIGINAL' or 's/pattern/replacement/': '{stmt}'")
    })?;
    let strip = |t: &str| {
        t.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string()
    };
    Ok(AliasDecl {
        kind,
        rule: AliasRule::Exact {
            alias: strip(alias),
            original: strip(original),
        },
    })
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub name: String,
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct NodeDecl {
    /// The container's name: `ips` from `/ips/ip`.
    pub name: String,
    /// The role each child plays in it: `ip` from `/ips/ip`. A hop
    /// into this container is labelled by this role — a label names
    /// what it lands on, never the property it came from.
    pub role: String,
    /// An explicit trait, `<addr>` in `node /ips/ip<addr>: …`;
    /// defaults to the role.
    pub trait_name: String,
    pub query: String,
    /// How many `trait` declarations precede this constructor in
    /// the file: the traits its query sees (a constructor sees the
    /// traits declared before it; one declared later is refused at
    /// load — spec: Node Constructors, 2026-11-06).
    pub traits_before: usize,
}

#[derive(Debug, Clone)]
pub struct RefDecl {
    /// The node-selecting path (the `::field` suffix stripped).
    pub scope: String,
    /// `acyclic`: the promise that no walk over the link returns to
    /// a node (spec: Model Files, Acyclic links).
    pub acyclic: bool,
    /// `weights(::a; ::b)`: the quantity-valued properties of the
    /// scoped nodes that are the edge's weights (spec: the edge
    /// model, Weights) — pairwise distinct dimensions, none
    /// dimensionless, checked at load.
    pub weights: Vec<WeightSpec>,
    /// The property resolved into the container.
    pub field: String,
    pub container: String,
    /// The target path as written, condition stripped: when no
    /// derived container answers to the first segment, the target
    /// is this path over the base and derived view (`/кузнецова/*`).
    pub path: String,
    /// The role within the container the path form names
    /// (`/tables/class`), which restricts resolution to that role's
    /// members; `None` for the bare form (`tables`), which resolves
    /// anywhere in the container.
    pub role: Option<String>,
    /// The target property the value is matched against, from the
    /// explicit form `--> /cookies/cookie[::id = $]`. `None` is the
    /// short form, matching the target's default projection.
    pub key_field: Option<String>,
    /// `as NAME`: the edge's own label, both ways (`->NAME`,
    /// `<-NAME`); `as ::prop` each row's edge labelled by that
    /// property's value; without either the role the hop arrives at.
    /// On a property the source already declares (a catalog's foreign
    /// key) it renames that one edge.
    pub label: Option<Label>,
}

/// `rel SOURCE -> TARGET[cond];` — a relation between two node sets
/// that no property carries: the condition is evaluated per pair,
/// with `$$` standing for the source node.
#[derive(Debug, Clone)]
pub struct RelDecl {
    pub source: String,
    /// The target path, without its condition.
    pub target: String,
    /// The condition, brackets included: `[::at > $$::at]`.
    pub cond: String,
}

/// The label a ref or edge statement gives the edges it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    /// `as NAME`: one label for every edge of the statement.
    Name(String),
    /// `as ::prop`: each edge labelled by its row's value of the
    /// property, a text (spec: Model Files, labels by property). A
    /// row whose value is null or missing declares no edge under the
    /// statement; a value that is no text refuses at load.
    Property(String),
}

impl Label {
    fn parse(text: &str) -> Label {
        match text.strip_prefix("::") {
            Some(p) => Label::Property(p.trim().to_string()),
            None => Label::Name(text.to_string()),
        }
    }

    /// The label as the statement wrote it.
    pub fn written(&self) -> String {
        match self {
            Label::Name(n) => n.clone(),
            Label::Property(p) => format!("::{p}"),
        }
    }
}

/// Split an edge statement's rest at the colon that ends its scope:
/// the first single colon at bracket depth zero outside quotes, so a
/// predicate's `::prop` and its string literals pass over.
fn split_scope_colon(rest: &str) -> Option<(&str, &str)> {
    let b = rest.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth = depth.saturating_sub(1),
                b':' if depth == 0 => {
                    if b.get(i + 1) == Some(&b':') {
                        i += 2;
                        continue;
                    }
                    return Some((&rest[..i], &rest[i + 1..]));
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

#[derive(Debug, Clone)]
pub struct EdgeDecl {
    pub scope: String,
    pub field_a: String,
    pub field_b: String,
    /// `as NAME`: the edge's own label on both ends; without it the
    /// role each end lands in (spec: Model Files, edges).
    pub label: Option<Label>,
    /// `acyclic`: the promise that no walk over the edge returns to
    /// a node (spec: Model Files, Acyclic links).
    pub acyclic: bool,
    /// `weights(::a; ::b)`: the quantity-valued properties of the
    /// scoped rows that are the edge's weights (spec: the edge
    /// model, Weights).
    pub weights: Vec<WeightSpec>,
}

/// `weight-potential SCOPE-PATH::prop;` — the property of the
/// scoped nodes is a potential: every crosslink edge with both ends
/// in the scope gains the weight prop(target) − prop(source), in the
/// direction walked, under the property's dimension (spec: Weight
/// Potentials).
#[derive(Debug, Clone)]
pub struct PotentialDecl {
    pub scope: String,
    pub prop: String,
    /// `::elevation<_m>`: the unit a bare number in the property is
    /// read in, as the weights clause's typed item (spec: Weight
    /// Potentials).
    pub unit: Option<String>,
    /// The statement as written, for messages.
    pub stmt: String,
}

/// The trailing `acyclic` of a ref or edge declaration, split off.
fn split_acyclic(rest: &str) -> (&str, bool) {
    let t = rest.trim_end();
    match t.strip_suffix("acyclic") {
        Some(head) if head.ends_with(char::is_whitespace) => (head.trim_end(), true),
        _ => (rest, false),
    }
}

/// The trailing `weights(::a; ::b)` clause of a ref or edge
/// declaration, split off: the property names, `::` stripped, each
/// with the unit of the typed form `::km<_km>`. The clause may stand
/// before or after `acyclic`.
fn split_weights(rest: &str) -> Result<(&str, Vec<WeightSpec>), String> {
    let t = rest.trim_end();
    let Some(head) = t.strip_suffix(')') else {
        return Ok((rest, Vec::new()));
    };
    let Some(open) = head.rfind("weights(") else {
        return Ok((rest, Vec::new()));
    };
    if open > 0 && !head[..open].ends_with(char::is_whitespace) {
        return Ok((rest, Vec::new()));
    }
    let inner = &head[open + "weights(".len()..];
    let mut names: Vec<WeightSpec> = Vec::new();
    for item in inner.split(';') {
        let item = item.trim();
        let Some(name) = item.strip_prefix("::") else {
            return Err(format!(
                "weights names the edge's weight properties as '::name', separated by ';': \
                 'weights(::duration; ::fare)' — found '{item}'"
            ));
        };
        let (name, unit) = match name.split_once('<') {
            Some((n, u)) => {
                let Some(u) = u.trim().strip_suffix('>') else {
                    return Err(format!(
                        "weights: '{item}' opens a unit with '<' and never closes it: a typed \
                         weight is '::km<_km>'"
                    ));
                };
                let u = split_unit_type(u.trim()).map_err(|e| format!("weights: '{item}' {e}"))?;
                (n.trim(), Some(u))
            }
            None => (name.trim(), None),
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("weights: '{item}' is no property name"));
        }
        if names.iter().any(|n| n.prop == name) {
            return Err(format!("weights names '::{name}' twice"));
        }
        names.push(WeightSpec {
            prop: name.to_string(),
            unit,
        });
    }
    if names.is_empty() {
        return Err("weights() names no property: 'weights(::km)'".to_string());
    }
    Ok((head[..open].trim_end(), names))
}

/// The unit between `<` and `>` of a typed weight or potential,
/// `_km` of `::km<_km>`: the underscore the quantity spelling writes
/// (`12_km`), then the unit's name (ruling 2026-11-07). The unit
/// without its underscore is refused with a pointer at the spelling.
fn split_unit_type(u: &str) -> Result<String, String> {
    let Some(unit) = u.strip_prefix('_') else {
        return Err(if u.is_empty() {
            "names no unit between '<' and '>': a typed weight is '::km<_km>'".to_string()
        } else {
            format!(
                "writes its unit without the underscore: a typed weight is '::km<_km>', the \
                 unit spelled as a quantity writes it ('<_{u}>')"
            )
        });
    };
    let unit = unit.trim();
    if unit.is_empty()
        || !unit
            .chars()
            .next()
            .is_some_and(quarb::quantity::is_name_char)
        || !unit.chars().all(|c| {
            quarb::quantity::is_name_char(c) || c.is_ascii_digit() || matches!(c, '*' | '/' | '^')
        })
    {
        return Err(
            "names no unit between '<_' and '>': a typed weight is '::km<_km>'".to_string(),
        );
    }
    Ok(unit.to_string())
}

/// The weights clause and the acyclic word in either order.
fn split_tail(rest: &str) -> Result<(&str, Vec<WeightSpec>, bool), String> {
    let (rest, acyclic) = split_acyclic(rest);
    let (rest, weights) = split_weights(rest)?;
    let (rest, acyclic2) = split_acyclic(rest);
    Ok((rest, weights, acyclic || acyclic2))
}

/// Split model-file text into statements at a lone `;` outside any
/// bracket pair. A maximal run of two or more `;` is a projection
/// sigil in a body (`;;;` and its `::;` cousin), never a terminator;
/// a `;` inside `(…)`, `[…]` or `{…}` separates a call's arguments
/// or a record's fields (`%(a = ::a; b = ::b)`), never statements;
/// strings are opaque.
/// `graph SCOPE(hop) { measure; measure(options); }`: the scope path
/// up to the hop's parentheses, the hop inside them, the measures
/// between the braces each ending in `;`.
fn parse_graph(rest: &str, stmt: &str) -> Result<GraphDecl, String> {
    let rest = rest.trim();
    let Some(open) = rest.find('{') else {
        return Err(format!(
            "graph takes its measures in braces: 'graph SCOPE(hop) {{ pagerank; }};' — '{stmt}'"
        ));
    };
    let Some(close) = rest.rfind('}') else {
        return Err(format!("graph: the braces do not close — '{stmt}'"));
    };
    if close < open {
        return Err(format!("graph: the braces do not close — '{stmt}'"));
    }
    let head = rest[..open].trim();
    let Some(hop_open) = head.rfind('(') else {
        return Err(format!(
            "graph takes the hop in parentheses after the scope: 'graph /pages/*(->link) {{ … }};' — '{stmt}'"
        ));
    };
    if !head.ends_with(')') {
        return Err(format!(
            "graph takes the hop in parentheses after the scope: 'graph /pages/*(->link) {{ … }};' — '{stmt}'"
        ));
    }
    let scope = head[..hop_open].trim();
    let hop = head[hop_open + 1..head.len() - 1].trim();
    if scope.is_empty() || hop.is_empty() {
        return Err(format!("graph takes a scope path and a hop — '{stmt}'"));
    }
    let mut measures = Vec::new();
    for m in rest[open + 1..close].split(';') {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        let name = m.split('(').next().unwrap_or(m).trim();
        if name.is_empty()
            || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
            || (m.contains('(') && !m.ends_with(')'))
        {
            return Err(format!(
                "graph: '{m}' is no measure; write the command's name with its options as a query does, 'betweenness(--normalized)' — '{stmt}'"
            ));
        }
        if measures
            .iter()
            .any(|x: &String| x.split('(').next() == Some(name))
        {
            return Err(format!(
                "graph: the measure '{name}' is declared twice — '{stmt}'"
            ));
        }
        measures.push(m.to_string());
    }
    if measures.is_empty() {
        return Err(format!("graph declares no measure — '{stmt}'"));
    }
    Ok(GraphDecl {
        scope: scope.to_string(),
        hop: hop.to_string(),
        measures,
        stmt: stmt.to_string(),
    })
}

fn statements(text: &str) -> Vec<String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            q @ ('\'' | '"') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '(' | '[' | '{' => {
                depth += 1;
                i += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            ';' => {
                let mut run = 0;
                while i < b.len() && b[i] == ';' {
                    run += 1;
                    i += 1;
                }
                if run == 1 && depth == 0 {
                    out.push(b[start..i - 1].iter().collect());
                    start = i;
                }
            }
            _ => i += 1,
        }
    }
    if start < b.len() {
        let tail: String = b[start..].iter().collect();
        if !tail.trim().is_empty() {
            out.push(tail);
        }
    }
    out
}

/// Strip the comments: a `#` at start of line or preceded by
/// whitespace runs to the end of its line (a `#` glued to text,
/// `s/\.#\d+//`, is body text, so substitutions survive); a `(!!`
/// opens the block comment (ruling 2026-11-06), closed by the `)`
/// that balances it, newlines inside kept so the lines keep their
/// numbers. Strings are opaque to both. A block comment no `)`
/// balances is refused at its opener.
fn strip_comments(text: &str) -> Result<String, String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ ('\'' | '"') => {
                out.push(q);
                i += 1;
                while i < b.len() && b[i] != q && b[i] != '\n' {
                    if b[i] == '\\' && i + 1 < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                    out.push(b[i]);
                    i += 1;
                }
                if i < b.len() && b[i] == q {
                    out.push(b[i]);
                    i += 1;
                }
            }
            '#' if i == 0 || b[i - 1].is_whitespace() => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '(' if b.get(i + 1) == Some(&'!') && b.get(i + 2) == Some(&'!') => {
                let open = i;
                let mut depth = 0usize;
                loop {
                    let Some(&c) = b.get(i) else {
                        return Err(format!(
                            "unterminated comment ((!!…) at character {}: no ')' balances its opener",
                            open + 1
                        ));
                    };
                    i += 1;
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        '\n' => out.push('\n'),
                        _ => {}
                    }
                }
                out.push(' ');
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Parse the left side of a `ref`/`edge` field into `(scope_path,
/// field)` — splitting at the last `::` (the property projection).
fn split_field(s: &str) -> Result<(String, String), String> {
    let s = s.trim();
    match s.rfind("::") {
        Some(idx) => {
            let path = s[..idx].trim().to_string();
            let field = s[idx + 2..].trim().to_string();
            if path.is_empty() {
                return Err(format!(
                    "ref needs a path scope before '::{field}' \
                     (bare fields are refused): '{s}'"
                ));
            }
            if field.is_empty() || field.contains(|c: char| c.is_whitespace()) {
                return Err(format!("ref/edge field is not a bare property: '{s}'"));
            }
            Ok((path, field))
        }
        None => Err(format!(
            "ref/edge needs a scoped '::field' (bare fields are refused): '{s}'"
        )),
    }
}

/// Resolve a `mount` target against the model file's directory: a
/// scheme target (`neo4j://`, `git:…`) or an absolute path is left
/// as-is; a relative filesystem path is joined to `base_dir`, so a
/// shipped fixture directory is self-locating.
pub fn resolve_mount_target(target: &str, base_dir: Option<&std::path::Path>) -> String {
    let has_scheme = target.split_once(':').is_some_and(|(s, _)| {
        s.len() > 1 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    let p = std::path::Path::new(target);
    if has_scheme || p.is_absolute() {
        return target.to_string();
    }
    match base_dir {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(target).display().to_string(),
        _ => target.to_string(),
    }
}

/// Split a trailing `<trait>` off a node head.
fn split_trait(head: &str) -> (String, Option<String>) {
    match head.split_once('<') {
        Some((path, rest)) => (
            path.trim().to_string(),
            rest.strip_suffix('>').map(|t| t.trim().to_string()),
        ),
        None => (head.to_string(), None),
    }
}

/// `/ips/ip` -> (`ips`, `ip`). A bare name has no role and is
/// refused: the role is what a hop into the container is called.
fn split_container_role(path: &str) -> Option<(String, String)> {
    let p = path.trim().trim_start_matches('/');
    let (container, role) = p.split_once('/')?;
    let (container, role) = (container.trim(), role.trim());
    if container.is_empty() || role.is_empty() || role.contains('/') {
        return None;
    }
    Some((container.to_string(), role.to_string()))
}

/// Split a trailing bracket group off a path: `/d/deploy [::at > $$]`
/// -> (`/d/deploy`, `[::at > $$]`). Scans from the end so a bracket
/// inside an earlier predicate stays with the path.
fn split_trailing_group(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if !s.ends_with(']') {
        return None;
    }
    let b: Vec<char> = s.chars().collect();
    let mut depth = 0usize;
    for i in (0..b.len()).rev() {
        match b[i] {
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    let path = b[..i].iter().collect::<String>().trim().to_string();
                    let group = b[i..].iter().collect::<String>();
                    return (!path.is_empty()).then_some((path, group));
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a `ref` target: `ips`, `/ips/ip`, or `/ips/ip[::id = $]` —
/// the container, the role the path form names, and the key field.
fn parse_ref_target(s: &str) -> Result<(String, String, Option<String>, Option<String>), String> {
    let (path, key_field) = match split_trailing_group(s) {
        Some((path, group)) => {
            let inner = group.trim_start_matches('[').trim_end_matches(']').trim();
            let (lhs, rhs) = inner
                .split_once('=')
                .ok_or_else(|| format!("ref target condition must be 'PROP = $': '{s}'"))?;
            if rhs.trim() != "$" {
                return Err(format!(
                    "ref resolves by value, so its condition is 'PROP = $': '{s}'"
                ));
            }
            let key = lhs.trim().trim_start_matches("::").trim().to_string();
            // The key is a property of the target node, read by name:
            // a navigation there would be looked up as a property
            // called `//creation/date::when` and match nothing.
            if key.contains('/') || key.contains("::") {
                return Err(format!(
                    "ref target condition names a property of the target node, \
                     '[::PROP = $]', not a path: '{s}' (aim the ref at the node \
                     that carries the property and climb from there, \
                     '->ROLE | \\?NAME')"
                ));
            }
            (path, (!key.is_empty()).then_some(key))
        }
        None => (s.trim().to_string(), None),
    };
    // The target is one path (a bare container, `/container/role`,
    // a mount's path) with its condition, then `as NAME` and
    // `acyclic`, both optional and in that order: any other
    // trailing words are a refusal, as a misspelt keyword is.
    if path.contains(char::is_whitespace) {
        return Err(format!(
            "ref target carries trailing text: '{s}' (a ref is 'PATH::field --> \
             container[::PROP = $]', then an optional 'as NAME' and 'acyclic')"
        ));
    }
    // The container is the first segment of a path form, or the whole
    // word of the bare form; the path form's second segment is the role.
    let mut segs = path.trim_start_matches('/').split('/').map(str::trim);
    let container = segs.next().unwrap_or("").to_string();
    let role = segs.next().filter(|r| !r.is_empty()).map(str::to_string);
    if container.is_empty() {
        return Err(format!("ref needs a target container: '{s}'"));
    }
    Ok((container, path, role, key_field))
}

/// Split at the first `->` that is not part of a `-->`.
fn split_arrow(s: &str) -> Option<(String, String)> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == '-' && b[i + 1] == '>' && !(i > 0 && b[i - 1] == '-') {
            return Some((
                b[..i].iter().collect::<String>().trim().to_string(),
                b[i + 2..].iter().collect::<String>().trim().to_string(),
            ));
        }
        i += 1;
    }
    None
}

/// Parse model-file text. A `use FILE;` statement needs a location
/// to resolve against, so it is refused here; [`parse_model_file`]
/// is the entry point for a model on disk.
pub fn parse_model(text: &str) -> Result<Model, String> {
    parse_model_in(text, None, &mut Vec::new(), &mut Vec::new())
}

/// Parse a model file, resolving its `use FILE;` includes against
/// its own directory (recursively; a cycle is refused). An included
/// model's mounts are resolved against the included file's
/// directory, so each model stays self-locating; its `node`, `ref`,
/// `rel`, `edge`, `alias` and `def` statements join the includer's,
/// ahead of them; a mount name declared twice is refused.
pub fn parse_model_file(path: &std::path::Path) -> Result<Model, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
    let mut visited = vec![path.canonicalize().unwrap_or_else(|_| path.to_path_buf())];
    parse_model_in(&text, path.parent(), &mut visited, &mut Vec::new())
}

fn parse_model_in(
    text: &str,
    base_dir: Option<&std::path::Path>,
    visited: &mut Vec<std::path::PathBuf>,
    done: &mut Vec<std::path::PathBuf>,
) -> Result<Model, String> {
    let mut model = Model::default();
    let mut defs = Vec::new();
    for raw in statements(&strip_comments(text)?) {
        let stmt = raw.trim();
        if stmt.is_empty() {
            continue;
        }
        let (head, rest) = match stmt.split_once(char::is_whitespace) {
            Some((h, r)) => (h, r.trim()),
            None => (stmt, ""),
        };
        match head {
            "node" => {
                let (head, query) = rest
                    .split_once(':')
                    .ok_or_else(|| format!("node needs '/container/role: query': '{stmt}'"))?;
                let (path, trait_name) = split_trait(head.trim());
                let (name, role) = split_container_role(&path).ok_or_else(|| {
                    format!(
                        "node needs '/container/role', so a hop can be \
                         labelled by the role it lands on: '{stmt}'"
                    )
                })?;
                let trait_name = trait_name.unwrap_or_else(|| role.clone());
                model.nodes.push(NodeDecl {
                    name,
                    role,
                    trait_name,
                    query: query.trim().to_string(),
                    traits_before: model.traits.len(),
                });
            }
            "mount" => {
                let (name, target) = rest
                    .split_once(':')
                    .ok_or_else(|| format!("mount needs 'NAME: target': '{stmt}'"))?;
                let name = name.trim().to_string();
                if model.mounts.iter().any(|m| m.name == name) {
                    return Err(format!("mount '{name}' is already declared: '{stmt}'"));
                }
                model.mounts.push(Mount {
                    name,
                    target: target.trim().to_string(),
                });
            }
            "use" => {
                let file = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                if file.is_empty() {
                    return Err(format!("use needs a model file: '{stmt}'"));
                }
                let Some(dir) = base_dir else {
                    return Err(format!(
                        "use needs a location to resolve '{file}' against: pass the model as a file (--model FILE)"
                    ));
                };
                let path = {
                    let p = std::path::Path::new(file);
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        dir.join(p)
                    }
                };
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                // `visited` is the chain of files being read: meeting
                // one of them again is a cycle. A file already read
                // on another branch (two models over one base, used
                // by a third) is in `done`, and is not read twice.
                if visited.contains(&canon) {
                    return Err(format!("use {file}: the model includes itself"));
                }
                if done.contains(&canon) {
                    continue;
                }
                visited.push(canon.clone());
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("use {file}: reading {}: {e}", path.display()))?;
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
                let sub = parse_model_in(&text, path.parent(), visited, done)
                    .map_err(|e| format!("use {file}: {e}"))?;
                visited.pop();
                done.push(canon);
                for m in sub.mounts {
                    if model.mounts.iter().any(|x| x.name == m.name) {
                        return Err(format!(
                            "use {file}: mount '{}' is already declared",
                            m.name
                        ));
                    }
                    // A mount's target is kept relative to its model's
                    // own directory (the caller resolves it once,
                    // against the file it was given). A used model's
                    // targets are therefore carried over by the `use`
                    // path as written, not by the path already joined
                    // to this model's directory: that would name the
                    // directory twice from anywhere but inside it.
                    model.mounts.push(Mount {
                        name: m.name,
                        target: resolve_mount_target(
                            &m.target,
                            std::path::Path::new(file).parent(),
                        ),
                    });
                }
                // the used file's constructors see the traits declared
                // before them in their own file, after the user's so far
                let traits_so_far = model.traits.len();
                model.nodes.extend(sub.nodes.into_iter().map(|mut n| {
                    n.traits_before += traits_so_far;
                    n
                }));
                model.refs.extend(sub.refs);
                model.potentials.extend(sub.potentials);
                model.rels.extend(sub.rels);
                model.edges.extend(sub.edges);
                model.aliases.extend(sub.aliases);
                model.indexes.extend(sub.indexes);
                model.units.extend(sub.units);
                for f in sub.unit_files {
                    if model.unit_files.contains(&f) {
                        return Err(format!("use {file}: a unit file is imported twice"));
                    }
                    model.unit_files.push(f);
                }
                for t in sub.trees {
                    if model.trees.iter().any(|x| x.identity == t.identity) {
                        return Err(format!(
                            "use {file}: tree '{}' is already declared",
                            t.identity
                        ));
                    }
                    model.trees.push(t);
                }
                for t in sub.traits {
                    if model.traits.iter().any(|x| x.name == t.name) {
                        return Err(format!(
                            "use {file}: trait <{}> is already declared",
                            t.name
                        ));
                    }
                    model.traits.push(t);
                }
                if !sub.defs_text.trim().is_empty() {
                    defs.push(sub.defs_text);
                }
            }
            "ref" => {
                let (rest, weights, acyclic) = split_tail(rest)?;
                let (left, container) = rest
                    .split_once("-->")
                    .ok_or_else(|| format!("ref needs 'PATH::field --> container': '{stmt}'"))?;
                let (scope, field) = split_field(left)?;
                // `--> container as NAME` names the edge's label, as
                // on an edge statement; `acyclic` follows it
                let (container, label) = match container.trim_end().rsplit_once(" as ") {
                    Some((c, name))
                        if !name.trim().is_empty()
                            && !name.trim().contains(char::is_whitespace) =>
                    {
                        (c, Some(Label::parse(name.trim())))
                    }
                    _ => (container, None),
                };
                let (container, path, role, key_field) = parse_ref_target(container)?;
                model.refs.push(RefDecl {
                    scope,
                    acyclic,
                    weights,
                    field,
                    container,
                    path,
                    role,
                    key_field,
                    label,
                });
            }
            "rel" => {
                let (source, target) = split_arrow(rest)
                    .ok_or_else(|| format!("rel needs 'SOURCE -> TARGET[cond]': '{stmt}'"))?;
                let (target, cond) = split_trailing_group(&target).ok_or_else(|| {
                    format!(
                        "rel needs a condition — two node sets joined by \
                         nothing are a cross product: '{stmt}'"
                    )
                })?;
                if source.is_empty() {
                    return Err(format!("rel needs a source path: '{stmt}'"));
                }
                model.rels.push(RelDecl {
                    source,
                    target,
                    cond,
                });
            }
            "edge" => {
                let (rest, weights, acyclic) = split_tail(rest)?;
                // the colon after the scope, outside any predicate
                // the scope carries (`/flights/*[::carrier = "x"]:`)
                let (scope_head, pair) = split_scope_colon(rest)
                    .ok_or_else(|| format!("edge needs 'PATH: ::a -- ::b': '{stmt}'"))?;
                let (a, b) = pair
                    .split_once("--")
                    .ok_or_else(|| format!("edge needs two fields 'a -- b': '{stmt}'"))?;
                let field_a = a.trim().trim_start_matches("::").trim().to_string();
                // `::b as NAME` names the edge's label
                let (b, label) = match b.rsplit_once(" as ") {
                    Some((b, name)) if !name.trim().is_empty() => {
                        (b.to_string(), Some(Label::parse(name.trim())))
                    }
                    _ => (b.to_string(), None),
                };
                let field_b = b.trim().trim_start_matches("::").trim().to_string();
                model.edges.push(EdgeDecl {
                    scope: scope_head.trim().to_string(),
                    field_a,
                    field_b,
                    label,
                    acyclic,
                    weights,
                });
            }
            "def" | "macro" => {
                defs.push(format!("{stmt};"));
            }
            "alias" => {
                model.aliases.push(parse_alias(rest, stmt)?);
            }
            "tree" => {
                // The identity may hold colons (`https://…`,
                // `urn:isbn:`): the path is what follows the LAST
                // `:` that whitespace follows.
                let (identity, path) = rest
                    .rfind(": ")
                    .or_else(|| rest.rfind(":\t"))
                    .map(|i| (rest[..i].trim(), rest[i + 1..].trim()))
                    .ok_or_else(|| format!("tree needs 'IDENTITY: PATH': '{stmt}'"))?;
                if identity.is_empty() || path.is_empty() {
                    return Err(format!("tree needs 'IDENTITY: PATH': '{stmt}'"));
                }
                if !path.starts_with('/') {
                    return Err(format!(
                        "a tree stands at a path from the root ('/mount' or '/mount/…'): '{stmt}'"
                    ));
                }
                if model.trees.iter().any(|t| t.identity == identity) {
                    return Err(format!("tree '{identity}' is declared twice: '{stmt}'"));
                }
                model.trees.push(TreeDecl {
                    identity: identity.to_string(),
                    path: path.to_string(),
                });
            }
            "unit" => {
                // `unit NAME[, ALIAS...];` declares a base unit of its
                // own dimension; an alias ending in `-` stands before
                // the magnitude (`$12`).
                let (names, right) = match rest.split_once('=') {
                    Some((n, r)) => (n, Some(r)),
                    None => (rest, None),
                };
                let names: Vec<String> = names.split(',').map(|n| n.trim().to_string()).collect();
                let good = |n: &str| {
                    let body = n.strip_suffix('-').unwrap_or(n);
                    !body.is_empty() && body.chars().all(quarb::quantity::is_name_char)
                };
                if names.iter().any(|n| !good(n)) || names[0].ends_with('-') {
                    return Err(format!(
                        "unit names are letters or currency symbols, separated by commas, a prefix alias ending in '-': '{stmt}'"
                    ));
                }
                let Some(right) = right else {
                    model.units.push(UnitDecl {
                        expr: names[0].clone(),
                        names,
                        factor: 1.0,
                        stmt: stmt.to_string(),
                    });
                    continue;
                };
                let right = right.trim();
                let (factor, expr) = right.split_once(char::is_whitespace).ok_or_else(|| {
                    format!("unit needs a factor and a unit expression after '=': '{stmt}'")
                })?;
                let factor = parse_factor(factor).ok_or_else(|| {
                    format!("unit: '{factor}' is not a positive decimal or ratio: '{stmt}'")
                })?;
                let expr = expr.trim().to_string();
                if quarb::quantity::scale_expr(&expr).is_none()
                    && !expr.chars().all(|c| {
                        quarb::quantity::is_name_char(c)
                            || c.is_ascii_digit()
                            || matches!(c, '*' | '/' | '^')
                    })
                {
                    return Err(format!("unit: '{expr}' is not unit text: '{stmt}'"));
                }
                model.units.push(UnitDecl {
                    names,
                    factor,
                    expr,
                    stmt: stmt.to_string(),
                });
            }
            "units" => {
                let file = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                if file.is_empty() {
                    return Err(format!("units needs a file of unit declarations: '{stmt}'"));
                }
                let path = std::path::Path::new(file);
                let path = match (path.is_absolute(), base_dir) {
                    (false, Some(dir)) => dir.join(path),
                    _ => path.to_path_buf(),
                };
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                if model.unit_files.contains(&canon) {
                    return Err(format!("units '{file}' is imported twice: '{stmt}'"));
                }
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("units {file}: reading {}: {e}", path.display()))?;
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
                let sub = parse_model_in(&text, path.parent(), visited, done)
                    .map_err(|e| format!("units {file}: {e}"))?;
                if !sub.mounts.is_empty()
                    || !sub.nodes.is_empty()
                    || !sub.refs.is_empty()
                    || !sub.rels.is_empty()
                    || !sub.edges.is_empty()
                    || !sub.aliases.is_empty()
                    || !sub.indexes.is_empty()
                    || !sub.trees.is_empty()
                    || !sub.traits.is_empty()
                    || !sub.defs_text.trim().is_empty()
                {
                    return Err(format!(
                        "units {file}: a units file holds unit declarations only"
                    ));
                }
                model.unit_files.push(canon);
                for f in sub.unit_files {
                    if model.unit_files.contains(&f) {
                        return Err(format!("units {file}: a unit file is imported twice"));
                    }
                    model.unit_files.push(f);
                }
                model.units.extend(sub.units);
            }
            "graph" => {
                let decl = parse_graph(rest, stmt)?;
                if model
                    .graphs
                    .iter()
                    .any(|g| g.scope == decl.scope && g.hop == decl.hop)
                {
                    return Err(format!("graph is declared twice: '{stmt}'"));
                }
                model.graphs.push(decl);
            }
            "index" => {
                let (scope, property) = split_field(rest)?;
                if model
                    .indexes
                    .iter()
                    .any(|d| d.scope == scope && d.property == property)
                {
                    return Err(format!("index is declared twice: '{stmt}'"));
                }
                model.indexes.push(IndexDecl { scope, property });
            }
            "trait" => {
                let decl = parse_trait(rest, stmt)?;
                if model.traits.iter().any(|t| t.name == decl.name) {
                    return Err(format!("trait <{}> is declared twice: '{stmt}'", decl.name));
                }
                model.traits.push(decl);
            }
            "weight-potential" => {
                let (scope, prop) = split_field(rest).map_err(|_| {
                    format!(
                        "weight-potential needs 'SCOPE-PATH::prop' (the property of the scoped \
                         nodes that is the potential): '{stmt}'"
                    )
                })?;
                // the typed form `::elevation<_m>`, as the weights clause's
                let (prop, unit) = match prop.split_once('<') {
                    Some((n, u)) => {
                        let Some(u) = u.trim().strip_suffix('>') else {
                            return Err(format!(
                                "weight-potential: '::{prop}' opens a unit with '<' and never \
                                 closes it: a typed potential is '::elevation<_m>': '{stmt}'"
                            ));
                        };
                        let u = split_unit_type(u.trim()).map_err(|e| {
                            format!(
                                "weight-potential: '::{prop}' {}: '{stmt}'",
                                e.replace(
                                    "a typed weight is '::km<_km>'",
                                    "a typed potential is '::elevation<_m>'"
                                )
                            )
                        })?;
                        (n.trim().to_string(), Some(u))
                    }
                    None => (prop, None),
                };
                if model
                    .potentials
                    .iter()
                    .any(|p| p.scope == scope && p.prop == prop)
                {
                    return Err(format!("weight-potential is declared twice: '{stmt}'"));
                }
                model.potentials.push(PotentialDecl {
                    scope,
                    prop,
                    unit,
                    stmt: stmt.to_string(),
                });
            }
            other => {
                return Err(format!(
                    "unknown model statement '{other}' \
                     (expected node/ref/rel/edge/mount/alias/trait/index/graph/tree/unit/use/\
                     weight-potential/def/macro)"
                ));
            }
        }
    }
    model.defs_text = defs.join("\n");
    // An edge's endpoints are the scoped nodes' *references*: each
    // field must be ref-declared, or the edge would have no
    // containers to connect. Refusing here keeps the failure loud —
    // a skipped edge at walk time reads as an empty graph.
    for edge in &model.edges {
        for field in [&edge.field_a, &edge.field_b] {
            if !model.refs.iter().any(|r| &r.field == field) {
                return Err(format!(
                    "edge field '::{field}' has no ref declaration — \
                     an edge connects the containers its fields \
                     resolve into, so declare \
                     'ref {}::{field} --> ...;' first",
                    edge.scope
                ));
            }
        }
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_block_comment_is_whitespace_in_a_model() {
        // between statements, inside one, holding `;`, `#` and parens
        let plain = super::parse_model("node /a/b: /x/*;\nref /a/b::k --> /c/*;\n").unwrap();
        let commented = super::parse_model(
            "(!! the header; (a; b) )\nnode /a/b: (!! # inside\n two lines ) /x/*;\n\
             ref /a/b::k (!! --> is the arrow ) --> /c/*; (!! trailing )\n",
        )
        .unwrap();
        assert_eq!(format!("{commented:?}"), format!("{plain:?}"));
        // a `#` line comment still runs to its line's end, `(!!` and all
        let lined =
            super::parse_model("# see (!! no comment opens here\nnode /a/b: /x/*;\n").unwrap();
        assert_eq!(
            format!("{lined:?}"),
            format!("{:?}", super::parse_model("node /a/b: /x/*;\n").unwrap())
        );
        let e = super::parse_model("node /a/b: /x/*; (!! open (x )\n").unwrap_err();
        assert!(e.contains("unterminated comment"), "{e}");
        assert!(e.contains("at character 18"), "{e}");
    }

    use super::*;

    #[test]
    fn a_ref_takes_as_name_then_acyclic_and_nothing_else() {
        let m = parse_model(
            "ref /figures/*::mother_id --> /figures/*[::id = $] as mother acyclic;\n\
             ref /figures/*::father_id --> /figures/*[::id = $] as father;\n\
             ref /figures/*::spouse_id --> /figures/*[::id = $];",
        )
        .unwrap();
        assert_eq!(m.refs[0].label, Some(Label::Name("mother".into())));
        assert!(m.refs[0].acyclic);
        assert_eq!(m.refs[0].key_field.as_deref(), Some("id"));
        assert_eq!(m.refs[1].label, Some(Label::Name("father".into())));
        assert!(!m.refs[1].acyclic);
        assert_eq!(m.refs[2].label, None);
        // a ref's target is one path and its condition, then `as
        // NAME` and `acyclic`: other trailing words refuse by the line
        let err = parse_model("ref /figures/*::mother_id --> /figures/*[::id = $] mother;")
            .err()
            .expect("refuses");
        assert!(err.contains("trailing text"), "{err}");
        assert!(err.contains("$] mother"), "{err}");
        let err = parse_model("ref /figures/*::mother_id --> /figures/*[::id = $] as the mother;")
            .err()
            .expect("refuses");
        assert!(err.contains("trailing text"), "{err}");
    }

    #[test]
    fn a_label_by_property_parses_on_a_ref_and_on_an_edge() {
        let m = parse_model(
            "ref /flights/*::origin --> /airports/*[::code = $] as ::carrier;\n\
             ref /flights/*::dest --> /airports/*[::code = $];\n\
             edge /flights/*: ::origin -- ::dest as ::carrier;\n\
             edge /flights/*[::carrier = \"panam\"]: ::origin -- ::dest as panam;",
        )
        .unwrap();
        assert_eq!(m.refs[0].label, Some(Label::Property("carrier".into())));
        assert_eq!(m.edges[0].label, Some(Label::Property("carrier".into())));
        assert_eq!(m.edges[0].label.as_ref().unwrap().written(), "::carrier");
        assert_eq!(m.edges[1].scope, "/flights/*[::carrier = \"panam\"]");
        assert_eq!(m.edges[1].label, Some(Label::Name("panam".into())));
    }

    #[test]
    fn parses_the_forum_model() {
        let m = parse_model(
            r#"
            # the forum's implied graph
            mount forum: posts.csv;
            node /ips/ip:         /forum/posts/*::ip     | unique;
            node /cookies/cookie: /forum/posts/*::cookie | unique;
            ref /forum/posts/*::ip     --> ips;
            ref /forum/posts/*::cookie --> cookies;
            edge /forum/posts/*: ::cookie -- ::ip;
            "#,
        )
        .unwrap();
        assert_eq!(m.mounts.len(), 1);
        assert_eq!(m.mounts[0].name, "forum");
        assert_eq!(m.mounts[0].target, "posts.csv");
        assert_eq!(m.nodes.len(), 2);
        assert_eq!(m.nodes[0].name, "ips");
        assert_eq!(m.nodes[0].role, "ip");
        assert_eq!(m.nodes[0].trait_name, "ip");
        assert_eq!(m.nodes[0].query, "/forum/posts/*::ip     | unique");
        assert_eq!(m.refs.len(), 2);
        assert_eq!(m.refs[1].scope, "/forum/posts/*");
        assert_eq!(m.refs[1].field, "cookie");
        assert_eq!(m.refs[1].container, "cookies");
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.edges[0].scope, "/forum/posts/*");
        assert_eq!(m.edges[0].field_a, "cookie");
        assert_eq!(m.edges[0].field_b, "ip");
    }

    #[test]
    fn trailing_comments_are_stripped_but_glued_hashes_survive() {
        let m = parse_model(
            "node /ips/ip: /row::ip | unique;   # distinct ips\n\
             node /tags/tag: /row::tag | s/#(\\d+)/$1/ | unique;\n\
             ref /row::ip --> ips;  # resolves there\n",
        )
        .unwrap();
        assert_eq!(m.nodes.len(), 2);
        assert_eq!(m.nodes[0].query, "/row::ip | unique");
        // the glued '#' inside the substitution is body text
        assert!(m.nodes[1].query.contains("s/#("));
        assert_eq!(m.refs.len(), 1);
    }

    #[test]
    fn collects_defs_and_comments() {
        let m = parse_model("def &recent: [::ts > '2026-01-01'];\nnode /who/user: /log/*::user;")
            .unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert!(m.defs_text.contains("def &recent"));
    }

    #[test]
    fn a_semicolon_projection_is_not_a_terminator() {
        // legacy ;;; in a body survives the statement splitter
        let m = parse_model("node /sizes/size: /files/*:::size;").unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].query, "/files/*:::size");
    }

    #[test]
    fn a_node_head_needs_a_role() {
        // `node ips: …` says nothing about what a hop landing in
        // the container should be called.
        assert!(parse_model("node ips: /row::ip;").is_err());
    }

    #[test]
    fn declares_an_explicit_trait() {
        let m = parse_model("node /ips/ip<addr>: /row::ip | unique;").unwrap();
        assert_eq!(m.nodes[0].role, "ip");
        assert_eq!(m.nodes[0].trait_name, "addr");
    }

    #[test]
    fn bare_field_ref_is_refused() {
        assert!(parse_model("ref ::ip --> ips;").is_err());
    }

    #[test]
    fn a_ref_target_may_be_a_path_with_its_condition_said_out_loud() {
        let m = parse_model("ref /row::cookie --> /cookies/cookie[::id = $];").unwrap();
        assert_eq!(m.refs[0].container, "cookies");
        assert_eq!(m.refs[0].key_field.as_deref(), Some("id"));
        // the short form is the same statement with the condition left
        // to resolution
        let m = parse_model("ref /row::cookie --> /cookies/cookie;").unwrap();
        assert_eq!(m.refs[0].container, "cookies");
        assert_eq!(m.refs[0].key_field, None);
    }

    #[test]
    fn parses_a_relation() {
        let m = parse_model("rel /errors/error -> /deploys/deploy [::at > $$0::at];").unwrap();
        assert_eq!(m.rels.len(), 1);
        assert_eq!(m.rels[0].source, "/errors/error");
        assert_eq!(m.rels[0].target, "/deploys/deploy");
        assert_eq!(m.rels[0].cond, "[::at > $$0::at]");
    }

    #[test]
    fn an_edge_over_unrefd_fields_is_refused() {
        // the edge's endpoints are references; without the refs
        // there is nothing to connect
        let err = parse_model(
            "node /ips/ip: /row::ip | unique;\n\
             edge /row: ::cookie -- ::ip;",
        )
        .unwrap_err();
        assert!(err.contains("no ref declaration"), "{err}");
    }

    #[test]
    fn a_relation_without_a_condition_is_refused() {
        // two node sets joined by nothing are a cross product
        assert!(parse_model("rel /errors/error -> /deploys/deploy;").is_err());
    }

    #[test]
    fn graph_declarations_parse_and_refuse_twice() {
        let m = parse_model(
            "graph /pages/*(->link) { pagerank; conncomp; };\n\
             graph /mod/**(--*) { conncomp; betweenness(--normalized); bridges; };",
        )
        .unwrap();
        assert_eq!(m.graphs.len(), 2);
        assert_eq!(m.graphs[0].scope, "/pages/*");
        assert_eq!(m.graphs[0].hop, "->link");
        assert_eq!(m.graphs[0].measures, ["pagerank", "conncomp"]);
        assert_eq!(m.graphs[1].hop, "--*");
        assert_eq!(m.graphs[1].measures[1], "betweenness(--normalized)");
        let twice = parse_model(
            "graph /pages/*(->link) { pagerank; };\ngraph /pages/*(->link) { conncomp; };",
        );
        assert!(twice.unwrap_err().contains("graph is declared twice"));
        assert!(parse_model("graph /pages/* { pagerank; };").is_err());
        assert!(parse_model("graph /pages/*(->link) { };").is_err());
        assert!(
            parse_model("graph /pages/*(->link) { pagerank; pagerank(--damping=0.5); };")
                .unwrap_err()
                .contains("declared twice")
        );
    }
}

/// The first literal segment of a path (`/books/*` → `books`,
/// `books` → `books`); `None` for an unanchored search (`//x`) or
/// a wildcard.
fn first_segment(path: &str) -> Option<String> {
    let t = path.trim();
    if t.starts_with("//") {
        return None;
    }
    let t = t.trim_start_matches('/');
    let end = t
        .find(|c: char| matches!(c, '/' | '[' | ':' | '(' | '<' | '>' | '*' | ' ' | '-'))
        .unwrap_or(t.len());
    let seg = &t[..end];
    (!seg.is_empty()).then(|| seg.to_string())
}

impl Model {
    /// The mounts a query over `named` may reach through the
    /// model's declarations, named included: a `ref` from a scope
    /// in one mount to a container or path in another, a `rel`
    /// from a source to a target, and every mount a `tree`
    /// declaration stands in (a reference by identity may come from
    /// anywhere). Pure: a replay plan prefetches these before the
    /// cells run. Unanchored declarations (`//x`) add nothing they
    /// cannot name.
    pub fn reachable_roots(&self, named: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = named.iter().map(|s| s.to_string()).collect();
        let mut edges: Vec<(Option<String>, Option<String>)> = Vec::new();
        for r in &self.refs {
            let to = first_segment(&r.path).or_else(|| first_segment(&r.container));
            edges.push((first_segment(&r.scope), to));
        }
        for r in &self.rels {
            edges.push((first_segment(&r.source), first_segment(&r.target)));
        }
        let mut changed = true;
        while changed {
            changed = false;
            for (from, to) in &edges {
                let reached = match from {
                    Some(f) => out.contains(f),
                    None => true,
                };
                if reached
                    && let Some(t) = to
                    && !out.contains(t)
                {
                    out.push(t.clone());
                    changed = true;
                }
            }
        }
        for t in &self.trees {
            if let Some(m) = first_segment(&t.path)
                && !out.contains(&m)
            {
                out.push(m);
            }
        }
        out
    }
}

#[cfg(test)]
mod ref_condition_tests {
    use super::parse_model;

    /// The condition of a ref target names a property of the target
    /// node, read by name; a navigation there is refused at parse
    /// time rather than looked up as a property and matching nothing.
    #[test]
    fn a_ref_target_condition_is_a_property_not_a_path() {
        assert!(parse_model("ref /a/*::when --> /b/c[::when = $];").is_ok());
        let e = parse_model("ref /a/*::when --> /b/c[//d/date::when = $];").unwrap_err();
        assert!(e.contains("not a path") && e.contains("climb"), "{e}");
        let e = parse_model("ref /a/*::when --> /b/c[/d::when = $];").unwrap_err();
        assert!(e.contains("not a path"), "{e}");
    }
}

/// A unit declaration's factor: a positive decimal, or a ratio of
/// two (`1/40`), exact where a decimal would round.
fn parse_factor(text: &str) -> Option<f64> {
    let f = match text.split_once('/') {
        Some((a, b)) => a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?,
        None => text.parse::<f64>().ok()?,
    };
    (f.is_finite() && f > 0.0).then_some(f)
}
