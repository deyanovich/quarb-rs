//! The Quarb **serve protocol**: any tool exposes its data to
//! `qua` over a child process — no linking, no adapter crate in
//! the engine workspace, no Rust required on the tool's side.
//!
//! The shape is LSP's: the *protocol* is the extension surface.
//! A tool that owns an interesting data model (a note vault, an
//! issue tracker, a build graph) implements a `quarb-serve`
//! subcommand; `qua` spawns it and navigates over line-oriented
//! JSON on stdin/stdout, one request per line, one response per
//! line, mirroring the `AstAdapter` methods:
//!
//! ```text
//! → {"op":"hello"}
//! ← {"serve":1,"name":"cuj"}
//! → {"op":"children","node":0}
//! ← {"nodes":[1,2,3]}
//! → {"op":"property","node":7,"name":"tags"}
//! ← {"value":"inbox urgent"}
//! ```
//!
//! **Server side** (the tool): implement `AstAdapter` over your
//! own model and hand it to [`serve`] — three lines. Non-Rust
//! tools implement the protocol directly; it is nine operations
//! over JSON.
//!
//! **Client side** (`qua`): [`ServeAdapter::spawn`] runs a
//! command line and presents the child as an ordinary adapter —
//! mountable beside every other source, responses cached per
//! node so navigation does not re-ask.
//!
//! **Two wire formats, one logical protocol.** The handshake is
//! always JSON (the universal floor); the server's hello
//! advertises `"formats"`, and when `daiv` is offered — every
//! server built with [`serve`] offers it — the session upgrades:
//! each message becomes a kaiv (`.daiv`) frame, blank-line
//! terminated, with the same fields as typed leaves under
//! `/@serve` — values ride natively typed (`!int`, `!float`,
//! `!bool`, `!str`, `!null`) instead of JSON-tagged. kaiv is the
//! house format and the richer wire; JSON remains for foreign
//! tools that want the five-minute integration. In JSON frames,
//! values wire tagged: `{"t":"int","v":5}`, `{"t":"list","v":
//! [...]}`, `null`. Node ids are opaque `u64`s owned by the
//! server either way. The protocol is read-only by construction:
//! there is no mutating operation to implement.

use quarb::{AstAdapter, NodeId, Value};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};

/// An error spawning or speaking to a served adapter.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("serve: {0}")]
    Io(#[from] std::io::Error),
    #[error("serve: {0}")]
    Protocol(String),
}

// ---------------------------------------------------------------
// Value wire form
// ---------------------------------------------------------------

fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => json!({"t": "bool", "v": b}),
        Value::Int(n) => json!({"t": "int", "v": n}),
        Value::Float(f) => json!({"t": "float", "v": f}),
        Value::Str(s) => json!({"t": "str", "v": s}),
        Value::List(items) => {
            json!({"t": "list", "v": items.iter().map(value_to_json).collect::<Vec<_>>()})
        }
        other => json!({"t": "str", "v": other.to_string()}),
    }
}

fn value_from_json(j: &Json) -> Value {
    if j.is_null() {
        return Value::Null;
    }
    // The daiv wire delivers untagged natives.
    match j {
        Json::Bool(b) => return Value::Bool(*b),
        Json::Number(n) => {
            return n
                .as_i64()
                .map(Value::Int)
                .or_else(|| n.as_f64().map(Value::Float))
                .unwrap_or(Value::Null);
        }
        Json::String(s) if j.pointer("/t").is_none() => return Value::Str(s.clone()),
        Json::Array(a) => return Value::List(a.iter().map(value_from_json).collect()),
        _ => {}
    }
    let t = j.pointer("/t").and_then(|v| v.as_str()).unwrap_or("");
    let v = j.pointer("/v");
    match (t, v) {
        ("bool", Some(v)) => Value::Bool(v.as_bool().unwrap_or(false)),
        ("int", Some(v)) => Value::Int(v.as_i64().unwrap_or(0)),
        ("float", Some(v)) => Value::Float(v.as_f64().unwrap_or(0.0)),
        ("str", Some(v)) => Value::Str(v.as_str().unwrap_or("").to_string()),
        ("list", Some(v)) => Value::List(
            v.as_array()
                .map(|a| a.iter().map(value_from_json).collect())
                .unwrap_or_default(),
        ),
        _ => Value::Null,
    }
}

