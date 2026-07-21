//! GitHub API adapter for the Quarb query engine.
//!
//! GitHub is three shapes at once, and the arbor keeps each in
//! its native register. The **tree** mirrors github.com URLs:
//! users and organizations at the root (addressed by login —
//! the root does not enumerate GitHub), their repositories as
//! direct children (`/torvalds/linux`), and each repository's
//! sections below (`files`, `issues`, `pulls`, `releases`).
//! File content, issue bodies, and release notes are node
//! *values*. The **social graph** is edge fabric, every label
//! API-backed in both directions: `follows` (`<-follows` are
//! the followers), `starred` (`<-starred` the stargazers),
//! `owner`, `parent` (`<-parent` the forks), `author` /
//! `assignee`, `org` (`<-org` the members), and `contributor`
//! — whose edge carries data: `$-::contributions`. The
//! **metadata** splits by kind: counts and scalars are
//! properties (`::stars`, `::language`, timestamps as
//! instants), while boolean flags (`<fork>`, `<archived>`,
//! `<open>`, `<merged>`), issue labels (`<bug>`), and repo
//! topics (`<cli>`) are traits — unary facts filter, values
//! compare.
//!
//! Direct addressing skips enumeration: `/rust-lang/rust` is
//! one GET, `/rust-lang/rust/issues/12345` another (closed
//! issues included — enumeration lists open ones, the API's
//! default). Everything loads lazily and is cached for the
//! session; listing costs one paginated sweep per touched
//! container. Read-only — the adapter only ever GETs.
//!
//! **Transport and auth**: the adapter shells out to `gh api`,
//! so authentication, hosts, and rate limits behave exactly as
//! the gh CLI does. Target: `github:` (whole GitHub, address
//! from the root), `github:OWNER`, or `github:OWNER/REPO` to
//! anchor the arbor. `QUARB_GH` overrides the binary.

use quarb::temporal::parse_iso;
use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// An error connecting to or reading GitHub.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("gh: {0}")]
    Gh(String),
    #[error("github target: {0} (expected github:[OWNER[/REPO]])")]
    Target(String),
}

/// A repository's fixed sections.
#[derive(Clone, Copy, PartialEq)]
enum Sec {
    Files,
    Issues,
    Pulls,
    Releases,
}

impl Sec {
    const ALL: [Sec; 4] = [Sec::Files, Sec::Issues, Sec::Pulls, Sec::Releases];
    fn name(self) -> &'static str {
        match self {
            Sec::Files => "files",
            Sec::Issues => "issues",
            Sec::Pulls => "pulls",
            Sec::Releases => "releases",
        }
    }
}

