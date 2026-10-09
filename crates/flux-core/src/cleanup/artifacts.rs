//! The artifact rules (cut 9b, spec "Artifact rules" 1-4): a state record names the temporaries an operation left
//! behind as hex paths. Cleanup deletes only a name it can prove is this operation's own temporary.

use crate::state::{PARTIAL_INFIX, from_native_hex, id_after};
use flux_fs::{Code, DirHandle, FileType, FsError};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Why a recorded artifact name was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactProblem {
    /// The text is not something `native_hex` could have produced.
    NotHex,
    /// The path has a root or a prefix.
    Absolute,
    /// A `.`, `..` or otherwise non-normal component.
    BadComponent,
    /// The last component is not `<name>.flux-partial.<this operation's id>`.
    NotThisOperationsPartial,
}

/// Rules 1 and 2: a relative path of normal components whose last is `<name>.flux-partial.<operation_id>`.
pub fn validate(hex: &str, operation_id: &str) -> Result<PathBuf, ArtifactProblem> {
    let path = from_native_hex(hex).ok_or(ArtifactProblem::NotHex)?;
    let mut rebuilt = PathBuf::new();
    let mut last: Option<&OsStr> = None;
    for part in path.components() {
        match part {
            Component::RootDir | Component::Prefix(_) => return Err(ArtifactProblem::Absolute),
            Component::CurDir | Component::ParentDir => return Err(ArtifactProblem::BadComponent),
            Component::Normal(name) => {
                rebuilt.push(name);
                last = Some(name);
            }
        }
    }
    // `components` swallows an interior `.`; a path whose text does not survive the round trip held one (`PathBuf` equality compares components, so compare the text).
    if rebuilt.as_os_str() != path.as_os_str() {
        return Err(ArtifactProblem::BadComponent);
    }
    let last = last.ok_or(ArtifactProblem::BadComponent)?;
    if id_after(last, PARTIAL_INFIX) != Some(operation_id) {
        return Err(ArtifactProblem::NotThisOperationsPartial);
    }
    Ok(path)
}

/// What `remove_validated` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    Deleted,
    AlreadyGone,
}

fn not_found(e: &FsError) -> bool {
    e.source.kind() == std::io::ErrorKind::NotFound
}

