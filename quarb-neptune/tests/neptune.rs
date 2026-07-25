//! A bottled Neptune: a std-only HTTP server speaking the
//! openCypher endpoint's recorded JSON shapes (alias-keyed result
//! rows, `~id`/`~labels`/`~properties` vertices, `~type` edges)
//! plus the statistics-summary catalog, asserting every request
//! carries a SigV4 Authorization header. Fully offline — Neptune
//! itself is VPC-only.

use std::io::{Read, Write};
use std::net::TcpListener;

use quarb_neptune::NeptuneAdapter;

fn vertex(id: &str, label: &str, props: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "~id": id, "~entityType": "node",
        "~labels": [label], "~properties": props
    })
}

fn edge(id: &str, ty: &str, props: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "~id": id, "~entityType": "relationship",
        "~type": ty, "~properties": props
    })
}

fn results(rows: Vec<serde_json::Value>) -> String {
    serde_json::json!({ "results": rows }).to_string()
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() + 1 => {
                let h = (b[i + 1] as char).to_digit(16).unwrap_or(0);
                let l = (b[i + 2] as char).to_digit(16).unwrap_or(0);
                out.push((h * 16 + l) as u8);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn ada() -> serde_json::Value {
    vertex("p1", "Person", serde_json::json!({"name": "Ada", "role": "engineer", "city": "Oslo"}))
}
fn bo() -> serde_json::Value {
    vertex("p2", "Person", serde_json::json!({"name": "Bo", "role": "analyst", "city": "Novi Sad"}))
}
fn oslo() -> serde_json::Value {
    vertex("c1", "City", serde_json::json!({"name": "Oslo", "country": "NO"}))
}
fn novisad() -> serde_json::Value {
    vertex("c2", "City", serde_json::json!({"name": "Novi Sad", "country": "RS"}))
}

fn answer(query: &str) -> String {
    let q = query.replace('`', "");
    if q.contains("MATCH (m:Person)") && q.contains("count(m)") {
        results(vec![serde_json::json!({"c": 2})])
    } else if q.contains("MATCH (m:Person)") && q.contains("WHERE") && q.contains("Ada") {
        results(vec![serde_json::json!({"m": ada()})])
    } else if q.contains("MATCH (m:Person)") {
        results(vec![
            serde_json::json!({"m": ada()}),
            serde_json::json!({"m": bo()}),
        ])
    } else if q.contains("MATCH (m:City)") && q.contains("WHERE") && q.contains("Oslo") {
        results(vec![serde_json::json!({"m": oslo()})])
    } else if q.contains("MATCH (m:City)") {
        results(vec![
            serde_json::json!({"m": novisad()}),
            serde_json::json!({"m": oslo()}),
        ])
    } else if q.contains("-[r]->") && q.contains("'p1'") {
        results(vec![
            serde_json::json!({
                "r": edge("e1", "LIVES_IN", serde_json::json!({"since": 2019})),
                "m": oslo()
            }),
            serde_json::json!({
                "r": edge("e2", "MENTORS", serde_json::json!({"hours": 3})),
                "m": bo()
            }),
        ])
    } else if q.contains("<-[r]-") && q.contains("'p2'") {
        results(vec![serde_json::json!({
            "r": edge("e2", "MENTORS", serde_json::json!({"hours": 3})),
            "m": ada()
        })])
    } else if q.contains("-[r]->") || q.contains("<-[r]-") {
        results(vec![])
    } else {
        results(vec![])
    }
}

fn serve(listener: TcpListener) {
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
                let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                assert!(
                    head.contains("authorization: aws4-hmac-sha256"),
                    "request not SigV4-signed:\n{head}"
                );
                content_length = head
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:")
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
        let head = String::from_utf8_lossy(&buf[..headers_end]).into_owned();
        let first = head.lines().next().unwrap_or_default().to_string();
        let body = String::from_utf8_lossy(&buf[headers_end..]).into_owned();

        let resp = if first.starts_with("GET /propertygraph/statistics/summary") {
            r#"{"status":"200 OK","payload":{"graphSummary":{"nodeLabels":["City","Person"],"edgeLabels":["LIVES_IN","MENTORS"]}}}"#
                .to_string()
        } else if first.starts_with("POST /openCypher") {
            let query = body
                .strip_prefix("query=")
                .map(urldecode)
                .unwrap_or_default();
            answer(&query)
        } else {
            "{}".to_string()
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            resp.len(),
            resp
        );
    }
}

fn values(a: &NeptuneAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn bottled_neptune() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || serve(listener));
    unsafe {
        std::env::set_var("AWS_ACCESS_KEY_ID", "bottled");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "bottled-secret");
    }
    let a = NeptuneAdapter::connect(&format!(
        "neptune://ignored?region=us-east-1&key=name&endpoint=http://127.0.0.1:{port}"
    ))
    .unwrap();

    // Catalog via the summary API.
    assert_eq!(values(&a, "/*:::name"), ["City", "Person"]);
    assert_eq!(values(&a, "/Person;;;n-rows"), ["2"]);

    // ?key= names nodes; properties project.
    assert_eq!(values(&a, "/Person/*:::name"), ["Ada", "Bo"]);
    assert_eq!(values(&a, "/Person/Ada::role"), ["engineer"]);

    // Relationships as typed crosslinks, both directions, with
    // edge properties on $-.
    assert_eq!(values(&a, "/Person/Ada->LIVES_IN::name"), ["Oslo"]);
    assert_eq!(values(&a, "/Person/Bo<-MENTORS::name"), ["Ada"]);
    assert_eq!(
        values(&a, "/Person/Ada->MENTORS[$-::hours = 3]::name"),
        ["Bo"]
    );

    // Hinted resolution against the ?key= property.
    assert_eq!(values(&a, "/Person/Ada::city~>City::country"), ["NO"]);

    drop(a);
    drop(handle);
}
