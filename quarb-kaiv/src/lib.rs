//! kaiv adapter for Quarb: a typed arbor over `.daiv` / `.kaiv`
//! documents.
//!
//! kaiv namepaths ARE Quarb paths (the `/@name` admission made the
//! adapter an identity map): the canonical line
//! `!int'/@servers/0::port=8080` mounts as node `/@servers/0` with
//! the typed property `::port` — the namepath pastes into a query
//! verbatim, no name mapping anywhere.
//!
//! - Containers (namespace segments, array elements) are nodes;
//!   fields are typed *properties* on their container AND leaf
//!   child nodes, so per-field provenance is addressable:
//!   `/@results/0::age` reads the value, `/@results/0/age::;dpid`
//!   reads where it came from.
//! - Core types mint typed values (`!int` → integer, `!bool` →
//!   boolean, `!null` → null); the std/time types with a date part
//!   mint *instants* (a `datetime`'s written offset is kept for
//!   display); `!b64` and any other named type ride as text with
//!   the type name in `::;type`.
//! - Unit-annotated types (`!float:km`) mint QUANTITIES: the value
//!   scales to its dimension's base through kaiv's frozen unit
//!   table, so `42 km` filters against `5000 m` or `30mi` and a
//!   criterion may be written in any compatible unit (spec: The
//!   Quantital Fragment). Pure-time units mint Durations; the
//!   written unit stays on display and in `::;unit`.
//! - Provenance surfaces as leaf metadata: `::;source` (the
//!   declared id), `::;source-uri` (its declared URI),
//!   `::;timestamp`, `::;dpid`.
//! - Authored sugar (variables, blocks, `+:=`, maps, units,
//!   named-type imports) is resolved by kaiv's own compiler before
//!   mounting, and `$field` references are denormalized to their
//!   values — what mounts is always the canonical document.
//!
//! This closes the emit→mount loop: `qua --daiv` output re-mounts
//! (`/@results/*::field`), so typed results graft beside any other
//! substrate and join against their own source.

use quarb::{AstAdapter, NodeId, Value};

/// A Quarb adapter over a compiled kaiv document.
pub struct KaivAdapter {
    nodes: Vec<Node>,
    /// Declared sources, in declaration order: (id, uri).
    sources: Vec<(String, String)>,
    /// The document's imported custom-unit definitions (`.!units`
    /// via `.faiv`), kept so criterion text in queries can use the
    /// document's own units (the `unit_scale` override).
    customs: std::collections::BTreeMap<String, kaiv::faiv::UnitDef>,
}

#[derive(Default)]
struct Node {
    /// The namepath segment (`@servers`, `0`, `server`); empty for
    /// the root. Field leaves carry the (unquoted) field name.
    name: String,
    parent: Option<usize>,
    children: Vec<usize>,
    /// Set on field leaves only.
    leaf: Option<Leaf>,
}

struct Leaf {
    value: Value,
    /// A non-core type name (`acme/net/port`, `b64`,
    /// `std/time/datetime`), when the line carried one.
    ty: Option<String>,
    unit: Option<String>,
    source: Option<String>,
    timestamp: Option<String>,
    dpid: Option<String>,
}

/// The parsed pieces of a canonical left side:
/// `!TYPE[:UNIT][?SRC[@TS]][#DPID]'NAMEPATH`.
struct LeftSide<'a> {
    ty: &'a str,
    unit: Option<&'a str>,
    source: Option<&'a str>,
    timestamp: Option<&'a str>,
    dpid: Option<&'a str>,
    namepath: &'a str,
}

fn parse_left(left: &str) -> Option<LeftSide<'_>> {
    let rest = left.strip_prefix('!')?;
    // The first `'` closes the meta; type names, units, and
    // provenance idents exclude it (quoted namepath segments use
    // doubled `"`, never `'`).
    let q = rest.find('\'')?;
    let (meta, namepath) = (&rest[..q], &rest[q + 1..]);
    let mut ty = meta;
    let mut unit = None;
    let mut source = None;
    let mut timestamp = None;
    let mut dpid = None;
    // Split the meta at its markers, left to right.
    let mut cut = meta.len();
    for (i, c) in meta.char_indices() {
        if matches!(c, ':' | '?' | '#') {
            cut = i;
            break;
        }
    }
    ty = &meta[..cut];
    let mut tail = &meta[cut..];
    if let Some(rest) = tail.strip_prefix(':') {
        let end = rest.find(['?', '#']).unwrap_or(rest.len());
        unit = Some(&rest[..end]);
        tail = &rest[end..];
    }
    if let Some(rest) = tail.strip_prefix('?') {
        let end = rest.find(['@', '#']).unwrap_or(rest.len());
        source = Some(&rest[..end]);
        tail = &rest[end..];
        if let Some(rest) = tail.strip_prefix('@') {
            let end = rest.find('#').unwrap_or(rest.len());
            timestamp = Some(&rest[..end]);
            tail = &rest[end..];
        }
    }
    if let Some(rest) = tail.strip_prefix('#') {
        dpid = Some(rest);
    }
    Some(LeftSide {
        ty,
        unit,
        source,
        timestamp,
        dpid,
        namepath,
    })
}

