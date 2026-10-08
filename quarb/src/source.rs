//! The source boundary: where a source's bytes become the text an
//! adapter reads. Text is text inside the language; bytes meet it
//! here and at the output boundary (`@| encoding(LABEL)`, which the
//! tools apply to what they print), nowhere else. An adapter decides
//! nothing about encodings — where its format declares one (an XML
//! prologue, an HTML meta charset, a MIME charset, a zip entry's
//! flag) the adapter *reports* the declaration and this one step
//! applies it.
//!
//! The order is fixed and nothing is guessed from bytes:
//!
//! 1. `?encoding=LABEL` on the mount;
//! 2. a byte-order mark;
//! 3. the declaration the adapter reports;
//! 4. UTF-8 — and a source that reaches this step and is not valid
//!    UTF-8 is refused, naming the option.
//!
//! Within a decided encoding a malformed sequence becomes U+FFFD and
//! is counted; the source's root answers `::::encoding` (the label as
//! decided) and `::::replaced` (the count). Labels are the WHATWG
//! Encoding Standard's, case-insensitive; an unknown label is refused
//! when the mount opens.

use crate::adapter::{AstAdapter, NodeId};
use crate::value::Value;
use encoding_rs::{DecoderResult, Encoding, UTF_8};

/// What decided the encoding, in the order the boundary consults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecidedBy {
    /// `?encoding=LABEL` on the mount.
    Option,
    /// A byte-order mark.
    Bom,
    /// The declaration the adapter reported.
    Declaration,
    /// Nothing said: UTF-8, strictly.
    Default,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Option => "option",
            DecidedBy::Bom => "bom",
            DecidedBy::Declaration => "declaration",
            DecidedBy::Default => "default",
        }
    }
}

/// A source's text with the record of its decoding.
#[derive(Clone, Debug)]
pub struct Decoded {
    pub text: String,
    /// The label as decided — the Encoding Standard's name
    /// (`UTF-8`, `windows-1251`, `KOI8-R`).
    pub encoding: String,
    /// Malformed sequences replaced by U+FFFD.
    pub replaced: usize,
    pub decided_by: DecidedBy,
}

/// How the metadata on a source's root reads: the pair every
/// decoding layer answers for the root it produced.
impl Decoded {
    pub fn metadata(&self, key: &str) -> Option<Value> {
        match key {
            "encoding" => Some(Value::Str(self.encoding.clone())),
            "replaced" => Some(Value::Int(self.replaced as i64)),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error(
        "unknown encoding label {0:?} (labels are the Encoding Standard's: utf-8, windows-1251, koi8-r, iso-8859-2, …)"
    )]
    UnknownLabel(String),
    #[error(
        "not valid UTF-8 at byte {offset} (no byte-order mark, no declaration); \
         say its encoding with ?encoding=LABEL"
    )]
    NotUtf8 { offset: usize },
}

/// A declaration reporter: the host's choice of which format's
/// reporter to ask, by the source's name (an extension decides;
/// the html and xml reporters are plain functions over bytes).
pub type Declared = fn(name: &str, bytes: &[u8]) -> Option<String>;

/// What a mount carries to the boundary: its `?encoding=` option
/// and the reporter to consult. One per mount, handed to every
/// layer that turns bytes into text under it (an archive's
/// members, a folder's grafted files, a crawl store's pages).
#[derive(Clone, Default)]
pub struct SourceDecoding {
    pub explicit: Option<String>,
    pub declared: Option<Declared>,
}

impl SourceDecoding {
    /// Validate the option at open: an unknown label is refused
    /// before any byte is read.
    pub fn with_option(mut self, label: Option<&str>) -> Result<Self, SourceError> {
        if let Some(l) = label {
            encoding_for_label(l).ok_or_else(|| SourceError::UnknownLabel(l.into()))?;
            self.explicit = Some(l.to_string());
        }
        Ok(self)
    }

    pub fn with_declared(mut self, declared: Declared) -> Self {
        self.declared = Some(declared);
        self
    }

