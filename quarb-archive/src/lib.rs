//! Zip, tar and 7z archive adapter for the Quarb query engine.
//!
//! Formats, every one through a pure-Rust library (so the wasm
//! build reads them too): zip (members deflate, deflate64, bzip2,
//! lzma; stored), tar plain and compressed with gzip, xz, bzip2
//! or zstd, and 7z (LZMA/LZMA2, bzip2 and zstd members excluded).
//! The format is told by its magic bytes, the extension deciding
//! only a plain `.tar`. Two more formats ride beside them: a CHM
//! help file (`ITSF`, through `libchm`, pure Rust): its members
//! under their paths, the HTML pages grafting as HTML, the `.hhc`
//! and `.hhk` sitemaps as members too, the `#SYSTEM` record's
//! locale naming the code page a page without a `<meta charset>`
//! decodes under; and RAR (`Rar!`), behind the opt-in `rar`
//! feature over the `unrar` crate, which binds rarlab's C++
//! library under its own licence — off for the playground and the
//! extension, on for `qua`; without the feature a RAR is refused
//! naming the feature. An MHTML web archive (`.mhtml`, `.mht`: a MIME
//! `multipart/related` envelope, a browser's "save as one file")
//! grafts like an archive too: each part is a member under the
//! path of its `Content-Location` (its `Content-ID`, or its
//! ordinal, when it has none), transfer-decoded and read under
//! the mount's source rule with the part's own charset as the
//! declaration, so the HTML part reads as HTML through the
//! compose graft.
//!
//! An archive is a directory tree in a file: entries become
//! nodes under their path (intermediate directories synthesized),
//! a file entry's content is its value (`::`, text lossily
//! decoded), and `;;;size` / `;;;compressed` (zip) report the
//! obvious. Half the world's document formats are zip archives —
//! `.jar`, `.docx`, `.xlsx`, `.odt`, `.epub` — and composed with
//! [`quarb-compose`], their inner XML/JSON is directly queryable:
//! `qua '/word/document.xml!//w:t::text' report.docx`.
//!
//! The index (entry list) loads at open; contents load lazily per
//! entry for zip, and in one pass for tar (the format is a
//! stream). Gzip'd tars (`.tar.gz`, `.tgz`) decompress
//! transparently. Read-only, like everything.

use quarb::source::SourceDecoding;
use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

/// An error opening an archive.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("archive: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error(
        "archive: unrecognized format (expected zip, 7z, rar, chm, mhtml, or tar plain / gzip / xz / bzip2 / zstd)"
    )]
    Format,
    #[error(
        "archive: {name}: the members read so far exceed {limit} bytes decompressed; this reading holds a tar or 7z archive in memory"
    )]
    TooLarge { name: String, limit: u64 },
    #[error("archive: 7z: {0}")]
    SevenZ(String),
    #[error("archive: chm: {0}")]
    Chm(String),
    #[error("archive: rar: {0}")]
    Rar(String),
    #[error(
        "archive: a RAR archive needs the `rar` feature (the unrar library, rarlab's licence); this build reads zip, 7z, tar, chm and mhtml"
    )]
    RarUnavailable,
    #[error(
        "archive: {0} is not valid UTF-8 and declares no encoding; say it with ?encoding=LABEL"
    )]
    Undecoded(String),
    /// An entry name the mount's rule cannot decode (tar carries
    /// bytes; zip names follow zip's own UTF-8 flag).
    #[error("archive: entry name {0}")]
    Name(quarb::source::SourceError),
}

struct Node {
    name: String,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    /// A file entry's index into `contents` (`None`: directory).
    entry: Option<usize>,
    size: Option<i64>,
    compressed: Option<i64>,
}

enum Source {
    Zip(RefCell<zip::ZipArchive<std::fs::File>>),
    /// Tar contents are read up front (streaming format).
    Tar,
}

/// Whether an extension names an archive this crate opens: zip and
/// jar, tar and its compressed forms, 7z, rar. The one list every
/// front-end and the composed view consult.
pub fn archive_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "zip"
            | "jar"
            | "rar"
            | "tar"
            | "tgz"
            | "gz"
            | "7z"
            | "xz"
            | "txz"
            | "bz2"
            | "tbz2"
            | "zst"
            | "tzst"
    )
}

/// Whether an extension names a document container that is an
/// archive underneath (a zip of parts, a MIME envelope, a CHM):
/// opened as an archive when no reader takes the document itself.
pub fn container_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "docx" | "odt" | "sxw" | "epub" | "mhtml" | "mht" | "chm" | "ofd" | "hwpx"
    )
}

/// How a tar stream is compressed, told by its magic bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compression {
    None,
    Gzip,
    Xz,
    Bzip2,
    Zstd,
}

impl Compression {
    fn of(bytes: &[u8]) -> Compression {
        if bytes.starts_with(&[0x1f, 0x8b]) {
            Compression::Gzip
        } else if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
            Compression::Xz
        } else if bytes.starts_with(b"BZh") {
            Compression::Bzip2
        } else if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            Compression::Zstd
        } else {
            Compression::None
        }
    }
}

/// How much a tar or 7z archive may decompress into memory at
/// open, and how large a zip member read on demand may be: 1 GiB.
/// Past it the open (or the read) is refused rather than the
/// process exhausted — a decompression bomb is a few hundred
/// kilobytes on disk.
pub const MAX_ARCHIVE_BYTES: u64 = 1 << 30;

/// What an archive holds, from its member table alone: a zip's
/// central directory (members and their uncompressed sizes, exact,
/// the members untouched); a tar's headers, walked through the
/// stream (a compressed tar is inflated on the way, nothing kept;
/// an xz tar's index declares the size without a walk); a 7z, chm
/// or rar file's size only (its table needs the library's open).
/// Nothing is extracted. `None` when the file cannot be read.
/// The walk stops at the first member past `cap` (or past
/// [`MEMBER_BUDGET`]) and reports a lower bound.
pub fn catalog(path: &Path, cap: quarb::Cap) -> Option<quarb::Catalog> {
    catalog_at(path, &[], cap)
}

/// [`catalog_at`] for a plan's ask: the path, the cap, and the
/// filters a member table applies before counting — `::size`
/// comparisons against the uncompressed size, `<file>` / `<dir>`.
pub fn catalog_for(path: &Path, ask: &quarb::Ask<'_>) -> Option<quarb::Catalog> {
    let sieve = Sieve::of(ask);
    let mut c = catalog_with(path, ask.scope, ask.cap, &sieve)?;
    c.applied = sieve.applied.clone();
    c.traits_applied = sieve.traits_applied;
    Some(c)
}

