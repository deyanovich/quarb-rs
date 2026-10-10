//! The conformance suite: what every adapter must keep true, tested
//! over a sample of its nodes. An adapter's own tests call
//! [`check`] on a fixture and assert the list is empty; the trait
//! documentation ([`AstAdapter`]) states the pairs it tests.

use crate::adapter::{AstAdapter, NodeId, PlanAxis, StepPlan};

/// Walk `adapter` breadth-first from its root, at most `budget`
/// nodes, and report every contract the sample breaks: a child's
/// `parent` is its parent; a child `has_node`; a child answers to
/// its own name, is among `children_named` for it and among a child
/// `select` for it; every trait of `traits` `has_trait`; every
/// property name of `properties` is in `property_names`;
/// `provenance_list`'s first entry is `provenance`; a descendant
/// `select` for a child's name (where the adapter answers one)
/// finds exactly what the walk finds, when the walk fits the
/// budget. Empty means the sample conforms.
pub fn check(adapter: &dyn AstAdapter, budget: usize) -> Vec<String> {
    let mut findings = Vec::new();
    let root = adapter.root();
    let mut queue = std::collections::VecDeque::from([root]);
    let mut seen = std::collections::HashSet::new();
    let mut names_seen = std::collections::HashSet::new();
    while let Some(n) = queue.pop_front() {
        if seen.len() >= budget || !seen.insert(n) {
            continue;
        }
        let children = adapter.children(n);
        for &c in &children {
            match adapter.parent(c) {
                Some(p) if p == n => {}
                Some(p) => findings.push(format!(
                    "parent({c:?}) is {p:?}, but {n:?} lists it among its children"
                )),
                None => findings.push(format!(
                    "parent({c:?}) is None, but {n:?} lists it among its children"
                )),
            }
            if !adapter.has_node(c) {
                findings.push(format!("has_node({c:?}) is false for a child of {n:?}"));
            }
            if let Some(name) = adapter.name(c) {
                if !adapter.answers_to(c, &name) {
                    findings.push(format!("{c:?} does not answer to its own name {name:?}"));
                }
                if !adapter.children_named(n, &name).contains(&c) {
                    findings.push(format!(
                        "children_named({n:?}, {name:?}) misses the child {c:?} named so"
                    ));
                }
                let plan = StepPlan {
                    traits: Vec::new(),
                    cap: Default::default(),
                    axis: PlanAxis::Child,
                    name: Some(&name),
                    conjuncts: Vec::new(),
                    limit: None,
                    order: None,
                };
                if let Some(sel) = adapter.select(n, &plan)
                    && !sel.nodes.iter().any(|(x, _)| *x == c)
                {
                    findings.push(format!(
                        "select({n:?}, child {name:?}) misses the child {c:?} named so"
                    ));
                }
                if names_seen.insert(name.clone()) {
                    check_descendant_select(adapter, root, &name, budget, &mut findings);
                }
            }
            queue.push_back(c);
        }
        for t in adapter.traits(n) {
            if !adapter.has_trait(n, &t) {
                findings.push(format!("{n:?} lists trait {t:?} but has_trait denies it"));
            }
        }
        let names = adapter.property_names(n);
        for (p, _) in adapter.properties(n) {
            if !names.contains(&p) {
                findings.push(format!(
                    "{n:?} has property {p:?} but property_names omits it"
                ));
            }
        }
        let list = adapter.provenance_list(n);
        if let Some(first) = list.entries.first()
            && *first != adapter.provenance(n)
        {
            findings.push(format!(
                "provenance_list({n:?}) opens with an entry that is not provenance({n:?})"
            ));
        }
    }
    findings
}

/// A descendant `select` for `name` from `root` against the walk,
/// when the walk stays within `budget`.
fn check_descendant_select(
    adapter: &dyn AstAdapter,
    root: NodeId,
    name: &str,
    budget: usize,
    findings: &mut Vec<String>,
) {
    let plan = StepPlan {
        traits: Vec::new(),
        cap: Default::default(),
        axis: PlanAxis::Descendant,
        name: Some(name),
        conjuncts: Vec::new(),
        limit: None,
        order: None,
    };
    let Some(sel) = adapter.select(root, &plan) else {
        return;
    };
    let mut walked = Vec::new();
    let mut stack = vec![(root, 0usize)];
    let mut visited = 0usize;
    while let Some((n, depth)) = stack.pop() {
        visited += 1;
        if visited > budget {
            return;
        }
        let children = adapter.children(n);
        for &c in children.iter().rev() {
            if adapter.answers_to(c, name) {
                walked.push((c, depth + 1));
            }
            stack.push((c, depth + 1));
        }
    }
    let mut got: Vec<(NodeId, usize)> = sel.nodes;
    got.sort();
    walked.sort();
    if got != walked {
        findings.push(format!(
            "select(root, descendants {name:?}) answers {} node(s), the walk finds {}",
            got.len(),
            walked.len()
        ));
    }
}
