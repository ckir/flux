//! Where a target's lock lives, what it is called, the keys recorded in it, and which workspaces its record may name.

use super::error::{LockCode, LockResult, refuse};
use super::record::LockRecord;
use flux_fs::{DirHandle, FileIdentity, FileType};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;

/// `P/<T-name>.flux-lock` (§96.1).
pub const LOCK_SUFFIX: &str = ".flux-lock";
/// `T/.flux-root.lock` for a filesystem root (§96.1, spec:4738-4748).
pub const ROOT_LOCK_NAME: &str = ".flux-root.lock";
/// The directory-lock acquirer's name, whose presence a per-name acquirer checks (`S96_1_dircheck`).
pub const DIR_LOCK_NAME: &str = ".flux-dir.lock";
/// The longest lock name 7a creates: 255 bytes on Unix, 255 UTF-16 units on Windows. Longer needs §96.1's
/// directory-lock fallback, which 7a does not implement (`PATH_COMPONENT_INVALID`, refinement 6).
pub const NAME_LIMIT: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    /// A directory target `T`: the lock is `P/<T>.flux-lock`, the workspace `T/.flux/operations/<id>/`.
    Directory,
    /// A filesystem-root target `T`: the lock is `T/.flux-root.lock`.
    Root,
    /// A single-file target: the lock is `P/<target>.flux-lock`, the state `P/<target>.flux-state.<id>`.
    File,
}

/// What a trusted, dead owner's recorded workspace looks like on disk (§120).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Workspace {
    Trusted,
    Missing,
    Untrusted,
}

/// A target's lock: the directory that holds the lock file, its name, and the target's own name.
pub struct LockSite<'a, D: DirHandle> {
    dir: &'a D,
    lock_name: OsString,
    target_name: OsString,
    kind: SiteKind,
}

impl<'a, D: DirHandle> LockSite<'a, D> {
    /// A directory target `dest_name` inside `parent`.
    pub fn directory(parent: &'a D, dest_name: &OsStr) -> LockResult<Self> {
        Self::named(parent, dest_name, SiteKind::Directory)
    }

    /// A single-file target `target_name` inside `parent`.
    pub fn file(parent: &'a D, target_name: &OsStr) -> LockResult<Self> {
        Self::named(parent, target_name, SiteKind::File)
    }

    /// A filesystem-root target, whose lock lives inside it.
    pub fn root(dest: &'a D) -> Self {
        Self {
            dir: dest,
            lock_name: ROOT_LOCK_NAME.into(),
            target_name: OsString::new(),
            kind: SiteKind::Root,
        }
    }

    fn named(dir: &'a D, name: &OsStr, kind: SiteKind) -> LockResult<Self> {
        let mut lock_name = name.to_os_string();
        lock_name.push(LOCK_SUFFIX);
        if name_len(&lock_name) > NAME_LIMIT {
            return Err(refuse(
                LockCode::PathComponentInvalid,
                None,
                format!(
                    "the lock name {} is longer than {NAME_LIMIT} units; the directory-lock fallback is not in this version",
                    lock_name.to_string_lossy()
                ),
            ));
        }
        Ok(Self { dir, lock_name, target_name: name.to_os_string(), kind })
    }

