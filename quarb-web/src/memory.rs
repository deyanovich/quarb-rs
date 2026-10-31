//! An in-memory store built from a set of pages: the tree from
//! their paths, each page's declared identity from its head, the
//! links from its DOM, and the analytics over the link graph. The
//! form behind a tarball or a directory of built pages — a whole
//! site small enough to hold.

use crate::contract::*;
use quarb::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The site a set of pages belongs to.
#[derive(Clone, Debug)]
pub struct SiteInput {
    /// `https://quarb.org/` — the base every path joins against
    /// when a page declares no canonical URL of its own.
    pub base_url: String,
    pub snapshot: Option<String>,
}

pub struct MemoryStore {
    catalog: Catalog,
    rows: Vec<LightRow>,
    html: HashMap<PageKey, String>,
    /// container → parent (None = top) → children in rank order
    children: HashMap<(Container, Option<PageKey>), Vec<PageKey>>,
    out_links: HashMap<PageKey, Vec<LinkRow>>,
    in_links: HashMap<PageKey, Vec<PageKey>>,
    /// target → (role, source) for every edge, in document order
    in_roles: HashMap<PageKey, Vec<(String, PageKey)>>,
    /// target page → (fragment, role, source) for the edges landing
    /// on a node inside it
    in_fragments: HashMap<PageKey, Vec<(String, String, PageKey)>>,
    /// category/tag row → member pages, in document order
    members: HashMap<PageKey, Vec<PageKey>>,
    /// page → its category rows / tag rows
    page_categories: HashMap<PageKey, Vec<PageKey>>,
    page_tags: HashMap<PageKey, Vec<PageKey>>,
    by_url: HashMap<String, PageKey>,
    /// target page → the redirect stubs that stand for it
    in_redirects: HashMap<PageKey, Vec<PageKey>>,
}

/// A page as read from a file: its site-relative path and its
/// bytes as text.
pub struct PageFile {
    pub path: String,
    pub html: String,
}

fn host_of(base_url: &str) -> String {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| base_url.trim_end_matches('/').to_string())
}

/// The site-relative path a URL names, when it is on the site:
/// `https://quarb.org/guides/` → `guides/index.html`, `/` →
/// `index.html`, percent-decoded, fragment and query dropped.
/// Whether a URL's last path segment names a file by an extension
/// (`spec.pdf`, `shot-1.png`): a same-site target with one is a
/// resource rather than a redlink; `.html`/`.htm` are pages' own.
fn has_file_extension(url: &url::Url) -> bool {
    url.path_segments()
        .and_then(|mut s| s.next_back())
        .and_then(|last| last.rsplit_once('.'))
        .map(|(stem, ext)| {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && !matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm")
        })
        .unwrap_or(false)
}

pub(crate) fn site_path(url: &url::Url, base: &url::Url) -> Option<String> {
    if url.host_str() != base.host_str() || url.scheme() != base.scheme() {
        return None;
    }
    let raw = url.path();
    let mut p = percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned();
    let base_path = base.path().trim_end_matches('/');
    if !base_path.is_empty() {
        p = p.strip_prefix(base_path)?.to_string();
    }
    let p = p.trim_start_matches('/');
    if p.is_empty() || p.ends_with('/') {
        return Some(format!("{p}index.html"));
    }
    Some(p.to_string())
}

/// Every hyperlink in the page's body, in document order, with
/// its anchor text and the lemma of the nearest preceding
/// heading — the crawler's view, complete where the text level's
/// prose refs are selective.
/// One anchor as the walk found it.
pub(crate) struct Found {
    pub href: String,
    pub anchor: Option<String>,
    pub section: Option<String>,
    /// `class="new"` — a wiki's own redlink mark.
    pub red: bool,
    /// Inside a transclusion (`typeof="mw:Transclusion"` or an
    /// `about="#mwt…"` ancestor): written by a template.
    pub transcluded: bool,
    /// The edge's role (see [`LinkRow::role`]).
    pub role: String,
    /// The edge's properties (see [`LinkRow::props`]).
    pub props: Vec<(String, Value)>,
}

/// The `rel` words that are hints about the link, never its label
/// (spec: Text Readings, the web reading): they ride as boolean
/// edge properties.
const REL_HINTS: [&str; 6] = [
    "nofollow",
    "ugc",
    "sponsored",
    "external",
    "noopener",
    "noreferrer",
];
/// The `rel` words naming an asset of the page: a resource under
/// `->asset`, the rel as `$-::rel`.
const REL_ASSETS: [&str; 6] = [
    "stylesheet",
    "icon",
    "preload",
    "prefetch",
    "modulepreload",
    "manifest",
];

/// A link's label from its `rel` and the attributes beside it, with
/// the edge's properties: the declared relation first (an
/// alternate by language or type, an asset, else the relation's own
/// name lowercased), `None` where the rel holds hints alone — the
/// position labels it then. `title`, `type`, `hreflang`, `rel` as
/// written and the hints ride as properties.
fn rel_label(
    rel: Option<&str>,
    hreflang: Option<&str>,
    mime: Option<&str>,
    title: Option<&str>,
) -> (Option<String>, Vec<(String, Value)>) {
    let mut props: Vec<(String, Value)> = Vec::new();
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        props.push(("title".to_string(), Value::Str(t.to_string())));
    }
    if let Some(l) = hreflang.map(str::trim).filter(|l| !l.is_empty()) {
        props.push(("hreflang".to_string(), Value::Str(l.to_string())));
    }
    if let Some(m) = mime.map(str::trim).filter(|m| !m.is_empty()) {
        props.push(("type".to_string(), Value::Str(m.to_string())));
    }
    let Some(rel) = rel.map(str::trim).filter(|r| !r.is_empty()) else {
        return (None, props);
    };
    props.push(("rel".to_string(), Value::Str(rel.to_string())));
    let words: Vec<String> = rel
        .split_whitespace()
        .map(|w| w.to_ascii_lowercase())
        .collect();
    for w in &words {
        if REL_HINTS.contains(&w.as_str()) {
            props.push((w.clone(), Value::Bool(true)));
        }
    }
    // a namespaced word (Parsoid's `mw:WikiLink`, `mw:ExtLink`) is a
    // markup annotation, not a relation: it rides in `rel` only
    let named: Vec<&String> = words
        .iter()
        .filter(|w| !REL_HINTS.contains(&w.as_str()) && !w.contains(':'))
        .collect();
    let Some(first) = named.first() else {
        return (None, props);
    };
    let label = if named.iter().any(|w| *w == "alternate") {
        if hreflang.is_some_and(|l| !l.trim().is_empty()) {
            "altlang".to_string()
        } else if let Some(l) = mime.and_then(alternate_label) {
            l
        } else {
            "alternate".to_string()
        }
    } else if named.iter().any(|w| REL_ASSETS.contains(&w.as_str())) {
        "asset".to_string()
    } else {
        first.to_string()
    };
    (Some(label), props)
}

