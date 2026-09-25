//! Portable filesystem abstractions (spec §3.2).

pub mod error;

pub use error::{Code, FsError, Result};

pub mod options;

pub use options::{
    CopyOptions, Durability, MetadataFailure, MetadataItem, OperationId, Outcome, Preserve,
    Publish, temp_path,
};

pub mod fs;
pub mod name;

pub use fs::{
    DestinationRoot, DirEntry, DirHandle, FileHandle, FileIdentity, FileSystem, FileType, Metadata,
    ObjectId, Perms,
};
pub use name::check_component;
