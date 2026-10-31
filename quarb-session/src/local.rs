//! The in-process executor: materialize and query a [`Doc`] here.
//!
//! Serves both native (the full adapter fleet) and wasm (the
//! text-format subset) — the two differ only in which `Doc` variants
//! compile. A native executor may also carry its source spec, so a
//! `&N!` live reading can re-open the source and see current data.

use crate::doc::Doc;
use crate::{Cell, Executor};
use quarb::QueryResult;
use quarb::Value;

pub struct LocalExecutor {
    doc: Doc,
    /// The reference targets: external-reference identifier
    /// (fragment-stripped URL, for html) → mounted root, fed to
    /// the engine before each run so `::href-->` can land on a
    /// sibling source. See [`Doc::url_roots`].
    url_roots: std::collections::HashMap<String, quarb::NodeId>,
    /// The unresolved external references of the last run —
    /// documents queried but not mounted. The host may acquire
    /// them, remount, and re-run; the executor only reports.
    refs: std::cell::RefCell<Vec<String>>,
    /// The diagnostics the last run raised (spec: Diagnostics).
    diagnostics: std::cell::RefCell<Vec<quarb::Diagnostic>>,
    /// The invocation instant `now()` denotes, bound once per session.
    now: (i64, u32),
    allow_shell: bool,
    /// The environments the host mounted (`go=exec:DIR`,
    /// `sh=shell:`), the namespaces `ALIAS:name(…)` reaches.
    environments: Vec<quarb::Environment>,
    /// A `--model` file enriching the source with derived structure
    /// (wasm-safe — the playground uses it too).
    model: Option<quarb_model::Model>,
    /// The session memory (`| ,name`, `$,name`, `%,`): one per
    /// executor, outliving every query it runs.
    session: quarb::SessionMemory,
    /// The source spec, kept so a `&N!` reading can re-materialize.
    /// `None` when the source can't be re-opened (wasm pasted text,
    /// which never drifts anyway).
    #[cfg(feature = "native")]
    respec: Option<(Vec<crate::MountSpec>, crate::Options)>,
}

impl LocalExecutor {
    /// The executor sharing a session memory with another (a remount
    /// keeps what the session filed).
    pub fn with_session(mut self, session: quarb::SessionMemory) -> Self {
        self.session = session;
        self
    }

    /// The session memory this executor runs with.
    pub fn session(&self) -> quarb::SessionMemory {
        self.session.clone()
    }

    /// An executor over a fixed materialized `Doc` (no live re-read).
    pub fn new(doc: Doc, now: (i64, u32), allow_shell: bool) -> Self {
        Self {
            doc,
            url_roots: Default::default(),
            refs: Default::default(),
            diagnostics: Default::default(),
            now,
            allow_shell,
            environments: Vec::new(),
            #[cfg(feature = "native")]
            respec: None,
            model: None,
            session: quarb::session_memory(),
        }
    }

    /// The environments mounted for the session; a `shell:` one
    /// opens the shell gate as `--allow-shell` does.
    pub fn with_environments(mut self, environments: Vec<quarb::Environment>) -> Self {
        if environments.iter().any(|e| e.kind == quarb::EnvKind::Shell) {
            self.allow_shell = true;
        }
        self.environments = environments;
        self
    }

    /// Register the reference targets (identifier → mounted root)
    /// the arrow may land on. See [`Doc::url_roots`].
    pub fn with_url_roots(mut self, map: std::collections::HashMap<String, quarb::NodeId>) -> Self {
        self.url_roots = map;
        self
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
            url_roots: Default::default(),
            refs: Default::default(),
            diagnostics: Default::default(),
            now,
            allow_shell,
            environments: Vec::new(),
            respec: Some((specs, opts)),
            model: None,
            session: quarb::session_memory(),
        }
    }
}

/// Run one query against a `Doc`, rendering its result to [`Cell`]s
/// and reporting the unresolved references.
type Ran = (Vec<Cell>, Vec<String>, Vec<quarb::Diagnostic>);

fn run_doc(doc: &Doc, query: &str, run: &quarb::Run) -> anyhow::Result<Ran> {
    let out = doc.run(query, run).map_err(anyhow::Error::new)?;
    let cells = match out.result {
        QueryResult::Nodes(nodes) => nodes
            .into_iter()
            .map(|n| Cell::Node(doc.render(n)))
            .collect(),
        QueryResult::Values(values) => values.into_iter().map(Cell::Value).collect(),
    };
    Ok((cells, out.refs, out.diagnostics))
}

