//! §240.5: take an uncertain lock over IN PLACE, so the lock path is never empty. Steps 1-5 here; step 6's overwrite
//! waits until Part 3 has created this operation's state (F5, the design's refinement 8).

use super::error::{LockCode, LockResult, refuse};
use super::held::Held;
use super::record::{Decoded, LockRecord, RECORD_LEN, Uncertain, decode};
use super::site::LockSite;
use flux_fs::{Code, DirHandle, FileIdentity, LockCapability, LockFile};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;

/// An uncertain lock this run has opened and locked, whose record it has not yet overwritten.
pub struct Claimed<'a, D: DirHandle> {
    dir: &'a D,
    lock_name: OsString,
    lock: D::Lock,
    identity: FileIdentity,
    prior: Uncertain,
}

/// What `Claimed::overwrite` came to.
pub enum Overwritten<'a, D: DirHandle> {
    Held(Held<'a, D>),
    /// The lock path no longer names the claimed file: start again (§240.5 step 6, amended 2026-09-17).
    Restart,
}

pub(crate) enum TakeOver<'a, D: DirHandle> {
    Claimed(Claimed<'a, D>),
    /// Start again. No payload: `obtain` records what the pass saw itself (decision 2).
    Restart,
}

/// `S240_5_s1` to `S240_5_s5`; `S240_5_close` is `lock` dropping on every non-claimed return.
pub(crate) fn take_over<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    reported: Uncertain,
) -> LockResult<TakeOver<'a, D>> {
    // S240_5_s1: strong identity and OS-native locks, decided from the capability, never by a trial lock.
    if !matches!(site.dir().identity()?, FileIdentity::Strong(_)) || !capability.allows_exclusive()
    {
        return Err(refuse(
            LockCode::TargetLockUncertain,
            None,
            "--break-lock needs strong file identity and a local filesystem with OS-native locks",
        ));
    }
    // S240_5_s2: open the existing file without creating it; gone or not a regular file, start again.
    let lock = match site.dir().open_lock(site.lock_name()) {
        Ok(l) => l,
        Err(e)
            if e.source.kind() == ErrorKind::NotFound
                || matches!(e.code, Code::SafetyRejected | Code::DestinationError) =>
        {
            return Ok(TakeOver::Restart);
        }
        Err(e) => return Err(e.into()),
    };
    // S240_5_s3: the OS-native lock without waiting; held means the owner or another takeover is alive.
    if !lock.try_lock()? {
        return Err(refuse(LockCode::TargetLockBusy, None, "another process holds the lock"));
    }
    // S240_5_s4: the open file is still the one at the lock path.
    let identity = lock.identity()?;
    if at_path(site.dir(), site.lock_name())? != Some(identity) {
        return Ok(TakeOver::Restart);
    }
    // S240_5_s5: a record written meanwhile is another holder (and, locked by us, a dead one: recovery's case), and
    // foreign content goes back through classification (decision 8). Still unreadable: continue.
    match decode(&lock.read_all(RECORD_LEN)?) {
        Decoded::Uncertain(_) => {}
        Decoded::Record(_) | Decoded::Foreign => return Ok(TakeOver::Restart),
    }
    Ok(TakeOver::Claimed(Claimed {
        dir: site.dir(),
        lock_name: site.lock_name().to_os_string(),
        lock,
        identity,
        prior: reported,
    }))
}

impl<'a, D: DirHandle> Claimed<'a, D> {
    /// What the classification read in the prior holder's place (its record was unreadable).
    pub fn prior(&self) -> Uncertain {
        self.prior
    }

    /// `S240_5_s6_write_begin`, `S240_5_s6_write_end`, `S240_5_s6_flush`, `S240_5_s6`: overwrite the record in place
    /// in one write, flush, and check the identity against the lock path again. Called after this operation's state
    /// exists (F5). A write or flush failure is an error after the write (exit 1).
    pub fn overwrite(self, record: LockRecord) -> LockResult<Overwritten<'a, D>> {
        self.lock.write_at_start(&record.encode())?;
        self.lock.sync_all()?;
        if at_path(self.dir, &self.lock_name)? != Some(self.identity) {
            return Ok(Overwritten::Restart);
        }
        Ok(Overwritten::Held(Held {
            dir: self.dir,
            lock_name: self.lock_name,
            lock: self.lock,
            identity: self.identity,
            record: Some(Box::new(record)),
            beat: std::cell::Cell::new(None),
        }))
    }
}

