//! SPARQL endpoint adapter for the Quarb query engine.
//!
//! RDF has no tree spine, so this adapter supplies one by the
//! triples-as-arbors ruling: the **`rdf:type` layer plays the
//! tables** — the root holds one child per class present in the
//! data, a typed resource lists under every class it carries —
//! and predicates split **per triple** by object kind: a literal
//! object is a property (`::pred`), an IRI object a typed
//! crosslink (`->pred`, with `<-pred` its reverse — RDF is
//! symmetric under SPARQL, so backlinks are first-class).
//!
//! Resources and predicates go by their IRI's **local name**
//! (fragment, else last path segment), full IRIs at `;;;iri`;
//! `?key=PRED` (e.g. `rdfs:label`) nominates a naming predicate
//! instead. Typed literals keep their types — `xsd:integer` is
//! an integer, `xsd:dateTime` mints an instant, `xsd:duration`
//! a duration. Multi-valued predicates answer as lists;
//! language-tagged text prefers `?lang=` (default `en`).
//!
//! Public endpoints are unbounded, so listings carry an explicit
//! cap (`?limit=`, default 1000) and announce truncation at
//! `;;;complete` on the class node — a capped listing must never
//! read as a full one.
//!
//! **Target**: `sparql:URL[#limit=N&key=PRED&lang=L]` — adapter
//! parameters ride the fragment, never reaching the server:
//! `sparql:https://query.wikidata.org/sparql#limit=200`.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a SPARQL endpoint.
#[derive(Debug, thiserror::Error)]
pub enum SparqlError {
    #[error("sparql: {0}")]
    Api(String),
    #[error("sparql target: {0} (expected sparql:URL[#limit=N&key=PRED&lang=L])")]
    Target(String),
}

fn api<E: std::fmt::Display>(e: E) -> SparqlError {
    SparqlError::Api(e.to_string())
}

/// The local name of an IRI: the fragment, else the last path
/// segment.
fn local_name(iri: &str) -> String {
    let tail = iri
        .rsplit_once('#')
        .map(|(_, t)| t)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| iri.trim_end_matches('/').rsplit('/').next().unwrap_or(iri));
    if tail.is_empty() { iri.to_string() } else { tail.to_string() }
}

/// Expand a `prefix:name` CURIE from the well-known table, or
/// pass a full IRI through.
fn expand(curie_or_iri: &str) -> String {
    const PREFIXES: &[(&str, &str)] = &[
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("skos", "http://www.w3.org/2004/02/skos/core#"),
        ("foaf", "http://xmlns.com/foaf/0.1/"),
        ("dc", "http://purl.org/dc/terms/"),
        ("schema", "http://schema.org/"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
    ];
    if curie_or_iri.starts_with("http://") || curie_or_iri.starts_with("https://") {
        return curie_or_iri.to_string();
    }
    if let Some((p, n)) = curie_or_iri.split_once(':')
        && let Some((_, base)) = PREFIXES.iter().find(|(k, _)| *k == p)
    {
        return format!("{base}{n}");
    }
    curie_or_iri.to_string()
}

/// A results-JSON binding term as a typed Quarb value (literals
/// only; IRI terms become edges, not values).
fn literal_value(term: &Json) -> Value {
    let value = term
        .pointer("/value")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let dt = term
        .pointer("/datatype")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match local_name(dt).as_str() {
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "positiveInteger" | "unsignedInt" | "unsignedLong" => value
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(value.to_string())),
        "decimal" | "double" | "float" => value
            .parse::<f64>()
            .map(Value::Float)
            .unwrap_or_else(|_| Value::Str(value.to_string())),
        "boolean" => Value::Bool(value == "true"),
        "dateTime" | "date" => match quarb::temporal::parse_iso(value) {
            Some((secs, nanos, offset_min)) => Value::Instant {
                secs,
                nanos,
                offset_min,
            },
            None => Value::Str(value.to_string()),
        },
        "duration" | "dayTimeDuration" | "yearMonthDuration" => {
            match quarb::temporal::parse_span(value) {
                Some((secs, nanos)) => Value::Duration { secs, nanos },
                None => Value::Str(value.to_string()),
            }
        }
        _ => Value::Str(value.to_string()),
    }
}

/// Percent-encode a form value.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A resource's loaded facts.
#[derive(Default, Clone)]
struct Facts {
    /// literal predicates → values (multi-valued kept in order).
    props: Vec<(String, Vec<Value>)>,
    /// IRI predicates → target IRIs.
    out: Vec<(String, String)>,
}

