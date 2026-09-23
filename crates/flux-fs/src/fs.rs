//! The portable filesystem surface. Only what single-file copy needs (§3.2).

use crate::Result;
use std::io::{Read, Write};
use std::path::Path;
use std::time::SystemTime;

/// Permissions in the only form this cut needs: the Unix mode, or the Windows
/// read-only flag. Deliberately not `std::fs::Permissions`, which cannot be
/// constructed portably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perms {
    UnixMode(u32),
    ReadOnly(bool),
}

/// Portable filesystem object identity (§109).
///
/// BOTH fields, always. §109 says "Never compare only inode" and "only file ID",
/// and requires `filesystem_id + object_id`, so that two unrelated objects on
/// different filesystems are never merged into one identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    /// Unix `st_dev`; Windows `VolumeSerialNumber`.
    pub volume: u64,
    /// Unix `st_ino`, zero-extended; Windows the 128-bit `FileId`.
    ///
    /// `u128` because Windows needs it: the 64-bit file index std exposes is not
    /// unique on ReFS, and this repository's own working tree is a ReFS Dev Drive.
    pub index: u128,
}

/// An `ObjectId` together with how far it can be trusted (§107).
///
/// §107: "A `FileIdentity` must therefore be accompanied by its reliability
/// classification." Three states, and deliberately NOT `Option<ObjectId>`: an
/// `Option` collapses *weak* into *strong*, which is precisely the failure §107
/// exists to prevent. FAT32, exFAT and some SMB and NFS configurations produce an
/// identity that exists and cannot be trusted.
///
/// Callers that act on identity for SAFETY must act on `Strong` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIdentity {
    Strong(ObjectId),
    Weak(ObjectId),
    Unavailable,
}

/// Only what single-file copy reads. Widened when a consumer needs more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub len: u64,
    /// False for directories, symlinks, devices, FIFOs and sockets.
    pub is_file: bool,
    pub permissions: Option<Perms>,
    pub modified: Option<SystemTime>,
}

/// A handle open for writing. Deliberately NOT `Read`: see `FileSystem::Reader`.
pub trait FileHandle: Write {
    fn sync_all(&self) -> Result<()>;
}

pub trait FileSystem: Send + Sync {
    /// A handle open for reading, and only reading.
    ///
    /// Two associated types rather than one, because with a single
    /// `type File: Read + Write` the handle returned by `open_read` accepts
    /// `write_all` -- it compiles and fails at run time on a read-only descriptor.
    /// `copy_file` is generic over this trait, so within it `Self::Reader` is known
    /// only to be `Read`, and writing to the source is a compile error even where
    /// the concrete reader type happens to implement `Write` as well.
    ///
    /// The cost is that `set_times` and `set_permissions` take `&Self::Writer`. If a
    /// later requirement needs to modify the SOURCE's metadata, it needs its own
    /// methods rather than reusing these.
    type Reader: Read;

    /// A handle open for writing, used for the temporary.
    type Writer: FileHandle;

    fn open_read(&self, path: &Path) -> Result<Self::Reader>;

    /// Exclusive create: fails if the path exists. FS-1 measured that a second
    /// exclusive create fails, which is what makes the temporary safe.
    fn create_new(&self, path: &Path) -> Result<Self::Writer>;

    fn metadata(&self, path: &Path) -> Result<Metadata>;

    /// On the HANDLE, not the path: the temporary must be fully prepared before
    /// publication, and addressing it by path in between is a race. FS-4 and FS-5
    /// measured that a handle survives both rename and unlink.
    fn set_times(&self, file: &Self::Writer, modified: Option<SystemTime>) -> Result<()>;

    fn set_permissions(&self, file: &Self::Writer, perms: Option<Perms>) -> Result<()>;

    /// Publishes over an existing target.
    fn rename_replace(&self, from: &Path, to: &Path) -> Result<()>;

    /// Refuses to replace. §18.1 publishes this way when the target was planned as
    /// new; FS-2 measured the failure.
    fn rename_no_replace(&self, from: &Path, to: &Path) -> Result<()>;

    fn remove_file(&self, path: &Path) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    struct NullReader;

    impl Read for NullReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }

    struct NullWriter;

    impl Write for NullWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl FileHandle for NullWriter {
        fn sync_all(&self) -> crate::Result<()> {
            Ok(())
        }
    }

    struct NullFs;

    impl FileSystem for NullFs {
        type Reader = NullReader;
        type Writer = NullWriter;

        fn open_read(&self, _: &Path) -> crate::Result<Self::Reader> {
            Ok(NullReader)
        }

        fn create_new(&self, _: &Path) -> crate::Result<Self::Writer> {
            Ok(NullWriter)
        }

        fn metadata(&self, _: &Path) -> crate::Result<Metadata> {
            Ok(Metadata { len: 0, is_file: true, permissions: None, modified: None })
        }

        fn set_times(
            &self,
            _: &Self::Writer,
            _: Option<std::time::SystemTime>,
        ) -> crate::Result<()> {
            Ok(())
        }

        fn set_permissions(&self, _: &Self::Writer, _: Option<Perms>) -> crate::Result<()> {
            Ok(())
        }

        fn rename_replace(&self, _: &Path, _: &Path) -> crate::Result<()> {
            Ok(())
        }

        fn rename_no_replace(&self, _: &Path, _: &Path) -> crate::Result<()> {
            Ok(())
        }

        fn remove_file(&self, _: &Path) -> crate::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn identity_distinguishes_strength_and_object() {
        let a = ObjectId { volume: 1, index: 2 };
        let b = ObjectId { volume: 1, index: 3 };

        // Same object, different reliability, is NOT the same identity. This is the
        // whole reason §107 makes strength part of the value rather than a sidecar.
        assert_ne!(FileIdentity::Strong(a), FileIdentity::Weak(a));
        // Different objects never compare equal.
        assert_ne!(FileIdentity::Strong(a), FileIdentity::Strong(b));
        assert_eq!(FileIdentity::Strong(a), FileIdentity::Strong(a));
        assert_eq!(FileIdentity::Unavailable, FileIdentity::Unavailable);

        // §109: both halves participate. An equal index on another volume is a
        // different object, and nothing may compare on the index alone.
        assert_ne!(ObjectId { volume: 1, index: 2 }, ObjectId { volume: 9, index: 2 });
    }

    #[test]
    fn a_trivial_implementation_compiles_and_is_object_safe_enough_to_use() {
        let fs = NullFs;
        assert!(fs.metadata(Path::new("x")).unwrap().is_file);
    }
}
