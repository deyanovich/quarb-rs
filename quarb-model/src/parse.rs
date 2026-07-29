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

/// Split model-file text into statements at a lone `;`. A maximal
/// run of two or more `;` is a projection sigil in a body (`;;;` and
/// its `::;` cousin), never a terminator; strings are opaque.
fn statements(text: &str) -> Vec<String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
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
            ';' => {
                let mut run = 0;
                while i < b.len() && b[i] == ';' {
                    run += 1;
                    i += 1;
                }
                if run == 1 {
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

/// Parse a `ref` target: `ips`, `/ips/ip`, or `/ips/ip[::id = $]`.
fn parse_ref_target(s: &str) -> Result<(String, Option<String>), String> {
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
    // word of the bare form.
    let container = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if container.is_empty() {
        return Err(format!("ref needs a target container: '{s}'"));
    }
    Ok((container, key_field))
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

/// Parse model-file text.
pub fn parse_model(text: &str) -> Result<Model, String> {
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
                let (head, query) = rest.split_once(':').ok_or_else(|| {
                    format!("node needs '/container/role: query': '{stmt}'")
                })?;
                let (path, trait_name) = split_trait(head.trim());
                let (name, role) = split_container_role(&path)
                    .ok_or_else(|| format!(
                        "node needs '/container/role', so a hop can be \
                         labelled by the role it lands on: '{stmt}'"
                    ))?;
                let trait_name = trait_name.unwrap_or_else(|| role.clone());
                model.nodes.push(NodeDecl {
                    name,
                    role,
                    trait_name,
                    query: query.trim().to_string(),
                });
            }
            "mount" => {
                let (name, target) = rest.split_once(':').ok_or_else(|| {
                    format!("mount needs 'NAME: target': '{stmt}'")
                })?;
                model.mounts.push(Mount {
                    name: name.trim().to_string(),
                    target: target.trim().to_string(),
                });
            }
            "ref" => {
                let (left, container) = rest.split_once("-->").ok_or_else(|| {
                    format!("ref needs 'PATH::field --> container': '{stmt}'")
                })?;
                let (scope, field) = split_field(left)?;
                let (container, key_field) = parse_ref_target(container)?;
                model.refs.push(RefDecl {
                    scope,
                    field,
                    container,
                    key_field,
                });
            }
            "rel" => {
                let (source, target) = split_arrow(rest).ok_or_else(|| {
                    format!("rel needs 'SOURCE -> TARGET[cond]': '{stmt}'")
                })?;
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
                let (scope_head, pair) = rest.split_once(':').ok_or_else(|| {
                    format!("edge needs 'PATH: ::a -- ::b': '{stmt}'")
                })?;
                let (a, b) = pair.split_once("--").ok_or_else(|| {
                    format!("edge needs two fields 'a -- b': '{stmt}'")
                })?;
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
            other => {
                return Err(format!(
                    "unknown model statement '{other}' \
                     (expected node/ref/rel/edge/mount/def/macro)"
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
        let m = parse_model(
            "def &recent: [::ts > '2026-01-01'];\nnode /who/user: /log/*::user;",
        )
        .unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert!(m.defs_text.contains("def &recent"));
    }

    #[test]
    fn a_semicolon_projection_is_not_a_terminator() {
        // legacy ;;; in a body survives the statement splitter
        let m = parse_model("node /sizes/size: /files/*;;;size;").unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].query, "/files/*;;;size");
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
        let m = parse_model("rel /errors/error -> /deploys/deploy [::at > $$::at];").unwrap();
        assert_eq!(m.rels.len(), 1);
        assert_eq!(m.rels[0].source, "/errors/error");
        assert_eq!(m.rels[0].target, "/deploys/deploy");
        assert_eq!(m.rels[0].cond, "[::at > $$::at]");
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