/// [`catalog_at`] counting through a caller's sieve (quarb-open
/// shares one sieve between a directory listing and an archive).
pub fn catalog_sieved(
    path: &Path,
    scope: &[&str],
    cap: quarb::Cap,
    sieve: &Sieve<'_>,
) -> Option<quarb::Catalog> {
    catalog_with(path, scope, cap, sieve)
}

/// The filters a listing applies while counting: which of the
/// asked conjuncts it takes (`::size`), and the entry kinds the
/// traits keep.
pub struct Sieve<'a> {
    pub sizes: Vec<(quarb::Cmp, &'a Value)>,
    pub applied: Vec<bool>,
    pub files: bool,
    pub dirs: bool,
    pub traits_applied: bool,
}

impl<'a> Sieve<'a> {
    pub fn none() -> Sieve<'static> {
        Sieve {
            sizes: Vec::new(),
            applied: Vec::new(),
            files: true,
            dirs: true,
            traits_applied: false,
        }
    }

    pub fn of(ask: &quarb::Ask<'a>) -> Sieve<'a> {
        let mut s = Sieve::none();
        for c in ask.conjuncts {
            match c {
                quarb::Conjunct::Compare("size", cmp, v) => {
                    s.sizes.push((*cmp, v));
                    s.applied.push(true);
                }
                _ => s.applied.push(false),
            }
        }
        s.traits_applied = !ask.traits.is_empty();
        for t in ask.traits {
            match *t {
                "file" => s.dirs = false,
                "dir" => s.files = false,
                _ => s.traits_applied = false,
            }
        }
        s
    }

    /// Whether an entry of `size` bytes (a file) or a directory
    /// passes.
    pub fn keeps(&self, is_file: bool, size: u64) -> bool {
        if is_file && !self.files || !is_file && !self.dirs {
            return false;
        }
        let v = Value::bytes(size as i64);
        self.sizes.iter().all(|(cmp, lit)| cmp.holds(&v, lit))
    }
}

/// How a member's path stands to the literal `scope` the plan
/// records: the member itself, one under it, or elsewhere.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Placing {
    Itself,
    Under,
    Elsewhere,
}

fn placing(member: &str, scope: &[&str]) -> Placing {
    let member = member.trim_start_matches("./").trim_end_matches('/');
    let mut parts = member.split('/').filter(|p| !p.is_empty());
    for want in scope {
        match parts.next() {
            Some(p) if p == *want => {}
            _ => return Placing::Elsewhere,
        }
    }
    if parts.next().is_some() {
        Placing::Under
    } else {
        Placing::Itself
    }
}

/// [`catalog`] narrowed to the literal path `scope` names inside
/// the archive: a member (its size, exact) or the members under a
/// folder. `None` when nothing in the archive answers to the path,
/// and for the formats whose table is not walked (xz, 7z, chm,
/// rar) at any path but the root.
pub fn catalog_at(path: &Path, scope: &[&str], cap: quarb::Cap) -> Option<quarb::Catalog> {
    catalog_with(path, scope, cap, &Sieve::none())
}

fn catalog_with(
    path: &Path,
    scope: &[&str],
    cap: quarb::Cap,
    sieve: &Sieve<'_>,
) -> Option<quarb::Catalog> {
    use quarb::{Catalog, Confidence, Latency};
    let file_size = std::fs::metadata(path).ok()?.len();
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 6];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    let members = |count: u64, bytes: u64, cut: bool| {
        Catalog::plain(
            if cut {
                (count, u64::MAX)
            } else {
                (count, count)
            },
            Some(if cut {
                (bytes, u64::MAX)
            } else {
                (bytes, bytes)
            }),
            Latency::Local,
            if cut {
                Confidence::Bound
            } else {
                Confidence::Exact
            },
        )
    };
    // One member named by the path: a document whose node count is
    // the reading's to say, its bytes exact.
    let member = |size: u64| {
        Catalog::plain(
            (1, u64::MAX),
            Some((size, size)),
            Latency::Local,
            Confidence::Bound,
        )
    };
    let stop = |count: u64, bytes: u64| count >= MEMBER_BUDGET || cap.past(count, bytes);
    if head.starts_with(b"PK") {
        let f = std::fs::File::open(path).ok()?;
        let mut zip = zip::ZipArchive::new(f).ok()?;
        let (mut count, mut bytes, mut cut, mut seen) = (0u64, 0u64, false, false);
        for i in 0..zip.len() {
            let Ok(e) = zip.by_index_raw(i) else {
                continue;
            };
            match placing(e.name(), scope) {
                Placing::Elsewhere => continue,
                Placing::Itself if !e.is_dir() => return Some(member(e.size())),
                Placing::Itself => seen = true,
                Placing::Under if e.is_dir() => {
                    seen = true;
                    if sieve.keeps(false, 0) {
                        count += 1;
                    }
                }
                Placing::Under => {
                    seen = true;
                    if !sieve.keeps(true, e.size()) {
                        continue;
                    }
                    count += 1;
                    bytes += e.size();
                    if stop(count, bytes) {
                        cut = true;
                        break;
                    }
                }
            }
        }
        return seen.then(|| members(count, bytes, cut));
    }
    let compression = if head.starts_with(&[0x1f, 0x8b]) {
        Some(Compression::Gzip)
    } else if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(Compression::Xz)
    } else if head.starts_with(b"BZh") {
        Some(Compression::Bzip2)
    } else if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        Some(Compression::Zstd)
    } else if path.extension().and_then(|e| e.to_str()) == Some("tar") {
        Some(Compression::None)
    } else {
        None
    };
    let Some(compression) = compression else {
        return scope.is_empty().then(|| {
            Catalog::plain(
                (0, u64::MAX),
                Some((0, file_size)),
                Latency::Local,
                Confidence::Guess,
            )
        });
    };
    let f = std::fs::File::open(path).ok()?;
    let reader: Box<dyn Read> = match compression {
        Compression::None => Box::new(f),
        Compression::Gzip => Box::new(flate2::read::GzDecoder::new(f)),
        Compression::Bzip2 => Box::new(bzip2::read::BzDecoder::new(f)),
        Compression::Zstd => Box::new(ruzstd::decoding::StreamingDecoder::new(f).ok()?),
        Compression::Xz => {
            if !scope.is_empty() {
                return None;
            }
            let mut f = f;
            let mut compressed = Vec::new();
            f.read_to_end(&mut compressed).ok()?;
            let declared = xz_uncompressed_size(&compressed)?;
            return Some(Catalog::plain(
                (0, u64::MAX),
                Some((0, declared)),
                Latency::Local,
                Confidence::Bound,
            ));
        }
    };
    let mut ar = tar::Archive::new(reader);
    let (mut count, mut bytes, mut cut, mut seen) = (0u64, 0u64, false, false);
    for entry in ar.entries().ok()? {
        let e = entry.ok()?;
        let name = e.path().ok()?.to_string_lossy().into_owned();
        let is_file = e.header().entry_type().is_file();
        match placing(&name, scope) {
            Placing::Elsewhere => continue,
            Placing::Itself if is_file => return Some(member(e.header().size().unwrap_or(0))),
            Placing::Itself => seen = true,
            Placing::Under if !is_file => {
                seen = true;
                if e.header().entry_type().is_dir() && sieve.keeps(false, 0) {
                    count += 1;
                }
            }
            Placing::Under => {
                seen = true;
                let size = e.header().size().unwrap_or(0);
                if !sieve.keeps(true, size) {
                    continue;
                }
                count += 1;
                bytes += size;
                if stop(count, bytes) {
                    cut = true;
                    break;
                }
            }
        }
    }
    seen.then(|| members(count, bytes, cut))
}