/// Split a namepath into container segments and the field name,
/// respecting quoted names (doubled-`"` escapes). The field
/// separator is the last `::` outside quotes.
fn split_namepath(np: &str) -> Option<(Vec<String>, String)> {
    let b = np.as_bytes();
    let mut in_quotes = false;
    let mut field_at = None;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => in_quotes = !in_quotes,
            b':' if !in_quotes && i + 1 < b.len() && b[i + 1] == b':' => {
                field_at = Some(i);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    let at = field_at?;
    let (path, field) = (&np[..at], &np[at + 2..]);
    let mut segs = Vec::new();
    let (mut in_quotes, mut start) = (false, 0);
    for (i, c) in path.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '/' if !in_quotes => {
                if i > start {
                    segs.push(unquote(&path[start..i]));
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if path.len() > start {
        segs.push(unquote(&path[start..]));
    }
    Some((segs, unquote(field)))
}

/// Strip a quoted name's quotes and undouble its `""` escapes;
/// bare names pass through.
fn unquote(name: &str) -> String {
    match name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => name.to_string(),
    }
}

/// The typed value for a canonical type name, plus the `::;type`
/// metadata where the name is worth keeping (non-core, named
/// types). Named types from kaiv's embedded standard libraries
/// resolve through their declared ordering class: `..num` mints
/// numerically, `..time` mints an instant (the written offset kept
/// for display; a local time has no timeline point and stays
/// text). Other libraries' types ride as text — resolving them
/// needs a configured registry, a recorded seam.
fn typed_value(ty: &str, raw: &str) -> (Value, Option<String>) {
    match ty {
        "int" => (
            raw.trim()
                .parse::<i64>()
                .map(Value::Int)
                .unwrap_or_else(|_| Value::Str(raw.to_string())),
            None,
        ),
        "float" => (
            raw.trim()
                .parse::<f64>()
                .map(Value::Float)
                .unwrap_or_else(|_| Value::Str(raw.to_string())),
            None,
        ),
        "bool" => (Value::Bool(raw.trim() == "true"), None),
        "null" => (Value::Null, None),
        "str" => (Value::Str(raw.to_string()), None),
        other => {
            let span = other
                .rsplit_once('/')
                .and_then(|(lib, name)| kaiv::taiv::embedded(lib).map(|l| (l, name)))
                .and_then(|(l, name)| l.types.get(name))
                .and_then(|def| {
                    def.items.iter().find_map(|i| match i {
                        kaiv::anno::Item::Constraint(kaiv::anno::Constraint::Span(s)) => {
                            Some(s.clone())
                        }
                        _ => None,
                    })
                });
            let value = match span.as_deref() {
                Some("..num") => raw
                    .trim()
                    .parse::<i64>()
                    .map(Value::Int)
                    .or_else(|_| raw.trim().parse::<f64>().map(Value::Float))
                    .unwrap_or_else(|_| Value::Str(raw.to_string())),
                Some("..time") => quarb::temporal::parse_iso(raw.trim())
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                    })
                    .unwrap_or_else(|| Value::Str(raw.to_string())),
                _ => Value::Str(raw.to_string()),
            };
            (value, Some(other.to_string()))
        }
    }
}

/// The resolver a mount at `dir` uses: the directory's own
/// `kaiv.kaiv` (Layer 2) when present, relative registry bases
/// anchored there — so a document resolves its `.!units` /
/// `.!types` imports exactly as `kaiv build` in that directory
/// would. No directory: the offline core-only resolver.
pub fn resolver_for(dir: Option<&std::path::Path>) -> kaiv::Resolver {
    let mut config = kaiv::Config::default();
    if let Some(d) = dir {
        if let Ok(bytes) = std::fs::read(d.join("kaiv.kaiv"))
            && let Ok(parsed) = kaiv::Config::parse(&bytes, Some(d.to_path_buf()))
        {
            config = parsed;
        }
        config.base_dir = Some(d.to_path_buf());
    }
    kaiv::Resolver::new(config)
}

