//! §96.1 acquisition: create the lock exclusively, check the directory lock, take the OS-native lock, verify.

use super::error::{LockCode, LockResult, refuse};
use super::held::Held;
use super::site::LockSite;
use flux_fs::{DirHandle, FileIdentity, LockFile};
use std::io::ErrorKind;
use std::time::Duration;

/// Tries of the OS-native lock on a freshly created file, while its path still names it and it is still empty
/// (decision 2).
pub(crate) const OWNLOCK_TRIES: u32 = 5;
const OWNLOCK_PAUSE: Duration = Duration::from_millis(20);

/// What a pass that must start again last saw (decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Last {
    /// Another process held the OS-native lock.
    HeldByOther,
    /// The last classification found an empty or torn lock that nobody held.
    EmptyOrTornUnheld,
    /// Anything else: the path changed under this pass.
    Other,
}

pub(crate) enum Acquire<'a, D: DirHandle> {
    Acquired(Held<'a, D>),
    /// The lock name is taken: classify what is there.
    Exists,
    Restart(Last),
}

/// `S96_1_create`, `S96_1_dircheck` (and `S96_1_backoff`), then `own_lock`.
pub(crate) fn acquire<'a, D: DirHandle>(site: &LockSite<'a, D>) -> LockResult<Acquire<'a, D>> {
    // S96_1_create
    let lock = match site.dir().create_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::AlreadyExists => return Ok(Acquire::Exists),
        Err(e) => return Err(e.into()),
    };
    // S96_1_dircheck: a present directory lock means S96_1_backoff - remove what this pass created, refuse BUSY.
    // Guarded by the OS-native lock (decision 13): a file another run already holds, or has written, is theirs now,
    // and is closed without unlinking.
    if site.dir_lock_present()? {
        let identity = lock.identity()?;
        if lock.try_lock()? && still_empty_at_path(site, &lock, &identity)? {
            site.dir().remove_file(site.lock_name())?;
        }
        drop(lock);
        return Err(refuse(
            LockCode::TargetLockBusy,
            None,
            "a directory lock (.flux-dir.lock) is present beside the target's lock",
        ));
    }
    Ok(match own_lock(site, lock)? {
        Ok(held) => Acquire::Acquired(held),
        Err(last) => Acquire::Restart(last),
    })
}

/// `S96_1_ownlock`, `S96_1_ownlock_wait`, `S96_1_ownlock_verify`, `S96_1_ownlock_close`; and the same four steps of a
/// recovery's own lock, `S240_3_s4_lock`, `S240_3_s4_lock_wait`, `S240_3_s4_lock_verify`, `S240_3_s4_lock_close`.
/// Giving up closes the handle and never removes the file by name, which could delete a takeover's lock.
pub(crate) fn own_lock<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    lock: D::Lock,
) -> LockResult<Result<Held<'a, D>, Last>> {
    let identity = lock.identity()?;
    let mut tries = 1;
    while !lock.try_lock()? {
        // S96_1_ownlock_wait: retry only while the path still names this file and it is still empty.
        if tries >= OWNLOCK_TRIES || !still_empty_at_path(site, &lock, &identity)? {
            return Ok(Err(Last::HeldByOther)); // S96_1_ownlock_close: the handle drops, nothing is unlinked
        }
        tries += 1;
        std::thread::sleep(OWNLOCK_PAUSE);
    }
    // S96_1_ownlock_verify: holding the lock, the path must still name the file, and nobody may have written it.
    if !still_empty_at_path(site, &lock, &identity)? {
        return Ok(Err(Last::Other));
    }
    Ok(Ok(Held {
        dir: site.dir(),
        lock_name: site.lock_name().to_os_string(),
        lock,
        identity,
        record: None,
    }))
}