/// Members a catalog walk counts before it stops: past this many
/// the count is a lower bound.
pub const MEMBER_BUDGET: u64 = 200_000;

/// The total uncompressed size an xz file declares: the sum over
/// its streams (concatenated streams are read from the end, each
/// footer naming its index, each index its blocks' sizes). `None`
/// when the container is not well-formed xz — then nothing is
/// known, and the caller refuses.
fn xz_uncompressed_size(bytes: &[u8]) -> Option<u64> {
    fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
        let mut v: u64 = 0;
        for i in 0..9 {
            let byte = *b.get(*at)?;
            *at += 1;
            v |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    let mut end = bytes.len();
    let mut total: u64 = 0;
    let mut streams = 0;
    while end >= 12 {
        // Stream padding: four-byte groups of zeros between streams.
        if bytes[end - 4..end] == [0, 0, 0, 0] {
            end -= 4;
            continue;
        }
        let footer = &bytes[end - 12..end];
        if &footer[10..12] != b"YZ" {
            break;
        }
        let backward = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);
        let index_size = (u64::from(backward) + 1) * 4;
        let index_start = (end as u64 - 12).checked_sub(index_size)? as usize;
        let index = &bytes[index_start..end - 12];
        if index.first() != Some(&0) {
            return None;
        }
        let mut at = 1;
        let records = varint(index, &mut at)?;
        let mut blocks: u64 = 0;
        for _ in 0..records {
            let unpadded = varint(index, &mut at)?;
            let uncompressed = varint(index, &mut at)?;
            blocks = blocks.checked_add(unpadded.div_ceil(4) * 4)?;
            total = total.checked_add(uncompressed)?;
        }
        // Before the blocks: the 12-byte stream header.
        end = (index_start as u64).checked_sub(blocks)?.checked_sub(12)? as usize;
        streams += 1;
    }
    (streams > 0).then_some(total)
}

/// A write sink that refuses past `limit` (the xz stream, which
/// lzma-rs decodes whole).
struct Capped {
    buf: Vec<u8>,
    limit: u64,
    over: bool,
}

impl Write for Capped {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if (self.buf.len() as u64).saturating_add(data.len() as u64) > self.limit {
            self.over = true;
            return Err(std::io::Error::other(
                "decompressed stream exceeds the bound",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An archive file, exposed as an arbor.
pub struct ArchiveAdapter {
    nodes: Vec<Node>,
    /// Lazily-filled entry contents (zip); prefilled for tar.
    contents: RefCell<Vec<Option<String>>>,
    /// Entry index → name (decoded under the mount's rule).
    zip_names: Vec<String>,
    /// Entry index → position in the zip's central directory.
    zip_indices: Vec<usize>,
    source: Source,
    /// The mount's rule at the source boundary; every member read
    /// as text decodes under it, and the member answers
    /// `::::encoding` / `::::replaced`.
    decoding: SourceDecoding,
    /// Per entry: the decoding record, once read.
    records: RefCell<Vec<Option<(String, usize)>>>,
}

impl ArchiveAdapter {
    /// Open a `.zip`-family or `.tar[.gz]` archive (format by
    /// magic bytes: `PK`, gzip, else tar by extension).
    pub fn open(path: &Path) -> Result<Self, ArchiveError> {
        Self::open_with_decoding(path, SourceDecoding::default())
    }

    /// [`open`](Self::open) under a mount's source-boundary rule
    /// (`?encoding=` and the host's declaration reporter).
    pub fn open_with_decoding(path: &Path, decoding: SourceDecoding) -> Result<Self, ArchiveError> {
        Self::open_bounded(path, decoding, MAX_ARCHIVE_BYTES)
    }

    /// [`open_with_decoding`](Self::open_with_decoding) with the
    /// bound on what a tar or 7z archive may decompress into
    /// memory ([`MAX_ARCHIVE_BYTES`] by default): past it the open
    /// is refused, naming the member it stopped at.
    pub fn open_bounded(
        path: &Path,
        decoding: SourceDecoding,
        limit: u64,
    ) -> Result<Self, ArchiveError> {
        let mut f = std::fs::File::open(path)?;
        let mut magic = [0u8; 2];
        let n = f.read(&mut magic)?;
        drop(f);
        if n == 2 && magic == *b"PK" {
            return Self::open_zip(path, decoding);
        }
        if n == 2 && magic == [0x1f, 0x8b] {
            return Self::open_tar(path, Compression::Gzip, decoding, limit);
        }
        // The longer magics: xz, bzip2, zstd, 7z.
        let mut head = [0u8; 6];
        let mut f = std::fs::File::open(path)?;
        let n = f.read(&mut head)?;
        drop(f);
        let head = &head[..n];
        if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
            return Self::open_tar(path, Compression::Xz, decoding, limit);
        }
        if head.starts_with(b"BZh") {
            return Self::open_tar(path, Compression::Bzip2, decoding, limit);
        }
        if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            return Self::open_tar(path, Compression::Zstd, decoding, limit);
        }
        if head.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
            return Self::open_7z(path, decoding, limit);
        }
        if head.starts_with(b"ITSF") {
            return Self::open_chm(path, decoding);
        }
        if head.starts_with(b"Rar!\x1a") {
            return Self::open_rar(path, decoding);
        }
        if path.extension().and_then(|e| e.to_str()) == Some("tar") {
            return Self::open_tar(path, Compression::None, decoding, limit);
        }
        // A MIME envelope: by its extension, or by the header
        // lines a browser's web archive opens with.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("mhtml" | "mht")) || looks_like_mime(path)? {
            return Self::open_mhtml(path, decoding);
        }
        Err(ArchiveError::Format)
    }