fn nodes_json(ids: &[NodeId]) -> Json {
    json!(ids.iter().map(|n| n.0).collect::<Vec<u64>>())
}

fn links_json(ls: &[(String, NodeId)]) -> Json {
    json!(
        ls.iter()
            .map(|(l, n)| json!({"label": l, "node": n.0}))
            .collect::<Vec<_>>()
    )
}

// ---------------------------------------------------------------
// The daiv wire: one message = one blank-line-terminated .daiv
// frame; the JSON message shape flattens to typed leaves under
// /@serve (tagged values become native typed leaves).
// ---------------------------------------------------------------

/// Wire type marking a string leaf whose payload is JSON-encoded,
/// used as the fallback when the raw value cannot ride verbatim on
/// a daiv leaf (it leads with `$` or carries a line break). The
/// decoder keys on this type to un-encode; a plain `str` leaf is
/// always verbatim, so a string that merely *looks* like a
/// JSON-quoted literal (e.g. `"quoted"`) round-trips intact instead
/// of being silently un-quoted.
const DAIV_STR_JSON: &str = "str-json";

fn daiv_encode(msg: &Json) -> String {
    let mut b = kaiv::DaivBuilder::new();
    fn put(b: &mut kaiv::DaivBuilder, path: &str, key: &str, v: &Json) {
        // A tagged value ({"t":..,"v":..}) becomes a native leaf.
        if let (Some(t), Some(inner)) = (v.pointer("/t").and_then(|x| x.as_str()), v.pointer("/v"))
        {
            match t {
                "list" => {
                    if let Some(items) = inner.as_array() {
                        for (i, item) in items.iter().enumerate() {
                            put(b, &format!("{path}/{key}"), &i.to_string(), item);
                        }
                        // Mark the list container so empty lists survive.
                        let _ = b.leaf(&format!("{path}/{key}::kind"), "str", "list", None);
                        return;
                    }
                }
                "int" | "float" | "bool" | "str" => {
                    let payload = match inner {
                        Json::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if b.leaf(&format!("{path}::{key}"), t, &payload, None)
                        .is_err()
                    {
                        let _ = b.leaf(
                            &format!("{path}::{key}"),
                            DAIV_STR_JSON,
                            &serde_json::to_string(&payload).unwrap_or_default(),
                            None,
                        );
                    }
                    return;
                }
                _ => {}
            }
        }
        match v {
            Json::Null => {
                let _ = b.leaf(&format!("{path}::{key}"), "null", "", None);
            }
            Json::Bool(x) => {
                let _ = b.leaf(&format!("{path}::{key}"), "bool", &x.to_string(), None);
            }
            Json::Number(n) => {
                let t = if n.is_i64() || n.is_u64() {
                    "int"
                } else {
                    "float"
                };
                let _ = b.leaf(&format!("{path}::{key}"), t, &n.to_string(), None);
            }
            Json::String(x) => {
                if b.leaf(&format!("{path}::{key}"), "str", x, None).is_err() {
                    let _ = b.leaf(
                        &format!("{path}::{key}"),
                        DAIV_STR_JSON,
                        &serde_json::to_string(x).unwrap_or_default(),
                        None,
                    );
                }
            }
            Json::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    put(b, &format!("{path}/{key}"), &i.to_string(), item);
                }
                let _ = b.leaf(&format!("{path}/{key}::kind"), "str", "list", None);
            }
            Json::Object(map) => {
                for (k, val) in map {
                    put(b, &format!("{path}/{key}"), k, val);
                }
            }
        }
    }
    if let Some(map) = msg.as_object() {
        for (k, v) in map {
            put(&mut b, "/@serve", k, v);
        }
    }
    // The flat canonical builder, deliberately: serve frames are a
    // machine wire format the decoder hand-parses line-per-leaf.
    // (qua's user-facing --kaiv export uses KaivBuilder's authored
    // form instead.)
    b.finish()
}

