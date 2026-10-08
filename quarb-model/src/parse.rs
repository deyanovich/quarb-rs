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
    /// `tree IDENTITY: PATH;` — the string other trees refer to this
    /// one by, and the path (a mount root or a node inside one) the
    /// tree stands at. An identity ending in a separator is a base.
    pub trees: Vec<TreeDecl>,
    /// `units LIBRARY;` — a kaiv `.faiv` unit library imported into
    /// the unit table the unital reading consults, for every source
    /// the model mounts: every spelling it lists reads, output
    /// prints the primary name. A relative path is joined to the
    /// model file's directory.
    pub units: Vec<UnitsDecl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnitsDecl {
    /// The library as written.
    pub library: String,
    /// The file, resolved.
    pub path: std::path::PathBuf,
}

/// A declared trait: `trait <heading>: (h1|h2|h3);`.
#[derive(Debug, Clone)]
pub struct TreeDecl {
    pub identity: String,
    pub path: String,
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
}

#[derive(Debug, Clone)]
pub struct RefDecl {
    /// The node-selecting path (the `::field` suffix stripped).
    pub scope: String,
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

#[derive(Debug, Clone)]
pub struct EdgeDecl {
    pub scope: String,
    pub field_a: String,
    pub field_b: String,
}

/// Split model-file text into statements at a lone `;` outside any
/// bracket pair. A maximal run of two or more `;` is a projection
/// sigil in a body (`;;;` and its `::;` cousin), never a terminator;
/// a `;` inside `(…)`, `[…]` or `{…}` separates a call's arguments
/// or a record's fields (`%(a = ::a; b = ::b)`), never statements;
/// strings are opaque.
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

/// Strip `#` comments: from a `#` at start of line or preceded by
/// whitespace, to end of line. Strings are opaque, and a `#` glued
/// to text (`s/\.#\d+//`) is body text, not a comment — so trailing
/// annotations parse while substitutions survive.
fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                q @ ('\'' | '"') => {
                    out.push(q);
                    i += 1;
                    while i < b.len() && b[i] != q {
                        if b[i] == '\\' && i + 1 < b.len() {
                            out.push(b[i]);
                            i += 1;
                        }
                        out.push(b[i]);
                        i += 1;
                    }
                    if i < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                }
                '#' if i == 0 || b[i - 1].is_whitespace() => break,
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out.push('\n');
    }
    out
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
            (path, (!key.is_empty()).then_some(key))
        }
        None => (s.trim().to_string(), None),
    };
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
    for raw in statements(&strip_comments(text)) {
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
                model.nodes.extend(sub.nodes);
                model.refs.extend(sub.refs);
                model.rels.extend(sub.rels);
                model.edges.extend(sub.edges);
                model.aliases.extend(sub.aliases);
                model.indexes.extend(sub.indexes);
                for u in sub.units {
                    if !model.units.iter().any(|x| x.path == u.path) {
                        model.units.push(u);
                    }
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
                let (left, container) = rest
                    .split_once("-->")
                    .ok_or_else(|| format!("ref needs 'PATH::field --> container': '{stmt}'"))?;
                let (scope, field) = split_field(left)?;
                let (container, path, role, key_field) = parse_ref_target(container)?;
                model.refs.push(RefDecl {
                    scope,
                    field,
                    container,
                    path,
                    role,
                    key_field,
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
                let (scope_head, pair) = rest
                    .split_once(':')
                    .ok_or_else(|| format!("edge needs 'PATH: ::a -- ::b': '{stmt}'"))?;
                let (a, b) = pair
                    .split_once("--")
                    .ok_or_else(|| format!("edge needs two fields 'a -- b': '{stmt}'"))?;
                let field_a = a.trim().trim_start_matches("::").trim().to_string();
                let field_b = b.trim().trim_start_matches("::").trim().to_string();
                model.edges.push(EdgeDecl {
                    scope: scope_head.trim().to_string(),
                    field_a,
                    field_b,
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
            "units" => {
                let library = rest.trim().trim_matches(|c| c == '"' || c == '\'');
                if library.is_empty() {
                    return Err(format!(
                        "units needs a library: 'units starye-mery.faiv;' ('{stmt}')"
                    ));
                }
                let path = std::path::Path::new(library);
                let path = match (path.is_absolute(), base_dir) {
                    (false, Some(dir)) => dir.join(path),
                    _ => path.to_path_buf(),
                };
                if model.units.iter().any(|u| u.path == path) {
                    return Err(format!("units '{library}' is imported twice: '{stmt}'"));
                }
                model.units.push(UnitsDecl {
                    library: library.to_string(),
                    path,
                });
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
            other => {
                return Err(format!(
                    "unknown model statement '{other}' \
                     (expected node/ref/rel/edge/mount/alias/trait/index/tree/def/macro)"
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
    use super::*;

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
        let m = parse_model("node /sizes/size: /files/*::::size;").unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].query, "/files/*::::size");
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
        let m = parse_model("rel /errors/error -> /deploys/deploy [::at > _::at];").unwrap();
        assert_eq!(m.rels.len(), 1);
        assert_eq!(m.rels[0].source, "/errors/error");
        assert_eq!(m.rels[0].target, "/deploys/deploy");
        assert_eq!(m.rels[0].cond, "[::at > _::at]");
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
}
