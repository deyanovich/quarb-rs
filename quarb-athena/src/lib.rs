//! Amazon Athena adapter for the Quarb query engine: the S3
//! datalake's query layer, as a thin catalog driver over the
//! shared relational model (`quarb-relational`).
//!
//! Athena runs Trino-flavored SQL over the Glue catalog, so the
//! catalog comes from `information_schema` (tables and columns).
//! Glue declares **no key constraints**, so nothing names rows or
//! feeds the reference machinery by itself; the target's
//! `?key=TABLE:COLUMN[,TABLE:COLUMN…]` parameter nominates
//! per-table keys (the same move the Neo4j driver's `?key=`
//! makes), and rows fall back to their row id otherwise.
//!
//! **Cost model.** Athena bills by bytes scanned, exactly like
//! BigQuery — the execution ladder is a billing instrument. Rows
//! load lazily (untouched tables are never queried), pushdown
//! compiles safe-set queries to SQL that scans only what it
//! names, and `--save` keeps the reduction local.
//!
//! **The flow.** Athena is asynchronous by design: each SQL
//! statement is `StartQueryExecution` → poll `GetQueryExecution`
//! → page `GetQueryResults` (the first page's first row is the
//! header, which the driver skips). Results land in the
//! workgroup's S3 output location — set `?output=s3://…` when
//! the workgroup declares none.
//!
//! **Target**:
//! `athena://DATABASE[?region=R][&workgroup=W][&output=s3://…][&catalog=C][&endpoint=URL][&key=T:C,…]`
//! — credentials and region from the standard chain
//! (see `quarb-aws`).

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};

/// An error connecting to or querying Athena.
#[derive(Debug, thiserror::Error)]
pub enum AthenaError {
    #[error("athena: {0}")]
    Api(String),
    #[error("pushdown plan: {0}")]
    Plan(String),
    #[error("athena target: {0} (expected athena://DATABASE[?region=R&workgroup=W&output=s3://…])")]
    Target(String),
    #[error("athena: no credentials in the chain (set AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or ~/.aws/credentials)")]
    NoCredentials,
}

struct Target {
    database: String,
    region: String,
    endpoint: String,
    workgroup: Option<String>,
    output: Option<String>,
    catalog: Option<String>,
    /// `?key=` nominations: (table, column).
    keys: Vec<(String, String)>,
}

