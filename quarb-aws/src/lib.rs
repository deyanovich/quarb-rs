//! Shared AWS plumbing for the Quarb adapters: the SigV4 request
//! signer and the credential chain. No SDK and no new
//! dependencies — the consumers (`quarb-objstore`'s `s3://`,
//! `quarb-dynamodb`, `quarb-athena`) speak plain HTTP and this
//! crate only computes the headers a request must carry.
//!
//! The credential chain, in order:
//! 1. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`
//!    (+ `AWS_SESSION_TOKEN`) from the environment;
//! 2. the `~/.aws/credentials` INI, profile from `AWS_PROFILE`
//!    (default `default`).
//!
//! Region resolution: an explicit `?region=` on the target wins,
//! then `AWS_REGION` / `AWS_DEFAULT_REGION`, then the profile's
//! `region` in `~/.aws/config`, then `us-east-1`.
//!
//! Read-only adapters mean the signer only ever authenticates
//! GETs and service POSTs; nothing here can mutate cloud state
//! beyond what the caller's credentials allow the request to say.

use quarb::{sha256, sha256_hex};

/// A resolved AWS credential set.
#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

/// Load credentials from the chain; `None` when the chain is
/// empty (callers then go anonymous or refuse, per adapter).
pub fn load_credentials() -> Option<Credentials> {
    if let (Ok(ak), Ok(sk)) = (
        std::env::var("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY"),
    ) && !ak.trim().is_empty()
        && !sk.trim().is_empty()
    {
        return Some(Credentials {
            access_key: ak.trim().to_string(),
            secret_key: sk.trim().to_string(),
            session_token: std::env::var("AWS_SESSION_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty()),
        });
    }
    let home = std::env::var("HOME").ok()?;
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
    let ini = std::fs::read_to_string(format!("{home}/.aws/credentials")).ok()?;
    let section = ini_section(&ini, &profile)?;
    Some(Credentials {
        access_key: section.get("aws_access_key_id")?.clone(),
        secret_key: section.get("aws_secret_access_key")?.clone(),
        session_token: section.get("aws_session_token").cloned(),
    })
}

/// Resolve the region: explicit target parameter first, then the
/// environment, then `~/.aws/config`, then `us-east-1`.
pub fn region(explicit: Option<&str>) -> String {
    if let Some(r) = explicit {
        return r.to_string();
    }
    for var in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
        if let Ok(r) = std::env::var(var)
            && !r.trim().is_empty()
        {
            return r.trim().to_string();
        }
    }
    if let Ok(home) = std::env::var("HOME")
        && let Ok(ini) = std::fs::read_to_string(format!("{home}/.aws/config"))
    {
        let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
        let section_name = if profile == "default" {
            profile
        } else {
            format!("profile {profile}")
        };
        if let Some(s) = ini_section(&ini, &section_name)
            && let Some(r) = s.get("region")
        {
            return r.clone();
        }
    }
    "us-east-1".to_string()
}

fn ini_section(
    ini: &str,
    name: &str,
) -> Option<std::collections::HashMap<String, String>> {
    let mut current = None::<String>;
    let mut out = std::collections::HashMap::new();
    let mut hit = false;
    for line in ini.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current = Some(line[1..line.len() - 1].trim().to_string());
            continue;
        }
        if current.as_deref() == Some(name)
            && let Some((k, v)) = line.split_once('=')
        {
            hit = true;
            out.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    hit.then_some(out)
}

/// HMAC-SHA256 over the engine's own SHA-256.
fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = sha256(&[ipad.as_slice(), msg].concat());
    sha256(&[opad.as_slice(), &inner].concat())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SigV4's URI encoding: RFC 3986 unreserved stays, `/` kept
