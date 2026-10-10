//! Google BigQuery adapter for the Quarb query engine.
//!
//! A thin catalog driver over the shared relational model
//! (`quarb-relational`), speaking the BigQuery REST API directly
//! (`jobs.query`) with a plain synchronous HTTP client — no cloud
//! SDK, no async runtime. The catalog comes from
//! `INFORMATION_SCHEMA` (tables, columns, and the *unenforced*
//! `PRIMARY KEY` / `FOREIGN KEY` constraints BigQuery records as
//! metadata — declare them and the whole `-->` / `->` / `<--`
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
use quarb_relational::{RelationalModel, RowSpec, TableSpec, where_equals};
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

/// What reading `spec`'s rows under `equals` would cost, before
/// running: the statement's dry run (`jobs.query` with `dryRun`)
/// gives the bytes BigQuery would process — exact for a plain or
/// partitioned table, an upper bound for a clustered one, since
/// cluster pruning happens at execution — and the table's own
/// statistics (`tables.get`) give its row count, an upper bound
/// under an equality. One request each; the model remembers the
/// answer for the run.
fn estimate_table(
    client: &Client,
    dataset: &str,
    spec: &TableSpec,
    equals: &[(usize, Value)],
) -> Result<quarb::Estimate, BigqueryError> {
    let sql = estimate_sql(dataset, spec, equals);
    let bytes = client.dry_run(&sql)?;
    let stats = client.table_stats(dataset, &spec.name)?;
    let rows = match (stats.rows, equals.is_empty()) {
        (Some(n), true) => (n, n),
        (Some(n), false) => (0, n),
        (None, _) => (0, u64::MAX),
    };
    Ok(quarb::Estimate {
        nodes: rows,
        bytes: Some((bytes, bytes)),
        requests: Some(1),
        latency: quarb::Latency::Remote,
        confidence: if stats.clustered || !equals.is_empty() {
            quarb::Confidence::Bound
        } else {
            quarb::Confidence::Exact
        },
    })
}

/// The statement a dry run prices: every column of the table,
/// filtered by the plan's equalities.
fn estimate_sql(dataset: &str, spec: &TableSpec, equals: &[(usize, Value)]) -> String {
    let cols: Vec<String> = spec
        .columns
        .iter()
        .map(|c| quarb_relational::quote_backtick(c))
        .collect();
    let mut sql = format!(
        "SELECT {} FROM `{dataset}`.`{}`",
        cols.join(", "),
        spec.name
    );
    if let Some(w) = where_equals(spec, equals, quarb_relational::quote_backtick) {
        sql.push_str(" WHERE ");
        sql.push_str(&w);
    }
    sql
}

/// A table's statistics from `tables.get`.
struct TableStats {
    rows: Option<u64>,
    clustered: bool,
}

/// The dry run's answer as the API returns it.
fn dry_run_bytes(resp: &serde_json::Value) -> Result<u64, BigqueryError> {
    if let Some(err) = resp.pointer("/error/message").and_then(|v| v.as_str()) {
        return Err(BigqueryError::Api(err.to_string()));
    }
    resp.pointer("/totalBytesProcessed")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| BigqueryError::Api("dry run without totalBytesProcessed".into()))
}

/// A table's statistics as `tables.get` returns them.
fn table_stats_of(resp: &serde_json::Value) -> TableStats {
    TableStats {
        rows: resp
            .pointer("/numRows")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok()),
        clustered: resp.pointer("/clustering").is_some(),
    }
}

/// A result set: the schema fields `(name, type)` and the rows.
type ResultSet = (Vec<(String, String)>, Vec<Vec<Value>>);

/// The REST client: one project, one token.
struct Client {
    project: String,
    token: String,
}