fn at_path<D: DirHandle>(dir: &D, name: &OsStr) -> LockResult<Option<FileIdentity>> {
    match dir.metadata(name) {
        Ok(m) => Ok(Some(m.identity)),
        Err(e) if e.source.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FakeDirHandle;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record, refusal};
    use flux_fs::{DestinationRoot, FileSystem};
    use std::ffi::OsStr;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";
    const LOCK: &str = "/p/dest.flux-lock";
    const STRONG: LockCapability = LockCapability::LocalStrong;

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn claimed<'a>(t: TakeOver<'a, FakeDirHandle>) -> Claimed<'a, FakeDirHandle> {
        match t {
            TakeOver::Claimed(c) => c,
            TakeOver::Restart => panic!("expected Claimed, got Restart"),
        }
    }

    #[test]
    fn an_unsupported_capability_stays_uncertain() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let r = take_over(&site(&d), LockCapability::Unsupported, Uncertain::Empty);
        assert_eq!(refusal(r).code, LockCode::TargetLockUncertain);
    }

    #[test]
    fn a_directory_without_strong_identity_stays_uncertain() {
        let (fs, d) = fake();
        dead_lock(&d, NAME, b"");
        fs.set_identity("/p", FileIdentity::Weak(flux_fs::ObjectId { volume: 1, index: 999 }));
        let r = take_over(&site(&d), STRONG, Uncertain::Empty);
        assert_eq!(refusal(r).code, LockCode::TargetLockUncertain);
        assert_eq!(fs.read_file(LOCK).as_deref(), Some(&b""[..]));
    }

    #[test]
    fn an_empty_lock_is_claimed_then_overwritten_in_place() {
        let (fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, b"");
        let before = d.metadata(OsStr::new(NAME)).unwrap().identity;
        let c = claimed(take_over(&site, STRONG, Uncertain::Empty).unwrap());
        assert_eq!(c.prior(), Uncertain::Empty);
        let mine = record(&site, &crate::ids::new_id(), "none");
        let Overwritten::Held(held) = c.overwrite(mine.clone()).unwrap() else {
            panic!("expected Held")
        };
        assert_eq!(fs.read_file(LOCK), Some(mine.encode()));
        assert_eq!(held.identity(), &before, "the same file: never an empty lock path");
        assert!(held.still_owned().unwrap());
    }

    #[test]
    fn a_held_lock_is_busy() {
        let (_fs, d) = fake();
        let _live = live_lock(&d, NAME, b"");
        assert_eq!(
            refusal(take_over(&site(&d), STRONG, Uncertain::Empty)).code,
            LockCode::TargetLockBusy
        );
    }

    #[test]
    fn a_vanished_lock_restarts() {
        let (_fs, d) = fake();
        assert!(matches!(
            take_over(&site(&d), STRONG, Uncertain::Empty).unwrap(),
            TakeOver::Restart
        ));
    }

    #[test]
    fn a_record_or_foreign_content_written_meanwhile_restarts() {
        let (_fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, &record(&site, &crate::ids::new_id(), "none").encode());
        assert!(matches!(take_over(&site, STRONG, Uncertain::Empty).unwrap(), TakeOver::Restart));
        let (fs, d) = fake();
        dead_lock(&d, NAME, b"");
        fs.on_nth("read_all", 1, |fs| {
            let d = fs.destination_root(Path::new("/p")).unwrap();
            d.open_lock(OsStr::new(NAME)).unwrap().write_at_start(b"hello, not a lock").unwrap();
        });
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        assert!(
            matches!(take_over(&site, STRONG, Uncertain::Empty).unwrap(), TakeOver::Restart),
            "decision 8"
        );
        assert_eq!(
            fs.read_file(LOCK).as_deref(),
            Some(&b"hello, not a lock"[..]),
            "never overwritten"
        );
    }

    #[test]
    fn a_path_replaced_before_the_identity_check_restarts() {
        let (fs, d) = fake();
        dead_lock(&d, NAME, b"");
        fs.on_nth("metadata", 1, |fs| {
            fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
            fs.write_file(LOCK, b"");
        });
        assert!(matches!(
            take_over(&site(&d), STRONG, Uncertain::Empty).unwrap(),
            TakeOver::Restart
        ));
    }

    #[test]
    fn an_overwrite_whose_file_left_the_path_restarts() {
        let (fs, d) = fake();
        let site = site(&d);
        dead_lock(&d, NAME, b"");
        let c = claimed(take_over(&site, STRONG, Uncertain::Empty).unwrap());
        fs.on_nth("lock_sync_all", 1, |fs| {
            fs.rename_no_replace(Path::new(LOCK), Path::new("/p/aside")).unwrap();
            fs.write_file(LOCK, b"");
        });
        let r = c.overwrite(record(&site, &crate::ids::new_id(), "none")).unwrap();
        assert!(matches!(r, Overwritten::Restart));
    }
}
