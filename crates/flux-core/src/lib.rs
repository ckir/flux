//! Flux engine (spec §3.1).

pub(crate) mod checkpoint;
pub mod cleanup;
pub mod copy;
pub mod dry_run;
pub mod ids;
pub mod lock;
mod names;
pub mod prior;
pub mod run;
pub mod state;
pub mod tree;
pub(crate) mod wal;
pub mod walk;

// Test-only. Nothing outside this crate uses the fake, so it is gated on `test`
// rather than on a `testing` feature that nothing would ever turn on -- and an
// undeclared feature in a `cfg` is a warning the repo's clippy gate would surface.
#[cfg(test)]
pub mod fault_fs;

pub use copy::{copy_file, copy_file_at};
pub use tree::{
    CheckpointDiscard, DegradedGroup, Discard, FailureTally, PartialResume, TreeAbort, TreeFailure,
    TreeFailureCause, TreeOutcome, WeakIdentityWarnings, copy_tree,
};
pub use walk::{DEFAULT_MAX_DEPTH, Walk, WalkError, WalkEvent, walk, walk_with_depth};