/// [`run_doc`], against a model-enriched view.
fn run_doc_modeled(
    doc: &Doc,
    query: &str,
    run: &quarb::Run,
    model: &quarb_model::Model,
) -> anyhow::Result<Ran> {
    let out = doc
        .run_modeled(query, run, model)
        .map_err(anyhow::Error::new)?;
    let cells = match out.result {
        QueryResult::Nodes(nodes) => nodes
            .into_iter()
            .map(|n| Cell::Node(doc.render_modeled(n, model)))
            .collect(),
        QueryResult::Values(values) => values.into_iter().map(Cell::Value).collect(),
    };
    Ok((cells, out.refs, out.diagnostics))
}

impl LocalExecutor {
    /// The options every query of this session runs under: the
    /// session's instant and shell gate, the registered reference
    /// targets, the ambient bonds.
    fn run_opts(&self) -> quarb::Run {
        quarb::Run {
            now: Some(self.now),
            allow_shell: self.allow_shell,
            environments: self.environments.clone(),
            ref_targets: self.url_roots.clone(),
            session: Some(self.session.clone()),
            debug: self.session.borrow().diagnostics == quarb::Severity::Debug,
            ..quarb::Run::ambient()
        }
    }
}

impl Executor for LocalExecutor {
    fn session_memory(&self) -> Option<quarb::SessionMemory> {
        Some(self.session.clone())
    }
    fn run(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        let run = self.run_opts();
        let (cells, refs, diagnostics) = if let Some(model) = &self.model {
            run_doc_modeled(&self.doc, query, &run, model)?
        } else {
            run_doc(&self.doc, query, &run)?
        };
        *self.refs.borrow_mut() = refs;
        *self.diagnostics.borrow_mut() = diagnostics;
        Ok(cells)
    }

    fn refs(&self) -> Vec<String> {
        self.refs.borrow().clone()
    }

    fn diagnostics(&self) -> Vec<quarb::Diagnostic> {
        self.diagnostics.borrow().clone()
    }

    fn run_fresh(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        #[cfg(feature = "native")]
        if let Some((specs, opts)) = &self.respec {
            let fresh = match specs.as_slice() {
                [one] if one.name.is_none() => Doc::open(&one.path, opts)?,
                many => Doc::mount_specs(many, opts)?,
            };
            return run_doc(&fresh, query, &self.run_opts()).map(|(cells, _, _)| cells);
        }
        self.run(query)
    }

    fn complete(&self, text: &str, cursor: usize) -> Vec<quarb::complete::Candidate> {
        quarb::complete::complete_with(text, cursor, self.doc.base_dyn())
    }

    fn run_table(&self, query: &str) -> anyhow::Result<(Vec<Cell>, Option<String>)> {
        if self.model.is_some() {
            return Ok((self.run(query)?, None));
        }
        let run = self.run_opts();
        let out = self
            .doc
            .run_rendering(query, &run)
            .map_err(anyhow::Error::new)?;
        *self.refs.borrow_mut() = out.refs;
        *self.diagnostics.borrow_mut() = out.diagnostics;
        let Some(closing) = out.closing.filter(|c| !c.csv) else {
            // Not a table after all (a csv, or no closing command):
            // the rows as the run would have printed them.
            let cells = match out.result {
                QueryResult::Nodes(nodes) => nodes
                    .into_iter()
                    .map(|n| Cell::Node(self.doc.render(n)))
                    .collect(),
                QueryResult::Values(values) => values.into_iter().map(Cell::Value).collect(),
            };
            return Ok((cells, None));
        };
        let rows = self.doc.table_rows(out.result, &closing.columns);
        let columns = (!closing.columns.is_empty()).then_some(closing.columns.as_slice());
        let text = quarb::tabulate_with(&rows, columns, false, b',');
        let html = quarb::table_html_plain(&rows, columns);
        Ok((vec![Cell::Value(Value::Str(text))], Some(html)))
    }

    fn plan(
        &self,
        query: &str,
        budget: Option<&quarb::plan::Budget>,
    ) -> anyhow::Result<quarb::plan::Plan> {
        if self.model.is_some() {
            anyhow::bail!("planning over a --model session is not supported yet");
        }
        Ok(self.doc.plan(query, &self.run_opts(), budget)?)
    }

    fn export(&self, query: &str, kind: &str) -> anyhow::Result<String> {
        if self.model.is_some() {
            anyhow::bail!("rendered export over a --model session is not supported yet");
        }
        self.doc.export(query, &self.run_opts(), kind)
    }
}
