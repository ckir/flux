//! Linux, macOS and Windows implementations of the `flux-fs` abstractions (spec §3.3).

pub mod std_fs;

pub use std_fs::{StdFile, StdFileSystem, StdReader};
