//! §21.1: classify a destination's prior operations before this run creates its own (the run's step 4). A tree's are
//! the workspaces in `DEST/.flux/operations/`; a single file's are the `<target>.flux-state.<id>` records beside it.
//! The entry named with this run's own id is skipped: after a `--break-lock` takeover this run's CREATED state already
//! exists, and it is never a prior operation.

use crate::ids::is_id;
use crate::lock::error::refuse;
use crate::lock::{LockCode, LockError, LockResult};
use crate::state::{
    FLUX_DIR, Kind, MANIFEST, OPERATIONS_DIR, OperationState, RECORD_INFIX, Unusable,
    control_path_conflict, read_state,
};
use flux_fs::{Code, DirHandle, FileType};
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// A prior operation that can still be resumed: `RESUMABLE_OPERATION_EXISTS` without `--restart`, superseded with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorOp {
    pub state: OperationState,
    /// Its state's path, for messages: `DEST/.flux/operations/<id>/manifest`, or `<target>.flux-state.<id>`.
    pub shown: PathBuf,
}

/// What the scan found: every resumable prior operation, in name order. COMPLETED and ABANDONED ones are passed over
/// (§21.1: proceed; what they left is cut 9's).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub resumable: Vec<PriorOp>,
}

/// A tree's prior operations. `dest_shown` is DEST as messages name it.
pub fn scan_tree<D: DirHandle>(dest: &D, dest_shown: &Path, own_id: &str) -> LockResult<Scan> {
    let flux_shown = dest_shown.join(FLUX_DIR);
    let Some(flux) = existing_dir(dest, FLUX_DIR, &flux_shown)? else {
        return Ok(Scan::default());
    };
    let ops_shown = flux_shown.join(OPERATIONS_DIR);
    let Some(operations) = existing_dir(&flux, OPERATIONS_DIR, &ops_shown)? else {
        return Ok(Scan::default());
    };
    let mut entries = operations.read_dir()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut scan = Scan::default();
    for entry in entries {
        // Decision 3: an operation is a DIRECTORY named by an id. Anything else here - a crash's `<id>.creating`, a
        // stray file - is not one, and is left for cut 9's cleanup.
        let Some(id) = entry.name.to_str().filter(|n| is_id(n)) else { continue };
        if entry.file_type != FileType::Dir || id == own_id {
            continue;
        }
        let shown = ops_shown.join(id).join(MANIFEST);
        let workspace = match operations.open_dir(&entry.name) {
            Ok(d) => d,
            // Gone, or no longer a directory, since the listing: not an operation now (decision 9).
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => continue,
            Err(e) => return Err(e.into()),
        };
        let state = match read_state(&workspace, OsStr::new(MANIFEST)) {
            Ok(decoded) => usable(decoded, &shown)?,
            // §21.1's last row: a workspace with no manifest is missing state (§249.4).
            Err(e) if e.source.kind() == ErrorKind::NotFound => {
                return Err(corrupt(&shown, "the workspace has no manifest"));
            }
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                return Err(corrupt(
                    &shown,
                    &format!("the manifest is not a regular file: {}", e.source),
                ));
            }
            Err(e) => return Err(e.into()),
        };
        if state.operation_id != id || state.kind != Kind::Tree {
            return Err(corrupt(
                &shown,
                "the manifest names another operation, or a single-file one",
            ));
        }
        if state.state.is_resumable() {
            scan.resumable.push(PriorOp { state, shown });
        }
    }
    Ok(scan)
}

/// A single file's prior operations: the records `<target>.flux-state.<id>` in `parent`. `parent_shown` is the
/// target's directory as messages name it.
pub fn scan_file<D: DirHandle>(
    parent: &D,
    target: &OsStr,
    parent_shown: &Path,
    own_id: &str,
) -> LockResult<Scan> {
    let mut prefix = target.to_os_string();
    prefix.push(RECORD_INFIX);
    let mut entries = parent.read_dir()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut scan = Scan::default();
    for entry in entries {
        // Exactly `<target>.flux-state.<id>`: another target's record, or a temporary (`<id>.tmp`), is not one.
        let Some(rest) = entry.name.as_encoded_bytes().strip_prefix(prefix.as_encoded_bytes())
        else {
            continue;
        };
        let Some(id) = std::str::from_utf8(rest).ok().filter(|r| is_id(r)) else { continue };
        if id == own_id {
            continue;
        }
        let shown = parent_shown.join(&entry.name);
        // A record's exact name beside a user's target is Flux's (F2), and no crash leaves anything but a regular file
        // there: anything else is unreadable state (§21.1), refused rather than passed over (decision 9; panel round 3).
        if entry.file_type != FileType::File {
            return Err(corrupt(
                &shown,
                "a record's name holds something other than a regular file",
            ));
        }
        let state = match read_state(parent, &entry.name) {
            Ok(decoded) => usable(decoded, &shown)?,
            // Removed by its own finishing run since the listing: not a record now.
            Err(e) if e.source.kind() == ErrorKind::NotFound => continue,
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                return Err(corrupt(
                    &shown,
                    &format!("the record is not a regular file: {}", e.source),
                ));
            }
            Err(e) => return Err(e.into()),
        };
        if state.operation_id != id || state.kind != Kind::File {
            return Err(corrupt(&shown, "the record names another operation, or a tree"));
        }
        if state.state.is_resumable() {
            scan.resumable.push(PriorOp { state, shown });
        }
    }
    Ok(scan)
}