/// A JSON-LD value as the URL an edge lands on: a URL string, or an
/// entity with `@id` or `url`.
fn ld_url(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => {
            let t = s.trim();
            (t.starts_with("http://") || t.starts_with("https://") || t.starts_with('/'))
                .then(|| t.to_string())
        }
        serde_json::Value::Object(o) => o
            .get("@id")
            .or_else(|| o.get("url"))
            .and_then(|u| u.as_str())
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty()),
        _ => None,
    }
}

/// An alternate's type as an edge label: `application/pdf` → `pdf`,
/// the feeds `feed`; another type its subtype's short name.
fn alternate_label(mime: &str) -> Option<String> {
    let t = mime.trim().to_ascii_lowercase();
    let t = t.split(';').next().unwrap_or("").trim();
    let sub = t.rsplit('/').next().unwrap_or(t);
    match sub {
        "" => None,
        "rss+xml" | "atom+xml" | "feed+json" => Some("feed".to_string()),
        s => Some(s.split('+').next().unwrap_or(s).to_string()),
    }
}

/// The most words a page's body may hold and still be a redirect
/// stub by its script alone: a sentence and a link. A hub whose
/// find box deep-links, or that forwards old anchors, carries such
/// a script too, inside a condition, and keeps its body — it stays
/// a page.
const STUB_WORDS: usize = 40;

/// The words of the body's visible text, scripts and styles left
/// out.
fn body_words(doc: &scraper::Html) -> usize {
    use scraper::Selector;
    let Ok(body) = Selector::parse("body") else {
        return 0;
    };
    let Some(b) = doc.select(&body).next() else {
        return 0;
    };
    let mut words = 0usize;
    for node in b.descendants() {
        if let Some(text) = node.value().as_text() {
            let skip = node.ancestors().any(|a| {
                a.value()
                    .as_element()
                    .is_some_and(|e| matches!(e.name(), "script" | "style" | "noscript"))
            });
            if !skip {
                words += text.split_whitespace().count();
            }
        }
    }
    words
}

