//! The engine's conformance suite over an HTML document.

use quarb_html::HtmlAdapter;

#[test]
fn an_html_document_conforms() {
    let doc = r##"<html><head><title>Shelf</title></head><body>
      <div class="card" id="emma"><h3>Emma</h3><p class="price">12</p><a href="#iliad">next</a></div>
      <div class="card" id="iliad"><h3>Iliad</h3><p class="price">9</p></div>
      <ul><li>old</li><li>new</li></ul></body></html>"##;
    let adapter = HtmlAdapter::parse(doc);
    let findings = quarb::conformance::check(&adapter, 10_000);
    assert!(findings.is_empty(), "{findings:#?}");
}