impl KaivAdapter {
    /// Mount a canonical `.daiv` document (the end of kaiv's own
    /// pipeline — `qua --daiv` output, `kaiv build` output). No
    /// compilation: canonical lines lex directly.
    pub fn parse_daiv(text: &str) -> Result<Self, kaiv::PipelineError> {
        Self::parse_daiv_with(text, &kaiv::Resolver::offline())
    }

    /// [`Self::parse_daiv`] with the document's directory, so its
    /// `.!units` imports resolve (custom units scale to base).
    pub fn parse_daiv_at(
        text: &str,
        dir: Option<&std::path::Path>,
    ) -> Result<Self, kaiv::PipelineError> {
        Self::parse_daiv_with(text, &resolver_for(dir))
    }

    /// [`Self::parse_daiv`] with an explicit resolver (embedding
    /// hosts preload artifacts; qua passes the file's directory).
    pub fn parse_daiv_with(
        text: &str,
        resolver: &kaiv::Resolver,
    ) -> Result<Self, kaiv::PipelineError> {
        Self::from_flat(text, resolver)
    }

    /// Mount an authored `.kaiv` document: kaiv's own compiler
    /// resolves the sugar (variables, blocks, `+:=`, maps, units,
    /// named-type imports), the denormalizer resolves `$field`
    /// references, and the canonical result mounts.
    pub fn parse_kaiv(text: &str) -> Result<Self, kaiv::PipelineError> {
        Self::parse_kaiv_with(text, &kaiv::Resolver::offline())
    }

    /// [`Self::parse_kaiv`] with the document's directory.
    pub fn parse_kaiv_at(
        text: &str,
        dir: Option<&std::path::Path>,
    ) -> Result<Self, kaiv::PipelineError> {
        Self::parse_kaiv_with(text, &resolver_for(dir))
    }

    /// [`Self::parse_kaiv`] with an explicit resolver.
    pub fn parse_kaiv_with(
        text: &str,
        resolver: &kaiv::Resolver,
    ) -> Result<Self, kaiv::PipelineError> {
        let canonical = kaiv::compile_with(text.as_bytes(), resolver)?;
        Self::from_flat(&kaiv::denormalize(&canonical)?, resolver)
    }

    /// Mount a relational canonical `.raiv` document: denormalize
    /// its `$field` references, then mount.
    pub fn parse_raiv(text: &str) -> Result<Self, kaiv::PipelineError> {
        Self::from_flat(&kaiv::denormalize(text)?, &kaiv::Resolver::offline())
    }

    /// [`Self::parse_raiv`] with the document's directory.
    pub fn parse_raiv_at(
        text: &str,
        dir: Option<&std::path::Path>,
    ) -> Result<Self, kaiv::PipelineError> {
        Self::from_flat(&kaiv::denormalize(text)?, &resolver_for(dir))
    }

