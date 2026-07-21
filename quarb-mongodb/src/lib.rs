//! MongoDB adapter for the Quarb query engine.
//!
//! A MongoDB database is two levels of container: the database
//! holds collections, a collection holds BSON documents. The
//! arbor follows: `/tracks/mars::title`, with documents named by
//! their `_id` (ObjectIds as hex) and embedded documents and
//! arrays descending as child nodes.
//!
//! **DBRefs are native references.** The `{ "$ref": COLL, "$id":
//! ID }` convention is a typed document pointer, so `~>` needs
//! neither a schema nor a declared refs file: `::album~>::artist`
//! follows the field's own target, and `->` enumerates every
//! DBRef field as a labeled edge.
//!
//! Everything loads lazily — collection names at connect, a
//! collection's documents on first descent (one `find`, sorted by
//! `_id` so listings are deterministic), reference targets by
//! single `findOne`s. The adapter only ever reads; it never
//! writes.
//!
//! Type mapping: strings, booleans, and the integer/double family
//! arrive natively; BSON dates (and internal timestamps) become
//! Quarb instants; ObjectIds and Decimal128 arrive as strings,
//! where Quarb's numeric reading takes over. `NULL` is null.
//!
//! **Target**: a standard connection string with the database as
//! the path — `mongodb://HOST[:PORT]/DATABASE[?options]` (or
//! `mongodb+srv://…`). The driver honors every URI option; when
//! `serverSelectionTimeoutMS` is not given, the adapter lowers
//! the driver's 30 s default to 5 s so a bad host fails fast.

use mongodb::bson::{Bson, Document, doc};
use mongodb::options::ClientOptions;
use mongodb::sync::{Client, Database};
use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

