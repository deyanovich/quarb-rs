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
            Cell::Value(v) => v.to_string(),
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

    /// Run against a *freshly re-materialized* source — the `&N!`
    /// live reading. The default re-runs against the standing arbor
    /// (an immutable source never drifts); executors that can re-open
    /// the source override it so a live reading sees current data,
    /// diverging from the frozen `&N#`.
    fn run_fresh(&self, query: &str) -> anyhow::Result<Vec<Cell>> {
        self.run(query)
    }
}

#[cfg(feature = "native")]
mod daemon;
#[cfg(feature = "native")]
pub use daemon::DaemonExecutor;
