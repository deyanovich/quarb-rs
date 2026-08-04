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
    /// A `--model` file enriching the source with derived structure
    /// (wasm-safe — the playground uses it too).
    model: Option<quarb_model::Model>,
    /// The source spec, kept so a `&N!` reading can re-materialize.
    /// `None` when the source can't be re-opened (wasm pasted text,
    /// which never drifts anyway).
    #[cfg(feature = "native")]
    respec: Option<(Vec<crate::MountSpec>, crate::Options)>,
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
            model: None,
        }
    }

    /// Attach a `--model` file: the session runs every query against
    /// the enriched view.
    pub fn with_model(mut self, model: Option<quarb_model::Model>) -> Self {
        self.model = model;
        self
    }

    /// A native executor that can re-materialize its source for a
    /// `&N!` live reading.
    #[cfg(feature = "native")]
    pub fn with_respec(
        doc: Doc,
        now: (i64, u32),
        allow_shell: bool,
        specs: Vec<crate::MountSpec>,
        opts: crate::Options,
    ) -> Self {
        Self {
            doc,
            now,
            allow_shell,
            respec: Some((specs, opts)),
            model: None,
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

/// [`run_doc`], against a model-enriched view.
fn run_doc_modeled(
    doc: &Doc,
    query: &str,
    now: (i64, u32),
    allow_shell: bool,
    model: &quarb_model::Model,
) -> anyhow::Result<Vec<Cell>> {
    let result = doc
        .run_modeled(query, now, allow_shell, model)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(match result {
        QueryResult::Nodes(nodes) => nodes
            .into_iter()
            .map(|n| Cell::Node(doc.render_modeled(n, model)))
            .collect(),
        QueryResult::Values(values) => values.into_iter().map(Cell::Value).collect(),
    })
}

impl Executor for LocalExecutor {
    fn run(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        if let Some(model) = &self.model {
            return run_doc_modeled(&self.doc, query, self.now, self.allow_shell, model);
        }
        run_doc(&self.doc, query, self.now, self.allow_shell)
    }

    fn run_fresh(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        #[cfg(feature = "native")]
        if let Some((specs, opts)) = &self.respec {
            let fresh = match specs.as_slice() {
                [one] if one.name.is_none() => Doc::open(&one.path, opts)?,
                many => Doc::mount_specs(many, opts)?,
            };
            return run_doc(&fresh, query, self.now, self.allow_shell);
        }
        self.run(query)
    }

    fn complete(&self, text: &str, cursor: usize) -> Vec<quarb::complete::Candidate> {
        quarb::complete::complete_with(text, cursor, self.doc.base_dyn())
    }

    fn export(&self, query: &str, kind: &str) -> anyhow::Result<String> {
        if self.model.is_some() {
            anyhow::bail!("rendered export over a --model session is not supported yet");
        }
        self.doc.export(query, self.now, self.allow_shell, kind)
    }
}
