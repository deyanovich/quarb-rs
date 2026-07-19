//! Where a session's durable state persists across runs.

/// The persistable part of a session: the macro history and the line
/// counter. Frozen snapshots stay in memory for now — they regenerate
/// on re-run, so persisting them is a later refinement.
#[derive(Clone, Default)]
pub struct SessionState {
    pub defs_text: String,
    pub line_no: usize,
}

/// A place to load and save [`SessionState`]. [`MemStore`] keeps
/// nothing (a fresh session each run); a file store (native) or a
/// browser store (wasm, localStorage) persists. The caller keys the
/// store by the session's source identity.
pub trait Store {
    fn load(&self) -> Option<SessionState>;
    fn save(&self, state: &SessionState) -> anyhow::Result<()>;
}

/// The ephemeral store: no persistence, a fresh session every time.
pub struct MemStore;

impl Store for MemStore {
    fn load(&self) -> Option<SessionState> {
        None
    }
    fn save(&self, _state: &SessionState) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A store persisting the macro history to a file under `~/.quarb`,
/// keyed by the source set — so restarting `quai` over the same
/// sources restores its `&N` history. (Native only; the wasm build
/// persists to localStorage instead.)
#[cfg(feature = "native")]
pub struct FileStore {
    path: std::path::PathBuf,
}

#[cfg(feature = "native")]
impl FileStore {
    /// A store keyed by the canonical form of `sources`.
    pub fn new(sources: &[std::path::PathBuf]) -> anyhow::Result<Self> {
        use std::hash::{Hash, Hasher};
        let dir = base_dir()?;
        std::fs::create_dir_all(&dir)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in sources {
            std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()).hash(&mut h);
        }
        Ok(FileStore {
            path: dir.join(format!("{:016x}.session", h.finish())),
        })
    }
}

/// The history directory: `$QUARB_CACHE_DIR/quai`, else
/// `~/.quarb/quai`, else a temp fallback.
#[cfg(feature = "native")]
fn base_dir() -> anyhow::Result<std::path::PathBuf> {
    let root = if let Some(d) = std::env::var_os("QUARB_CACHE_DIR") {
        std::path::PathBuf::from(d)
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".quarb")
    } else {
        std::env::temp_dir().join("quarb")
    };
    Ok(root.join("quai"))
}

#[cfg(feature = "native")]
impl Store for FileStore {
    fn load(&self) -> Option<SessionState> {
        // Line 1 is the counter; the rest is the macro table verbatim.
        let text = std::fs::read_to_string(&self.path).ok()?;
        let (first, rest) = text.split_once('\n')?;
        Some(SessionState {
            line_no: first.trim().parse().ok()?,
            defs_text: rest.to_string(),
        })
    }
    fn save(&self, state: &SessionState) -> anyhow::Result<()> {
        std::fs::write(&self.path, format!("{}\n{}", state.line_no, state.defs_text))?;
        Ok(())
    }
}
