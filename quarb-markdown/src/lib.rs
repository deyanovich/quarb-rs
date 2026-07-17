//! Markdown adapter for Quarb. Markdown is a subset of HTML, so a
//! document is rendered to HTML (CommonMark + tables/strikethrough)
//! and served by [`quarb_html::HtmlAdapter`] — every HTML and CSS
//! recipe applies to a `.md` file. Headings become `<h1>`…`<h6>`,
//! lists `<ul>`/`<ol>`/`<li>`, links `<a href>`, fenced code
//! `<pre><code>`, and so on.

use pulldown_cmark::{Options, Parser};
use quarb_html::HtmlAdapter;

/// Render Markdown `text` to HTML and build an HTML adapter over it.
pub fn parse(text: &str) -> HtmlAdapter {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, opts);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    HtmlAdapter::parse(&html)
}
