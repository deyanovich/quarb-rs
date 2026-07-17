//! Hashing and binary-to-text encodings for the standard library:
//! SHA-256 (FIPS 180-4, hand-rolled — the one hash the stdlib
//! promises) and RFC 4648 base64 / base64url.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The SHA-256 digest of `data`, as 32 raw bytes (FIPS 180-4).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

/// The SHA-256 digest of `data`, as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    sha256(data).iter().map(|b| format!("{b:02x}")).collect()
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_with(data: &[u8], alphabet: &[u8], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        let keep = chunk.len() + 1;
        for (i, &ix) in idx.iter().enumerate() {
            if i < keep {
                out.push(alphabet[ix as usize] as char);
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

/// RFC 4648 base64, padded.
pub fn base64(data: &[u8]) -> String {
    b64_with(data, B64, true)
}

/// RFC 4648 §5 URL-safe base64, unpadded (the JWT convention).
pub fn base64url(data: &[u8]) -> String {
    b64_with(data, B64URL, false)
}

const B32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32, padded.
pub fn base32(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        // 8 output symbols per 5 input bytes; a partial chunk of
        // 1/2/3/4 bytes keeps 2/4/5/7 symbols.
        let keep = [0, 2, 4, 5, 7, 8][chunk.len()];
        for i in 0..8 {
            if i < keep {
                let ix = ((n >> (35 - 5 * i)) & 31) as usize;
                out.push(B32[ix] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Crockford's Base32: the RFC 4648 bit layout with Crockford's
/// alphabet (no I, L, O, U — the human-transcription set, as in
/// ULIDs), unpadded.
pub fn crockford32(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        let keep = [0, 2, 4, 5, 7, 8][chunk.len()];
        for i in 0..keep {
            let ix = ((n >> (35 - 5 * i)) & 31) as usize;
            out.push(CROCKFORD[ix] as char);
        }
    }
    out
}

/// Lowercase hex.
pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode one of the reversible encodings (base64, base64url,
/// base32, crockford32, hex) back to bytes. Returns `None` on
/// malformed input; each scheme accepts its lenient conventions.
pub fn decode(scheme: &str, s: &str) -> Option<Vec<u8>> {
    match scheme {
        "base64" | "base64url" => decode_base64(s),
        "base32" => decode_base32(s, false),
        "crockford32" => decode_base32(s, true),
        "hex" => decode_hex(s),
        _ => None,
    }
}

/// Whether a scheme name can be decoded (`sha256` cannot — a
/// digest is one-way). The parser uses this to reject `dec(...)`
/// of a non-reversible scheme.
pub fn is_decodable(scheme: &str) -> bool {
    matches!(
        scheme,
        "base64" | "base64url" | "base32" | "crockford32" | "hex"
    )
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

// Both base64 alphabets, padding and whitespace tolerated.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let syms: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(syms.len() * 3 / 4);
    for chunk in syms.chunks(4) {
        // A trailing quantum of a single symbol carries fewer than
        // 8 bits — no encoder emits it; reject rather than drop it.
        if chunk.len() == 1 {
            return None;
        }
        let mut acc = 0u32;
        for &c in chunk {
            acc = (acc << 6) | b64_val(c)? as u32;
        }
        // A trailing chunk of k symbols carries k-1 bytes.
        acc <<= 6 * (4 - chunk.len());
        let bytes = chunk.len().saturating_sub(1);
        for i in 0..bytes {
            out.push((acc >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

fn b32_val(c: u8, crockford: bool) -> Option<u8> {
    let c = c.to_ascii_uppercase();
    if crockford {
        match c {
            b'0' | b'O' => Some(0),
            b'1' | b'I' | b'L' => Some(1),
            b'2'..=b'9' => Some(c - b'0'),
            // A-Z minus I L O U, in value order.
            _ => CROCKFORD.iter().position(|&x| x == c).map(|p| p as u8),
        }
    } else {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'2'..=b'7' => Some(c - b'2' + 26),
            _ => None,
        }
    }
}

// RFC 4648 base32 and Crockford; padding, whitespace, and (for
// Crockford) hyphens tolerated.
fn decode_base32(s: &str, crockford: bool) -> Option<Vec<u8>> {
    let syms: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && c != b'-' && !c.is_ascii_whitespace())
        .collect();
    let mut acc = 0u64;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(syms.len() * 5 / 8);
    for &c in &syms {
        acc = (acc << 5) | b32_val(c, crockford)? as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let syms: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if syms.len() % 2 != 0 {
        return None;
    }
    syms.chunks(2)
        .map(|p| {
            let hi = (p[0] as char).to_digit(16)?;
            let lo = (p[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

/// Parse JSON text into a [`Value`]: object → record (key order
/// preserved), array → list, and the scalars map directly. `None`
/// on malformed JSON — the decode(json) stage nulls it.
pub fn json_to_value(text: &str) -> Option<crate::value::Value> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(from_serde(&v))
}

/// Parse YAML text into a [`Value`] (the JSON data model applies).
pub fn yaml_to_value(text: &str) -> Option<crate::value::Value> {
    let v: serde_json::Value = serde_yaml_ng::from_str(text).ok()?;
    Some(from_serde(&v))
}

/// Parse TOML text into a [`Value`].
pub fn toml_to_value(text: &str) -> Option<crate::value::Value> {
    let v: serde_json::Value = toml::from_str(text).ok()?;
    Some(from_serde(&v))
}

/// Parse XML text into a [`Value`], by a fixed convention: a
/// text-only element (no attributes, no child elements) is its
/// text; otherwise a record whose attributes are `@name` keys,
/// child elements their tag names (a tag repeated within one
/// parent collects into a list), and any non-whitespace text a
/// `#text` key. `None` on malformed XML.
pub fn xml_to_value(text: &str) -> Option<crate::value::Value> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let reader_ = Reader::from_str(text);
    let mut reader = reader_;

    // A frame per open element: its attribute/child fields (in
    // order) and accumulated text.
    struct Frame {
        fields: Vec<(String, crate::value::Value)>,
        text: String,
    }
    let mut stack: Vec<Frame> = vec![Frame {
        fields: Vec::new(),
        text: String::new(),
    }];

    let attrs = |e: &quick_xml::events::BytesStart| -> Vec<(String, crate::value::Value)> {
        let mut out = Vec::new();
        for a in e.attributes().flatten() {
            let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let val = a
                .unescape_value()
                .ok()
                .map(|v| v.into_owned())
                .unwrap_or_default();
            out.push((format!("@{key}"), crate::value::Value::Str(val)));
        }
        out
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let mut f = Frame {
                    fields: Vec::new(),
                    text: String::new(),
                };
                f.fields.extend(attrs(&e));
                stack.push(f);
            }
            Ok(Event::Empty(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let a = attrs(&e);
                let v = if a.is_empty() {
                    crate::value::Value::Str(String::new())
                } else {
                    crate::value::Value::Record(a)
                };
                push_field(&mut stack.last_mut()?.fields, tag, v);
            }
            Ok(Event::Text(t)) => {
                let s = t.xml_content().ok()?;
                stack.last_mut()?.text.push_str(&s);
            }
            Ok(Event::CData(t)) => {
                stack
                    .last_mut()?
                    .text
                    .push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::GeneralRef(e)) => {
                let frame = stack.last_mut()?;
                if let Ok(Some(ch)) = e.resolve_char_ref() {
                    frame.text.push(ch);
                } else {
                    let name = e.decode().ok()?;
                    frame
                        .text
                        .push_str(quick_xml::escape::resolve_predefined_entity(&name)?);
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let frame = stack.pop()?;
                let v = frame_to_value(frame.fields, frame.text);
                push_field(&mut stack.last_mut()?.fields, tag, v);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    // The document element is the lone field of the synthetic root.
    let root = stack.pop()?;
    match root.fields.into_iter().next() {
        Some((_, v)) => Some(v),
        None => Some(crate::value::Value::Null),
    }
}

/// Fold an element's fields and text into its value.
fn frame_to_value(fields: Vec<(String, crate::value::Value)>, text: String) -> crate::value::Value {
    if fields.is_empty() {
        return crate::value::Value::Str(text);
    }
    let mut fields = fields;
    if !text.trim().is_empty() {
        fields.push(("#text".to_string(), crate::value::Value::Str(text)));
    }
    crate::value::Value::Record(fields)
}

/// Add `(key, value)` to a field list, collecting a repeated key
/// into a list (in document order).
fn push_field(
    fields: &mut Vec<(String, crate::value::Value)>,
    key: String,
    value: crate::value::Value,
) {
    if let Some((_, existing)) = fields.iter_mut().find(|(k, _)| *k == key) {
        match existing {
            crate::value::Value::List(items) => items.push(value),
            other => {
                let prev = std::mem::replace(other, crate::value::Value::Null);
                *other = crate::value::Value::List(vec![prev, value]);
            }
        }
    } else {
        fields.push((key, value));
    }
}

/// Whether `fmt` decodes a document into a structured value
/// (record/list) rather than the byte encodings' text.
pub fn is_structured_format(fmt: &str) -> bool {
    matches!(fmt, "json" | "yaml" | "toml" | "xml")
}

fn from_serde(v: &serde_json::Value) -> crate::value::Value {
    use crate::value::Value;
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Value::Int)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(a) => Value::List(a.iter().map(from_serde).collect()),
        serde_json::Value::Object(o) => {
            Value::Record(o.iter().map(|(k, v)| (k.clone(), from_serde(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        // FIPS 180-4 / NIST test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn base64_vectors() {
        // RFC 4648 §10.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(b"fo"), "Zm8");
        // The url-safe alphabet: 0xfb 0xff encodes with - and _.
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(hex(b"quarb"), "7175617262");
        // RFC 4648 §10 base32 vectors.
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY======");
        assert_eq!(base32(b"fo"), "MZXQ====");
        assert_eq!(base32(b"foo"), "MZXW6===");
        assert_eq!(base32(b"foob"), "MZXW6YQ=");
        assert_eq!(base32(b"fooba"), "MZXW6YTB");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI======");
        // Crockford: same bit layout, transcription-safe alphabet,
        // no padding.
        assert_eq!(crockford32(b"foobar"), "CSQPYRK1E8");
        assert_eq!(crockford32(b"f"), "CR");
        assert_eq!(crockford32(b""), "");
    }

    #[test]
    fn decode_round_trips() {
        for s in [b"".as_slice(), b"f", b"fo", b"foobar", &[0xfb, 0xff]] {
            assert_eq!(decode("base64", &base64(s)).as_deref(), Some(s));
            assert_eq!(decode("base64url", &base64url(s)).as_deref(), Some(s));
            assert_eq!(decode("base32", &base32(s)).as_deref(), Some(s));
            assert_eq!(decode("crockford32", &crockford32(s)).as_deref(), Some(s));
            assert_eq!(decode("hex", &hex(s)).as_deref(), Some(s));
        }
        // Crockford leniency: lowercase, O/I/L aliases, hyphens.
        assert_eq!(
            decode("crockford32", "csqpyrk1e8"),
            decode("crockford32", "CSQPYRK1E8")
        );
        assert_eq!(
            decode("crockford32", "C5-QP"),
            decode("crockford32", "C5QP")
        );
        // Unpadded base64 still decodes.
        assert_eq!(decode("base64", "Zm8"), Some(b"fo".to_vec()));
        // Malformed: odd hex, bad symbol.
        assert_eq!(decode("hex", "abc"), None);
        assert_eq!(decode("base32", "1"), None);
        // Base64 lengths ≡ 1 (mod 4) are impossible encoder output;
        // reject rather than silently dropping the dangling symbol.
        assert_eq!(decode("base64", "A"), None);
        assert_eq!(decode("base64", "Zm9vY"), None);
        assert!(!is_decodable("sha256"));
        // Structured decode: json/yaml/toml → Value.
        use crate::value::Value;
        assert_eq!(
            json_to_value("[1,2]"),
            Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
        );
        assert!(matches!(yaml_to_value("a: 1"), Some(Value::Record(_))));
        assert!(matches!(toml_to_value("a = 1"), Some(Value::Record(_))));
        assert_eq!(json_to_value("{bad"), None);
        assert!(is_structured_format("yaml") && is_structured_format("toml"));
        // XML: text-only element → scalar; attrs → @keys; repeated
        // tags → list; entities resolved.
        assert_eq!(xml_to_value("<p>hi</p>"), Some(Value::Str("hi".into())));
        assert_eq!(
            xml_to_value("<p>a &amp; b</p>"),
            Some(Value::Str("a & b".into()))
        );
        assert!(matches!(
            xml_to_value("<b id='1'><t>x</t></b>"),
            Some(Value::Record(_))
        ));
        if let Some(Value::Record(f)) = xml_to_value("<l><i>1</i><i>2</i></l>") {
            assert_eq!(f[0].0, "i");
            assert!(matches!(f[0].1, Value::List(ref v) if v.len() == 2));
        } else {
            panic!("expected record");
        }
        assert_eq!(xml_to_value("<not well formed"), None);
        assert!(is_structured_format("xml"));
    }
}