/// A redirect stub's target (draft 164): the URL of a meta refresh
/// in the head (`<meta http-equiv="refresh" content="0; url=…">`),
/// else, on a page whose body is a stub (`STUB_WORDS`), the first
/// quoted URL of a script that sends the browser on
/// (`location.replace(…)`, `location.href = …`, `location.assign(…)`).
/// A page with a body of its own is a page whatever its scripts do.
pub(crate) fn redirect_target(doc: &scraper::Html) -> Option<String> {
    use scraper::Selector;
    let meta = Selector::parse("meta[http-equiv]").ok()?;
    for m in doc.select(&meta) {
        let v = m.value();
        if v.attr("http-equiv")
            .is_some_and(|h| h.eq_ignore_ascii_case("refresh"))
            && let Some(content) = v.attr("content")
            && let Some(i) = content.to_ascii_lowercase().find("url=")
        {
            let t = content[i + 4..]
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    if body_words(doc) > STUB_WORDS {
        return None;
    }
    let script = Selector::parse("script").ok()?;
    for s in doc.select(&script) {
        let text: String = s.text().collect();
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let sends = [
            "location.replace(",
            "location.href",
            "location.assign(",
            "location=",
        ];
        if !sends.iter().any(|p| compact.contains(p)) {
            continue;
        }
        let t = text.trim();
        if let Some(q) = t.find(['"', '\''])
            && let Some(end) = t[q + 1..].find(t.as_bytes()[q] as char)
        {
            let target = &t[q + 1..q + 1 + end];
            if !target.is_empty() {
                return Some(target.to_string());
            }
        }
    }
    None
}

pub(crate) fn dom_links(html: &str) -> (Option<String>, Vec<Found>) {
    use scraper::{Html, Node, Selector};
    let doc = Html::parse_document(html);
    let base = quarb_text_html::base_href(&doc);
    let mut out = Vec::new();
    // The head's own links (spec: Text Readings, the web reading):
    // every `<link rel>`, labelled by its relation — the alternates
    // by language and by type, the assets under `asset`, the rest
    // by the rel's own name (canonical, prev, next, author,
    // license, …).
    if let Ok(sel) = Selector::parse("link[rel][href]") {
        for l in doc.select(&sel) {
            let el = l.value();
            let Some(href) = el.attr("href").map(str::trim).filter(|h| !h.is_empty()) else {
                continue;
            };
            let (label, props) = rel_label(
                el.attr("rel"),
                el.attr("hreflang"),
                el.attr("type"),
                el.attr("title"),
            );
            let Some(role) = label else { continue };
            out.push(Found {
                href: href.to_string(),
                anchor: el.attr("title").map(|t| quarb_text::normalize_ws(t)),
                section: None,
                red: false,
                transcluded: false,
                role,
                props,
            });
        }
    }
    // Open Graph and the Twitter card: the media and the people a
    // page declares, as edges named after the property.
    if let Ok(sel) = Selector::parse("meta[content]") {
        for m in doc.select(&sel) {
            let el = m.value();
            let Some(key) = el.attr("property").or_else(|| el.attr("name")) else {
                continue;
            };
            let Some(content) = el.attr("content").map(str::trim).filter(|c| !c.is_empty()) else {
                continue;
            };
            let role = match key.trim().to_ascii_lowercase().as_str() {
                "og:image" | "og:image:url" | "og:image:secure_url" | "twitter:image" => "image",
                "og:video" | "og:video:url" | "og:video:secure_url" => "video",
                "og:audio" | "og:audio:url" | "og:audio:secure_url" => "audio",
                "article:author" => "author",
                "article:publisher" => "publisher",
                "og:see_also" => "seealso",
                _ => continue,
            };
            if !(content.starts_with("http://")
                || content.starts_with("https://")
                || content.starts_with('/'))
            {
                continue;
            }
            out.push(Found {
                href: content.to_string(),
                anchor: None,
                section: None,
                red: false,
                transcluded: false,
                role: role.to_string(),
                props: Vec::new(),
            });
        }
    }
    // A schema.org BreadcrumbList in JSON-LD: the crumb trail,
    // ordered, the position kept.
    if let Ok(sel) = Selector::parse(r#"script[type="application/ld+json"]"#) {
        for script in doc.select(&sel) {
            let text = script.text().collect::<String>();
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let items: Vec<serde_json::Value> = match v {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            };
            for item in items {
                let is_list = item
                    .get("@type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "BreadcrumbList");
                if !is_list {
                    // Any property whose value is a URL, or an entity
                    // with `@id` / `url`, is an edge named after the
                    // property, lowercased; a list of such values one
                    // edge each; potentialAction lands on its target.
                    let Some(obj) = item.as_object() else {
                        continue;
                    };
                    for (key, value) in obj {
                        if key.starts_with('@') {
                            continue;
                        }
                        let label = key.to_ascii_lowercase();
                        let values: Vec<&serde_json::Value> = match value {
                            serde_json::Value::Array(vs) => vs.iter().collect(),
                            v => vec![v],
                        };
                        for v in values {
                            let target = if label == "potentialaction" {
                                v.get("target").and_then(|t| match t {
                                    serde_json::Value::Object(o) => o
                                        .get("urlTemplate")
                                        .or_else(|| o.get("url"))
                                        .and_then(|u| u.as_str())
                                        .map(|u| u.trim().to_string()),
                                    other => ld_url(other),
                                })
                            } else {
                                ld_url(v)
                            };
                            let Some(href) = target else { continue };
                            out.push(Found {
                                href,
                                anchor: v
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .map(|n| quarb_text::normalize_ws(n)),
                                section: None,
                                red: false,
                                transcluded: false,
                                role: label.clone(),
                                props: Vec::new(),
                            });
                        }
                    }
                    continue;
                }
                let Some(elems) = item.get("itemListElement").and_then(|e| e.as_array()) else {
                    continue;
                };
                for (i, e) in elems.iter().enumerate() {
                    let href = match e.get("item") {
                        Some(serde_json::Value::String(u)) => Some(u.clone()),
                        Some(serde_json::Value::Object(o)) => o
                            .get("@id")
                            .or_else(|| o.get("url"))
                            .and_then(|u| u.as_str())
                            .map(str::to_string),
                        _ => None,
                    };
                    let Some(href) = href.filter(|h| !h.trim().is_empty()) else {
                        continue;
                    };
                    let position = e
                        .get("position")
                        .and_then(|p| p.as_i64())
                        .unwrap_or(i as i64 + 1);
                    out.push(Found {
                        href: href.trim().to_string(),
                        anchor: e
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(|n| quarb_text::normalize_ws(n)),
                        section: None,
                        red: false,
                        transcluded: false,
                        role: "breadcrumb".to_string(),
                        props: vec![("position".to_string(), Value::Int(position))],
                    });
                }
            }
        }
    }
    let body = Selector::parse("body").unwrap();
    let Some(body) = doc.select(&body).next() else {
        return (base, out);
    };
    let mut section: Option<String> = None;
    // A depth-first walk, tracking the last heading seen, the
    // transclusion depth (a node and its subtree are transcluded
    // when an ancestor carries the marker) and the landmark the
    // anchor sits in: the content is `link`, the header and a nav
    // `navbar`, a breadcrumb nav `breadcrumb` (its anchors
    // numbered), the footer `footer`; `<main>` / `<article>` are
    // content wherever they stand.
    let mut crumb_pos = 0i64;
    let mut stack = vec![(*body, false, "link")];
    while let Some((node, in_template, role)) = stack.pop() {
        let mut transcluded = in_template;
        let mut role = role;
        if let Node::Element(el) = node.value() {
            let tag = el.name();
            if matches!(tag, "script" | "style") {
                continue;
            }
            match tag {
                "header" => role = "navbar",
                "footer" => role = "footer",
                "aside" => role = "aside",
                "main" | "article" => role = "link",
                "nav" => {
                    let label = el
                        .attr("aria-label")
                        .map(|l| l.to_ascii_lowercase())
                        .unwrap_or_default();
                    let class = el
                        .attr("class")
                        .map(|c| c.to_ascii_lowercase())
                        .unwrap_or_default();
                    if label.contains("breadcrumb") || class.contains("breadcrumb") {
                        role = "breadcrumb";
                        crumb_pos = 0;
                    } else if label.contains("table of contents")
                        || label == "toc"
                        || class.split_whitespace().any(|c| c == "toc")
                    {
                        role = "toc";
                    } else if role == "link" {
                        role = "navbar";
                    }
                }
                _ => {}
            }
            if el
                .attr("typeof")
                .is_some_and(|t| t.contains("mw:Transclusion"))
                || el.attr("about").is_some_and(|a| a.starts_with("#mwt"))
            {
                transcluded = true;
            }
            // A heading names the section the links after it sit
            // under — and may carry links of its own (an index's
            // `<h3><a href>`), so the walk goes on into it.
            if matches!(tag, "h1" | "h2" | "h3" | "h4") {
                let t: String = scraper::ElementRef::wrap(node)
                    .map(|e| e.text().collect::<String>())
                    .unwrap_or_default();
                section = Some(quarb_text::normalize_ws(&t));
            }
            if tag == "a"
                && let Some(href) = el.attr("href")
                && !href.trim().is_empty()
            {
                let anchor = scraper::ElementRef::wrap(node)
                    .map(|e| quarb_text::normalize_ws(&e.text().collect::<String>()))
                    .filter(|s| !s.is_empty());
                let red = el
                    .attr("class")
                    .is_some_and(|c| c.split_whitespace().any(|w| w == "new"));
                // a declared relation labels the anchor first (`rel`);
                // hints alone leave it to its position
                let (declared, mut props) = rel_label(
                    el.attr("rel"),
                    el.attr("hreflang"),
                    el.attr("type"),
                    el.attr("title"),
                );
                let role_here = declared.unwrap_or_else(|| role.to_string());
                if role_here == "breadcrumb" {
                    crumb_pos += 1;
                    props.push(("position".to_string(), Value::Int(crumb_pos)));
                }
                out.push(Found {
                    href: href.trim().to_string(),
                    anchor,
                    section: if role_here == "link" {
                        section.clone()
                    } else {
                        None
                    },
                    red,
                    transcluded,
                    role: role_here,
                    props,
                });
            }
        }
        // children in document order: push reversed
        let kids: Vec<_> = node.children().collect();
        for c in kids.into_iter().rev() {
            stack.push((c, transcluded, role));
        }
    }
    (base, out)
}

/// A page's declared own URL: `<link rel=canonical>`, or Parsoid's
/// `<link rel="dc:isVersionOf">` (a protocol-relative `//host/…`
/// read as https).
fn declared_url(doc: &scraper::Html) -> Option<url::Url> {
    use scraper::Selector;
    for rel in ["link[rel=canonical]", "link[rel=\"dc:isVersionOf\"]"] {
        let sel = Selector::parse(rel).ok()?;
        if let Some(href) = doc.select(&sel).next().and_then(|e| e.value().attr("href")) {
            let href = href.trim();
            let abs = if href.starts_with("//") {
                format!("https:{href}")
            } else {
                href.to_string()
            };
            if let Ok(u) = url::Url::parse(&abs) {
                return Some(u);
            }
        }
    }
    None
}

/// The base URL a set of pages is served from, from the first
/// page that declares its own URL ending in its own path.
fn infer_base(files: &[PageFile]) -> Option<String> {
    use scraper::Html;
    let mut sorted: Vec<&PageFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    for f in sorted {
        let lower = f.path.to_ascii_lowercase();
        if !(lower.ends_with(".html") || lower.ends_with(".htm")) {
            continue;
        }
        let doc = Html::parse_document(&f.html);
        let Some(u) = declared_url(&doc) else {
            continue;
        };
        let rel = f.path.trim_start_matches("./").trim_start_matches('/');
        let path = percent_encoding::percent_decode_str(u.path())
            .decode_utf8_lossy()
            .into_owned();
        let path = path.as_str();
        let stem = rel.strip_suffix("index.html").unwrap_or(rel);
        // `…/guides/jq.html` ends the canonical path, the directory
        // form `…/guides/` for an index page, or the extension-less
        // `…/wiki/X` a wiki declares for the file `wiki/X.html`.
        let bare = rel.strip_suffix(".html").unwrap_or(rel);
        let prefix = if path.ends_with(rel) {
            &path[..path.len() - rel.len()]
        } else if path.ends_with(stem) {
            &path[..path.len() - stem.len()]
        } else if path.ends_with(bare) {
            &path[..path.len() - bare.len()]
        } else {
            continue;
        };
        let mut b = u.clone();
        b.set_path(if prefix.is_empty() { "/" } else { prefix });
        b.set_query(None);
        b.set_fragment(None);
        return Some(b.to_string());
    }
    None
}

struct Built {
    row: LightRow,
    html: Option<String>,
    /// A redirect stub's declared target, resolved once every page
    /// is known (draft 164).
    redirect: Option<String>,
}

impl MemoryStore {
    /// Build the store from a site's pages. Paths are site-relative
    /// (`guides/jq.html`); only `.html`/`.htm` files become pages,
    /// every path prefix a directory.
    pub fn build(mut site: SiteInput, files: Vec<PageFile>) -> MemoryStore {
        // No base given: read it off a page's canonical URL — the
        // origin plus whatever precedes the page's own path.
        if site.base_url.is_empty() {
            site.base_url = infer_base(&files).unwrap_or_else(|| "https://localhost/".to_string());
        }
        let base = url::Url::parse(&site.base_url).ok();
        let host = host_of(&site.base_url);
        let sid = SiteId(1);
        let mut next_key = 1u64;

        // 1. Rows: a directory for every path prefix, a page for
        // every html file. Prefixes sort before what they contain,
        // so creating rows in path order sees every parent first.
        let mut pages: BTreeMap<String, String> = BTreeMap::new();
        for f in files {
            let path = f
                .path
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_string();
            let lower = path.to_ascii_lowercase();
            // An html file by name, or a page fetched under an
            // extension-less URL (`docs/intro`) whose body is
            // markup — a site as its sitemap names it.
            let last = lower.rsplit('/').next().unwrap_or(&lower);
            let extensionless = !last.contains('.') && !path.is_empty();
            if lower.ends_with(".html")
                || lower.ends_with(".htm")
                || (extensionless && f.html.trim_start().starts_with('<'))
            {
                pages.insert(path, f.html);
            }
        }
        let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for path in pages.keys() {
            let mut cur = String::new();
            let segs: Vec<&str> = path.split('/').collect();
            for seg in &segs[..segs.len() - 1] {
                if !cur.is_empty() {
                    cur.push('/');
                }
                cur.push_str(seg);
                dirs.insert(cur.clone());
            }
        }
        let blank =
            |k: PageKey, parent: Option<PageKey>, kind: PageKind, name: String, path: String| {
                LightRow {
                    key: k,
                    site: sid,
                    container: Container::Pages,
                    parent,
                    kind,
                    name,
                    path,
                    title: None,
                    url: None,
                    depth: 0,
                    tree_rank: 0,
                    category: None,
                    categories: Vec::new(),
                    tags: Vec::new(),
                    description: None,
                    modified: None,
                    published: None,
                    redirect_to: None,
                    analytics: Analytics::default(),
                }
            };
        let mut by_path: BTreeMap<String, usize> = BTreeMap::new();
        let mut built: Vec<Built> = Vec::new();
        let mut entries: Vec<(String, Option<String>)> =
            dirs.into_iter().map(|d| (d, None)).collect();
        entries.extend(pages.into_iter().map(|(p, h)| (p, Some(h))));
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, html) in entries {
            let parent = path
                .rfind('/')
                .and_then(|i| by_path.get(&path[..i]))
                .map(|&i| built[i].row.key);
            let k = PageKey(next_key);
            next_key += 1;
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            let Some(html) = html else {
                built.push(Built {
                    row: blank(k, parent, PageKind::Dir, name, path.clone()),
                    html: None,
                    redirect: None,
                });
                by_path.insert(path, built.len() - 1);
                continue;
            };
            // The page's declared identity, read once at build from
            // the head alone — the lowering waits for the graft.
            let doc = scraper::Html::parse_document(&html);
            let head = quarb_text_html::head_meta(&doc);
            // The page's own URL: declared (canonical, or Parsoid's
            // dc:isVersionOf), else its path under the base.
            let url = match head.declared_value("canonical") {
                Some(Value::Str(u)) => Some(u),
                _ => declared_url(&doc).map(|u| u.to_string()).or_else(|| {
                    base.as_ref()
                        .and_then(|b| b.join(&path).ok())
                        .map(|u| u.to_string())
                }),
            };
            let redirect = redirect_target(&doc);
            let mut row = blank(k, parent, PageKind::Page, name, path.clone());
            row.title = head.title.clone();
            row.url = url;
            row.category = head.category.clone();
            row.categories = head.category.iter().cloned().collect();
            row.tags = head.tags.clone();
            row.description = head.description.clone();
            row.modified = head.modified.clone();
            row.published = head.published.clone();
            built.push(Built {
                row,
                html: Some(html),
                redirect,
            });
            by_path.insert(path, built.len() - 1);
        }

        // 2. Tree order: pre-order, siblings by name (byte order).
        let mut kids: HashMap<Option<PageKey>, Vec<usize>> = HashMap::new();
        for (i, b) in built.iter().enumerate() {
            kids.entry(b.row.parent).or_default().push(i);
        }
        for v in kids.values_mut() {
            v.sort_by(|&a, &b| {
                built[a]
                    .row
                    .name
                    .as_bytes()
                    .cmp(built[b].row.name.as_bytes())
            });
        }
        let mut rank = 0u64;
        let mut stack: Vec<(usize, u32)> = kids
            .get(&None)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .map(|i| (i, 1))
            .collect();
        while let Some((i, depth)) = stack.pop() {
            rank += 1;
            built[i].row.tree_rank = rank;
            built[i].row.depth = depth;
            let k = built[i].row.key;
            if let Some(cs) = kids.get(&Some(k)) {
                for &c in cs.iter().rev() {
                    stack.push((c, depth + 1));
                }
            }
        }

        // 2b. Redirect stubs (spec: Text Readings, the web model,
        // draft 164): a page whose head carries a meta refresh, or
        // whose script sends the browser on, stands for the page its
        // URL resolves to within the site — kind `redirect`,
        // `->redirect` the target; its own text is no content, so
        // its html is dropped before the lowering and the links.
        let targets: Vec<(usize, Option<usize>)> = built
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let href = b.redirect.as_ref()?;
                let page_url = b.row.url.as_deref().and_then(|u| url::Url::parse(u).ok());
                let to = page_url
                    .and_then(|pu| pu.join(href).ok())
                    .and_then(|mut u| {
                        u.set_fragment(None);
                        base.as_ref().and_then(|bs| site_path(&u, bs))
                    })
                    .and_then(|p| {
                        path_variants(&p)
                            .iter()
                            .find_map(|v| by_path.get(v.as_str()).copied())
                    })
                    .filter(|&j| j != i && built[j].row.kind != PageKind::Dir);
                Some((i, to))
            })
            .collect();
        for (i, to) in targets {
            built[i].row.kind = PageKind::Redirect;
            built[i].row.redirect_to = to.map(|j| built[j].row.key);
            built[i].html = None;
        }

        // 3. Categories and tags as rows of their own containers.
        let mut cat_names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut tag_names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, b) in built.iter().enumerate() {
            if b.row.kind != PageKind::Page {
                continue;
            }
            for c in &b.row.categories {
                cat_names.entry(c.clone()).or_default().push(i);
            }
            for t in &b.row.tags {
                tag_names.entry(t.clone()).or_default().push(i);
            }
        }
        let mut rows: Vec<LightRow> = built.iter().map(|b| b.row.clone()).collect();
        let mut in_redirects: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        for r in &rows {
            if let Some(t) = r.redirect_to {
                in_redirects.entry(t).or_default().push(r.key);
            }
        }
        let mut members: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut page_categories: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut page_tags: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut children: HashMap<(Container, Option<PageKey>), Vec<PageKey>> = HashMap::new();
        let mut key_of_name: HashMap<(PageKind, String), PageKey> = HashMap::new();
        for (container, names, kind) in [
            (Container::Categories, &cat_names, PageKind::Category),
            (Container::Tags, &tag_names, PageKind::Tag),
        ] {
            let mut top = Vec::new();
            for (rank, (name, pages)) in names.iter().enumerate() {
                let k = PageKey(next_key);
                next_key += 1;
                rows.push(LightRow {
                    key: k,
                    site: sid,
                    container,
                    parent: None,
                    kind,
                    name: name.clone(),
                    // The path column carries the trait spelling
                    // (`<tag:X>` / `<category:X>` name the term by it).
                    path: quarb_text::HeadMeta::trait_name(name),
                    title: Some(name.clone()),
                    url: None,
                    depth: 1,
                    tree_rank: rank as u64 + 1,
                    category: None,
                    categories: Vec::new(),
                    tags: Vec::new(),
                    description: None,
                    modified: None,
                    published: None,
                    redirect_to: None,
                    analytics: Analytics::default(),
                });
                top.push(k);
                let mut ms: Vec<PageKey> = pages.iter().map(|&i| built[i].row.key).collect();
                ms.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
                members.insert(k, ms);
                key_of_name.insert((kind, name.clone()), k);
            }
            children.insert((container, None), top);
        }
        // A page's categories and tags in its own declared order.
        for b in &built {
            if b.row.kind != PageKind::Page {
                continue;
            }
            let cs: Vec<PageKey> = b
                .row
                .categories
                .iter()
                .filter_map(|c| key_of_name.get(&(PageKind::Category, c.clone())).copied())
                .collect();
            let ts: Vec<PageKey> = b
                .row
                .tags
                .iter()
                .filter_map(|t| key_of_name.get(&(PageKind::Tag, t.clone())).copied())
                .collect();
            if !cs.is_empty() {
                page_categories.insert(b.row.key, cs);
            }
            if !ts.is_empty() {
                page_tags.insert(b.row.key, ts);
            }
        }
        // the tree's child lists, in rank order
        for (parent, is) in &kids {
            let mut ks: Vec<PageKey> = is.iter().map(|&i| built[i].row.key).collect();
            ks.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
            children.insert((Container::Pages, *parent), ks);
        }

        // 4. Links from the DOM, resolved against the page's URL.
        // A page before a directory of the same stem: a wiki's
        // `X` page sits beside its `X/` subpages, and a link to
        // `X` means the page.
        let page_by_path: HashMap<&str, PageKey> = by_path
            .iter()
            .filter(|(_, i)| built[**i].row.kind != PageKind::Dir)
            .map(|(p, &i)| (p.as_str(), built[i].row.key))
            .collect();
        let key_by_path: HashMap<&str, PageKey> = by_path
            .iter()
            .map(|(p, &i)| (p.as_str(), built[i].row.key))
            .collect();
        let mut by_url: HashMap<String, PageKey> = HashMap::new();
        for r in &rows {
            if let Some(u) = &r.url {
                by_url.insert(u.trim_end_matches('#').to_string(), r.key);
            }
        }
        let mut out_links: HashMap<PageKey, Vec<LinkRow>> = HashMap::new();
        let mut html: HashMap<PageKey, String> = HashMap::new();
        for b in built.iter_mut() {
            let Some(h) = b.html.take() else { continue };
            let k = b.row.key;
            let page_url = b.row.url.as_deref().and_then(|u| url::Url::parse(u).ok());
            let mut seen: HashMap<(String, PageKey, Option<String>), ()> = HashMap::new();
            let mut lrs = Vec::new();
            let (base_href, found) = dom_links(&h);
            // `<base href>` moves the join base (Parsoid pages
            // write `//host/wiki/`), itself relative to the page.
            let join_base = match (&page_url, base_href) {
                (Some(pu), Some(b)) => pu.join(&b).ok().or_else(|| Some(pu.clone())),
                (pu, _) => pu.clone(),
            };
            for f in found {
                let Some(pu) = &join_base else { break };
                let Ok(mut u) = pu.join(&f.href) else {
                    continue;
                };
                let fragment = u
                    .fragment()
                    .map(str::trim)
                    .filter(|f| !f.is_empty())
                    .map(str::to_string);
                u.set_fragment(None);
                let to = base
                    .as_ref()
                    .and_then(|bs| site_path(&u, bs))
                    .and_then(|p| {
                        path_variants(&p)
                            .iter()
                            .find_map(|v| page_by_path.get(v.as_str()).copied())
                            .or_else(|| {
                                path_variants(&p)
                                    .iter()
                                    .find_map(|v| key_by_path.get(v.as_str()).copied())
                            })
                    });
                // A same-site path no page answers to is a redlink —
                // unless it names a file by its extension (the
                // spec's PDF): that is a resource, shipped or not,
                // as another host's target is (spec: Text Readings,
                // the web reading).
                let red = f.red
                    || (to.is_none()
                        && base
                            .as_ref()
                            .is_some_and(|bs| bs.host_str() == u.host_str())
                        && !has_file_extension(&u));
                // a same-page fragment lands on the page's own node
                // bearing the name; a page's own canonical points at
                // itself and stays; any other self-link is no edge
                let fragment = fragment.filter(|_| to.is_some());
                if let Some(t) = to {
                    let own = t == k && (f.role == "canonical" || fragment.is_some());
                    if (t == k && !own) || seen.contains_key(&(f.role.clone(), t, fragment.clone()))
                    {
                        continue;
                    }
                    seen.insert((f.role.clone(), t, fragment.clone()), ());
                }
                lrs.push(LinkRow {
                    to,
                    to_url: u.to_string(),
                    anchor: f.anchor,
                    section: f.section,
                    red,
                    via_template: f.transcluded,
                    role: f.role,
                    props: f.props,
                    fragment,
                });
            }
            out_links.insert(k, lrs);
            html.insert(k, h);
        }
        // 4b. Resources: a link whose target is no page of the site
        // and no redlink — a file by its extension, another host's
        // page — points at a node of kind `resource`, one per URL,
        // carrying the URL (and the site path where it is the
        // site's own); `<-link` from it lists the pages pointing at
        // it. Not a page: outside the page graph the analytics run
        // over, after every page in the document order.
        let mut resource_by_url: HashMap<String, PageKey> = HashMap::new();
        let mut resource_in: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        let mut page_keys: Vec<PageKey> = out_links.keys().copied().collect();
        page_keys.sort();
        for from in page_keys {
            let lrs = out_links.get_mut(&from).expect("a page's links");
            let mut seen_res: HashSet<(String, PageKey)> = HashSet::new();
            let mut kept = Vec::with_capacity(lrs.len());
            for mut l in lrs.drain(..) {
                if l.to.is_none() && !l.red {
                    let key = match resource_by_url.get(&l.to_url) {
                        Some(&k) => k,
                        None => {
                            let k = PageKey(rows.len() as u64 + 1);
                            let u = url::Url::parse(&l.to_url).ok();
                            let path = u
                                .as_ref()
                                .and_then(|u| base.as_ref().and_then(|b| site_path(u, b)))
                                .unwrap_or_else(|| l.to_url.clone());
                            // named by its path (a same-site file) or
                            // its URL, so no page's name is shadowed
                            let name = path.clone();
                            let mut row = blank(k, None, PageKind::Resource, name, path);
                            row.url = Some(l.to_url.clone());
                            row.depth = 1;
                            rows.push(row);
                            resource_by_url.insert(l.to_url.clone(), k);
                            k
                        }
                    };
                    if !seen_res.insert((l.role.clone(), key)) {
                        continue;
                    }
                    l.to = Some(key);
                    resource_in.entry(key).or_default().push(from);
                }
                kept.push(l);
            }
            *lrs = kept;
        }
        let resources: HashSet<PageKey> = resource_by_url.values().copied().collect();
        // 5. Analytics over the page graph — the same computation
        // the index-backed stores run.
        // The graph's nodes: the pages, and any category with a
        // document of its own (none from a set of files).
        let pages: Vec<PageKey> = rows
            .iter()
            .filter(|r| {
                r.kind == PageKind::Page
                    || (r.kind == PageKind::Category && html.contains_key(&r.key))
            })
            .map(|r| r.key)
            .collect();
        let mut edges: Vec<(PageKey, PageKey)> = Vec::new();
        let mut reds: HashMap<PageKey, u32> = HashMap::new();
        // every role's in-links, for `<-navbar`, `<-footer` and kin;
        // the page graph the analytics run over is the content links
        let mut in_roles: HashMap<PageKey, Vec<(String, PageKey)>> = HashMap::new();
        let mut in_fragments: HashMap<PageKey, Vec<(String, String, PageKey)>> = HashMap::new();
        for (from, lrs) in &out_links {
            for l in lrs {
                if let Some(t) = l.to {
                    in_roles.entry(t).or_default().push((l.role.clone(), *from));
                    if let Some(f) = &l.fragment {
                        in_fragments
                            .entry(t)
                            .or_default()
                            .push((f.clone(), l.role.clone(), *from));
                    }
                }
                if let Some(t) = l.to
                    && !resources.contains(&t)
                    && l.role == "link"
                {
                    edges.push((*from, t));
                }
                if l.red {
                    *reds.entry(*from).or_default() += 1;
                }
            }
        }
        let computed = crate::db::analyze::compute(&pages, &edges, &reds);
        let link_count = edges.len() as u64;
        let mut in_links: HashMap<PageKey, Vec<PageKey>> = HashMap::new();
        for (from, to) in &edges {
            in_links.entry(*to).or_default().push(*from);
        }
        for (k, v) in resource_in {
            in_links.insert(k, v);
        }
        for v in in_links.values_mut() {
            v.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
            v.dedup();
        }
        for v in in_roles.values_mut() {
            v.sort_by_key(|(_, k)| rows[(k.0 - 1) as usize].tree_rank);
            v.dedup();
        }
        for (k, a) in computed {
            rows[(k.0 - 1) as usize].analytics = a;
        }

        // 6. Document order (spec: Text Readings, the web reading):
        // the tree's pre-order with siblings by pagerank descending,
        // ties by name — the most linked pages first at every level,
        // so `//page[..10]` and every search result come most-linked
        // first; the lists kept in rank order follow.
        let mut by_parent: HashMap<Option<PageKey>, Vec<PageKey>> = HashMap::new();
        for r in &rows {
            if r.container == Container::Pages {
                by_parent.entry(r.parent).or_default().push(r.key);
            }
        }
        for v in by_parent.values_mut() {
            v.sort_by(|a, b| {
                let ra = &rows[(a.0 - 1) as usize];
                let rb = &rows[(b.0 - 1) as usize];
                rb.analytics
                    .pagerank
                    .partial_cmp(&ra.analytics.pagerank)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| ra.name.as_bytes().cmp(rb.name.as_bytes()))
            });
        }
        let mut rank = 0u64;
        let mut stack: Vec<PageKey> = by_parent
            .get(&None)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .collect();
        while let Some(k) = stack.pop() {
            rank += 1;
            rows[(k.0 - 1) as usize].tree_rank = rank;
            if let Some(cs) = by_parent.get(&Some(k)) {
                stack.extend(cs.iter().rev().copied());
            }
        }
        for ((container, parent), ks) in children.iter_mut() {
            if *container == Container::Pages {
                *ks = by_parent.get(parent).cloned().unwrap_or_default();
            }
        }
        for v in members.values_mut() {
            v.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
        }
        for v in in_links.values_mut() {
            v.sort_by_key(|k| rows[(k.0 - 1) as usize].tree_rank);
        }
        for v in in_roles.values_mut() {
            v.sort_by_key(|(_, k)| rows[(k.0 - 1) as usize].tree_rank);
        }
        let catalog = Catalog {
            sites: vec![SiteRow {
                id: sid,
                host,
                base_url: site.base_url,
                snapshot: site.snapshot,
                hierarchy: Hierarchy::Directories,
                page_count: rows.iter().filter(|r| r.kind == PageKind::Page).count() as u64,
                link_count,
            }],
            lowering: format!("quarb-text-html {}", env!("CARGO_PKG_VERSION")),
        };
        MemoryStore {
            catalog,
            rows,
            html,
            children,
            out_links,
            in_links,
            in_roles,
            in_fragments,
            members,
            page_categories,
            page_tags,
            by_url,
            in_redirects,
        }
    }

    fn row_ref(&self, key: PageKey) -> Option<&LightRow> {
        self.rows.get((key.0.checked_sub(1)?) as usize)
    }
}