fn parse_target(target: &str) -> Result<Target, AthenaError> {
    let rest = target
        .strip_prefix("athena://")
        .or_else(|| target.strip_prefix("athena:"))
        .ok_or_else(|| AthenaError::Target(target.to_string()))?;
    let (database, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    if database.is_empty() {
        return Err(AthenaError::Target(target.to_string()));
    }
    let param = |k: &str| {
        query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
        })
    };
    let region = quarb_aws::region(param("region").as_deref());
    let endpoint = param("endpoint")
        .map(|e| e.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://athena.{region}.amazonaws.com"));
    let keys = param("key")
        .map(|spec| {
            spec.split(',')
                .filter_map(|kv| {
                    kv.split_once(':')
                        .map(|(t, c)| (t.to_string(), c.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Target {
        database: database.to_string(),
        region,
        endpoint,
        workgroup: param("workgroup"),
        output: param("output"),
        catalog: param("catalog"),
        keys,
    })
}

struct Client {
    creds: quarb_aws::Credentials,
    t: Target,
}

impl Client {
    fn call(&self, op: &str, body: &serde_json::Value) -> Result<serde_json::Value, AthenaError> {
        let payload = body.to_string();
        let target = format!("AmazonAthena.{op}");
        let extra = [
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target.as_str()),
        ];
        let headers = quarb_aws::sign(
            &self.creds,
            "POST",
            &self.t.endpoint,
            &self.t.region,
            "athena",
            payload.as_bytes(),
            &extra,
        );
        let mut req = ureq::post(&self.t.endpoint);
        for (k, v) in &headers {
            if k != "host" {
                req = req.set(k, v);
            }
        }
        let resp = match req.send_string(&payload) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/message")
                            .or_else(|| v.pointer("/Message"))
                            .and_then(|m| m.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or(text);
                return Err(AthenaError::Api(format!("{op}: {code}: {msg}")));
            }
            Err(e) => return Err(AthenaError::Api(format!("{op}: {e}"))),
        };
        serde_json::from_str(
            &resp
                .into_string()
                .map_err(|e| AthenaError::Api(format!("{op}: reading response: {e}")))?,
        )
        .map_err(|e| AthenaError::Api(format!("{op}: parsing response: {e}")))
    }

    /// Run one statement through the async flow; the typed
    /// columns and rows, header row skipped.
    fn sql(&self, sql: &str) -> Result<(Vec<(String, String)>, Vec<Vec<Value>>), AthenaError> {
        let mut start = serde_json::Map::new();
        start.insert("QueryString".into(), sql.into());
        let mut ctx = serde_json::Map::new();
        ctx.insert("Database".into(), self.t.database.clone().into());
        if let Some(c) = &self.t.catalog {
            ctx.insert("Catalog".into(), c.clone().into());
        }
        start.insert("QueryExecutionContext".into(), ctx.into());
        if let Some(w) = &self.t.workgroup {
            start.insert("WorkGroup".into(), w.clone().into());
        }
        if let Some(o) = &self.t.output {
            start.insert(
                "ResultConfiguration".into(),
                serde_json::json!({ "OutputLocation": o }),
            );
        }
        let resp = self.call("StartQueryExecution", &serde_json::Value::Object(start))?;
        let id = resp
            .pointer("/QueryExecutionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AthenaError::Api("no QueryExecutionId".into()))?
            .to_string();

        // Poll to completion (Athena has no sync path). Capped
        // exponential backoff; a stuck query surfaces after ~5
        // minutes rather than hanging forever.
        let mut wait_ms = 100u64;
        let mut waited = 0u64;
        loop {
            let resp = self.call(
                "GetQueryExecution",
                &serde_json::json!({ "QueryExecutionId": id }),
            )?;
            match resp
                .pointer("/QueryExecution/Status/State")
                .and_then(|v| v.as_str())
            {
                Some("SUCCEEDED") => break,
                Some("FAILED") | Some("CANCELLED") => {
                    let reason = resp
                        .pointer("/QueryExecution/Status/StateChangeReason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("query failed");
                    return Err(AthenaError::Api(reason.to_string()));
                }
                _ => {
                    if waited > 300_000 {
                        return Err(AthenaError::Api(format!(
                            "query {id} still running after {waited} ms"
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                    waited += wait_ms;
                    wait_ms = (wait_ms * 2).min(2000);
                }
            }
        }

        let mut columns: Vec<(String, String)> = Vec::new();
        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut token: Option<String> = None;
        let mut first_page = true;
        loop {
            let mut body = serde_json::Map::new();
            body.insert("QueryExecutionId".into(), id.clone().into());
            if let Some(t) = &token {
                body.insert("NextToken".into(), t.clone().into());
            }
            let resp = self.call("GetQueryResults", &serde_json::Value::Object(body))?;
            if columns.is_empty()
                && let Some(infos) = resp
                    .pointer("/ResultSet/ResultSetMetadata/ColumnInfo")
                    .and_then(|v| v.as_array())
            {
                columns = infos
                    .iter()
                    .map(|c| {
                        (
                            c.pointer("/Name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            c.pointer("/Type")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect();
            }
            if let Some(rs) = resp.pointer("/ResultSet/Rows").and_then(|v| v.as_array()) {
                let mut iter = rs.iter();
                if first_page {
                    // The first row of the first page is the
                    // header (SELECT statements only; it echoes
                    // the column names) — skip it when it does.
                    if let Some(first) = rs.first() {
                        let echoes_header = first
                            .pointer("/Data")
                            .and_then(|v| v.as_array())
                            .is_some_and(|cells| {
                                cells.len() == columns.len()
                                    && cells.iter().zip(&columns).all(|(c, (n, _))| {
                                        c.pointer("/VarCharValue")
                                            .and_then(|v| v.as_str())
                                            == Some(n.as_str())
                                    })
                            });
                        if echoes_header {
                            iter.next();
                        }
                    }
                }
                for r in iter {
                    let cells = r
                        .pointer("/Data")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    rows.push(
                        cells
                            .iter()
                            .enumerate()
                            .map(|(i, c)| {
                                let text = c.pointer("/VarCharValue").and_then(|v| v.as_str());
                                typed(
                                    text,
                                    columns.get(i).map(|(_, t)| t.as_str()).unwrap_or(""),
                                )
                            })
                            .collect(),
                    );
                }
            }
            first_page = false;
            token = resp
                .pointer("/NextToken")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        Ok((columns, rows))
    }
}

/// Convert one cell by its declared Athena/Trino type.
fn typed(text: Option<&str>, ty: &str) -> Value {
    let Some(s) = text else { return Value::Null };
    match ty {
        "tinyint" | "smallint" | "integer" | "int" | "bigint" => s
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(s.to_string())),
        "double" | "float" | "real" | "decimal" => s
            .parse::<f64>()
            .map(Value::Float)
            .unwrap_or_else(|_| Value::Str(s.to_string())),
        "boolean" => match s {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::Str(s.to_string()),
        },
        _ => Value::Str(s.to_string()),
    }
}

/// An Athena database, exposed on the shared relational model.
pub struct AthenaAdapter {
    model: RelationalModel,
}

impl AthenaAdapter {
    /// Connect and introspect `athena://DATABASE`; rows load
    /// lazily, per table on first touch (each fetch is one billed
    /// scan of only that table's columns).
    pub fn connect(target: &str) -> Result<Self, AthenaError> {
        Self::connect_impl(target, None)
    }

    /// [`connect`], with one table's fetch filtered by a WHERE
    /// clause (partial pushdown; the engine re-applies the
    /// predicates).
    pub fn connect_filtered(
        target: &str,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, AthenaError> {
        Self::connect_impl(target, Some((table.to_string(), where_sql.to_string())))
    }

    fn connect_impl(
        target: &str,
        filter: Option<(String, String)>,
    ) -> Result<Self, AthenaError> {
        let client = client_for(target)?;
        let specs = introspect(&client)?;
        let db = client.t.database.clone();
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |_, spec| {
                let w = filter
                    .as_ref()
                    .filter(|(tn, _)| *tn == spec.name)
                    .map(|(_, w)| w.as_str());
                fetch_rows(&client, &db, spec, w).map_err(|e| e.to_string())
            }),
        );
        Ok(AthenaAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

fn client_for(target: &str) -> Result<Client, AthenaError> {
    let t = parse_target(target)?;
    let creds = quarb_aws::load_credentials().ok_or(AthenaError::NoCredentials)?;
    Ok(Client { creds, t })
}

/// The catalog: tables and columns from `information_schema`;
/// keys only from the target's `?key=` nominations (Glue declares
/// none).
fn introspect(client: &Client) -> Result<Vec<TableSpec>, AthenaError> {
    let db = &client.t.database;
    let (_, tables) = client.sql(&format!(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = '{db}' ORDER BY table_name"
    ))?;
    let (_, cols) = client.sql(&format!(
        "SELECT table_name, column_name FROM information_schema.columns \
         WHERE table_schema = '{db}' ORDER BY table_name, ordinal_position"
    ))?;
    let text = |v: &Value| v.to_string();
    let mut out = Vec::new();
    for t in &tables {
        let name = text(&t[0]);
        let columns: Vec<String> = cols
            .iter()
            .filter(|r| text(&r[0]) == name)
            .map(|r| text(&r[1]))
            .collect();
        let pk = client
            .t
            .keys
            .iter()
            .find(|(tn, _)| *tn == name)
            .and_then(|(_, c)| columns.iter().position(|col| col == c));
        out.push(TableSpec {
            name,
            columns,
            pk,
            fks: Vec::new(),
        });
    }
    Ok(out)
}

/// Fetch one table's rows, optionally filtered; ordered by the
/// nominated key when one names rows.
fn fetch_rows(
    client: &Client,
    _db: &str,
    spec: &TableSpec,
    where_sql: Option<&str>,
) -> Result<Vec<RowSpec>, AthenaError> {
    let cols: Vec<String> = spec.columns.iter().map(|c| format!("\"{c}\"")).collect();
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    let order = match spec.pk {
        Some(i) => format!(" ORDER BY \"{}\"", spec.columns[i]),
        None => String::new(),
    };
    let (_, rows) = client.sql(&format!(
        "SELECT {} FROM \"{}\"{filter}{order}",
        cols.join(", "),
        spec.name
    ))?;
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(i, values)| RowSpec {
            rowid: i as i64 + 1,
            values,
        })
        .collect())
}

/// Execute pushed-down SQL directly: the column names and rows,
/// ordered by `order_table`'s nominated key when one is given.
pub fn raw_query(
    target: &str,
    sql: &str,
    order_table: Option<&str>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), AthenaError> {
    // Witness-JOIN plans carry a uniqueness obligation this
    // driver cannot verify (Glue has no unique constraints);
    // decline so the caller falls back to the (sound) scan.
    if join_left.is_some() {
        return Err(AthenaError::Plan(
            "witness-JOIN uniqueness not verifiable on Glue".into(),
        ));
    }
    let client = client_for(target)?;
    let sql = match order_table {
        Some(table) => {
            let key = client
                .t
                .keys
                .iter()
                .find(|(tn, _)| *tn == table)
                .map(|(_, c)| c.clone());
            match key {
                Some(k) => format!("{sql} ORDER BY \"{table}\".\"{k}\""),
                None => sql.to_string(),
            }
        }
        None => sql.to_string(),
    };
    let (schema, rows) = client.sql(&sql)?;
    Ok((schema.into_iter().map(|(n, _)| n).collect(), rows))
}

impl AstAdapter for AthenaAdapter {
    fn root(&self) -> NodeId {
        self.model.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.model.children(node)
    }
    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        self.model.children_named(node, name)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.model.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.model.parent(node)
    }
    fn traits(&self, node: NodeId) -> Vec<String> {
        self.model.traits(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.model.property(node, name)
    }
    fn default_value(&self, node: NodeId) -> Option<Value> {
        self.model.default_value(node)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.model.metadata(node, key)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.backlinks(node)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.model.resolve(node, property, hint)
    }
}
