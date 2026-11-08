//! The unit suffix convention on demand (spec: Mounts, the autounits
//! option; ruling 2026-11-08, the 0.57 round): `?autounits` on any
//! target reads every property whose last `_` segment names a unit
//! the run knows — `body_mass_g`, `latency_ms`, `масса_кг` — as the
//! property without the suffix holding quantities in that unit;
//! `?autounit=_km,_g` converts the named suffixes only. Never on by
//! default: an unknown suffix reads as an opaque unit in query text,
//! so a convention that fired by itself would turn `created_at` into
//! attotonnes and flip `::body_mass_g > 4000` silently (lang's
//! corpus scan, 2026-11-07).
//!
//! The suffix is matched byte for byte against the run's unit table —
//! the built-ins, the GOST designations, a model's `unit`
//! declarations — as spaced unit text, so no opaque unit qualifies
//! and a spelling the grammar refuses (`mb`) is skipped, not an
//! error. A numeric cell or numeric text is minted in the unit; a
//! quantity cell of the dimension keeps its base value and is shown
//! in the unit; null stays null; any other cell reads as before. The
//! suffixed name no longer answers. Two columns stripping to one
//! name, or a stripped name the rows already carry, refuse. The
//! conversions are told under `--info`, once per header.
//!
//! Two suffixes are temporal, not unital (ruling 2026-11-08): `_at`
//! types the column as instants, `_on` as dates at day precision —
//! in place, the name kept (`::created_at`, since `::created` would
//! read as a boolean) — a text cell by the ISO reading, a number by
//! the one epoch rule (seconds; milliseconds from 10^11 up, as the
//! `<instant>` cast and `datetime` read it). Under the global flag a
//! short list of English
//! words a column's tail uses is never read as a unit (`in` is the
//! preposition there, not the inch); the named form converts what
//! it names. Either way a column converts only when the deciding
//! row's cell is a number, numeric text or a reading of the kind;
//! a text cell that is none of these leaves the column as it is.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::graph::adapter::{AstAdapter, Conjunct, Estimate, Selection, StepPlan};
use crate::value::quantity::{is_name_char, parse_unit_text, unscale_by};
use crate::{Diagnostic, NodeId, Value};

/// Which suffixes a mount converts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suffixes {
    /// `?autounits`: every suffix the run's unit table knows.
    Known,
    /// `?autounit=_km,_g`: the named ones, held without their
    /// underscore (`km`, `g`).
    Named(Vec<String>),
}

impl Suffixes {
    /// The union of two option values on one target: the flag wins
    /// over any list, two lists merge.
    pub fn merge(self, other: Suffixes) -> Suffixes {
        match (self, other) {
            (Suffixes::Known, _) | (_, Suffixes::Known) => Suffixes::Known,
            (Suffixes::Named(mut a), Suffixes::Named(b)) => {
                for s in b {
                    if !a.contains(&s) {
                        a.push(s);
                    }
                }
                Suffixes::Named(a)
            }
        }
    }

