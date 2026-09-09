//! Workspace-level integration tests.
//!
//! Spec §68.2 enumerates the required cases: single file, directory, nested
//! directory, multiple sources, existing destination, overwrite, update,
//! skip-existing, verification, timestamps, permissions, symlinks, Unicode,
//! spaces, newlines, zero-byte files, large files, sparse files, hardlinks,
//! excluded hardlink members, resume, interrupted copy, reflink, dry-run, JSON,
//! destination-inside-source, mount boundaries, disk full, stale operation
//! cleanup, operation lock contention.
//!
//! Each becomes a submodule declared here as it is implemented.

#[test]
fn harness_runs() {
    // Placeholder so the integration target is a real, running test binary from
    // day one. Delete once the first real case lands.
}
