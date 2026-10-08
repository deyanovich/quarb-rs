//! The archive store: a tarball (or zip) of a site's built pages,
//! held in memory — the form a browser has after one fetch, and
//! the form quarb.org's search page runs on.

use crate::WebAdapter;
use crate::memory::{MemoryStore, PageFile, SiteInput};

/// Open a tar / tar.gz held in memory as a site whose pages join
/// against `base_url` (`https://quarb.org/`).
pub fn open_tar(
    bytes: &[u8],
    base_url: &str,
) -> Result<WebAdapter<MemoryStore>, quarb_archive::ArchiveError> {
    let archive = quarb_archive::ArchiveAdapter::from_tar_bytes(bytes)?;
    Ok(from_archive(&archive, base_url))
}

/// Open an archive file on disk (`.tar`, `.tar.gz`, `.zip`).
pub fn open_path(
    path: &std::path::Path,
    base_url: &str,
) -> Result<WebAdapter<MemoryStore>, quarb_archive::ArchiveError> {
    open_path_with_decoding(
        path,
        base_url,
        quarb::source::SourceDecoding::default()
            .with_declared(|_, b| quarb_html::declared_encoding(b)),
    )
}

/// [`open_path`] under a mount's source-boundary rule: each page
/// decodes by `?encoding=`, then a byte-order mark, then the charset
/// it declares, then strict UTF-8 — and a page that decodes under
/// none refuses the mount, as the directory store does.
pub fn open_path_with_decoding(
    path: &std::path::Path,
    base_url: &str,
    decoding: quarb::source::SourceDecoding,
) -> Result<WebAdapter<MemoryStore>, quarb_archive::ArchiveError> {
    let archive = quarb_archive::ArchiveAdapter::open_with_decoding(path, decoding)?;
    if let Some(page) = archive.undecoded().into_iter().find(|p| {
        let lower = p.to_ascii_lowercase();
        lower.ends_with(".html") || lower.ends_with(".htm")
    }) {
        return Err(quarb_archive::ArchiveError::Undecoded(page));
    }
    Ok(from_archive(&archive, base_url))
}

fn from_archive(
    archive: &quarb_archive::ArchiveAdapter,
    base_url: &str,
) -> WebAdapter<MemoryStore> {
    let files = archive
        .files()
        .into_iter()
        .map(|(path, html)| PageFile { path, html })
        .collect();
    let store = MemoryStore::build(
        SiteInput {
            base_url: base_url.to_string(),
            snapshot: None,
        },
        files,
    );
    WebAdapter::new(store)
}
