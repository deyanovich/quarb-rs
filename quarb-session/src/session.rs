//! The session: a macro table where each accepted line becomes
//! `def &N: <line> ;`, evaluated through an [`Executor`] and persisted
//! through a [`Store`].
//!
//! History is the language's own reuse mechanism, not a bolted-on
//! cell store: line 3 is the fragment `&3`, continued through the pipe
//! (`&3 | /name::`, `&3 | [pred]`, `&3 @| count`). Frozen recall
//! (`&N#`) replays a line's captured footprint; the live/version-
//! pinned variants sharpen once a re-materializing executor (the
//! daemon) is in play.

use crate::{Cell, Executor, SessionState, Store};
use anyhow::{Context, Result};
use std::collections::HashMap;

pub struct Session {
    executor: Box<dyn Executor>,
    store: Box<dyn Store>,
    /// The macro table as definition text, re-parsed per query
    /// (`def &1: …;\n …`). Kept as text so it seeds from `--defs`,
    /// round-trips for display, and persists trivially.
    defs_text: String,
    /// Each line's rendered output, captured at commit — the frozen
    /// footprint a `&N#` recall replays. In memory only for now.
    snapshots: HashMap<usize, Vec<Cell>>,
    /// The next line's number — the `&N` a fresh line will claim.
    line_no: usize,
}

impl Session {
    /// Build a session over an executor and a store, restoring any
    /// persisted macro history from the store.
    pub fn new(executor: Box<dyn Executor>, store: Box<dyn Store>) -> Session {
        let state = store.load().unwrap_or_default();
        let line_no = state.line_no.max(1);
        Session {
            executor,
            store,
            defs_text: state.defs_text,
            snapshots: HashMap::new(),
            line_no,
        }
    }

    /// Swap the executor under a running session — an in-session
    /// remount (`:mount`). History and the macro table stay: `&N`
    /// re-runs against the new source set.
    pub fn set_executor(&mut self, executor: Box<dyn Executor>) {
        self.executor = executor;
    }

    /// Seed the macro table from a `--defs` file (validated first).
    /// Stored stripped of `#` comment lines: the table is prepended
    /// to query text, and the query lexer has no comment syntax.
    pub fn seed_defs(&mut self, text: &str) -> Result<()> {
        quarb::parse_defs(text).context("parsing --defs")?;
        self.defs_text = format!("{}\n", quarb::strip_defs_comments(text));
        self.persist();
        Ok(())
    }

    /// Add a `def`/`macro` line to the table (validated first). Unlike
    /// a query line, a definition is not run.
    pub fn add_def(&mut self, line: &str) -> Result<()> {
        let candidate = format!("{}{}\n", self.defs_text, line);
        quarb::parse_defs(&candidate).context("parsing definition")?;
        self.defs_text = candidate;
        self.persist();
        Ok(())
    }

    /// The line with the macro table prepended, so history refs
    /// resolve inline.
    fn combined(&self, line: &str) -> String {
        if self.defs_text.is_empty() {
            line.to_string()
        } else {
            format!("{}\n{line}", self.defs_text)
        }
    }

    /// Evaluate a line against the standing arbor (`&N`). Pure —
    /// history is not touched (a failed line commits nothing).
    pub fn eval(&self, line: &str) -> Result<Vec<Cell>> {
        self.executor.run(&self.combined(line))
    }

    /// Evaluate a line against a freshly re-materialized source — the
    /// `&N!` live reading, which sees current data.
    pub fn eval_fresh(&self, line: &str) -> Result<Vec<Cell>> {
        self.executor.run_fresh(&self.combined(line))
    }

    /// Run a line and render its results as exportable markup
    /// (`md`, `html`, `txt`) — the panel's rendered-export path.
    /// History refs resolve inline, exactly as in [`eval`](Self::eval);
    /// an empty line exports the whole document.
    pub fn export(&self, line: &str, kind: &str) -> Result<String> {
        let line = line.trim();
        if line.is_empty() {
            return self.executor.export("", kind);
        }
        self.executor.export(&self.combined(line), kind)
    }

    /// Register an accepted line as `&N` and capture its output as the
    /// frozen footprint for `&N#`. Returns whether the line's shape
    /// could be a macro body (so `&N` will resolve); either way the
    /// line number advances so labels track what the user saw.
    pub fn commit(&mut self, line: &str, snapshot: Vec<Cell>) -> bool {
        self.snapshots.insert(self.line_no, snapshot);
        // A space before the `;` terminator: a line ending in a `::`
        // projection would otherwise lex `::;` as the metadata sigil.
        let candidate = format!("{}def &{}: {} ;\n", self.defs_text, self.line_no, line);
        let referenceable = quarb::parse_defs(&candidate).is_ok();
        if referenceable {
            self.defs_text = candidate;
        }
        self.line_no += 1;
        self.persist();
        referenceable
    }

    /// The frozen output of line `n`, if captured — what a `&N#`
    /// recall replays.
    pub fn frozen(&self, n: usize) -> Option<&Vec<Cell>> {
        self.snapshots.get(&n)
    }

    /// Record a frozen-recall line: it takes the next number and keeps
    /// its own snapshot, but is not itself a referenceable macro body.
    pub fn record_frozen(&mut self, snapshot: Vec<Cell>) {
        self.snapshots.insert(self.line_no, snapshot);
        self.line_no += 1;
        self.persist();
    }

    /// Persist the durable state (best-effort; a store error does not
    /// abort the session).
    fn persist(&self) {
        let state = SessionState {
            defs_text: self.defs_text.clone(),
            line_no: self.line_no,
        };
        let _ = self.store.save(&state);
    }

    /// The `&N` a fresh line will claim.
    /// Tab-completion candidates for a line being edited.
    /// Session-aware by construction: the executor answers with
    /// its live arbor when it has one, the syntax tier otherwise.
    pub fn complete(&self, text: &str, cursor: usize) -> Vec<quarb::complete::Candidate> {
        self.executor.complete(text, cursor)
    }

    pub fn line_no(&self) -> usize {
        self.line_no
    }

    /// The macro history text, for a `:history` command.
    pub fn history(&self) -> &str {
        &self.defs_text
    }

    /// Replace the macro history and line counter — restoring a
    /// persisted session (e.g. from the browser's localStorage). Frozen
    /// snapshots are not restored; they regenerate on re-run.
    pub fn restore(&mut self, defs_text: String, line_no: usize) {
        self.defs_text = defs_text;
        self.line_no = line_no.max(1);
    }

    /// Clear the macro history and restart line numbering.
    pub fn reset(&mut self) {
        self.defs_text.clear();
        self.snapshots.clear();
        self.line_no = 1;
        self.persist();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemStore;

    struct NullExec;
    impl Executor for NullExec {
        fn run(&self, _query: &str) -> anyhow::Result<Vec<Cell>> {
            Ok(vec![])
        }
    }

    /// A commented defs file seeds a table the query lexer can take:
    /// the stored text is prepended to every line, and the lexer has
    /// no comment syntax, so `#` lines must not survive seeding.
    #[test]
    fn seed_defs_strips_comments() {
        let mut s = Session::new(Box::new(NullExec), Box::new(MemStore));
        s.seed_defs("# a library header\ndef &a: /x;\n# and a note\ndef &b: /y;")
            .unwrap();
        assert!(!s.history().contains('#'), "history: {}", s.history());
        assert!(s.history().contains("def &a"));
        assert!(s.history().contains("def &b"));
    }
}