    pub fn dir(&self) -> &'a D {
        self.dir
    }

    pub fn lock_name(&self) -> &OsStr {
        &self.lock_name
    }

    pub fn kind(&self) -> SiteKind {
        self.kind
    }

    /// `<lock-name>.broken.<operation-id>`: where §240.3 step 2 moves a dead owner's lock.
    pub fn broken_name(&self, operation_id: &str) -> OsString {
        let mut n = self.lock_name.clone();
        n.push(".broken.");
        n.push(operation_id);
        n
    }

    /// The target's final name in `FluxPathKey` encoding, as lowercase hex (decision 1). Empty for a root.
    pub fn target_path_key(&self) -> String {
        hex(self.target_name.as_encoded_bytes())
    }

    /// §259.6's `K`: the lock directory's physical identity where it is strong, then the target key (decision 1).
    pub fn complete_lock_key(&self) -> LockResult<String> {
        let prefix = match self.dir.identity()? {
            FileIdentity::Strong(o) => format!("{:016x}:{:032x}", o.volume, o.index),
            FileIdentity::Weak(_) | FileIdentity::Unavailable => "none".to_string(),
        };
        Ok(format!("{prefix}/{}", self.target_path_key()))
    }

    /// `S96_1_dircheck`: a per-name acquirer checks that `P/.flux-dir.lock` is absent. A root's lock is not per-name.
    pub(crate) fn dir_lock_present(&self) -> LockResult<bool> {
        if self.kind == SiteKind::Root {
            return Ok(false);
        }
        match self.dir.metadata(OsStr::new(DIR_LOCK_NAME)) {
            Ok(_) => Ok(true),
            Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// §120: is the dead owner's recorded `workspace_path` one Flux trusts, and is it there (decision 10)?
    pub(crate) fn workspace(&self, record: &LockRecord) -> LockResult<Workspace> {
        let id = record.operation_id.as_str();
        if !crate::ids::is_id(id) {
            return Ok(Workspace::Untrusted);
        }
        let path = record.workspace_path.as_str();
        if path == "none" {
            // A cleanup lock names no workspace (§259.6); a dead one is recovered like any other (decision 5).
            return Ok(Workspace::Trusted);
        }
        let present = match self.kind {
            SiteKind::Directory | SiteKind::Root if path == format!("operations/{id}") => {
                self.operation_dir_exists(id)?
            }
            SiteKind::File if path == format!("adjacent/{id}") => {
                let mut state = self.target_name.clone();
                state.push(".flux-state.");
                state.push(id);
                match self.dir.metadata(&state) {
                    Ok(m) => m.file_type == FileType::File,
                    Err(e) if e.source.kind() == ErrorKind::NotFound => false,
                    Err(e) => return Err(e.into()),
                }
            }
            _ => return Ok(Workspace::Untrusted),
        };
        Ok(if present { Workspace::Trusted } else { Workspace::Missing })
    }

    /// `T/.flux/operations/<id>/`, each level looked up by metadata first so a missing level is `false`, not an error.
    fn operation_dir_exists(&self, id: &str) -> LockResult<bool> {
        let dest_owned;
        let dest: &D = match self.kind {
            SiteKind::Root => self.dir,
            _ => {
                if !has_dir(self.dir, &self.target_name)? {
                    return Ok(false);
                }
                dest_owned = self.dir.open_dir(&self.target_name)?;
                &dest_owned
            }
        };
        if !has_dir(dest, OsStr::new(".flux"))? {
            return Ok(false);
        }
        let flux = dest.open_dir(OsStr::new(".flux"))?;
        if !has_dir(&flux, OsStr::new("operations"))? {
            return Ok(false);
        }
        let operations = flux.open_dir(OsStr::new("operations"))?;
        has_dir(&operations, OsStr::new(id))
    }
}

fn has_dir<D: DirHandle>(d: &D, name: &OsStr) -> LockResult<bool> {
    match d.metadata(name) {
        Ok(m) => Ok(m.file_type == FileType::Dir),
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(windows)]
fn name_len(name: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt;
    name.encode_wide().count()
}

#[cfg(not(windows))]
fn name_len(name: &OsStr) -> usize {
    name.as_encoded_bytes().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::test_support::{fake, record, refusal};
    use flux_fs::{DirHandle, FileSystem};
    use std::path::Path;

    #[test]
    fn lock_names_follow_the_target() {
        let (_fs, d) = fake();
        let dir = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert_eq!(dir.lock_name(), OsStr::new("dest.flux-lock"));
        assert_eq!(dir.broken_name("ab"), OsString::from("dest.flux-lock.broken.ab"));
        assert_eq!(
            LockSite::file(&d, OsStr::new("t.bin")).unwrap().lock_name(),
            OsStr::new("t.bin.flux-lock")
        );
        assert_eq!(LockSite::root(&d).lock_name(), OsStr::new(".flux-root.lock"));
    }

    #[test]
    fn a_lock_name_over_the_limit_is_path_component_invalid() {
        let (_fs, d) = fake();
        let fits = "a".repeat(NAME_LIMIT - LOCK_SUFFIX.len());
        assert!(
            LockSite::directory(&d, OsStr::new(&fits)).is_ok(),
            "exactly {NAME_LIMIT} units fits"
        );
        let over = "a".repeat(NAME_LIMIT - LOCK_SUFFIX.len() + 1);
        assert_eq!(
            refusal(LockSite::directory(&d, OsStr::new(&over))).code,
            LockCode::PathComponentInvalid
        );
    }

    #[test]
    fn the_keys_are_hex_and_carry_the_directory_identity() {
        let (_fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert_eq!(site.target_path_key(), "64657374", "hex of b\"dest\"");
        let FileIdentity::Strong(o) = d.identity().unwrap() else {
            panic!("the fake's directories are strong")
        };
        assert_eq!(
            site.complete_lock_key().unwrap(),
            format!("{:016x}:{:032x}/64657374", o.volume, o.index)
        );
        assert!(
            LockSite::root(&d).complete_lock_key().unwrap().ends_with('/'),
            "a root's target key is empty"
        );
    }

    #[test]
    fn the_directory_lock_check_sees_only_a_per_name_site() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert!(!site.dir_lock_present().unwrap());
        fs.write_file("/p/.flux-dir.lock", b"");
        assert!(site.dir_lock_present().unwrap());
        assert!(
            !LockSite::root(&d).dir_lock_present().unwrap(),
            "a root lock has no per-name acquirer"
        );
    }

    #[test]
    fn a_workspace_is_trusted_only_in_its_exact_derived_form() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        let id = crate::ids::new_id();
        let ops = format!("operations/{id}");
        assert_eq!(site.workspace(&record(&site, &id, &ops)).unwrap(), Workspace::Missing);
        for dir in [
            "/p/dest",
            "/p/dest/.flux",
            "/p/dest/.flux/operations",
            &format!("/p/dest/.flux/operations/{id}"),
        ] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
        assert_eq!(site.workspace(&record(&site, &id, &ops)).unwrap(), Workspace::Trusted);
        assert_eq!(
            site.workspace(&record(&site, &id, "none")).unwrap(),
            Workspace::Trusted,
            "a cleanup lock"
        );
        let other = crate::ids::new_id();
        assert_eq!(
            site.workspace(&record(&site, &id, &format!("operations/{other}"))).unwrap(),
            Workspace::Untrusted,
            "another operation's workspace"
        );
        assert_eq!(
            site.workspace(&record(&site, &id, &format!("adjacent/{id}"))).unwrap(),
            Workspace::Untrusted
        );
        assert_eq!(
            site.workspace(&record(&site, "../x", "operations/../x")).unwrap(),
            Workspace::Untrusted,
            "an operation id that is not an id is never a path"
        );
    }

    #[test]
    fn a_single_files_workspace_is_its_adjacent_state_record() {
        let (fs, d) = fake();
        let site = LockSite::file(&d, OsStr::new("t.bin")).unwrap();
        let id = crate::ids::new_id();
        let adjacent = format!("adjacent/{id}");
        assert_eq!(site.workspace(&record(&site, &id, &adjacent)).unwrap(), Workspace::Missing);
        fs.write_file(format!("/p/t.bin.flux-state.{id}"), b"{}");
        assert_eq!(site.workspace(&record(&site, &id, &adjacent)).unwrap(), Workspace::Trusted);
        assert_eq!(
            site.workspace(&record(&site, &id, &format!("operations/{id}"))).unwrap(),
            Workspace::Untrusted
        );
    }
}
