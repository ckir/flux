//! Linux, macOS and Windows implementations of the `flux-fs` abstractions (spec §3.3).

pub mod std_fs;

pub(crate) mod lock_file;

mod claims;
pub use claims::{CACHE_BYTES, RedbClaimStore, SYNC_CAP};

#[cfg(unix)]
mod dir_unix;
#[cfg(unix)]
pub use dir_unix::StdDir;

#[cfg(windows)]
mod dir_windows;
#[cfg(windows)]
pub use dir_windows::StdDir;

pub use lock_file::{StdLock, boot_session_id};
pub use std_fs::{StdFile, StdFileSystem, StdReader};

/// The process's soft limit on open file descriptors (`RLIMIT_NOFILE`), which a batched tree copy turns into a cap on
/// the writers it holds open (cut 9e). `None` on Windows, which has no equivalent soft limit for handles.
#[cfg(unix)]
pub fn soft_descriptor_limit() -> Option<u64> {
    rustix::process::getrlimit(rustix::process::Resource::Nofile).current
}

/// See the unix definition: Windows has no soft limit on handles, so there is nothing to answer.
#[cfg(windows)]
pub fn soft_descriptor_limit() -> Option<u64> {
    None
}

#[cfg(all(test, unix))]
mod descriptor_limit_tests {
    #[test]
    fn the_soft_descriptor_limit_is_known_and_at_least_64() {
        let n = super::soft_descriptor_limit();
        assert!(n.is_some_and(|n| n >= 64), "{n:?}");
    }
}