    /// An MHTML web archive: the parts of the `multipart/related`
    /// envelope as members, each named by its `Content-Location`
    /// (scheme dropped, so `https://example.org/a/b.html` is the
    /// member `example.org/a/b.html`; a text/html part with no
    /// extension gets `.html`), else its `Content-ID`, else its
    /// ordinal. A part's charset parameter is its declaration at
    /// the source boundary; its transfer encoding (base64,
    /// quoted-printable, 7bit/8bit/binary) is undone first. A
    /// single-part file is one member.
    fn open_mhtml(path: &Path, decoding: SourceDecoding) -> Result<Self, ArchiveError> {
        let bytes = std::fs::read(path)?;
        let parts = mime::parts(&bytes);
        if parts.is_empty() {
            return Err(ArchiveError::Format);
        }
        let mut b = TreeBuilder::new();
        let mut contents = Vec::new();
        let mut records = Vec::new();
        for (k, part) in parts.into_iter().enumerate() {
            let name = part.member_name(k);
            let idx = contents.len();
            let size = part.body.len() as i64;
            let charset = part.charset.clone();
            let declared = decoding.declared;
            let decoded = quarb::source::decode(&part.body, decoding.explicit.as_deref(), |bb| {
                charset
                    .clone()
                    .or_else(|| declared.and_then(|f| f(&name, bb)))
            });
            match decoded {
                Ok(d) => {
                    contents.push(Some(d.text));
                    records.push(Some((d.encoding, d.replaced)));
                }
                Err(_) => {
                    contents.push(None);
                    records.push(None);
                }
            }
            b.file(&name, idx, Some(size), None);
        }
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(contents),
            zip_names: Vec::new(),
            zip_indices: Vec::new(),
            source: Source::Tar,
            decoding,
            records: RefCell::new(records),
        })
    }

    /// A CHM help file: every normal entry (not the `#`/`$` system
    /// records, not the internal metadata) as a member under its
    /// path, read up front through `libchm`. A page's own
    /// declaration (`<meta charset>`, the host's reporter) wins;
    /// a page without one decodes under the code page of the
    /// `#SYSTEM` record's locale, the format's own declaration of
    /// its text's encoding.
    fn open_chm(path: &Path, decoding: SourceDecoding) -> Result<Self, ArchiveError> {
        let mut chm = libchm::ChmFile::open(path).map_err(|e| ArchiveError::Chm(e.to_string()))?;
        let locale_label = chm
            .read_path("/#SYSTEM")
            .ok()
            .and_then(|b| chm_system_lcid(&b))
            .map(code_page_for_lcid);
        let entries = chm
            .entries(libchm::EntrySel::NORMAL | libchm::EntrySel::FILES | libchm::EntrySel::DIRS)
            .map_err(|e| ArchiveError::Chm(e.to_string()))?;
        let mut b = TreeBuilder::new();
        let mut contents = Vec::new();
        let mut records = Vec::new();
        for entry in entries {
            let name = entry.path.trim_start_matches('/').to_string();
            if entry.kind == libchm::EntryKind::Dir {
                b.dir(&name);
                continue;
            }
            let buf = chm
                .read(&entry)
                .map_err(|e| ArchiveError::Chm(e.to_string()))?;
            let idx = contents.len();
            let declared = decoding.declared;
            let fallback = locale_label.clone();
            let decoded = quarb::source::decode(&buf, decoding.explicit.as_deref(), |bb| {
                declared
                    .and_then(|f| f(&name, bb))
                    .or_else(|| fallback.clone())
            });
            match decoded {
                Ok(d) => {
                    contents.push(Some(d.text));
                    records.push(Some((d.encoding, d.replaced)));
                }
                Err(_) => {
                    contents.push(None);
                    records.push(None);
                }
            }
            b.file(&name, idx, Some(entry.length as i64), None);
        }
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(contents),
            zip_names: Vec::new(),
            zip_indices: Vec::new(),
            source: Source::Tar,
            decoding,
            records: RefCell::new(records),
        })
    }

    /// A RAR archive through the `unrar` library: every file entry
    /// read up front (the format is solid as often as not), under
    /// the mount's rule like tar's.
    #[cfg(feature = "rar")]
    fn open_rar(path: &Path, decoding: SourceDecoding) -> Result<Self, ArchiveError> {
        let mut archive = unrar::Archive::new(path)
            .open_for_processing()
            .map_err(|e| ArchiveError::Rar(e.to_string()))?;
        let mut b = TreeBuilder::new();
        let mut contents = Vec::new();
        let mut records = Vec::new();
        loop {
            let Some(at_file) = archive
                .read_header()
                .map_err(|e| ArchiveError::Rar(e.to_string()))?
            else {
                break;
            };
            let header = at_file.entry();
            let name = header.filename.to_string_lossy().replace('\\', "/");
            if header.is_directory() {
                b.dir(&name);
                archive = at_file
                    .skip()
                    .map_err(|e| ArchiveError::Rar(e.to_string()))?;
                continue;
            }
            let size = header.unpacked_size as i64;
            let (buf, next) = at_file
                .read()
                .map_err(|e| ArchiveError::Rar(e.to_string()))?;
            archive = next;
            let idx = contents.len();
            match decoding.decode(&name, &buf) {
                Ok(d) => {
                    contents.push(Some(d.text));
                    records.push(Some((d.encoding, d.replaced)));
                }
                Err(_) => {
                    contents.push(None);
                    records.push(None);
                }
            }
            b.file(&name, idx, Some(size), None);
        }
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(contents),
            zip_names: Vec::new(),
            zip_indices: Vec::new(),
            source: Source::Tar,
            decoding,
            records: RefCell::new(records),
        })
    }

    #[cfg(not(feature = "rar"))]
    fn open_rar(_path: &Path, _decoding: SourceDecoding) -> Result<Self, ArchiveError> {
        Err(ArchiveError::RarUnavailable)
    }

    /// A 7z archive in the browser build: refused, the reader is
    /// native-only.
    #[cfg(target_arch = "wasm32")]
    fn open_7z(_path: &Path, _decoding: SourceDecoding, _limit: u64) -> Result<Self, ArchiveError> {
        Err(ArchiveError::SevenZ(
            "the 7z reader is not part of the browser build".into(),
        ))
    }

    /// A 7z archive: its entries read up front (the format is
    /// solid by default, so members come in one stream), under the
    /// mount's rule like tar's.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_7z(path: &Path, decoding: SourceDecoding, limit: u64) -> Result<Self, ArchiveError> {
        let mut reader = sevenz_rust::SevenZReader::open(path, sevenz_rust::Password::empty())
            .map_err(|e| ArchiveError::SevenZ(e.to_string()))?;
        let mut b = TreeBuilder::new();
        let mut contents = Vec::new();
        let mut records = Vec::new();
        let mut total: u64 = 0;
        let mut too_large: Option<String> = None;
        reader
            .for_each_entries(|entry, r| {
                let name = entry.name().replace('\\', "/");
                if entry.is_directory() {
                    b.dir(&name);
                    return Ok(true);
                }
                total = total.saturating_add(entry.size());
                if total > limit {
                    too_large = Some(name);
                    return Ok(false);
                }
                let mut buf = Vec::new();
                r.read_to_end(&mut buf)?;
                let idx = contents.len();
                let size = buf.len() as i64;
                match decoding.decode(&name, &buf) {
                    Ok(d) => {
                        contents.push(Some(d.text));
                        records.push(Some((d.encoding, d.replaced)));
                    }
                    Err(_) => {
                        contents.push(None);
                        records.push(None);
                    }
                }
                b.file(&name, idx, Some(size), None);
                Ok(true)
            })
            .map_err(|e| ArchiveError::SevenZ(e.to_string()))?;
        if let Some(name) = too_large {
            return Err(ArchiveError::TooLarge { name, limit });
        }
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(contents),
            zip_names: Vec::new(),
            zip_indices: Vec::new(),
            source: Source::Tar,
            decoding,
            records: RefCell::new(records),
        })
    }

    fn open_zip(path: &Path, decoding: SourceDecoding) -> Result<Self, ArchiveError> {
        let f = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(f)?;
        let mut b = TreeBuilder::new();
        let mut zip_names = Vec::new();
        let mut zip_indices = Vec::new();
        for i in 0..zip.len() {
            let e = zip.by_index_raw(i)?;
            // An entry name is bytes: the mount's rule decodes it
            // (`?encoding=` for an archive written under a legacy
            // code page, else strict UTF-8), never the CP437 guess
            // the format's own flag would leave for an unflagged
            // name.
            let name = decoding
                .decode("", e.name_raw())
                .map(|d| d.text)
                .map_err(ArchiveError::Name)?;
            if e.is_dir() {
                b.dir(&name);
                continue;
            }
            let idx = zip_names.len();
            zip_names.push(name.clone());
            zip_indices.push(i);
            b.file(
                &name,
                idx,
                Some(e.size() as i64),
                Some(e.compressed_size() as i64),
            );
        }
        let n = zip_names.len();
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(vec![None; n]),
            zip_names,
            zip_indices,
            source: Source::Zip(RefCell::new(zip)),
            decoding,
            records: RefCell::new(vec![None; n]),
        })
    }

    /// A tar or gzipped tar held in memory — the form a browser
    /// has after one fetch. Zip needs a seekable file and stays
    /// with [`open`](Self::open).
    pub fn from_tar_bytes(bytes: &[u8]) -> Result<Self, ArchiveError> {
        Self::from_tar_bytes_with_decoding(bytes, SourceDecoding::default())
    }

    /// [`from_tar_bytes`](Self::from_tar_bytes) under a mount's
    /// source-boundary rule.
    pub fn from_tar_bytes_with_decoding(
        bytes: &[u8],
        decoding: SourceDecoding,
    ) -> Result<Self, ArchiveError> {
        Self::read_tar(
            std::io::Cursor::new(bytes),
            Compression::of(bytes),
            decoding,
            MAX_ARCHIVE_BYTES,
        )
    }

    fn open_tar(
        path: &Path,
        compression: Compression,
        decoding: SourceDecoding,
        limit: u64,
    ) -> Result<Self, ArchiveError> {
        Self::read_tar(std::fs::File::open(path)?, compression, decoding, limit)
    }

    fn read_tar<'a, R: Read + 'a>(
        f: R,
        compression: Compression,
        decoding: SourceDecoding,
        limit: u64,
    ) -> Result<Self, ArchiveError> {
        let reader: Box<dyn Read + 'a> = match compression {
            Compression::None => Box::new(f),
            Compression::Gzip => Box::new(flate2::read::GzDecoder::new(f)),
            Compression::Bzip2 => Box::new(bzip2::read::BzDecoder::new(f)),
            Compression::Zstd => Box::new(
                ruzstd::decoding::StreamingDecoder::new(f)
                    .map_err(|e| std::io::Error::other(e.to_string()))?,
            ),
            // lzma-rs decodes a whole stream at once and holds it
            // in memory before writing a byte, so the bound is
            // checked first against what the stream declares: the
            // xz index at the end of the file records every
            // block's uncompressed size. A stream declaring more
            // than the bound (or whose compressed form alone
            // exceeds it) is refused before any decoding; the sink
            // still stops at the bound should a stream lie.
            Compression::Xz => {
                let mut compressed = Vec::new();
                f.take(limit + 1).read_to_end(&mut compressed)?;
                let declared = xz_uncompressed_size(&compressed);
                if compressed.len() as u64 > limit || declared.is_none_or(|d| d > limit) {
                    return Err(ArchiveError::TooLarge {
                        name: "(xz stream)".into(),
                        limit,
                    });
                }
                let mut input = std::io::Cursor::new(compressed);
                let mut out = Capped {
                    buf: Vec::new(),
                    limit,
                    over: false,
                };
                match lzma_rs::xz_decompress(&mut input, &mut out) {
                    Ok(()) => {}
                    Err(_) if out.over => {
                        return Err(ArchiveError::TooLarge {
                            name: "(xz stream)".into(),
                            limit,
                        });
                    }
                    Err(e) => return Err(std::io::Error::other(e.to_string()).into()),
                }
                Box::new(std::io::Cursor::new(out.buf))
            }
        };
        let mut ar = tar::Archive::new(reader);
        let mut b = TreeBuilder::new();
        let mut contents = Vec::new();
        let mut records = Vec::new();
        let mut total: u64 = 0;
        for entry in ar.entries()? {
            let mut e = entry?;
            // A tar name is bytes: the mount's rule decodes it
            // (`?encoding=`, else strict UTF-8), no guessing.
            let name = decoding
                .decode("", &e.path_bytes())
                .map(|d| d.text)
                .map_err(ArchiveError::Name)?;
            if e.header().entry_type().is_dir() {
                b.dir(&name);
                continue;
            }
            if !e.header().entry_type().is_file() {
                continue;
            }
            total = total.saturating_add(e.header().size().unwrap_or(0));
            if total > limit {
                return Err(ArchiveError::TooLarge { name, limit });
            }
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)?;
            let idx = contents.len();
            let size = buf.len() as i64;
            // A member that does not decode under the rule has no
            // text (an image, a binary): the leaf stays a leaf.
            match decoding.decode(&name, &buf) {
                Ok(d) => {
                    contents.push(Some(d.text));
                    records.push(Some((d.encoding, d.replaced)));
                }
                Err(_) => {
                    contents.push(None);
                    records.push(None);
                }
            }
            b.file(&name, idx, Some(size), None);
        }
        Ok(ArchiveAdapter {
            nodes: b.nodes,
            contents: RefCell::new(contents),
            zip_names: Vec::new(),
            zip_indices: Vec::new(),
            source: Source::Tar,
            decoding,
            records: RefCell::new(records),
        })
    }

    /// A human-readable locator: the entry path.
    pub fn locator(&self, node: NodeId) -> String {
        let mut parts = Vec::new();
        let mut cur = Some(node);
        while let Some(n) = cur {
            let nd = &self.nodes[n.0 as usize];
            if !nd.name.is_empty() {
                parts.push(nd.name.clone());
            }
            cur = nd.parent;
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }

    /// Every file entry as `(path, text)`, in archive order —
    /// the form a store over the archive builds from.
    pub fn files(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let Some(entry) = n.entry else { continue };
            if let Some(text) = self.content(entry) {
                out.push((
                    self.locator(NodeId(i as u64))
                        .trim_start_matches('/')
                        .to_string(),
                    text,
                ));
            }
        }
        out
    }

    /// The file entries whose bytes did not decode under the mount's
    /// rule (binary members, or text in an undeclared encoding), by
    /// path — for a store that must not drop them silently.
    pub fn undecoded(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let Some(entry) = n.entry else { continue };
            if self.content(entry).is_none() {
                out.push(
                    self.locator(NodeId(i as u64))
                        .trim_start_matches('/')
                        .to_string(),
                );
            }
        }
        out
    }

    /// An entry's text content, reading it on first touch (zip).
    fn content(&self, entry: usize) -> Option<String> {
        if let Some(c) = &self.contents.borrow()[entry] {
            return Some(c.clone());
        }
        let Source::Zip(zip) = &self.source else {
            return None;
        };
        let mut zip = zip.borrow_mut();
        let mut e = zip.by_index(self.zip_indices[entry]).ok()?;
        // A member past the bound is refused, not read: the query
        // errors instead of the process filling memory.
        if e.size() > MAX_ARCHIVE_BYTES {
            quarb::record_refusal(format!(
                "archive: {}: {} bytes decompressed exceeds the {MAX_ARCHIVE_BYTES}-byte bound on a member read into memory",
                self.zip_names[entry],
                e.size()
            ));
            return None;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).ok()?;
        drop(e);
        drop(zip);
        let d = self.decoding.decode(&self.zip_names[entry], &buf).ok()?;
        self.records.borrow_mut()[entry] = Some((d.encoding, d.replaced));
        self.contents.borrow_mut()[entry] = Some(d.text.clone());
        Some(d.text)
    }
}

