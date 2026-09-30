//! §240.3: replace a dead owner's lock by moving it aside. The caller classified it `Dead` and still holds its handle
//! and OS-native lock. The new lock is returned EMPTY: the record comes after the state (decision 3, F5).

use super::acquire::{Last, own_lock};
use super::error::LockResult;
use super::held::Held;
use super::record::{Decoded, LockRecord, RECORD_LEN, decode};
use super::site::LockSite;
use flux_fs::{DirHandle, FileIdentity, LockFile};
use std::ffi::OsString;
use std::io::ErrorKind;

pub(crate) enum Recovered<'a, D: DirHandle> {
    /// A new, empty, held lock; plus the moved-aside file if its deletion failed (a warning, decision 11).
    Held {
        held: Held<'a, D>,
        leftover: Option<OsString>,
    },
    Restart(Last),
}

/// `S240_3_s1` to `S240_3_s5`, with `S240_3_putback`, `S240_3_s4_drop`, `S240_3_restart` and `S240_3_release`.
pub(crate) fn recover<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    old: D::Lock,
    old_identity: FileIdentity,
    seen: &LockRecord,
    operation_id: &str,
) -> LockResult<Recovered<'a, D>> {
    let dir = site.dir();
    // S240_3_s1: the path still names the same dead owner's file, holding the same record.
    let at_path = match dir.metadata(site.lock_name()) {
        Ok(m) => Some(m.identity),
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if at_path.as_ref() != Some(&old_identity) || !holds(&old, seen)? {
        return Ok(Recovered::Restart(Last::Other)); // S240_3_restart; S240_3_release drops `old`
    }
    // S240_3_s2: only one of several concurrent recoverers can move it.
    let broken = site.broken_name(operation_id);
    match dir.rename_no_replace(site.lock_name(), dir, &broken) {
        Ok(()) => {}
        Err(e) if matches!(e.source.kind(), ErrorKind::NotFound | ErrorKind::AlreadyExists) => {
            return Ok(Recovered::Restart(Last::Other));
        }
        Err(e) => return Err(e.into()),
    }
    // S240_3_s3: the moved file is the one step 1 re-read, by identity AND record (a takeover rewrites in place).
    let moved = match dir.metadata(&broken) {
        Ok(m) => Some(m.identity),
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if moved.as_ref() != Some(&old_identity) || !holds(&old, seen)? {
        // S240_3_putback: back without replacing, then start again.
        let _ = dir.rename_no_replace(&broken, dir, site.lock_name());
        return Ok(Recovered::Restart(Last::Other));
    }
    // S240_3_s4: its own lock, exclusively.
    let new = match dir.create_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => {
            // S240_3_s4_drop: another operation owns the target now; the moved file's owner is dead, so drop it.
            drop(old);
            let _ = dir.remove_file(&broken);
            return Ok(Recovered::Restart(Last::Other));
        }
        Err(e) => return Err(e.into()),
    };
    let held = match own_lock(site, new)? {
        Ok(h) => h,
        Err(last) => {
            // S240_3_s4_lock_close, then S240_3_s4_drop.
            drop(old);
            let _ = dir.remove_file(&broken);
            return Ok(Recovered::Restart(last));
        }
    };
    // S240_3_s5: delete the moved file (S240_3_release closes `old` first). A failure leaves it for a later
    // cleanup; the crash table accepts the same leftover.
    drop(old);
    let leftover = match dir.remove_file(&broken) {
        Ok(()) => None,
        Err(_) => Some(broken),
    };
    Ok(Recovered::Held { held, leftover })
}

