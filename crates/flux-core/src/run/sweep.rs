// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! J1: the deletion of superseded operations' partials. The `--restart` sweep (`TreePlace::sweep`) and, from cut 9b,
//! `flux cleanup` share it.

use super::RunWarning;
use super::session::Fault;
use crate::state::{PARTIAL_INFIX, id_after};
use crate::tree::reserved_path;
use crate::walk::{WalkEvent, walk};
use flux_fs::{DestinationRoot, DirHandle};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// J1: delete every `<name>.flux-partial.<id>` for `ids` under `dest`, found by a walk of `dest_shown` that skips the reserved
/// control directories, through handles one component at a time. `check` runs before every deletion. `on_removed` is called with
/// each partial's path (`dest_shown` joined with the path below it) after it was deleted. Returns the ids whose partials are all
/// gone; a partial that stays is reported through `kept` with the path, the error and `shown(id)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sweep_partials<F: DestinationRoot>(
    fs: &F,
    dest: &F::Dir,
    dest_shown: &Path,
    operations_shown: &Path,
    ids: &BTreeSet<&str>,
    shown: &dyn Fn(&str) -> PathBuf,
    check: &mut dyn FnMut() -> Result<(), Fault>,
    on_removed: &mut dyn FnMut(PathBuf),
    warnings: &mut Vec<RunWarning>,
) -> Result<Vec<String>, Fault> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut kept: BTreeSet<&str> = BTreeSet::new();
    let (found, unlistable) = partials_under(fs, dest_shown, operations_shown, ids, warnings);
    if unlistable {
        // A root or directory the sweep cannot read may hold a partial: every prior's state stays as its record.
        kept.extend(ids.iter().copied());
    }
    for (path, prior) in found {
        let prior =
            ids.get(prior.as_str()).copied().expect("partials_under yields only ids from `ids`");
        check()?;
        match remove_below(dest, &path) {
            Ok(()) => on_removed(dest_shown.join(&path)),
            Err(error) => {
                warnings.push(RunWarning::PartialKept {
                    path: dest_shown.join(&path),
                    error,
                    kept: shown(prior),
                });
                kept.insert(prior);
            }
        }
    }
    Ok(ids.difference(&kept).map(|i| (*i).to_string()).collect())
}

/// J1 and cut 11b: every `<name>.flux-partial.<id>` for `ids` under DEST, found by a walk of `dest_shown` that skips the
/// reserved control directories. Read-only: each entry is the partial's DEST-relative path (as the walk yields it) and
/// its prior's id, in walk order. A root or directory that cannot be listed is reported through `warnings` as
/// `PartialKept` (kept: `operations_shown`); the bool answers whether the root or a directory below it could not be
/// listed, so the caller must treat every id as possibly still holding partials (a failing root yields no entries).
pub(crate) fn partials_under<F: DestinationRoot>(
    fs: &F,
    dest_shown: &Path,
    operations_shown: &Path,
    ids: &BTreeSet<&str>,
    warnings: &mut Vec<RunWarning>,
) -> (Vec<(PathBuf, String)>, bool) {
    let mut found = Vec::new();
    let mut unlistable = false;
    // J1: every `<name>.flux-partial.<prior-id>` under DEST, found by a walk of DEST by path that skips the
    // reserved control directories.
    let events = match walk(fs, dest_shown) {
        Ok(events) => events,
        Err(error) => {
            warnings.push(RunWarning::PartialKept {
                path: dest_shown.to_path_buf(),
                error,
                kept: operations_shown.to_path_buf(),
            });
            return (found, true);
        }
    };
    for item in events {
        let path = match item {
            // Cut 10a: a link temporary too (a crash between its creation and its rename); `remove_file` unlinks it.
            Ok(WalkEvent::File { path }) | Ok(WalkEvent::Symlink { path }) => path,
            Ok(_) => continue,
            Err(e) => {
                let path = dest_shown.join(&e.path);
                warnings.push(RunWarning::PartialKept {
                    path,
                    error: e.cause,
                    kept: operations_shown.to_path_buf(),
                });
                unlistable = true;
                continue;
            }
        };
        if reserved_path(&path) {
            continue;
        }
        let Some(prior) = path
            .file_name()
            .and_then(|n| id_after(n, PARTIAL_INFIX))
            .and_then(|i| ids.get(i).copied())
        else {
            continue;
        };
        found.push((path, prior.to_string()));
    }
    (found, unlistable)
}

/// Remove the file at `rel` below `dest` through handles opened one component at a time, so no link is followed
/// (§149.7; J1).
fn remove_below<D: DirHandle>(dest: &D, rel: &Path) -> flux_fs::Result<()> {
    let mut parts: Vec<&OsStr> = rel.iter().collect();
    let name = parts.pop().expect("a walk path ends in a name");
    let mut opened: Option<D> = None;
    for part in parts {
        let next = opened.as_ref().unwrap_or(dest).open_dir(part)?;
        opened = Some(next);
    }
    opened.as_ref().unwrap_or(dest).remove_file(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::FileSystem;

    #[test]
    fn each_deleted_partial_is_reported_with_its_path_and_the_others_stay() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        fs.create_dir(Path::new("/p/dest/sub")).unwrap();
        let id = crate::ids::new_id();
        let other = crate::ids::new_id();
        let mine = format!("/p/dest/sub/a{PARTIAL_INFIX}{id}");
        let theirs = format!("/p/dest/b{PARTIAL_INFIX}{other}");
        fs.write_file(&mine, b"x");
        fs.write_file(&theirs, b"x");
        fs.write_file("/p/dest/keep.txt", b"x");
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let ids: BTreeSet<&str> = [id.as_str()].into();
        let mut removed = Vec::new();
        let mut warnings = Vec::new();
        let done = sweep_partials(
            &fs,
            &dest,
            Path::new("/p/dest"),
            Path::new("/p/dest/.flux/operations"),
            &ids,
            &|i| PathBuf::from(i),
            &mut || Ok(()),
            &mut |p| removed.push(p),
            &mut warnings,
        )
        .ok()
        .unwrap();
        assert_eq!(done, vec![id.clone()]);
        assert_eq!(removed, vec![PathBuf::from(&mine)]);
        assert!(warnings.is_empty());
        assert!(!fs.exists(&mine) && fs.exists(&theirs) && fs.exists("/p/dest/keep.txt"));
    }

    #[test]
    fn an_unlistable_root_keeps_every_priors_state() {
        let fs = FaultFs::new();
        fs.create_dir(Path::new("/p")).unwrap();
        fs.create_dir(Path::new("/p/dest")).unwrap();
        let id = crate::ids::new_id();
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        fs.fail("read_dir", flux_fs::Code::PermissionDenied);
        let ids: BTreeSet<&str> = [id.as_str()].into();
        let mut warnings = Vec::new();
        let done = sweep_partials(
            &fs,
            &dest,
            Path::new("/p/dest"),
            Path::new("/p/dest/.flux/operations"),
            &ids,
            &|i| PathBuf::from(i),
            &mut || Ok(()),
            &mut |_| panic!("nothing is deleted"),
            &mut warnings,
        )
        .ok()
        .unwrap();
        assert!(done.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            matches!(&warnings[0], RunWarning::PartialKept { path, .. } if path == Path::new("/p/dest")),
            "{warnings:?}"
        );
    }
}
