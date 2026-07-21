//! Google BigQuery adapter for the Quarb query engine.
//!
//! A thin catalog driver over the shared relational model
//! (`quarb-relational`), speaking the BigQuery REST API directly
//! (`jobs.query`) with a plain synchronous HTTP client — no cloud
//! SDK, no async runtime. The catalog comes from
//! `INFORMATION_SCHEMA` (tables, columns, and the *unenforced*
//! `PRIMARY KEY` / `FOREIGN KEY` constraints BigQuery records as
//! metadata — declare them and the whole `~>` / `->` / `<~`
//! reference machinery works on BigQuery like on any engine).
//!
//! **Cost model.** BigQuery bills by bytes scanned, which makes
//! the execution ladder a billing instrument: rows load lazily
//! (untouched tables are never queried), pushdown compiles
//! safe-set queries to SQL that runs — and is billed — server-side
//! on only the columns it names, and partial pushdown filters the
//! fetch. `--save` the reduction once, iterate locally for free.
//!
//! **Authentication.** A bearer token, resolved in order: the
//! `QUARB_BQ_TOKEN` environment variable; else `gcloud auth
//! print-access-token [account]` (the `account` taken from the
//! target's `?account=` parameter when present). Tokens are
//! short-lived; the adapter fetches one per connect.
//!
//! **Target syntax** (what `qua` accepts where a path would go):
//! `bigquery://PROJECT/DATASET[?account=EMAIL]`.

use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use serde_json::json;

/// An error connecting to or querying BigQuery.
#[derive(Debug, thiserror::Error)]
pub enum BigqueryError {
    #[error("bigquery: {0}")]
    Http(#[from] Box<ureq::Error>),
    #[error("pushdown plan: {0}")]
    Plan(String),
    #[error("bigquery auth: {0}")]
    Auth(String),
    #[error("bigquery: {0}")]
    Api(String),
    #[error("bigquery target: {0} (expected bigquery://PROJECT/DATASET[?account=EMAIL])")]
    Target(String),
}

/// A parsed `bigquery://PROJECT/DATASET[?account=EMAIL]` target.
#[derive(Clone)]
struct Target {
    project: String,
    dataset: String,
    account: Option<String>,
}

fn parse_target(target: &str) -> Result<Target, BigqueryError> {
    let rest = target
        .strip_prefix("bigquery://")
        .ok_or_else(|| BigqueryError::Target(target.to_string()))?;
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let (project, dataset) = path
        .split_once('/')
        .ok_or_else(|| BigqueryError::Target(target.to_string()))?;
    if project.is_empty() || dataset.is_empty() || dataset.contains('/') {
        return Err(BigqueryError::Target(target.to_string()));
    }
    let account = query.and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("account=").map(str::to_string))
    });
    Ok(Target {
        project: project.to_string(),
        dataset: dataset.to_string(),
        account,
    })
}

