//! Shared package resolution for command-line tools and the language server.
pub mod pm;

// The compiler binary supplies its tracked reader for build fingerprints.
// Read-only library consumers resolve directly from disk on each request.
mod build_cache {
    pub use std::fs::read_to_string;
}
