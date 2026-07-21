//! GitLab API adapter for the Quarb query engine.
//!
//! GitLab's defining shape is the **deep tree** — groups nest
//! arbitrarily, and a group's children mix subgroups and
//! projects — which the REST API makes you walk by URL-encoded
//! full paths and numeric ids. The arbor restores it to what it
//! is: `/tesslab/instruments/gauge/issues/7` addresses a project
//! three levels down and its issue in one path, each segment one
//! GET. Under a project: `files` (the default branch's tree,
//! content as the node value), `issues` and `mrs` (the open
//! sets; direct addressing by iid reaches any state),
//! `releases`, and `pipelines` (jobs as children — CI is half of
//! what a GitLab project is).
//!
//! GitLab is about the code, not the social graph, and the edge
//! fabric follows: no follows, stars as a *count* (`::stars`).
//! The relation that matters is **membership** — `->member`
//! edges on groups and projects carry their access level as edge
//! data (`$-::access`, `$-::role`) — plus `parent` on forks
//! (`<-parent` enumerates a project's forks) and `author` /
//! `assignee` / `reviewer` on issues and merge requests.
//!
//! Metadata splits by register: comparable values are properties
//! (`::stars`, `::open-issues`, timestamps as instants); unary
//! facts are traits — visibility (`<private>`), `<archived>`,
//! `<fork>`, project topics, issue/MR labels verbatim (scoped
//! labels included), `<draft>`, `<confidential>`, and
//! pipeline/job statuses (`<failed>`).
//!
//! **Transport and auth**: the adapter shells out to `glab api`,
//! so hosts (`GITLAB_HOST`), tokens, and self-managed instances
//! behave exactly as the glab CLI does. Read-only — only GETs.
//! Target: `gitlab:` (address from the root), or
//! `gitlab:PATH` to anchor a group, project, or user namespace.
//! `QUARB_GLAB` overrides the binary.

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// An error connecting to or reading GitLab.
#[derive(Debug, thiserror::Error)]
pub enum GitlabError {
    #[error("glab: {0}")]
    Glab(String),
    #[error("gitlab target: {0} (expected gitlab:[PATH])")]
    Target(String),
}

/// A project's fixed sections.
#[derive(Clone, Copy, PartialEq)]
enum Sec {
    Files,
    Issues,
    Mrs,
    Releases,
    Pipelines,
}

impl Sec {
    const ALL: [Sec; 5] = [
        Sec::Files,
        Sec::Issues,
        Sec::Mrs,
        Sec::Releases,
        Sec::Pipelines,
    ];
    fn name(self) -> &'static str {
        match self {
            Sec::Files => "files",
            Sec::Issues => "issues",
            Sec::Mrs => "mrs",
            Sec::Releases => "releases",
            Sec::Pipelines => "pipelines",
        }
    }
}

/// What a node is.
enum Kind {
    Root,
    /// A group or subgroup, by full path.
    Group { path: String },
    /// A project, by full path (`path_with_namespace`).
    Project { path: String },
    /// A user (a personal namespace), by username.
    User { username: String },
    Section { proj: String, sec: Sec },
    /// An issue, merge request, or release (told apart by their
    /// JSON: releases carry `tag_name`, MRs `source_branch`).
    Item,
    /// A pipeline; jobs are its children.
    Pipeline { proj: String, id: i64 },
    Job,
    /// A directory in the default branch's tree.
    Dir { proj: String, path: String },
    /// A file (blob); content is the value.
    File { proj: String, path: String },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
    data: RefCell<Option<Rc<Json>>>,
}

/// URL-encode a full path for the API (`a/b/c` → `a%2Fb%2Fc`).
fn enc(path: &str) -> String {
    path.replace('/', "%2F")
}

/// A string value; RFC 3339 timestamps become instants.
fn str_value(s: &str) -> Value {
    if s.contains('T')
        && let Some((secs, nanos, offset_min)) = parse_iso(s)
    {
        return Value::Instant {
            secs,
            nanos,
            offset_min,
        };
    }
    Value::Str(s.to_string())
}

fn json_scalar(v: &Json) -> Option<Value> {
    match v {
        Json::String(s) => Some(str_value(s)),
        Json::Number(n) => Some(match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64()?),
        }),
        Json::Bool(b) => Some(Value::Bool(*b)),
        _ => None,
    }
}

/// Standard base64 (the files API's encoding).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// The role name for a GitLab access level.
fn role_name(level: i64) -> &'static str {
    match level {
        50.. => "owner",
        40..=49 => "maintainer",
        30..=39 => "developer",
        20..=29 => "reporter",
        10..=19 => "guest",
        _ => "minimal",
    }
}

