//! Git repository adapter for the Quarb query engine.
//!
//! A repository is an arbor with a real DAG for its crosslink
//! fabric. The root exposes four names:
//!
//! - `/branches/<name>`, `/tags/<name>` — the refs. A ref node is
//!   an *alias for its commit*: same properties, same children.
//! - `/HEAD` — the checked-out commit, same alias treatment.
//! - `/commits/<hash>` — every commit. Navigating by literal name
//!   accepts anything `git rev-parse` does (unique prefixes,
//!   `HEAD~2`, `v1.0^{}`), without enumerating; enumeration
//!   (`/commits/*`) lists every commit reachable from any ref, in
//!   reverse chronological order, batched through one
//!   `rev-list --all`.
//!
//! A commit's properties are its header (`::author`, `::email`,
//! `::date` as an instant (the author date, offset preserved),
//! `::committer`, `::subject`,
//! `::message`, `::parent` — the first parent's hash); its
//! *children are its tree* — descend `/branches/master/src/lib.rs`
//! and the blob's content is the node's value (`::`), giving
//! time-travel file access at any commit. Tree entries carry
//! `::;type`, `::;mode`, `::;size`, and `::;hash`.
//!
//! References are inherent, no schema needed: `::parent~>`
//! resolves to the first parent, `->parent` enumerates all
//! parents (merges fan out), and `<-parent` finds the commits
//! that point *here* (children — served from the enumeration, so
//! it loads the commit list). Traits name what a node is:
//! `<commit>`, `<branch>`, `<tag>`, `<tree>`, `<blob>`.
//!
//! Everything is read through git plumbing over subprocess
//! (`rev-list`, `rev-parse`, `ls-tree`, `cat-file`, ...) — no
//! libgit2, no new dependencies — lazily, one object on first
//! touch, cached for the adapter's lifetime. The adapter never
//! writes.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// An error opening or reading a repository.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git: {0}")]
    Git(String),
    #[error("git: running git: {0}")]
    Spawn(#[from] std::io::Error),
}

/// A commit's parsed header.
#[derive(Clone)]
struct CommitInfo {
    author: String,
    email: String,
    date: i64,
    /// The author date's UTC offset in minutes (from `%ai`),
    /// preserved for display on the minted instant.
    date_offset: Option<i16>,
    committer: String,
    subject: String,
    message: String,
    tree: String,
    parents: Vec<String>,
}

/// What a node is.
#[derive(Clone)]
enum Kind {
    Root,
    /// `branches`, `tags`, or `commits`.
    Dir(&'static str),
    /// A ref (branch, tag, or HEAD): an alias for its commit.
    Ref {
        name: String,
        commit: String,
    },
    Commit(String),
    /// A tree entry: a subtree or a blob.
    Entry {
        name: String,
        oid: String,
        entry_type: String, // "tree" | "blob"
        mode: String,
    },
}

struct Node {
    kind: Kind,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A git repository, exposed as an arbor.
pub struct GitAdapter {
    repo: PathBuf,
    nodes: RefCell<Vec<Node>>,
    commits: RefCell<HashMap<String, CommitInfo>>,
    /// hash → its `/commits/<hash>` node.
    commit_nodes: RefCell<HashMap<String, NodeId>>,
    /// Whether `/commits` has been enumerated (rev-list --all).
    enumerated: RefCell<bool>,
    /// hash → the paths its diff (vs first parent) touches.
    changed: RefCell<HashMap<String, Vec<String>>>,
    /// commit hash → the tag names pointing at it (lazy).
    tag_map: RefCell<Option<HashMap<String, Vec<String>>>>,
}

const ROOT: NodeId = NodeId(0);
const BRANCHES: NodeId = NodeId(1);
const TAGS: NodeId = NodeId(2);
const COMMITS: NodeId = NodeId(3);

impl GitAdapter {
    /// Open the repository at `path` (any directory inside it).
    pub fn open(path: &std::path::Path) -> Result<Self, GitError> {
        let adapter = GitAdapter {
            repo: path.to_path_buf(),
            nodes: RefCell::new(vec![
                Node {
                    kind: Kind::Root,
                    parent: None,
                    children: RefCell::new(None),
                },
                Node {
                    kind: Kind::Dir("branches"),
                    parent: Some(ROOT),
                    children: RefCell::new(None),
                },
                Node {
                    kind: Kind::Dir("tags"),
                    parent: Some(ROOT),
                    children: RefCell::new(None),
                },
                Node {
                    kind: Kind::Dir("commits"),
                    parent: Some(ROOT),
                    children: RefCell::new(None),
                },
            ]),
            commits: RefCell::new(HashMap::new()),
            commit_nodes: RefCell::new(HashMap::new()),
            enumerated: RefCell::new(false),
            changed: RefCell::new(HashMap::new()),
            tag_map: RefCell::new(None),
        };
        // Probe: is this a repository at all?
        adapter.git(&["rev-parse", "--git-dir"])?;
        Ok(adapter)
    }

