//! Emit `QUARB_GRAMMAR_STAMP` — the exact versions of tree-sitter
//! and each grammar crate, read from the workspace `Cargo.lock`.
//!
//! The AST cache folds this stamp into its grammar fingerprint so a
//! grammar-crate upgrade invalidates stale cache entries. This
//! matters because a grammar's *external scanner* (`scanner.c`) is
//! compiled separately and is invisible to every runtime
//! `Language` fingerprint (node-kind tables, ABI version, counts):
//! a scanner-only release can change parse output while leaving the
//! runtime fingerprint byte-identical. Pinning the crate version
//! closes that gap.
//!
//! When `Cargo.lock` is absent (a downstream crates.io build of
//! quarb-code as a library), the stamp falls back to "nolock"; the
//! cache stays sound within that build but cannot distinguish
//! grammar versions across it — such users should clear the cache
//! on a grammar upgrade. For the `qua` binary, built in this
//! workspace, the lock is always present.

use std::path::PathBuf;

const PKGS: &[&str] = &[
    "tree-sitter",
    "tree-sitter-rust",
    "tree-sitter-python",
    "tree-sitter-javascript",
    "tree-sitter-c",
];

fn main() {
    let stamp = find_lock()
        .and_then(|p| stamp_from_lock(&p))
        .unwrap_or_else(|| "nolock".to_string());
    println!("cargo:rustc-env=QUARB_GRAMMAR_STAMP={stamp}");
}

/// Walk up from the crate dir to the first `Cargo.lock`.
fn find_lock() -> Option<PathBuf> {
    let mut dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    loop {
        let lock = dir.join("Cargo.lock");
        if lock.is_file() {
            println!("cargo:rerun-if-changed={}", lock.display());
            return Some(lock);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Extract `name version` pairs for the grammar packages from a
/// `Cargo.lock` (TOML `[[package]]` blocks), without a TOML dep.
fn stamp_from_lock(path: &PathBuf) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut cur_name: Option<String> = None;
    let mut versions: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            cur_name = None;
        } else if let Some(rest) = line.strip_prefix("name = ") {
            cur_name = Some(rest.trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("version = ")
            && let Some(name) = &cur_name
            && PKGS.contains(&name.as_str())
        {
            versions.push((name.clone(), rest.trim_matches('"').to_string()));
        }
    }
    versions.sort();
    let stamp = versions
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    Some(if stamp.is_empty() {
        "nogrammar".into()
    } else {
        stamp
    })
}