impl Client {
    /// `jobs.query` with `dryRun: true`: the bytes the statement
    /// would process, with nothing run and nothing billed.
    fn dry_run(&self, query: &str) -> Result<u64, BigqueryError> {
        let url = format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/queries",
            self.project
        );
        let resp: serde_json::Value = quarb_http::agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(json!({"query": query, "useLegacySql": false, "dryRun": true}))
            .map_err(Box::new)?
            .into_json()
            .map_err(|e| BigqueryError::Api(format!("decoding dry run: {e}")))?;
        dry_run_bytes(&resp)
    }

    /// `tables.get`: the table's row count and whether it is
    /// clustered.
    fn table_stats(&self, dataset: &str, table: &str) -> Result<TableStats, BigqueryError> {
        let url = format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/datasets/{dataset}/tables/{table}",
            self.project
        );
        let resp: serde_json::Value = quarb_http::agent()
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(Box::new)?
            .into_json()
            .map_err(|e| BigqueryError::Api(format!("decoding table statistics: {e}")))?;
        if let Some(err) = resp.pointer("/error/message").and_then(|v| v.as_str()) {
            return Err(BigqueryError::Api(err.to_string()));
        }
        Ok(table_stats_of(&resp))
    }

    /// Run a SQL statement to completion, following result pages;
    /// returns the schema fields `(name, type)` and the rows.
    fn sql(&self, query: &str) -> Result<ResultSet, BigqueryError> {
        let url = format!(
            "https://bigquery.googleapis.com/bigquery/v2/projects/{}/queries",
            self.project
        );
        let resp: serde_json::Value = quarb_http::agent()
            .post(&url)
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
        quarb_http::agent()
            .get(&url)
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
        let client = std::rc::Rc::new(client);
        let (fetch_client, fetch_dataset) = (client.clone(), dataset.clone());
        let model = RelationalModel::lazy(
            specs,
            Box::new(move |_, spec| {
                let w = filter
                    .as_ref()
                    .filter(|(tn, _)| *tn == spec.name)
                    .map(|(_, w)| w.as_str());
                fetch_rows(&fetch_client, &fetch_dataset, spec, w).map_err(|e| e.to_string())
            }),
        )
        .with_estimator(std::rc::Rc::new(move |_, spec, equals| {
            estimate_table(&client, &dataset, spec, equals).map_err(|e| e.to_string())
        }));
        Ok(BigqueryAdapter { model })
    }

    /// A human-readable locator: `/table/key` for rows.
    /// Surface a filtered fetch's error now (see
    /// `RelationalModel::prefetch`): the partial-pushdown caller
    /// falls back to the scan instead of answering empty.
    pub fn prefetch(&self, table: &str) -> Result<(), String> {
        self.model.prefetch(table)
    }

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
    let cols: Vec<String> = spec
        .columns
        .iter()
        .map(|c| quarb_relational::quote_backtick(c))
        .collect();
    let filter = match where_sql {
        Some(w) => format!(" WHERE {w}"),
        None => String::new(),
    };
    let order = match spec.pk {
        Some(i) => format!(
            " ORDER BY {}",
            quarb_relational::quote_backtick(&spec.columns[i])
        ),
        None => String::new(),
    };
    let (_, rows) = client.sql(&format!(
        "SELECT {} FROM {dataset}.{}{filter}{order}",
        cols.join(", "),
        quarb_relational::quote_backtick(&spec.name)
    ))?;
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(i, values)| RowSpec {
            rowid: Some(i as i64 + 1),
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
    // --explain prints what ran: the ORDER BY key is a
    // catalog lookup made here, so the final statement
    // exists nowhere earlier.
    quarb_relational::record_executed(&sql);
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
    use super::{TableSpec, Value, dry_run_bytes, estimate_sql, table_stats_of};

    #[test]
    fn the_dry_run_statement_and_answer() {
        let spec = TableSpec {
            name: "orders".into(),
            columns: vec!["id".into(), "customer".into(), "total".into()],
            pk: Some(0),
            fks: vec![],
        };
        assert_eq!(
            estimate_sql("shop", &spec, &[]),
            "SELECT `id`, `customer`, `total` FROM `shop`.`orders`"
        );
        assert_eq!(
            estimate_sql(
                "shop",
                &spec,
                &[(1, Value::Str("Emma".into())), (2, Value::Int(9))]
            ),
            "SELECT `id`, `customer`, `total` FROM `shop`.`orders` WHERE `customer` = 'Emma' AND `total` = 9"
        );
        let resp: serde_json::Value = serde_json::from_str(
            r#"{"kind":"bigquery#queryResponse","totalBytesProcessed":"1048576","jobComplete":true}"#,
        )
        .unwrap();
        assert_eq!(dry_run_bytes(&resp).unwrap(), 1_048_576);
        let err: serde_json::Value =
            serde_json::from_str(r#"{"error":{"message":"Not found: Table shop.orders"}}"#)
                .unwrap();
        assert!(dry_run_bytes(&err).is_err());
        let stats = table_stats_of(
            &serde_json::from_str(
                r#"{"numRows":"4200","numBytes":"9","clustering":{"fields":["customer"]}}"#,
            )
            .unwrap(),
        );
        assert_eq!(stats.rows, Some(4200));
        assert!(stats.clustered);
        let plain = table_stats_of(&serde_json::from_str(r#"{"numRows":"3"}"#).unwrap());
        assert_eq!((plain.rows, plain.clustered), (Some(3), false));
    }

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
    // Every trait method, through the relational model: the search
    // fast paths, the plan and its estimate included.
    quarb::forward_ast_adapter!(model);
}
