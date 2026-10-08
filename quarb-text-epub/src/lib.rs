//! The EPUB text-level adapter: `text:book.epub` beside
//! `text:page.html` — mostly composition, by design (the
//! document-adapter row in lang/TODO.md).
//!
//! An EPUB is a zip of XHTML with its reading order declared
//! twice over: `META-INF/container.xml` names the OPF package,
//! the OPF's `<spine>` lists the content documents in order, and
//! each chapter is ordinary XHTML the existing HTML adapter
//! already lowers. This crate walks exactly that declared chain —
//! container, package, spine — and concatenates each linear
//! chapter's blocks; the chapters' own headings carry the outline
//! and [`TextModel::build`] derives the section tree once, as for
//! every adapter.
//!
//! Limits, recorded in the design row: `linear="no"` spine
//! items are declared out of the reading order and are skipped;
//! the navigation document (`nav.xhtml` / NCX) is declared
//! structure that a later pass may synthesize section titles from
//! (the PDF outline's sibling), not this one; non-XHTML spine
//! items (SVG pages) are skipped. Plain `book.epub` keeps its
//! archive reading with the raw XHTML grafted — the two-level
//! pattern, one file.

use std::io::Read;

use quarb::source::SourceDecoding;

use quarb_text::{Block, TextModel};
use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug, thiserror::Error)]
pub enum EpubError {
    #[error("member {name}: {error}")]
    MemberText { name: String, error: String },
    #[error("not an epub (zip) container: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("META-INF/container.xml is missing — not an EPUB")]
    NoContainer,
    #[error("container.xml names no package rootfile")]
    NoRootfile,
    #[error("the package ({0}) is missing from the archive")]
    NoPackage(String),
}

/// Parse `.epub` bytes into the text-level model.
pub fn parse(bytes: &[u8]) -> Result<TextModel, EpubError> {
    parse_with_decoding(bytes, &SourceDecoding::default())
}

/// [`parse`] under a mount's source-boundary rule: every member
/// decodes by `?encoding=`, its byte-order mark, its XML
/// declaration, else strict UTF-8.
pub fn parse_with_decoding(
    bytes: &[u8],
    decoding: &SourceDecoding,
) -> Result<TextModel, EpubError> {
    Ok(TextModel::build(blocks_with_decoding(bytes, decoding)?))
}

/// Lower `.epub` bytes into the block event stream: the spine's
/// linear chapters, in declared order, through the HTML reading.
pub fn blocks(bytes: &[u8]) -> Result<Vec<Block>, EpubError> {
    blocks_with_decoding(bytes, &SourceDecoding::default())
}

/// [`blocks`] under a mount's source-boundary rule.
pub fn blocks_with_decoding(
    bytes: &[u8],
    decoding: &SourceDecoding,
) -> Result<Vec<Block>, EpubError> {
    let mut zip = Members::new(zip::ZipArchive::new(std::io::Cursor::new(bytes))?, decoding);
    let container = zip
        .get("META-INF/container.xml")?
        .ok_or(EpubError::NoContainer)?;
    let opf_path = rootfile(&container).ok_or(EpubError::NoRootfile)?;
    let opf = zip
        .get(&opf_path)?
        .ok_or_else(|| EpubError::NoPackage(opf_path.clone()))?;
    let base = match opf_path.rfind('/') {
        Some(i) => &opf_path[..=i],
        None => "",
    };
    let mut out = Vec::new();
    for href in spine_hrefs(&opf) {
        let path = join(base, &href);
        // A missing or non-text spine member is skipped, not an
        // error: the spine may name resources this reading has no
        // lowering for (SVG pages), and a broken href should not
        // hide the readable chapters.
        if let Some(xhtml) = zip.get(&path)? {
            out.extend(scoped(quarb_text_html::blocks(&xhtml), &href));
        }
    }
    Ok(out)
}

/// The package's members, read under the mount's rule: a member
/// decodes by `?encoding=`, its byte-order mark, its XML
/// declaration, else strict UTF-8. A member that decodes under
/// none is an error naming it, never a part silently missing.
struct Members<'a> {
    zip: zip::ZipArchive<std::io::Cursor<&'a [u8]>>,
    explicit: Option<String>,
}