/// GitLab, exposed as an arbor.
pub struct GitlabAdapter {
    glab: String,
    /// The anchor's root children (empty for a bare `gitlab:`).
    anchor: Vec<NodeId>,
    nodes: RefCell<Vec<Node>>,
    groups: RefCell<HashMap<String, NodeId>>,
    projects: RefCell<HashMap<String, NodeId>>,
    users: RefCell<HashMap<String, NodeId>>,
    /// `project!kind!iid-or-tag` → item node.
    items: RefCell<HashMap<String, NodeId>>,
    /// Projects whose full (single-GET) object is cached.
    full: RefCell<HashSet<String>>,
    /// `scope-path@username` → access level (member edge data).
    access: RefCell<HashMap<String, i64>>,
    /// `project:path` → decoded file content.
    contents: RefCell<HashMap<String, Option<String>>>,
}

impl GitlabAdapter {
    /// Connect to `gitlab:` or `gitlab:PATH` (a group, project,
    /// or user namespace to anchor).
    pub fn connect(target: &str) -> Result<Self, GitlabError> {
        let anchor = target
            .strip_prefix("gitlab:")
            .ok_or_else(|| GitlabError::Target(target.to_string()))?;
        let glab = std::env::var("QUARB_GLAB").unwrap_or_else(|_| "glab".to_string());
        let adapter = GitlabAdapter {
            glab,
            anchor: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
                data: RefCell::new(None),
            }]),
            groups: RefCell::new(HashMap::new()),
            projects: RefCell::new(HashMap::new()),
            users: RefCell::new(HashMap::new()),
            items: RefCell::new(HashMap::new()),
            full: RefCell::new(HashSet::new()),
            access: RefCell::new(HashMap::new()),
            contents: RefCell::new(HashMap::new()),
        };
        let mut adapter = adapter;
        if anchor.is_empty() {
            // Probe: auth and reachability surface here.
            adapter.call("version").map_err(|e| {
                GitlabError::Glab(format!("{e} (is `glab auth login` done?)"))
            })?;
        } else {
            let n = adapter
                .fetch_project(anchor)
                .or_else(|| adapter.fetch_group(anchor))
                .or_else(|| adapter.fetch_user(anchor))
                .ok_or_else(|| {
                    GitlabError::Glab(format!("no such project, group, or user: {anchor}"))
                })?;
            adapter.anchor = vec![n];
        }
        Ok(adapter)
    }

    /// A human-readable locator: the gitlab.com-shaped path.
    pub fn locator(&self, node: NodeId) -> String {
        let nodes = self.nodes.borrow();
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            if let Some(name) = &nodes[n.0 as usize].name {
                parts.push(name.clone());
            }
            cur = nodes[n.0 as usize].parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn call(&self, path: &str) -> Result<Json, String> {
        let out = std::process::Command::new(&self.glab)
            .args(["api", path])
            .output()
            .map_err(|e| format!("running {}: {e}", self.glab))?;
        if !out.status.success() {
            return Err(format!(
                "api {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("decoding {path}: {e}"))
    }

    /// A fully paginated array endpoint; first-page failure is an
    /// empty listing, mid-pagination failure fails loud.
    fn call_paged(&self, path: &str) -> Vec<Json> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        for page in 1.. {
            let url = format!("{path}{sep}per_page=100&page={page}");
            let items = match self.call(&url) {
                Ok(Json::Array(a)) => a,
                Ok(_) => break,
                Err(e) if page > 1 => {
                    panic!("gitlab: listing {path} truncated mid-pagination: {e}")
                }
                Err(_) => break,
            };
            let n = items.len();
            out.extend(items);
            if n < 100 {
                break;
            }
        }
        out
    }

    fn push_node(
        &self,
        kind: Kind,
        name: Option<String>,
        parent: Option<NodeId>,
        data: Option<Json>,
    ) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind,
            name,
            parent,
            children: RefCell::new(None),
            data: RefCell::new(data.map(Rc::new)),
        });
        id
    }

    fn data(&self, node: NodeId) -> Option<Rc<Json>> {
        self.nodes.borrow()[node.0 as usize].data.borrow().clone()
    }

    fn set_data(&self, node: NodeId, data: Json) -> Rc<Json> {
        let rc = Rc::new(data);
        *self.nodes.borrow()[node.0 as usize].data.borrow_mut() = Some(rc.clone());
        rc
    }

    /// The group node for a full path, interned once; ancestors
    /// intern as groups too (only a namespace root can be a
    /// user), so locators are whole from any entry point.
    fn group_node(&self, path: &str, seed: Option<&Json>) -> NodeId {
        if let Some(&n) = self.groups.borrow().get(path) {
            return n;
        }
        let (parent, name) = match path.rsplit_once('/') {
            Some((pp, name)) => (self.group_node(pp, None), name),
            None => (NodeId(0), path),
        };
        let n = self.push_node(
            Kind::Group {
                path: path.to_string(),
            },
            Some(name.to_string()),
            Some(parent),
            seed.cloned(),
        );
        self.groups.borrow_mut().insert(path.to_string(), n);
        n
    }

    fn group_data(&self, node: NodeId, path: &str) -> Option<Rc<Json>> {
        if let Some(d) = self.data(node) {
            return Some(d);
        }
        let d = self.call(&format!("groups/{}", enc(path))).ok()?;
        Some(self.set_data(node, d))
    }

    fn fetch_group(&self, path: &str) -> Option<NodeId> {
        let d = self.call(&format!("groups/{}", enc(path))).ok()?;
        let full = d.get("full_path")?.as_str()?.to_string();
        let n = self.group_node(&full, Some(&d));
        self.set_data(n, d);
        Some(n)
    }

    /// The user node for a username, interned once at the root.
    fn user_node(&self, username: &str, seed: Option<&Json>) -> NodeId {
        if let Some(&n) = self.users.borrow().get(username) {
            return n;
        }
        let n = self.push_node(
            Kind::User {
                username: username.to_string(),
            },
            Some(username.to_string()),
            Some(NodeId(0)),
            seed.cloned(),
        );
        self.users.borrow_mut().insert(username.to_string(), n);
        n
    }

    /// A user's data (the lookup endpoint answers by username).
    fn user_data(&self, node: NodeId, username: &str) -> Option<Rc<Json>> {
        if let Some(d) = self.data(node)
            && d.get("id").is_some()
        {
            return Some(d);
        }
        let list = self.call(&format!("users?username={username}")).ok()?;
        let d = list.as_array()?.first()?.clone();
        Some(self.set_data(node, d))
    }

    fn fetch_user(&self, username: &str) -> Option<NodeId> {
        if username.contains('/') {
            return None;
        }
        let n = self.user_node(username, None);
        self.user_data(n, username).map(|_| n)
    }

    /// The project node for a full path; its parent chain is the
    /// namespace (a user for personal projects, groups
    /// otherwise).
    fn project_node(&self, path: &str, seed: Option<&Json>) -> NodeId {
        if let Some(&n) = self.projects.borrow().get(path) {
            return n;
        }
        let (ns, name) = match path.rsplit_once('/') {
            Some((ns, name)) => (ns, name),
            None => ("", path),
        };
        let ns_is_user = seed
            .and_then(|d| d.pointer("/namespace/kind"))
            .and_then(|v| v.as_str())
            == Some("user");
        let parent = if ns.is_empty() {
            NodeId(0)
        } else if ns_is_user && !ns.contains('/') {
            self.user_node(ns, None)
        } else {
            self.group_node(ns, None)
        };
        let n = self.push_node(
            Kind::Project {
                path: path.to_string(),
            },
            Some(name.to_string()),
            Some(parent),
            seed.cloned(),
        );
        self.projects.borrow_mut().insert(path.to_string(), n);
        n
    }

    /// A project's data; `full` forces the single-GET object
    /// (listing objects omit `forked_from_project` and friends).
    fn project_data(&self, node: NodeId, path: &str, full: bool) -> Option<Rc<Json>> {
        if let Some(d) = self.data(node)
            && (!full || self.full.borrow().contains(path))
        {
            return Some(d);
        }
        let d = self.call(&format!("projects/{}", enc(path))).ok()?;
        self.full.borrow_mut().insert(path.to_string());
        Some(self.set_data(node, d))
    }

    fn fetch_project(&self, path: &str) -> Option<NodeId> {
        if !path.contains('/') {
            return None;
        }
        let d = self.call(&format!("projects/{}", enc(path))).ok()?;
        let full = d.get("path_with_namespace")?.as_str()?.to_string();
        let n = self.project_node(&full, Some(&d));
        self.set_data(n, d);
        self.full.borrow_mut().insert(full);
        Some(n)
    }

    fn item_node(&self, key: String, name: String, parent: Option<NodeId>, data: Json) -> NodeId {
        if let Some(&n) = self.items.borrow().get(&key) {
            return n;
        }
        let n = self.push_node(Kind::Item, Some(name), parent, Some(data));
        self.items.borrow_mut().insert(key, n);
        n
    }

    /// A directory listing from the repository tree API.
    fn tree_children(&self, parent: NodeId, proj: &str, path: &str) -> Vec<NodeId> {
        let url = match path.is_empty() {
            true => format!("projects/{}/repository/tree", enc(proj)),
            false => format!("projects/{}/repository/tree?path={}", enc(proj), enc(path)),
        };
        self.call_paged(&url)
            .iter()
            .filter_map(|e| {
                let name = e.get("name")?.as_str()?.to_string();
                let epath = e.get("path")?.as_str()?.to_string();
                let kind = match e.get("type")?.as_str()? {
                    "tree" => Kind::Dir {
                        proj: proj.to_string(),
                        path: epath,
                    },
                    _ => Kind::File {
                        proj: proj.to_string(),
                        path: epath,
                    },
                };
                Some(self.push_node(kind, Some(name), Some(parent), Some(e.clone())))
            })
            .collect()
    }

    /// A file's decoded content (one GET, cached).
    fn file_content(&self, proj: &str, path: &str) -> Option<String> {
        let key = format!("{proj}:{path}");
        if let Some(c) = self.contents.borrow().get(&key) {
            return c.clone();
        }
        let ref_q = self
            .projects
            .borrow()
            .get(proj)
            .copied()
            .and_then(|n| self.data(n))
            .and_then(|d| Some(d.get("default_branch")?.as_str()?.to_string()))
            .unwrap_or_else(|| "HEAD".to_string());
        let content = self
            .call(&format!(
                "projects/{}/repository/files/{}?ref={ref_q}",
                enc(proj),
                enc(path)
            ))
            .ok()
            .and_then(|d| {
                let b64 = d.get("content")?.as_str()?.to_string();
                String::from_utf8(base64_decode(&b64)?).ok()
            });
        self.contents.borrow_mut().insert(key, content.clone());
        content
    }

    /// List a section's items (issues and MRs list the open
    /// sets; direct addressing by iid reaches any state).
    fn section_children(&self, node: NodeId, proj: &str, sec: Sec) -> Vec<NodeId> {
        let p = enc(proj);
        match sec {
            Sec::Files => self.tree_children(node, proj, ""),
            Sec::Issues => self
                .call_paged(&format!("projects/{p}/issues?state=opened"))
                .iter()
                .filter_map(|i| {
                    let iid = i.get("iid")?.as_i64()?;
                    Some(self.item_node(
                        format!("{proj}!i!{iid}"),
                        iid.to_string(),
                        Some(node),
                        i.clone(),
                    ))
                })
                .collect(),
            Sec::Mrs => self
                .call_paged(&format!("projects/{p}/merge_requests?state=opened"))
                .iter()
                .filter_map(|m| {
                    let iid = m.get("iid")?.as_i64()?;
                    Some(self.item_node(
                        format!("{proj}!m!{iid}"),
                        iid.to_string(),
                        Some(node),
                        m.clone(),
                    ))
                })
                .collect(),
            Sec::Releases => self
                .call_paged(&format!("projects/{p}/releases"))
                .iter()
                .filter_map(|r| {
                    let tag = r.get("tag_name")?.as_str()?.to_string();
                    Some(self.item_node(
                        format!("{proj}!r!{tag}"),
                        tag,
                        Some(node),
                        r.clone(),
                    ))
                })
                .collect(),
            Sec::Pipelines => self
                .call_paged(&format!("projects/{p}/pipelines"))
                .iter()
                .filter_map(|pl| {
                    let id = pl.get("id")?.as_i64()?;
                    Some(self.push_node(
                        Kind::Pipeline {
                            proj: proj.to_string(),
                            id,
                        },
                        Some(id.to_string()),
                        Some(node),
                        Some(pl.clone()),
                    ))
                })
                .collect(),
        }
    }

    fn author_of(&self, data: &Json) -> Option<NodeId> {
        let u = data.get("author")?;
        let username = u.get("username")?.as_str()?;
        Some(self.user_node(username, Some(u)))
    }

    /// Member edges for a group or project, recording access
    /// levels as edge data.
    fn member_links(&self, scope_api: &str, scope_path: &str) -> Vec<(String, NodeId)> {
        self.call_paged(&format!("{scope_api}/{}/members", enc(scope_path)))
            .iter()
            .filter_map(|m| {
                let username = m.get("username")?.as_str()?;
                if let Some(level) = m.get("access_level").and_then(|v| v.as_i64()) {
                    self.access
                        .borrow_mut()
                        .insert(format!("{scope_path}@{username}"), level);
                }
                Some(("member".to_string(), self.user_node(username, Some(m))))
            })
            .collect()
    }
}

