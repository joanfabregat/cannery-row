//! Maintained tar/gzip decoding with application extraction policy.
mod extract;
pub use extract::{CodeLimits, ExtractError, extract_into, extract_tree};