fn still_empty_at_path<D: DirHandle>(
    site: &LockSite<'_, D>,
    lock: &D::Lock,
    identity: &FileIdentity,
) -> LockResult<bool> {
    let at_path = match site.dir().metadata(site.lock_name()) {
        Ok(m) => m.identity,
        Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    Ok(at_path == *identity && lock.read_all(0)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::held::Released;
    use crate::lock::record::{Decoded, decode};
    use crate::lock::test_support::{fake, record, refusal};
    use flux_fs::{DestinationRoot, FileSystem};
    use std::ffi::OsStr;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    const LOCK: &str = "/p/dest.flux-lock";

    fn site(d: &crate::fault_fs::FakeDirHandle) -> LockSite<'_, crate::fault_fs::FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn acquired<'a, D: DirHandle>(a: Acquire<'a, D>) -> Held<'a, D> {
        match a {
            Acquire::Acquired(h) => h,
            Acquire::Exists => panic!("expected Acquired, got Exists"),
            Acquire::Restart(l) => panic!("expected Acquired, got Restart({l:?})"),
        }
    }

    #[test]
    fn a_fresh_lock_is_created_held_and_empty() {
        let (fs, d) = fake();
        let site = site(&d);
        let held = acquired(acquire(&site).unwrap());
        assert!(held.record().is_none(), "F5: no record until the state exists");
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
        let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
        assert!(!other.try_lock().unwrap(), "the acquirer holds the OS-native lock");
    }

    #[test]
    fn a_taken_name_is_reported_as_existing() {
        let (fs, d) = fake();
        fs.write_file(LOCK, b"");
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Exists));
    }

    #[test]
    fn a_directory_lock_beside_it_backs_off_busy_and_removes_what_it_created() {
        let (fs, d) = fake();
        fs.write_file("/p/.flux-dir.lock", b"");
        assert_eq!(refusal(acquire(&site(&d))).code, LockCode::TargetLockBusy);
        assert!(!fs.exists(LOCK), "S96_1_backoff removes the lock this pass created");
    }

    #[test]
    fn a_backoff_never_removes_a_file_another_run_has_locked() {
        let (fs, d) = fake();
        fs.write_file("/p/.flux-dir.lock", b"");
        let slot = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&slot);
        fs.on_nth("try_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
            assert!(other.try_lock().unwrap());
            *keep.lock().unwrap() = Some(other);
        });
        assert_eq!(refusal(acquire(&site(&d))).code, LockCode::TargetLockBusy);
        assert!(fs.exists(LOCK), "decision 13: the file another run holds stays");
        drop(slot);
    }

    #[test]
    fn a_lock_another_handle_takes_on_the_fresh_file_restarts_without_removing_it() {
        let (fs, d) = fake();
        let slot = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&slot);
        fs.on_nth("try_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
            assert!(other.try_lock().unwrap());
            *keep.lock().unwrap() = Some(other);
        });
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Restart(Last::HeldByOther)));
        assert!(fs.exists(LOCK), "giving up closes; it never unlinks by name");
        drop(slot);
    }

    #[test]
    fn a_lock_path_replaced_before_the_verify_restarts() {
        let (fs, d) = fake();
        fs.on_nth("try_lock", 1, |fs| {
            fs.rename_no_replace(Path::new(LOCK), Path::new("/p/elsewhere")).unwrap();
            fs.write_file(LOCK, b"");
        });
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Restart(Last::Other)));
    }

    #[test]
    fn a_holder_that_lets_go_while_the_acquirer_waits_is_outlasted() {
        let (fs, d) = fake();
        let slot = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&slot);
        fs.on_nth("try_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
            assert!(other.try_lock().unwrap());
            *keep.lock().unwrap() = Some(other);
        });
        // The wait's emptiness check is the first `read_all`: the transient holder closes just before it.
        let release = Arc::clone(&slot);
        fs.on_nth("read_all", 1, move |_| drop(release.lock().unwrap().take()));
        let held = acquired(acquire(&site(&d)).unwrap());
        assert!(held.record().is_none());
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
    }

    #[test]
    fn a_record_written_into_the_fresh_file_before_the_verify_restarts() {
        let (fs, d) = fake();
        let bytes = record(&site(&d), &crate::ids::new_id(), "none").encode();
        let written = bytes.clone();
        // Another handle writes the file without holding its lock: the path still names it, but it is not empty.
        fs.on_nth("try_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            d.open_lock(OsStr::new("dest.flux-lock")).unwrap().write_at_start(&written).unwrap();
        });
        assert!(matches!(acquire(&site(&d)).unwrap(), Acquire::Restart(Last::Other)));
        assert_eq!(fs.read_file(LOCK), Some(bytes), "another run's record is left alone");
    }

    #[test]
    fn a_backoff_never_removes_a_file_another_run_has_written() {
        let (fs, d) = fake();
        fs.write_file("/p/.flux-dir.lock", b"");
        let bytes = record(&site(&d), &crate::ids::new_id(), "none").encode();
        let written = bytes.clone();
        fs.on_nth("try_lock", 1, move |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            d.open_lock(OsStr::new("dest.flux-lock")).unwrap().write_at_start(&written).unwrap();
        });
        assert_eq!(refusal(acquire(&site(&d))).code, LockCode::TargetLockBusy);
        assert_eq!(fs.read_file(LOCK), Some(bytes), "decision 13: a written file is theirs");
    }

    #[test]
    fn release_and_discard_unlink_while_the_lock_is_still_held() {
        for discard in [false, true] {
            let (fs, d) = fake();
            let site = site(&d);
            let mut held = acquired(acquire(&site).unwrap());
            let seen = Arc::new(Mutex::new(None));
            let out = Arc::clone(&seen);
            fs.on_nth("remove_file", 1, move |fs| {
                let d = fs.destination_root(Path::new("/p")).unwrap();
                let other = d.open_lock(OsStr::new("dest.flux-lock")).unwrap();
                *out.lock().unwrap() = Some(other.try_lock().unwrap());
            });
            if discard {
                held.discard().unwrap();
            } else {
                held.write_record(record(&site, &crate::ids::new_id(), "none")).unwrap();
                assert_eq!(held.release().unwrap(), Released::Unlinked);
            }
            assert!(!fs.exists(LOCK));
            assert_eq!(
                *seen.lock().unwrap(),
                Some(false),
                "decision 7 (discard={discard}): the OS-native lock is held at the unlink"
            );
        }
    }

    #[test]
    fn a_record_of_the_same_operation_by_another_instance_is_not_owned() {
        let (_fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        let mine = record(&site, &crate::ids::new_id(), "none");
        held.write_record(mine.clone()).unwrap();
        let theirs = record(&site, &mine.operation_id, "none");
        assert_ne!(theirs.owner_instance_id, mine.owner_instance_id);
        d.open_lock(OsStr::new("dest.flux-lock"))
            .unwrap()
            .write_at_start(&theirs.encode())
            .unwrap();
        assert!(!held.still_owned().unwrap(), "both the operation and the instance must match");
    }

    #[test]
    fn a_written_record_is_owned_until_another_operation_overwrites_it() {
        let (fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        assert!(!held.still_owned().unwrap(), "no record yet: not provably owned");
        let mine = record(&site, &crate::ids::new_id(), "none");
        held.write_record(mine.clone()).unwrap();
        assert_eq!(decode(&fs.read_file(LOCK).unwrap()), Decoded::Record(mine));
        assert!(held.still_owned().unwrap());
        // A takeover overwrites the record in place: same file, another operation's record.
        let theirs = record(&site, &crate::ids::new_id(), "none");
        d.open_lock(OsStr::new("dest.flux-lock"))
            .unwrap()
            .write_at_start(&theirs.encode())
            .unwrap();
        assert!(!held.still_owned().unwrap(), "the identity alone is not ownership");
        assert_eq!(held.release().unwrap(), Released::NotOwned);
        assert!(fs.exists(LOCK), "a lock that is not ours is never unlinked");
    }

    #[test]
    fn still_owned_is_false_once_the_path_names_another_file() {
        let (fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        held.write_record(record(&site, &crate::ids::new_id(), "none")).unwrap();
        fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
        assert!(!held.still_owned().unwrap(), "the path is empty");
        fs.write_file(LOCK, b"");
        assert!(!held.still_owned().unwrap(), "the path names another file");
    }

    #[test]
    fn release_unlinks_an_owned_lock() {
        let (fs, d) = fake();
        let site = site(&d);
        let mut held = acquired(acquire(&site).unwrap());
        held.write_record(record(&site, &crate::ids::new_id(), "none")).unwrap();
        assert_eq!(held.release().unwrap(), Released::Unlinked);
        assert!(!fs.exists(LOCK));
        let again = d.create_lock(OsStr::new("dest.flux-lock")).unwrap();
        assert!(again.try_lock().unwrap(), "closing released the OS-native lock");
    }

    #[test]
    fn discard_removes_only_its_own_recordless_lock() {
        let (fs, d) = fake();
        let site = site(&d);
        acquired(acquire(&site).unwrap()).discard().unwrap();
        assert!(!fs.exists(LOCK), "its own empty lock is removed");
        let held = acquired(acquire(&site).unwrap());
        fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
        fs.write_file(LOCK, b"someone else's");
        held.discard().unwrap();
        assert_eq!(
            fs.read_file(LOCK).as_deref(),
            Some(&b"someone else's"[..]),
            "another file is never removed"
        );
    }
}
