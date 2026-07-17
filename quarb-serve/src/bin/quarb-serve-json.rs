//! The protocol's reference server: serve a JSON file. Doubles as
//! the identity-test fixture and the copy-me example for tools.

fn main() -> std::io::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: quarb-serve-json FILE");
    let text = std::fs::read_to_string(&path)?;
    let adapter = quarb_json::JsonAdapter::parse(&text).expect("parsing JSON");
    quarb_serve::serve(&adapter, |n| adapter.pointer(n), "json-file")
}
