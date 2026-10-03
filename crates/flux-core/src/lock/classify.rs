//! §240.1 classification of an existing lock (decision 6's order).

use super::error::LockResult;
use super::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use super::site::{LockSite, Workspace};
use flux_fs::{Code, DirHandle, FileIdentity, LockFile};
use std::io::ErrorKind;

pub(crate) enum Classified<D: DirHandle> {
    /// Nothing at the lock path any more: acquire again.
    Vanished,
    /// Another process holds the OS-native lock (§240.2): `TARGET_LOCK_BUSY`, with its record if readable.
    Busy(Option<LockRecord>),
    /// Empty, torn, checksum-failed or newer-version, and nobody holds it: `TARGET_LOCK_UNCERTAIN`.
    Uncertain(Uncertain),
    /// Not a Flux lock: `CONTROL_PLANE_NAMESPACE_CONFLICT`, never overwritten.
    Foreign,
    /// A dead owner whose recorded workspace is missing or untrusted: `ARTIFACT_OWNERSHIP_UNCERTAIN`.
    Untrusted(LockRecord),
    /// A dead owner with a trusted workspace. The handle and its OS-native lock are KEPT for §240.3 recovery.
    Dead { lock: D::Lock, identity: FileIdentity, record: LockRecord },
}

/// `S240_1_open`, `S240_1_trylock`, `S240_1_read`, then `S240_1_close` unless the lock is kept for recovery.
pub(crate) fn classify<D: DirHandle>(site: &LockSite<'_, D>) -> LockResult<Classified<D>> {
    // S240_1_open: without creating it; a link, a directory or another non-regular object is not a lock.
    let lock = match site.dir().open_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(Classified::Vanished),
        Err(e) if matches!(e.code, Code::SafetyRejected | Code::DestinationError) => {
            return Ok(Classified::Foreign);
        }
        Err(e) => return Err(e.into()),
    };
    // S240_1_trylock
    let got = lock.try_lock()?;
    // S240_1_read
    let decoded = decode(&lock.read_all(RECORD_LEN)?);
    // S240_1_close happens as `lock` drops on every return except `Dead`.
    match decoded {
        Decoded::Foreign => Ok(Classified::Foreign),
        Decoded::Record(record) if !got => Ok(Classified::Busy(Some(record))),
        Decoded::Uncertain(_) if !got => Ok(Classified::Busy(None)),
        Decoded::Uncertain(why) => Ok(Classified::Uncertain(why)),
        Decoded::Record(record) => match site.workspace(&record)? {
            Workspace::Trusted => {
                let identity = lock.identity()?;
                Ok(Classified::Dead { lock, identity, record })
            }
            Workspace::Missing | Workspace::Untrusted => Ok(Classified::Untrusted(record)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FakeDirHandle;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record};
    use flux_fs::{FileSystem, FileType};
    use std::ffi::OsStr;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn workspace(fs: &crate::fault_fs::FaultFs, id: &str) {
        for dir in [
            "/p/dest",
            "/p/dest/.flux",
            "/p/dest/.flux/operations",
            &format!("/p/dest/.flux/operations/{id}"),
        ] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
    }

    #[test]
    fn nothing_at_the_path_has_vanished() {
        let (_fs, d) = fake();
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Vanished));
    }

    #[test]
    fn a_held_lock_is_busy_and_reports_its_record_when_readable() {
        let (_fs, d) = fake();
        let site = site(&d);
        let rec = record(&site, &crate::ids::new_id(), "none");
        let _live = live_lock(&d, NAME, &rec.encode());
        assert!(matches!(classify(&site).unwrap(), Classified::Busy(Some(r)) if r == rec));
    }

    #[test]
    fn a_held_empty_lock_is_busy_not_uncertain() {
        // An acquirer inside F5's window: created and held, no record yet (decision 6).
        let (_fs, d) = fake();
        let _live = live_lock(&d, NAME, b"");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Busy(None)));
    }

    #[test]
    fn foreign_content_is_foreign_even_when_held() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"hello, not a lock");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign));
        let (_fs, d) = fake();
        let _live = live_lock(&d, NAME, b"hello, not a lock");
        assert!(
            matches!(classify(&site(&d)).unwrap(), Classified::Foreign),
            "content is judged first"
        );
    }

    #[test]
    fn an_object_that_is_not_a_regular_file_is_foreign() {
        let (fs, d) = fake();
        fs.create_dir(Path::new("/p/dest.flux-lock")).unwrap();
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign), "a directory");
        let (fs, d) = fake();
        fs.write_file("/p/dest.flux-lock", b"");
        fs.set_type("/p/dest.flux-lock", FileType::Symlink);
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Foreign), "a link");
    }

    #[test]
    fn an_unheld_empty_or_torn_lock_is_uncertain() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Uncertain(Uncertain::Empty)));
        let (_fs, d) = fake();
        dead_lock(&d, NAME, &[0u8; 4096]);
        assert!(matches!(classify(&site(&d)).unwrap(), Classified::Uncertain(Uncertain::Torn)));
    }

    #[test]
    fn a_dead_owner_with_its_workspace_is_dead_and_the_lock_is_kept() {
        let (fs, d) = fake();
        let site = site(&d);
        let id = crate::ids::new_id();
        workspace(&fs, &id);
        let rec = record(&site, &id, &format!("operations/{id}"));
        dead_lock(&d, NAME, &rec.encode());
        let Classified::Dead { record: seen, .. } = classify(&site).unwrap() else {
            panic!("expected Dead")
        };
        assert_eq!(seen, rec);
        let kept = classify(&site);
        assert!(
            matches!(kept.unwrap(), Classified::Dead { .. }),
            "the first result was dropped, releasing the lock"
        );
    }

    #[test]
    fn the_dead_classification_keeps_the_os_lock_while_it_lives() {
        let (fs, d) = fake();
        let site = site(&d);
        let id = crate::ids::new_id();
        workspace(&fs, &id);
        dead_lock(&d, NAME, &record(&site, &id, &format!("operations/{id}")).encode());
        let dead = classify(&site).unwrap();
        assert!(matches!(dead, Classified::Dead { .. }));
        let other = d.open_lock(OsStr::new(NAME)).unwrap();
        assert!(!other.try_lock().unwrap(), "kept for recovery");
        drop(dead);
        assert!(other.try_lock().unwrap());
    }

    #[test]
    fn a_dead_owner_whose_workspace_is_missing_is_untrusted_and_released() {
        let (_fs, d) = fake();
        let site = site(&d);
        let id = crate::ids::new_id();
        let rec = record(&site, &id, &format!("operations/{id}"));
        dead_lock(&d, NAME, &rec.encode());
        assert!(matches!(classify(&site).unwrap(), Classified::Untrusted(r) if r == rec));
        assert!(d.open_lock(OsStr::new(NAME)).unwrap().try_lock().unwrap(), "closed on refusal");
    }

    #[test]
    fn a_dead_cleanup_lock_is_dead() {
        let (_fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, &record(&site, &crate::ids::new_id(), "none").encode());
        assert!(matches!(classify(&site).unwrap(), Classified::Dead { .. }));
    }
}
