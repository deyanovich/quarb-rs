//! Re-export of the koine renderer, which moved into the engine
//! core when `| markdown` / `| html` / `| atrep` became pipeline
//! stages (the stage and the export button are the same verb).
//! The extension, the wasm builds, and the notebook exporters
//! keep their `quarb_text::render_*` paths through this shim.
pub use quarb::koine::{Render, render_node, render_nodes, render_values};
