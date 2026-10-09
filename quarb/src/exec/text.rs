//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// Run one shell command for the `sh(...)` stage: `cmd` under
/// `sh -c`, the topic's text on stdin, stdout (one trailing newline
/// trimmed) as the result. A failing spawn or exit status is `None`
/// (the stage nulls it); stderr is inherited so diagnostics stay
/// visible.
pub(super) fn run_shell(cmd: &str, topic: Option<&Value>) -> Option<Value> {
    use std::io::Write;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(v) = topic
        && let Some(mut stdin) = child.stdin.take()
    {
        let text = v.to_string();
        // Feed stdin from a separate thread so the parent drains
        // stdout concurrently: a command that echoes its input
        // (cat, jq, sort, …) over a topic larger than the OS pipe
        // buffer would otherwise deadlock — parent blocked writing
        // stdin, child blocked writing stdout. The thread drops
        // stdin on completion, giving the child EOF.
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
            if !text.ends_with('\n') {
                let _ = stdin.write_all(b"\n");
            }
        });
    }
    // Any stdin handle not moved into the writer thread (the
    // topicless case) is dropped so the child still sees EOF.
    drop(child.stdin.take());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.ends_with('\n') {
        text.pop();
    }
    Some(Value::Str(text))
}

thread_local! {
    /// Compiled-pattern memo. `=~` predicates evaluate per node,
    /// and recompiling one pattern for every candidate made a
    /// workspace-wide regex filter ~10x its equality twin; a
    /// query's patterns are few, so the memo stays tiny. The
    /// `s///` stage shares it: a Unicode class such as `\p{L}`
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
pub(super) fn regex_test(a: &Value, b: &Value, want: bool) -> bool {
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