enum Kind {
    Root,
    Class {
        iri: String,
        /// Whether the row listing hit the cap.
        complete: RefCell<Option<bool>>,
    },
    Resource {
        iri: String,
        facts: RefCell<Option<Facts>>,
    },
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A SPARQL endpoint, exposed as an arbor.
pub struct SparqlAdapter {
    endpoint: String,
    limit: usize,
    key: Option<String>,
    lang: String,
    classes: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    by_iri: RefCell<HashMap<String, NodeId>>,
}

impl SparqlAdapter {
    /// Connect to `sparql:URL`; the class sweep doubles as the
    /// probe.
    pub fn connect(target: &str) -> Result<Self, SparqlError> {
        let rest = target
            .strip_prefix("sparql:")
            .ok_or_else(|| SparqlError::Target(target.to_string()))?;
        let (url, frag) = match rest.split_once('#') {
            Some((u, f)) => (u, Some(f)),
            None => (rest, None),
        };
        if url.is_empty() {
            return Err(SparqlError::Target(target.to_string()));
        }
        let param = |k: &str| {
            frag.and_then(|f| {
                f.split('&')
                    .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            })
        };
        let adapter = SparqlAdapter {
            endpoint: url.to_string(),
            limit: param("limit").and_then(|l| l.parse().ok()).unwrap_or(1000),
            key: param("key").map(|k| expand(&k)),
            lang: param("lang").unwrap_or_else(|| "en".to_string()),
            classes: Vec::new(),
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            by_iri: RefCell::new(HashMap::new()),
        };
        let rows = adapter.select(&format!(
            "SELECT DISTINCT ?t WHERE {{ ?s a ?t }} ORDER BY ?t LIMIT {}",
            adapter.limit
        ))?;
        let classes: Vec<String> = rows
            .iter()
            .filter_map(|b| b.pointer("/t/value").and_then(|v| v.as_str().map(str::to_string)))
            .collect();
        let mut adapter = adapter;
        for c in &classes {
            adapter.nodes.get_mut().push(Node {
                kind: Kind::Class {
                    iri: c.clone(),
                    complete: RefCell::new(None),
                },
                name: Some(local_name(c)),
                parent: Some(NodeId(0)),
                children: RefCell::new(None),
            });
        }
        adapter.classes = classes;
        Ok(adapter)
    }

    /// A human-readable locator: `/Class/name`.
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

