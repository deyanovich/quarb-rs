//! LDAP directory adapter for the Quarb query engine.
//!
//! An LDAP directory *is* a tree — the DIT, whose distinguished
//! names are paths — so the arbor mounts it directly: the root
//! is the base DN, an entry's children are the entries one level
//! below it (`onelevel` scope), and each entry is named by its
//! RDN (`cn=ada`, `ou=people`). Attributes are properties;
//! multi-valued attributes answer as lists. `objectClass` values
//! are traits, so `<person>`, `<groupOfNames>`, `<organizationalUnit>`
//! filter the way a type does.
//!
//! **DN-valued attributes are native references.** `member`,
//! `manager`, `secretary`, `seeAlso`, and `memberOf` hold DNs,
//! so `~>` follows them with no schema: `::manager~>::cn` walks
//! an entry to its manager's common name, and `->member`
//! enumerates a group's members as labeled edges. The reverse of
//! `member` — the groups an entry belongs to — is `<-member`,
//! answered by a filtered search.
//!
//! Loads lazily: one search per level on first descent, one
//! base-scope search per referenced DN, cached for the session.
//! Read-only — the adapter only ever searches.
//!
//! **Target**: `ldap://[USER:PASS@]HOST[:PORT]/BASE_DN` (or
//! `ldaps://`; port defaults to 389/636). With credentials the
//! adapter does a simple bind; without, it binds anonymously.
//! `USER` is a bind DN and may be percent-encoded to carry its
//! own commas.

use ldap3::{LdapConn, Scope, SearchEntry};
use quarb::{AstAdapter, NodeId, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// An error connecting to or reading a directory.
#[derive(Debug, thiserror::Error)]
pub enum LdapError {
    #[error("ldap: {0}")]
    Ldap(#[from] ldap3::LdapError),
    #[error("ldap: bind failed: {0}")]
    Bind(String),
    #[error("ldap target: {0} (expected ldap://[USER:PASS@]HOST[:PORT]/BASE_DN)")]
    Target(String),
}

/// Attributes whose values are DNs — followed as references.
const DN_ATTRS: &[&str] = &[
    "member",
    "uniquemember",
    "manager",
    "secretary",
    "seealso",
    "owner",
    "memberof",
];

/// A decoded entry: its DN and attributes (lowercased keys,
/// original-case preserved in values), in server order.
struct Entry {
    dn: String,
    rdn: String,
    attrs: Vec<(String, Vec<String>)>,
}

impl Entry {
    fn get(&self, name: &str) -> Option<&[String]> {
        let name = name.to_ascii_lowercase();
        self.attrs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_slice())
    }
}

/// The RDN is the first component of a DN (`cn=ada,ou=people` →
/// `cn=ada`). DN commas inside values are escaped `\,`, so a
/// bare comma is the separator.
fn rdn_of(dn: &str) -> String {
    let bytes = dn.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b',' => return dn[..i].to_string(),
            _ => i += 1,
        }
    }
    dn.to_string()
}

struct Node {
    /// Index into `entries`; the root's own entry.
    entry: usize,
    parent: Option<NodeId>,
    children: RefCell<Option<Vec<NodeId>>>,
}

/// An LDAP directory, exposed as an arbor.
pub struct LdapAdapter {
    conn: RefCell<LdapConn>,
    base: String,
    nodes: RefCell<Vec<Node>>,
    entries: RefCell<Vec<Entry>>,
    /// DN (lowercased) → node, for reference resolution.
    by_dn: RefCell<HashMap<String, NodeId>>,
}

/// Percent-decode a URL component (bind DNs carry commas).
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A parsed target: the LDAP URL, optional (bind DN, password),
/// and the base DN.
type Target = (String, Option<(String, String)>, String);