/// Builds the path tree from entry names.
struct TreeBuilder {
    nodes: Vec<Node>,
    by_path: HashMap<String, usize>,
}

/// Normalize an entry path: split on `/`, dropping empty and `.`
/// segments and collapsing `..`. GNU tar (`tar -c -C dir .`)
/// stores `./`-prefixed names; without this a spurious `.` node
/// is interned and everything lands beneath it, breaking
/// absolute-path navigation that works for the equivalent zip.
/// The locale id in a CHM's `#SYSTEM` record: a version word,
/// then records of `(code u16, length u16, data)`; code 4 holds
/// the LCID in its first four bytes.
fn chm_system_lcid(bytes: &[u8]) -> Option<u32> {
    let mut i = 4;
    while i + 4 <= bytes.len() {
        let code = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let len = u16::from_le_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        let data = bytes.get(i + 4..i + 4 + len)?;
        if code == 4 && data.len() >= 4 {
            return Some(u32::from_le_bytes([data[0], data[1], data[2], data[3]]));
        }
        i += 4 + len;
    }
    None
}

/// The Windows code page of a locale id's text, as the Encoding
/// Standard's label.
fn code_page_for_lcid(lcid: u32) -> String {
    let primary = lcid & 0x3FF;
    match primary {
        0x19 | 0x02 | 0x22 | 0x23 | 0x1A | 0x2F | 0x43 | 0x40 | 0x3F | 0x50 | 0x28 | 0x44
        | 0x46 => "windows-1251",
        0x05 | 0x0E | 0x15 | 0x1B | 0x24 | 0x18 | 0x1C => "windows-1250",
        0x08 => "windows-1253",
        0x1F => "windows-1254",
        0x0D => "windows-1255",
        0x01 | 0x20 | 0x29 => "windows-1256",
        0x25..=0x27 => "windows-1257",
        0x2A => "windows-1258",
        0x1E => "windows-874",
        0x11 => "shift_jis",
        0x04 if lcid == 0x0804 || lcid == 0x1004 => "gbk",
        0x04 => "big5",
        0x12 => "euc-kr",
        _ => "windows-1252",
    }
    .to_string()
}

