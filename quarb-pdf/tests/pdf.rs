//! The object graph over a hand-assembled PDF: objects named by
//! their declared /Type, references as edges (->Root, ->Kids,
//! and <-Parent finding the children), scalars as properties.

use quarb::QueryResult;
use quarb_pdf::PdfAdapter;

fn pdf(objects: &[&str]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).into_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n", objects.len() + 1).into_bytes());
    out.extend(b"0000000000 65535 f \n");
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").into_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
            objects.len() + 1
        )
        .into_bytes(),
    );
    out
}

fn doc() -> PdfAdapter {
    PdfAdapter::load(&pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        "<< /Type /Page /Parent 2 0 R /Rotate 90 >>",
    ]))
    .unwrap()
}

fn values(a: &PdfAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
    }
}

#[test]
fn objects_by_declared_type() {
    let a = doc();
    assert_eq!(values(&a, "/objects/page @| count"), ["2"]);
    assert_eq!(values(&a, "/objects/catalog::id"), ["1 0"]);
    assert_eq!(values(&a, "/objects/pages/Count::"), Vec::<String>::new());
    // scalar dict entries are properties
    assert_eq!(values(&a, "/objects/pages::Count"), ["2"]);
    assert_eq!(values(&a, "/objects/page[::Rotate = 90]::id"), ["4 0"]);
}

#[test]
fn references_are_edges() {
    let a = doc();
    // the trailer points at the catalog; Kids fan out as items
    assert_eq!(values(&a, "/trailer->Root::id"), ["1 0"]);
    assert_eq!(values(&a, "/objects/pages->Kids @| count"), ["2"]);
    // the reverse fabric: which objects point at the pages node
    // through their Parent key — the children pages
    assert_eq!(values(&a, "/objects/pages<-Parent @| count"), ["2"]);
    // walk: catalog -> Pages -> items -> back up by Parent
    assert_eq!(
        values(&a, "/objects/catalog->Pages->Kids[::Rotate = 90]->Parent::id"),
        ["2 0"]
    );
}

#[test]
fn direct_structure_is_children() {
    let a = doc();
    // MediaBox is a direct array: a child named by its key, its
    // scalar elements as items
    assert_eq!(values(&a, "/objects/page/MediaBox/item::"), ["0", "0", "612", "792"]);
}

#[test]
fn not_a_pdf_refuses() {
    assert!(PdfAdapter::load(b"nope").is_err());
}

