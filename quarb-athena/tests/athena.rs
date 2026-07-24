//! A bottled Athena: a std-only HTTP server speaking the recorded
//! JSON protocol shapes (StartQueryExecution / GetQueryExecution /
//! GetQueryResults, header row, pagination), answering the
//! introspection and fetch statements the driver actually sends
//! for a two-table music catalog. Fully offline; the mock also
//! asserts every request carries a SigV4 Authorization header.

use std::io::{Read, Write};
use std::net::TcpListener;

use quarb_athena::AthenaAdapter;

fn rows_json(rows: &[Vec<&str>]) -> String {
    let rows: Vec<String> = rows
        .iter()
        .map(|r| {
            let cells: Vec<String> = r
                .iter()
                .map(|c| format!("{{\"VarCharValue\":{}}}", serde_json::json!(c)))
                .collect();
            format!("{{\"Data\":[{}]}}", cells.join(","))
        })
        .collect();
    format!("[{}]", rows.join(","))
}

fn result_set(cols: &[(&str, &str)], rows: &[Vec<&str>], next: Option<&str>) -> String {
    let infos: Vec<String> = cols
        .iter()
        .map(|(n, t)| format!("{{\"Name\":\"{n}\",\"Type\":\"{t}\"}}"))
        .collect();
    let next = match next {
        Some(t) => format!(",\"NextToken\":\"{t}\""),
        None => String::new(),
    };
    format!(
        "{{\"ResultSet\":{{\"ResultSetMetadata\":{{\"ColumnInfo\":[{}]}},\"Rows\":{}}}{next}}}",
        infos.join(","),
        rows_json(rows)
    )
}

/// Serve the bottled catalog until the sender drops.
fn serve(listener: TcpListener) {
    // Queries by execution id, so results can be replayed.
    let mut queries: Vec<String> = Vec::new();
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let (mut headers_end, mut content_length) = (0usize, 0usize);
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if headers_end == 0
                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                headers_end = pos + 4;
                let head = String::from_utf8_lossy(&buf[..pos]);
                assert!(
                    head.to_lowercase().contains("authorization: aws4-hmac-sha256"),
                    "request not SigV4-signed:\n{head}"
                );
                content_length = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap_or(0))
                    })
                    .unwrap_or(0);
            }
            if headers_end > 0 && buf.len() >= headers_end + content_length {
                break;
            }
        }
        if buf.is_empty() {
            break;
        }
        let head = String::from_utf8_lossy(&buf[..headers_end]);
        let body: serde_json::Value =
            serde_json::from_slice(&buf[headers_end..]).unwrap_or_default();
        let op = head
            .lines()
            .find_map(|l| l.to_lowercase().strip_prefix("x-amz-target:").map(str::to_string))
            .unwrap_or_default()
            .trim()
            .trim_start_matches("amazonathena.")
            .to_string();

        let resp = match op.as_str() {
            "startqueryexecution" => {
                let q = body
                    .pointer("/QueryString")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                queries.push(q);
                format!("{{\"QueryExecutionId\":\"q-{}\"}}", queries.len() - 1)
            }
            "getqueryexecution" => {
                r#"{"QueryExecution":{"Status":{"State":"SUCCEEDED"}}}"#.to_string()
            }
            "getqueryresults" => {
                let id: usize = body
                    .pointer("/QueryExecutionId")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.strip_prefix("q-"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let page2 = body.pointer("/NextToken").is_some();
                let q = queries.get(id).cloned().unwrap_or_default();
                if q.contains("information_schema.tables") {
                    result_set(
                        &[("table_name", "varchar")],
                        &[vec!["table_name"], vec!["artists"], vec!["tracks"]],
                        None,
                    )
                } else if q.contains("information_schema.columns") {
                    result_set(
                        &[("table_name", "varchar"), ("column_name", "varchar")],
                        &[
                            vec!["table_name", "column_name"],
                            vec!["artists", "id"],
                            vec!["artists", "name"],
                            vec!["artists", "country"],
                            vec!["tracks", "id"],
                            vec!["tracks", "title"],
                            vec!["tracks", "artist_id"],
                            vec!["tracks", "secs"],
                        ],
                        None,
                    )
                } else if q.contains("FROM \"artists\"") {
                    result_set(
                        &[
                            ("id", "integer"),
                            ("name", "varchar"),
                            ("country", "varchar"),
                        ],
                        &[
                            vec!["id", "name", "country"],
                            vec!["1", "Holst", "England"],
                            vec!["2", "Bartok", "Hungary"],
                        ],
                        None,
                    )
                } else if q.contains("FROM \"tracks\"") && !page2 {
                    // Two pages, to exercise pagination.
                    result_set(
                        &[
                            ("id", "integer"),
                            ("title", "varchar"),
                            ("artist_id", "integer"),
                            ("secs", "integer"),
                        ],
                        &[
                            vec!["id", "title", "artist_id", "secs"],
                            vec!["1", "Jupiter", "1", "95"],
                        ],
                        Some("page-2"),
                    )
                } else if q.contains("FROM \"tracks\"") {
                    result_set(
                        &[
                            ("id", "integer"),
                            ("title", "varchar"),
                            ("artist_id", "integer"),
                            ("secs", "integer"),
                        ],
                        &[vec!["2", "Bourree", "2", "82"]],
                        None,
                    )
                } else if q.contains("SELECT") {
                    // Pushed-down SQL: a canned single-value answer.
                    result_set(
                        &[("n", "bigint")],
                        &[vec!["n"], vec!["2"]],
                        None,
                    )
                } else {
                    result_set(&[], &[], None)
                }
            }
            _ => "{}".to_string(),
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/x-amz-json-1.1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            resp.len(),
            resp
        );
    }
}

fn values(a: &AthenaAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn bottled_athena_catalog() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || serve(listener));

    // The signer needs SOME credentials in scope; the bottle only
    // checks the SigV4 shape, so fixed test keys do.
    unsafe {
        std::env::set_var("AWS_ACCESS_KEY_ID", "bottled");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "bottled-secret");
    }
    let a = AthenaAdapter::connect(&format!(
        "athena://music?region=us-east-1&endpoint=http://127.0.0.1:{port}&key=artists:name,tracks:title"
    ))
    .unwrap();
    // catalog
    assert_eq!(values(&a, "/*:::name"), ["artists", "tracks"]);
    // rows named by the ?key= nomination; typed cells
    assert_eq!(values(&a, "/artists/Holst::country"), ["England"]);
    // pagination: both tracks pages arrive
    assert_eq!(values(&a, "/tracks/* @| count"), ["2"]);
    assert_eq!(values(&a, "/tracks/*[::secs > 90]::title"), ["Jupiter"]);
    drop(a);
    drop(handle);
}
