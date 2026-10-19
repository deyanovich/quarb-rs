//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// The node's name-path from the adapter's root: `/teams/0/members/0`.
pub fn name_path<A: AstAdapter + ?Sized>(adapter: &A, node: NodeId) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(node);
    while let Some(n) = cur {
        if adapter.parent(n).is_none() {
            break;
        }
        if let Some(t) = adapter.name(n) {
            parts.push(t);
        }
        cur = adapter.parent(n);
    }
    parts.reverse();
    format!("/{}", parts.join("/"))
}

/// Whether `text` is a URL with a scheme and a host (`https://…`),
/// as against a path or an opaque identity.
pub fn is_url(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|u| u.has_host() || u.cannot_be_a_base()) && text.contains(':')
}

/// An identity in the form the tree registry compares: a URL
/// normalized (scheme and host lowercased, the default port
/// dropped, no fragment); an absolute path with `.` and `..` folded
/// (symlinks left alone); any other string as it is. A trailing
/// `/` is kept: it marks a base. Pure: evaluation never touches the
/// filesystem — a relative path is made absolute when its tree is
/// registered ([`canonical_identity_at_open`]).
pub fn canonical_identity(text: &str) -> String {
    if is_url(text)
        && let Ok(mut u) = url::Url::parse(text)
    {
        u.set_fragment(None);
        return u.to_string();
    }
    if !text.starts_with('/') {
        return text.to_string();
    }
    fold_path(std::path::Path::new(text), text)
}

/// [`canonical_identity`] for the mount layer, at open time: a
/// relative path (`./notes.html`, a bare file name that exists) is
/// made absolute against the working directory first. The one
/// place an identity reads the filesystem.
pub fn canonical_identity_at_open(text: &str) -> String {
    if is_url(text) || text.starts_with('/') {
        return canonical_identity(text);
    }
    let looks_like_path =
        text.starts_with("./") || text.starts_with("../") || text == "." || text == "..";
    if !looks_like_path && !std::path::Path::new(text).exists() {
        return text.to_string();
    }
    let base = std::env::current_dir().unwrap_or_default();
    fold_path(&base.join(text), text)
}

/// An absolute path with `.` and `..` folded, keeping `text`'s
/// trailing `/`.
fn fold_path(joined: &std::path::Path, text: &str) -> String {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                parts.clear();
            }
            std::path::Component::Normal(p) => parts.push(p.to_os_string()),
        }
    }
    let mut out = String::from("/");
    out.push_str(
        &parts
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    );
    if text.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out
}

/// A reference made absolute against the identity of the tree it
/// was written in: a URL by RFC 3986 where the base is a URL, a
/// path joined to the base's directory and folded where the base is
/// a path; a reference absolute in its own terms (a URL, a `/`-led
/// path) and a reference with no base stay as they are, canonical.
pub(crate) fn absolute_identity(reference: &str, base: Option<&str>) -> String {
    if is_url(reference) {
        return canonical_identity(reference);
    }
    match base {
        Some(b) if is_url(b) => match url::Url::parse(b).and_then(|u| u.join(reference)) {
            Ok(mut u) => {
                u.set_fragment(None);
                u.to_string()
            }
            Err(_) => canonical_identity(reference),
        },
        Some(b) if b.starts_with('/') && !reference.starts_with('/') => {
            let dir = if b.ends_with('/') {
                b.to_string()
            } else {
                b.rsplit_once('/')
                    .map_or("/".to_string(), |(d, _)| format!("{d}/"))
            };
            canonical_identity(&format!("{dir}{reference}"))
        }
        _ => canonical_identity(reference),
    }
}

