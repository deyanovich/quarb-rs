//! A run's options as the tool sets them, and the graph's state for
//! one run: the declared references' targets, the memo of scopes
//! and links, the children cache. The pipeline keeps its own run
//! state below; entering a run here enters the pipeline's too.
use super::*;
use crate::graph::plan::Budget;
use crate::pipeline::{RunGuard, RunOptions, begin_run};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// What a run is given: the pinned instant, the shell gate, the
/// quantifier bound, reproducibility, the declared references'
/// targets, the collation, the sentence bonds, the budget, the plan
/// switch, the head's topic, the environments and the libraries.
#[derive(Clone, Default)]
pub struct Run {
    /// The session memory (`| ,name`, `$,name`, `%,`), the host's.
    pub session: Option<crate::pipeline::SessionMemory>,
    pub now: Option<(i64, u32)>,
    pub allow_shell: bool,
    pub quantifier_bound: Option<usize>,
    pub reproducible: bool,
    pub ref_targets: HashMap<String, NodeId>,
    pub collation: Option<String>,
    pub bonds: Option<Rc<dyn crate::value::ambient::SentenceBonds>>,
    pub budget: Option<Budget>,
    pub plan: bool,
    pub topic: Option<Value>,
    pub environments: Vec<crate::pipeline::env::Environment>,
    pub libraries: Vec<std::sync::Arc<dyn crate::pipeline::library::CommandLibrary>>,
}

impl Run {
    /// The session's pins and nothing else.
    pub fn ambient() -> Run {
        Run {
            now: crate::value::ambient::invocation_instant(),
            bonds: crate::value::ambient::sentence_bonds(),
            ..Run::default()
        }
    }

    /// The pipeline's share of the options.
    pub fn options(&self) -> RunOptions {
        RunOptions {
            session: self.session.clone(),
            now: self.now,
            allow_shell: self.allow_shell,
            quantifier_bound: self.quantifier_bound,
            reproducible: self.reproducible,
            collation: self.collation.clone(),
            bonds: self.bonds.clone(),
            topic: self.topic.clone(),
            environments: self.environments.clone(),
            libraries: self.libraries.clone(),
        }
    }
}

/// The graph's state of one run.
#[derive(Default)]
pub(crate) struct GraphRun {
    pub(crate) run: Run,
    pub(crate) scopes: HashMap<(usize, String), Rc<ScopeMemo>>,
    pub(crate) last_out: LastLinks,
    pub(crate) last_in: LastLinks,
    pub(crate) children: HashMap<NodeId, Rc<Vec<NodeId>>>,
}

thread_local! {
    static RUNS: RefCell<Vec<GraphRun>> = const { RefCell::new(Vec::new()) };
}

/// A run in progress; dropping it ends the run, the pipeline's with
/// it.
pub(crate) struct GraphGuard {
    _pipeline: RunGuard,
}

impl Drop for GraphGuard {
    fn drop(&mut self) {
        RUNS.with(|r| r.borrow_mut().pop());
    }
}

/// Enter a run: with the options given, else the enclosing run's,
/// else the ambient.
pub(crate) fn enter(run: Option<Run>) -> GraphGuard {
    let run = run.unwrap_or_else(inherited);
    let pipeline = begin_run(Some(run.options()));
    RUNS.with(|r| {
        r.borrow_mut().push(GraphRun {
            run,
            ..GraphRun::default()
        })
    });
    GraphGuard {
        _pipeline: pipeline,
    }
}

/// The run in progress, or the ambient.
pub fn inherited() -> Run {
    RUNS.with(|r| r.borrow().last().map(|g| g.run.clone()))
        .unwrap_or_else(Run::ambient)
}

pub(crate) fn with_graph_run<R>(f: impl FnOnce(&mut GraphRun) -> R) -> R {
    RUNS.with(|r| {
        let mut r = r.borrow_mut();
        match r.last_mut() {
            Some(top) => f(top),
            None => f(&mut GraphRun::default()),
        }
    })
}

/// The node a declared reference's identity resolves to.
pub(crate) fn ref_target(id: &str) -> Option<NodeId> {
    with_graph_run(|g| g.run.ref_targets.get(id).copied())
}
