//! A bottled Cosmos DB: a std-only HTTP server speaking the
//! recorded REST shapes (collection listing, paginated document
//! feeds via x-ms-continuation, parameterized id queries),
//! asserting every request carries the master-key authorization
//! triple and the x-ms-date header. Fully offline.

use std::io::{Read, Write};
use std::net::TcpListener;

use quarb_cosmos::CosmosAdapter;

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
                    head.contains("authorization: type%3dmaster%26ver%3d1.0%26sig%3d"),
                    "request without the master-key triple:\n{head}"
                );
                assert!(head.contains("x-ms-date:"), "request without x-ms-date");
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
        let continued = head.to_lowercase().contains("x-ms-continuation:");

        let mut extra_header = String::new();
        let resp = if first.starts_with("GET /dbs/shop/colls ")
            || first.starts_with("GET /dbs/shop/colls?")
        {
            r#"{"DocumentCollections":[{"id":"orders"},{"id":"customers"}]}"#.to_string()
        } else if first.starts_with("GET /dbs/shop/colls/customers/docs") {
            r#"{"Documents":[
                {"id":"ada","name":"Ada","tier":"gold","_rid":"r1","_ts":1753380000},
                {"id":"bo","name":"Bo","tier":"basic","_rid":"r2","_ts":1753380001}]}"#
                .to_string()
        } else if first.starts_with("GET /dbs/shop/colls/orders/docs") && !continued {
            extra_header = "x-ms-continuation: page-2\r\n".to_string();
            r#"{"Documents":[
                {"id":"o-1","customer_id":"ada","total":120,
                 "lines":[{"sku":"tea","qty":2},{"sku":"cup","qty":1}],
                 "_rid":"r3","_ts":1753380002}]}"#
                .to_string()
        } else if first.starts_with("GET /dbs/shop/colls/orders/docs") {
            r#"{"Documents":[
                {"id":"o-2","customer_id":"bo","total":45,
                 "lines":[{"sku":"pot","qty":1}],
                 "_rid":"r4","_ts":1753380003}]}"#
                .to_string()
        } else if first.starts_with("POST /dbs/shop/colls/customers/docs") {
            assert!(body.contains("c.id = @v"), "expected the id query: {body}");
            if body.contains("\"ada\"") {
                r#"{"Documents":[{"id":"ada","name":"Ada","tier":"gold","_rid":"r1","_ts":1753380000}]}"#
                    .to_string()
            } else {
                r#"{"Documents":[]}"#.to_string()
            }
        } else {
            "{}".to_string()
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n{extra_header}content-length: {}\r\nconnection: close\r\n\r\n{}",
            resp.len(),
            resp
        );
    }
}

fn values(a: &CosmosAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn bottled_cosmos() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || serve(listener));
    unsafe {
        std::env::set_var(
            "AZURE_COSMOS_KEY",
            // any base64 does — the bottle checks the header shape
            "Ym90dGxlZC1jb3Ntb3Mta2V5LWZvci10ZXN0aW5n",
        );
    }
    let a =
        CosmosAdapter::connect(&format!("cosmos://shop/shop?endpoint=http://127.0.0.1:{port}"))
            .unwrap();
    assert_eq!(values(&a, "/*:::name"), ["customers", "orders"]);
    // both feed pages arrive; docs are id-named and id-sorted
    assert_eq!(values(&a, "/orders/*:::name"), ["o-1", "o-2"]);
    // nested arrays descend; system fields stay out of the tree
    assert_eq!(values(&a, "/orders/o-1/lines/*::sku"), ["tea", "cup"]);
    assert_eq!(values(&a, "/customers/ada/* @| count"), ["3"]);
    // …but surface as metadata
    assert_eq!(values(&a, "/customers/ada;;;ts"), ["1753380000"]);
    // hinted and hint-less resolution, via the id query
    assert_eq!(
        values(&a, "/orders/o-1::customer_id~>customers::tier"),
        ["gold"]
    );
    assert_eq!(values(&a, "/orders/o-1::customer_id~>::name"), ["Ada"]);
    drop(a);
    drop(handle);
}
