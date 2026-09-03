//! The backend-agnostic interactive Quarb session.
//!
//! The session logic — the `&N` / `&N!` / `&N#` macro history, its
//! resolution, and rendering coordination — is pure and depends on
//! two pluggable seams:
//!
//! - an [`Executor`]: where the arbor materializes and queries run.
//!   [`LocalExecutor`] runs in-process (the native full fleet, or the
//!   wasm text-format subset); a daemon executor (native, elsewhere)
//!   proxies to a resident process.
//! - a [`Store`]: where the session's durable state persists — a file
//!   store on native, a browser store (localStorage) on wasm, or
//!   [`MemStore`] for none.
//!
//! Results cross those seams as [`Cell`]s — node results already
//! rendered to their locator string, value results keeping their
//! type — because a raw `NodeId` is meaningless across a socket or
//! the JS boundary.

pub mod doc;
mod local;
mod session;
mod store;

pub use doc::{Doc, Options};

/// One mount source: a path, optionally under an explicit mount name
/// (the `NAME=TARGET` spelling); unnamed sources mount under their
/// file stem.
#[derive(Clone, Debug)]
pub struct MountSpec {
    pub name: Option<String>,
    pub path: std::path::PathBuf,
}

impl MountSpec {
    /// Parse a CLI argument: `NAME=TARGET` when the left side is a
    /// plain mount name (letters, digits, `_`, `-`), else a bare
    /// path.
    pub fn parse(arg: &str) -> MountSpec {
        if let Some((name, target)) = arg.split_once('=')
            && !name.is_empty()
            && !target.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            return MountSpec {
                name: Some(name.to_string()),
                path: std::path::PathBuf::from(target),
            };
        }
        MountSpec {
            name: None,
            path: std::path::PathBuf::from(arg),
        }
    }
}
pub use local::LocalExecutor;
pub use session::Session;
pub use store::{MemStore, SessionState, Store};
#[cfg(feature = "native")]
pub use store::FileStore;

use quarb::Value;

/// One result row, rendered so it can cross a process or JS boundary:
/// a node as its locator string, a value keeping its type.
#[derive(Clone, Debug)]
pub enum Cell {
    Node(String),
    Value(Value),
}

impl Cell {
    /// The line as printed in a terminal.
    pub fn display(&self) -> String {
        match self {
            Cell::Node(s) => s.clone(),
            // The Quarb form for a record or list, bare scalars
            // (ruling #51) — what qua prints.
            Cell::Value(v) => v.display_form(),
        }
    }
}

/// The materialization + query substrate. Implementations run a query
/// against a standing arbor and return rendered [`Cell`]s.
///
/// `query` is the whole text to run — the session prepends its macro
/// table (`def &N: …;`) so history resolves inline, which also lets it
/// cross a process boundary (the daemon) as plain text.
pub trait Executor {
    fn run(&self, query: &str) -> anyhow::Result<Vec<Cell>>;

    /// The unresolved external references the last `run` recorded:
    /// identifiers (URLs, for html) of documents queries referenced
    /// (`::href-->`) but could not reach. The engine performs no IO
    /// — the host may acquire these, remount, and re-run the line.
    /// Empty by default.
    fn refs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Run against a *freshly re-materialized* source — the `&N!`
    /// live reading. The default re-runs against the standing arbor
    /// (an immutable source never drifts); executors that can re-open
    /// the source override it so a live reading sees current data,
    /// diverging from the frozen `&N#`.
    fn run_fresh(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        self.run(query)
    }

    /// Run a query and render its results as exportable markup
    /// (`md`, `html`, `txt`) — structural through the text-level
    /// vocabulary for node results, projection lines otherwise.
    /// Unsupported by default.
    fn export(&self, _query: &str, _kind: &str) -> anyhow::Result<String> {
        anyhow::bail!("this session's executor does not support rendered export")
    }

    /// Tab-completion candidates at a byte cursor. The default
    /// answers the syntax tier (the stdlib registry needs no
    /// adapter), so even a daemon-backed session completes
    /// function names; executors with a live arbor override this
    /// with the data tier — real children, real annotations.
    fn complete(&self, text: &str, cursor: usize) -> Vec<quarb::complete::Candidate> {
        quarb::complete::complete(text, cursor)
    }
}

#[cfg(feature = "native")]
mod daemon;
#[cfg(feature = "native")]
pub use daemon::DaemonExecutor;

#[cfg(test)]
mod mount_spec_tests {
    use super::MountSpec;

    #[test]
    fn name_target_splits() {
        let spec = MountSpec::parse("t=data/titanic.csv");
        assert_eq!(spec.name.as_deref(), Some("t"));
        assert_eq!(spec.path.to_str(), Some("data/titanic.csv"));
        let spec = MountSpec::parse("births-2=x.json");
        assert_eq!(spec.name.as_deref(), Some("births-2"));
    }

    #[test]
    fn bare_paths_stay_bare() {
        for arg in [
            "titanic.csv",
            "git:.",
            // a left side that isn't a plain mount name is path text
            "a/b=c.json",
            "=x.json",
            "name=",
        ] {
            let spec = MountSpec::parse(arg);
            assert!(spec.name.is_none(), "{arg} should !split");
            assert_eq!(spec.path.to_str(), Some(arg));
        }
    }
}
