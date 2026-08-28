//! PDFs assembled byte-by-byte in-test (offsets computed, xref
//! written by hand): the outline→section derivation with nesting
//! and front matter, exact-baseline line grouping, the boundary
//! rule at page+y, ToUnicode decoding, the no-outline page tree,
//! and the not-a-PDF refusal.

use quarb::QueryResult;
use quarb_text_pdf::parse;

/// Assemble a PDF from numbered object bodies (1-based,
/// contiguous), with a correct xref table and trailer.
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

fn stream(body: &str) -> String {
    format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len() + 1)
}

fn values(model: &quarb_text_pdf::PdfText, q: &str) -> Vec<String> {
    match quarb::run(q, model).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| model.locator(*n)).collect(),
    }
}

fn outlined() -> Vec<u8> {
    let c1 = stream(
        "BT /F1 12 Tf 1 0 0 1 72 760 Tm (Front matter) Tj \
         1 0 0 1 72 700 Tm (Emus advanced.) Tj \
         1 0 0 1 200 700 Tm (On the wheat.) Tj ET",
    );
    let c2 = stream("BT /F1 12 Tf 1 0 0 1 72 700 Tm (The gun jammed.) Tj ET");
    pdf(&[
        "<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
         /Resources << /Font << /F1 7 0 R >> >> >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R \
         /Resources << /Font << /F1 7 0 R >> >> >>",
        &c1,
        &c2,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Outlines /First 9 0 R /Last 9 0 R >>",
        "<< /Title (The war) /Parent 8 0 R /Dest [3 0 R /XYZ 0 720 0] \
         /First 10 0 R /Last 10 0 R >>",
        "<< /Title (First attempt) /Parent 9 0 R /Dest [4 0 R /XYZ 0 720 0] >>",
    ])
}

#[test]
fn outline_sections_with_nesting_and_front_matter() {
    let m = parse(&outlined()).unwrap();
    assert_eq!(values(&m, "/section::lemma"), ["The war"]);
    assert_eq!(values(&m, "/section/section::lemma"), ["First attempt"]);
    // above the first destination = front matter, at the root
    assert_eq!(values(&m, "/line::"), ["Front matter"]);
    // two runs at the identical declared baseline are one line,
    // in x order
    assert_eq!(
        values(&m, r#"/section[::lemma = "The war"]/line::"#),
        ["Emus advanced. On the wheat."]
    );
    // the mention reading: a section mentions what its whole
    // subtree shows
    assert_eq!(
        values(&m, r#"//section[:: *= "jammed"]::lemma"#),
        ["The war", "First attempt"]
    );
    // geometry rides as adapter metadata
    assert_eq!(values(&m, "//line[::::y = 760]::::page"), ["1"]);
}

#[test]
fn no_outline_reads_as_pages() {
    let c = stream("BT /F1 12 Tf 1 0 0 1 72 700 Tm (Only line) Tj ET");
    let m = parse(&pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>",
        &c,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]))
    .unwrap();
    assert_eq!(values(&m, "//section @| count"), ["0"]);
    assert_eq!(values(&m, "/page/line::"), ["Only line"]);
}

#[test]
fn tounicode_cmap_decodes() {
    let cmap = "/CIDInit /ProcSet findresource begin\n\
        begincmap\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
        2 beginbfchar\n<01> <0048>\n<02> <0069>\nendbfchar\nendcmap\nend";
    let cmap_stream = stream(cmap);
    let c = stream("BT /F1 12 Tf 1 0 0 1 72 700 Tm <0102> Tj ET");
    let m = parse(&pdf(&[
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>",
        &c,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Custom /ToUnicode 6 0 R >>",
        &cmap_stream,
    ]))
    .unwrap();
    assert_eq!(values(&m, "//line::"), ["Hi"]);
}

#[test]
fn not_a_pdf_refuses() {
    assert!(parse(b"not a pdf at all").is_err());
}
