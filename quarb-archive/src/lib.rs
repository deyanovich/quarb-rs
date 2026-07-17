//! Zip and tar archive adapter for the Quarb query engine.
//!
//! An archive is a directory tree in a file: entries become
//! nodes under their path (intermediate directories synthesized),
//! a file entry's content is its value (`::`, text lossily
//! decoded), and `::;size` / `::;compressed` (zip) report the
//! obvious. Half the world's document formats are zip archives —
//! `.jar`, `.docx`, `.xlsx`, `.odt`, `.epub` — and composed with
//! [`quarb-compose`], their inner XML/JSON is directly queryable:
//! `qua '/word/document.xml!//w:t::text' report.docx`.
//!
//! The index (entry list) loads at open; contents load lazily per
//! entry for zip, and in one pass for tar (the format is a
//! stream). Gzip'd tars (`.tar.gz`, `.tgz`) decompress
//! transparently. Read-only, like everything.

use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// An error opening an archive.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("archive: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("archive: unrecognized format (expected zip or tar)")]
    Format,
}

struct Node {
    name: String,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// A file entry's index into `contents` (`None`: directory).
    entry: Option<usize>,
    size: Option<i64>,
    compressed: Option<i64>,
}

enum Source {
    Zip(RefCell<zip::ZipArchive<std::fs::File>>),
    /// Tar contents are read up front (streaming format).
    Tar,
}

/// An archive file, exposed as an arbor.
pub struct ArchiveAdapter {
    nodes: Vec<Node>,
    /// Lazily-filled entry contents (zip); prefilled for tar.
    contents: RefCell<Vec<Option<String>>>,
    /// Entry index → name in the zip index.
    zip_names: Vec<String>,
    source: Source,
}

impl ArchiveAdapter {
    /// Open a `.zip`-family or `.tar[.gz]` archive (format by
    /// magic bytes: `PK`, gzip, else tar by extension).
    pub fn open(path: &Path) -> Result<Self, ArchiveError> {
        let mut f = std::fs::File::open(path)?;
        let mut magic = [0u8; 2];
        let n = f.read(&mut magic)?;
        drop(f);
        if n == 2 && magic == *b"PK" {
            return Self::open_zip(path);
        }
        if n == 2 && magic == [0x1f, 0x8b] {
            return Self::open_tar(path, true);
        }
        if path.extension().and_then(|e| e.to_str()) == Some("tar") {
            return Self::open_tar(path, false);
        }
        Err(ArchiveError::Format)
    }

    fn open_zip(path: &Path) -> Result<Self, ArchiveError> {
        let f = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(f)?;
        let mut b = TreeBuilder::new();
        let mut zip_names = Vec::new();
        for i in 0..zip.len() {
            let e = zip.by_index_raw(i)?;
            if e.is_dir() {
                b.dir(e.name());
                continue;
            }
            let idx = zip_names.len();
            zip_names.push(e.name().to_string());
            b.file(
                e.name(),
                idx,
                Some(e.size() as i64),
                Some(e.compressed_size() as i64),
            );
        }
        let n = zip_names.len();
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(vec![None; n]),
            zip_names,
            source: Source::Zip(RefCell::new(zip)),
        })
    }

    fn open_tar(path: &Path, gz: bool) -> Result<Self, ArchiveError> {
        let f = std::fs::File::open(path)?;
        let reader: Box<dyn Read> = if gz {
            Box::new(flate2::read::GzDecoder::new(f))
        } else {
            Box::new(f)
        };
        let mut ar = tar::Archive::new(reader);
        let mut b = TreeBuilder::new();
        let mut contents = Vec::new();
        for entry in ar.entries()? {
            let mut e = entry?;
            let name = e.path()?.to_string_lossy().into_owned();
            if e.header().entry_type().is_dir() {
                b.dir(&name);
                continue;
            }
            if !e.header().entry_type().is_file() {
                continue;
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)?;
            let idx = contents.len();
            let size = buf.len() as i64;
            contents.push(Some(String::from_utf8_lossy(&buf).into_owned()));
            b.file(&name, idx, Some(size), None);
        }
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(contents),
            zip_names: Vec::new(),
            source: Source::Tar,
        })
    }

    /// A human-readable locator: the entry path.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let nd = &self.nodes[n.0 as usize];
            if !nd.name.is_empty() {
                parts.push(nd.name.clone());
            }
            cur = nd.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// An entry's text content, reading it on first touch (zip).
    fn content(&self, entry: usize) -> Option<String> {
        if let Some(c) = &self.contents.borrow()[entry] {
            return Some(c.clone());
        }
        let Source::Zip(zip) = &self.source else {
            return None;
        };
        let mut zip = zip.borrow_mut();
        let mut e = zip.by_name(&self.zip_names[entry]).ok()?;
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        self.contents.borrow_mut()[entry] = Some(text.clone());
        Some(text)
    }
}