fn daiv_decode(frame: &str) -> Result<Json, String> {
    let lines = kaiv::lex(frame.as_bytes(), kaiv::FileKind::Data)
        .map_err(|e| format!("daiv frame: {e:?}"))?;
    let mut root = Json::Object(serde_json::Map::new());
    for line in lines {
        let kaiv::lexer::LineKind::Content { left, value } = line.kind else {
            continue;
        };
        // left: !TYPE['?src']['#dpid']'NAMEPATH  (machine daiv).
        let Some(rest) = left.strip_prefix('!') else {
            continue;
        };
        let Some(q) = rest.find('\'') else { continue };
        let meta = &rest[..q];
        let namepath = &rest[q + 1..];
        let ty = meta.split(['?', '#']).next().unwrap_or("str");
        let Some(np) = namepath.strip_prefix("/@serve") else {
            continue;
        };
        let (segs, field) = match np.rsplit_once("::") {
            Some((s, f)) => (s, f),
            None => continue,
        };
        let typed: Json = match ty {
            "null" => Json::Null,
            "bool" => Json::Bool(value.trim() == "true"),
            "int" => value
                .trim()
                .parse::<i64>()
                .map(Json::from)
                .unwrap_or(Json::Null),
            "float" => value
                .trim()
                .parse::<f64>()
                .map(|f| json!(f))
                .unwrap_or(Json::Null),
            DAIV_STR_JSON => {
                // The encoder's fallback for a value that could not
                // ride verbatim: un-encode the JSON-quoted payload.
                serde_json::from_str::<String>(value)
                    .map(Json::String)
                    .unwrap_or_else(|_| Json::String(value.to_string()))
            }
            // A plain `str` leaf (and any other type) is verbatim:
            // never guess-decode, so JSON-looking strings survive.
            _ => Json::String(value.to_string()),
        };
        // Walk/create the container path.
        let mut cur = &mut root;
        for seg in segs.split('/').filter(|s| !s.is_empty()) {
            cur = cur
                .as_object_mut()
                .unwrap()
                .entry(seg)
                .or_insert_with(|| Json::Object(serde_json::Map::new()));
            if !cur.is_object() {
                return Err("daiv frame: path collision".into());
            }
        }
        cur.as_object_mut()
            .unwrap()
            .insert(field.to_string(), typed);
    }
    // Objects whose keys are all numeric (plus the list marker)
    // become arrays; leaf objects keep their shape.
    fn arrayify(v: Json) -> Json {
        match v {
            Json::Object(map) => {
                let is_list = map.get("kind").and_then(|k| k.as_str()) == Some("list")
                    && map
                        .keys()
                        .all(|k| k == "kind" || k.parse::<usize>().is_ok());
                if is_list {
                    let mut items: Vec<(usize, Json)> = map
                        .into_iter()
                        .filter(|(k, _)| k != "kind")
                        .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, arrayify(v))))
                        .collect();
                    items.sort_by_key(|(i, _)| *i);
                    Json::Array(items.into_iter().map(|(_, v)| v).collect())
                } else {
                    Json::Object(map.into_iter().map(|(k, v)| (k, arrayify(v))).collect())
                }
            }
            other => other,
        }
    }
    Ok(arrayify(root))
}

/// The session's wire format.
#[derive(Clone, Copy, PartialEq)]
enum Wire {
    Json,
    Daiv,
}

