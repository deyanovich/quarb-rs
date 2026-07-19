//! The in-process executor: materialize and query a [`Doc`] here.
//!
//! Serves both native (the full adapter fleet) and wasm (the
//! text-format subset) — the two differ only in which `Doc` variants
//! compile. A native executor may also carry its source spec, so a
//! `&N!` live reading can re-open the source and see current data.

use crate::doc::Doc;
use crate::{Cell, Executor};
use quarb::QueryResult;

pub struct LocalExecutor {
    doc: Doc,
    /// The invocation instant `now()` denotes, bound once per session.
    now: (i64, u32),
    allow_shell: bool,
    /// The source spec, kept so a `&N!` reading can re-materialize.
    /// `None` when the source can't be re-opened (wasm pasted text,
    /// which never drifts anyway).
    #[cfg(feature = "native")]
    respec: Option<(Vec<std::path::PathBuf>, crate::Options)>,
}

impl LocalExecutor {
    /// An executor over a fixed materialized `Doc` (no live re-read).
    pub fn new(doc: Doc, now: (i64, u32), allow_shell: bool) -> Self {
        Self {
            doc,
            now,
            allow_shell,
            #[cfg(feature = "native")]
            respec: None,
        }
    }

    /// A native executor that can re-materialize its source for a
    /// `&N!` live reading.
    #[cfg(feature = "native")]
    pub fn with_respec(
        doc: Doc,
        now: (i64, u32),
        allow_shell: bool,
        paths: Vec<std::path::PathBuf>,
        opts: crate::Options,
    ) -> Self {
        Self {
            doc,
            now,
            allow_shell,
            respec: Some((paths, opts)),
        }
    }
}

/// Run one query against a `Doc`, rendering its result to [`Cell`]s.
fn run_doc(doc: &Doc, query: &str, now: (i64, u32), allow_shell: bool) -> anyhow::Result<Vec<Cell>> {
    let result = doc
        .run(query, now, allow_shell)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(match result {
        QueryResult::Nodes(nodes) => nodes.into_iter().map(|n| Cell::Node(doc.render(n))).collect(),
        QueryResult::Values(values) => values.into_iter().map(Cell::Value).collect(),
    })
}

impl Executor for LocalExecutor {
    fn run(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        run_doc(&self.doc, query, self.now, self.allow_shell)
    }

    fn run_fresh(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        #[cfg(feature = "native")]
        if let Some((paths, opts)) = &self.respec {
            let fresh = match paths.as_slice() {
                [one] => Doc::open(one, opts)?,
                many => Doc::mount(many, opts)?,
            };
            return run_doc(&fresh, query, self.now, self.allow_shell);
        }
        self.run(query)
    }
}