impl<'a> Members<'a> {
    fn new(zip: zip::ZipArchive<std::io::Cursor<&'a [u8]>>, decoding: &SourceDecoding) -> Self {
        Members {
            zip,
            explicit: decoding.explicit.clone(),
        }
    }

    /// `None` when the package has no such member.
    fn get(&mut self, name: &str) -> Result<Option<String>, EpubError> {
        let Ok(mut f) = self.zip.by_name(name) else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)
            .map_err(|e| EpubError::MemberText {
                name: name.to_string(),
                error: e.to_string(),
            })?;
        let d = quarb::source::decode(
            &bytes,
            self.explicit.as_deref(),
            quarb::source::xml_declared_encoding,
        )
        .map_err(|e| EpubError::MemberText {
            name: name.to_string(),
            error: e.to_string(),
        })?;
        Ok(Some(d.text))
    }
}

/// The OPF package path from container.xml's first rootfile.
fn rootfile(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match &ev {
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == b"rootfile" => {
                if let Some(p) = attr(e, b"full-path") {
                    return Some(p);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// The spine's hrefs, in declared order: manifest id → href, then
/// each `<itemref>` unless it declares itself out of the reading
/// order (`linear="no"`).
fn spine_hrefs(opf: &str) -> Vec<String> {
    let mut manifest: std::collections::HashMap<String, String> = Default::default();
    let mut order: Vec<String> = Vec::new();
    let mut reader = Reader::from_str(opf);
    let mut buf = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        match &ev {
            Event::Start(e) | Event::Empty(e) => match local(e.name().as_ref()) {
                b"item" => {
                    if let (Some(id), Some(href)) = (attr(e, b"id"), attr(e, b"href")) {
                        manifest.insert(id, href);
                    }
                }
                b"itemref" => {
                    let linear = attr(e, b"linear").map_or(true, |v| v != "no");
                    if linear && let Some(idref) = attr(e, b"idref") {
                        order.push(idref);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    order
        .into_iter()
        .filter_map(|id| manifest.get(&id).map(|h| decode_href(h)))
        .collect()
}

/// Resolve `href` against the package directory, folding `../`.
/// A chapter's ids in its own namespace: an `id` reused across
/// chapters (`n1` in every chapter's notes) names one bearer per
/// chapter, `<href>#<id>`, and a reference keeps landing in its
/// own chapter (`#n1`) or the one it names (`ch2.xhtml#n1`),
/// joined against the chapter's directory as the package does.
fn scoped(blocks: Vec<Block>, href: &str) -> Vec<Block> {
    let dir = match href.rfind('/') {
        Some(i) => &href[..=i],
        None => "",
    };
    blocks
        .into_iter()
        .map(|b| match b {
            Block::Label { onym } => Block::Label {
                onym: format!("{href}#{onym}"),
            },
            Block::Anchor { onym } => Block::Anchor {
                onym: format!("{href}#{onym}"),
            },
            Block::Ref {
                target,
                text,
                internal,
            } => {
                let target = if let Some(frag) = target.strip_prefix('#') {
                    format!("{href}#{frag}")
                } else if internal || is_in_package(&target) {
                    match target.split_once('#') {
                        Some((file, frag)) => format!("{}#{frag}", join(dir, &decode_href(file))),
                        None => join(dir, &decode_href(&target)),
                    }
                } else {
                    target
                };
                let internal = internal || is_in_package(&target);
                Block::Ref {
                    target,
                    text,
                    internal,
                }
            }
            other => other,
        })
        .collect()
}

/// Whether a link target names a member of the package rather
/// than the world outside: a relative path, no scheme.
fn is_in_package(target: &str) -> bool {
    !target.contains("://") && !target.starts_with("mailto:") && !target.starts_with('/')
}

fn join(base: &str, href: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Strip a fragment and decode the percent-escapes an href may
/// carry (`My%20Chapter.xhtml`).
fn decode_href(href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let bytes = href.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| href.to_string())
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn local(name: &[u8]) -> &[u8] {
    match name.iter().position(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

fn attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
}