    fn from_flat(flat: &str, resolver: &kaiv::Resolver) -> Result<Self, kaiv::PipelineError> {
        let lines =
            kaiv::lex(flat.as_bytes(), kaiv::FileKind::Data).map_err(kaiv::PipelineError::Lex)?;
        let mut a = KaivAdapter {
            nodes: vec![Node::default()],
            sources: Vec::new(),
            customs: Default::default(),
        };
        // Pre-pass: the document's unit imports (`.!units LIB`) and
        // registry overrides (`.!registry prefix=base`). Each
        // import's `.faiv` definitions join the customs table, so
        // custom units scale to base exactly like built-ins; an
        // unresolvable library is skipped, and its units keep the
        // text-plus-`::;unit` fallback.
        let mut layer1: Vec<(String, String)> = Vec::new();
        let mut unit_libs: Vec<String> = Vec::new();
        for line in &lines {
            if let kaiv::lexer::LineKind::Decl(d) = &line.kind {
                if let Some(rest) = d.strip_prefix(".!registry") {
                    if let Some((p, b)) = rest.trim().split_once('=') {
                        layer1.push((p.trim().to_string(), b.trim().to_string()));
                    }
                } else if let Some(rest) = d.strip_prefix(".!units") {
                    unit_libs.push(rest.trim().to_string());
                }
            }
        }
        for lib in &unit_libs {
            if let Ok(defs) = resolver.unit_defs(lib, &layer1) {
                a.customs.extend(defs);
            }
        }
        for line in &lines {
            match &line.kind {
                kaiv::lexer::LineKind::Decl(d) => {
                    // `.?id URI` — a source declaration.
                    if let Some(rest) = d.strip_prefix(".?")
                        && let Some((id, uri)) = rest.split_once(char::is_whitespace)
                    {
                        a.sources.push((id.to_string(), uri.trim().to_string()));
                    }
                }
                kaiv::lexer::LineKind::Content { left, value } => {
                    let Some(l) = parse_left(left) else { continue };
                    let Some((segs, field)) = split_namepath(l.namepath) else {
                        continue;
                    };
                    let container = a.container(&segs);
                    // Unit-annotated leaves mint QUANTITIES, scaled
                    // to their dimension's base via kaiv's own
                    // frozen table — 42 km compares against 5000 m.
                    // A pure-time unit mints a Duration instead
                    // (one ontology per dimension of time), a
                    // dimensionless one stays a plain number, and
                    // an unresolvable custom unit keeps the old
                    // text-plus-::;unit behavior (the .faiv
                    // registry route is a recorded seam).
                    let minted = l.unit.and_then(|unit| {
                        let n: f64 = value.trim().parse().ok()?;
                        let (factor, base) = kaiv::unit::scale_with(unit, &a.customs)?;
                        if base == "1" {
                            return None;
                        }
                        if base == "s" {
                            let total = n * factor;
                            return Some(Value::Duration {
                                secs: total.floor() as i64,
                                nanos: ((total - total.floor()) * 1e9) as u32,
                            });
                        }
                        Some(Value::Quantity {
                            value: n * factor,
                            base,
                            written: Some((n, unit.to_string())),
                        })
                    });
                    let (v, ty_meta) = match minted {
                        Some(v) => (v, None),
                        None => typed_value(l.ty, value),
                    };
                    let leaf = Leaf {
                        value: v,
                        ty: ty_meta,
                        unit: l.unit.map(str::to_string),
                        source: l.source.map(str::to_string),
                        timestamp: l.timestamp.map(str::to_string),
                        dpid: l.dpid.map(str::to_string),
                    };
                    let id = a.nodes.len();
                    a.nodes.push(Node {
                        name: field,
                        parent: Some(container),
                        children: Vec::new(),
                        leaf: Some(leaf),
                    });
                    a.nodes[container].children.push(id);
                }
                _ => {}
            }
        }
        Ok(a)
    }

    /// Walk/create the container chain for `segs`, returning its
    /// node index.
    fn container(&mut self, segs: &[String]) -> usize {
        let mut cur = 0usize;
        for seg in segs {
            let found = self.nodes[cur]
                .children
                .iter()
                .copied()
                .find(|&c| self.nodes[c].leaf.is_none() && self.nodes[c].name == *seg);
            cur = match found {
                Some(c) => c,
                None => {
                    let id = self.nodes.len();
                    self.nodes.push(Node {
                        name: seg.clone(),
                        parent: Some(cur),
                        children: Vec::new(),
                        leaf: None,
                    });
                    self.nodes[cur].children.push(id);
                    id
                }
            };
        }
        cur
    }

    /// The kaiv namepath of `node` — the identity locator: `/` for
    /// the root, `/a/@b/0` for containers, `/a/@b/0::field` for
    /// field leaves.
    pub fn locator(&self, node: NodeId) -> String {
        let i = node.0 as usize;
        if i == 0 || i >= self.nodes.len() {
            return "/".to_string();
        }
        let mut segs = Vec::new();
        let mut cur = Some(i);
        let mut field = None;
        while let Some(c) = cur {
            if c == 0 {
                break;
            }
            let n = &self.nodes[c];
            if n.leaf.is_some() && field.is_none() && segs.is_empty() {
                field = Some(n.name.clone());
            } else {
                segs.push(n.name.clone());
            }
            cur = n.parent;
        }
        segs.reverse();
        let mut out = String::new();
        for s in &segs {
            out.push('/');
            out.push_str(s);
        }
        match field {
            Some(f) => format!("{out}::{f}"),
            None if out.is_empty() => "/".to_string(),
            None => out,
        }
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.0 as usize)
    }
}

