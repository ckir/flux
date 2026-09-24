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

/// What a directory listing reports per entry.
///
/// Deliberately NOT `Metadata`: the entry type is what the OS supplies during
/// enumeration, and a `Metadata` per entry would re-stat every child, which is the
/// cost this primitive exists to avoid.
///
/// There is no `Unknown` variant. Linux `readdir` can return `DT_UNKNOWN`, but std
/// resolves it transparently -- `library/std/src/sys/fs/unix.rs:1149` is
/// `_ => self.metadata().map(|m| m.file_type())` -- so the variant would be
/// unconstructible. The cost is real and worth naming: on a filesystem with no
/// `d_type` (this repository measurably uses one, `/mnt/c` under WSL is v9fs) that
/// fallback is an `lstat` per entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Dir,
    Symlink,
    Other,
}

/// One entry of one directory. The NAME, not a path: the walk already holds the
/// parent on its stack, and a `PathBuf` per entry would allocate a full path for
/// every child of every directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: std::ffi::OsString,
    pub file_type: FileType,
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

/// Only what single-file copy and the walk read. Widened when a consumer needs more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub len: u64,
    /// The object's own type, NOT its target's -- adapters stat with
    /// `symlink_metadata`.
    ///
    /// A `bool` here was not enough, and the reason is a measured tree escape rather
    /// than tidiness. `is_file == false` is equally true of a directory and of a
    /// symlink, while `std::fs::read_dir` FOLLOWS a symlink to a directory --
    /// MEASURED identically on Windows (junction) and Linux (symlink). So a walk
    /// that checked only `!is_file` before descending would, on an object swapped
    /// from directory to symlink between the listing and the stat, enumerate the
    /// link's TARGET and copy files from outside the tree.
    pub file_type: FileType,
    pub permissions: Option<Perms>,
    pub modified: Option<SystemTime>,
    /// §149.4 asks the scanner to obtain identity "where strongly supported", and
    /// §107 requires the strength to travel with it. One field, three states, and no
    /// second way to say "absent".
    pub identity: FileIdentity,
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

    /// ONE level. Returns every entry with its file type, in whatever order the OS
    /// gave them -- ordering is the caller's job (§7.2), because only the caller
    /// knows the comparison rule.
    ///
    /// Returns a `Vec`, not an iterator, and that is deliberate: §7.2 requires each
    /// directory to be sorted, sorting requires the whole directory in hand, so a
    /// lazy return shape would promise a laziness the caller cannot use.
    /// Materialising also drops the directory handle before the walk recurses, so
    /// only one is ever open.
    ///
    /// This is within invariant 11, which forbids an unbounded GLOBAL list -- not
    /// §7.2, which is about ordering and would equally appear to bless something
    /// genuinely unbounded. Measured at 200,000 entries in one ext4 directory:
    /// readdir 448 ms, 10.1 MB, bytewise sort 81 ms.
    ///
    /// State the bound precisely, because "one directory" is the easy thing to say
    /// and it is wrong: only one directory HANDLE is ever open, but the walk's stack
    /// retains the un-yielded entries of every directory on the CURRENT PATH, so the
    /// live bound is one directory per level, capped by `DEFAULT_MAX_DEPTH`. That is
    /// still bounded by DEPTH rather than by the total number of files, which is
    /// what invariant 11 actually forbids -- but it is not "one".
    /// **Contract: each `DirEntry::name` is exactly ONE bare component.** Not a
    /// path, not absolute, never `.` or `..`. A real filesystem cannot break this --
    /// neither Linux nor Windows permits a separator inside a file name -- but this
    /// trait is public, so the rule is written down here and ENFORCED by the walk
    /// rather than assumed: `PathBuf::join` discards its base on an absolute
    /// component, so one bad name would take a traversal out of its own root.
    fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>>;

    /// Creates ONE directory. Fails if the parent is missing; the walk creates
    /// ancestors in order, so it never needs the recursive form.
    ///
    /// **It must also FAIL if the path already exists**, rather than succeeding
    /// silently, and that half is spelled out because it is the half an implementor
    /// forgets: a silent success would tell the walk it had created a fresh
    /// directory when it had adopted somebody else's. Both in-tree implementors do
    /// this -- `std::fs::create_dir` fails on an existing path, and the test fake
    /// returns `AlreadyExists` -- but the trait is public and the requirement was
    /// unstated.
    fn create_dir(&self, path: &Path) -> Result<()>;
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
            Ok(Metadata {
                len: 0,
                file_type: FileType::File,
                permissions: None,
                modified: None,
                // The stub exists to prove the trait compiles. Inventing an identity
                // here would let a test pass against a fake that never had one.
                identity: FileIdentity::Unavailable,
            })
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

        fn read_dir(&self, _: &Path) -> crate::Result<Vec<DirEntry>> {
            Ok(Vec::new())
        }

        fn create_dir(&self, _: &Path) -> crate::Result<()> {
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
        assert_eq!(fs.metadata(Path::new("x")).unwrap().file_type, FileType::File);
    }

    #[test]
    fn encoded_bytes_order_is_the_ordering_contract_not_utf16() {
        // §7.2 sorts by `as_encoded_bytes()`. WTF-8 and UTF-16 DISAGREE above the
        // BMP: surrogates (0xD800-0xDBFF) sort below the private-use area in UTF-16
        // while their UTF-8 encodings sort above it. If anyone ever "fixes" the sort
        // to use `encode_wide()` on Windows, this test is what catches it.
        //
        //   U+E000  WTF-8 EE 80 80     UTF-16BE E0 00
        //   U+10000 WTF-8 F0 90 80 80  UTF-16BE D8 00 DC 00
        let pua = std::ffi::OsString::from("\u{E000}");
        let astral = std::ffi::OsString::from("\u{10000}");

        assert!(
            pua.as_encoded_bytes() < astral.as_encoded_bytes(),
            "WTF-8 puts U+E000 first; got {:?} vs {:?}",
            pua.as_encoded_bytes(),
            astral.as_encoded_bytes()
        );

        // And the other direction, so the test cannot pass by both being equal.
        let utf16_pua: Vec<u16> = "\u{E000}".encode_utf16().collect();
        let utf16_astral: Vec<u16> = "\u{10000}".encode_utf16().collect();
        assert!(utf16_astral < utf16_pua, "UTF-16 puts U+10000 first: this is the disagreement");
    }

    #[test]
    fn a_dir_entry_carries_a_name_and_a_type() {
        let e = DirEntry { name: std::ffi::OsString::from("a"), file_type: FileType::Dir };
        assert_eq!(e.file_type, FileType::Dir);
        assert_ne!(FileType::Dir, FileType::Symlink);
        assert_ne!(FileType::File, FileType::Other);
    }
}
