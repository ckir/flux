//! Flux engine (spec §3.1).

pub mod copy;
pub mod walk;

// Test-only. Nothing outside this crate uses the fake, so it is gated on `test`
// rather than on a `testing` feature that nothing would ever turn on -- and an
// undeclared feature in a `cfg` is a warning the repo's clippy gate would surface.
#[cfg(test)]
pub mod fault_fs;

pub use copy::copy_file;
pub use walk::{DEFAULT_MAX_DEPTH, Walk, WalkError, WalkEvent, walk, walk_with_depth};