    /// A human-readable locator: `/commits/<short>/path`,
    /// `/branches/<name>/path`, ...
    pub fn locator(&self, node: NodeId) -> String {
        let nodes = self.nodes.borrow();
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let nd = &nodes[n.0 as usize];
            match &nd.kind {
                Kind::Root => {}
                Kind::Dir(d) => parts.push(d.to_string()),
                Kind::Ref { name, .. } => parts.push(name.clone()),
                Kind::Commit(h) => parts.push(h[..7.min(h.len())].to_string()),
                Kind::Entry { name, .. } => parts.push(name.clone()),
            }
            cur = nd.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    fn git(&self, args: &[&str]) -> Result<String, GitError> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(GitError::Git(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn push_node(&self, kind: Kind, parent: Option<NodeId>) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind,
            parent,
            children: RefCell::new(None),
        });
        id
    }

    /// The `/commits/<hash>` node for a full hash, interning it.
    fn commit_node(&self, hash: &str) -> NodeId {
        if let Some(&id) = self.commit_nodes.borrow().get(hash) {
            return id;
        }
        let id = self.push_node(Kind::Commit(hash.to_string()), Some(COMMITS));
        self.commit_nodes.borrow_mut().insert(hash.to_string(), id);
        id
    }

    /// A commit's parsed header, fetched on first touch.
    fn commit_info(&self, hash: &str) -> Option<CommitInfo> {
        if let Some(i) = self.commits.borrow().get(hash) {
            return Some(i.clone());
        }
        let out = self
            .git(&[
                "show",
                "-s",
                "--format=%an%x00%ae%x00%at%x00%cn%x00%s%x00%B%x00%T%x00%P%x00%ai",
                hash,
            ])
            .ok()?;
        let info = parse_info(&out)?;
        self.commits
            .borrow_mut()
            .insert(hash.to_string(), info.clone());
        Some(info)
    }

    /// Enumerate every commit reachable from any ref: one
    /// `rev-list --all` with the header format, parsed in bulk.
    fn enumerate_commits(&self) -> Vec<NodeId> {
        if let Some(c) = self.nodes.borrow()[COMMITS.0 as usize]
            .children
            .borrow()
            .as_ref()
        {
            return c.clone();
        }
        let out = self
            .git(&[
                "rev-list",
                "--all",
                "--format=%an%x00%ae%x00%at%x00%cn%x00%s%x00%B%x00%T%x00%P%x00%ai%x1e",
            ])
            .unwrap_or_default();
        let mut ids = Vec::new();
        for (hash, body) in split_commit_records(&out) {
            if let Some(info) = parse_info(body) {
                self.commits.borrow_mut().insert(hash.to_string(), info);
            }
            ids.push(self.commit_node(hash));
        }
        *self.enumerated.borrow_mut() = true;
        *self.nodes.borrow()[COMMITS.0 as usize]
            .children
            .borrow_mut() = Some(ids.clone());
        ids
    }