fn write_msg(out: &mut impl Write, wire: Wire, msg: &Json) -> std::io::Result<()> {
    match wire {
        Wire::Json => writeln!(out, "{msg}"),
        Wire::Daiv => {
            let frame = daiv_encode(msg);
            out.write_all(frame.as_bytes())?;
            writeln!(out)
        }
    }?;
    out.flush()
}

/// Read one message: a line (JSON) or a blank-line-terminated
/// frame (daiv). `Ok(None)` on EOF.
fn read_msg(inp: &mut impl BufRead, wire: Wire) -> Result<Option<Json>, String> {
    match wire {
        Wire::Json => {
            let mut line = String::new();
            loop {
                line.clear();
                let n = inp.read_line(&mut line).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Ok(None);
                }
                if !line.trim().is_empty() {
                    return serde_json::from_str(&line)
                        .map(Some)
                        .map_err(|e| format!("bad message: {e}"));
                }
            }
        }
        Wire::Daiv => {
            let mut frame = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                let n = inp.read_line(&mut line).map_err(|e| e.to_string())?;
                if n == 0 {
                    return if frame.trim().is_empty() {
                        Ok(None)
                    } else {
                        daiv_decode(&frame).map(Some)
                    };
                }
                if line.trim().is_empty() {
                    if frame.trim().is_empty() {
                        continue;
                    }
                    return daiv_decode(&frame).map(Some);
                }
                frame.push_str(&line);
            }
        }
    }
}

// ---------------------------------------------------------------
// Server
// ---------------------------------------------------------------

/// Serve `adapter` on stdin/stdout until EOF. This is the whole
/// server side of a tool's `quarb-serve` subcommand:
///
/// ```ignore
/// let vault = my_tool::open(...)?;
/// quarb_serve::serve(&vault.arbor(), |n| vault.locator(n), "my-tool");
/// ```
pub fn serve(
    adapter: &impl AstAdapter,
    locator: impl Fn(NodeId) -> String,
    name: &str,
) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut inp = BufReader::new(stdin.lock());
    let mut out = std::io::stdout().lock();
    let mut wire = Wire::Json;
    loop {
        let req = match read_msg(&mut inp, wire) {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                write_msg(&mut out, wire, &json!({"error": e}))?;
                continue;
            }
        };
        let op = req.pointer("/op").and_then(|v| v.as_str()).unwrap_or("");
        let node = NodeId(req.pointer("/node").and_then(|v| v.as_u64()).unwrap_or(0));
        let name_arg = req.pointer("/name").and_then(|v| v.as_str()).unwrap_or("");
        let resp = match op {
            "hello" => json!({"serve": 1, "name": name, "formats": ["daiv", "json"]}),
            "format" => {
                let want = req.pointer("/format").and_then(|v| v.as_str());
                if want == Some("daiv") {
                    // Ack in the OLD wire, then switch.
                    write_msg(&mut out, wire, &json!({"ok": true}))?;
                    wire = Wire::Daiv;
                    continue;
                }
                json!({"ok": want == Some("json")})
            }
            "root" => json!({"node": adapter.root().0}),
            "children" => json!({"nodes": nodes_json(&adapter.children(node))}),
            "children_named" => {
                json!({"nodes": nodes_json(&adapter.children_named(node, name_arg))})
            }
            "name" => json!({"name": adapter.name(node)}),
            "parent" => json!({"node": adapter.parent(node).map(|n| n.0)}),
            "traits" => json!({"traits": adapter.traits(node)}),
            "property" => json!({"value": adapter
                .property(node, name_arg)
                .as_ref()
                .map(value_to_json)}),
            "default_value" => {
                json!({"value": adapter.default_value(node).as_ref().map(value_to_json)})
            }
            "metadata" => json!({"value": adapter
                .metadata(node, name_arg)
                .as_ref()
                .map(value_to_json)}),
            "resolve" => {
                let hint = req.pointer("/hint").and_then(|v| v.as_str());
                json!({"node": adapter.resolve(node, name_arg, hint).map(|n| n.0)})
            }
            "links" => json!({"links": links_json(&adapter.links(node))}),
            "backlinks" => json!({"links": links_json(&adapter.backlinks(node))}),
            "locator" => json!({"locator": locator(node)}),
            other => json!({"error": format!("unknown op: {other}")}),
        };
        write_msg(&mut out, wire, &resp)?;
    }
    Ok(())
}