/// Rules 3 and 4: open each directory component with `open_dir`, check the last is a regular file by `metadata`,
/// `remove_file` it. `NotFound` anywhere is `AlreadyGone`. A link, a directory or anything else is the error.
pub fn remove_validated<D: DirHandle>(dest: &D, rel: &Path) -> flux_fs::Result<Removed> {
    let parts: Vec<&OsStr> = rel.iter().collect();
    let Some((leaf, dirs)) = parts.split_last() else {
        return Err(FsError::new(
            Code::SafetyRejected,
            std::io::Error::other("an empty artifact path names nothing to remove"),
        ));
    };
    let mut held: Option<D> = None;
    for name in dirs {
        let next = match held.as_ref().unwrap_or(dest).open_dir(name) {
            Ok(next) => next,
            Err(e) if not_found(&e) => return Ok(Removed::AlreadyGone),
            Err(e) => return Err(e),
        };
        held = Some(next);
    }
    let dir = held.as_ref().unwrap_or(dest);
    match dir.metadata(leaf) {
        Ok(meta) if meta.file_type == FileType::File => {}
        Ok(_) => {
            return Err(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("a recorded artifact is not a regular file"),
            ));
        }
        Err(e) if not_found(&e) => return Ok(Removed::AlreadyGone),
        Err(e) => return Err(e),
    }
    match dir.remove_file(leaf) {
        Ok(()) => Ok(Removed::Deleted),
        Err(e) if not_found(&e) => Ok(Removed::AlreadyGone),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use crate::state::native_hex;
    use flux_fs::{DestinationRoot, FileSystem};

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const OTHER: &str = "fedcba9876543210fedcba9876543210";

    fn partial(dir: &str) -> String {
        format!("{dir}b.flux-partial.{ID}")
    }

    #[test]
    fn a_relative_partial_of_this_operation_validates() {
        let name = partial("sub/");
        assert_eq!(validate(&native_hex(Path::new(&name)), ID), Ok(PathBuf::from(&name)));
    }

    #[test]
    fn an_absolute_path_is_refused() {
        #[cfg(unix)]
        let name = format!("/etc/passwd.flux-partial.{ID}");
        #[cfg(windows)]
        let name = format!("C:\\x.flux-partial.{ID}");
        assert_eq!(validate(&native_hex(Path::new(&name)), ID), Err(ArtifactProblem::Absolute));
    }

    #[test]
    fn dot_dot_and_dot_and_empty_components_are_refused() {
        for dir in ["../", "sub/../", "./"] {
            let name = partial(dir);
            assert_eq!(
                validate(&native_hex(Path::new(&name)), ID),
                Err(ArtifactProblem::BadComponent),
                "{name}"
            );
        }
        assert_eq!(validate("2f2f", ID), Err(ArtifactProblem::NotHex));
        // `Path::components` drops an interior `.`, so `native_hex` cannot make one: spell the bytes out.
        let interior: String =
            format!("sub/./{}", partial("")).bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(validate(&interior, ID), Err(ArtifactProblem::BadComponent));
    }

    #[test]
    fn an_artifact_naming_another_operations_partial_or_a_user_file_is_refused() {
        for name in [format!("b.flux-partial.{OTHER}"), "report.doc".to_string()] {
            assert_eq!(
                validate(&native_hex(Path::new(&name)), ID),
                Err(ArtifactProblem::NotThisOperationsPartial),
                "{name}"
            );
        }
    }

    fn dest_with_partial() -> (FaultFs, PathBuf) {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dest")).unwrap();
        fs.create_dir(Path::new("/dest/sub")).unwrap();
        let file = PathBuf::from(format!("/dest/sub/b.flux-partial.{ID}"));
        fs.write_file(&file, b"x");
        (fs, file)
    }

    #[test]
    fn remove_validated_deletes_through_handles_and_tolerates_absence() {
        let (fs, file) = dest_with_partial();
        let dest = fs.destination_root(Path::new("/dest")).unwrap();
        let rel = PathBuf::from(partial("sub/"));

        assert_eq!(remove_validated(&dest, &rel).unwrap(), Removed::Deleted);
        assert!(!fs.exists(&file));
        assert_eq!(remove_validated(&dest, &rel).unwrap(), Removed::AlreadyGone);

        // The fake records `open_dir` as the `metadata` of the directory it opens.
        let calls = fs.calls();
        let opened = calls.iter().position(|c| c == "metadata(/dest/sub)").expect("sub opened");
        let removed = calls
            .iter()
            .position(|c| *c == format!("remove_file({})", file.display()))
            .expect("leaf removed");
        assert!(opened < removed, "{calls:?}");
        // A name that vanishes between the `metadata` and the `remove_file` is also `AlreadyGone`.
        fs.write_file(&file, b"x");
        fs.fail_kind("remove_file", Code::IoError, std::io::ErrorKind::NotFound);
        assert_eq!(remove_validated(&dest, &rel).unwrap(), Removed::AlreadyGone);
        // A missing directory component is also `AlreadyGone`.
        let gone = PathBuf::from(partial("nope/"));
        assert_eq!(remove_validated(&dest, &gone).unwrap(), Removed::AlreadyGone);
    }

    #[test]
    fn remove_validated_refuses_a_link_component_and_a_directory_leaf() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/dest")).unwrap();
        fs.add_symlink("/dest/sub");
        let dest = fs.destination_root(Path::new("/dest")).unwrap();
        let err = remove_validated(&dest, Path::new(&partial("sub/"))).unwrap_err();
        assert!(
            matches!(err.code, Code::SafetyRejected | Code::DestinationError),
            "{:?}",
            err.code
        );
        assert!(fs.exists("/dest/sub"));

        let leaf = format!("/dest/dir/b.flux-partial.{ID}");
        fs.create_dir(Path::new("/dest/dir")).unwrap();
        fs.create_dir(Path::new(&leaf)).unwrap();
        let err = remove_validated(&dest, Path::new(&partial("dir/"))).unwrap_err();
        assert!(
            matches!(err.code, Code::SafetyRejected | Code::DestinationError),
            "{:?}",
            err.code
        );
        assert!(fs.exists(&leaf));
    }
}
