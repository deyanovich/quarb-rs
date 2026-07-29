//! The model-file grammar: a definitions file
//! (Section~\ref{sec:fragments}) extended with `node`, `ref`,
//! `edge`, and `mount` statements. Statements terminate with `;`;
//! `#` line comments and `def`/`macro` statements are collected as
//! defs text (prepended to every constructor query so a model's own
//! fragments are visible to its `node` queries).

/// One parsed model file.
#[derive(Debug, Default, Clone)]
pub struct Model {
    /// `mount NAME: target;` — sources the model opens itself.
    pub mounts: Vec<Mount>,
    /// `node NAME: query;` — derived value containers.
    pub nodes: Vec<NodeDecl>,
    /// `ref PATH::field --> container;` — scoped references.
    pub refs: Vec<RefDecl>,
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
    pub name: String,
    pub query: String,
}

#[derive(Debug, Clone)]
pub struct RefDecl {
    /// The node-selecting path (the `::field` suffix stripped).
    pub scope: String,
    /// The property resolved into the container.
    pub field: String,
    pub container: String,
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

/// Strip `#` line comments (a `#` starting a line, after optional
/// whitespace) — the defs-file convention.
fn strip_comments(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
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
                let (name, query) = rest.split_once(':').ok_or_else(|| {
                    format!("node needs 'NAME: query': '{stmt}'")
                })?;
                model.nodes.push(NodeDecl {
                    name: name.trim().to_string(),
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
                model.refs.push(RefDecl {
                    scope,
                    field,
                    container: container.trim().to_string(),
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
                    "unknown model statement '{other}' (expected node/ref/edge/mount/def/macro)"
                ));
            }
        }
    }
    model.defs_text = defs.join("\n");
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
            node ips:     /forum/posts/*::ip;
            node cookies: /forum/posts/*::cookie;
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
        assert_eq!(m.nodes[0].query, "/forum/posts/*::ip");
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
    fn collects_defs_and_comments() {
        let m = parse_model(
            "def &recent: [::ts > '2026-01-01'];\nnode who: /log/*::user;",
        )
        .unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert!(m.defs_text.contains("def &recent"));
    }

    #[test]
    fn a_semicolon_projection_is_not_a_terminator() {
        // legacy ;;; in a body survives the statement splitter
        let m = parse_model("node sizes: /files/*;;;size;").unwrap();
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].query, "/files/*;;;size");
    }

    #[test]
    fn bare_field_ref_is_refused() {
        assert!(parse_model("ref ::ip --> ips;").is_err());
    }
}
