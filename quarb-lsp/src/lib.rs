//! The transport plumbing quarb's language servers share.
//!
//! quarb-lsp (the Quarb language) and quarb-code-lsp (the code
//! level over source files) are separate server personalities —
//! one core each — speaking the same two codecs: LSP JSON-RPC
//! with Content-Length framing on stdio, and kaivrpc on a Unix
//! socket. The framing lives here once; each server owns its
//! method dispatch.

pub mod framing;
