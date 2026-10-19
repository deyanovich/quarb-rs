//! The value layer: the value space (null, bool, int, float, text,
//! list, record, table, foreign) with numbers, quantities, locales,
//! transliteration and encodings. Below the syntax: the literals an
//! AST holds are these values. No node, no arbor, no thread.
mod space;
pub use space::*;
mod words;
pub(crate) use words::*;
pub mod accents;
pub mod ambient;
pub mod bibref;
pub mod encoding;
pub mod locale;
pub mod number;
pub mod quantity;
pub mod table;
pub mod translit;
