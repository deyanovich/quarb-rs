//! Part of the executor: see `exec/mod.rs`.

use crate::thread::Position;

/// The most origins a value keeps by name; beyond it only the
/// count grows (`Origins::more`).
pub const ORIGIN_CAP: usize = 8;

/// Where a value was read: a node, and the pipeline command (1-based
/// ordinal of the top-level command; 0 = branch level) during which
/// it was read. The coordinate a capsa's provenance is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub position: Position,
    pub command: u32,
}

impl Default for Origin {
    fn default() -> Self {
        Origin {
            position: Position(0),
            command: 0,
        }
    }
}

/// A bounded set of origins: the first [`ORIGIN_CAP`] distinct
/// coordinates by name, and how many more were seen. Exact when
/// `more == 0`; past the bound the count is an upper bound (two
/// bounded sets cannot tell which of their unnamed origins they
/// share) and saturates rather than overflow — a value that has
/// been through twenty rounds of unions over a whole context has
/// more history than a counter holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Origins {
    pub known: Vec<Origin>,
    pub more: u32,
}

impl Origins {
    pub fn at(position: Position, command: u32) -> Origins {
        Origins {
            known: vec![Origin {
                position,
                command: command,
            }],
            more: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty() && self.more == 0
    }

    pub fn insert(&mut self, o: Origin) {
        if self.known.contains(&o) {
            return;
        }
        if self.known.len() < ORIGIN_CAP {
            self.known.push(o);
        } else {
            self.more = self.more.saturating_add(1);
        }
    }

    pub fn union(&mut self, other: &Origins) {
        for o in &other.known {
            self.insert(*o);
        }
        self.more = self.more.saturating_add(other.more);
    }

    /// The origins' nodes, in first-read order.
    pub fn positions(&self) -> impl Iterator<Item = Position> + '_ {
        self.known.iter().map(|o| o.position)
    }
}

/// A value's provenance: its origin set, and — for a record or a
/// list — one part per field (by field name) or item (by decimal
/// index), so a field keeps its own origins through a record and
/// out again.
/// The parts are shared, never copied: a register holding a list
/// pushed from a sub-pipeline carries a part per item, and every
/// capsa of the context clones the register at every command, so
/// a clone of the tree is a reference count, not a walk.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prov {
    pub origins: Origins,
    pub parts: std::sync::Arc<Vec<(String, Prov)>>,
}

impl Prov {
    pub fn at(position: Position, command: u32) -> Prov {
        Prov {
            origins: Origins::at(position, command),
            parts: Default::default(),
        }
    }

    pub fn leaf(origins: Origins) -> Prov {
        Prov {
            origins,
            parts: Default::default(),
        }
    }

    /// A record's provenance: the union of its parts' origins.
    pub fn record(parts: Vec<(String, Prov)>) -> Prov {
        let mut origins = Origins::default();
        for (_, p) in &parts {
            origins.union(&p.origins);
        }
        Prov {
            origins,
            parts: std::sync::Arc::new(parts),
        }
    }

    /// A list's provenance: parts keyed by decimal index.
    pub fn list(items: Vec<Prov>) -> Prov {
        Prov::record(
            items
                .into_iter()
                .enumerate()
                .map(|(i, p)| (i.to_string(), p))
                .collect(),
        )
    }

    /// The named part, else the whole (a value read as one).
    pub fn part(&self, key: &str) -> Prov {
        self.parts
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| Prov::leaf(self.origins.clone()))
    }

    pub fn item(&self, i: usize) -> Prov {
        self.part(&i.to_string())
    }

    /// The origins alone, parts dropped.
    pub fn flat(&self) -> Prov {
        Prov::leaf(self.origins.clone())
    }
}