/// Whether the file opens with MIME header lines — a browser's
/// web archive begins `From: <Saved by Blink>` or
/// `MIME-Version:`, a mail-shaped one with any header.
fn looks_like_mime(path: &Path) -> std::io::Result<bool> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 512];
    let n = f.read(&mut head)?;
    let head = String::from_utf8_lossy(&head[..n]);
    let mut lines = head.lines().take(8).peekable();
    let Some(first) = lines.peek() else {
        return Ok(false);
    };
    let is_header = |l: &str| {
        l.split_once(':').is_some_and(|(k, _)| {
            !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    };
    if !is_header(first) && !first.starts_with("From ") {
        return Ok(false);
    }
    Ok(lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("mime-version:") || l.starts_with("content-type:")
    }))
}

/// The MIME envelope: headers, boundaries, transfer encodings.
/// Enough of RFC 2045/2046/2557 for a web archive; nothing is
/// guessed.
mod mime {
    /// One part: its decoded body and the headers that name it.
    pub struct Part {
        pub body: Vec<u8>,
        pub location: Option<String>,
        pub id: Option<String>,
        pub content_type: Option<String>,
        pub charset: Option<String>,
    }

    impl Part {
        /// The member path: the location's path (scheme and query
        /// dropped), a content id, or `part-N`; an HTML part
        /// without an extension gets `.html`.
        pub fn member_name(&self, k: usize) -> String {
            let (mut name, from_location) = match (&self.location, &self.id) {
                (Some(loc), _) if !loc.is_empty() => (location_path(loc), true),
                (_, Some(id)) if !id.is_empty() => (id.trim_matches(['<', '>']).to_string(), false),
                _ => (format!("part-{}", k + 1), false),
            };
            if name.is_empty() || name.ends_with('/') {
                name.push_str("index");
            }
            // A location's basename has its own extension or none;
            // a content id is no path, and gets the type's.
            let base = name.rsplit('/').next().unwrap_or("");
            if !from_location || !base.contains('.') {
                let ext = self
                    .content_type
                    .as_deref()
                    .map(|t| t.to_ascii_lowercase())
                    .and_then(|t| match t.as_str() {
                        "text/html" | "application/xhtml+xml" => Some("html"),
                        "text/css" => Some("css"),
                        "text/javascript" | "application/javascript" => Some("js"),
                        "text/plain" => Some("txt"),
                        "image/png" => Some("png"),
                        "image/jpeg" => Some("jpg"),
                        "image/gif" => Some("gif"),
                        "image/svg+xml" => Some("svg"),
                        "application/json" => Some("json"),
                        "text/xml" | "application/xml" => Some("xml"),
                        _ => None,
                    });
                if let Some(ext) = ext {
                    name.push('.');
                    name.push_str(ext);
                }
            }
            name
        }
    }

