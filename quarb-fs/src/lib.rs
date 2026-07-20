//! Filesystem adapter for Quarb.
//!
//! Maps a directory tree onto the arbor model: the base directory is
//! the root, directory entries are its children, and a node's *name*
//! is its filename.
//!
//! The tree is materialized once, at construction, by walking with
//! [`ignore::WalkBuilder`] — the same machinery ripgrep uses — so the
//! adapter respects `.gitignore`/`.ignore` files and skips hidden
//! entries by default. This is eager: a targeted query still walks
//! the whole tree up front. A lazy traversal with cascading ignore
//! rules is a planned optimization.
//!
//! Projections and traits are live: the default projection (`::`) is
//! a file's text content; `;;;key` exposes metadata (`size`,
//! `modified`, `extension`, `is-dir`, `is-file`, and on Unix `mode`
//! and `permissions`); and `<trait>` filters on a structural class
//! (`dir`, `file`, `symlink`) or a file class by extension (`code`,
//! `text`, `image`, `audio`, `video`, `document`, `archive`, `data`).
//!
//! Symlinks are crosslinks: a symlink node carries a `target`
//! crosslink to the node it points at, navigable with `->` (and
//! `<-` for the reverse), and carries the `<symlink>` trait. The
//! walk itself does not follow symlinks.

use ignore::WalkBuilder;
use quarb::{AstAdapter, NodeId, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Which entries the adapter exposes.
#[derive(Debug, Clone, Copy)]
pub struct FsOptions {
    /// Include hidden entries (names starting with `.`). Default: `false`.
    pub hidden: bool,
    /// Respect `.gitignore` / `.ignore` files. Default: `true`.
    pub respect_ignore: bool,
}

impl Default for FsOptions {
    fn default() -> Self {
        FsOptions {
            hidden: false,
            respect_ignore: true,
        }
    }
}

/// A Quarb adapter over a directory tree rooted at a base path.
pub struct FsAdapter {
    paths: Vec<PathBuf>,
    children: Vec<Vec<NodeId>>,
    parents: Vec<Option<NodeId>>,
    index: HashMap<PathBuf, NodeId>,
    root: NodeId,
}

impl FsAdapter {
    /// Build an adapter rooted at `root` with default options
    /// (ignore-aware, hidden entries skipped).
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::with_options(root, FsOptions::default())
    }

    /// Build an adapter rooted at `root` with explicit options.
    ///
    /// The path is canonicalized, so results are absolute and
    /// symlink-resolved.
    pub fn with_options(root: impl AsRef<Path>, opts: FsOptions) -> std::io::Result<Self> {
        let root_path = root.as_ref().canonicalize()?;

        let mut paths: Vec<PathBuf> = Vec::new();
        let mut children: Vec<Vec<NodeId>> = Vec::new();
        let mut parents: Vec<Option<NodeId>> = Vec::new();
        let mut index: HashMap<PathBuf, usize> = HashMap::new();

        let mut builder = WalkBuilder::new(&root_path);
        builder
            .hidden(!opts.hidden)
            .git_ignore(opts.respect_ignore)
            .git_global(opts.respect_ignore)
            .git_exclude(opts.respect_ignore)
            .ignore(opts.respect_ignore)
            .parents(opts.respect_ignore)
            .require_git(false)
            .follow_links(false)
            .sort_by_file_name(|a, b| a.cmp(b));

        for entry in builder.build() {
            let Ok(entry) = entry else { continue };
            let path = entry.path().to_path_buf();

            let id = *index.entry(path.clone()).or_insert_with(|| {
                let id = paths.len();
                paths.push(path.clone());
                children.push(Vec::new());
                parents.push(None);
                id
            });

            // The walk yields a directory before its contents, so a
            // node's parent is always interned first.
            if let Some(parent_path) = path.parent()
                && let Some(&pidx) = index.get(parent_path)
                && pidx != id
            {
                parents[id] = Some(NodeId(pidx as u64));
                children[pidx].push(NodeId(id as u64));
            }
        }

        let index = index
            .into_iter()
            .map(|(p, i)| (p, NodeId(i as u64)))
            .collect();

        Ok(FsAdapter {
            paths,
            children,
            parents,
            index,
            root: NodeId(0),
        })
    }

    /// The filesystem path a `NodeId` stands for.
    pub fn path(&self, node: NodeId) -> PathBuf {
        self.paths[node.0 as usize].clone()
    }

    /// If `path` is a symlink, the canonical path it points to.
    fn symlink_target(path: &Path) -> Option<PathBuf> {
        let md = std::fs::symlink_metadata(path).ok()?;
        if !md.file_type().is_symlink() {
            return None;
        }
        let target = std::fs::read_link(path).ok()?;
        let absolute = if target.is_absolute() {
            target
        } else {
            path.parent()?.join(target)
        };
        absolute.canonicalize().ok()
    }
}

