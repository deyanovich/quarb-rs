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
    /// A bound every line is held to before it runs: the executor
    /// plans the line and a plan past the bound is refused unrun.
    budget: Option<quarb::plan::Budget>,
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
            budget: None,
        }
    }

    /// Hold every line to a budget (`None` lifts it): the executor
    /// plans the line first, and a line whose estimate passes the
    /// bound is refused before anything is read.
    pub fn set_budget(&mut self, budget: Option<quarb::plan::Budget>) {
        self.budget = budget;
    }

    /// The budget the session holds its lines to, if any.
    pub fn budget(&self) -> Option<&quarb::plan::Budget> {
        self.budget.as_ref()
    }

    /// The static plan of a line — history refs resolved as in
    /// [`eval`](Self::eval) — priced by the executor, under the
    /// session's budget when one is set.
    pub fn plan(&self, line: &str) -> Result<quarb::plan::Plan> {
        self.executor
            .plan(&self.combined(line), self.budget.as_ref())
    }

    /// The plan of a line as its kaiv payload, the line as its
    /// `query` value.
    pub fn plan_text(&self, line: &str) -> Result<String> {
        Ok(self.plan(line)?.to_kaiv(line))
    }

    /// The budget's check of a combined query: nothing to do without
    /// one; with one, the plan past the bound is the refusal.
    fn within_budget(&self, query: &str) -> Result<()> {
        if let Some(b) = &self.budget {
            let plan = self.executor.plan(query, Some(b))?;
            if let Some(why) = b.exceeded_by(&plan) {
                return Err(quarb::QuarbError::refused(why).into());
            }
        }
        Ok(())
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
    /// a query line, a definition is not run — but it is a cell of the
    /// session and takes its ordinal (spec: .quai, Numbering): the line
    /// number advances, with no result behind it, so `&N` of a
    /// definition is a reference to a cell with no result and `&N#`
    /// replays nothing.
    pub fn add_def(&mut self, line: &str) -> Result<()> {
        let candidate = format!("{}{}\n", self.defs_text, line);
        quarb::parse_defs(&candidate).context("parsing definition")?;
        self.defs_text = candidate;
        self.snapshots.insert(self.line_no, Vec::new());
        self.line_no += 1;
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
        let q = self.combined(line);
        self.within_budget(&q)?;
        self.executor.run(&q)
    }

    /// Evaluate a line, and when its closing command is `@| pretty`,
    /// hand back the HTML grid beside the cells (the text table as
    /// one cell) — the browser hosts' path; see
    /// [`crate::Executor::run_table`].
    pub fn eval_table(&self, line: &str) -> Result<(Vec<Cell>, Option<String>)> {
        let q = self.combined(line);
        self.within_budget(&q)?;
        if quarb::closes_with_table(&q) {
            self.executor.run_table(&q)
        } else {
            Ok((self.executor.run(&q)?, None))
        }
    }

    /// The unresolved external references the last evaluation
    /// recorded — see [`crate::Executor::refs`]. The host closes
    /// the loop: acquire, remount, re-run.
    pub fn refs(&self) -> Vec<String> {
        self.executor.refs()
    }

    /// The warnings and infos the last evaluation raised beside its
    /// answer (spec: Diagnostics), in raising order.
    pub fn diagnostics(&self) -> Vec<quarb::Diagnostic> {
        self.executor.diagnostics()
    }

    /// The diagnostics level the session shows
    /// (`session(--diagnostics=…)`; warning when the session keeps
    /// no memory).
    pub fn diagnostics_level(&self) -> quarb::Severity {
        self.session_memory()
            .map(|m| m.borrow().diagnostics)
            .unwrap_or(quarb::Severity::Warning)
    }

    /// The session memory, when the executor keeps one.
    pub fn session_memory(&self) -> Option<quarb::SessionMemory> {
        self.executor.session_memory()
    }

    /// Evaluate a line against a freshly re-materialized source — the
    /// `&N!` live reading, which sees current data.
    pub fn eval_fresh(&self, line: &str) -> Result<Vec<Cell>> {
        let q = self.combined(line);
        self.within_budget(&q)?;
        self.executor.run_fresh(&q)
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