    /// The option as written, for messages: `autounits` or
    /// `autounit=_km,_g`.
    fn spelled(&self) -> String {
        match self {
            Suffixes::Known => "autounits".to_string(),
            Suffixes::Named(list) => format!(
                "autounit={}",
                list.iter()
                    .map(|s| format!("_{s}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

/// One named suffix as the option writes it, `_km`: the underscore
/// a quantity writes (`12_km`), then a unit name. The name without
/// its underscore; a spelling without one, or shaped as no unit, is
/// refused with the pointer at the spelling.
pub fn named_suffix(text: &str) -> Result<String, String> {
    let text = text.trim();
    let Some(u) = text.strip_prefix('_') else {
        return Err(if text.is_empty() {
            "autounit= names no suffix: the suffix of 'length_km' is '_km' ('autounit=_km'); \
             every known suffix is 'autounits'"
                .to_string()
        } else {
            format!(
                "autounit={text}: a suffix is written with its underscore, as the column \
                 carries it — the suffix of 'length_{text}' is '_{text}' ('autounit=_{text}')"
            )
        });
    };
    if u.is_empty()
        || !u.chars().next().is_some_and(is_name_char)
        || !u.chars().all(|c| is_name_char(c) || c.is_ascii_digit())
    {
        return Err(format!(
            "autounit=_{u}: '{u}' is shaped as no unit (a unit is letters, as 'km' or 'кг')"
        ));
    }
    Ok(u.to_string())
}

/// How a converted column reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// In the unit: `body_mass_g` → `body_mass` in g.
    Unit(String),
    /// As instants: `created_at` → `created`.
    Instant,
    /// As dates, day precision: `published_on` → `published`.
    Date,
}

impl Reading {
    /// Whether the reading keeps the property's name: the temporal
    /// ones type the column in place, a unit renames.
    fn in_place(&self) -> bool {
        !matches!(self, Reading::Unit(_))
    }

    /// The info line's entry for one column: `length_km → length in
    /// km`, `created_at: instants (epoch seconds)`, `published_on:
    /// dates`.
    fn told(&self, name: &str, stem: &str, cell: Option<&Value>) -> String {
        match self {
            Reading::Unit(u) => format!("{name} → {stem} in {u}"),
            Reading::Instant | Reading::Date => {
                let kind = if matches!(self, Reading::Instant) {
                    "instants"
                } else {
                    "dates"
                };
                let epoch = match cell {
                    Some(Value::Int(n)) => Some(*n as f64),
                    Some(Value::Float(x)) => Some(*x),
                    Some(Value::Str(s)) => s.trim().parse::<f64>().ok(),
                    _ => None,
                };
                match epoch {
                    Some(x) if x.abs() >= crate::value::EPOCH_MILLIS_FROM => {
                        format!("{name}: {kind} (epoch milliseconds)")
                    }
                    Some(_) => format!("{name}: {kind} (epoch seconds)"),
                    None => format!("{name}: {kind}"),
                }
            }
        }
    }
}

/// The temporal suffixes and their readings.
const TEMPORAL: &[(&str, Reading)] = &[("at", Reading::Instant), ("on", Reading::Date)];

/// The English words the global flag never reads as a unit: a
/// column's tail says a preposition there, never the attotonne (`at`)
/// or the inch (`in`). The named form converts what it names, stop
/// list or not. Checked against the table by `stop_list_covers_the_
/// table_words`.
const STOP: &[&str] = &[
    "a", "an", "at", "in", "on", "by", "to", "of", "as", "is", "it", "or", "no", "so", "do", "if",
    "am", "pm", "up", "for", "per",
];

/// A text cell as an instant: the ISO reading, extended or compact.
fn text_instant(s: &str) -> Option<Value> {
    let t = s.trim();
    crate::temporal::parse_iso(t)
        .or_else(|| crate::temporal::parse_iso_compact(t))
        .map(|(secs, nanos, offset_min)| Value::Instant {
            secs,
            nanos,
            offset_min,
            calendar: Default::default(),
        })
}

/// An instant at the start of its UTC day, no offset: what a date
/// column holds (a midnight instant with no offset prints as a bare
/// date).
fn to_date(v: Value) -> Value {
    match v {
        Value::Instant { secs, .. } => Value::Instant {
            secs: secs - secs.rem_euclid(86_400),
            nanos: 0,
            offset_min: None,
            calendar: Default::default(),
        },
        other => other,
    }
}

/// The scale of one unit name under the run's table: the factor to
/// the base and the base expansion, as spaced unit text reads it (no
/// opaque unit). `None` for a name that is no unit, or a
/// dimensionless one.
fn scale_of(unit: &str) -> Option<(f64, String)> {
    // a spelling the grammar refuses in query text (`mb`: no fraction
    // of a bit) is no suffix either
    if crate::value::quantity::misprefixed(unit).is_some() {
        return None;
    }
    parse_unit_text(&format!("1 {unit}")).map(|(v, base, _, _)| (v, base))
}

/// The conversions decided for one list of property names.
#[derive(Debug, Default)]
struct Plan {
    /// The stripped name → the source property and its reading.
    by_new: HashMap<String, (String, Reading)>,
    /// The source property → the stripped name.
    by_source: HashMap<String, String>,
    /// Per unit, its factor to the base and the base.
    scales: HashMap<String, (f64, String)>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.by_new.is_empty()
    }

    /// Whether a plan's property names touch a converted column —
    /// under its new name, or under the suffixed name that answers no
    /// more.
    fn touches(&self, plan: &StepPlan<'_>) -> bool {
        let names = plan
            .conjuncts
            .iter()
            .map(|c| match c {
                Conjunct::Equals(n, _) | Conjunct::Compare(n, _, _) => *n,
            })
            .chain(plan.order.map(|(n, _)| n));
        for n in names {
            if self.by_new.contains_key(n) || self.by_source.contains_key(n) {
                return true;
            }
        }
        false
    }

    /// A cell of a converted column as the property answers it.
    fn convert(&self, v: Value, reading: &Reading) -> Value {
        let unit = match reading {
            Reading::Unit(u) => u,
            Reading::Instant | Reading::Date => {
                let inst = match v {
                    Value::Null => Value::Null,
                    Value::Int(n) => Value::from_epoch(n as f64),
                    Value::Float(x) => Value::from_epoch(x),
                    Value::Str(s) => match s.trim().parse::<f64>() {
                        Ok(x) => Value::from_epoch(x),
                        Err(_) => text_instant(&s).unwrap_or(Value::Str(s)),
                    },
                    other => other,
                };
                return match reading {
                    Reading::Date => to_date(inst),
                    _ => inst,
                };
            }
        };
        let Some((f, base)) = self.scales.get(unit) else {
            return v;
        };
        let mint = |n: f64| Value::Quantity {
            value: n * f,
            base: base.clone(),
            written: Some((n, unit.to_string())),
        };
        match v {
            Value::Null => Value::Null,
            Value::Int(n) => mint(n as f64),
            Value::Float(x) => mint(x),
            Value::Str(s) => match s.trim().parse::<f64>() {
                Ok(n) => mint(n),
                Err(_) => match parse_unit_text(&s) {
                    Some((value, held, _, _)) if &held == base => Value::Quantity {
                        value,
                        base: base.clone(),
                        written: Some((unscale_by(value, *f), unit.to_string())),
                    },
                    _ => Value::Str(s),
                },
            },
            Value::Quantity {
                value, base: held, ..
            } if &held == base => Value::Quantity {
                value,
                base: base.clone(),
                written: Some((unscale_by(value, *f), unit.to_string())),
            },
            other => other,
        }
    }
}

/// The adapter `inner` under the convention: node ids are its own,
/// the properties of every node read through the plan its names
/// decide.
pub struct AutoUnits<A: AstAdapter> {
    pub inner: A,
    suffixes: Suffixes,
    /// The target as written, without its options, for the info
    /// line and the refusals.
    target: String,
    /// Per distinct list of property names, the plan: a table's rows
    /// share one header, so one entry serves them all; a document's
    /// nodes each bring their own.
    plans: RefCell<HashMap<Vec<String>, Rc<Plan>>>,
    /// Per node whose rows are asked for, the header beneath it.
    headers: RefCell<HashMap<NodeId, Option<(NodeId, Vec<String>)>>>,
    /// Per parent of rows with a shape, their plan: a table's rows
    /// share one header, so one entry serves them all and no row
    /// read allocates the header to find it.
    by_parent: RefCell<HashMap<NodeId, Rc<Plan>>>,
}

impl<A: AstAdapter> AutoUnits<A> {
    pub fn new(inner: A, suffixes: Suffixes, target: &str) -> Self {
        AutoUnits {
            inner,
            suffixes,
            target: target.to_string(),
            plans: RefCell::new(HashMap::new()),
            headers: RefCell::new(HashMap::new()),
            by_parent: RefCell::new(HashMap::new()),
        }
    }

    /// The plan for one list of names, decided once; the first
    /// decision tells the conversions (an info) or refuses a
    /// collision.
    fn plan_for(&self, names: &[String], sample: &dyn Fn(&str) -> Option<Value>) -> Rc<Plan> {
        if let Some(p) = self.plans.borrow().get(names) {
            return p.clone();
        }
        let plan = Rc::new(self.decide(names, sample));
        self.plans.borrow_mut().insert(names.to_vec(), plan.clone());
        plan
    }

    /// Whether the deciding cell admits the reading: a number,
    /// numeric text, a value of the kind, or null (undecided,
    /// converted); a text cell that is none of these keeps its
    /// column as it is.
    fn admits(reading: &Reading, v: Option<Value>) -> bool {
        match (reading, v) {
            (_, None | Some(Value::Null)) => true,
            (_, Some(Value::Int(_) | Value::Float(_))) => true,
            (Reading::Unit(_), Some(Value::Quantity { .. })) => true,
            (Reading::Unit(_), Some(Value::Str(s))) => {
                s.trim().parse::<f64>().is_ok() || parse_unit_text(&s).is_some()
            }
            (Reading::Instant | Reading::Date, Some(Value::Instant { .. })) => true,
            (Reading::Instant | Reading::Date, Some(Value::Str(s))) => {
                s.trim().parse::<f64>().is_ok() || text_instant(&s).is_some()
            }
            _ => false,
        }
    }

    fn decide(&self, names: &[String], sample: &dyn Fn(&str) -> Option<Value>) -> Plan {
        let mut plan = Plan::default();
        let mut skipped: Vec<String> = Vec::new();
        let mut converted: Vec<String> = Vec::new();
        let refuse = |msg: String| {
            crate::value::ambient::record_refusal_coded(
                "AUTOUNITS",
                self.target.clone(),
                format!("{} on {}: {msg}", self.suffixes.spelled(), self.target),
            );
        };
        for name in names {
            let Some((stem, seg)) = name.rsplit_once('_') else {
                continue;
            };
            if stem.is_empty() || seg.is_empty() {
                continue;
            }
            let named = match &self.suffixes {
                Suffixes::Named(list) => {
                    if !list.iter().any(|s| s == seg) {
                        continue;
                    }
                    true
                }
                Suffixes::Known => false,
            };
            // the temporal suffixes first; then the stop list; then
            // the unit table
            let reading = match TEMPORAL.iter().find(|(s, _)| *s == seg) {
                Some((_, r)) => r.clone(),
                None if !named && STOP.contains(&seg) => {
                    skipped.push(format!("{name} ('{seg}' is a word here, not a unit)"));
                    continue;
                }
                // `price_min` beside `price_max` is the minimum, not
                // the minute (ruling 2026-11-08); the named form
                // converts regardless
                None if !named
                    && seg == "min"
                    && names.iter().any(|n| n == &format!("{stem}_max")) =>
                {
                    skipped.push(format!("{name} ('min' beside {stem}_max is the minimum)"));
                    continue;
                }
                None => match scale_of(seg) {
                    Some(scale) => {
                        plan.scales.insert(seg.to_string(), scale);
                        Reading::Unit(seg.to_string())
                    }
                    None if named => {
                        refuse(format!(
                            "'{seg}' is no unit this run knows (a unit statement declares one: \
                             'unit {seg} = …;')"
                        ));
                        return Plan::default();
                    }
                    None => {
                        skipped.push(format!("{name} ('{seg}' is no unit)"));
                        continue;
                    }
                },
            };
            let cell = sample(name);
            if !Self::admits(&reading, cell.clone()) {
                let kind = match reading {
                    Reading::Unit(_) => "numbers",
                    Reading::Instant => "instants",
                    Reading::Date => "dates",
                };
                skipped.push(format!("{name} (its cells are no {kind})"));
                continue;
            }
            // a temporal reading types the column where it is; a
            // unit renames, and the new name must be free
            let new = if reading.in_place() {
                name.as_str()
            } else {
                stem
            };
            if !reading.in_place() {
                if names.iter().any(|n| n == stem) {
                    refuse(format!(
                        "'{name}' would become '{stem}', a property the rows already carry"
                    ));
                    return Plan::default();
                }
                if let Some((other, _)) = plan.by_new.get(stem) {
                    refuse(format!("'{other}' and '{name}' would both become '{stem}'"));
                    return Plan::default();
                }
            }
            converted.push(reading.told(name, stem, cell.as_ref()));
            plan.by_new.insert(new.to_string(), (name.clone(), reading));
            plan.by_source.insert(name.clone(), new.to_string());
        }
        let mut msg = format!("{} on {}: ", self.suffixes.spelled(), self.target);
        if converted.is_empty() {
            msg.push_str(match &self.suffixes {
                Suffixes::Known => "no property ends in a unit",
                Suffixes::Named(_) => "no property ends in a named suffix",
            });
        } else {
            msg.push_str(&converted.join(", "));
        }
        if !skipped.is_empty() {
            msg.push_str(&format!("; skipped: {}", skipped.join(", ")));
        }
        crate::value::ambient::record_diagnostic(Diagnostic::info(
            "AUTOUNITS",
            self.target.clone(),
            msg,
        ));
        plan
    }

    /// The plan a node's properties read through: by its row shape
    /// when it has one (remembered under its parent, so a table's
    /// rows find it without allocating the header), else by its own
    /// property names (a document's nodes each bring their own).
    fn plan_of(&self, node: NodeId) -> Rc<Plan> {
        let parent = self.inner.parent(node);
        if let Some(p) = parent
            && let Some(plan) = self.by_parent.borrow().get(&p)
        {
            return plan.clone();
        }
        let shaped = self.inner.row_columns(node);
        let names = shaped
            .clone()
            .unwrap_or_else(|| self.inner.property_names(node));
        let plan = self.plan_for(&names, &|n| self.inner.property(node, n));
        if let (Some(p), Some(_)) = (parent, shaped) {
            self.by_parent.borrow_mut().insert(p, plan.clone());
        }
        plan
    }

    /// The plan a member node reads through — a document's record
    /// members are its properties and its children at once (a JSON
    /// object's keys, a row's columns in a database), so the child
    /// view renames and converts as the property view does. `None`
    /// for a node its parent's plan leaves alone.
    fn member_reading(&self, node: NodeId) -> Option<(Rc<Plan>, String, String)> {
        let base = self.inner.name(node)?;
        let parent = self.inner.parent(node)?;
        let plan = self.plan_of(parent);
        if plan.is_empty() {
            return None;
        }
        let new = plan.by_source.get(&base)?.clone();
        Some((plan, base, new))
    }

    /// The plan for a header seen through one of its rows.
    fn plan_for_row(&self, cols: &[String], row: NodeId) -> Rc<Plan> {
        self.plan_for(cols, &|n| self.inner.property(row, n))
    }

    /// The locator of `node` with a renamed member spelled by its new
    /// name: the source's own rendering, its last segment replaced.
    pub fn locate(&self, node: NodeId, inner: &dyn Fn(NodeId) -> String) -> String {
        let loc = inner(node);
        match self.member_reading(node) {
            Some((_, base, new)) if base != new => match loc.strip_suffix(base.as_str()) {
                Some(head) if head.ends_with('/') => format!("{head}{new}"),
                _ => loc,
            },
            _ => loc,
        }
    }

    /// The header of the rows beneath `node`: one row's columns,
    /// reached through the source's own selection when it has one,
    /// so a table is never listed to learn its shape.
    fn header_below(&self, node: NodeId, like: &StepPlan<'_>) -> Option<(NodeId, Vec<String>)> {
        if let Some(h) = self.headers.borrow().get(&node) {
            return h.clone();
        }
        // any child, so a renamed step name never empties the probe
        let probe = StepPlan {
            axis: like.axis,
            name: None,
            conjuncts: Vec::new(),
            traits: Vec::new(),
            limit: Some(1),
            cap: like.cap,
            order: None,
        };
        let first = match self.inner.select(node, &probe) {
            Some(sel) => sel.nodes.first().map(|(n, _)| *n),
            None => self.inner.children(node).into_iter().next(),
        };
        let header = first.map(|n| {
            (
                n,
                self.inner
                    .row_columns(n)
                    .unwrap_or_else(|| self.inner.property_names(n)),
            )
        });
        self.headers.borrow_mut().insert(node, header.clone());
        header
    }

    /// Whether a step plan from `node` touches a conversion: its
    /// step name is a member of `node` renamed (the source knows the
    /// name as written only), or a predicate or an order names a
    /// converted column of the rows beneath. Then the engine reads
    /// the nodes itself.
    fn plan_touches_below(&self, node: NodeId, plan: &StepPlan<'_>) -> bool {
        if let Some(name) = plan.name {
            let members = self.plan_of(node);
            if members.by_new.contains_key(name) || members.by_source.contains_key(name) {
                return true;
            }
        }
        if plan.conjuncts.is_empty() && plan.order.is_none() {
            return false;
        }
        match self.header_below(node, plan) {
            Some((row, h)) => self.plan_for_row(&h, row).touches(plan),
            None => false,
        }
    }

    /// The node a scope path names, each segment a child by name.
    fn node_of_scope(&self, scope: &[&str]) -> Option<NodeId> {
        let mut cur = self.inner.root();
        for seg in scope {
            let kids = self.inner.children_named(cur, seg);
            cur = *kids.first()?;
        }
        Some(cur)
    }

    /// Whether any plan decided so far touches the step plan — what
    /// a scope-level hook can know without a node.
    fn any_plan_touches(&self, plan: &StepPlan<'_>) -> bool {
        if let Some(name) = plan.name
            && self
                .plans
                .borrow()
                .values()
                .any(|p| p.by_new.contains_key(name) || p.by_source.contains_key(name))
        {
            return true;
        }
        if plan.conjuncts.is_empty() && plan.order.is_none() {
            return false;
        }
        self.plans.borrow().values().any(|p| p.touches(plan))
    }
}

impl<A: AstAdapter> AstAdapter for AutoUnits<A> {
    crate::forward_ast_adapter!(inner; except property, properties, property_names,
        row_columns, row_text, prefetch_properties, select, select_plan, estimate, count,
        index_lookup, property_withheld, reference_properties, resolve,
        name, children_named, answers_to, default_value, descendants_named, descendants_where);

    // The child view of a record's members: renamed and converted
    // as the property view is, so a serialization by children (`%()`,
    // `@| #` over a database's rows or a document's records) and a
    // path step by name (`/legs/*/distance`) agree with `::distance`.
    fn name(&self, node: NodeId) -> Option<String> {
        match self.member_reading(node) {
            Some((_, _, new)) => Some(new),
            None => self.inner.name(node),
        }
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        let plan = self.plan_of(node);
        if plan.is_empty() {
            return self.inner.children_named(node, name);
        }
        if let Some((source, _)) = plan.by_new.get(name) {
            return self.inner.children_named(node, source);
        }
        if plan.by_source.contains_key(name) {
            return Vec::new();
        }
        self.inner.children_named(node, name)
    }

    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        // a renamed member answers to its new name alone
        match self.member_reading(node) {
            Some((_, _, new)) => name == new,
            None => self.inner.answers_to(node, name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match self.member_reading(node) {
            Some((plan, _, new)) => {
                let reading = &plan.by_new[&new].1;
                self.inner
                    .default_value(node)
                    .map(|v| plan.convert(v, reading))
            }
            None => self.inner.default_value(node),
        }
    }

    // A descendant search by name is the engine's walk here: a
    // source's own index knows the names as written, not as renamed.
    fn descendants_named(&self, _node: NodeId, _name: &str) -> Option<Vec<(NodeId, usize)>> {
        None
    }

    fn descendants_where(
        &self,
        _node: NodeId,
        _name: &str,
        _property: &str,
        _value: &Value,
    ) -> Option<Vec<(NodeId, usize)>> {
        None
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let plan = self.plan_of(node);
        if plan.is_empty() {
            return self.inner.property(node, name);
        }
        if let Some((source, reading)) = plan.by_new.get(name) {
            return self
                .inner
                .property(node, source)
                .map(|v| plan.convert(v, reading));
        }
        if plan.by_source.contains_key(name) {
            return None;
        }
        self.inner.property(node, name)
    }

    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        let plan = self.plan_of(node);
        let props = self.inner.properties(node);
        if plan.is_empty() {
            return props;
        }
        props
            .into_iter()
            .map(|(k, v)| match plan.by_source.get(&k) {
                Some(new) => {
                    let reading = &plan.by_new[new].1;
                    (new.clone(), plan.convert(v, reading))
                }
                None => (k, v),
            })
            .collect()
    }

    fn property_names(&self, node: NodeId) -> Vec<String> {
        let names = self.inner.property_names(node);
        let plan = self.plan_for_row(
            &self
                .inner
                .row_columns(node)
                .unwrap_or_else(|| names.clone()),
            node,
        );
        if plan.is_empty() {
            return names;
        }
        names
            .into_iter()
            .map(|n| plan.by_source.get(&n).cloned().unwrap_or(n))
            .collect()
    }

    fn row_columns(&self, node: NodeId) -> Option<Vec<String>> {
        let cols = self.inner.row_columns(node)?;
        let plan = self.plan_for_row(&cols, node);
        if plan.is_empty() {
            return Some(cols);
        }
        Some(
            cols.into_iter()
                .map(|c| plan.by_source.get(&c).cloned().unwrap_or(c))
                .collect(),
        )
    }

    fn row_text(&self, node: NodeId, f: &mut dyn FnMut(&[&str])) -> bool {
        let Some(cols) = self.inner.row_columns(node) else {
            return self.inner.row_text(node, f);
        };
        let plan = self.plan_for_row(&cols, node);
        if plan.is_empty() {
            return self.inner.row_text(node, f);
        }
        // the converted columns' readings by position: a numeric cell
        // there is respelled as the value it reads as (`12` in a
        // column of grams → `12_g`, an epoch number → its ISO text),
        // so the one-pass typed build types the column as the
        // property answers
        let readings: Vec<Option<&Reading>> = cols
            .iter()
            .map(|c| plan.by_source.get(c).map(|new| &plan.by_new[new].1))
            .collect();
        self.inner.row_text(node, &mut |cells| {
            let respelled: Vec<Option<String>> = cells
                .iter()
                .enumerate()
                .map(|(j, c)| match readings.get(j).copied().flatten() {
                    Some(Reading::Unit(u)) if c.trim().parse::<f64>().is_ok() => {
                        Some(format!("{}_{u}", c.trim()))
                    }
                    Some(r @ (Reading::Instant | Reading::Date))
                        if c.trim().parse::<f64>().is_ok() =>
                    {
                        Some(plan.convert(Value::Str(c.to_string()), r).to_string())
                    }
                    _ => None,
                })
                .collect();
            let refs: Vec<&str> = cells
                .iter()
                .enumerate()
                .map(|(j, c)| respelled[j].as_deref().unwrap_or(c))
                .collect();
            f(&refs)
        })
    }

    fn prefetch_properties(&self, nodes: &[NodeId], names: &[&str]) {
        let Some(first) = nodes.first() else {
            return;
        };
        let plan = self.plan_of(*first);
        if plan.is_empty() {
            return self.inner.prefetch_properties(nodes, names);
        }
        let mapped: Vec<&str> = names
            .iter()
            .map(|n| plan.by_new.get(*n).map(|(s, _)| s.as_str()).unwrap_or(n))
            .collect();
        self.inner.prefetch_properties(nodes, &mapped)
    }

    fn select(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<Selection> {
        if self.plan_touches_below(from, plan) {
            return None;
        }
        self.inner.select(from, plan)
    }

    fn select_plan(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Vec<bool>> {
        if self.any_plan_touches(plan)
            || self
                .node_of_scope(scope)
                .is_some_and(|n| self.plan_touches_below(n, plan))
        {
            return None;
        }
        self.inner.select_plan(scope, plan)
    }

    fn estimate(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Estimate> {
        if self.any_plan_touches(plan)
            || self
                .node_of_scope(scope)
                .is_some_and(|n| self.plan_touches_below(n, plan))
        {
            return None;
        }
        self.inner.estimate(scope, plan)
    }

    fn count(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<usize> {
        if self.plan_touches_below(from, plan) {
            return None;
        }
        self.inner.count(from, plan)
    }

    fn index_lookup(&self, scope: &str, property: &str, value: &Value) -> Option<Vec<NodeId>> {
        let converted = self
            .plans
            .borrow()
            .values()
            .any(|p| p.by_new.contains_key(property) || p.by_source.contains_key(property));
        if converted {
            return None;
        }
        self.inner.index_lookup(scope, property, value)
    }

    fn property_withheld(&self, node: NodeId, name: &str) -> bool {
        let plan = self.plan_of(node);
        match plan.by_new.get(name) {
            Some((source, _)) => self.inner.property_withheld(node, source),
            None => self.inner.property_withheld(node, name),
        }
    }

    fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
        let plan = self.plan_of(source);
        self.inner
            .reference_properties(source, target)
            .into_iter()
            .map(|n| plan.by_source.get(&n).cloned().unwrap_or(n))
            .collect()
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let plan = self.plan_of(node);
        match plan.by_new.get(property) {
            Some((source, _)) => self.inner.resolve(node, source, hint),
            None => self.inner.resolve(node, property, hint),
        }
    }
}

/// The two options off a target's query, merged: `autounits` (no
/// value) and `autounit=_km,_g` (a comma list; the option may repeat).
/// `Ok(None)` when neither stands in the pairs; the pairs that are
/// neither are returned for the reader's own parsing.
pub fn take_options<'a>(
    pairs: impl IntoIterator<Item = &'a str>,
) -> Result<(Option<Suffixes>, Vec<&'a str>), String> {
    let mut found: Option<Suffixes> = None;
    let mut rest = Vec::new();
    for pair in pairs {
        let this = match pair.split_once('=') {
            Some(("autounits", v)) => {
                return Err(format!(
                    "autounits={v}: autounits takes no value (it converts every known suffix); \
                     the named form is 'autounit=_km,_g'"
                ));
            }
            Some(("autounit", list)) => {
                let names = list
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(named_suffix)
                    .collect::<Result<Vec<_>, _>>()?;
                if names.is_empty() {
                    return Err(named_suffix("").unwrap_err());
                }
                Suffixes::Named(names)
            }
            None if pair == "autounits" => Suffixes::Known,
            None if pair == "autounit" => {
                return Err(
                    "autounit names its suffixes: 'autounit=_km,_g'; every known suffix is \
                     'autounits'"
                        .to_string(),
                );
            }
            _ => {
                rest.push(pair);
                continue;
            }
        };
        found = Some(match found {
            Some(prev) => prev.merge(this),
            None => this,
        });
    }
    Ok((found, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe;
    impl AstAdapter for Probe {
        fn root(&self) -> NodeId {
            NodeId(1)
        }
        fn children(&self, _node: NodeId) -> Vec<NodeId> {
            Vec::new()
        }
        fn name(&self, _node: NodeId) -> Option<String> {
            None
        }
    }

    #[test]
    fn named_suffixes_are_written_with_their_underscore() {
        assert_eq!(named_suffix("_km").unwrap(), "km");
        assert_eq!(named_suffix("_кг").unwrap(), "кг");
        assert!(named_suffix("km").unwrap_err().contains("'_km'"));
        assert!(named_suffix("_").unwrap_err().contains("shaped as no unit"));
        assert!(
            named_suffix("_k/m")
                .unwrap_err()
                .contains("shaped as no unit")
        );
    }

    #[test]
    fn the_options_merge_and_refuse() {
        let (s, rest) = take_options(["autounit=_km,_g", "root=/x", "autounit=_min"]).unwrap();
        assert_eq!(
            s,
            Some(Suffixes::Named(vec!["km".into(), "g".into(), "min".into()]))
        );
        assert_eq!(rest, vec!["root=/x"]);
        let (s, _) = take_options(["autounit=_km", "autounits"]).unwrap();
        assert_eq!(s, Some(Suffixes::Known));
        assert_eq!(take_options(["encoding=utf-8"]).unwrap().0, None);
        assert!(
            take_options(["autounits=yes"])
                .unwrap_err()
                .contains("takes no value")
        );
        assert!(
            take_options(["autounit"])
                .unwrap_err()
                .contains("names its suffixes")
        );
        assert!(take_options(["autounit=km"]).unwrap_err().contains("'_km'"));
    }

    #[test]
    fn stop_list_covers_the_table_words() {
        // every English function word the table reads as a unit
        // stands on the stop list; the test names any newcomer
        let candidates = [
            "a", "an", "at", "in", "on", "by", "to", "of", "as", "is", "it", "or", "no", "so",
            "do", "if", "am", "pm", "up", "for", "per", "via", "from", "with", "the", "and", "but",
            "not", "nor", "yet", "off", "out", "over", "into", "onto", "upon", "than", "then",
            "when", "was", "be", "has", "had", "can", "may", "must", "will", "id", "nb", "nr",
            "num", "no", "ref", "key", "idx", "raw", "old", "new", "min", "max", "avg", "sum",
            "cnt", "pct", "abs", "rel", "end", "top", "low", "mid", "all", "any",
        ];
        let units: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|w| scale_of(w).is_some())
            .collect();
        let uncovered: Vec<&str> = units
            .iter()
            .copied()
            .filter(|w| !STOP.contains(w) && !["min", "max"].contains(w))
            .collect();
        assert!(
            uncovered.is_empty(),
            "units among the words, not on the stop list: {uncovered:?} (all units: {units:?})"
        );
    }

    #[test]
    fn temporal_suffixes_read_instants_and_dates() {
        let plan = Plan::default();
        let v = plan.convert(Value::Str("2024-03-05T10:20:30Z".into()), &Reading::Instant);
        assert_eq!(v.to_string(), "2024-03-05T10:20:30Z");
        let d = plan.convert(Value::Str("2024-03-05T10:20:30Z".into()), &Reading::Date);
        assert_eq!(d.to_string(), "2024-03-05");
        // epoch seconds, then milliseconds from 10^11 up
        let e = plan.convert(Value::Int(1_700_000_000), &Reading::Instant);
        assert_eq!(e.to_string(), "2023-11-14T22:13:20");
        let m = plan.convert(Value::Int(1_700_000_000_000), &Reading::Instant);
        assert_eq!(m.to_string(), "2023-11-14T22:13:20");
        assert_eq!(
            Reading::Instant.told("created_at", "created", Some(&Value::Int(1_700_000_000))),
            "created_at: instants (epoch seconds)"
        );
        assert_eq!(
            Reading::Date.told(
                "published_on",
                "published",
                Some(&Value::Str("2024-01-05".into()))
            ),
            "published_on: dates"
        );
        assert_eq!(
            Reading::Unit("km".into()).told("length_km", "length", None),
            "length_km → length in km"
        );
        let t = plan.convert(Value::Str("n/a".into()), &Reading::Instant);
        assert_eq!(t, Value::Str("n/a".into()));
        assert!(AutoUnits::<Probe>::admits(
            &Reading::Instant,
            Some(Value::Str("2024-01-05".into()))
        ));
        assert!(!AutoUnits::<Probe>::admits(
            &Reading::Instant,
            Some(Value::Str("yesterday".into()))
        ));
        assert!(!AutoUnits::<Probe>::admits(
            &Reading::Unit("t".into()),
            Some(Value::Str("2024-01-05".into()))
        ));
    }

    #[test]
    fn a_plan_converts_numbers_and_same_dimension_quantities() {
        let mut plan = Plan::default();
        plan.scales.insert("g".into(), scale_of("g").unwrap());
        let g = Reading::Unit("g".into());
        let q = plan.convert(Value::Str("3750".into()), &g);
        match q {
            Value::Quantity { written, .. } => assert_eq!(written, Some((3750.0, "g".into()))),
            other => panic!("{other:?}"),
        }
        assert_eq!(plan.convert(Value::Null, &g), Value::Null);
        assert_eq!(
            plan.convert(Value::Str("n/a".into()), &g),
            Value::Str("n/a".into())
        );
        match plan.convert(Value::Str("2 kg".into()), &g) {
            Value::Quantity { written, .. } => assert_eq!(written, Some((2000.0, "g".into()))),
            other => panic!("{other:?}"),
        }
        // a quantity of another dimension stays as it is
        let min = Value::Str("12 min".into());
        assert_eq!(plan.convert(min.clone(), &g), min);
    }

    #[test]
    fn the_unit_table_decides_a_suffix() {
        assert!(scale_of("g").is_some());
        assert!(scale_of("min").is_some());
        assert!(scale_of("кг").is_some());
        // the attotonne: a prefix on the tonne, so a unit by the
        // table; the cell probe is what keeps `created_at` a text
        // column
        assert!(scale_of("at").is_some());
        assert!(scale_of("mb").is_none());
        // the typographic point: a unit too
        assert!(scale_of("pt").is_some());
        assert!(scale_of("name").is_none());
        assert!(scale_of("id").is_none());
        let t = Reading::Unit("t".into());
        assert!(!AutoUnits::<Probe>::admits(
            &t,
            Some(Value::Str("2024-01-01".into()))
        ));
        assert!(AutoUnits::<Probe>::admits(
            &t,
            Some(Value::Str(" 12.5 ".into()))
        ));
        assert!(AutoUnits::<Probe>::admits(&t, None));
    }
}