impl WebStore for MemoryStore {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    fn row(&self, key: PageKey) -> Option<LightRow> {
        self.row_ref(key).cloned()
    }
    fn children(
        &self,
        _site: SiteId,
        container: Container,
        parent: Option<PageKey>,
    ) -> Vec<PageKey> {
        self.children
            .get(&(container, parent))
            .cloned()
            .unwrap_or_default()
    }
    fn children_named(
        &self,
        site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        name: &str,
    ) -> Vec<PageKey> {
        self.children(site, container, parent)
            .into_iter()
            .filter(|k| self.row_ref(*k).is_some_and(|r| r.name == name))
            .collect()
    }
    fn related(&self, key: PageKey, kind: LinkKind, dir: LinkDir) -> Vec<PageKey> {
        match (kind, dir) {
            (LinkKind::Link, LinkDir::Out) => self
                .out_links
                .get(&key)
                .map(|v| {
                    v.iter()
                        .filter(|l| l.role == "link")
                        .filter_map(|l| l.to)
                        .collect()
                })
                .unwrap_or_default(),
            (LinkKind::Link, LinkDir::In) => self.in_links.get(&key).cloned().unwrap_or_default(),
            (LinkKind::Category, LinkDir::Out) => {
                self.page_categories.get(&key).cloned().unwrap_or_default()
            }
            (LinkKind::Tag, LinkDir::Out) => self.page_tags.get(&key).cloned().unwrap_or_default(),
            (LinkKind::Category | LinkKind::Tag, LinkDir::In) => {
                self.members.get(&key).cloned().unwrap_or_default()
            }
            (LinkKind::Redirect, LinkDir::Out) => self
                .row_ref(key)
                .and_then(|r| r.redirect_to)
                .into_iter()
                .collect(),
            (LinkKind::Redirect, LinkDir::In) => {
                self.in_redirects.get(&key).cloned().unwrap_or_default()
            }
        }
    }
    fn in_roles(&self, key: PageKey) -> Vec<(String, PageKey)> {
        self.in_roles.get(&key).cloned().unwrap_or_default()
    }
    fn in_fragments(&self, key: PageKey) -> Vec<(String, String, PageKey)> {
        self.in_fragments.get(&key).cloned().unwrap_or_default()
    }
    fn link_rows(&self, key: PageKey) -> Vec<LinkRow> {
        self.out_links.get(&key).cloned().unwrap_or_default()
    }
    fn html(&self, key: PageKey) -> Option<String> {
        self.html.get(&key).cloned()
    }
    /// Rows of `kind` below `parent`, by rank: the container's rows
    /// are in rank order already, and a subtree is a contiguous
    /// run of ranks after its root, bounded by depth.
    fn descendants_of_kind(
        &self,
        _site: SiteId,
        container: Container,
        parent: Option<PageKey>,
        kind: PageKind,
    ) -> Vec<(PageKey, u32)> {
        let (from_rank, base_depth) = match parent.and_then(|p| self.row_ref(p)) {
            Some(r) => (r.tree_rank, r.depth),
            None => (0, 0),
        };
        let mut out = Vec::new();
        let mut ranked: Vec<&LightRow> = self
            .rows
            .iter()
            .filter(|r| r.container == container && r.tree_rank > from_rank)
            .collect();
        ranked.sort_by_key(|r| r.tree_rank);
        for r in ranked {
            if r.depth <= base_depth {
                break;
            }
            if PageKind::answers_kind(r.kind, kind) {
                out.push((r.key, r.depth - base_depth));
            }
        }
        out
    }