    /// Decode one source under this mount's rule; `name` picks the
    /// reporter (a file name, an archive entry, a URL path).
    pub fn decode(&self, name: &str, bytes: &[u8]) -> Result<Decoded, SourceError> {
        decode(bytes, self.explicit.as_deref(), |b| {
            self.declared.and_then(|f| f(name, b))
        })
    }
}

/// The encoding a label names, or `None` for a label the Encoding
/// Standard does not know. The CJK encodings ride the `cjk` feature
/// (on for `qua`, off for the playground and the extension, which
/// spare their tables).
pub fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    // The labels the Encoding Standard maps to `replacement`
    // (iso-2022-kr, hz-gb-2312 …) name nothing to read or write.
    lookup_label(label).filter(|e| *e != encoding_rs::REPLACEMENT)
}

fn lookup_label(label: &str) -> Option<&'static Encoding> {
    let label = label.trim();
    #[cfg(feature = "cjk")]
    {
        Encoding::for_label(label.as_bytes())
    }
    #[cfg(not(feature = "cjk"))]
    {
        let enc = Encoding::for_label(label.as_bytes())?;
        // Naming the statics (not `for_label` alone) is what lets the
        // linker leave the CJK tables out of a build without them.
        const WITHOUT_CJK: &[&Encoding] = &[
            &encoding_rs::UTF_8_INIT,
            &encoding_rs::UTF_16LE_INIT,
            &encoding_rs::UTF_16BE_INIT,
            &encoding_rs::IBM866_INIT,
            &encoding_rs::ISO_8859_2_INIT,
            &encoding_rs::ISO_8859_3_INIT,
            &encoding_rs::ISO_8859_4_INIT,
            &encoding_rs::ISO_8859_5_INIT,
            &encoding_rs::ISO_8859_6_INIT,
            &encoding_rs::ISO_8859_7_INIT,
            &encoding_rs::ISO_8859_8_INIT,
            &encoding_rs::ISO_8859_8_I_INIT,
            &encoding_rs::ISO_8859_10_INIT,
            &encoding_rs::ISO_8859_13_INIT,
            &encoding_rs::ISO_8859_14_INIT,
            &encoding_rs::ISO_8859_15_INIT,
            &encoding_rs::ISO_8859_16_INIT,
            &encoding_rs::KOI8_R_INIT,
            &encoding_rs::KOI8_U_INIT,
            &encoding_rs::MACINTOSH_INIT,
            &encoding_rs::WINDOWS_874_INIT,
            &encoding_rs::WINDOWS_1250_INIT,
            &encoding_rs::WINDOWS_1251_INIT,
            &encoding_rs::WINDOWS_1252_INIT,
            &encoding_rs::WINDOWS_1253_INIT,
            &encoding_rs::WINDOWS_1254_INIT,
            &encoding_rs::WINDOWS_1255_INIT,
            &encoding_rs::WINDOWS_1256_INIT,
            &encoding_rs::WINDOWS_1257_INIT,
            &encoding_rs::WINDOWS_1258_INIT,
            &encoding_rs::X_MAC_CYRILLIC_INIT,
            &encoding_rs::X_USER_DEFINED_INIT,
        ];
        WITHOUT_CJK.iter().copied().find(|e| std::ptr::eq(*e, enc))
    }
}