/// The tree `from` is written in: the nearest ancestor-or-self that
/// is the root of its source (its provenance path empty), named by
/// the identity declared for it, or the base declared above it with
/// the names down to it, else the automatic one (its mount's
/// target, a file's path).
pub(crate) fn identity_of(adapter: &impl AstAdapter, from: NodeId) -> Option<String> {
    let mut document = from;
    loop {
        let p = adapter.provenance(document);
        let at_root = p.path.as_deref().is_none_or(|x| x.is_empty() || x == "/");
        if at_root {
            break;
        }
        match adapter.parent(document) {
            Some(q) => document = q,
            None => break,
        }
    }
    let entries = adapter.tree_identities();
    // nearest ancestor-or-self of the document that carries an
    // identity; a base carries its remainder
    let mut names: Vec<String> = Vec::new();
    let mut at = Some(document);
    while let Some(n) = at {
        if let Some((identity, _)) = entries.iter().find(|(_, node)| *node == n) {
            return Some(match identity.chars().last() {
                Some(sep) if !sep.is_alphanumeric() && n != document => {
                    let mut id = identity.clone();
                    names.reverse();
                    id.push_str(&names.join(&sep.to_string()));
                    id
                }
                _ => identity.clone(),
            });
        }
        if let Some(name) = adapter.name(n) {
            names.push(name);
        }
        at = adapter.parent(n);
    }
    adapter
        .provenance(document)
        .source
        .map(|s| canonical_identity(&s))
}

/// The node a reference's document part names: the identity made
/// absolute against the tree the reference was written in, then —
/// after the engine's own locator boundary `!/…` is split off — the
/// host's registration, the adapter's own lookup (a store of pages
/// answers by URL), an exact identity, and the longest base, whose
/// remainder's segments (split on the base's own separator) are
/// navigated by names beneath its node; then `!/…`, a Quarb path
/// evaluated inside the tree. `Err` carries the absolute identity,
/// for the run's record of unresolved references — or nothing,
/// for a relative reference with no tree to be relative to.
pub(crate) fn find_tree(
    adapter: &impl AstAdapter,
    document: &str,
    from: NodeId,
    trace: &Trace,
    outer: Option<&Scope<'_, GraphExt>>,
    witness: &[Option<NodeId>],
    register: &[Reg],
) -> Result<NodeId, Option<String>> {
    let (identity, within) = match document.find("!/") {
        Some(i) => (&document[..i], Some(&document[i + 1..])),
        None => (document, None),
    };
    let base = identity_of(adapter, from);
    // A reference from a tree with no identity of its own (pasted
    // text, nowhere) is anchored only when it is absolute by
    // itself — a URL with a scheme. A relative one, and a `/`-led
    // one (a path, or a URL path: which, the missing base would
    // have said), names nothing: it resolves nothing and is not
    // recorded, since there is nothing to acquire.
    let anchored = base.is_some() || is_url(identity);
    let identity = absolute_identity(identity, base.as_deref());
    let found = ref_target(&identity)
        .or_else(|| adapter.document_by_ref(&identity))
        .or_else(|| {
            let entries = adapter.tree_identities();
            if let Some((_, n)) = entries
                .iter()
                .find(|(i, _)| canonical_identity(i) == identity)
            {
                return Some(*n);
            }
            let mut bases: Vec<(String, NodeId)> = entries
                .into_iter()
                .filter(|(i, _)| i.chars().last().is_some_and(|c| !c.is_alphanumeric()))
                .map(|(i, n)| (canonical_identity(&i), n))
                .filter(|(i, _)| identity.starts_with(i.as_str()))
                .collect();
            bases.sort_by_key(|(i, _)| std::cmp::Reverse(i.len()));
            let (base, node) = bases.into_iter().next()?;
            let sep = base.chars().last()?;
            let mut cur = node;
            for seg in identity[base.len()..].split(sep).filter(|s| !s.is_empty()) {
                cur = adapter.children_named(cur, seg).into_iter().next()?;
            }
            Some(cur)
        });
    let Some(node) = found else {
        return Err(anchored.then_some(identity));
    };
    // `!/…`: a Quarb path inside the tree, evaluated from its node.
    let Some(path) = within else {
        return Ok(node);
    };
    let elems = crate::syntax::parser::parse_scope_path(path)
        .map_err(|_| Some(format!("{identity}!{path}")))?;
    navigate_from(
        &elems.0, adapter, node, trace, outer, witness, register,
    )
    .into_iter()
    .next()
    .ok_or_else(|| Some(format!("{identity}!{path}")))
}
