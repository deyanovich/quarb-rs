//! Tab-completion candidates for query text.
//!
//! Two tiers behind one call. The **syntax tier** needs nothing
//! but the text: after `@|` the aggregate registry answers, after
//! `|` the scalars, after `$` the register spellings — the same
//! stdlib registry the highlighter reads, so a new built-in
//! completes without a second edit. The **data tier** adds a live
//! adapter: hop-name candidates enumerate the actual children at
//! the cursor's path prefix, and `::` candidates include the
//! adapter's aliased annotations (ruling #29), so a git mount
//! offers `::short` and a JSON mount offers nothing invented.
//!
//! Consumers: quai's readline completer calls [`complete_with`]
//! in-process against its mounted session (the best completions —
//! real data); quarb-lsp serves editors the syntax tier always
//! and the data tier only for explicitly configured mounts.
//!
//! The context scan is deliberately token-shallow: it inspects
//! the text before the cursor, not a full parse, so it answers
//! mid-keystroke on text no parser would accept. Property
//! *enumeration* (which `::keys` exist on a node) has no adapter
//! surface yet — candidates come from `aliased_metadata` alone;
//! an enumeration method is a 0.21 trait discussion.

use crate::adapter::{AstAdapter, NodeId};
use crate::stdlib;

/// One completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The text to insert.
    pub text: String,
    /// What kind of thing it names.
    pub kind: Kind,
}

/// The candidate's kind, for icons/sorting on the consumer side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A `|` scalar function.
    Function,
    /// An `@|` aggregate (plain or keyed).
    Aggregate,
    /// A register spelling (`$.`, `@.`, `%.`, …).
    Register,
    /// A child hop name from the mounted arbor.
    Child,
    /// A property/annotation key (`::…`), aliased or core.
    Property,
    /// A trait name from the mounted arbor.
    Trait,
}

fn push_all(out: &mut Vec<Candidate>, names: &[&str], kind: Kind, prefix: &str) {
    for n in names {
        if n.starts_with(prefix) {
            out.push(Candidate { text: (*n).to_string(), kind });
        }
    }
}

/// The word (identifier chars) immediately before `cursor`, and
/// the byte where it starts.
fn word_before(text: &str, cursor: usize) -> (&str, usize) {
    let head = &text[..cursor.min(text.len())];
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map_or(0, |i| i + c_len(head, i));
    (&head[start..], start)
}

fn c_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(1, char::len_utf8)
}

/// What sits before the current word — the completion context.
#[derive(Debug, PartialEq, Eq)]
enum Cx {
    AggPipe,
    Pipe,
    Projection,
    Register,
    Hop,
    Trait,
    Other,
}

fn context(text: &str, word_start: usize) -> Cx {
    let head = text[..word_start].trim_end();
    if head.ends_with("@|") {
        return Cx::AggPipe;
    }
    if head.ends_with('|') && !head.ends_with("||") {
        return Cx::Pipe;
    }
    // The colon ladder: any run of 2+ colons is a projection
    // context (:: data, ::: core, :::: adapter).
    if head.ends_with("::") {
        return Cx::Projection;
    }
    if head.ends_with('$') {
        return Cx::Register;
    }
    if (head.ends_with('<') && !head.ends_with("<=")) || head.ends_with("(:") {
        return Cx::Trait;
    }
    // The pointy hops and their rounded spellings (`:-`, `-:`, the
    // sibling `;-` / `-;`; `../` and `:--` / `--:` end in `/` and
    // `--` / `-:` already).
    if head.ends_with('/')
        || head.ends_with("->")
        || head.ends_with("<-")
        || head.ends_with("--")
        || head.ends_with(":-")
        || head.ends_with("-:")
        || head.ends_with(";-")
        || head.ends_with("-;")
    {
        return Cx::Hop;
    }
    Cx::Other
}

/// Register spellings offered after a bare `$`.
const REGISTERS: &[&str] = &["$_", "$.", "$$", "$-", "$ordinal"];