    /// The tag names pointing at `hash` (annotated tags
    /// dereferenced), from one cached `for-each-ref` sweep.
    fn tags_at(&self, hash: &str) -> Vec<String> {
        if self.tag_map.borrow().is_none() {
            let out = self
                .git(&[
                    "for-each-ref",
                    "refs/tags",
                    "--format=%(refname:short)%00%(objectname)%00%(*objectname)",
                ])
                .unwrap_or_default();
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            for line in out.lines() {
                let mut f = line.split('\u{0}');
                let (Some(name), Some(oid)) = (f.next(), f.next()) else {
                    continue;
                };
                let peeled = f.next().filter(|p| !p.is_empty()).unwrap_or(oid);
                map.entry(peeled.to_string())
                    .or_default()
                    .push(name.to_string());
            }
            *self.tag_map.borrow_mut() = Some(map);
        }
        self.tag_map
            .borrow()
            .as_ref()
            .and_then(|m| m.get(hash).cloned())
            .unwrap_or_default()
    }

    /// The refs under `refs/heads` or `refs/tags` (tags
    /// dereferenced to their commits).
    fn refs(&self, dir: NodeId, prefix: &str) -> Vec<NodeId> {
        if let Some(c) = self.nodes.borrow()[dir.0 as usize]
            .children
            .borrow()
            .as_ref()
        {
            return c.clone();
        }
        let out = self
            .git(&[
                "for-each-ref",
                prefix,
                "--format=%(refname:short)%00%(objectname)%00%(*objectname)",
            ])
            .unwrap_or_default();
        let mut ids = Vec::new();
        for line in out.lines() {
            let mut f = line.split('\u{0}');
            let (Some(name), Some(oid)) = (f.next(), f.next()) else {
                continue;
            };
            let deref = f.next().unwrap_or("");
            let commit = if deref.is_empty() { oid } else { deref };
            ids.push(self.push_node(
                Kind::Ref {
                    name: name.to_string(),
                    commit: commit.to_string(),
                },
                Some(dir),
            ));
        }
        *self.nodes.borrow()[dir.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    /// The entries of a tree object, as child nodes of `parent`.
    fn tree_children(&self, parent: NodeId, tree_oid: &str) -> Vec<NodeId> {
        if let Some(c) = self.nodes.borrow()[parent.0 as usize]
            .children
            .borrow()
            .as_ref()
        {
            return c.clone();
        }
        let out = self.git(&["ls-tree", "-z", tree_oid]).unwrap_or_default();
        let mut ids = Vec::new();
        // "<mode> <type> <oid>\t<name>" records, NUL-terminated by
        // `-z` so names arrive raw — never C-quoted the way the
        // newline-delimited default renders non-ASCII paths.
        for entry in out.split('\u{0}') {
            let Some((meta, name)) = entry.split_once('\t') else {
                continue;
            };
            let mut f = meta.split(' ');
            let (Some(mode), Some(entry_type), Some(oid)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            ids.push(self.push_node(
                Kind::Entry {
                    name: name.to_string(),
                    oid: oid.to_string(),
                    entry_type: entry_type.to_string(),
                    mode: mode.to_string(),
                },
                Some(parent),
            ));
        }
        *self.nodes.borrow()[parent.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    /// The paths a commit's diff touches, relative to its first
    /// parent (the whole tree for a root commit), computed once.
    fn changed_paths(&self, hash: &str) -> Vec<String> {
        if let Some(c) = self.changed.borrow().get(hash) {
            return c.clone();
        }
        let out = self
            .git(&[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-z",
                "-r",
                hash,
            ])
            .unwrap_or_default();
        // `-z`: NUL-separated, raw paths (never C-quoted), so they
        // compare equal to the raw names from `ls-tree -z`.
        let paths: Vec<String> = out
            .split('\u{0}')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        self.changed
            .borrow_mut()
            .insert(hash.to_string(), paths.clone());
        paths
    }

    /// The commit a tree entry belongs to and the entry's path
    /// within it.
    fn entry_context(&self, node: NodeId) -> Option<(String, String)> {
        let nodes = self.nodes.borrow();
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let nd = &nodes[n.0 as usize];
            match &nd.kind {
                Kind::Entry { name, .. } => parts.push(name.clone()),
                Kind::Commit(h) => {
                    parts.reverse();
                    return Some((h.clone(), parts.join("/")));
                }
                Kind::Ref { commit, .. } => {
                    parts.reverse();
                    return Some((commit.clone(), parts.join("/")));
                }
                _ => return None,
            }
            cur = nd.parent;
        }
        None
    }

    /// The commit hash a node stands for (commits and ref aliases).
    fn commit_of(&self, node: NodeId) -> Option<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Commit(h) => Some(h.clone()),
            Kind::Ref { commit, .. } => Some(commit.clone()),
            _ => None,
        }
    }
}

/// Split `rev-list --format=…%x1e` output into `(hash, body)`
/// records. Each commit's format expansion is terminated by the
/// ASCII Record Separator (`%x1e`), so a literal `commit ` line
/// inside a message body (`%s`/`%B`) can never split a record;
/// the leading `commit <hash>` header of each record supplies the
/// hash.
fn split_commit_records(out: &str) -> impl Iterator<Item = (&str, &str)> {
    out.split('\u{1e}').filter_map(|record| {
        let rest = record.trim_start().strip_prefix("commit ")?;
        let (hash, body) = rest.split_once('\n')?;
        Some((hash.trim(), body))
    })
}

fn parse_info(body: &str) -> Option<CommitInfo> {
    let f: Vec<&str> = body.trim_end_matches('\n').split('\u{0}').collect();
    if f.len() < 8 {
        return None;
    }
    Some(CommitInfo {
        author: f[0].to_string(),
        email: f[1].to_string(),
        date: f[2].parse().unwrap_or(0),
        date_offset: f.get(8).and_then(|iso| {
            // `%ai` ends `±HHMM`; the offset rides the instant for
            // display only.
            let tail = iso.trim().rsplit(' ').next()?;
            let sign = match tail.as_bytes().first()? {
                b'+' => 1i16,
                b'-' => -1i16,
                _ => return None,
            };
            let h: i16 = tail.get(1..3)?.parse().ok()?;
            let m: i16 = tail.get(3..5)?.parse().ok()?;
            Some(sign * (h * 60 + m))
        }),
        committer: f[3].to_string(),
        subject: f[4].to_string(),
        message: f[5].trim_end().to_string(),
        tree: f[6].to_string(),
        parents: f[7].split_whitespace().map(str::to_string).collect(),
    })
}

impl AstAdapter for GitAdapter {
    fn root(&self) -> NodeId {
        ROOT
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let kind = self.nodes.borrow()[node.0 as usize].kind.clone();
        match kind {
            Kind::Root => {
                // branches, tags, commits — plus HEAD as a ref
                // alias, interned once.
                if self.nodes.borrow()[ROOT.0 as usize]
                    .children
                    .borrow()
                    .is_none()
                {
                    let mut ids = vec![BRANCHES, TAGS, COMMITS];
                    if let Ok(h) = self.git(&["rev-parse", "HEAD"]) {
                        ids.push(self.push_node(
                            Kind::Ref {
                                name: "HEAD".to_string(),
                                commit: h.trim().to_string(),
                            },
                            Some(ROOT),
                        ));
                    }
                    *self.nodes.borrow()[ROOT.0 as usize].children.borrow_mut() = Some(ids);
                }
                self.nodes.borrow()[ROOT.0 as usize]
                    .children
                    .borrow()
                    .clone()
                    .unwrap_or_default()
            }
            Kind::Dir("branches") => self.refs(BRANCHES, "refs/heads"),
            Kind::Dir("tags") => self.refs(TAGS, "refs/tags"),
            Kind::Dir(_) => self.enumerate_commits(),
            Kind::Ref { commit, .. } | Kind::Commit(commit) => {
                let Some(info) = self.commit_info(&commit) else {
                    return Vec::new();
                };
                self.tree_children(node, &info.tree)
            }
            Kind::Entry {
                oid, entry_type, ..
            } => {
                if entry_type == "tree" {
                    self.tree_children(node, &oid)
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Literal names under `/commits` go straight through
    /// `rev-parse` — unique prefixes, `HEAD~2`, `v1.0^{}` — with
    /// no enumeration.
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        if node == COMMITS {
            // A known full hash answers from the intern map;
            // anything else (prefixes, HEAD~2, tag^{}) goes
            // through rev-parse — enumeration never required.
            if let Some(&id) = self.commit_nodes.borrow().get(name) {
                return vec![id];
            }
            let Ok(out) = self.git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ]) else {
                return Vec::new();
            };
            return vec![self.commit_node(out.trim())];
        }
        self.children(node)
            .into_iter()
            .filter(|&c| self.name(c).as_deref() == Some(name))
            .collect()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => None,
            Kind::Dir(d) => Some(d.to_string()),
            Kind::Ref { name, .. } => Some(name.clone()),
            Kind::Commit(h) => Some(h.clone()),
            Kind::Entry { name, .. } => Some(name.clone()),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// `<commit>`, `<branch>`, `<tag>`, `<tree>`, `<blob>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let nodes = self.nodes.borrow();
        let t = match &nodes[node.0 as usize].kind {
            Kind::Root | Kind::Dir(_) => return Vec::new(),
            Kind::Commit(_) => "commit",
            Kind::Ref { .. } => match nodes[node.0 as usize].parent {
                Some(TAGS) => "tag",
                Some(BRANCHES) => "branch",
                _ => "commit",
            },
            Kind::Entry { entry_type, .. } => {
                let base = if entry_type == "tree" { "tree" } else { "blob" };
                let mut out = vec![base.to_string()];
                drop(nodes);
                // <changed>: this path is in its commit's diff
                // (blobs by exact path, trees when any descendant
                // changed).
                if let Some((hash, path)) = self.entry_context(node) {
                    let prefix = format!("{path}/");
                    if self
                        .changed_paths(&hash)
                        .iter()
                        .any(|p| *p == path || p.starts_with(&prefix))
                    {
                        out.push("changed".to_string());
                    }
                }
                return out;
            }
        };
        vec![t.to_string()]
    }

    /// Commit header fields (ref aliases answer for their commit).
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let hash = self.commit_of(node)?;
        let info = self.commit_info(&hash)?;
        Some(match name {
            "author" => Value::Str(info.author),
            "email" => Value::Str(info.email),
            "date" => Value::Instant {
                secs: info.date,
                nanos: 0,
                offset_min: info.date_offset,
            },
            "committer" => Value::Str(info.committer),
            "subject" => Value::Str(info.subject),
            "message" => Value::Str(info.message),
            "tree" => Value::Str(info.tree),
            "hash" => Value::Str(hash),
            "parent" => Value::Str(info.parents.first()?.clone()),
            // The paths this commit's diff touches (vs its first
            // parent) — deletions included, unlike the tree view.
            "changed" => Value::List(
                self.changed_paths(&hash)
                    .into_iter()
                    .map(Value::Str)
                    .collect(),
            ),
            _ => return None,
        })
    }

    /// A blob's content (text, lossily decoded).
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let (oid, is_blob) = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entry {
                oid, entry_type, ..
            } => (oid.clone(), entry_type == "blob"),
            _ => return None,
        };
        if !is_blob {
            return None;
        }
        self.git(&["cat-file", "blob", &oid]).ok().map(Value::Str)
    }