    /// One SELECT, bindings back.
    fn select(&self, query: &str) -> Result<Vec<Json>, SparqlError> {
        let body = format!("query={}", urlencode(query));
        let resp = ureq::post(&self.endpoint)
            .set("content-type", "application/x-www-form-urlencoded")
            .set("accept", "application/sparql-results+json")
            .set("user-agent", "quarb-sparql (https://quarb.org)")
            .send_string(&body)
            .map_err(|e| api(e))?;
        let json: Json = resp.into_json().map_err(|e| api(e))?;
        Ok(json
            .pointer("/results/bindings")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Intern a resource by IRI (name resolved from `?key=` when
    /// nominated, else the local name).
    fn intern(&self, iri: &str, parent: Option<NodeId>) -> NodeId {
        if let Some(&id) = self.by_iri.borrow().get(iri) {
            return id;
        }
        let name = match &self.key {
            Some(pred) => {
                let q = format!(
                    "SELECT ?n WHERE {{ <{iri}> <{pred}> ?n . \
                     FILTER (lang(?n) = '' || langMatches(lang(?n), '{}')) }} LIMIT 1",
                    self.lang
                );
                self.select(&q)
                    .ok()
                    .and_then(|rows| {
                        rows.first()
                            .and_then(|b| b.pointer("/n/value"))
                            .and_then(|v| v.as_str().map(str::to_string))
                    })
                    .unwrap_or_else(|| local_name(iri))
            }
            None => local_name(iri),
        };
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind: Kind::Resource {
                iri: iri.to_string(),
                facts: RefCell::new(None),
            },
            name: Some(name),
            parent,
            children: RefCell::new(None),
        });
        drop(nodes);
        self.by_iri.borrow_mut().insert(iri.to_string(), id);
        id
    }

    /// A resource's facts, loaded once: literals become
    /// properties (language filter applied), IRI objects edges.
    fn facts_of(&self, node: NodeId) -> Facts {
        let iri = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Resource { iri, facts } => {
                if let Some(f) = &*facts.borrow() {
                    return f.clone();
                }
                iri.clone()
            }
            _ => return Facts::default(),
        };
        let rows = self
            .select(&format!(
                "SELECT ?p ?o WHERE {{ <{iri}> ?p ?o }} ORDER BY ?p LIMIT {}",
                self.limit
            ))
            .unwrap_or_default();
        let mut facts = Facts::default();
        for b in &rows {
            let Some(p) = b.pointer("/p/value").and_then(|v| v.as_str()) else {
                continue;
            };
            let pname = local_name(p);
            let Some(o) = b.pointer("/o") else { continue };
            match o.pointer("/type").and_then(|v| v.as_str()) {
                Some("uri") => {
                    if let Some(t) = o.pointer("/value").and_then(|v| v.as_str()) {
                        facts.out.push((pname, t.to_string()));
                    }
                }
                Some("literal") | Some("typed-literal") => {
                    // Language selection: untagged always wins a
                    // slot; tagged text only when it matches.
                    if let Some(tag) = o.pointer("/xml:lang").and_then(|v| v.as_str())
                        && !tag.eq_ignore_ascii_case(&self.lang)
                        && !tag
                            .split('-')
                            .next()
                            .unwrap_or("")
                            .eq_ignore_ascii_case(&self.lang)
                    {
                        continue;
                    }
                    let v = literal_value(o);
                    match facts.props.iter_mut().find(|(k, _)| k == &pname) {
                        Some((_, vs)) => vs.push(v),
                        None => facts.props.push((pname, vec![v])),
                    }
                }
                _ => {}
            }
        }
        if let Kind::Resource { facts: cell, .. } = &self.nodes.borrow()[node.0 as usize].kind {
            *cell.borrow_mut() = Some(facts.clone());
        }
        facts
    }
}

impl AstAdapter for SparqlAdapter {
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
        let class_iri = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => {
                return (1..=self.classes.len()).map(|i| NodeId(i as u64)).collect();
            }
            Kind::Resource { .. } => return Vec::new(),
            Kind::Class { iri, .. } => iri.clone(),
        };
        let rows = self
            .select(&format!(
                "SELECT ?s WHERE {{ ?s a <{class_iri}> }} ORDER BY ?s LIMIT {}",
                self.limit
            ))
            .unwrap_or_default();
        let complete = rows.len() < self.limit;
        let ids: Vec<NodeId> = rows
            .iter()
            .filter_map(|b| b.pointer("/s/value").and_then(|v| v.as_str()))
            .map(|iri| self.intern(iri, Some(node)))
            .collect();
        {
            let nodes = self.nodes.borrow();
            if let Kind::Class { complete: c, .. } = &nodes[node.0 as usize].kind {
                *c.borrow_mut() = Some(complete);
            }
            *nodes[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        }
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Vec::new(),
            Kind::Class { .. } => vec!["class".to_string()],
            Kind::Resource { .. } => Vec::new(),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let facts = self.facts_of(node);
        let (_, vs) = facts.props.iter().find(|(k, _)| k == name)?;
        match vs.len() {
            0 => None,
            1 => Some(vs[0].clone()),
            _ => Some(Value::List(vs.clone())),
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let nodes = self.nodes.borrow();
        match (&nodes[node.0 as usize].kind, key) {
            (Kind::Class { iri, .. }, "iri") | (Kind::Resource { iri, .. }, "iri") => {
                Some(Value::Str(iri.clone()))
            }
            (Kind::Class { complete, .. }, "complete") => {
                if complete.borrow().is_none() {
                    drop(nodes);
                    // The flag is a fact about the listing; make
                    // the listing happen.
                    self.children(node);
                    let nodes = self.nodes.borrow();
                    if let Kind::Class { complete, .. } = &nodes[node.0 as usize].kind {
                        return complete.borrow().map(Value::Bool);
                    }
                    return None;
                }
                complete.borrow().map(Value::Bool)
            }
            (Kind::Class { iri, .. }, "n-rows") => {
                let iri = iri.clone();
                drop(nodes);
                let rows = self
                    .select(&format!(
                        "SELECT (COUNT(?s) AS ?c) WHERE {{ ?s a <{iri}> }}"
                    ))
                    .ok()?;
                rows.first()
                    .and_then(|b| b.pointer("/c"))
                    .map(literal_value)
            }
            _ => None,
        }
    }

    /// Outgoing triples with IRI objects, as typed crosslinks.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let facts = self.facts_of(node);
        facts
            .out
            .iter()
            .map(|(p, t)| (p.clone(), self.intern(t, None)))
            .collect()
    }

    /// Reverse triples: whoever points here, by predicate.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let iri = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Resource { iri, .. } => iri.clone(),
            _ => return Vec::new(),
        };
        let rows = self
            .select(&format!(
                "SELECT ?p ?s WHERE {{ ?s ?p <{iri}> }} ORDER BY ?p ?s LIMIT {}",
                self.limit
            ))
            .unwrap_or_default();
        rows.iter()
            .filter_map(|b| {
                let p = b.pointer("/p/value").and_then(|v| v.as_str())?;
                let s = b.pointer("/s/value").and_then(|v| v.as_str())?;
                Some((local_name(p), self.intern(s, None)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names() {
        assert_eq!(local_name("http://schema.org/Person"), "Person");
        assert_eq!(
            local_name("http://www.w3.org/2000/01/rdf-schema#label"),
            "label"
        );
        assert_eq!(local_name("http://example.org/a/b/"), "b");
    }

    #[test]
    fn curies_expand() {
        assert_eq!(
            expand("rdfs:label"),
            "http://www.w3.org/2000/01/rdf-schema#label"
        );
        assert_eq!(expand("http://x/y"), "http://x/y");
    }

    #[test]
    fn literals_type() {
        let term = serde_json::json!({
            "type": "literal", "value": "42",
            "datatype": "http://www.w3.org/2001/XMLSchema#integer"
        });
        assert_eq!(literal_value(&term), Value::Int(42));
        let term = serde_json::json!({
            "type": "literal", "value": "2019-03-01T00:00:00Z",
            "datatype": "http://www.w3.org/2001/XMLSchema#dateTime"
        });
        assert!(matches!(literal_value(&term), Value::Instant { .. }));
    }
}