/// Map a lowercased file extension to a file-class trait.
fn file_class(ext: &str) -> Option<&'static str> {
    let class = match ext {
        "rs" | "py" | "c" | "cpp" | "cc" | "h" | "hpp" | "js" | "ts" | "go" | "java" | "rb"
        | "php" | "sh" | "swift" | "kt" | "lua" | "pl" | "scala" | "clj" | "ex" | "hs" => "code",
        "txt" | "md" | "rst" | "adoc" | "org" | "tex" => "text",
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "ps" | "epub" => "document",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "tiff" | "ico" => "image",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "audio",
        "mp4" | "mkv" | "webm" | "mov" | "avi" => "video",
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" => "archive",
        "json" | "yaml" | "yml" | "toml" | "xml" | "csv" | "ini" => "data",
        _ => return None,
    };
    Some(class)
}

impl AstAdapter for FsAdapter {
    fn root(&self) -> NodeId {
        self.root
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.children[node.0 as usize].clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        if node == self.root {
            return None;
        }
        self.paths[node.0 as usize]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.parents[node.0 as usize]
    }

    /// Traits of a filesystem node: a structural trait (`dir`,
    /// `file`, or `symlink`) plus, for files, a class derived from
    /// the extension (`code`, `text`, `image`, …).
    fn traits(&self, node: NodeId) -> Vec<String> {
        let path = &self.paths[node.0 as usize];
        let mut out = Vec::new();
        if let Ok(md) = std::fs::symlink_metadata(path) {
            let ft = md.file_type();
            if ft.is_symlink() {
                out.push("symlink".into());
            } else if ft.is_dir() {
                out.push("dir".into());
            } else if ft.is_file() {
                out.push("file".into());
            }
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && let Some(class) = file_class(&ext.to_ascii_lowercase())
        {
            out.push(class.into());
        }
        out
    }

    /// The default projection of a file is its text content; a
    /// directory has none.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let path = &self.paths[node.0 as usize];
        if path.is_file() {
            std::fs::read_to_string(path).ok().map(Value::Str)
        } else {
            None
        }
    }

    /// Filesystem metadata (`;;;key`): `size`, `modified`,
    /// `extension`, `is-dir`, `is-file`, and (on Unix) `mode` and
    /// `permissions`.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let path = &self.paths[node.0 as usize];
        let md = std::fs::metadata(path).ok()?;
        match key {
            "size" => Some(Value::bytes(md.len() as i64)),
            "is-dir" => Some(Value::Bool(md.is_dir())),
            "is-file" => Some(Value::Bool(md.is_file())),
            "modified" => md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| Value::Instant {
                    secs: d.as_secs() as i64,
                    nanos: d.subsec_nanos(),
                    offset_min: None,
                }),
            "extension" => path
                .extension()
                .map(|e| Value::Str(e.to_string_lossy().into_owned())),
            #[cfg(unix)]
            "mode" => {
                use std::os::unix::fs::MetadataExt;
                Some(Value::Int(md.mode() as i64))
            }
            #[cfg(unix)]
            "permissions" => {
                use std::os::unix::fs::PermissionsExt;
                Some(Value::Str(format!(
                    "{:o}",
                    md.permissions().mode() & 0o7777
                )))
            }
            _ => None,
        }
    }

    /// A symlink node has one outgoing crosslink, labelled `target`,
    /// to the node it points at (when that node is within the tree).
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let path = &self.paths[node.0 as usize];
        match Self::symlink_target(path).and_then(|t| self.index.get(&t)) {
            Some(&target) => vec![("target".to_string(), target)],
            None => Vec::new(),
        }
    }

    /// Incoming crosslinks: the symlinks in the tree that point at
    /// `node`. Requires scanning every node.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let target = &self.paths[node.0 as usize];
        self.paths
            .iter()
            .enumerate()
            .filter(|(_, p)| Self::symlink_target(p).as_ref() == Some(target))
            .map(|(i, _)| ("target".to_string(), NodeId(i as u64)))
            .collect()
    }
}