    /// Commits: `::;short`, `::;n-parents`, `::;tags`,
    /// `::;n-tags`. Entries: `::;type`, `::;mode`, `::;size`,
    /// `::;hash`.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if let Some(hash) = self.commit_of(node) {
            return match key {
                "short" => Some(Value::Str(hash[..7.min(hash.len())].to_string())),
                "n-parents" => Some(Value::Int(self.commit_info(&hash)?.parents.len() as i64)),
                "n-changed" => Some(Value::Int(self.changed_paths(&hash).len() as i64)),
                "tags" => Some(Value::List(
                    self.tags_at(&hash).into_iter().map(Value::Str).collect(),
                )),
                "n-tags" => Some(Value::Int(self.tags_at(&hash).len() as i64)),
                _ => None,
            };
        }
        let (oid, entry_type, mode) = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Entry {
                oid,
                entry_type,
                mode,
                ..
            } => (oid.clone(), entry_type.clone(), mode.clone()),
            _ => return None,
        };
        match key {
            "type" => Some(Value::Str(entry_type)),
            "mode" => Some(Value::Str(mode)),
            "hash" => Some(Value::Str(oid)),
            "size" => self
                .git(&["cat-file", "-s", &oid])
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .map(Value::bytes),
            _ => None,
        }
    }

    /// `::parent~>` — the first parent (the hint is unused: the
    /// target is inherently a commit).
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        if property != "parent" {
            return None;
        }
        let hash = self.commit_of(node)?;
        let first = self.commit_info(&hash)?.parents.first()?.clone();
        Some(self.commit_node(&first))
    }

    /// Every parent is an outgoing `parent` edge (merges fan out).
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some(hash) = self.commit_of(node) else {
            return Vec::new();
        };
        let Some(info) = self.commit_info(&hash) else {
            return Vec::new();
        };
        info.parents
            .iter()
            .map(|p| ("parent".to_string(), self.commit_node(p)))
            .collect()
    }

    /// The commits whose parent is here (the children — served
    /// from the enumeration, so this loads the commit list).
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some(hash) = self.commit_of(node) else {
            return Vec::new();
        };
        self.enumerate_commits();
        let commits = self.commits.borrow();
        let mut out: Vec<(String, String)> = commits
            .iter()
            .filter(|(_, i)| i.parents.contains(&hash))
            .map(|(h, _)| ("parent".to_string(), h.clone()))
            .collect();
        out.sort();
        out.into_iter()
            .map(|(l, h)| (l, self.commit_node(&h)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: a commit whose subject and body contain the word
    // "commit " must not corrupt enumeration. Records are delimited
    // by the trailing %x1e sentinel, not by "commit " in the body.
    #[test]
    fn record_split_survives_commit_word_in_body() {
        let out = "commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
Ann\u{0}ann@example\u{0}1000\u{0}Ann\u{0}\
Revert commit deadbeef\u{0}\
Revert commit deadbeef\n\nThis reverts commit deadbeef.\u{0}\
tttttttttttttttttttttttttttttttttttttttt\u{0}\
pppppppppppppppppppppppppppppppppppppppp\u{0}\
2026-07-15 12:00:00 +0100\u{1e}\n\
commit bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
Bo\u{0}bo@example\u{0}2000\u{0}Bo\u{0}second\u{0}second\u{0}\
tttttttttttttttttttttttttttttttttttttttt\u{0}\u{0}\
2026-07-15 13:00:00 +0100\u{1e}\n";
        let recs: Vec<(&str, &str)> = split_commit_records(out).collect();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].0, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let a = parse_info(recs[0].1).expect("first record parses");
        assert_eq!(a.author, "Ann");
        assert_eq!(a.subject, "Revert commit deadbeef");
        assert_eq!(
            a.message,
            "Revert commit deadbeef\n\nThis reverts commit deadbeef."
        );
        assert_eq!(recs[1].0, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let b = parse_info(recs[1].1).expect("second record parses");
        assert_eq!(b.subject, "second");
    }
}
