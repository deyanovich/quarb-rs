//! A minimal hand-built workbook (inline strings), opened and
//! queried.

use quarb_xlsx::XlsxAdapter;
use std::io::Write as _;

fn fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("quarb-xlsx-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.xlsx");
    let f = std::fs::File::create(&path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default();
    let mut put = |name: &str, body: &str| {
        z.start_file(name, o).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    };
    put(
        "[Content_Types].xml",
        r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
    );
    put(
        "_rels/.rels",
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    );
    put(
        "xl/workbook.xml",
        r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="expenses" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
    );
    put(
        "xl/_rels/workbook.xml.rels",
        r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
    );
    put(
        "xl/worksheets/sheet1.xml",
        r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>item</t></is></c><c r="B1" t="inlineStr"><is><t>amount</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>rent</t></is></c><c r="B2"><v>1200</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>coffee</t></is></c><c r="B3"><v>87</v></c></row></sheetData></worksheet>"#,
    );
    z.finish().unwrap();
    path
}

#[test]
fn sheets_rows_and_types() {
    let a = XlsxAdapter::open(&fixture()).unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/expenses/* @| count"), ["2"]);
    // Numbers arrive numeric; rows are named by sheet row number.
    assert_eq!(v("/expenses/*[::amount > 100]"), ["/expenses/2"]);
    assert_eq!(v("/expenses/2::item"), ["rent"]);
    assert_eq!(v("/expenses/* | ::amount @| sum"), ["1287"]);
}