/// `RESUMABLE_OPERATION_EXISTS` for `op`, naming it and the way out (the refusal-guidance table).
pub fn resumable_refusal(op: &PriorOp) -> LockError {
    refuse(
        LockCode::ResumableOperationExists,
        None,
        format!(
            "operation {} is {} ({}); run again with --restart to supersede it (its partials are deleted)",
            op.state.operation_id,
            op.state.state.as_str(),
            op.shown.display()
        ),
    )
}

fn usable(decoded: Result<OperationState, Unusable>, shown: &Path) -> LockResult<OperationState> {
    match decoded {
        Ok(state) => Ok(state),
        Err(Unusable::Corrupt(why)) => Err(corrupt(shown, &why)),
        Err(Unusable::Incompatible(version)) => Err(refuse(
            LockCode::IncompatibleState,
            None,
            format!(
                "{} has format_version {version}: it was written by a newer Flux; use that version, or remove it by hand",
                shown.display()
            ),
        )),
    }
}

fn corrupt(shown: &Path, why: &str) -> LockError {
    refuse(
        LockCode::StateCorrupt,
        None,
        format!(
            "{}: {why}; Flux never deletes state it cannot read (§249.4): inspect it, and remove it by hand if it is not needed",
            shown.display()
        ),
    )
}

