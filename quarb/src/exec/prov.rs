//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// The most origins a value keeps by name; beyond it only the
/// count grows (`Origins::more`).
pub const ORIGIN_CAP: usize = 8;

/// Where a value was read: a node, and the pipeline stage (1-based
/// ordinal of the top-level stage; 0 = branch level) during which
/// it was read. The coordinate a capsa's provenance is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub node: NodeId,
    pub stage: u32,
}

impl Default for Origin {
    fn default() -> Self {
        Origin {
            node: NodeId(0),
            stage: 0,
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
    pub fn at(node: NodeId, stage: u32) -> Origins {
        Origins {
            known: vec![Origin { node, stage }],
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
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.known.iter().map(|o| o.node)
    }
}

/// A value's provenance: its origin set, and — for a record or a
/// list — one part per field (by field name) or item (by decimal
/// index), so a field keeps its own origins through a record and
/// out again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prov {
    pub origins: Origins,
    pub parts: Vec<(String, Prov)>,
}

impl Prov {
    pub fn at(node: NodeId, stage: u32) -> Prov {
        Prov {
            origins: Origins::at(node, stage),
            parts: Vec::new(),
        }
    }

    pub fn leaf(origins: Origins) -> Prov {
        Prov {
            origins,
            parts: Vec::new(),
        }
    }

    /// A record's provenance: the union of its parts' origins.
    pub fn record(parts: Vec<(String, Prov)>) -> Prov {
        let mut origins = Origins::default();
        for (_, p) in &parts {
            origins.union(&p.origins);
        }
        Prov { origins, parts }
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

/// A final capsa with its provenance: the node it stands at, its
/// topic (`None` for a node result), and where the topic's value
/// — per field, for a record — was read.
#[derive(Clone, Debug)]
pub struct Traced {
    pub node: NodeId,
    pub topic: Option<Value>,
    pub prov: Prov,
}

/// A node's provenance as the query answers it: the adapter's
/// entries (one empty entry where it records nothing), each with
/// its path filled from the arbor root where no layer set it, and
/// the instant ladder completed — the datum's own timestamp, else
/// the source's modification time (both the adapters' work), else
/// the moment of reading, the invocation instant. Under a
/// reproducible run ([`RunOptions::reproducible`]) the last two
/// rungs are held back: only what the data itself recorded travels.
pub fn resolved_provenance<A: AstAdapter + ?Sized>(adapter: &A, node: NodeId) -> ProvenanceList {
    let mut list = adapter.provenance_list(node);
    if list.entries.is_empty() {
        list.entries.push(Provenance::default());
    }
    let (reproducible, access) = run_option(|o| (o.reproducible, o.now));
    let mut path: Option<String> = None;
    for e in &mut list.entries {
        if e.path.is_none() {
            e.path = Some(path.get_or_insert_with(|| name_path(adapter, node)).clone());
        }
        if reproducible {
            if matches!(
                e.instant_from,
                Some(InstantFrom::Source | InstantFrom::Access)
            ) {
                e.instant = None;
                e.instant_from = None;
            }
        } else if e.instant.is_none()
            && let Some((secs, nanos)) = access
        {
            e.instant = Some((secs, nanos, Some(0)));
            e.instant_from = Some(InstantFrom::Access);
        }
    }
    list
}

/// A value's provenance keys over its origin set: `source` the
/// anchoring (first) origin's, `dpid` likewise, `instant` the latest
/// (a derived value is as fresh as its newest input), `provenance`
/// the canonical list `?a@ts#x;b#y` (entries deduplicated by
/// source and dpid, first-read order), `origins` one canonical text
/// per origin, `elided` how many the bound dropped.
pub(super) fn origins_meta(adapter: &impl AstAdapter, o: &Origins, key: &str) -> Option<Value> {
    let (entries, elided) = origin_entries(adapter, o);
    match key {
        "source" => Some(
            entries
                .first()
                .and_then(|p| p.source.clone())
                .map_or(Value::Null, Value::Str),
        ),
        "dpid" => Some(
            entries
                .first()
                .and_then(|p| p.dpid.clone())
                .map_or(Value::Null, Value::Str),
        ),
        "path" => Some(
            entries
                .first()
                .and_then(|p| p.path.clone())
                .map_or(Value::Null, Value::Str),
        ),
        "instant" => Some(
            entries
                .iter()
                .filter_map(|p| p.instant)
                .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)))
                .map_or(Value::Null, |(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                }),
        ),
        "provenance" => {
            Some(Provenance::canonical_list(&entries, elided).map_or(Value::Null, Value::Str))
        }
        // The record: the same answers the singular keys give.
        "%provenance" => {
            let newest = entries
                .iter()
                .filter(|p| p.instant.is_some())
                .max_by(|a, b| {
                    (a.instant.unwrap().0, a.instant.unwrap().1)
                        .cmp(&(b.instant.unwrap().0, b.instant.unwrap().1))
                });
            Some(prov_record(&Provenance {
                source: entries.first().and_then(|p| p.source.clone()),
                path: entries.first().and_then(|p| p.path.clone()),
                instant: newest.and_then(|p| p.instant),
                instant_from: newest.and_then(|p| p.instant_from),
                dpid: entries.first().and_then(|p| p.dpid.clone()),
            }))
        }
        "@provenance" => Some(Value::list(entries.iter().map(prov_record).collect())),
        // The coordinates: the first origin's id, its record, all.
        "origin" => Some(
            o.known
                .first()
                .map_or(Value::Null, |x| Value::Int(x.node.0 as i64)),
        ),
        "%origin" => Some(o.known.first().map_or(Value::Null, |x| origin_record(*x))),
        "@origin" => Some(Value::list(
            o.known.iter().map(|x| origin_record(*x)).collect(),
        )),
        "elided" => Some(Value::Int(elided as i64)),
        _ => None,
    }
}

/// A provenance entry as a record: `%(source; path; instant;
/// instant-from; dpid)`, absent components null.
pub(super) fn prov_record(p: &Provenance) -> Value {
    Value::Record(vec![
        (
            "source".to_string(),
            p.source.clone().map_or(Value::Null, Value::Str),
        ),
        (
            "path".to_string(),
            p.path.clone().map_or(Value::Null, Value::Str),
        ),
        (
            "instant".to_string(),
            p.instant
                .map_or(Value::Null, |(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                }),
        ),
        (
            "instant-from".to_string(),
            p.instant_from
                .map_or(Value::Null, |f| Value::Str(f.name().to_string())),
        ),
        (
            "dpid".to_string(),
            p.dpid.clone().map_or(Value::Null, Value::Str),
        ),
    ])
}

/// An origin coordinate as a record: `%(id; stage)`.
pub(super) fn origin_record(o: Origin) -> Value {
    Value::Record(vec![
        ("id".to_string(), Value::Int(o.node.0 as i64)),
        ("stage".to_string(), Value::Int(o.stage as i64)),
    ])
}

/// The origins' provenance entries, resolved through the adapter's
/// layers (an origin that itself records a list contributes every
/// entry), deduplicated by (source, dpid) in first-read order, with
/// the count elided: by the origin bound, and by the origins' own
/// records. An origin that answers nothing contributes nothing.
pub(super) fn origin_entries(adapter: &impl AstAdapter, o: &Origins) -> (Vec<Provenance>, u32) {
    let mut out: Vec<Provenance> = Vec::new();
    let mut elided = o.more;
    for n in o.nodes() {
        let list = resolved_provenance(adapter, n);
        elided = elided.saturating_add(list.elided);
        for p in list.entries {
            // One entry per data point: the source's own key where
            // it assigned one (several nodes under one dpid are one
            // datum by the source's claim), the path otherwise.
            let key =
                |e: &Provenance| (e.source.clone(), e.dpid.clone().or_else(|| e.path.clone()));
            let pk = key(&p);
            match out.iter_mut().find(|q| key(q) == pk) {
                Some(q) => {
                    // The same datum read twice: the latest observation.
                    if p.instant.map(|i| (i.0, i.1)) > q.instant.map(|i| (i.0, i.1)) {
                        q.instant = p.instant;
                        q.instant_from = p.instant_from;
                    }
                }
                None => out.push(p),
            }
        }
    }
    (out, elided)
}