/// only in paths, everything else uppercase percent-escaped.
fn uri_encode(s: &[u8], keep_slash: bool) -> String {
    let mut out = String::new();
    for &b in s {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Percent-decode to raw bytes; malformed escapes pass through.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// The UTC timestamp pair SigV4 wants: (`YYYYMMDDTHHMMSSZ`,
/// `YYYYMMDD`), computed from the system clock with a civil-date
/// conversion (no time dependency).
fn timestamp() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before 1970")
        .as_secs();
    let days = secs / 86400;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    let date = format!("{y:04}{mo:02}{d:02}");
    (format!("{date}T{h:02}{m:02}{s:02}Z"), date)
}

/// Sign a request. `url` is the full target URL (the query string
/// included); `payload` is the request body (empty for GET).
/// Returns the headers to set — `host` is included and MUST be
/// sent exactly as returned; `extra_headers` (lowercase names)
/// are folded into the signature and must also be sent verbatim.
pub fn sign(
    creds: &Credentials,
    method: &str,
    url: &str,
    region: &str,
    service: &str,
    payload: &[u8],
    extra_headers: &[(&str, &str)],
) -> Vec<(String, String)> {
    let (amz_date, date) = timestamp();
    sign_at(
        creds,
        method,
        url,
        region,
        service,
        payload,
        extra_headers,
        &amz_date,
        &date,
    )
}

/// The clock-injected core of [`sign`], separable for the test
/// vectors.
#[allow(clippy::too_many_arguments)]
fn sign_at(
    creds: &Credentials,
    method: &str,
    url: &str,
    region: &str,
    service: &str,
    payload: &[u8],
    extra_headers: &[(&str, &str)],
    amz_date: &str,
    date: &str,
) -> Vec<(String, String)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path_query) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_query, ""),
    };
    // The canonical form NORMALIZES the URL's path and query:
    // each component is percent-decoded and then SigV4-encoded
    // (uppercase escapes over the RFC 3986 unreserved set). This
    // is what the server does before checking the signature, and
    // it makes the signer indifferent to how the caller mixed
    // raw and pre-encoded characters — a raw `delimiter=/` and a
    // pre-encoded `prefix=a%2Fb` both canonicalize correctly
    // (naive re-encoding turns `%2F` into `%252F` and fails;
    // as-is passing leaves the raw `/` unencoded and fails).
    let canonical_path = if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", uri_encode(&percent_decode(path), true))
    };
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (
                uri_encode(&percent_decode(k), false),
                uri_encode(&percent_decode(v), false),
            ),
            None => (uri_encode(&percent_decode(kv), false), String::new()),
        })
        .collect();
    pairs.sort();
    let canonical_query = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let payload_hash = sha256_hex(payload);
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), host.to_string()),
        ("x-amz-content-sha256".into(), payload_hash.clone()),
        ("x-amz-date".into(), amz_date.to_string()),
    ];
    if let Some(t) = &creds.session_token {
        headers.push(("x-amz-security-token".into(), t.clone()));
    }
    for (k, v) in extra_headers {
        headers.push((k.to_lowercase(), v.to_string()));
    }
    headers.sort();
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_names = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{canonical_path}\n{canonical_query}\n{canonical_headers}\n{signed_names}\n{payload_hash}"
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac(format!("AWS4{}", creds.secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_names}, Signature={signature}",
        creds.access_key
    );
    let mut out = headers;
    out.push(("authorization".into(), auth));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A regression pin on the classic SigV4 example request
    /// (GET ListUsers at 2015-08-30T12:36:00Z with the
    /// documented example keys). The value is NOT the published
    /// vector — this signer always includes
    /// x-amz-content-sha256 in the signed set, which the
    /// classic example does not — so the pin locks this
    /// implementation against drift; end-to-end correctness is
    /// proven by MinIO validating live signatures in the
    /// objstore integration test.
    #[test]
    fn sigv4_reference_vector() {
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let headers = sign_at(
            &creds,
            "GET",
            "https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08",
            "us-east-1",
            "iam",
            b"",
            &[],
            "20150830T123600Z",
            "20150830",
        );
        let auth = &headers.iter().find(|(k, _)| k == "authorization").unwrap().1;
        assert!(
            auth.ends_with(
                "Signature=65f031d93b4631aedf16a8f7f830cdc8ce2bc5276c307b5a2cc2143d4b68e323"
            ),
            "got {auth}"
        );
    }

    #[test]
    fn ini_parsing() {
        let ini = "[default]\naws_access_key_id = AK\n\
                   aws_secret_access_key = SK\n[other]\nregion = eu-west-1\n";
        let s = ini_section(ini, "default").unwrap();
        assert_eq!(s["aws_access_key_id"], "AK");
        assert!(ini_section(ini, "missing").is_none());
        assert_eq!(ini_section(ini, "other").unwrap()["region"], "eu-west-1");
    }
}
