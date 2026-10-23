//! The machine run over bare threads: a query whose every branch is
//! the root with no steps (the calculator's shape, and a body's over
//! values) opens one thread per branch, seeded from the invoking
//! scope, the run's topic on the head, and runs the pipeline. The
//! graph's evaluation takes the same pipeline after navigation and
//! correlation; this entry needs no arbor.
use crate::pipeline::*;

/// Whether a query's shape is the bare one: branches anchored at the
/// root with no steps and no projection, no correlation.
pub(crate) fn is_bare(query: &Query) -> bool {
    query.correlations.is_empty()
        && !query.branches.is_empty()
        && query.branches.iter().all(|b| {
            matches!(b.anchor, Anchor::Root) && b.steps.is_empty() && b.projection.is_none()
        })
}

/// Evaluate a bare query at a host: the threads it ends with, and
/// whether the pipeline projected (value-typed them). `outer` is the
/// invoking scope of a body, `None` at the top level, where the run's
/// topic seeds the head.
pub(crate) fn eval_bare<H: Host>(
    query: &Query,
    host: &H,
    outer: Option<&Scope<'_, H::Ext>>,
) -> (Vec<Thread<H::Ext>>, bool) {
    let trace = H::Trace::default();
    let mut caps: Vec<Thread<H::Ext>> = Vec::new();
    for _ in &query.branches {
        let t = Thread {
            register: outer.map(|s| s.register.to_vec()).unwrap_or_default(),
            topic: None,
            prov: Prov::default(),
            members: Vec::new(),
            captures: outer.map(|s| s.captures.to_vec()).unwrap_or_default(),
            named: outer.map(|s| s.named.to_vec()).unwrap_or_default(),
            ext: host.root_ext(),
        };
        // Branches at the same position with the same register are one
        // thread, as the graph deduplicates them.
        if !caps
            .iter()
            .any(|c| reg_key(&c.register) == reg_key(&t.register))
        {
            caps.push(t);
        }
    }
    if outer.is_none()
        && let Some(seed) = run_option(|o| o.topic.clone())
    {
        for c in caps.iter_mut().filter(|c| c.topic.is_none()) {
            c.topic = Some(seed.clone());
        }
    }
    let at = query.join_at.min(query.pipeline.len());
    let top = outer.is_none();
    caps = apply_commands(&query.pipeline[..at], 0, top, caps, host, &trace, outer);
    caps = apply_commands(&query.pipeline[at..], at, top, caps, host, &trace, outer);
    (caps, pipeline_projected(query))
}

/// The query's static value-vs-node typing, folded over the pipeline.
/// `@| group` and `@| window` set it (list topics); `@| ungroup`
/// restores whatever held before the matching member-maker — its
/// members carry the context as it was then. Other commands set it per
/// [`command_projects`] and never clear it.
pub(crate) fn pipeline_projected(query: &Query) -> bool {
    let mut projected = query.branches.iter().any(|b| b.projection.is_some());
    let mut before_group: Vec<bool> = Vec::new();
    for command in &query.pipeline {
        match command {
            Command::Agg(call) if matches!(call.name.as_str(), "group" | "window") => {
                before_group.push(projected);
                projected = true;
            }
            Command::Agg(call) if call.name == "ungroup" => {
                projected = before_group.pop().unwrap_or(false);
            }
            // A navigation command rebuilds the capsae: without a
            // projection the context is node-typed again no matter
            // what came before; with one it is value-typed.
            Command::Nav(b) => projected = b.projection.is_some(),
            s => projected = projected || command_projects(s),
        }
    }
    projected
}

/// Whether a pipeline command turns the context value-typed. Positional
/// selection, per-capsa filters, and keyed aggregates reorder or
/// filter capsae without producing values; every other command does.
pub(crate) fn command_projects(command: &Command) -> bool {
    match command {
        Command::Select(_) | Command::Filter(_) => false,
        // A push of any spelling appends to the capsa and leaves
        // the cursor untouched (spec: Def Capsa, draft 154): the
        // topic before it is the topic after it, so the runtime
        // any-topic check in [`to_result`] carries the typing; in
        // node context a bare push MARKS and a computed one files
        // its value, neither value-typing the query.
        Command::Push(_)
        | Command::Subcontext { .. }
        | Command::ExprPush { .. }
        | Command::RecordPush { .. }
        | Command::FieldsPush
        | Command::AggPush { .. } => false,
        // A quantified push is its push, round after round.
        Command::Repeat { command, .. } => command_projects(command),
        // A spliced quarq is whatever it holds: the runtime
        // any-topic check types the result.
        Command::Splice(_) => false,
        // the session push files the topic and leaves the cursor untouched
        Command::SessionPush(_) => false,
        // A navigation command re-types by its own projection —
        // handled in the [`pipeline_projected`] fold (it can also
        // RESET a projected pipeline back to nodes).
        Command::Nav(b) => b.projection.is_some(),
        // `group` produces list topics (value-typed); the other keyed
        // aggregates only reorder or filter. (`ungroup` is handled by
        // the fold in [`pipeline_projected`].)
        // An ordering reduction (sort, unique, reverse, first, last)
        // acts on the nodes and yields node threads (spec: Lists,
        // draft 164); over values the runtime any-topic check types
        // the result.
        Command::Agg(call) => {
            !(stdlib::known_keyed(&call.name)
                || matches!(
                    call.name.as_str(),
                    "sort" | "unique" | "reverse" | "first" | "last"
                ))
                || call.name == "group"
        }
        _ => true,
    }
}
