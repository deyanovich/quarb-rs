//! The archive store: a tarball (or zip) of a site's built pages,
//! held in memory — the form a browser has after one fetch, and
//! the form quarb.org's search page runs on.

use crate::memory::{MemoryStore, PageFile, SiteInput};
use crate::WebAdapter;

/// Open a tar / tar.gz held in memory as a site whose pages join
/// against `base_url` (`https://quarb.org/`).
pub fn open_tar(bytes: &[u8], base_url: &str) -> Result<WebAdapter<MemoryStore>, quarb_archive::ArchiveError> {
    let archive = quarb_archive::ArchiveAdapter::from_tar_bytes(bytes)?;
    Ok(from_archive(&archive, base_url))
}

/// Open an archive file on disk (`.tar`, `.tar.gz`, `.zip`).
pub fn open_path(path: &std::path::Path, base_url: &str) -> Result<WebAdapter<MemoryStore>, quarb_archive::ArchiveError> {
    let archive = quarb_archive::ArchiveAdapter::open(path)?;
    Ok(from_archive(&archive, base_url))
}

fn from_archive(archive: &quarb_archive::ArchiveAdapter, base_url: &str) -> WebAdapter<MemoryStore> {
    let files = archive
        .files()
        .into_iter()
        .map(|(path, html)| PageFile { path, html })
        .collect();
    let store = MemoryStore::build(
        SiteInput { base_url: base_url.to_string(), snapshot: None },
        files,
    );
    WebAdapter::new(store)
}
