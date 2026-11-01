// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! J1: the deletion of superseded operations' partials. The `--restart` sweep (`TreePlace::sweep`) and, from cut 9b,
//! `flux cleanup` share it.

use super::RunWarning;
use super::session::Fault;
use crate::scratch::ScratchSpace;
use crate::state::{PARTIAL_INFIX, id_after};
use crate::tree::reserved_path;
use crate::walk::{DEFAULT_MAX_DEPTH, MountPolicy, Tick, WalkEvent, WalkOptions, walk_unstarted};
use flux_fs::{Code, DestinationRoot, DirHandle, FsError};
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
    scratch: Option<&ScratchSpace<'_>>,
    warnings: &mut Vec<RunWarning>,
) -> Result<Vec<String>, Fault> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut kept: BTreeSet<&str> = BTreeSet::new();
    // The walk's tick runs `check`; a fault it raised is stashed so the caller gets the fault itself, not the
    // placeholder error the tick hands the walk.
    let mut stashed: Option<Fault> = None;
    let walked = partials_under(
        fs,
        dest_shown,
        operations_shown,
        ids,
        scratch,
        &mut || {
            check().map_err(|fault| {
                stashed = Some(fault);
                FsError::new(
                    Code::IoError,
                    std::io::Error::other("the sweep's liveness check failed"),
                )
            })
        },
        warnings,
    );
    let (found, unlistable) = match walked {
        Ok(v) => v,
        Err(e) => {
            return Err(stashed.unwrap_or_else(|| Fault::Io(dest_shown.to_path_buf(), e)));
        }
    };
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
/// The walk spills a large directory into `scratch` and calls `tick` as it goes; a FATAL walk error (a failed tick or
/// a scratch error) is the `Err`: the listing is incomplete, so nothing may be deleted on it.
pub(crate) fn partials_under<F: DestinationRoot>(
    fs: &F,
    dest_shown: &Path,
    operations_shown: &Path,
    ids: &BTreeSet<&str>,
    scratch: Option<&ScratchSpace<'_>>,
    tick: &mut Tick<'_>,
    warnings: &mut Vec<RunWarning>,
) -> Result<(Vec<(PathBuf, String)>, bool), FsError> {
    let mut found = Vec::new();
    let mut unlistable = false;
    // J1: every `<name>.flux-partial.<prior-id>` under DEST, found by a walk of DEST by path that skips the
    // reserved control directories.
    let unreadable_root = |warnings: &mut Vec<RunWarning>, error: FsError| {
        warnings.push(RunWarning::PartialKept {
            path: dest_shown.to_path_buf(),
            error,
            kept: operations_shown.to_path_buf(),
        });
    };
    let mut events = match walk_unstarted(
        fs,
        dest_shown,
        WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts: MountPolicy::Cross },
        scratch,
    ) {
        Ok(events) => events,
        Err(error) => {
            unreadable_root(warnings, error);
            return Ok((found, true));
        }
    };
    // The root is listed here, with the tick: `walk_with` would fold a FATAL listing error (a scratch failure) into
    // an ordinary unreadable root, and the sweep would go on to delete on an incomplete listing.
    if let Err(e) = events.start(tick) {
        if e.fatal {
            return Err(e.cause);
        }
        unreadable_root(warnings, e.cause);
        return Ok((found, true));
    }
    while let Some(item) = events.next_with(tick) {
        let path = match item {
            // Cut 10a: a link temporary too (a crash between its creation and its rename); `remove_file` unlinks it.
            Ok(WalkEvent::File { path }) | Ok(WalkEvent::Symlink { path }) => path,
            Ok(_) => continue,
            Err(e) if e.fatal => return Err(e.cause),
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
    Ok((found, unlistable))
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
            None,
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
            None,
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

    fn spilling_dest() -> (FaultFs, ScratchSpace<'static>) {
        let fs = FaultFs::new();
        for d in ["/p", "/p/dest", "/scratch"] {
            fs.create_dir(Path::new(d)).unwrap();
        }
        for i in 0..40 {
            fs.write_file(format!("/p/dest/f{i:03}"), b"x");
        }
        let home = fs.destination_root(Path::new("/scratch")).unwrap();
        let t = crate::scratch::Thresholds::new(4, 8, 2, 16, 3).unwrap();
        let space = ScratchSpace::unplaced(t, None);
        space.place(home, PathBuf::from("/scratch"));
        (fs, space)
    }

    fn sweep_spilling(
        fs: &FaultFs,
        space: &ScratchSpace<'_>,
        check: &mut dyn FnMut() -> Result<(), Fault>,
    ) -> Result<Vec<String>, Fault> {
        let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
        let id = crate::ids::new_id();
        let ids: BTreeSet<&str> = [id.as_str()].into();
        sweep_partials(
            fs,
            &dest,
            Path::new("/p/dest"),
            Path::new("/p/dest/.flux/operations"),
            &ids,
            &|i| PathBuf::from(i),
            check,
            &mut |_| panic!("nothing is deleted on an incomplete listing"),
            Some(space),
            &mut Vec::new(),
        )
    }

    #[test]
    fn a_fatal_walk_error_in_the_sweep_returns_the_fault() {
        // A scratch failure: the walk's own error, at DEST's path.
        let (fs, space) = spilling_dest();
        space.fail_writes_after(2);
        let r = sweep_spilling(&fs, &space, &mut || Ok(()));
        assert!(matches!(&r, Err(Fault::Io(p, _)) if p == Path::new("/p/dest")), "Io expected");

        // A check that fails during the walk: the stashed fault itself, not the placeholder error.
        let (fs, space) = spilling_dest();
        let mut calls = 0;
        let r = sweep_spilling(&fs, &space, &mut || {
            calls += 1;
            if calls == 2 { Err(Fault::Lost) } else { Ok(()) }
        });
        assert!(matches!(r, Err(Fault::Lost)), "Lost expected");
        assert_eq!(calls, 2, "the walk stops at the failed tick");
    }
}