    /// `https://example.org/a/b.html?x#y` → `example.org/a/b.html`;
    /// `cid:…` and `file:///` likewise lose their scheme.
    fn location_path(loc: &str) -> String {
        let loc = loc.split(['?', '#']).next().unwrap_or(loc);
        let rest = match loc.split_once("://") {
            Some((_, r)) => r,
            None => loc.split_once(':').map_or(loc, |(_, r)| r),
        };
        rest.trim_start_matches('/').to_string()
    }

    /// Split the envelope into its parts, recursively through
    /// nested multiparts; a single-part message is one part.
    pub fn parts(bytes: &[u8]) -> Vec<Part> {
        let mut out = Vec::new();
        collect(bytes, &mut out, 0);
        out
    }

    fn collect(bytes: &[u8], out: &mut Vec<Part>, depth: usize) {
        let Some((headers, body)) = split_headers(bytes) else {
            return;
        };
        let ctype = header(&headers, "content-type");
        let boundary = ctype.as_deref().and_then(|c| param(c, "boundary"));
        if let Some(b) = boundary
            && depth < 8
        {
            let delim = format!("--{b}");
            let mut sections = split_by_boundary(body, delim.as_bytes());
            // The preamble before the first boundary is not a part.
            if !sections.is_empty() {
                sections.remove(0);
            }
            for sec in sections {
                collect(sec, out, depth + 1);
            }
            return;
        }
        let cte = header(&headers, "content-transfer-encoding")
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let decoded = match cte.as_str() {
            "base64" => quarb::base64_decode(&String::from_utf8_lossy(body)).unwrap_or_default(),
            "quoted-printable" => quoted_printable(body),
            _ => body.to_vec(),
        };
        let mime_type = ctype.as_deref().map(|c| {
            c.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
        out.push(Part {
            body: decoded,
            location: header(&headers, "content-location").map(|v| v.trim().to_string()),
            id: header(&headers, "content-id").map(|v| v.trim().to_string()),
            charset: ctype.as_deref().and_then(|c| param(c, "charset")),
            content_type: mime_type,
        });
    }

    type Headers = Vec<(String, String)>;

    /// Headers (unfolded, `name: value` per line) and the body
    /// after the first blank line.
    fn split_headers(bytes: &[u8]) -> Option<(Headers, &[u8])> {
        let mut i = 0;
        // A leading blank line or two is tolerated.
        while bytes.get(i) == Some(&b'\r') || bytes.get(i) == Some(&b'\n') {
            i += 1;
        }
        let text_end = find_blank_line(&bytes[i..]).map(|e| i + e)?;
        let head = String::from_utf8_lossy(&bytes[i..text_end]);
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in head.lines() {
            if (line.starts_with(' ') || line.starts_with('\t'))
                && let Some(last) = headers.last_mut()
            {
                last.1.push(' ');
                last.1.push_str(line.trim());
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        // Past the blank line: the line end that closed the headers,
        // then the empty line's own.
        let mut body_start = text_end;
        for _ in 0..2 {
            if bytes.get(body_start) == Some(&b'\r') {
                body_start += 1;
            }
            if bytes.get(body_start) == Some(&b'\n') {
                body_start += 1;
            }
        }
        Some((headers, &bytes[body_start.min(bytes.len())..]))
    }

    fn find_blank_line(bytes: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\n' {
                if bytes.get(i + 1) == Some(&b'\n') {
                    return Some(i);
                }
                if bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    fn header(headers: &[(String, String)], name: &str) -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// A parameter of a `Content-Type` value, unquoted.
    fn param(value: &str, name: &str) -> Option<String> {
        value.split(';').skip(1).find_map(|p| {
            let (k, v) = p.split_once('=')?;
            (k.trim().eq_ignore_ascii_case(name)).then(|| v.trim().trim_matches('"').to_string())
        })
    }

    /// The sections between boundary lines; the closing
    /// `--boundary--` ends the last one.
    fn split_by_boundary<'a>(body: &'a [u8], delim: &[u8]) -> Vec<&'a [u8]> {
        let mut sections = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i + delim.len() <= body.len() {
            let at_line_start = i == 0 || body[i - 1] == b'\n';
            if at_line_start && body[i..].starts_with(delim) {
                let mut end = i;
                // The CRLF before a boundary belongs to it.
                if end > start && body[end - 1] == b'\n' {
                    end -= 1;
                    if end > start && body[end - 1] == b'\r' {
                        end -= 1;
                    }
                }
                sections.push(&body[start..end]);
                let after = i + delim.len();
                let closing = body.get(after..after + 2) == Some(b"--");
                // Skip to the end of the boundary line.
                let mut j = after;
                while j < body.len() && body[j] != b'\n' {
                    j += 1;
                }
                j = (j + 1).min(body.len());
                if closing {
                    return sections;
                }
                start = j;
                i = j;
                continue;
            }
            i += 1;
        }
        sections.push(&body[start..]);
        sections
    }

    /// RFC 2045 quoted-printable: `=XX` a byte, `=\r\n` a soft
    /// break, trailing spaces before a break dropped.
    fn quoted_printable(body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            let c = body[i];
            if c == b'=' {
                if body.get(i + 1) == Some(&b'\r') && body.get(i + 2) == Some(&b'\n') {
                    i += 3;
                    continue;
                }
                if body.get(i + 1) == Some(&b'\n') {
                    i += 2;
                    continue;
                }
                if let Some(hex) = body.get(i + 1..i + 3)
                    && let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(hex), 16)
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            if c == b'\n' || c == b'\r' {
                // Trailing whitespace before a line end is padding.
                while out.last().is_some_and(|b| *b == b' ' || *b == b'\t') {
                    out.pop();
                }
            }
            out.push(c);
            i += 1;
        }
        out
    }
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
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

impl TreeBuilder {
    fn new() -> Self {
        TreeBuilder {
            nodes: vec![Node {
                name: String::new(),
                parent: None,
                children: Vec::new(),
                entry: None,
                size: None,
                compressed: None,
            }],
            by_path: HashMap::new(),
        }
    }

    /// The node for a directory path, creating the chain.
    fn dir(&mut self, path: &str) -> usize {
        let path = normalize(path);
        if path.is_empty() {
            return 0;
        }
        if let Some(&i) = self.by_path.get(&path) {
            return i;
        }
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (self.dir(p), n.to_string()),
            None => (0, path.clone()),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            name,
            parent: Some(NodeId(parent as u64)),
            children: Vec::new(),
            entry: None,
            size: None,
            compressed: None,
        });
        self.nodes[parent].children.push(NodeId(id as u64));
        self.by_path.insert(path, id);
        id
    }

    fn file(&mut self, path: &str, entry: usize, size: Option<i64>, compressed: Option<i64>) {
        let path = normalize(path);
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (self.dir(p), n.to_string()),
            None => (0, path.clone()),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            name,
            parent: Some(NodeId(parent as u64)),
            children: Vec::new(),
            entry: Some(entry),
            size,
            compressed,
        });
        self.nodes[parent].children.push(NodeId(id as u64));
        self.by_path.insert(path, id);
    }
}

