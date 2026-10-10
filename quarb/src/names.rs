//! A per-document name table: the distinct tag names, attribute
//! names or object keys of one document, each stored once and
//! referred to by index. Fifty thousand `<tr>` rows share one name;
//! a node holds four bytes instead of a string.

use std::collections::HashMap;

/// The table: names in first-seen order, and their indexes.
#[derive(Debug, Clone, Default)]
pub struct Names {
    table: Vec<String>,
    index: HashMap<String, u32>,
}

impl Names {
    pub fn new() -> Names {
        Names::default()
    }

    /// The index of `name`, entering it when new.
    pub fn intern(&mut self, name: &str) -> u32 {
        if let Some(&k) = self.index.get(name) {
            return k;
        }
        let k = self.table.len() as u32;
        self.table.push(name.to_string());
        self.index.insert(name.to_string(), k);
        k
    }

    /// The name at `k`.
    pub fn get(&self, k: u32) -> &str {
        &self.table[k as usize]
    }

    /// The index of `name`, when the document has it: a lookup for
    /// a name the document never used answers nothing without a
    /// scan.
    pub fn key(&self, name: &str) -> Option<u32> {
        self.index.get(name).copied()
    }

    /// How many distinct names the document has.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}
