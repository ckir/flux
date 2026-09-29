//! Linux, macOS and Windows implementations of the `flux-fs` abstractions (spec §3.3).

pub mod std_fs;

mod lock_file;

#[cfg(unix)]
mod dir_unix;
#[cfg(unix)]
pub use dir_unix::StdDir;

#[cfg(windows)]
mod dir_windows;
#[cfg(windows)]
pub use dir_windows::StdDir;

pub use lock_file::StdLock;
pub use std_fs::{StdFile, StdFileSystem, StdReader};
