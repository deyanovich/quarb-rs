//! Markdown adapter for Quarb. Markdown is a subset of HTML, so a
//! document is rendered to HTML (CommonMark + tables/strikethrough)
//! and served by [`quarb_html::HtmlAdapter`] — every HTML and CSS
//! recipe applies to a `.md` file. Headings become `<h1>`…`<h6>`,
//! lists `<ul>`/`<ol>`/`<li>`, links `<a href>`, fenced code
//! `<pre><code>`, and so on.
//!
//! A YAML header at the top of the file (a note's front matter:
//! `---`, a mapping, `---` or `...`) is the document's head, as a
//! page's is: `title` renders as the `<title>` element and every
//! other key as `<meta name="KEY" content="VALUE">`, one element
//! per item of a list, a nested mapping's keys joined to their
//! parent's with a dot (`geo.lat`). The header is no part of the
//! body: without this it read as a rule and a heading.

use pulldown_cmark::{Options, Parser};
use quarb::Value;
use quarb_html::HtmlAdapter;

/// Render Markdown `text` to HTML and build an HTML adapter over it.
pub fn parse(text: &str) -> HtmlAdapter {
    let (mut html, body) = match quarb_yaml::front_matter(text) {
        Some((entries, body)) => (head(&entries), body),
        None => (String::new(), text),
    };
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(body, opts);
    pulldown_cmark::html::push_html(&mut html, parser);
    HtmlAdapter::parse(&html)
}

/// A header's keys as the elements of an HTML head.
fn head(entries: &[(String, Value)]) -> String {
    let mut out = String::from("<head>");
    for (key, value) in entries {
        match value {
            Value::Str(s) if key == "title" => {
                out.push_str("<title>");
                escape(s, &mut out);
                out.push_str("</title>");
            }
            _ => meta(key, value, &mut out),
        }
    }
    out.push_str("</head>\n");
    out
}

/// One declaration as `meta` elements: a scalar is one, a list one
/// per item, a mapping one per key under `NAME.KEY`; a null
/// declares nothing.
fn meta(name: &str, value: &Value, out: &mut String) {
    let content = match value {
        Value::Null => return,
        Value::List(items) => {
            for item in items.iter() {
                meta(name, item, out);
            }
            return;
        }
        Value::Record(r) => {
            for (k, v) in r.iter() {
                meta(&format!("{name}.{k}"), v, out);
            }
            return;
        }
        Value::Str(s) => s.clone(),
        other => other.to_string(),
    };
    out.push_str("<meta name=\"");
    escape(name, out);
    out.push_str("\" content=\"");
    escape(&content, out);
    out.push_str("\">");
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}
