//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// What a register reference recalls, read through `part` (the
/// regula's value, or its provenance): the latest regula, one by
/// position or name, the whole register as a list, the named view
/// as a record, or the full view — the named regulae as `%.` lays
/// them out plus each anonymous one keyed `.N` by its 1-based
/// position (`#` cannot appear in a push name; names MAY be
/// numeric). `none` stands for a miss.
pub(super) fn recall_part<T: Clone>(
    register: &[Reg],
    r: &RegRef,
    part: impl Fn(&Reg) -> &T,
    none: T,
    list: impl Fn(Vec<T>) -> T,
    record: impl Fn(Vec<(String, T)>) -> T,
) -> T {
    match r {
        RegRef::Top => register.last().map(|r| part(r).clone()).unwrap_or(none),
        RegRef::Index(n) => register
            .get(n.saturating_sub(1))
            .map(|r| part(r).clone())
            .unwrap_or(none),
        RegRef::Named(name) => register
            .iter()
            .rev()
            .find(|r| r.name.as_deref() == Some(name.as_str()))
            .map(|r| part(r).clone())
            .unwrap_or(none),
        RegRef::Whole => list(register.iter().map(|r| part(r).clone()).collect()),
        RegRef::Record => record(named_register(register, part)),
        RegRef::FullRecord => {
            let mut fields: Vec<(String, T)> = Vec::new();
            for (i, reg) in register.iter().enumerate() {
                match &reg.name {
                    Some(name) => match fields.iter_mut().find(|(n, _)| n == name) {
                        Some((_, v)) => *v = part(reg).clone(),
                        None => fields.push((name.clone(), part(reg).clone())),
                    },
                    None => fields.push((format!(".{}", i + 1), part(reg).clone())),
                }
            }
            record(fields)
        }
    }
}

/// The value a register reference recalls.
pub(super) fn recall(register: &[Reg], r: &RegRef) -> Value {
    recall_part(
        register,
        r,
        |r| &r.value,
        Value::Null,
        Value::list,
        Value::Record,
    )
}

/// The origins a register reference recalls — what a read of it
/// notes — without cloning the regula's provenance tree: a list
/// pushed from a sub-pipeline carries a part per item, and `[$_ =
/// $.set]` reads the register once per value of the stream.
pub(super) fn recall_origins(register: &[Reg], r: &RegRef) -> Origins {
    let union = |items: Vec<Origins>| {
        let mut o = Origins::default();
        for i in &items {
            o.union(i);
        }
        o
    };
    recall_part(
        register,
        r,
        |r| &r.prov.origins,
        Origins::default(),
        union,
        |fields| union(fields.into_iter().map(|(_, o)| o).collect()),
    )
}

/// The origins of one field of a recalled record (`$.r:name`): that
/// part's own, the rest of the tree untouched.
pub(super) fn recall_field_origins(register: &[Reg], r: &RegRef, name: &str) -> Origins {
    let reg = match r {
        RegRef::Top => register.last(),
        RegRef::Index(n) => register.get(n.saturating_sub(1)),
        RegRef::Named(key) => register
            .iter()
            .rev()
            .find(|r| r.name.as_deref() == Some(key.as_str())),
        _ => None,
    };
    match reg {
        Some(reg) => reg.prov.part(name).origins,
        None => recall_prov(register, r).part(name).origins,
    }
}

/// The provenance a register reference recalls: the regula's own,
/// or, for the views, a record/list of the regulae's.
pub(super) fn recall_prov(register: &[Reg], r: &RegRef) -> Prov {
    recall_part(
        register,
        r,
        |r| &r.prov,
        Prov::default(),
        Prov::list,
        Prov::record,
    )
}

/// The `%.` layout, read through `part`: one entry per named
/// regula, in first-push order, carrying the latest push under that
/// name — so a repointed column keeps its place in the row. Unnamed
/// regulae are not part of the record.
pub(super) fn named_register<T: Clone>(
    register: &[Reg],
    part: impl Fn(&Reg) -> &T,
) -> Vec<(String, T)> {
    let mut fields: Vec<(String, T)> = Vec::new();
    for reg in register {
        if let Some(name) = &reg.name {
            match fields.iter_mut().find(|(n, _)| n == name) {
                Some((_, v)) => *v = part(reg).clone(),
                None => fields.push((name.clone(), part(reg).clone())),
            }
        }
    }
    fields
}

/// The `%.` layout's provenance.
pub(super) fn named_register_parts(register: &[Reg]) -> Vec<(String, Prov)> {
    named_register(register, |r| &r.prov)
}

/// The `%.` layout's fields.
pub(super) fn named_register_fields(register: &[Reg]) -> Vec<(String, Value)> {
    named_register(register, |r| &r.value)
}

/// A register rendered as a hashable dedup key: names plus value
/// text (Value carries floats, so it cannot be a key itself).
/// A register's identity: its regulae's names and value texts.
pub(super) type RegKey = Vec<(Option<String>, String)>;

/// What a round of a quantified push may change on a capsa: its
/// node, its topic, and the named view of its register.
pub(super) type RoundState = (NodeId, Option<Value>, Vec<(String, Value)>);

pub(super) fn reg_key(register: &[Reg]) -> RegKey {
    register
        .iter()
        .map(|r| (r.name.clone(), value_key(&r.value)))
        .collect()
}

/// A value's text for the key — except a large list, whose text
/// would be rendered once per capsa per command: a list pushed from
/// a sub-pipeline is shared by every capsa of the context, so its
/// key is a digest of its items computed once per list (equal lists
/// digest equal, so the dedup is unchanged).
fn value_key(v: &Value) -> String {
    match v {
        Value::List(items) if items.len() >= 16 => {
            format!("@list:{}:{:016x}", items.len(), list_digest(items))
        }
        _ => v.to_string(),
    }
}

fn list_digest(items: &std::sync::Arc<Vec<Value>>) -> u64 {
    use std::cell::RefCell;
    use std::hash::{Hash, Hasher};
    thread_local! {
        static DIGEST: RefCell<Option<(std::sync::Weak<Vec<Value>>, u64)>> =
            const { RefCell::new(None) };
    }
    DIGEST.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some((w, d)) = slot.as_ref()
            && w.upgrade()
                .is_some_and(|a| std::sync::Arc::ptr_eq(&a, items))
        {
            return *d;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for item in items.iter() {
            item.to_string().hash(&mut h);
        }
        let d = h.finish();
        *slot = Some((std::sync::Arc::downgrade(items), d));
        d
    })
}