/// What a node is. Entity JSON hangs off the node (lazily
/// completed for users and repositories, whose edge-listing
/// appearances are partial objects).
enum Kind {
    Root,
    /// A user or organization, by login.
    User { login: String },
    /// A repository; `key` is `owner/name`.
    Repo { key: String },
    Section { repo: String, sec: Sec },
    /// An issue, pull request, or release (told apart by their
    /// JSON: releases carry `tag_name`, pulls `head` or
    /// `pull_request`).
    Item,
    /// A directory in a repository's default-branch tree.
    Dir { repo: String, path: String },
    /// A file (or symlink/submodule); content is the value.
    File { repo: String, path: String },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
    data: RefCell<Option<Rc<Json>>>,
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

/// Standard base64 (the contents API's encoding; padding
/// required, whitespace ignored).
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

/// GitHub, exposed as an arbor.
pub struct GithubAdapter {
    gh: String,
    /// The anchor's root children (empty for a bare `github:`).
    anchor: Vec<NodeId>,
    nodes: RefCell<Vec<Node>>,
    users: RefCell<HashMap<String, NodeId>>,
    repos: RefCell<HashMap<String, NodeId>>,
    /// `owner/name#number` → issue/pull node.
    items: RefCell<HashMap<String, NodeId>>,
    /// `owner/name@login` → contributions (edge data).
    contributions: RefCell<HashMap<String, i64>>,
    /// `owner/name:path` → decoded file content.
    contents: RefCell<HashMap<String, Option<String>>>,
}

impl GithubAdapter {
    /// Connect to `github:`, `github:OWNER`, or
    /// `github:OWNER/REPO`.
    pub fn connect(target: &str) -> Result<Self, GithubError> {
        let anchor = target
            .strip_prefix("github:")
            .ok_or_else(|| GithubError::Target(target.to_string()))?;
        let gh = std::env::var("QUARB_GH").unwrap_or_else(|_| "gh".to_string());
        let adapter = GithubAdapter {
            gh,
            anchor: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
                data: RefCell::new(None),
            }]),
            users: RefCell::new(HashMap::new()),
            repos: RefCell::new(HashMap::new()),
            items: RefCell::new(HashMap::new()),
            contributions: RefCell::new(HashMap::new()),
            contents: RefCell::new(HashMap::new()),
        };
        let mut adapter = adapter;
        match anchor.split_once('/') {
            None if anchor.is_empty() => {
                // Probe: auth and reachability surface here.
                adapter.call("/rate_limit").map_err(|e| {
                    GithubError::Gh(format!("{e} (is `gh auth login` done?)"))
                })?;
            }
            None => {
                let u = adapter
                    .fetch_user(anchor)
                    .ok_or_else(|| GithubError::Gh(format!("no such user: {anchor}")))?;
                adapter.anchor = vec![u];
            }
            Some((owner, repo)) => {
                let key = format!("{owner}/{repo}");
                let r = adapter
                    .fetch_repo(&key)
                    .ok_or_else(|| GithubError::Gh(format!("no such repository: {key}")))?;
                adapter.anchor = vec![r];
            }
        }
        Ok(adapter)
    }

    /// A human-readable locator: the github.com-shaped path.
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
        let out = std::process::Command::new(&self.gh)
            .args(["api", path])
            .output()
            .map_err(|e| format!("running {}: {e}", self.gh))?;
        if !out.status.success() {
            return Err(format!(
                "api {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("decoding {path}: {e}"))
    }

    /// A fully paginated array endpoint. A failure on the first
    /// page is an empty listing (the `children` contract); a
    /// failure mid-pagination fails loud rather than cache a
    /// silently-short answer.
    fn call_paged(&self, path: &str) -> Vec<Json> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        for page in 1.. {
            let url = format!("{path}{sep}per_page=100&page={page}");
            let items = match self.call(&url) {
                Ok(Json::Array(a)) => a,
                Ok(_) => break,
                Err(e) if page > 1 => {
                    panic!("github: listing {path} truncated mid-pagination: {e}")
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

    /// The user node for a login, interned once; `seed` is a
    /// possibly-partial object from an edge listing.
    fn user_node(&self, login: &str, seed: Option<&Json>) -> NodeId {
        if let Some(&n) = self.users.borrow().get(login) {
            return n;
        }
        let n = self.push_node(
            Kind::User {
                login: login.to_string(),
            },
            Some(login.to_string()),
            Some(NodeId(0)),
            seed.cloned(),
        );
        self.users.borrow_mut().insert(login.to_string(), n);
        n
    }

    /// A user's data, completed to the full profile when the
    /// cached object is a partial edge-listing one (partials
    /// lack `created_at`).
    fn user_data(&self, node: NodeId, login: &str) -> Option<Rc<Json>> {
        if let Some(d) = self.data(node)
            && d.get("created_at").is_some()
        {
            return Some(d);
        }
        let d = self.call(&format!("/users/{login}")).ok()?;
        Some(self.set_data(node, d))
    }

    fn fetch_user(&self, login: &str) -> Option<NodeId> {
        let n = self.user_node(login, None);
        self.user_data(n, login).map(|_| n)
    }

    /// The repo node for `owner/name`, interned once.
    fn repo_node(&self, key: &str, seed: Option<&Json>) -> NodeId {
        if let Some(&n) = self.repos.borrow().get(key) {
            return n;
        }
        let (owner, name) = key.split_once('/').unwrap_or((key, key));
        let parent = self.user_node(owner, None);
        let n = self.push_node(
            Kind::Repo {
                key: key.to_string(),
            },
            Some(name.to_string()),
            Some(parent),
            seed.cloned(),
        );
        self.repos.borrow_mut().insert(key.to_string(), n);
        n
    }

    /// A repo's data; refetched singly when a needed field is
    /// missing from a listing object (`full` forces that).
    fn repo_data(&self, node: NodeId, key: &str, full: bool) -> Option<Rc<Json>> {
        if let Some(d) = self.data(node)
            && (!full || d.get("subscribers_count").is_some())
        {
            return Some(d);
        }
        let d = self.call(&format!("/repos/{key}")).ok()?;
        Some(self.set_data(node, d))
    }

    fn fetch_repo(&self, key: &str) -> Option<NodeId> {
        let n = self.repo_node(key, None);
        self.repo_data(n, key, true).map(|_| n)
    }

    /// An issue/pull/release node, interned by a repo-scoped key.
    fn item_node(&self, key: String, name: String, parent: Option<NodeId>, data: Json) -> NodeId {
        if let Some(&n) = self.items.borrow().get(&key) {
            return n;
        }
        let n = self.push_node(Kind::Item, Some(name), parent, Some(data));
        self.items.borrow_mut().insert(key, n);
        n
    }

    fn section_node(&self, repo_node: NodeId, repo: &str, sec: Sec) -> NodeId {
        self.push_node(
            Kind::Section {
                repo: repo.to_string(),
                sec,
            },
            Some(sec.name().to_string()),
            Some(repo_node),
            None,
        )
    }

    /// A directory listing (the contents API), as Dir/File nodes.
    fn dir_children(&self, parent: NodeId, repo: &str, path: &str) -> Vec<NodeId> {
        let url = match path.is_empty() {
            true => format!("/repos/{repo}/contents/"),
            false => format!("/repos/{repo}/contents/{path}"),
        };
        let Ok(Json::Array(entries)) = self.call(&url) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|e| {
                let name = e.get("name")?.as_str()?.to_string();
                let epath = e.get("path")?.as_str()?.to_string();
                let kind = match e.get("type")?.as_str()? {
                    "dir" => Kind::Dir {
                        repo: repo.to_string(),
                        path: epath,
                    },
                    _ => Kind::File {
                        repo: repo.to_string(),
                        path: epath,
                    },
                };
                Some(self.push_node(kind, Some(name), Some(parent), Some(e.clone())))
            })
            .collect()
    }

    /// A file's decoded content (one GET, cached; `None` for
    /// binary or oversized files).
    fn file_content(&self, repo: &str, path: &str) -> Option<String> {
        let key = format!("{repo}:{path}");
        if let Some(c) = self.contents.borrow().get(&key) {
            return c.clone();
        }
        let content = self
            .call(&format!("/repos/{repo}/contents/{path}"))
            .ok()
            .and_then(|d| {
                let b64 = d.get("content")?.as_str()?.to_string();
                String::from_utf8(base64_decode(&b64)?).ok()
            });
        self.contents.borrow_mut().insert(key, content.clone());
        content
    }

    /// List a section's items (issues and pulls list the open
    /// ones — the API's default; direct addressing reaches any).
    fn section_children(&self, node: NodeId, repo: &str, sec: Sec) -> Vec<NodeId> {
        match sec {
            Sec::Files => self.dir_children(node, repo, ""),
            Sec::Issues => self
                .call_paged(&format!("/repos/{repo}/issues"))
                .iter()
                // The issues listing interleaves pull requests.
                .filter(|i| i.get("pull_request").is_none())
                .filter_map(|i| {
                    let num = i.get("number")?.as_i64()?;
                    Some(self.item_node(
                        format!("{repo}#{num}"),
                        num.to_string(),
                        Some(node),
                        i.clone(),
                    ))
                })
                .collect(),
            Sec::Pulls => self
                .call_paged(&format!("/repos/{repo}/pulls"))
                .iter()
                .filter_map(|i| {
                    let num = i.get("number")?.as_i64()?;
                    Some(self.item_node(
                        format!("{repo}#{num}"),
                        num.to_string(),
                        Some(node),
                        i.clone(),
                    ))
                })
                .collect(),
            Sec::Releases => self
                .call_paged(&format!("/repos/{repo}/releases"))
                .iter()
                .filter_map(|r| {
                    let tag = r.get("tag_name")?.as_str()?.to_string();
                    Some(self.item_node(
                        format!("{repo}@{tag}"),
                        tag,
                        Some(node),
                        r.clone(),
                    ))
                })
                .collect(),
        }
    }

    /// The item's author edge target.
    fn author_of(&self, data: &Json) -> Option<NodeId> {
        let u = data.get("user").or_else(|| data.get("author"))?;
        let login = u.get("login")?.as_str()?;
        Some(self.user_node(login, Some(u)))
    }
}

impl AstAdapter for GithubAdapter {
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
            User(String),
            Repo(String),
            Section(String, Sec),
            Dir(String, String),
            Leaf,
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::User { login } => Plan::User(login.clone()),
            Kind::Repo { key } => Plan::Repo(key.clone()),
            Kind::Section { repo, sec } => Plan::Section(repo.clone(), *sec),
            Kind::Dir { repo, path } => Plan::Dir(repo.clone(), path.clone()),
            Kind::Item | Kind::File { .. } => Plan::Leaf,
        };
        let ids = match plan {
            // The root does not enumerate GitHub; an anchored
            // target roots its entity here.
            Plan::Root => self.anchor.clone(),
            Plan::User(login) => self
                .call_paged(&format!("/users/{login}/repos"))
                .iter()
                .filter_map(|r| {
                    let key = r.get("full_name")?.as_str()?;
                    Some(self.repo_node(key, Some(r)))
                })
                .collect(),
            Plan::Repo(key) => Sec::ALL
                .iter()
                .map(|&sec| self.section_node(node, &key, sec))
                .collect(),
            Plan::Section(repo, sec) => self.section_children(node, &repo, sec),
            Plan::Dir(repo, path) => self.dir_children(node, &repo, &path),
            Plan::Leaf => Vec::new(),
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    /// Direct addressing: a login at the root, a repository
    /// under its owner, an issue or pull by number (closed ones
    /// included) — one GET each, no enumeration.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        enum Plan {
            Root,
            User(String),
            Section(String, Sec),
            Other,
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::User { login } => Plan::User(login.clone()),
            Kind::Section { repo, sec: sec @ (Sec::Issues | Sec::Pulls) } => {
                Plan::Section(repo.clone(), *sec)
            }
            _ => Plan::Other,
        };
        match plan {
            Plan::Root => {
                if let Some(&n) = self.users.borrow().get(name) {
                    return vec![n];
                }
                self.fetch_user(name).into_iter().collect()
            }
            Plan::User(login) => {
                let key = format!("{login}/{name}");
                if let Some(&n) = self.repos.borrow().get(&key) {
                    return vec![n];
                }
                self.fetch_repo(&key).into_iter().collect()
            }
            Plan::Section(repo, sec) => {
                let Ok(num) = name.parse::<i64>() else {
                    return Vec::new();
                };
                if let Some(&n) = self.items.borrow().get(&format!("{repo}#{num}")) {
                    return vec![n];
                }
                let path = match sec {
                    Sec::Pulls => format!("/repos/{repo}/pulls/{num}"),
                    _ => format!("/repos/{repo}/issues/{num}"),
                };
                let Ok(d) = self.call(&path) else {
                    return Vec::new();
                };
                vec![self.item_node(
                    format!("{repo}#{num}"),
                    num.to_string(),
                    Some(node),
                    d,
                )]
            }
            Plan::Other => {
                // Default: enumerate and filter by name.
                self.children(node)
                    .into_iter()
                    .filter(|&c| self.name(c).as_deref() == Some(name))
                    .collect()
            }
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// Type traits (`<user>`, `<repo>`, `<issue>`…), boolean
    /// flags (`<fork>`, `<archived>`, `<open>`, `<merged>`,
    /// `<draft>`, `<prerelease>`), issue labels verbatim
    /// (`<bug>`), and repository topics (`<cli>`).
    fn traits(&self, node: NodeId) -> Vec<String> {
        enum Shape {
            Fixed(&'static str),
            User(String),
            Repo,
            Item,
            File,
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => return Vec::new(),
            Kind::Section { .. } => Shape::Fixed("section"),
            Kind::Dir { .. } => Shape::Fixed("dir"),
            Kind::User { login } => Shape::User(login.clone()),
            Kind::Repo { .. } => Shape::Repo,
            Kind::Item => Shape::Item,
            Kind::File { .. } => Shape::File,
        };
        let data = self.data(node);
        let d = data.as_deref();
        let flag = |k: &str| d.and_then(|d| d.get(k)).and_then(|v| v.as_bool()) == Some(true);
        let set = |k: &str| d.and_then(|d| d.get(k)).map(|v| !v.is_null()) == Some(true);
        match shape {
            Shape::Fixed(t) => vec![t.to_string()],
            Shape::User(login) => {
                // The type rides on even partial objects; fetch
                // only if there is no object at all.
                let t = d
                    .and_then(|d| d.get("type"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        let n = *self.users.borrow().get(&login)?;
                        let d = self.user_data(n, &login)?;
                        Some(d.get("type")?.as_str()?.to_string())
                    })
                    .unwrap_or_else(|| "User".to_string());
                // An organization also answers <org> (the
                // universal shorthand) and the forge-neutral
                // <group> (GitLab's native word), so container
                // filters read the same on both forges.
                match t.as_str() {
                    "Organization" => vec![
                        "organization".to_string(),
                        "org".to_string(),
                        "group".to_string(),
                    ],
                    _ => vec![t.to_lowercase()],
                }
            }
            Shape::Repo => {
                let mut ts = vec!["repo".to_string()];
                for f in ["fork", "archived", "private", "is_template"] {
                    if flag(f) {
                        ts.push(f.trim_start_matches("is_").to_string());
                    }
                }
                if let Some(topics) = d.and_then(|d| d.get("topics")).and_then(|v| v.as_array()) {
                    ts.extend(
                        topics
                            .iter()
                            .filter_map(|t| t.as_str().map(str::to_string)),
                    );
                }
                ts
            }
            Shape::Item => {
                let mut ts = Vec::new();
                let kind = if d.is_some_and(|d| d.get("tag_name").is_some()) {
                    "release"
                } else if d.is_some_and(|d| d.get("head").is_some() || d.get("pull_request").is_some())
                {
                    "pull"
                } else {
                    "issue"
                };
                ts.push(kind.to_string());
                if let Some(state) = d.and_then(|d| d.get("state")).and_then(|v| v.as_str()) {
                    ts.push(state.to_string());
                }
                if set("merged_at") {
                    ts.push("merged".to_string());
                }
                if flag("draft") {
                    ts.push("draft".to_string());
                }
                if flag("prerelease") {
                    ts.push("prerelease".to_string());
                }
                if let Some(labels) = d.and_then(|d| d.get("labels")).and_then(|v| v.as_array()) {
                    ts.extend(labels.iter().filter_map(|l| {
                        l.get("name").and_then(|v| v.as_str()).map(str::to_string)
                    }));
                }
                ts
            }
            Shape::File => {
                let t = d
                    .and_then(|d| d.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("file");
                vec![t.to_string()]
            }
        }
    }

    /// Curated scalars per kind; timestamps answer as instants,
    /// `::topics` and `::labels` as lists.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        enum Shape {
            User(String),
            Repo(String),
            Item,
            Entry,
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::User { login } => Shape::User(login.clone()),
            Kind::Repo { key } => Shape::Repo(key.clone()),
            Kind::Item => Shape::Item,
            Kind::File { .. } | Kind::Dir { .. } => Shape::Entry,
            _ => return None,
        };
        let at = |d: &Json, ptr: &str| d.pointer(ptr).and_then(json_scalar);
        match shape {
            Shape::User(login) => {
                let d = self.user_data(node, &login)?;
                let key = match name {
                    "login" => "/login",
                    "name" => "/name",
                    "bio" => "/bio",
                    "company" => "/company",
                    "location" => "/location",
                    "blog" => "/blog",
                    "email" => "/email",
                    "followers" => "/followers",
                    "following" => "/following",
                    "repos" => "/public_repos",
                    "gists" => "/public_gists",
                    "created" => "/created_at",
                    "updated" => "/updated_at",
                    "type" => "/type",
                    _ => return None,
                };
                at(&d, key)
            }
            Shape::Repo(rkey) => {
                // Listing objects cover most reads; watchers and
                // license force the full object.
                let full = matches!(name, "watchers" | "license");
                let d = self.repo_data(node, &rkey, full)?;
                match name {
                    "topics" => {
                        let ts = d.get("topics")?.as_array()?;
                        Some(Value::List(
                            ts.iter()
                                .filter_map(|t| Some(Value::Str(t.as_str()?.to_string())))
                                .collect(),
                        ))
                    }
                    "owner" => at(&d, "/owner/login"),
                    // A listing object omits `parent`; a fork
                    // completes itself before answering.
                    "parent" => {
                        let d = if d.get("parent").is_none()
                            && d.get("fork").and_then(|v| v.as_bool()) == Some(true)
                        {
                            self.repo_data(node, &rkey, true)?
                        } else {
                            d
                        };
                        at(&d, "/parent/full_name")
                    }
                    "license" => at(&d, "/license/spdx_id"),
                    _ => {
                        let key = match name {
                            "name" => "/name",
                            "full-name" => "/full_name",
                            "description" => "/description",
                            "stars" => "/stargazers_count",
                            "forks" => "/forks_count",
                            "watchers" => "/subscribers_count",
                            "open-issues" => "/open_issues_count",
                            "language" => "/language",
                            "default-branch" => "/default_branch",
                            "homepage" => "/homepage",
                            "size" => "/size",
                            "created" => "/created_at",
                            "updated" => "/updated_at",
                            "pushed" => "/pushed_at",
                            _ => return None,
                        };
                        at(&d, key)
                    }
                }
            }
            Shape::Item => {
                let d = self.data(node)?;
                match name {
                    "labels" => {
                        let ls = d.get("labels")?.as_array()?;
                        Some(Value::List(
                            ls.iter()
                                .filter_map(|l| {
                                    Some(Value::Str(l.get("name")?.as_str()?.to_string()))
                                })
                                .collect(),
                        ))
                    }
                    "author" => at(&d, "/user/login").or_else(|| at(&d, "/author/login")),
                    "milestone" => at(&d, "/milestone/title"),
                    "base" => at(&d, "/base/ref"),
                    "head" => at(&d, "/head/ref"),
                    _ => {
                        let key = match name {
                            "number" => "/number",
                            "title" => "/title",
                            "state" => "/state",
                            "comments" => "/comments",
                            "created" => "/created_at",
                            "updated" => "/updated_at",
                            "closed" => "/closed_at",
                            "merged" => "/merged_at",
                            "tag" => "/tag_name",
                            "name" => "/name",
                            "published" => "/published_at",
                            _ => return None,
                        };
                        at(&d, key)
                    }
                }
            }
            Shape::Entry => {
                let d = self.data(node)?;
                match name {
                    "size" => at(&d, "/size"),
                    "type" => at(&d, "/type"),
                    _ => None,
                }
            }
        }
    }

    /// A file's decoded content; an issue's, pull's, or
    /// release's body text.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (repo, path) = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::File { repo, path } => (repo.clone(), path.clone()),
            Kind::Item => {
                let d = self.data(node)?;
                let body = d.get("body")?.as_str()?;
                return Some(Value::Str(body.to_string()));
            }
            _ => return None,
        };
        self.file_content(&repo, &path).map(Value::Str)
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "url" => {
                let d = self.data(node)?;
                Some(Value::Str(d.get("html_url")?.as_str()?.to_string()))
            }
            _ => None,
        }
    }

    /// `::owner~>`, `::parent~>` (a fork's upstream),
    /// `::author~>`.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let kind_key = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Repo { key } => Some(key.clone()),
            Kind::Item => None,
            _ => return None,
        };
        match (kind_key, property) {
            (Some(key), "owner") => {
                let d = self.repo_data(node, &key, false)?;
                let login = d.pointer("/owner/login")?.as_str()?;
                Some(self.user_node(login, d.get("owner")))
            }
            (Some(key), "parent") => {
                let d = self.repo_data(node, &key, false)?;
                let d = if d.get("parent").is_none()
                    && d.get("fork").and_then(|v| v.as_bool()) == Some(true)
                {
                    self.repo_data(node, &key, true)?
                } else {
                    d
                };
                let pkey = d.pointer("/parent/full_name")?.as_str()?.to_string();
                Some(self.repo_node(&pkey, d.get("parent")))
            }
            (None, "author") => {
                let d = self.data(node)?;
                self.author_of(&d)
            }
            _ => None,
        }
    }

    /// The outgoing fabric: `follows`, `starred`, and `org`
    /// from a user; `owner`, `parent`, and `contributor` from a
    /// repository; `author` and `assignee` from an item.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        enum Shape {
            User(String),
            Repo(String),
            Item,
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::User { login } => Shape::User(login.clone()),
            Kind::Repo { key } => Shape::Repo(key.clone()),
            Kind::Item => Shape::Item,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        match shape {
            Shape::User(login) => {
                for u in self.call_paged(&format!("/users/{login}/following")) {
                    if let Some(l) = u.get("login").and_then(|v| v.as_str()) {
                        out.push(("follows".to_string(), self.user_node(l, Some(&u))));
                    }
                }
                for r in self.call_paged(&format!("/users/{login}/starred")) {
                    if let Some(k) = r.get("full_name").and_then(|v| v.as_str()) {
                        out.push(("starred".to_string(), self.repo_node(k, Some(&r))));
                    }
                }
                for o in self.call_paged(&format!("/users/{login}/orgs")) {
                    if let Some(l) = o.get("login").and_then(|v| v.as_str()) {
                        out.push(("org".to_string(), self.user_node(l, Some(&o))));
                    }
                }
            }
            Shape::Repo(key) => {
                if let Some(n) = self.resolve(node, "owner", None) {
                    out.push(("owner".to_string(), n));
                }
                if let Some(n) = self.resolve(node, "parent", None) {
                    out.push(("parent".to_string(), n));
                }
                for c in self.call_paged(&format!("/repos/{key}/contributors")) {
                    let Some(l) = c.get("login").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if let Some(n) = c.get("contributions").and_then(|v| v.as_i64()) {
                        self.contributions
                            .borrow_mut()
                            .insert(format!("{key}@{l}"), n);
                    }
                    out.push(("contributor".to_string(), self.user_node(l, Some(&c))));
                }
            }
            Shape::Item => {
                let Some(d) = self.data(node) else {
                    return out;
                };
                if let Some(n) = self.author_of(&d) {
                    out.push(("author".to_string(), n));
                }
                if let Some(assignees) = d.get("assignees").and_then(|v| v.as_array()) {
                    for a in assignees {
                        if let Some(l) = a.get("login").and_then(|v| v.as_str()) {
                            out.push(("assignee".to_string(), self.user_node(l, Some(a))));
                        }
                    }
                }
            }
        }
        out
    }

    /// The incoming fabric, from the API's own reverse indexes:
    /// a user's followers (`<-follows`), an org's members
    /// (`<-org`), a repository's stargazers (`<-starred`) and
    /// forks (`<-parent`).
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        enum Shape {
            User(String),
            Repo(String),
        }
        let shape = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::User { login } => Shape::User(login.clone()),
            Kind::Repo { key } => Shape::Repo(key.clone()),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        match shape {
            Shape::User(login) => {
                for u in self.call_paged(&format!("/users/{login}/followers")) {
                    if let Some(l) = u.get("login").and_then(|v| v.as_str()) {
                        out.push(("follows".to_string(), self.user_node(l, Some(&u))));
                    }
                }
                let is_org = self.traits(node).contains(&"organization".to_string());
                if is_org {
                    for m in self.call_paged(&format!("/orgs/{login}/members")) {
                        if let Some(l) = m.get("login").and_then(|v| v.as_str()) {
                            out.push(("org".to_string(), self.user_node(l, Some(&m))));
                        }
                    }
                }
            }
            Shape::Repo(key) => {
                for u in self.call_paged(&format!("/repos/{key}/stargazers")) {
                    if let Some(l) = u.get("login").and_then(|v| v.as_str()) {
                        out.push(("starred".to_string(), self.user_node(l, Some(&u))));
                    }
                }
                for r in self.call_paged(&format!("/repos/{key}/forks")) {
                    if let Some(k) = r.get("full_name").and_then(|v| v.as_str()) {
                        out.push(("parent".to_string(), self.repo_node(k, Some(&r))));
                    }
                }
            }
        }
        out
    }

    /// The contributor edge carries its commit count:
    /// `$-::contributions`.
    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        if label != "contributor" || name != "contributions" {
            return None;
        }
        let key = match &self.nodes.borrow()[source.0 as usize].kind {
            Kind::Repo { key } => key.clone(),
            _ => return None,
        };
        let login = match &self.nodes.borrow()[target.0 as usize].kind {
            Kind::User { login } => login.clone(),
            _ => return None,
        };
        self.contributions
            .borrow()
            .get(&format!("{key}@{login}"))
            .copied()
            .map(Value::Int)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(
            base64_decode("aGVsbG8gcXVhcmI=").as_deref(),
            Some(b"hello quarb".as_slice())
        );
        assert_eq!(
            base64_decode("aGVsbG8g\ncXVhcmI=\n").as_deref(),
            Some(b"hello quarb".as_slice())
        );
        assert!(base64_decode("not!base64").is_none());
    }

    #[test]
    fn timestamps_become_instants() {
        assert!(matches!(
            str_value("2011-01-25T18:44:36Z"),
            Value::Instant { .. }
        ));
        assert!(matches!(str_value("v1.0.0"), Value::Str(_)));
    }

    #[test]
    fn item_traits_from_shape() {
        let a = GithubAdapter {
            gh: "false".into(),
            anchor: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
                data: RefCell::new(None),
            }]),
            users: RefCell::new(HashMap::new()),
            repos: RefCell::new(HashMap::new()),
            items: RefCell::new(HashMap::new()),
            contributions: RefCell::new(HashMap::new()),
            contents: RefCell::new(HashMap::new()),
        };
        let issue = a.push_node(
            Kind::Item,
            Some("1".into()),
            None,
            Some(json!({"number": 1, "state": "open",
                "labels": [{"name": "bug"}, {"name": "good first issue"}]})),
        );
        assert_eq!(a.traits(issue), ["issue", "open", "bug", "good first issue"]);
        let pull = a.push_node(
            Kind::Item,
            Some("2".into()),
            None,
            Some(json!({"number": 2, "state": "closed", "head": {"ref": "x"},
                "merged_at": "2026-01-01T00:00:00Z", "labels": []})),
        );
        assert_eq!(a.traits(pull), ["pull", "closed", "merged"]);
        let release = a.push_node(
            Kind::Item,
            Some("v1".into()),
            None,
            Some(json!({"tag_name": "v1", "prerelease": true})),
        );
        assert_eq!(a.traits(release), ["release", "prerelease"]);
    }

    #[test]
    fn repo_traits_include_flags_and_topics() {
        let a = GithubAdapter {
            gh: "false".into(),
            anchor: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
                data: RefCell::new(None),
            }]),
            users: RefCell::new(HashMap::new()),
            repos: RefCell::new(HashMap::new()),
            items: RefCell::new(HashMap::new()),
            contributions: RefCell::new(HashMap::new()),
            contents: RefCell::new(HashMap::new()),
        };
        let r = a.push_node(
            Kind::Repo { key: "o/r".into() },
            Some("r".into()),
            None,
            Some(json!({"fork": true, "archived": false,
                "topics": ["cli", "rust"]})),
        );
        assert_eq!(a.traits(r), ["repo", "fork", "cli", "rust"]);
    }

    #[test]
    fn target_needs_the_scheme() {
        assert!(matches!(
            GithubAdapter::connect("gh:torvalds"),
            Err(GithubError::Target(_))
        ));
    }
}