/// Decode a source's bytes in the ruled order. `explicit` is the
/// mount's `?encoding=` label (already validated at open, but checked
/// again here); `declared` is the adapter's declaration reporter —
/// a plain function over the bytes, consulted only when neither the
/// option nor a byte-order mark decided.
pub fn decode(
    bytes: &[u8],
    explicit: Option<&str>,
    declared: impl FnOnce(&[u8]) -> Option<String>,
) -> Result<Decoded, SourceError> {
    if let Some(label) = explicit {
        let enc =
            encoding_for_label(label).ok_or_else(|| SourceError::UnknownLabel(label.into()))?;
        return Ok(decode_with(bytes, enc, DecidedBy::Option));
    }
    if let Some((enc, _)) = Encoding::for_bom(bytes) {
        return Ok(decode_with(bytes, enc, DecidedBy::Bom));
    }
    if let Some(label) = declared(bytes) {
        let enc = encoding_for_label(&label).ok_or(SourceError::UnknownLabel(label))?;
        return Ok(decode_with(bytes, enc, DecidedBy::Declaration));
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(Decoded {
            text: text.to_owned(),
            encoding: UTF_8.name().to_owned(),
            replaced: 0,
            decided_by: DecidedBy::Default,
        }),
        Err(e) => Err(SourceError::NotUtf8 {
            offset: e.valid_up_to(),
        }),
    }
}

/// Decode under a decided encoding: a byte-order mark of that
/// encoding is dropped, every malformed sequence becomes one U+FFFD
/// and is counted (decoding without replacement, resuming after each
/// malformed sequence).
fn decode_with(bytes: &[u8], enc: &'static Encoding, decided_by: DecidedBy) -> Decoded {
    let mut decoder = enc.new_decoder_with_bom_removal();
    let mut text = String::with_capacity(
        decoder
            .max_utf8_buffer_length_without_replacement(bytes.len())
            .unwrap_or(bytes.len()),
    );
    let mut replaced = 0;
    let mut pos = 0;
    loop {
        let (result, read) =
            decoder.decode_to_string_without_replacement(&bytes[pos..], &mut text, true);
        pos += read;
        match result {
            DecoderResult::InputEmpty => break,
            DecoderResult::OutputFull => text.reserve(bytes.len().max(64)),
            DecoderResult::Malformed(_, _) => {
                text.push('\u{FFFD}');
                replaced += 1;
            }
        }
    }
    Decoded {
        text,
        encoding: enc.name().to_owned(),
        replaced,
        decided_by,
    }
}

/// What the output boundary does with a character the encoding
/// cannot hold: refuse the whole output, or write the escape the
/// stage before it provides (`@| json` → `\uXXXX`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Escape {
    #[default]
    Refuse,
    /// JSON's `\u` escape, a surrogate pair beyond the BMP.
    Json,
}

/// Encode text for the output boundary (`@| encoding(LABEL)`): the
/// bytes a tool writes as they are. A character the encoding cannot
/// represent refuses the whole output — no substitution, no entity.
pub fn encode(text: &str, label: &str) -> Result<Vec<u8>, String> {
    encode_with(text, label, Escape::Refuse)
}

/// Decode bytes under a label alone (a MIME encoded-word's charset):
/// malformed sequences replaced, an unknown label `None`.
pub fn decode_label(bytes: &[u8], label: &str) -> Option<String> {
    let enc = encoding_for_label(label)?;
    Some(decode_with(bytes, enc, DecidedBy::Declaration).text)
}

/// [`encode`] with the escape the preceding stage provides for a
/// character the encoding lacks.
pub fn encode_with(text: &str, label: &str, escape: Escape) -> Result<Vec<u8>, String> {
    let enc = encoding_for_label(label)
        .ok_or_else(|| SourceError::UnknownLabel(label.into()).to_string())?;
    if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
        // The Encoding Standard has no UTF-16 encoder; the output
        // boundary does, as the text's code units.
        let mut out = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            if enc == encoding_rs::UTF_16LE {
                out.extend_from_slice(&unit.to_le_bytes());
            } else {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
        return Ok(out);
    }
    let mut encoder = enc.new_encoder();
    let mut out = Vec::with_capacity(
        encoder
            .max_buffer_length_from_utf8_without_replacement(text.len())
            .unwrap_or(text.len()),
    );
    let mut rest = text;
    loop {
        let (result, read) =
            encoder.encode_from_utf8_to_vec_without_replacement(rest, &mut out, true);
        rest = &rest[read..];
        match result {
            encoding_rs::EncoderResult::InputEmpty => return Ok(out),
            encoding_rs::EncoderResult::OutputFull => out.reserve(rest.len().max(64)),
            encoding_rs::EncoderResult::Unmappable(c) => match escape {
                Escape::Json => {
                    let mut units = [0u16; 2];
                    for u in c.encode_utf16(&mut units) {
                        out.extend_from_slice(format!("\\u{u:04x}").as_bytes());
                    }
                }
                Escape::Refuse => {
                    return Err(format!(
                        "{} cannot represent {c:?} (U+{:04X}); the output is refused",
                        enc.name(),
                        c as u32
                    ));
                }
            },
        }
    }
}