/// Parse the target into (url, bind creds, base DN).
fn parse_target(target: &str) -> Result<Target, LdapError> {
    let scheme = if target.starts_with("ldaps://") {
        "ldaps"
    } else if target.starts_with("ldap://") {
        "ldap"
    } else {
        return Err(LdapError::Target(target.to_string()));
    };
    let rest = &target[scheme.len() + 3..];
    let (authority, base) = rest
        .split_once('/')
        .ok_or_else(|| LdapError::Target(target.to_string()))?;
    let (creds, hostport) = match authority.rsplit_once('@') {
        Some((c, h)) => {
            let (u, p) = c
                .split_once(':')
                .ok_or_else(|| LdapError::Target(target.to_string()))?;
            (Some((pct_decode(u), pct_decode(p))), h)
        }
        None => (None, authority),
    };
    let hostport = if hostport.contains(':') {
        hostport.to_string()
    } else {
        let port = if scheme == "ldaps" { 636 } else { 389 };
        format!("{hostport}:{port}")
    };
    if base.is_empty() {
        return Err(LdapError::Target(target.to_string()));
    }
    Ok((format!("{scheme}://{hostport}"), creds, pct_decode(base)))
}

impl LdapAdapter {
    /// Connect, bind, and root the arbor at the base DN.
    pub fn connect(target: &str) -> Result<Self, LdapError> {
        let (url, creds, base) = parse_target(target)?;
        let mut conn = LdapConn::new(&url)?;
        match creds {
            Some((dn, pw)) => {
                conn.simple_bind(&dn, &pw)?
                    .success()
                    .map_err(|e| LdapError::Bind(e.to_string()))?;
            }
            None => {
                // Anonymous bind; some servers refuse, which
                // surfaces on the first search rather than here.
                let _ = conn.simple_bind("", "");
            }
        }
        let adapter = LdapAdapter {
            conn: RefCell::new(conn),
            base: base.clone(),
            nodes: RefCell::new(Vec::new()),
            entries: RefCell::new(Vec::new()),
            by_dn: RefCell::new(HashMap::new()),
        };
        // The base entry is the root; a base-scope read both
        // fetches its attributes and proves the bind/base valid.
        let root_entry = adapter.read_entry(&base)?.ok_or_else(|| {
            LdapError::Bind(format!("base DN not found or not readable: {base}"))
        })?;
        let idx = adapter.push_entry(root_entry);
        let root = adapter.push_node(idx, None);
        adapter.by_dn.borrow_mut().insert(base.to_ascii_lowercase(), root);
        Ok(adapter)
    }

    /// A human-readable locator: the DN.
    pub fn locator(&self, node: NodeId) -> String {
        let entries = self.entries.borrow();
        let nodes = self.nodes.borrow();
        entries[nodes[node.0 as usize].entry].dn.clone()
    }

    fn push_entry(&self, e: Entry) -> usize {
        let mut entries = self.entries.borrow_mut();
        entries.push(e);
        entries.len() - 1
    }

    fn push_node(&self, entry: usize, parent: Option<NodeId>) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        let id = NodeId(nodes.len() as u64);
        nodes.push(Node {
            entry,
            parent,
            children: RefCell::new(None),
        });
        id
    }

    fn to_entry(se: SearchEntry) -> Entry {
        let mut attrs: Vec<(String, Vec<String>)> = se
            .attrs
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        // Binary attributes (e.g. jpegPhoto) arrive separately;
        // note their presence without carrying the bytes.
        for (k, v) in se.bin_attrs {
            let k = k.to_ascii_lowercase();
            if !attrs.iter().any(|(ek, _)| *ek == k) {
                attrs.push((k, vec![format!("<{} bytes>", v.first().map_or(0, |b| b.len()))]));
            }
        }
        Entry {
            rdn: rdn_of(&se.dn),
            dn: se.dn,
            attrs,
        }
    }

    /// Read one entry by DN (base scope).
    fn read_entry(&self, dn: &str) -> Result<Option<Entry>, LdapError> {
        let mut conn = self.conn.borrow_mut();
        let (rs, _) = conn
            .search(dn, Scope::Base, "(objectClass=*)", vec!["*", "+"])?
            .success()?;
        Ok(rs
            .into_iter()
            .next()
            .map(|e| Self::to_entry(SearchEntry::construct(e))))
    }

    /// The node for a DN, fetching (base scope) when not already
    /// interned.
    fn dn_node(&self, dn: &str) -> Option<NodeId> {
        let key = dn.to_ascii_lowercase();
        if let Some(&n) = self.by_dn.borrow().get(&key) {
            return Some(n);
        }
        let entry = self.read_entry(dn).ok().flatten()?;
        let idx = self.push_entry(entry);
        // A referenced entry interns shallowly (no parent chain);
        // its locator is the full DN regardless.
        let node = self.push_node(idx, None);
        self.by_dn.borrow_mut().insert(key, node);
        Some(node)
    }

    fn entry_of(&self, node: NodeId) -> usize {
        self.nodes.borrow()[node.0 as usize].entry
    }
}