// ---------------------------------------------------------------
// Client
// ---------------------------------------------------------------

/// A served adapter: a child process speaking the protocol,
/// presented as an ordinary `AstAdapter`.
pub struct ServeAdapter {
    child: RefCell<std::process::Child>,
    stdin: RefCell<std::process::ChildStdin>,
    stdout: RefCell<BufReader<std::process::ChildStdout>>,
    /// The tool's self-reported name (from the handshake).
    pub name: String,
    /// The negotiated wire (daiv when the server offers it).
    wire: RefCell<Wire>,
    cache: RefCell<HashMap<String, Json>>,
}

impl ServeAdapter {
    /// Spawn `command` (through the shell) and handshake.
    pub fn spawn(command: &str) -> Result<Self, ServeError> {
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut adapter = ServeAdapter {
            child: RefCell::new(child),
            stdin: RefCell::new(stdin),
            stdout: RefCell::new(stdout),
            name: String::new(),
            wire: RefCell::new(Wire::Json),
            cache: RefCell::new(HashMap::new()),
        };
        let hello = adapter.call(json!({"op": "hello"}))?;
        if hello.pointer("/serve").and_then(|v| v.as_u64()) != Some(1) {
            return Err(ServeError::Protocol(
                "handshake failed (no {\"serve\":1})".into(),
            ));
        }
        adapter.name = hello
            .pointer("/name")
            .and_then(|v| v.as_str())
            .unwrap_or("served")
            .to_string();
        // Upgrade to the daiv wire when offered (the default for
        // every first-party server; JSON stays the foreign floor).
        let daiv_offered = hello
            .pointer("/formats")
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|f| f.as_str() == Some("daiv")));
        if daiv_offered {
            let ack = adapter.call(json!({"op": "format", "format": "daiv"}))?;
            if ack.pointer("/ok").and_then(|v| v.as_bool()) == Some(true) {
                *adapter.wire.borrow_mut() = Wire::Daiv;
            }
        }
        Ok(adapter)
    }

    fn call(&self, req: Json) -> Result<Json, ServeError> {
        let key = req.to_string();
        if let Some(c) = self.cache.borrow().get(&key) {
            return Ok(c.clone());
        }
        let wire = *self.wire.borrow();
        write_msg(&mut *self.stdin.borrow_mut(), wire, &req)?;
        let resp = read_msg(&mut *self.stdout.borrow_mut(), wire)
            .map_err(ServeError::Protocol)?
            .ok_or_else(|| ServeError::Protocol("server closed the stream".into()))?;
        if let Some(err) = resp.pointer("/error").and_then(|v| v.as_str()) {
            return Err(ServeError::Protocol(err.to_string()));
        }
        self.cache.borrow_mut().insert(key, resp.clone());
        Ok(resp)
    }

    fn call_ok(&self, req: Json) -> Json {
        self.call(req).unwrap_or(Json::Null)
    }

    /// The served locator (`/jots/41`, whatever the tool says).
    pub fn locator(&self, node: NodeId) -> String {
        self.call_ok(json!({"op": "locator", "node": node.0}))
            .pointer("/locator")
            .and_then(|v| v.as_str())
            .unwrap_or("/")
            .to_string()
    }
}

impl Drop for ServeAdapter {
    fn drop(&mut self) {
        let _ = self.child.borrow_mut().kill();
    }
}