impl AstAdapter for GitlabAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if let Some(c) = self.nodes.borrow()[node.0 as usize]
            .children
            .borrow()
            .as_ref()
        {
            return c.clone();
        }
        enum Plan {
            Root,
            Group(String),
            User(String),
            Project(String),
            Section(String, Sec),
            Pipeline(String, i64),
            Dir(String, String),
            Leaf,
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::Group { path } => Plan::Group(path.clone()),
            Kind::User { username } => Plan::User(username.clone()),
            Kind::Project { path } => Plan::Project(path.clone()),
            Kind::Section { proj, sec } => Plan::Section(proj.clone(), *sec),
            Kind::Pipeline { proj, id } => Plan::Pipeline(proj.clone(), *id),
            Kind::Dir { proj, path } => Plan::Dir(proj.clone(), path.clone()),
            Kind::Item | Kind::Job | Kind::File { .. } => Plan::Leaf,
        };
        let ids = match plan {
            // The root does not enumerate GitLab; an anchored
            // target roots its entity.
            Plan::Root => self.anchor.clone(),
            // A group's children mix subgroups and projects,
            // each sorted by name.
            Plan::Group(path) => {
                let p = enc(&path);
                let mut subs: Vec<(String, Json)> = self
                    .call_paged(&format!("groups/{p}/subgroups?all_available=true"))
                    .into_iter()
                    .filter_map(|g| {
                        Some((g.get("full_path")?.as_str()?.to_string(), g))
                    })
                    .collect();
                subs.sort_by(|a, b| a.0.cmp(&b.0));
                let mut projs: Vec<(String, Json)> = self
                    .call_paged(&format!("groups/{p}/projects"))
                    .into_iter()
                    .filter_map(|pr| {
                        Some((pr.get("path_with_namespace")?.as_str()?.to_string(), pr))
                    })
                    .collect();
                projs.sort_by(|a, b| a.0.cmp(&b.0));
                let mut ids: Vec<NodeId> = subs
                    .iter()
                    .map(|(fp, g)| {
                        let n = self.group_node(fp, Some(g));
                        self.set_data(n, g.clone());
                        n
                    })
                    .collect();
                ids.extend(projs.iter().map(|(fp, pr)| self.project_node(fp, Some(pr))));
                ids
            }
            Plan::User(username) => {
                let Some(d) = self.users.borrow().get(&username).copied() else {
                    return Vec::new();
                };
                let Some(data) = self.user_data(d, &username) else {
                    return Vec::new();
                };
                let Some(id) = data.get("id").and_then(|v| v.as_i64()) else {
                    return Vec::new();
                };
                let mut projs: Vec<(String, Json)> = self
                    .call_paged(&format!("users/{id}/projects"))
                    .into_iter()
                    .filter_map(|pr| {
                        Some((pr.get("path_with_namespace")?.as_str()?.to_string(), pr))
                    })
                    .collect();
                projs.sort_by(|a, b| a.0.cmp(&b.0));
                projs
                    .iter()
                    .map(|(fp, pr)| self.project_node(fp, Some(pr)))
                    .collect()
            }
            Plan::Project(path) => Sec::ALL
                .iter()
                .map(|&sec| {
                    self.push_node(
                        Kind::Section {
                            proj: path.clone(),
                            sec,
                        },
                        Some(sec.name().to_string()),
                        Some(node),
                        None,
                    )
                })
                .collect(),
            Plan::Section(proj, sec) => self.section_children(node, &proj, sec),
            Plan::Pipeline(proj, id) => self
                .call_paged(&format!("projects/{}/pipelines/{id}/jobs", enc(&proj)))
                .iter()
                .filter_map(|j| {
                    let name = j.get("name")?.as_str()?.to_string();
                    Some(self.push_node(Kind::Job, Some(name), Some(node), Some(j.clone())))
                })
                .collect(),
            Plan::Dir(proj, path) => self.tree_children(node, &proj, &path),
            Plan::Leaf => Vec::new(),
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    /// Direct addressing, one GET per segment: a group or user
    /// at the root, a subgroup or project under a group, an
    /// issue/MR by iid (any state), a pipeline by id.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        enum Plan {
            Root,
            Group(String),
            User(String),
            Section(String, Sec),
            Other,
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::Group { path } => Plan::Group(path.clone()),
            Kind::User { username } => Plan::User(username.clone()),
            Kind::Section {
                proj,
                sec: sec @ (Sec::Issues | Sec::Mrs | Sec::Pipelines),
            } => Plan::Section(proj.clone(), *sec),
            _ => Plan::Other,
        };
        match plan {
            Plan::Root => {
                if let Some(&n) = self.groups.borrow().get(name) {
                    return vec![n];
                }
                if let Some(&n) = self.users.borrow().get(name) {
                    return vec![n];
                }
                self.fetch_group(name)
                    .or_else(|| self.fetch_user(name))
                    .into_iter()
                    .collect()
            }
            Plan::Group(path) => {
                let child = format!("{path}/{name}");
                if let Some(&n) = self.projects.borrow().get(&child) {
                    return vec![n];
                }
                if let Some(&n) = self.groups.borrow().get(&child) {
                    return vec![n];
                }
                self.fetch_project(&child)
                    .or_else(|| self.fetch_group(&child))
                    .into_iter()
                    .collect()
            }
            Plan::User(username) => {
                let child = format!("{username}/{name}");
                if let Some(&n) = self.projects.borrow().get(&child) {
                    return vec![n];
                }
                self.fetch_project(&child).into_iter().collect()
            }
            Plan::Section(proj, sec) => {
                let Ok(num) = name.parse::<i64>() else {
                    return Vec::new();
                };
                let (tag, api) = match sec {
                    Sec::Mrs => ('m', "merge_requests"),
                    Sec::Pipelines => ('p', "pipelines"),
                    _ => ('i', "issues"),
                };
                if sec == Sec::Pipelines {
                    let Ok(d) = self.call(&format!(
                        "projects/{}/pipelines/{num}",
                        enc(&proj)
                    )) else {
                        return Vec::new();
                    };
                    return vec![self.push_node(
                        Kind::Pipeline {
                            proj: proj.clone(),
                            id: num,
                        },
                        Some(num.to_string()),
                        Some(node),
                        Some(d),
                    )];
                }
                let key = format!("{proj}!{tag}!{num}");
                if let Some(&n) = self.items.borrow().get(&key) {
                    return vec![n];
                }
                let Ok(d) = self.call(&format!("projects/{}/{api}/{num}", enc(&proj))) else {
                    return Vec::new();
                };
                vec![self.item_node(key, num.to_string(), Some(node), d)]
            }
            Plan::Other => self
                .children(node)
                .into_iter()
                .filter(|&c| self.name(c).as_deref() == Some(name))
                .collect(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// Kinds, visibility, flags, topics, labels (scoped labels
    /// included), and pipeline/job statuses, all as traits.
    fn traits(&self, node: NodeId) -> Vec<String> {
        enum Shape {
            Fixed(&'static str),
            Group,
            Project,
            Item,
            Status(&'static str),
            Entry(&'static str),
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => return Vec::new(),
            Kind::Section { .. } => Shape::Fixed("section"),
            Kind::User { .. } => Shape::Fixed("user"),
            Kind::Group { .. } => Shape::Group,
            Kind::Project { .. } => Shape::Project,
            Kind::Item => Shape::Item,
            Kind::Pipeline { .. } => Shape::Status("pipeline"),
            Kind::Job => Shape::Status("job"),
            Kind::Dir { .. } => Shape::Entry("dir"),
            Kind::File { .. } => Shape::Entry("file"),
        };
        let data = self.data(node);
        let d = data.as_deref();
        let s = |k: &str| {
            d.and_then(|d| d.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let flag = |k: &str| d.and_then(|d| d.get(k)).and_then(|v| v.as_bool()) == Some(true);
        match shape {
            Shape::Fixed(t) => vec![t.to_string()],
            Shape::Group => {
                let mut ts = vec!["group".to_string()];
                ts.extend(s("visibility"));
                ts
            }
            Shape::Project => {
                // "project" is GitLab lingo; the rest of the
                // world says repository, so <repo> matches too —
                // //*<repo> sweeps both forges identically.
                let mut ts = vec!["project".to_string(), "repo".to_string()];
                ts.extend(s("visibility"));
                if flag("archived") {
                    ts.push("archived".to_string());
                }
                if d.is_some_and(|d| d.get("forked_from_project").is_some_and(|v| !v.is_null())) {
                    ts.push("fork".to_string());
                }
                if let Some(topics) = d.and_then(|d| d.get("topics")).and_then(|v| v.as_array()) {
                    ts.extend(topics.iter().filter_map(|t| t.as_str().map(str::to_string)));
                }
                ts
            }
            Shape::Item => {
                let mut ts = Vec::new();
                let kind = if d.is_some_and(|d| d.get("tag_name").is_some()) {
                    "release"
                } else if d.is_some_and(|d| d.get("source_branch").is_some()) {
                    "mr"
                } else {
                    "issue"
                };
                ts.push(kind.to_string());
                ts.extend(s("state"));
                if flag("draft") {
                    ts.push("draft".to_string());
                }
                if flag("confidential") {
                    ts.push("confidential".to_string());
                }
                if flag("upcoming_release") {
                    ts.push("upcoming".to_string());
                }
                if let Some(labels) = d.and_then(|d| d.get("labels")).and_then(|v| v.as_array()) {
                    ts.extend(labels.iter().filter_map(|l| l.as_str().map(str::to_string)));
                }
                ts
            }
            Shape::Status(kind) => {
                let mut ts = vec![kind.to_string()];
                ts.extend(s("status"));
                ts
            }
            Shape::Entry(t) => {
                // A submodule entry is type "commit" in the tree.
                if d.and_then(|d| d.get("type")).and_then(|v| v.as_str()) == Some("commit") {
                    return vec!["submodule".to_string()];
                }
                vec![t.to_string()]
            }
        }
    }

    /// Curated scalars per kind; timestamps as instants,
    /// `::topics` and `::labels` as lists.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        enum Shape {
            Group(String),
            Project(String),
            User(String),
            Data,
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Group { path } => Shape::Group(path.clone()),
            Kind::Project { path } => Shape::Project(path.clone()),
            Kind::User { username } => Shape::User(username.clone()),
            Kind::Item | Kind::Pipeline { .. } | Kind::Job | Kind::File { .. } => Shape::Data,
            _ => return None,
        };
        let at = |d: &Json, ptr: &str| d.pointer(ptr).and_then(json_scalar);
        match shape {
            Shape::Group(path) => {
                let d = self.group_data(node, &path)?;
                let key = match name {
                    "name" => "/name",
                    "path" => "/path",
                    "full-path" => "/full_path",
                    "description" => "/description",
                    "visibility" => "/visibility",
                    "created" => "/created_at",
                    _ => return None,
                };
                at(&d, key)
            }
            Shape::Project(path) => {
                let full = matches!(name, "parent" | "stars" | "forks" | "open-issues");
                let d = self.project_data(node, &path, full)?;
                match name {
                    "topics" => {
                        let ts = d.get("topics")?.as_array()?;
                        Some(Value::List(
                            ts.iter()
                                .filter_map(|t| Some(Value::Str(t.as_str()?.to_string())))
                                .collect(),
                        ))
                    }
                    "parent" => at(&d, "/forked_from_project/path_with_namespace"),
                    _ => {
                        let key = match name {
                            "name" => "/name",
                            "path" => "/path",
                            "full-path" => "/path_with_namespace",
                            "description" => "/description",
                            "stars" => "/star_count",
                            "forks" => "/forks_count",
                            "open-issues" => "/open_issues_count",
                            "default-branch" => "/default_branch",
                            "visibility" => "/visibility",
                            "created" => "/created_at",
                            "activity" => "/last_activity_at",
                            _ => return None,
                        };
                        at(&d, key)
                    }
                }
            }
            Shape::User(username) => {
                let d = self.user_data(node, &username)?;
                let key = match name {
                    "username" => "/username",
                    "name" => "/name",
                    "state" => "/state",
                    _ => return None,
                };
                at(&d, key)
            }
            Shape::Data => {
                let d = self.data(node)?;
                match name {
                    "labels" => {
                        let ls = d.get("labels")?.as_array()?;
                        Some(Value::List(
                            ls.iter()
                                .filter_map(|l| Some(Value::Str(l.as_str()?.to_string())))
                                .collect(),
                        ))
                    }
                    "author" => at(&d, "/author/username"),
                    "milestone" => at(&d, "/milestone/title"),
                    _ => {
                        let key = match name {
                            "iid" => "/iid",
                            "title" => "/title",
                            "state" => "/state",
                            "comments" => "/user_notes_count",
                            "upvotes" => "/upvotes",
                            "weight" => "/weight",
                            "created" => "/created_at",
                            "updated" => "/updated_at",
                            "closed" => "/closed_at",
                            "merged" => "/merged_at",
                            "source-branch" => "/source_branch",
                            "target-branch" => "/target_branch",
                            "sha" => "/sha",
                            "tag" => "/tag_name",
                            "name" => "/name",
                            "released" => "/released_at",
                            "status" => "/status",
                            "ref" => "/ref",
                            "source" => "/source",
                            "duration" => "/duration",
                            "stage" => "/stage",
                            "size" => "/size",
                            _ => return None,
                        };
                        at(&d, key)
                    }
                }
            }
        }
    }

    /// A file's decoded content; an issue's, MR's, or release's
    /// description text.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (proj, path) = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::File { proj, path } => (proj.clone(), path.clone()),
            Kind::Item => {
                let d = self.data(node)?;
                let body = d.get("description")?.as_str()?;
                return Some(Value::Str(body.to_string()));
            }
            _ => return None,
        };
        self.file_content(&proj, &path).map(Value::Str)
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "url" => {
                let d = self.data(node)?;
                Some(Value::Str(d.get("web_url")?.as_str()?.to_string()))
            }
            _ => None,
        }
    }

    /// `::parent~>` (a fork's upstream), `::author~>`.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let proj = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Project { path } => Some(path.clone()),
            Kind::Item => None,
            _ => return None,
        };
        match (proj, property) {
            (Some(path), "parent") => {
                let d = self.project_data(node, &path, true)?;
                let pkey = d
                    .pointer("/forked_from_project/path_with_namespace")?
                    .as_str()?
                    .to_string();
                Some(self.project_node(&pkey, d.get("forked_from_project")))
            }
            (None, "author") => {
                let d = self.data(node)?;
                self.author_of(&d)
            }
            _ => None,
        }
    }

    /// `member` edges (with access-level edge data) from groups
    /// and projects; `parent` from forks; `author` / `assignee` /
    /// `reviewer` from issues and merge requests.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        enum Shape {
            Group(String),
            Project(String),
            Item,
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Group { path } => Shape::Group(path.clone()),
            Kind::Project { path } => Shape::Project(path.clone()),
            Kind::Item => Shape::Item,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        match shape {
            Shape::Group(path) => out.extend(self.member_links("groups", &path)),
            Shape::Project(path) => {
                if let Some(n) = self.resolve(node, "parent", None) {
                    out.push(("parent".to_string(), n));
                }
                out.extend(self.member_links("projects", &path));
            }
            Shape::Item => {
                let Some(d) = self.data(node) else {
                    return out;
                };
                if let Some(n) = self.author_of(&d) {
                    out.push(("author".to_string(), n));
                }
                for key in ["assignees", "reviewers"] {
                    let label = key.trim_end_matches('s');
                    if let Some(us) = d.get(key).and_then(|v| v.as_array()) {
                        for u in us {
                            if let Some(username) = u.get("username").and_then(|v| v.as_str()) {
                                out.push((label.to_string(), self.user_node(username, Some(u))));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// A project's forks: `<-parent`.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Project { path } => path.clone(),
            _ => return Vec::new(),
        };
        self.call_paged(&format!("projects/{}/forks", enc(&path)))
            .iter()
            .filter_map(|f| {
                let fp = f.get("path_with_namespace")?.as_str()?;
                Some(("parent".to_string(), self.project_node(fp, Some(f))))
            })
            .collect()
    }

    /// The member edge carries its access level:
    /// `$-::access` (the numeric level) and `$-::role`.
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        if label != "member" || !matches!(name, "access" | "role") {
            return None;
        }
        let scope = match &self.nodes.borrow()[source.0 as usize].kind {
            Kind::Group { path } | Kind::Project { path } => path.clone(),
            _ => return None,
        };
        let username = match &self.nodes.borrow()[target.0 as usize].kind {
            Kind::User { username } => username.clone(),
            _ => return None,
        };
        let level = *self.access.borrow().get(&format!("{scope}@{username}"))?;
        Some(match name {
            "access" => Value::Int(level),
            _ => Value::Str(role_name(level).to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_encode() {
        assert_eq!(enc("tesslab/instruments/gauge"), "tesslab%2Finstruments%2Fgauge");
    }

    #[test]
    fn roles_map() {
        assert_eq!(role_name(50), "owner");
        assert_eq!(role_name(40), "maintainer");
        assert_eq!(role_name(30), "developer");
        assert_eq!(role_name(20), "reporter");
        assert_eq!(role_name(10), "guest");
        assert_eq!(role_name(5), "minimal");
    }

    #[test]
    fn timestamps_become_instants() {
        assert!(matches!(
            str_value("2026-07-20T12:00:00.000Z"),
            Value::Instant { .. }
        ));
        assert!(matches!(str_value("v1.0"), Value::Str(_)));
    }

    #[test]
    fn target_needs_the_scheme() {
        assert!(matches!(
            GitlabAdapter::connect("gl:tesslab"),
            Err(GitlabError::Target(_))
        ));
    }
}