/// A control directory that may be absent: `None` if it is, a handle if it is a directory, and
/// `CONTROL_PLANE_NAMESPACE_CONFLICT` if something else holds the name.
fn existing_dir<D: DirHandle>(parent: &D, name: &str, shown: &Path) -> LockResult<Option<D>> {
    let name = OsStr::new(name);
    match parent.metadata(name) {
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) if m.file_type != FileType::Dir => Err(control_path_conflict(shown)),
        Ok(_) => match parent.open_dir(name) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
                Err(control_path_conflict(shown))
            }
            Err(e) => Err(e.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::test_support::refusal;
    use crate::state::{OpState, record_name};
    use flux_fs::{DestinationRoot, FileSystem};

    const OPS: &str = "/p/dest/.flux/operations";

    fn id(n: u8) -> String {
        format!("{n:032x}")
    }

    fn state(n: u8, kind: Kind, s: OpState) -> OperationState {
        OperationState {
            state: s,
            ..OperationState::created(&id(n), kind, Path::new("/p/dest"), 1)
        }
    }

    /// A fake with `/p/dest`; the handle is on it.
    fn dest() -> (FaultFs, FakeDirHandle) {
        let fs = FaultFs::new();
        for p in ["/p", "/p/dest"] {
            fs.create_dir(Path::new(p)).unwrap();
        }
        let d = fs.destination_root(Path::new("/p/dest")).unwrap();
        (fs, d)
    }

    fn operations(fs: &FaultFs) {
        for p in ["/p/dest/.flux", OPS] {
            fs.create_dir(Path::new(p)).unwrap();
        }
    }

    fn workspace(fs: &FaultFs, n: u8, manifest: Option<&[u8]>) {
        fs.create_dir(Path::new(&format!("{OPS}/{}", id(n)))).unwrap();
        if let Some(bytes) = manifest {
            fs.write_file(format!("{OPS}/{}/manifest", id(n)), bytes);
        }
    }

    fn scan(d: &FakeDirHandle, own: u8) -> LockResult<Scan> {
        scan_tree(d, Path::new("D"), &id(own))
    }

    fn ids(s: &Scan) -> Vec<String> {
        s.resumable.iter().map(|p| p.state.operation_id.clone()).collect()
    }

    #[test]
    fn no_control_directory_means_no_prior_operation() {
        let (fs, d) = dest();
        assert_eq!(scan(&d, 0).unwrap(), Scan::default());
        fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
        assert_eq!(scan(&d, 0).unwrap(), Scan::default(), ".flux without operations/");
    }

    #[test]
    fn a_foreign_object_at_a_control_path_is_a_conflict() {
        let (fs, d) = dest();
        fs.write_file("/p/dest/.flux", b"x");
        let r = refusal(scan(&d, 0));
        assert_eq!(r.code, LockCode::ControlPlaneNamespaceConflict);
        assert!(r.detail.contains(".flux"), "{}", r.detail);
        let (fs, d) = dest();
        fs.create_dir(Path::new("/p/dest/.flux")).unwrap();
        fs.write_file(OPS, b"x");
        assert_eq!(refusal(scan(&d, 0)).code, LockCode::ControlPlaneNamespaceConflict);
    }

    #[test]
    fn only_created_transferring_and_failed_operations_are_resumable() {
        let (fs, d) = dest();
        operations(&fs);
        for (n, s) in [
            (1, OpState::Created),
            (2, OpState::Transferring),
            (3, OpState::Failed),
            (4, OpState::Completed),
            (5, OpState::Abandoned),
            (6, OpState::Created),
        ] {
            workspace(&fs, n, Some(&state(n, Kind::Tree, s).encode()));
        }
        let found = scan(&d, 6).unwrap();
        assert_eq!(
            ids(&found),
            vec![id(1), id(2), id(3)],
            "COMPLETED, ABANDONED and this run's own are passed over"
        );
        let shown = found.resumable[0].shown.display().to_string();
        assert!(shown.contains(&id(1)) && shown.ends_with("manifest"), "{shown}");
    }

    #[test]
    fn what_is_not_a_directory_named_by_an_id_is_not_an_operation() {
        let (fs, d) = dest();
        operations(&fs);
        fs.create_dir(Path::new(&format!("{OPS}/junk"))).unwrap();
        fs.create_dir(Path::new(&format!("{OPS}/{}.creating", id(7)))).unwrap();
        fs.write_file(format!("{OPS}/{}", id(8)), &state(8, Kind::Tree, OpState::Created).encode());
        assert_eq!(scan(&d, 0).unwrap(), Scan::default());
    }

    #[test]
    fn a_workspace_with_no_manifest_is_state_corrupt() {
        let (fs, d) = dest();
        operations(&fs);
        workspace(&fs, 9, None);
        let r = refusal(scan(&d, 0));
        assert_eq!(r.code, LockCode::StateCorrupt);
        assert!(r.detail.contains(&id(9)) && r.detail.contains("no manifest"), "{}", r.detail);
        assert!(r.detail.contains("remove it by hand"), "{}", r.detail);
    }

    #[test]
    fn an_unreadable_newer_or_mismatched_manifest_is_refused() {
        for (bytes, code) in [
            (b"garbage".to_vec(), LockCode::StateCorrupt),
            (br#"{"format_version":2}"#.to_vec(), LockCode::IncompatibleState),
            (state(3, Kind::Tree, OpState::Created).encode(), LockCode::StateCorrupt),
            (state(4, Kind::File, OpState::Created).encode(), LockCode::StateCorrupt),
        ] {
            let (fs, d) = dest();
            operations(&fs);
            workspace(&fs, 4, Some(&bytes));
            let r = refusal(scan(&d, 0));
            assert_eq!(r.code, code, "{}", r.detail);
            if code == LockCode::IncompatibleState {
                assert!(r.detail.contains("format_version 2"), "{}", r.detail);
            }
        }
    }

    #[test]
    fn a_corrupt_state_is_refused_even_beside_a_resumable_one() {
        let (fs, d) = dest();
        operations(&fs);
        workspace(&fs, 1, Some(&state(1, Kind::Tree, OpState::Created).encode()));
        workspace(&fs, 2, Some(b"garbage"));
        assert_eq!(refusal(scan(&d, 0)).code, LockCode::StateCorrupt);
    }

    #[test]
    fn a_single_file_scan_reads_only_its_targets_records() {
        let (fs, d) = dest();
        let put = |name: String, s: &OperationState| {
            fs.write_file(format!("/p/dest/{name}"), &s.encode())
        };
        let rec = |n: u8| record_name(OsStr::new("t"), &id(n)).into_string().unwrap();
        put(rec(1), &state(1, Kind::File, OpState::Created));
        put(rec(2), &state(2, Kind::File, OpState::Completed));
        put(format!("u.flux-state.{}", id(3)), &state(3, Kind::File, OpState::Created));
        put(format!("{}.tmp", rec(4)), &state(4, Kind::File, OpState::Created));
        put(format!("tt.flux-state.{}", id(5)), &state(5, Kind::File, OpState::Created));
        put(rec(6), &state(6, Kind::File, OpState::Created));
        let found = scan_file(&d, OsStr::new("t"), Path::new("P"), &id(6)).unwrap();
        assert_eq!(ids(&found), vec![id(1)]);
        assert!(found.resumable[0].shown.ends_with(rec(1)));

        fs.write_file(format!("/p/dest/{}", rec(7)), b"garbage");
        assert_eq!(
            refusal(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(6))).code,
            LockCode::StateCorrupt
        );
        let (fs, d) = dest();
        fs.write_file(
            format!("/p/dest/{}", rec(8)),
            &state(8, Kind::Tree, OpState::Created).encode(),
        );
        assert_eq!(
            refusal(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(0))).code,
            LockCode::StateCorrupt,
            "a tree's state in a single file's record"
        );
        let (fs, d) = dest();
        fs.create_dir(Path::new(&format!("/p/dest/{}", rec(9)))).unwrap();
        assert_eq!(
            refusal(scan_file(&d, OsStr::new("t"), Path::new("P"), &id(0))).code,
            LockCode::StateCorrupt,
            "a directory at a record's exact name is never passed over"
        );
    }

    fn rec(target: &OsStr, n: u8) -> PathBuf {
        Path::new("/p/dest").join(record_name(target, &id(n)))
    }

    #[test]
    fn a_single_files_record_naming_another_operation_or_written_by_a_newer_flux_is_refused() {
        let t = OsStr::new("t");
        for (bytes, code) in [
            (state(3, Kind::File, OpState::Created).encode(), LockCode::StateCorrupt),
            (br#"{"format_version":2}"#.to_vec(), LockCode::IncompatibleState),
        ] {
            let (fs, d) = dest();
            fs.write_file(rec(t, 4), &bytes);
            let r = refusal(scan_file(&d, t, Path::new("P"), &id(0)));
            assert_eq!(r.code, code, "{}", r.detail);
            assert!(r.detail.contains(&id(4)), "{}", r.detail);
        }
    }

    #[test]
    fn what_changes_after_the_listing_is_passed_over() {
        // Decision 9: a workspace that is no longer a directory when it is opened.
        let (fs, d) = dest();
        operations(&fs);
        workspace(&fs, 1, Some(&state(1, Kind::Tree, OpState::Created).encode()));
        workspace(&fs, 2, Some(&state(2, Kind::Tree, OpState::Created).encode()));
        fs.set_type(format!("{OPS}/{}", id(2)), FileType::File);
        assert_eq!(ids(&scan(&d, 0).unwrap()), vec![id(1)]);

        // A record its own finishing run removed between the listing and the read.
        let (fs, d) = dest();
        let t = OsStr::new("t");
        for n in [1, 2] {
            fs.write_file(rec(t, n), &state(n, Kind::File, OpState::Created).encode());
        }
        let gone = rec(t, 1);
        fs.on_nth("read_file", 1, move |fs| fs.remove_file(&gone).unwrap());
        assert_eq!(ids(&scan_file(&d, t, Path::new("P"), &id(0)).unwrap()), vec![id(2)]);
    }

    #[test]
    fn a_manifest_that_is_not_a_regular_file_is_state_corrupt() {
        for link in [false, true] {
            let (fs, d) = dest();
            operations(&fs);
            workspace(&fs, 1, None);
            let manifest = format!("{OPS}/{}/manifest", id(1));
            if link {
                fs.add_symlink(&manifest);
            } else {
                fs.create_dir(Path::new(&manifest)).unwrap();
            }
            let r = refusal(scan(&d, 0));
            assert_eq!(r.code, LockCode::StateCorrupt, "link {link}: {}", r.detail);
            assert!(r.detail.contains("not a regular file"), "link {link}: {}", r.detail);
        }
    }

    #[test]
    fn a_non_utf8_targets_records_are_found() {
        #[cfg(unix)]
        let target = {
            use std::os::unix::ffi::OsStrExt;
            OsStr::from_bytes(b"t\xff").to_os_string()
        };
        #[cfg(windows)]
        let target = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[u16::from(b't'), 0xD800])
        };
        assert!(target.to_str().is_none());
        let (fs, d) = dest();
        fs.write_file(rec(&target, 1), &state(1, Kind::File, OpState::Created).encode());
        assert_eq!(ids(&scan_file(&d, &target, Path::new("P"), &id(0)).unwrap()), vec![id(1)]);
    }

    #[test]
    fn a_resumable_refusal_names_the_operation_and_the_way_out() {
        let op = PriorOp {
            state: state(1, Kind::Tree, OpState::Failed),
            shown: PathBuf::from("D/.flux/operations/x/manifest"),
        };
        let r = refusal::<()>(Err(resumable_refusal(&op)));
        assert_eq!(r.code, LockCode::ResumableOperationExists);
        for part in [id(1).as_str(), "FAILED", "--restart", "manifest"] {
            assert!(r.detail.contains(part), "{part}: {}", r.detail);
        }
    }
}