/// The record read through the kept handle is still exactly `seen`.
fn holds<L: LockFile>(lock: &L, seen: &LockRecord) -> LockResult<bool> {
    Ok(matches!(decode(&lock.read_all(RECORD_LEN)?), Decoded::Record(r) if &r == seen))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::{FakeDirHandle, FaultFs};
    use crate::lock::classify::{Classified, classify};
    use crate::lock::test_support::{dead_lock, fake, record};
    use flux_fs::{Code, DestinationRoot, FileSystem};
    use std::ffi::OsStr;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";
    const LOCK: &str = "/p/dest.flux-lock";

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    /// A dead owner's lock with a present workspace, classified; returns the kept handle, its identity, the record.
    fn dead(
        fs: &FaultFs,
        site: &LockSite<'_, FakeDirHandle>,
    ) -> (crate::fault_fs::FakeLock, FileIdentity, LockRecord) {
        let id = crate::ids::new_id();
        for dir in [
            "/p/dest",
            "/p/dest/.flux",
            "/p/dest/.flux/operations",
            &format!("/p/dest/.flux/operations/{id}"),
        ] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
        let rec = record(site, &id, &format!("operations/{id}"));
        dead_lock(site.dir(), NAME, &rec.encode());
        match classify(site).unwrap() {
            Classified::Dead { lock, identity, record } => (lock, identity, record),
            _ => panic!("expected Dead"),
        }
    }

    #[test]
    fn a_dead_owners_lock_is_replaced_by_an_empty_held_lock() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let me = crate::ids::new_id();
        let Recovered::Held { held, leftover } = recover(&site, old, old_id, &rec, &me).unwrap()
        else {
            panic!("expected Held")
        };
        assert_eq!(leftover, None);
        assert!(held.record().is_none(), "the record comes after the state (F5)");
        assert_ne!(held.identity(), &old_id, "a new file");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
        assert!(
            !fs.exists(format!("/p/dest.flux-lock.broken.{me}")),
            "S240_3_s5 deleted the moved file"
        );
    }

    #[test]
    fn a_record_changed_since_classification_restarts_without_moving_anything() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let other = record(&site, &crate::ids::new_id(), "none");
        d.open_lock(OsStr::new(NAME)).unwrap().write_at_start(&other.encode()).unwrap();
        assert!(matches!(
            recover(&site, old, old_id, &rec, &crate::ids::new_id()).unwrap(),
            Recovered::Restart(_)
        ));
        assert_eq!(fs.read_file(LOCK), Some(other.encode()), "S240_3_s1 moved nothing");
        // Without s1, s3 would catch the rewrite too, but only AFTER moving the file and putting it back.
        assert!(!fs.called("rename_no_replace"), "S240_3_s1 stops before any move");
    }

    #[test]
    fn a_taken_move_aside_name_restarts() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let me = crate::ids::new_id();
        fs.write_file(format!("/p/dest.flux-lock.broken.{me}"), b"x");
        assert!(matches!(recover(&site, old, old_id, &rec, &me).unwrap(), Recovered::Restart(_)));
        assert_eq!(fs.read_file(LOCK), Some(rec.encode()), "the lock stays where it was");
    }

    #[test]
    fn a_file_rewritten_as_it_moved_is_put_back() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        let other = record(&site, &crate::ids::new_id(), "none");
        let bytes = other.encode();
        fs.on_nth("rename_no_replace", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            d.open_lock(OsStr::new(NAME)).unwrap().write_at_start(&bytes).unwrap();
        });
        let me = crate::ids::new_id();
        assert!(matches!(recover(&site, old, old_id, &rec, &me).unwrap(), Recovered::Restart(_)));
        assert_eq!(
            fs.read_file(LOCK),
            Some(other.encode()),
            "S240_3_putback restored it at the lock path"
        );
        assert!(!fs.exists(format!("/p/dest.flux-lock.broken.{me}")));
    }

    #[test]
    fn another_lock_created_after_the_move_wins_and_the_moved_file_is_dropped() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        // `dead` created the lock once; recovery's create is the second `create_lock`.
        fs.on_nth("create_lock", 2, |fs| fs.write_file(LOCK, b""));
        let me = crate::ids::new_id();
        assert!(matches!(recover(&site, old, old_id, &rec, &me).unwrap(), Recovered::Restart(_)));
        assert!(!fs.exists(format!("/p/dest.flux-lock.broken.{me}")), "S240_3_s4_drop");
        assert_eq!(
            fs.read_file(LOCK).as_deref(),
            Some(&b""[..]),
            "the other operation's lock is untouched"
        );
    }

    #[test]
    fn a_moved_file_that_cannot_be_deleted_is_reported_as_a_leftover() {
        let (fs, d) = fake();
        let site = site(&d);
        let (old, old_id, rec) = dead(&fs, &site);
        fs.fail("remove_file", Code::IoError);
        let me = crate::ids::new_id();
        let Recovered::Held { leftover, .. } = recover(&site, old, old_id, &rec, &me).unwrap()
        else {
            panic!("expected Held")
        };
        assert_eq!(leftover, Some(OsString::from(format!("dest.flux-lock.broken.{me}"))));
    }
}