impl AstAdapter for LdapAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if let Some(c) = self.nodes.borrow()[node.0 as usize]
            .children
            .borrow()
            .as_ref()
        {
            return c.clone();
        }
        let dn = self.locator(node);
        let found = {
            let mut conn = self.conn.borrow_mut();
            match conn
                .search(&dn, Scope::OneLevel, "(objectClass=*)", vec!["*", "+"])
                .and_then(|r| r.success())
            {
                Ok((rs, _)) => rs,
                // An unreadable level is an empty one (ACLs deny
                // some subtrees; the rest of the DIT still works).
                Err(_) => Vec::new(),
            }
        };
        let mut ids = Vec::new();
        for raw in found {
            let entry = Self::to_entry(SearchEntry::construct(raw));
            let key = entry.dn.to_ascii_lowercase();
            if let Some(&existing) = self.by_dn.borrow().get(&key) {
                ids.push(existing);
                continue;
            }
            let idx = self.push_entry(entry);
            let child = self.push_node(idx, Some(node));
            self.by_dn.borrow_mut().insert(key, child);
            ids.push(child);
        }
        *self.nodes.borrow()[node.0 as usize].children.borrow_mut() = Some(ids.clone());
        ids
    }

    fn name(&self, node: NodeId) -> Option<String> {
        let e = self.entry_of(node);
        // The root prints its full base DN; deeper entries their
        // RDN (the path segment).
        if node.0 == 0 {
            return None;
        }
        Some(self.entries.borrow()[e].rdn.clone())
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes.borrow()[node.0 as usize].parent
    }

    /// `objectClass` values, each a trait (`<person>`,
    /// `<groupOfNames>`), lowercased.
    fn traits(&self, node: NodeId) -> Vec<String> {
        let entries = self.entries.borrow();
        let e = &entries[self.entry_of(node)];
        e.get("objectclass")
            .map(|vs| vs.iter().map(|v| v.to_ascii_lowercase()).collect())
            .unwrap_or_default()
    }

    /// An attribute: a single value as a scalar, several as a
    /// list. Attribute names are case-insensitive.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let entries = self.entries.borrow();
        let e = &entries[self.entry_of(node)];
        let vals = e.get(name)?;
        match vals {
            [] => None,
            [one] => Some(Value::Str(one.clone())),
            many => Some(Value::List(
                many.iter().map(|v| Value::Str(v.clone())).collect(),
            )),
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match key {
            "dn" => Some(Value::Str(self.locator(node))),
            "n-attrs" => {
                let entries = self.entries.borrow();
                Some(Value::Int(entries[self.entry_of(node)].attrs.len() as i64))
            }
            _ => None,
        }
    }

    /// `::manager~>`, `::member~>`, and the other DN-valued
    /// attributes follow to the referenced entry (the first
    /// value when multi-valued).
    fn resolve(&self, node: NodeId, property: &str, _hint: Option<&str>) -> Option<NodeId> {
        let name = property.to_ascii_lowercase();
        if !DN_ATTRS.contains(&name.as_str()) {
            return None;
        }
        let target_dn = {
            let entries = self.entries.borrow();
            let e = &entries[self.entry_of(node)];
            e.get(&name)?.first()?.clone()
        };
        self.dn_node(&target_dn)
    }

    /// Every DN-valued attribute is an outgoing crosslink,
    /// labeled by the attribute name (`->member`, `->manager`).
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let refs: Vec<(String, String)> = {
            let entries = self.entries.borrow();
            let e = &entries[self.entry_of(node)];
            e.attrs
                .iter()
                .filter(|(k, _)| DN_ATTRS.contains(&k.as_str()))
                .flat_map(|(k, vs)| vs.iter().map(move |v| (k.clone(), v.clone())))
                .collect()
        };
        refs.into_iter()
            .filter_map(|(k, dn)| self.dn_node(&dn).map(|n| (k, n)))
            .collect()
    }

    /// The reverse of the DN-valued attributes: `<-member` finds
    /// the groups this entry belongs to (and likewise for the
    /// others), by a subtree search over the base.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let dn = self.locator(node);
        // Escape the DN for an LDAP filter assertion value.
        let esc: String = dn
            .chars()
            .flat_map(|c| match c {
                '(' => "\\28".chars().collect::<Vec<_>>(),
                ')' => "\\29".chars().collect(),
                '*' => "\\2a".chars().collect(),
                '\\' => "\\5c".chars().collect(),
                other => vec![other],
            })
            .collect();
        let filter = format!(
            "(|{})",
            DN_ATTRS
                .iter()
                .map(|a| format!("({a}={esc})"))
                .collect::<String>()
        );
        let found = {
            let mut conn = self.conn.borrow_mut();
            match conn
                .search(&self.base, Scope::Subtree, &filter, vec!["*", "+"])
                .and_then(|r| r.success())
            {
                Ok((rs, _)) => rs,
                Err(_) => return Vec::new(),
            }
        };
        let mut out = Vec::new();
        for raw in found {
            let entry = Self::to_entry(SearchEntry::construct(raw));
            // Label each backlink by which DN attribute of the
            // referrer pointed here.
            let label = DN_ATTRS
                .iter()
                .find(|a| {
                    entry
                        .get(a)
                        .is_some_and(|vs| vs.iter().any(|v| v.eq_ignore_ascii_case(&dn)))
                })
                .copied()
                .unwrap_or("ref");
            let key = entry.dn.to_ascii_lowercase();
            let n = if let Some(&existing) = self.by_dn.borrow().get(&key) {
                existing
            } else {
                let idx = self.push_entry(entry);
                let n = self.push_node(idx, None);
                self.by_dn.borrow_mut().insert(key, n);
                n
            };
            out.push((label.to_string(), n));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        let (url, creds, base) =
            parse_target("ldap://localhost/dc=tesslab,dc=org").unwrap();
        assert_eq!(url, "ldap://localhost:389");
        assert!(creds.is_none());
        assert_eq!(base, "dc=tesslab,dc=org");

        let (url, creds, _) = parse_target(
            "ldaps://cn=admin%2Cdc=tesslab%2Cdc=org:secret@dir:1636/dc=tesslab,dc=org",
        )
        .unwrap();
        assert_eq!(url, "ldaps://dir:1636");
        let (dn, pw) = creds.unwrap();
        assert_eq!(dn, "cn=admin,dc=tesslab,dc=org");
        assert_eq!(pw, "secret");

        assert!(parse_target("http://x/dc=y").is_err());
        assert!(parse_target("ldap://host").is_err()); // no base
    }

    #[test]
    fn rdn_extraction() {
        assert_eq!(rdn_of("cn=ada,ou=people,dc=tesslab"), "cn=ada");
        assert_eq!(rdn_of("dc=tesslab,dc=org"), "dc=tesslab");
        // An escaped comma stays inside the RDN.
        assert_eq!(rdn_of("cn=Ada\\, Lovelace,ou=people"), "cn=Ada\\, Lovelace");
        assert_eq!(rdn_of("dc=org"), "dc=org");
    }
}