impl AstAdapter for KaivAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.node(node)
            .map(|n| n.children.iter().map(|&c| NodeId(c as u64)).collect())
            .unwrap_or_default()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let n = self.node(node)?;
        if node.0 == 0 {
            return None;
        }
        Some(n.name.clone())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.node(node)?.parent.map(|p| NodeId(p as u64))
    }

    /// A field projects on its container: `/@servers/0::port`.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let n = self.node(node)?;
        n.children.iter().find_map(|&c| {
            let child = &self.nodes[c];
            (child.name == name)
                .then(|| child.leaf.as_ref().map(|l| l.value.clone()))
                .flatten()
        })
    }

    /// A field leaf's own value: `/@servers/0/port | ...`.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.node(node)?.leaf.as_ref().map(|l| l.value.clone())
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = self.node(node)?;
        let l = n.leaf.as_ref()?;
        match key {
            "type" => l.ty.clone().map(Value::Str),
            "unit" => l.unit.clone().map(Value::Str),
            "source" => l.source.clone().map(Value::Str),
            "source-uri" => {
                let id = l.source.as_deref()?;
                self.sources
                    .iter()
                    .find(|(i, _)| i == id)
                    .map(|(_, uri)| Value::Str(uri.clone()))
            }
            "timestamp" => l.timestamp.clone().map(Value::Str),
            "dpid" => l.dpid.clone().map(Value::Str),
            _ => None,
        }
    }

    /// Criterion text resolves through the document's own unit
    /// imports as well as the built-in table, so
    /// `[::range < '50kellicam']` means what the mounted document
    /// says a kellicam is.
    fn unit_scale(&self, expr: &str) -> Option<(f64, String)> {
        kaiv::unit::scale_with(expr, &self.customs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(a: &KaivAdapter, query: &str) -> Vec<String> {
        match quarb::run(query, a).expect(query) {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
        }
    }

    #[test]
    fn typed_scalars_and_identity_paths() {
        let a = KaivAdapter::parse_daiv(concat!(
            ".!kaiv 1\n",
            "!int'/cfg::port=8080\n!bool'/cfg::active=true\n!float'/cfg::ratio=0.5\n",
        ))
        .unwrap();
        assert_eq!(values(&a, "/cfg::port"), ["8080"]);
        assert_eq!(values(&a, "/cfg[::active]::ratio"), ["0.5"]);
        // Typed: arithmetic works without coercion ceremony.
        assert_eq!(values(&a, "/cfg | ::port idiv 1000"), ["8"]);
    }

    #[test]
    fn namespace_arrays_paste_verbatim() {
        let a = KaivAdapter::parse_daiv(concat!(
            ".!kaiv 1\n",
            "!str'/@servers/0::host=a\n!int'/@servers/0::port=1\n",
            "!str'/@servers/1::host=b\n!int'/@servers/1::port=2\n",
        ))
        .unwrap();
        // The kaiv namepath IS the quarb path.
        assert_eq!(values(&a, "/@servers/0::port"), ["1"]);
        assert_eq!(values(&a, "/@servers/*[::port > 1]::host"), ["b"]);
        // Field leaves make fields addressable as nodes too.
        assert_eq!(values(&a, "/@servers/1/port::"), ["2"]);
    }

    #[test]
    fn provenance_units_and_named_types() {
        let a = KaivAdapter::parse_daiv(concat!(
            ".!kaiv 1\n",
            ".?sensor1 https://sensors.example.com/1\n",
            "!int?sensor1@20250115T093000Z#req-42'/readings::temp=100\n",
            "!float:km'/trip::length=42\n",
            "!float:W'/rig::power=290\n",
            "!acme/net/label'/net::host=web-01\n",
        ))
        .unwrap();
        assert_eq!(values(&a, "/readings::temp"), ["100"]);
        assert_eq!(values(&a, "/readings/temp::"), ["100"]);
        assert_eq!(values(&a, "/readings/temp::;source"), ["sensor1"]);
        assert_eq!(
            values(&a, "/readings/temp::;source-uri"),
            ["https://sensors.example.com/1"]
        );
        assert_eq!(values(&a, "/readings/temp::;dpid"), ["req-42"]);
        assert_eq!(values(&a, "/trip/length::;unit"), ["km"]);
        // Unit-annotated: a quantity, filterable in ANY compatible
        // unit — the criterion converts through the same table.
        assert_eq!(values(&a, "/trip[::length > 26mi]/length::"), ["42 km"]);
        assert_eq!(values(&a, "/trip[::length = 42000]::length"), ["42 km"]);
        assert_eq!(values(&a, "/trip | ::length + 500m"), ["42500 m"]);
        // The kW criterion against a W-minted value, and the
        // explicit conversion stage.
        assert_eq!(values(&a, "/rig[::power > 0.2kW]::power"), ["290 W"]);
        assert_eq!(values(&a, "/rig | ::power | convert(kW)"), ["0.29 kW"]);
        // Dimension mismatch: the comparison fails, never lies.
        assert_eq!(values(&a, "/rig[::power > 5km]::power"), Vec::<String>::new());
        assert_eq!(values(&a, "/net/host::;type"), ["acme/net/label"]);
        assert_eq!(values(&a, "/net::host"), ["web-01"]);
    }

    #[test]
    fn time_types_mint_instants() {
        let a = KaivAdapter::parse_daiv(concat!(
            ".!kaiv 1\n",
            "!std/time/datetime'/msg::at=2026-07-06T17:20:00Z\n",
            "!std/time/date'/msg::day=2026-07-06\n",
        ))
        .unwrap();
        // Minted: temporal comparison and arithmetic apply directly.
        assert_eq!(values(&a, "/msg[::at > 2026-07-01]::day"), ["2026-07-06"]);
        assert_eq!(values(&a, "/msg | ::at - 2026-07-06"), ["PT17H20M"]);
    }

    #[test]
    fn authored_sugar_compiles_before_mounting() {
        // The +:= array sugar from the conformance corpus.
        let a = KaivAdapter::parse_kaiv(concat!(
            ".!kaiv 1\n",
            "/@servers+:=host=a|port=1\n",
            "/@servers+:=host=b|port=2\n",
        ))
        .unwrap();
        assert_eq!(values(&a, "/@servers/* @| count"), ["2"]);
        assert_eq!(values(&a, "/@servers/*::host"), ["a", "b"]);
    }

    #[test]
    fn custom_units_scale_via_faiv() {
        // A weird/units .faiv preloaded on the resolver — the same
        // road qua takes via the document's directory.
        let faiv = concat!(
            ".!kaivunit 1 weird/units\n",
            "m 1.7018\n&smoot=\n",
            "m 2000\n&kellicam=\n",
        );
        let resolver = kaiv::Resolver::offline();
        resolver.preload("weird/units", "faiv", faiv.as_bytes().to_vec());
        let a = KaivAdapter::parse_daiv_with(
            concat!(
                ".!kaiv 1\n",
                ".!units weird/units\n",
                "!float:smoot'/bridge::length=364.4\n",
                "!float:kellicam'/@contacts/0::range=45\n",
                "!float:kellicam/h'/@contacts/0::speed=2\n",
            ),
            &resolver,
        )
        .unwrap();
        // Custom units scale to base: criteria in built-in units.
        assert_eq!(
            values(&a, "/bridge[::length > 0.5km]/length::"),
            ["364.4 smoot"]
        );
        assert_eq!(
            values(&a, "/@contacts/*[::range < 100km] | ::range | convert(km)"),
            ["90 km"]
        );
        // Criterion text in the document's OWN units — unit_scale
        // answers from the customs — and compound criteria, quoted
        // (`/` is a navigation operator in query text).
        assert_eq!(
            values(&a, "/@contacts/*[::range > '40kellicam']::range"),
            ["45 kellicam"]
        );
        assert_eq!(
            values(&a, "/@contacts/*[::speed > '1km/h']::speed"),
            ["2 kellicam/h"]
        );
        assert_eq!(
            values(&a, "/@contacts/0 | ::speed | convert('km/h')"),
            ["4 km/h"]
        );
        // Without the import resolving, the value stays text.
        let b = KaivAdapter::parse_daiv(
            ".!kaiv 1\n.!units weird/units\n!float:smoot'/bridge::length=364.4\n",
        )
        .unwrap();
        assert_eq!(values(&b, "/bridge::length"), ["364.4"]);
        assert_eq!(values(&b, "/bridge/length::;unit"), ["smoot"]);
    }

    #[test]
    fn scalar_arrays_and_quoted_names() {
        let a = KaivAdapter::parse_daiv(concat!(
            ".!kaiv 1\n",
            "!str'/@numbers::0=one\n!str'/@numbers::1=two\n",
            "!str'/cfg::\"server name\"=prod-1\n",
        ))
        .unwrap();
        assert_eq!(values(&a, "/@numbers::1"), ["two"]);
        assert_eq!(values(&a, "/@numbers/* @| count"), ["2"]);
        assert_eq!(values(&a, "/cfg::'server name'"), ["prod-1"]);
    }
}