    fn page_by_url(&self, url: &str) -> Option<PageKey> {
        let u = url.split('#').next().unwrap_or(url);
        if let Some(k) = self.by_url.get(u) {
            return Some(*k);
        }
        // A URL naming a directory lands on its index page.
        let base = url::Url::parse(&self.catalog.sites[0].base_url).ok()?;
        let parsed = url::Url::parse(u).ok()?;
        let path = site_path(&parsed, &base)?;
        let variants = path_variants(&path);
        self.rows
            .iter()
            .find(|r| {
                r.container == Container::Pages
                    && r.kind == PageKind::Page
                    && variants.contains(&r.path)
            })
            .map(|r| r.key)
    }
}

/// The paths a link target may be stored under, most literal
/// first: the path itself; a directory URL's index page
/// (`docs/` → `docs/index.html`, already applied by
/// [`site_path`]) also answers to the bare directory when the
/// site was fetched under extension-less URLs (`docs`), and an
/// extension-less target answers to its own page file (`X` →
/// `X.html`, the wiki convention) and to its index page when the
/// site was packed as files. Every spelling of one page meets.
pub(crate) fn path_variants(path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    if let Some(dir) = path.strip_suffix("/index.html") {
        out.push(dir.to_string());
    } else if path == "index.html" {
        // The root has no bare spelling.
    } else {
        let last = path.rsplit('/').next().unwrap_or(path);
        if !last.contains('.') {
            out.push(format!("{path}.html"));
            out.push(format!("{path}/index.html"));
        }
    }
    out
}