/// A bearer token: `QUARB_BQ_TOKEN`, else `gcloud auth
/// print-access-token [account]`.
fn token(account: Option<&str>) -> Result<String, BigqueryError> {
    if let Ok(t) = std::env::var("QUARB_BQ_TOKEN")
        && !t.trim().is_empty()
    {
        return Ok(t.trim().to_string());
    }
    let mut cmd = std::process::Command::new("gcloud");
    cmd.args(["auth", "print-access-token"]);
    if let Some(a) = account {
        cmd.arg(a);
    }
    let out = cmd
        .output()
        .map_err(|e| BigqueryError::Auth(format!("running gcloud: {e}")))?;
    if !out.status.success() {
        return Err(BigqueryError::Auth(format!(
            "gcloud auth print-access-token failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A result set: the schema fields `(name, type)` and the rows.
type ResultSet = (Vec<(String, String)>, Vec<Vec<Value>>);

/// The REST client: one project, one token.
struct Client {
    project: String,
    token: String,
}

impl Client {
    /// Run a SQL statement to completion, following result pages;
    /// returns the schema fields `(name, type)` and the rows.
    fn sql(&self, query: &str) -> Result<ResultSet, BigqueryError> {
        let url = format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/queries",
            self.project
        );
        let resp: serde_json::Value = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(json!({"query": query, "useLegacySql": false}))
            .map_err(Box::new)?
            .into_json()
            .map_err(|e| BigqueryError::Api(format!("decoding response: {e}")))?;
        if let Some(err) = resp.pointer("/error/message").and_then(|v| v.as_str()) {
            return Err(BigqueryError::Api(err.to_string()));
        }

        let mut resp = resp;
        // Poll until the job completes (small queries usually
        // return complete on the first call).
        while resp.pointer("/jobComplete") == Some(&serde_json::Value::Bool(false)) {
            let job = resp
                .pointer("/jobReference/jobId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| BigqueryError::Api("incomplete job without an id".into()))?
                .to_string();
            std::thread::sleep(std::time::Duration::from_millis(250));
            resp = self.get_results(&job, None)?;
        }

        let schema: Vec<(String, String)> = resp
            .pointer("/schema/fields")
            .and_then(|v| v.as_array())
            .map(|fields| {
                fields
                    .iter()
                    .map(|f| {
                        (
                            f["name"].as_str().unwrap_or_default().to_string(),
                            f["type"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut rows = Vec::new();
        loop {
            if let Some(page) = resp.pointer("/rows").and_then(|v| v.as_array()) {
                for r in page {
                    let cells = r["f"].as_array().cloned().unwrap_or_default();
                    rows.push(
                        cells
                            .iter()
                            .zip(&schema)
                            .map(|(c, (_, ty))| cell(&c["v"], ty))
                            .collect(),
                    );
                }
            }
            let Some(next) = resp
                .pointer("/pageToken")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            else {
                break;
            };
            let job = resp
                .pointer("/jobReference/jobId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| BigqueryError::Api("paged job without an id".into()))?
                .to_string();
            resp = self.get_results(&job, Some(&next))?;
        }
        Ok((schema, rows))
    }

    fn get_results(
        &self,
        job_id: &str,
        page: Option<&str>,
    ) -> Result<serde_json::Value, BigqueryError> {
        let mut url = format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/queries/{job_id}",
            self.project
        );
        if let Some(p) = page {
            url.push_str(&format!("?pageToken={p}"));
        }
        ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(Box::new)?
            .into_json()
            .map_err(|e| BigqueryError::Api(format!("decoding response: {e}")))
    }
}

/// One result cell: the JSON API returns scalars stringly, typed
/// by the schema (`INTEGER`/`INT64`, `FLOAT`/`FLOAT64`,
/// `BOOLEAN`/`BOOL`; everything else stays text).
fn cell(v: &serde_json::Value, ty: &str) -> Value {
    let Some(s) = v.as_str() else {
        return Value::Null;
    };
    match ty {
        "INTEGER" | "INT64" => s.parse().map(Value::Int).unwrap_or(Value::Null),
        "FLOAT" | "FLOAT64" => s.parse().map(Value::Float).unwrap_or(Value::Null),
        "BOOLEAN" | "BOOL" => Value::Bool(s == "true"),
        _ => Value::Str(s.to_string()),
    }
}

/// A BigQuery dataset, exposed as an arbor.
pub struct BigqueryAdapter {
    model: RelationalModel,
}

impl BigqueryAdapter {
    /// Connect and introspect `bigquery://PROJECT/DATASET`; rows
    /// load lazily, per table on first touch (each fetch is one
    /// billed query over only that table's columns).
    pub fn connect(target: &str) -> Result<Self, BigqueryError> {
        Self::connect_impl(target, None)
    }

    /// [`connect`], with one table's fetch filtered by a WHERE
    /// clause (partial pushdown; the engine re-applies the
    /// predicates).
    pub fn connect_filtered(
        target: &str,
        table: &str,
        where_sql: &str,
    ) -> Result<Self, BigqueryError> {
        Self::connect_impl(target, Some((table.to_string(), where_sql.to_string())))
    }

    fn connect_impl(target: &str, filter: Option<(String, String)>) -> Result<Self, BigqueryError> {
        let t = parse_target(target)?;
        let client = Client {
            project: t.project.clone(),
            token: token(t.account.as_deref())?,
        };
        let specs = introspect(&client, &t.dataset)?;
        let dataset = t.dataset.clone();
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |_, spec| {
                let w = filter
                    .as_ref()
                    .filter(|(tn, _)| *tn == spec.name)
                    .map(|(_, w)| w.as_str());
                fetch_rows(&client, &dataset, spec, w).map_err(|e| e.to_string())
            }),
        );
        Ok(BigqueryAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

/// The catalog: tables, columns, and the unenforced key
/// constraints, via `INFORMATION_SCHEMA`.
fn introspect(client: &Client, dataset: &str) -> Result<Vec<TableSpec>, BigqueryError> {
    let (_, tables) = client.sql(&format!(
        "SELECT table_name FROM {dataset}.INFORMATION_SCHEMA.TABLES \
         WHERE table_type = 'BASE TABLE' ORDER BY table_name"
    ))?;
    let (_, cols) = client.sql(&format!(
        "SELECT table_name, column_name FROM {dataset}.INFORMATION_SCHEMA.COLUMNS \
         ORDER BY table_name, ordinal_position"
    ))?;
    let (_, pks) = client.sql(&format!(
        "SELECT kcu.table_name, kcu.column_name \
         FROM {dataset}.INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
         JOIN {dataset}.INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
           ON tc.constraint_name = kcu.constraint_name \
         WHERE tc.constraint_type = 'PRIMARY KEY' \
         ORDER BY kcu.table_name, kcu.ordinal_position"
    ))?;
    let (_, fks) = client.sql(&format!(
        "SELECT kcu.table_name, kcu.column_name, ccu.table_name, ccu.column_name \
         FROM {dataset}.INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
         JOIN {dataset}.INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
           ON tc.constraint_name = kcu.constraint_name \
         JOIN {dataset}.INFORMATION_SCHEMA.CONSTRAINT_COLUMN_USAGE ccu \
           ON ccu.constraint_name = kcu.constraint_name \
         WHERE tc.constraint_type = 'FOREIGN KEY'"
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
        let table_pks: Vec<String> = pks
            .iter()
            .filter(|r| text(&r[0]) == name)
            .map(|r| text(&r[1]))
            .collect();
        let pk = (table_pks.len() == 1)
            .then(|| columns.iter().position(|c| *c == table_pks[0]))
            .flatten();
        let table_fks = fks
            .iter()
            .filter(|r| text(&r[0]) == name)
            .filter_map(|r| {
                columns
                    .iter()
                    .position(|c| *c == text(&r[1]))
                    .map(|idx| (idx, text(&r[2]), text(&r[3])))
            })
            .collect();
        out.push(TableSpec {
            name,
            columns,
            pk,
            fks: table_fks,
        });
    }
    Ok(out)
}

/// Stream one table's rows, optionally filtered; order follows the
/// key when one names rows.
fn fetch_rows(
    client: &Client,
    dataset: &str,
    spec: &TableSpec,
    where_sql: Option<&str>,
) -> Result<Vec<RowSpec>, BigqueryError> {
    let cols: Vec<String> = spec.columns.iter().map(|c| format!("`{c}`")).collect();
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    let order = match spec.pk {
        Some(i) => format!(" ORDER BY `{}`", spec.columns[i]),
        None => String::new(),
    };
    let (_, rows) = client.sql(&format!(
        "SELECT {} FROM {dataset}.`{}`{filter}{order}",
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
/// ordered by `order_table`'s key when one is given. Table
/// references in the SQL are dataset-qualified here.
pub fn raw_query(
    target: &str,
    sql: &str,
    order_table: Option<&str>,
    join_left: Option<(&str, &[String])>,
) -> Result<(Vec<String>, Vec<Vec<Value>>), BigqueryError> {
    // Witness-JOIN plans carry a uniqueness obligation this
    // driver does not yet verify against its catalog; decline
    // so the caller falls back to the (sound) scan.
    if join_left.is_some() {
        return Err(BigqueryError::Plan(
            "witness-JOIN uniqueness not verified by this driver".into(),
        ));
    }

    let t = parse_target(target)?;
    let client = Client {
        project: t.project.clone(),
        token: token(t.account.as_deref())?,
    };
    // The pushdown SQL names bare tables; BigQuery needs
    // dataset-qualified references.
    let sql = qualify_tables(sql, &t.dataset);
    let sql = match order_table {
        Some(table) => {
            let specs = introspect(&client, &t.dataset)?;
            let key = specs
                .into_iter()
                .find(|s| s.name == table)
                .and_then(|s| s.pk.map(|i| s.columns[i].clone()));
            match key {
                Some(k) => format!("{sql} ORDER BY `{table}`.`{k}`"),
                None => sql,
            }
        }
        None => sql,
    };
    let (schema, rows) = client.sql(&sql)?;
    Ok((schema.into_iter().map(|(n, _)| n).collect(), rows))
}

/// Qualify the `FROM x` / `JOIN y` table references with the
/// dataset (the pushdown translator emits bare names).
///
/// Only `FROM`/`JOIN` tokens *outside* a string literal are
/// keywords: a `from` or `join` occurring inside a pushed-down
/// comparison value (e.g. `text = 'orders from paris'`) must not
/// trigger requalification, which would silently corrupt the
/// literal. String literals are single-quoted with `''` escaping
/// (see `quarb-sql`'s pushdown translator), so quote parity
/// tracks the in-string state exactly — a doubled `''` toggles
/// twice, staying in-string.
fn qualify_tables(sql: &str, dataset: &str) -> String {
    let mut out = Vec::new();
    let mut qualify_next = false;
    let mut in_string = false;
    for word in sql.split(' ') {
        // Decide keyword/qualify status from the string state at
        // the start of this word, then advance the state past it.
        let at_start_in_string = in_string;
        if word.bytes().filter(|&b| b == b'\'').count() % 2 == 1 {
            in_string = !in_string;
        }
        if qualify_next && !word.is_empty() && !at_start_in_string {
            out.push(format!("{dataset}.{word}"));
            qualify_next = false;
            continue;
        }
        if !at_start_in_string
            && (word.eq_ignore_ascii_case("FROM") || word.eq_ignore_ascii_case("JOIN"))
        {
            qualify_next = true;
        }
        out.push(word.to_string());
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::qualify_tables;

    #[test]
    fn qualifies_bare_from_and_join() {
        assert_eq!(
            qualify_tables("SELECT id FROM notes", "ds"),
            "SELECT id FROM ds.notes"
        );
        assert_eq!(
            qualify_tables("SELECT a FROM x JOIN y ON x.k = y.k", "ds"),
            "SELECT a FROM ds.x JOIN ds.y ON x.k = y.k"
        );
    }

    #[test]
    fn from_inside_string_literal_is_not_a_keyword() {
        // The `from` inside the literal must be left untouched;
        // only the real table reference is qualified.
        assert_eq!(
            qualify_tables(
                "SELECT id FROM notes WHERE text = 'orders from paris'",
                "ds"
            ),
            "SELECT id FROM ds.notes WHERE text = 'orders from paris'"
        );
    }

    #[test]
    fn escaped_quote_keeps_string_tracking_in_sync() {
        // `''` is an escaped quote inside the literal; the trailing
        // real FROM (there is none here) stays correctly tracked and
        // the in-literal `join` is left alone.
        assert_eq!(
            qualify_tables("SELECT id FROM notes WHERE t = 'it''s a join now'", "ds"),
            "SELECT id FROM ds.notes WHERE t = 'it''s a join now'"
        );
    }
}

impl AstAdapter for BigqueryAdapter {
    fn root(&self) -> NodeId {
        self.model.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.model.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.model.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.model.parent(node)
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
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.model.resolve(node, property, hint)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.backlinks(node)
    }
}