/// An error connecting to or reading a database.
#[derive(Debug, thiserror::Error)]
pub enum MongodbError {
    #[error("mongodb: {0}")]
    Driver(#[from] mongodb::error::Error),
    #[error("mongodb target: {0} (expected mongodb://HOST[:PORT]/DATABASE[?options])")]
    Target(String),
}

/// What a node is.
enum Kind {
    Root,
    /// A collection, by name.
    Collection { name: String },
    /// A document: `path` is `collection/id`.
    Doc { path: String },
    /// A field value inside a document (scalar, embedded
    /// document, or array element).
    Field { value: Field },
}

/// A decoded BSON value.
#[derive(Clone)]
enum Field {
    Scalar(Value),
    /// A DBRef: the target collection and the raw `$id` (kept as
    /// BSON so the lookup matches exactly). `db` is the optional
    /// `$db` — a reference into another database displays but
    /// does not resolve (the arbor is scoped to one database).
    Reference {
        coll: String,
        id: Bson,
        db: Option<String>,
    },
    Map(Vec<(String, Field)>),
    Array(Vec<Field>),
}

/// The display form of a document id: ObjectIds as hex, strings
/// as themselves, everything else via BSON's rendering.
fn id_str(id: &Bson) -> String {
    match id {
        Bson::ObjectId(o) => o.to_hex(),
        Bson::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The internal cache key for a document. Distinct from the
/// display form: `_id: 5` and `_id: "5"` both *display* as `5`,
/// but must not share a cache slot, so the key keeps the BSON
/// type (via `Debug`).
fn doc_key(coll: &str, id: &Bson) -> String {
    format!("{coll}/{id:?}")
}

fn decode(v: &Bson) -> Field {
    match v {
        Bson::String(s) => Field::Scalar(Value::Str(s.clone())),
        Bson::Boolean(b) => Field::Scalar(Value::Bool(*b)),
        Bson::Int32(i) => Field::Scalar(Value::Int(*i as i64)),
        Bson::Int64(i) => Field::Scalar(Value::Int(*i)),
        Bson::Double(f) => Field::Scalar(Value::Float(*f)),
        Bson::ObjectId(o) => Field::Scalar(Value::Str(o.to_hex())),
        Bson::DateTime(dt) => {
            let ms = dt.timestamp_millis();
            Field::Scalar(Value::Instant {
                secs: ms.div_euclid(1000),
                nanos: (ms.rem_euclid(1000) * 1_000_000) as u32,
                offset_min: None,
            })
        }
        Bson::Timestamp(t) => Field::Scalar(Value::Instant {
            secs: t.time as i64,
            nanos: 0,
            offset_min: None,
        }),
        Bson::Decimal128(d) => Field::Scalar(Value::Str(d.to_string())),
        Bson::Symbol(s) => Field::Scalar(Value::Str(s.clone())),
        Bson::JavaScriptCode(s) => Field::Scalar(Value::Str(s.clone())),
        Bson::RegularExpression(r) => Field::Scalar(Value::Str(r.to_string())),
        Bson::Document(d) => {
            if let Some(Bson::String(coll)) = d.get("$ref")
                && let Some(id) = d.get("$id")
            {
                let db = match d.get("$db") {
                    Some(Bson::String(s)) => Some(s.clone()),
                    _ => None,
                };
                return Field::Reference {
                    coll: coll.clone(),
                    id: id.clone(),
                    db,
                };
            }
            Field::Map(d.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        }
        Bson::Array(a) => Field::Array(a.iter().map(decode).collect()),
        // Null, Undefined, Binary, MinKey/MaxKey, DbPointer,
        // JavaScriptCodeWithScope: no scalar reading.
        _ => Field::Scalar(Value::Null),
    }
}

struct Node {
    kind: Kind,
    name: Option<String>,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// A MongoDB database, exposed as an arbor.
pub struct MongodbAdapter {
    db: Database,
    /// The collection names, from the connect probe.
    collections: Vec<String>,
    nodes: RefCell<Vec<Node>>,
    /// document path (`collection/id`) → decoded fields.
    docs: RefCell<HashMap<String, Vec<(String, Field)>>>,
    /// document path → node (for reference resolution).
    doc_nodes: RefCell<HashMap<String, NodeId>>,
    /// collection name → node (so reference targets fetched out
    /// of listing order still locate under their collection).
    coll_nodes: RefCell<HashMap<String, NodeId>>,
}

impl MongodbAdapter {
    /// Connect to `mongodb://HOST[:PORT]/DATABASE[?options]`.
    pub fn connect(target: &str) -> Result<Self, MongodbError> {
        if !target.starts_with("mongodb://") && !target.starts_with("mongodb+srv://") {
            return Err(MongodbError::Target(target.to_string()));
        }
        let mut opts = ClientOptions::parse(target).run()?;
        let Some(dbname) = opts.default_database.clone() else {
            return Err(MongodbError::Target(format!("{target}: no database in path")));
        };
        if opts.server_selection_timeout.is_none() {
            opts.server_selection_timeout = Some(Duration::from_secs(5));
        }
        let client = Client::with_options(opts)?;
        let db = client.database(&dbname);
        // Probe (connection errors surface here, not mid-query).
        let mut collections = db.list_collection_names().run()?;
        collections.sort();
        Ok(MongodbAdapter {
            db,
            collections,
            nodes: RefCell::new(vec![Node {
                kind: Kind::Root,
                name: None,
                parent: None,
                children: RefCell::new(None),
            }]),
            docs: RefCell::new(HashMap::new()),
            doc_nodes: RefCell::new(HashMap::new()),
            coll_nodes: RefCell::new(HashMap::new()),
        })
    }

    /// A human-readable locator: the document/field path.
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

    fn push_node(&self, kind: Kind, name: Option<String>, parent: Option<NodeId>) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            kind,
            name,
            parent,
            children: RefCell::new(None),
        });
        id
    }

    /// List a collection's documents (one cursor, `_id`-sorted),
    /// caching fields.
    fn list_docs(&self, parent: NodeId, coll: &str) -> Vec<NodeId> {
        let cursor = match self
            .db
            .collection::<Document>(coll)
            .find(doc! {})
            .sort(doc! { "_id": 1 })
            .run()
        {
            Ok(c) => c,
            // First request unreadable: an empty (unreadable)
            // collection, per the adapter's `children` contract.
            Err(_) => return Vec::new(),
        };
        let mut ids = Vec::new();
        for d in cursor {
            let d = match d {
                Ok(d) => d,
                // A failure mid-cursor truncates the listing.
                // `children` has no error channel, and swallowing
                // it would cache a silently-short collection (a
                // wrong count) for the adapter's lifetime. Fail
                // loud rather than return a partial answer.
                Err(e) => panic!("mongodb: listing {coll} truncated mid-cursor: {e}"),
            };
            let raw_id = d.get("_id").unwrap_or(&Bson::Null);
            let doc_id = id_str(raw_id);
            let path = doc_key(coll, raw_id);
            let fields: Vec<(String, Field)> =
                d.iter().map(|(k, v)| (k.clone(), decode(v))).collect();
            self.docs.borrow_mut().insert(path.clone(), fields);
            let id = self.push_node(Kind::Doc { path: path.clone() }, Some(doc_id), Some(parent));
            self.doc_nodes.borrow_mut().insert(path, id);
            ids.push(id);
        }
        ids
    }

    /// The node for a DBRef target, fetching the document if its
    /// collection was not already listed. A reference into
    /// another database (`$db`) does not resolve.
    fn doc_node(&self, coll: &str, id: &Bson, db: Option<&str>) -> Option<NodeId> {
        if db.is_some_and(|d| d != self.db.name()) {
            return None;
        }
        let path = doc_key(coll, id);
        if let Some(&n) = self.doc_nodes.borrow().get(&path) {
            return Some(n);
        }
        // Fetch the single document. It is interned under its
        // collection's node when that exists (the usual case —
        // any query from the root materializes the collection
        // level), so its locator reads `/albums/the-planets`
        // whether it arrived by listing or by reference.
        let d = self
            .db
            .collection::<Document>(coll)
            .find_one(doc! { "_id": id.clone() })
            .run()
            .ok()??;
        let fields: Vec<(String, Field)> = d.iter().map(|(k, v)| (k.clone(), decode(v))).collect();
        self.docs.borrow_mut().insert(path.clone(), fields);
        let parent = self.coll_nodes.borrow().get(coll).copied();
        let n = self.push_node(Kind::Doc { path: path.clone() }, Some(id_str(id)), parent);
        self.doc_nodes.borrow_mut().insert(path, n);
        Some(n)
    }

    fn field_children(&self, parent: NodeId, f: &Field) -> Vec<NodeId> {
        match f {
            Field::Map(entries) => entries
                .iter()
                .map(|(k, v)| {
                    self.push_node(
                        Kind::Field { value: v.clone() },
                        Some(k.clone()),
                        Some(parent),
                    )
                })
                .collect(),
            Field::Array(items) => items
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    self.push_node(
                        Kind::Field { value: v.clone() },
                        Some(i.to_string()),
                        Some(parent),
                    )
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A reference field's display form: the target path,
    /// prefixed with the database when the reference leaves ours.
    fn ref_value(&self, coll: &str, id: &Bson, db: Option<&str>) -> Value {
        match db {
            Some(d) if d != self.db.name() => Value::Str(format!("{d}/{coll}/{}", id_str(id))),
            _ => Value::Str(format!("{coll}/{}", id_str(id))),
        }
    }
}

impl AstAdapter for MongodbAdapter {
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
        // Extract what we need before pushing nodes (which
        // borrows mutably).
        enum Plan {
            Root,
            Coll(String),
            Doc(String),
            Field(Field),
        }
        let plan = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => Plan::Root,
            Kind::Collection { name } => Plan::Coll(name.clone()),
            Kind::Doc { path } => Plan::Doc(path.clone()),
            Kind::Field { value } => Plan::Field(value.clone()),
        };
        let ids = match plan {
            Plan::Root => self
                .collections
                .iter()
                .map(|c| {
                    let id = self.push_node(
                        Kind::Collection { name: c.clone() },
                        Some(c.clone()),
                        Some(node),
                    );
                    self.coll_nodes.borrow_mut().insert(c.clone(), id);
                    id
                })
                .collect(),
            Plan::Coll(name) => self.list_docs(node, &name),
            Plan::Doc(path) => {
                let fields = self.docs.borrow().get(&path).cloned().unwrap_or_default();
                fields
                    .iter()
                    .map(|(k, v)| {
                        self.push_node(
                            Kind::Field { value: v.clone() },
                            Some(k.clone()),
                            Some(node),
                        )
                    })
                    .collect()
            }
            Plan::Field(value) => self.field_children(node, &value),
        };
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.nodes.borrow()[node.0 as usize].name.clone()
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// `<collection>`, `<document>`, `<map>`, `<array>`,
    /// `<reference>`, or the scalar's kind (`<instant>` for BSON
    /// dates).
    fn traits(&self, node: NodeId) -> Vec<String> {
        let t = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Root => return Vec::new(),
            Kind::Collection { .. } => "collection",
            Kind::Doc { .. } => "document",
            Kind::Field { value } => match value {
                Field::Map(_) => "map",
                Field::Array(_) => "array",
                Field::Reference { .. } => "reference",
                Field::Scalar(Value::Str(_)) => "string",
                Field::Scalar(Value::Int(_) | Value::Float(_)) => "number",
                Field::Scalar(Value::Bool(_)) => "boolean",
                Field::Scalar(Value::Instant { .. }) => "instant",
                Field::Scalar(_) => "null",
            },
        };
        vec![t.to_string()]
    }

    /// A document's scalar fields (`::title`); a DBRef field
    /// answers its target path.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { path } => path.clone(),
            _ => return None,
        };
        let docs = self.docs.borrow();
        let fields = docs.get(&path)?;
        match fields.iter().find(|(k, _)| k == name)? {
            (_, Field::Scalar(v)) => match v {
                Value::Null => None,
                other => Some(other.clone()),
            },
            (_, Field::Reference { coll, id, db }) => {
                Some(self.ref_value(coll, id, db.as_deref()))
            }
            _ => None,
        }
    }