impl AstAdapter for ArchiveAdapter {
    /// The member table knows how many members a step reaches and
    /// their uncompressed sizes; nothing is decoded.
    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let bytes_of = |n: NodeId| self.nodes.get(n.0 as usize)?.size.map(|b| b.max(0) as u64);
        quarb::adapter::arena_estimate(self, scope, plan, quarb::Latency::Memory, Some(&bytes_of))
    }

    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node.0 as usize].children.clone()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let n = &self.nodes[node.0 as usize].name;
        (!n.is_empty()).then(|| n.clone())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node.0 as usize].parent
    }

    /// `<file>` or `<dir>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        if node.0 == 0 {
            return Vec::new();
        }
        let t = if self.nodes[node.0 as usize].entry.is_some() {
            "file"
        } else {
            "dir"
        };
        vec![t.to_string()]
    }

    /// A file entry's content, text.
    fn default_value(&self, node: NodeId) -> Option<Value> {
        let entry = self.nodes[node.0 as usize].entry?;
        self.content(entry).map(Value::Str)
    }

    /// `;;;size`, `;;;compressed` (zip), `;;;n-entries` (root).
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let n = &self.nodes[node.0 as usize];
        if matches!(key, "encoding" | "replaced")
            && let Some(entry) = n.entry
        {
            // The decoding record for a member read as text
            // (reading it if it has not been).
            if self.records.borrow()[entry].is_none() {
                self.content(entry)?;
            }
            let records = self.records.borrow();
            let (enc, replaced) = records[entry].as_ref()?;
            return Some(match key {
                "encoding" => Value::Str(enc.clone()),
                _ => Value::Int(*replaced as i64),
            });
        }
        match key {
            "size" => n.size.map(Value::bytes),
            "compressed" => n.compressed.map(Value::Int),
            "n-entries" if node.0 == 0 => Some(Value::Int(self.nodes.len() as i64 - 1)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_dot_and_dotdot_segments() {
        assert_eq!(normalize("./"), "");
        assert_eq!(normalize("./a.txt"), "a.txt");
        assert_eq!(normalize("./word/document.xml"), "word/document.xml");
        assert_eq!(normalize("word/document.xml"), "word/document.xml");
        assert_eq!(normalize("a/./b"), "a/b");
        assert_eq!(normalize("a/../b"), "b");
        assert_eq!(normalize("../x"), "x");
    }

    #[test]
    fn dot_prefixed_tar_entries_land_under_root() {
        // GNU tar (`tar -c -C dir .`) stores `./` and `./a.txt`;
        // no spurious `.` node must be interned, and the file must
        // hang directly off the root as it does for the equal zip.
        let mut b = TreeBuilder::new();
        b.dir("./");
        b.file("./a.txt", 0, Some(3), None);
        let root = &b.nodes[0];
        assert_eq!(root.children.len(), 1);
        let child = &b.nodes[root.children[0].0 as usize];
        assert_eq!(child.name, "a.txt");
        assert_eq!(child.parent, Some(NodeId(0)));
        assert!(b.nodes.iter().all(|n| n.name != "."));
    }
}

#[cfg(test)]
mod xz_tests {
    use super::xz_uncompressed_size;

    #[test]
    fn the_xz_index_declares_the_uncompressed_size() {
        let plain: Vec<u8> = b"Sing, goddess, the wrath. ".repeat(2000);
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(&plain), &mut xz).unwrap();
        assert_eq!(xz_uncompressed_size(&xz), Some(plain.len() as u64));
        assert_eq!(xz_uncompressed_size(b"not xz at all"), None);
        // Two concatenated streams add up.
        let mut twice = xz.clone();
        twice.extend_from_slice(&xz);
        assert_eq!(xz_uncompressed_size(&twice), Some(2 * plain.len() as u64));
    }
}
