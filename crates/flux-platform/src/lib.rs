//! Linux, macOS and Windows implementations of the `flux-fs` abstractions (spec §3.3).

pub mod std_fs;

#[cfg(unix)]
mod dir_unix;
#[cfg(unix)]
pub use dir_unix::StdDir;

pub use std_fs::{StdFile, StdFileSystem, StdReader};