    /// A scalar field node projects to its value; a DBRef to its
    /// target path.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Field {
                value: Field::Scalar(v),
            } => Some(v.clone()),
            Kind::Field {
                value: Field::Reference { coll, id, db },
            } => Some(self.ref_value(coll, id, db.as_deref())),
            _ => None,
        }
    }

    /// `;;;path`, `;;;n-fields` (a document's top-level field
    /// count), and `;;;length` (element count for an array or
    /// embedded document, character count for a string).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "path" => Some(Value::Str(self.locator(node))),
            "n-fields" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Doc { path } => Some(Value::Int(self.docs.borrow().get(path)?.len() as i64)),
                _ => None,
            },
            "length" => match &self.nodes.borrow()[node.0 as usize].kind {
                Kind::Field { value: Field::Array(items) } => {
                    Some(Value::Int(items.len() as i64))
                }
                Kind::Field { value: Field::Map(entries) } => {
                    Some(Value::Int(entries.len() as i64))
                }
                Kind::Field {
                    value: Field::Scalar(Value::Str(s)),
                } => Some(Value::Int(s.chars().count() as i64)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `::field~>` follows a DBRef — no schema, no refs file; the
    /// field knows its target.
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { path } => path.clone(),
            _ => return None,
        };
        let (coll, id, db) = {
            let docs = self.docs.borrow();
            let fields = docs.get(&path)?;
            match fields.iter().find(|(k, _)| k == property)? {
                (_, Field::Reference { coll, id, db }) => {
                    (coll.clone(), id.clone(), db.clone())
                }
                _ => return None,
            }
        };
        self.doc_node(&coll, &id, db.as_deref())
    }

    /// Every DBRef field is an outgoing crosslink, labeled by the
    /// field name.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let path = match &self.nodes.borrow()[node.0 as usize].kind {
            Kind::Doc { path } => path.clone(),
            _ => return Vec::new(),
        };
        let refs: Vec<(String, String, Bson, Option<String>)> = {
            let docs = self.docs.borrow();
            let Some(fields) = docs.get(&path) else {
                return Vec::new();
            };
            fields
                .iter()
                .filter_map(|(k, v)| match v {
                    Field::Reference { coll, id, db } => {
                        Some((k.clone(), coll.clone(), id.clone(), db.clone()))
                    }
                    _ => None,
                })
                .collect()
        };
        refs.into_iter()
            .filter_map(|(k, coll, id, db)| {
                self.doc_node(&coll, &id, db.as_deref()).map(|n| (k, n))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::oid::ObjectId;

    #[test]
    fn scalars_decode() {
        assert!(matches!(
            decode(&Bson::String("x".into())),
            Field::Scalar(Value::Str(s)) if s == "x"
        ));
        assert!(matches!(
            decode(&Bson::Int32(5)),
            Field::Scalar(Value::Int(5))
        ));
        assert!(matches!(
            decode(&Bson::Int64(-7)),
            Field::Scalar(Value::Int(-7))
        ));
        assert!(matches!(
            decode(&Bson::Double(1.5)),
            Field::Scalar(Value::Float(f)) if f == 1.5
        ));
        assert!(matches!(
            decode(&Bson::Boolean(true)),
            Field::Scalar(Value::Bool(true))
        ));
        assert!(matches!(decode(&Bson::Null), Field::Scalar(Value::Null)));
    }

    #[test]
    fn dates_become_instants() {
        let dt = mongodb::bson::DateTime::from_millis(1_700_000_000_250);
        match decode(&Bson::DateTime(dt)) {
            Field::Scalar(Value::Instant {
                secs,
                nanos,
                offset_min,
            }) => {
                assert_eq!(secs, 1_700_000_000);
                assert_eq!(nanos, 250_000_000);
                assert_eq!(offset_min, None);
            }
            _ => panic!("expected instant"),
        }
    }

    #[test]
    fn dbref_detected() {
        let oid = ObjectId::new();
        let d = doc! { "$ref": "albums", "$id": oid };
        match decode(&Bson::Document(d)) {
            Field::Reference { coll, id, db } => {
                assert_eq!(coll, "albums");
                assert_eq!(id, Bson::ObjectId(oid));
                assert_eq!(db, None);
            }
            _ => panic!("expected reference"),
        }
        // The optional `$db` is carried along.
        let d = doc! { "$ref": "albums", "$id": "x", "$db": "other" };
        assert!(matches!(
            decode(&Bson::Document(d)),
            Field::Reference { db: Some(db), .. } if db == "other"
        ));
        // A plain embedded document stays a map.
        assert!(matches!(
            decode(&Bson::Document(doc! { "a": 1 })),
            Field::Map(_)
        ));
    }

    #[test]
    fn doc_keys_keep_the_id_type() {
        // `_id: 5` and `_id: "5"` display identically but must
        // not share a cache slot.
        assert_eq!(id_str(&Bson::Int32(5)), id_str(&Bson::String("5".into())));
        assert_ne!(
            doc_key("c", &Bson::Int32(5)),
            doc_key("c", &Bson::String("5".into()))
        );
    }

    #[test]
    fn id_renderings() {
        let oid = ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap();
        assert_eq!(id_str(&Bson::ObjectId(oid)), "507f1f77bcf86cd799439011");
        assert_eq!(id_str(&Bson::String("mars".into())), "mars");
        assert_eq!(id_str(&Bson::Int32(5)), "5");
    }

    #[test]
    fn target_needs_scheme_and_database() {
        assert!(matches!(
            MongodbAdapter::connect("http://localhost/x"),
            Err(MongodbError::Target(_))
        ));
        assert!(matches!(
            MongodbAdapter::connect("mongodb://localhost:27017"),
            Err(MongodbError::Target(_))
        ));
    }
}