impl AstAdapter for ServeAdapter {
    fn root(&self) -> NodeId {
        NodeId(
            self.call_ok(json!({"op": "root"}))
                .pointer("/node")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.call_ok(json!({"op": "children", "node": node.0}))
            .pointer("/nodes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).map(NodeId).collect())
            .unwrap_or_default()
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.call_ok(json!({"op": "children_named", "node": node.0, "name": name}))
            .pointer("/nodes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_u64()).map(NodeId).collect())
            .unwrap_or_default()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        self.call_ok(json!({"op": "name", "node": node.0}))
            .pointer("/name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.call_ok(json!({"op": "parent", "node": node.0}))
            .pointer("/node")
            .and_then(|v| v.as_u64())
            .map(NodeId)
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        self.call_ok(json!({"op": "traits", "node": node.0}))
            .pointer("/traits")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let resp = self.call_ok(json!({"op": "property", "node": node.0, "name": name}));
        let v = resp.pointer("/value")?;
        if v.is_null() {
            return None;
        }
        Some(value_from_json(v))
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let resp = self.call_ok(json!({"op": "default_value", "node": node.0}));
        let v = resp.pointer("/value")?;
        if v.is_null() {
            return None;
        }
        Some(value_from_json(v))
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let resp = self.call_ok(json!({"op": "metadata", "node": node.0, "name": key}));
        let v = resp.pointer("/value")?;
        if v.is_null() {
            return None;
        }
        Some(value_from_json(v))
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let mut req = json!({"op": "resolve", "node": node.0, "name": property});
        if let Some(h) = hint {
            req["hint"] = json!(h);
        }
        self.call_ok(req)
            .pointer("/node")
            .and_then(|v| v.as_u64())
            .map(NodeId)
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.call_ok(json!({"op": "links", "node": node.0}))
            .pointer("/links")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|l| {
                        Some((
                            l.pointer("/label")?.as_str()?.to_string(),
                            NodeId(l.pointer("/node")?.as_u64()?),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.call_ok(json!({"op": "backlinks", "node": node.0}))
            .pointer("/links")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|l| {
                        Some((
                            l.pointer("/label")?.as_str()?.to_string(),
                            NodeId(l.pointer("/node")?.as_u64()?),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Carry `v` as a server would carry a property value: wrap it in
    /// the `{"value": ...}` response, push it through the daiv wire,
    /// and read `/value` back on the client side.
    fn daiv_roundtrip(v: Json) -> Json {
        let frame = daiv_encode(&json!({ "value": v }));
        let back = daiv_decode(&frame).expect("frame decodes");
        back.pointer("/value").cloned().unwrap_or(Json::Null)
    }

    #[test]
    fn json_looking_string_survives_verbatim() {
        // A string whose content is literally `"quoted"` rides a plain
        // `str` leaf and must not be un-quoted in transit. This was
        // the bug: the decoder guess-decoded any JSON-looking payload.
        let wire = value_to_json(&Value::Str("\"quoted\"".into()));
        assert_eq!(daiv_roundtrip(wire), json!("\"quoted\""));
    }

    #[test]
    fn fallback_encoded_strings_roundtrip() {
        // Values that cannot ride a leaf verbatim (leading `$`, or an
        // embedded line break) take the JSON-encoded fallback and must
        // decode back to the original.
        for original in ["$ref", "line1\nline2"] {
            let wire = value_to_json(&Value::Str(original.to_string()));
            assert_eq!(daiv_roundtrip(wire), json!(original), "for {original:?}");
        }
    }

    #[test]
    fn plain_string_and_scalars_roundtrip() {
        assert_eq!(
            daiv_roundtrip(value_to_json(&Value::Str("hello".into()))),
            json!("hello")
        );
        assert_eq!(daiv_roundtrip(value_to_json(&Value::Int(42))), json!(42));
        assert_eq!(
            daiv_roundtrip(value_to_json(&Value::Bool(true))),
            json!(true)
        );
    }
}