/// The adapter over a decoded source: forwards everything to the
/// adapter that read the text, and answers `::::encoding` and
/// `::::replaced` on its root — the decoding layer owns that pair,
/// not the format adapter.
pub struct Sourced<A: AstAdapter> {
    pub inner: A,
    pub encoding: String,
    pub replaced: usize,
}

impl<A: AstAdapter> Sourced<A> {
    pub fn new(inner: A, decoded: &Decoded) -> Self {
        Sourced {
            inner,
            encoding: decoded.encoding.clone(),
            replaced: decoded.replaced,
        }
    }
}

impl<A: AstAdapter> AstAdapter for Sourced<A> {
    crate::forward_ast_adapter!(inner; except metadata);
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node == self.inner.root() {
            match key {
                "encoding" => return Some(Value::Str(self.encoding.clone())),
                "replaced" => return Some(Value::Int(self.replaced as i64)),
                _ => {}
            }
        }
        self.inner.metadata(node, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_by_default_strictly() {
        let d = decode("Илиада".as_bytes(), None, |_| None).unwrap();
        assert_eq!(d.text, "Илиада");
        assert_eq!(d.encoding, "UTF-8");
        assert_eq!(d.decided_by, DecidedBy::Default);
        let e = decode(&[0xC8, 0xEB, 0xE8], None, |_| None).unwrap_err();
        assert!(matches!(e, SourceError::NotUtf8 { offset: 0 }));
        assert!(e.to_string().contains("?encoding=LABEL"));
    }

    #[test]
    fn option_then_bom_then_declaration() {
        // "Или" in windows-1251.
        let cp1251 = [0xC8, 0xEB, 0xE8];
        let d = decode(&cp1251, Some("Windows-1251"), |_| Some("koi8-r".into())).unwrap();
        assert_eq!((d.text.as_str(), d.decided_by), ("Или", DecidedBy::Option));
        assert_eq!(d.encoding, "windows-1251");
        let d = decode(&cp1251, None, |_| Some("windows-1251".into())).unwrap();
        assert_eq!(
            (d.text.as_str(), d.decided_by),
            ("Или", DecidedBy::Declaration)
        );
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("x".as_bytes());
        let d = decode(&bom, None, |_| Some("windows-1251".into())).unwrap();
        assert_eq!((d.text.as_str(), d.decided_by), ("x", DecidedBy::Bom));
        let mut le = vec![0xFF, 0xFE];
        le.extend_from_slice(&[0x18, 0x04, 0x3B, 0x04]);
        let d = decode(&le, None, |_| None).unwrap();
        assert_eq!((d.text.as_str(), d.encoding.as_str()), ("Ил", "UTF-16LE"));
    }

    #[test]
    fn malformed_sequences_are_replaced_and_counted() {
        let bytes = [b'a', 0xFF, b'b', 0xC3, b'c', 0xFF, 0xFE];
        let d = decode(&bytes, Some("utf-8"), |_| None).unwrap();
        assert_eq!(d.text, "a\u{FFFD}b\u{FFFD}c\u{FFFD}\u{FFFD}");
        assert_eq!(d.replaced, 4);
        assert_eq!(d.metadata("replaced"), Some(Value::Int(4)));
        assert_eq!(d.metadata("encoding"), Some(Value::Str("UTF-8".into())));
    }

    #[test]
    fn unknown_labels_are_refused() {
        assert!(matches!(
            decode(b"x", Some("cp-nonesuch"), |_| None),
            Err(SourceError::UnknownLabel(_))
        ));
        assert!(matches!(
            decode(b"x", None, |_| Some("nonesuch".into())),
            Err(SourceError::UnknownLabel(_))
        ));
        assert!(encoding_for_label("latin1").is_some());
        assert!(encoding_for_label(" KOI8-R ").is_some());
    }

    #[test]
    fn output_encoding_refuses_the_unrepresentable() {
        assert_eq!(
            encode("Или", "windows-1251").unwrap(),
            vec![0xC8, 0xEB, 0xE8]
        );
        assert_eq!(
            encode("Ил", "utf-16be").unwrap(),
            vec![0x04, 0x18, 0x04, 0x3B]
        );
        let e = encode("Ил€", "koi8-r").unwrap_err();
        assert!(e.contains("U+20AC"), "{e}");
        assert!(encode("x", "nonesuch").is_err());
    }
}

/// The encoding an XML prologue declares (`<?xml … encoding="…"?>`),
/// for a reporter over XML sources and for the XML members of a
/// package (docx, odt, epub) read under a mount's rule.
pub fn xml_declared_encoding(bytes: &[u8]) -> Option<String> {
    let head = bytes.strip_prefix(b"<?xml")?;
    // `<?xml-stylesheet …?>` is another instruction, not the prologue.
    if !head.first().is_some_and(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let end = head.windows(2).position(|w| w == b"?>")?;
    let prologue = &head[..end];
    let at = prologue.windows(8).position(|w| w == b"encoding")?;
    let rest = &prologue[at + 8..];
    let rest = &rest[rest.iter().position(|b| *b == b'=')? + 1..];
    let rest = &rest[rest.iter().position(|b| !b.is_ascii_whitespace())?..];
    let quote = *rest.first()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let value = &rest[1..];
    let close = value.iter().position(|b| *b == quote)?;
    let label = std::str::from_utf8(&value[..close]).ok()?.trim();
    (!label.is_empty()).then(|| label.to_string())
}

/// How much text a response body may carry: 256 MiB.
pub const MAX_BODY_BYTES: u64 = 256 << 20;

/// A response body as text: read whole under [`MAX_BODY_BYTES`]
/// and decoded as UTF-8 strictly (the wire formats the adapters
/// speak — JSON, NDJSON, XML — are UTF-8 by their standards), so a
/// body is never cut at a client library's own cap nor read with
/// replacement characters. The error names what went wrong.
pub fn text_from_reader(reader: impl std::io::Read) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut bounded = std::io::Read::take(reader, MAX_BODY_BYTES + 1);
    std::io::Read::read_to_end(&mut bounded, &mut buf)
        .map_err(|e| format!("reading the response body: {e}"))?;
    if buf.len() as u64 > MAX_BODY_BYTES {
        return Err(format!("the response body exceeds {MAX_BODY_BYTES} bytes"));
    }
    String::from_utf8(buf).map_err(|e| {
        format!(
            "the response body is not UTF-8 at byte {}",
            e.utf8_error().valid_up_to()
        )
    })
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn xml_prologue_declares_an_encoding() {
        assert_eq!(
            xml_declared_encoding(b"<?xml version='1.0' encoding=\"windows-1251\"?><a/>"),
            Some("windows-1251".into())
        );
        assert_eq!(xml_declared_encoding(b"<?xml version='1.0'?><a/>"), None);
        assert_eq!(xml_declared_encoding(b"<?xml-stylesheet href='x'?>"), None);
        assert_eq!(xml_declared_encoding(b"<a/>"), None);
    }

    #[test]
    fn a_body_is_read_whole_and_strictly() {
        let big = vec![b'x'; 11 << 20];
        assert_eq!(text_from_reader(&big[..]).unwrap().len(), 11 << 20);
        let err = text_from_reader(&b"caf\xe9"[..]).unwrap_err();
        assert!(err.contains("not UTF-8 at byte 3"), "{err}");
    }
}