/// Syntax-tier completion: candidates from the text alone.
pub fn complete(text: &str, cursor: usize) -> Vec<Candidate> {
    let (word, start) = word_before(text, cursor);
    let mut out = Vec::new();
    match context(text, start) {
        Cx::AggPipe => {
            push_all(&mut out, stdlib::aggregate_names(), Kind::Aggregate, word);
            push_all(&mut out, stdlib::keyed_names(), Kind::Aggregate, word);
        }
        Cx::Pipe => {
            push_all(&mut out, stdlib::scalar_names(), Kind::Function, word);
            // Keyed and reducing aggregates also ride the plain
            // pipe (per capsa on a group's members).
            push_all(&mut out, stdlib::keyed_names(), Kind::Aggregate, word);
        }
        Cx::Register => {
            // The `$` is already typed; offer the full spellings.
            for r in REGISTERS {
                out.push(Candidate { text: (*r).to_string(), kind: Kind::Register });
            }
        }
        _ => {}
    }
    out
}

/// Data-tier completion: the syntax tier plus candidates
/// enumerated from a live adapter at the cursor's path prefix.
pub fn complete_with(text: &str, cursor: usize, adapter: &dyn AstAdapter) -> Vec<Candidate> {
    let (word, start) = word_before(text, cursor);
    let mut out = complete(text, cursor);
    match context(text, start) {
        Cx::Hop => {
            let at = resolve_prefix(text, start, adapter);
            let mut seen = std::collections::BTreeSet::new();
            for node in at {
                for child in adapter.children(node) {
                    if let Some(name) = adapter.name(child) {
                        if name.starts_with(word) && seen.insert(name.clone()) {
                            out.push(Candidate { text: name, kind: Kind::Child });
                        }
                    }
                }
            }
        }
        Cx::Projection => {
            let at = resolve_prefix(text, start, adapter);
            let mut seen = std::collections::BTreeSet::new();
            for node in at {
                for key in adapter.aliased_metadata(node) {
                    if key.starts_with(word) && seen.insert(*key) {
                        out.push(Candidate { text: (*key).to_string(), kind: Kind::Property });
                    }
                }
            }
        }
        Cx::Trait => {
            let at = resolve_prefix(text, start, adapter);
            let mut seen = std::collections::BTreeSet::new();
            for node in at {
                for t in adapter.traits(node) {
                    if t.starts_with(word) && seen.insert(t.clone()) {
                        out.push(Candidate { text: t, kind: Kind::Trait });
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Walk the plain-hop prefix before `word_start` down from the
/// root: `/a/b/` resolves to the b-nodes. Anything fancier than
/// name hops (predicates, links, searches) falls back to the
/// walked-so-far set — candidates stay real, just broader.
fn resolve_prefix(text: &str, word_start: usize, adapter: &dyn AstAdapter) -> Vec<NodeId> {
    let head = &text[..word_start];
    // The last whitespace-delimited token holds the path.
    let path = head.rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
    let mut nodes = vec![adapter.root()];
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if seg == "*" {
            nodes = nodes.iter().flat_map(|n| adapter.children(*n)).collect();
            continue;
        }
        if !seg.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
            break;
        }
        nodes = nodes
            .iter()
            .flat_map(|n| adapter.children_named(*n, seg))
            .collect();
        if nodes.is_empty() {
            break;
        }
    }
    nodes.truncate(64); // a completion answers fast or not at all
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(cands: &[Candidate]) -> Vec<&str> {
        cands.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn agg_pipe_offers_aggregates() {
        let c = complete("/cf/entry @| co", 15);
        assert!(texts(&c).contains(&"count"));
        assert!(!texts(&c).contains(&"upper"));
    }

    #[test]
    fn plain_pipe_offers_scalars_and_keyed() {
        let c = complete("/x | up", 7);
        assert!(texts(&c).contains(&"upper"));
        let c = complete("/x | max", 8);
        assert!(texts(&c).contains(&"max_by"));
    }

    #[test]
    fn prefix_filters() {
        let c = complete("/x @| uni", 9);
        assert_eq!(texts(&c), vec!["unique", "unique_by"]);
    }

    #[test]
    fn dollar_offers_registers() {
        let c = complete("/x[::a = $", 10);
        assert!(texts(&c).contains(&"$ordinal"));
    }

    #[test]
    fn mid_word_cursor() {
        // Cursor inside "cou|nt" completes from "cou".
        let c = complete("/x @| count", 9);
        assert!(texts(&c).contains(&"count"));
    }

    #[test]
    fn data_tier_enumerates_children() {
        // The reflection arbor is itself an adapter — complete
        // against a reflected query (Quarb completing Quarb).
        let arbor =
            crate::reflect::QueryArbor::parse("/books/*[::price > 20]::title").unwrap();
        let text = "/query/";
        let c = complete_with(text, text.len(), &arbor);
        let names = texts(&c);
        assert!(names.contains(&"branch"), "got {names:?}");
    }
}
