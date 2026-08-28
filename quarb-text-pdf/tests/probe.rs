//! Env-gated diagnostics over a real PDF: set QUARB_PDF_PROBE to
//! a path and run with --nocapture.

#[test]
fn probe() {
    let Ok(path) = std::env::var("QUARB_PDF_PROBE") else { return };
    let bytes = std::fs::read(&path).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let pages = doc.get_pages();
    println!("pages: {}", pages.len());
    let (&pnum, &pid) = pages.iter().next().unwrap();
    let content = doc.get_page_content(pid).unwrap();
    println!("page {pnum} content bytes: {}", content.len());
    match lopdf::content::Content::decode(&content) {
        Ok(ops) => {
            println!("ops: {}", ops.operations.len());
            let mut shows = 0;
            let mut tfs = Vec::new();
            for op in &ops.operations {
                match op.operator.as_str() {
                    "Tj" | "TJ" | "'" | "\"" => shows += 1,
                    "Tf" => {
                        if let Some(lopdf::Object::Name(n)) = op.operands.first() {
                            tfs.push(String::from_utf8_lossy(n).to_string());
                        }
                    }
                    _ => {}
                }
            }
            println!("show ops: {shows}; fonts used: {tfs:?}");
        }
        Err(e) => println!("content decode FAILED: {e}"),
    }
    let (res, ids) = doc.get_page_resources(pid).unwrap();
    println!("inline resources: {}; resource objs: {ids:?}", res.is_some());
    if let Some(r) = res.or_else(|| {
        ids.first()
            .and_then(|id| doc.get_object(*id).ok())
            .and_then(|o| o.as_dict().ok())
    }) {
        if let Ok(f) = r.get(b"Font") {
            println!("font entry: {f:?}");
        } else {
            println!("no /Font in resources");
        }
    }
}
