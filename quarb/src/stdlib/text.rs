//! Part of the executor: see `exec/mod.rs`.

use crate::value::Value;
use regex::Regex;
use std::collections::HashMap;

thread_local! {
    /// Compiled-pattern memo. `=~` predicates evaluate per node,
    /// and recompiling one pattern for every candidate made a
    /// workspace-wide regex filter ~10x its equality twin; a
    /// query's patterns are few, so the memo stays tiny. The
    /// `s///` command shares it: a Unicode class such as `\p{L}`
    /// compiles in about a millisecond, which over the seventy
    /// thousand words of a novel was a hundred seconds. `None`
    /// remembers a bad pattern (it never matches — no point
    /// re-failing per node).
    static REGEX_MEMO: std::cell::RefCell<HashMap<String, Option<Regex>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The compiled form of `pattern`, memoized; `None` for a bad
/// pattern. (`Regex` clones share the compiled program.)
pub(crate) fn compiled_regex(pattern: &str) -> Option<Regex> {
    REGEX_MEMO.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(re) = m.get(pattern) {
            return re.clone();
        }
        // Bounded: a query whose patterns are data (one per node)
        // would otherwise grow the memo without limit.
        if m.len() >= 4_096 {
            m.clear();
        }
        let re = Regex::new(pattern).ok();
        m.insert(pattern.to_string(), re.clone());
        re
    })
}

/// Regex match: `a` (as text) against `b` (as pattern); `want` selects
/// match vs non-match. A bad pattern never matches.
pub(crate) fn regex_test(a: &Value, b: &Value, want: bool) -> bool {
    // A null subject or pattern doesn't participate — the match is
    // undefined, so null-propagate rather than stringifying to "":
    // a null pattern would otherwise compile to the empty regex and
    // match everything. `=~` (want = true) yields false, `!~`
    // (want = false) yields true, mirroring `=` / `!=` on null.
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return !want;
    }
    match compiled_regex(&b.to_string()) {
        Some(re) => re.is_match(&a.to_string()) == want,
        None => false,
    }
}
