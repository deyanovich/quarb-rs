//! The filesystem store: a directory of built pages (a static
//! site's output tree), read once into memory.

use crate::memory::{MemoryStore, PageFile, SiteInput};
use crate::WebAdapter;
use std::path::Path;

/// Open a directory of pages as a site whose paths join against
/// `base_url`. Only `.html` / `.htm` files become pages; hidden
/// entries (`.git`, `.cito.yml`) are skipped.
pub fn open_dir(root: &Path, base_url: &str) -> std::io::Result<WebAdapter<MemoryStore>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let lower = name.to_string_lossy().to_ascii_lowercase();
            if !(lower.ends_with(".html") || lower.ends_with(".htm")) {
                continue;
            }
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let html = String::from_utf8_lossy(&std::fs::read(&p)?).into_owned();
            files.push(PageFile { path: rel, html });
        }
    }
    let snapshot = std::fs::metadata(root)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string());
    Ok(WebAdapter::new(MemoryStore::build(
        SiteInput { base_url: base_url.to_string(), snapshot },
        files,
    )))
}
