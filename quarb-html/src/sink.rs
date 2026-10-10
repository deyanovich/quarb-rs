//! The parser's tree sink, writing the arena's shape directly: one
//! pass over html5ever's events into a flat node list (an element's
//! tag and attributes interned, its content as text pieces and
//! child handles in order), then one walk into the adapter's arena
//! in document order. No DOM stands beside the arena at the peak.

use html5ever::tendril::StrTendril;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, ExpandedName, QualName};
use quarb::names::Names;
use std::borrow::Cow;

/// A node of the sink: the document, an element, or a comment /
/// processing instruction (kept as a handle, skipped in the walk).
pub(crate) struct SinkNode {
    pub name: Option<QualName>,
    pub attrs: Vec<(u32, String)>,
    pub kids: Vec<Kid>,
    pub parent: Option<usize>,
    pub skip: bool,
}

pub(crate) enum Kid {
    Text(String),
    Node(usize),
}

pub(crate) struct Sink {
    pub nodes: Vec<SinkNode>,
    pub names: Names,
}

impl Sink {
    pub(crate) fn new() -> Sink {
        Sink {
            nodes: vec![SinkNode {
                name: None,
                attrs: Vec::new(),
                kids: Vec::new(),
                parent: None,
                skip: false,
            }],
            names: Names::new(),
        }
    }

    fn push(&mut self, name: Option<QualName>, attrs: Vec<(u32, String)>, skip: bool) -> usize {
        self.nodes.push(SinkNode {
            name,
            attrs,
            kids: Vec::new(),
            parent: None,
            skip,
        });
        self.nodes.len() - 1
    }

    fn detach(&mut self, id: usize) {
        if let Some(p) = self.nodes[id].parent.take() {
            self.nodes[p]
                .kids
                .retain(|k| !matches!(k, Kid::Node(n) if *n == id));
        }
    }

    fn append_text(kids: &mut Vec<Kid>, at: usize, text: &str) {
        // Adjacent text concatenates, as the tree builder asks.
        if at > 0
            && let Some(Kid::Text(t)) = kids.get_mut(at - 1)
        {
            t.push_str(text);
            return;
        }
        kids.insert(at, Kid::Text(text.to_string()));
    }
}

impl TreeSink for Sink {
    type Output = Sink;
    type Handle = usize;

    fn finish(self) -> Sink {
        self
    }

    fn parse_error(&mut self, _msg: Cow<'static, str>) {}

    fn set_quirks_mode(&mut self, _mode: QuirksMode) {}

    fn get_document(&mut self) -> usize {
        0
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    fn elem_name(&self, target: &usize) -> ExpandedName<'_> {
        self.nodes[*target]
            .name
            .as_ref()
            .expect("an element")
            .expanded()
    }

    fn create_element(
        &mut self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> usize {
        let attrs = attrs
            .into_iter()
            .map(|a| (self.names.intern(&a.name.local), a.value.to_string()))
            .collect();
        self.push(Some(name), attrs, false)
    }

    fn create_comment(&mut self, _text: StrTendril) -> usize {
        self.push(None, Vec::new(), true)
    }

    fn create_pi(&mut self, _target: StrTendril, _data: StrTendril) -> usize {
        self.push(None, Vec::new(), true)
    }

    fn append_doctype_to_document(&mut self, _n: StrTendril, _p: StrTendril, _s: StrTendril) {}

    fn get_template_contents(&mut self, target: &usize) -> usize {
        // No fragment of its own: a template's content hangs under
        // the template element.
        *target
    }

    fn append(&mut self, parent: &usize, child: NodeOrText<usize>) {
        let p = *parent;
        match child {
            NodeOrText::AppendNode(id) => {
                self.detach(id);
                self.nodes[id].parent = Some(p);
                self.nodes[p].kids.push(Kid::Node(id));
            }
            NodeOrText::AppendText(text) => {
                let at = self.nodes[p].kids.len();
                Self::append_text(&mut self.nodes[p].kids, at, &text);
            }
        }
    }

    fn append_before_sibling(&mut self, sibling: &usize, new_node: NodeOrText<usize>) {
        let Some(p) = self.nodes[*sibling].parent else {
            return;
        };
        if let NodeOrText::AppendNode(id) = &new_node {
            self.detach(*id);
        }
        let Some(at) = self.nodes[p]
            .kids
            .iter()
            .position(|k| matches!(k, Kid::Node(n) if n == sibling))
        else {
            return;
        };
        match new_node {
            NodeOrText::AppendNode(id) => {
                self.nodes[id].parent = Some(p);
                self.nodes[p].kids.insert(at, Kid::Node(id));
            }
            NodeOrText::AppendText(text) => {
                Self::append_text(&mut self.nodes[p].kids, at, &text);
            }
        }
    }

    fn append_based_on_parent_node(
        &mut self,
        element: &usize,
        prev_element: &usize,
        child: NodeOrText<usize>,
    ) {
        if self.nodes[*element].parent.is_some() {
            self.append_before_sibling(element, child)
        } else {
            self.append(prev_element, child)
        }
    }

    fn remove_from_parent(&mut self, target: &usize) {
        self.detach(*target);
    }

    fn reparent_children(&mut self, node: &usize, new_parent: &usize) {
        let kids = std::mem::take(&mut self.nodes[*node].kids);
        for k in &kids {
            if let Kid::Node(n) = k {
                self.nodes[*n].parent = Some(*new_parent);
            }
        }
        self.nodes[*new_parent].kids.extend(kids);
    }

    fn add_attrs_if_missing(&mut self, target: &usize, attrs: Vec<Attribute>) {
        for a in attrs {
            let k = self.names.intern(&a.name.local);
            if !self.nodes[*target].attrs.iter().any(|(n, _)| *n == k) {
                self.nodes[*target].attrs.push((k, a.value.to_string()));
            }
        }
    }

    fn mark_script_already_started(&mut self, _node: &usize) {}

    fn pop(&mut self, _node: &usize) {}
}