/// Builds the path tree from entry names.
struct TreeBuilder {
    nodes: Vec<Node>,
    by_path: HashMap<String, usize>,
}

/// Normalize an entry path: split on `/`, dropping empty and `.`
/// segments and collapsing `..`. GNU tar (`tar -c -C dir .`)
/// stores `./`-prefixed names; without this a spurious `.` node
/// is interned and everything lands beneath it, breaking
/// absolute-path navigation that works for the equivalent zip.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

impl TreeBuilder {
    fn new() -> Self {
        TreeBuilder {
            nodes: vec![Node {
                name: String::new(),
                parent: None,
                children: Vec::new(),
                entry: None,
                size: None,
                compressed: None,
            }],
            by_path: HashMap::new(),
        }
    }

    /// The node for a directory path, creating the chain.
    fn dir(&mut self, path: &str) -> usize {
        let path = normalize(path);
        if path.is_empty() {
            return 0;
        }
        if let Some(&i) = self.by_path.get(&path) {
            return i;
        }
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (self.dir(p), n.to_string()),
            None => (0, path.clone()),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            name,
            parent: Some(NodeId(parent as u64)),
            children: Vec::new(),
            entry: None,
            size: None,
            compressed: None,
        });
        self.nodes[parent].children.push(NodeId(id as u64));
        self.by_path.insert(path, id);
        id
    }

    fn file(&mut self, path: &str, entry: usize, size: Option<i64>, compressed: Option<i64>) {
        let path = normalize(path);
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (self.dir(p), n.to_string()),
            None => (0, path.clone()),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            name,
            parent: Some(NodeId(parent as u64)),
            children: Vec::new(),
            entry: Some(entry),
            size,
            compressed,
        });
        self.nodes[parent].children.push(NodeId(id as u64));
        self.by_path.insert(path, id);
    }
}

impl AstAdapter for ArchiveAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize].name;
        (!n.is_empty()).then(|| n.clone())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// `<file>` or `<dir>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        if node.0 == 0 {
            return Vec::new();
        }
        let t = if self.nodes[node.0 as usize].entry.is_some() {
            "file"
        } else {
            "dir"
        };
        vec![t.to_string()]
    }

    /// A file entry's content, text.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let entry = self.nodes[node.0 as usize].entry?;
        self.content(entry).map(Value::Str)
    }

    /// `::;size`, `::;compressed` (zip), `::;n-entries` (root).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        match key {
            "size" => n.size.map(Value::bytes),
            "compressed" => n.compressed.map(Value::Int),
            "n-entries" if node.0 == 0 => Some(Value::Int(self.nodes.len() as i64 - 1)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_dot_and_dotdot_segments() {
        assert_eq!(normalize("./"), "");
        assert_eq!(normalize("./a.txt"), "a.txt");
        assert_eq!(normalize("./word/document.xml"), "word/document.xml");
        assert_eq!(normalize("word/document.xml"), "word/document.xml");
        assert_eq!(normalize("a/./b"), "a/b");
        assert_eq!(normalize("a/../b"), "b");
        assert_eq!(normalize("../x"), "x");
    }

    #[test]
    fn dot_prefixed_tar_entries_land_under_root() {
        // GNU tar (`tar -c -C dir .`) stores `./` and `./a.txt`;
        // no spurious `.` node must be interned, and the file must
        // hang directly off the root as it does for the equal zip.
        let mut b = TreeBuilder::new();
        b.dir("./");
        b.file("./a.txt", 0, Some(3), None);
        let root = &b.nodes[0];
        assert_eq!(root.children.len(), 1);
        let child = &b.nodes[root.children[0].0 as usize];
        assert_eq!(child.name, "a.txt");
        assert_eq!(child.parent, Some(NodeId(0)));
        assert!(b.nodes.iter().all(|n| n.name != "."));
    }
}
