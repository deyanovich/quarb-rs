//! The fixture site the store tests share.
#![allow(dead_code)]

use quarb_web::{MemoryStore, PageFile, SiteInput, WebAdapter};

pub fn page(title: &str, extra_head: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><title>{title}</title>{extra_head}</head><body><main><h1>{title}</h1>{body}</main></body></html>"
    )
}

pub fn files() -> Vec<PageFile> {
    vec![
        PageFile {
            path: "index.html".into(),
            html: page(
                "Home",
                "<meta name=\"description\" content=\"The front page\"><meta property=\"article:section\" content=\"Start\">",
                "<p>Read the <a href=\"/guides/jq.html\">jq guide</a> or the <a href=\"guides/sql.html#joins\">SQL guide</a>.</p><ul><li><a href=\"about.html\">About</a></li></ul>",
            ),
        },
        PageFile {
            path: "about.html".into(),
            html: page(
                "About",
                "<meta property=\"article:section\" content=\"Start\">",
                "<p>Nothing links here but the <a href=\"/\">home page</a>.</p>",
            ),
        },
        PageFile {
            path: "guides/jq.html".into(),
            html: page(
                "Quarb for jq users",
                "<meta name=\"keywords\" content=\"jq, JSON\"><meta property=\"article:section\" content=\"Guides\"><link rel=\"canonical\" href=\"https://example.org/guides/jq.html\">",
                "<h2 id=\"filters\">Filters</h2><p>Compare with <a href=\"sql.html\">SQL</a> and <a href=\"/missing.html\">a page that is not there</a>.</p>",
            ),
        },
        PageFile {
            path: "guides/sql.html".into(),
            html: page(
                "Quarb for SQL users",
                "<meta name=\"keywords\" content=\"SQL, JSON\"><meta property=\"article:section\" content=\"Guides\">",
                "<h2 id=\"joins\">Joins</h2><p>See the <a href=\"jq.html\">jq guide</a>.</p><p>Back <a href=\"../index.html\">home</a>.</p><h3><a href=\"/about.html\">About</a></h3>",
            ),
        },
        PageFile {
            path: "guides/notes.html".into(),
            html: page(
                "Notes",
                "<meta property=\"article:section\" content=\"Guides\">",
                "<p>No page links here.</p>",
            ),
        },
        PageFile {
            path: "style.css".into(),
            html: "body{}".into(),
        },
    ]
}

pub fn site() -> WebAdapter<MemoryStore> {
    WebAdapter::new(MemoryStore::build(
        SiteInput {
            base_url: "https://example.org/".into(),
            snapshot: None,
        },
        files(),
    ))
}
