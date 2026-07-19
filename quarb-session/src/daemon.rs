//! The daemon-backed executor: reuse `qua`'s resident session.
//!
//! Rather than reimplement the socket, spawn, keying, TTL, and
//! security machinery, [`DaemonExecutor`] shells out to
//! `qua --resident`, which materializes the source once in a
//! background process keyed by the target set and answers later
//! queries from the standing arbor. `quai` sends the combined query
//! text (its macro table prepended) and wraps the rendered lines as
//! [`Cell`]s.
//!
//! The win is a *shared, persistent* hot arbor: it survives `quai`
//! exiting and can be shared with other clients (a Jupyter kernel).
//! For a single session over a RAM-sized source the in-process
//! [`LocalExecutor`](crate::LocalExecutor) is faster (no IPC); the
//! daemon earns its keep on expensive sources reused across runs.

use crate::{Cell, Executor};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub struct DaemonExecutor {
    /// The `qua` binary to drive.
    qua: PathBuf,
    /// The source paths (the resident session's identity).
    paths: Vec<PathBuf>,
    /// A pinned `--now` (reproducible; also keys the session). Omitted
    /// so the daemon reads the clock per query — which keeps the
    /// session reusable across `quai` restarts.
    pinned_now: Option<String>,
    /// Semantics flags forwarded verbatim (`--allow-shell`, …).
    flags: Vec<String>,
}

impl DaemonExecutor {
    pub fn new(
        paths: Vec<PathBuf>,
        pinned_now: Option<String>,
        allow_shell: bool,
        hidden: bool,
        no_ignore: bool,
        descend: bool,
        cache: bool,
    ) -> Result<Self> {
        let qua = resolve_qua();
        let mut flags = Vec::new();
        if allow_shell {
            flags.push("--allow-shell".to_string());
        }
        if hidden {
            flags.push("--hidden".to_string());
        }
        if no_ignore {
            flags.push("--no-ignore".to_string());
        }
        if descend {
            flags.push("--descend".to_string());
        }
        // Cache and daemon are layers, not alternatives: the resident
        // arbor's cold start reads parsed ASTs from the on-disk cache,
        // and populates it as it materializes.
        if cache {
            flags.push("--cache".to_string());
        }
        Ok(Self {
            qua,
            paths,
            pinned_now,
            flags,
        })
    }
}

/// Prefer a `qua` sitting beside the running binary (installed as a
/// pair); otherwise rely on `PATH`.
fn resolve_qua() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for name in ["qua", "qua.exe"] {
            let sib = dir.join(name);
            if sib.exists() {
                return sib;
            }
        }
    }
    PathBuf::from("qua")
}

impl DaemonExecutor {
    /// Invoke `qua`: `resident` hits the standing arbor (`&N`), while
    /// a one-shot (not resident) re-materializes the source live
    /// (`&N!`). Results are opaque rendered lines across this boundary
    /// (typed values are a later protocol upgrade for Jupyter).
    fn invoke(&self, resident: bool, query: &str) -> Result<Vec<Cell>> {
        let mut cmd = Command::new(&self.qua);
        if resident {
            cmd.arg("--resident");
        }
        if let Some(iso) = &self.pinned_now {
            cmd.arg("--now").arg(iso);
        }
        for f in &self.flags {
            cmd.arg(f);
        }
        cmd.arg(query);
        for p in &self.paths {
            cmd.arg(p);
        }
        let out = cmd.output().with_context(|| {
            format!(
                "invoking '{}' (is qua installed and on PATH?)",
                self.qua.display()
            )
        })?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| Cell::Node(l.to_string()))
            .collect())
    }
}

impl Executor for DaemonExecutor {
    fn run(&self, query: &str) -> Result<Vec<Cell>> {
        self.invoke(true, query)
    }

    fn run_fresh(&self, query: &str) -> Result<Vec<Cell>> {
        // A one-shot qua re-reads the source, bypassing the (fixed)
        // resident arbor — a genuinely live reading.
        self.invoke(false, query)
    }
}
