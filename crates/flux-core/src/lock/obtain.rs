//! The run's steps 1 and 3 ("The run"): the capability gate, then acquisition, classifying and recovering or taking
//! over as the model's `plain`, `rec` and `brk` processes do (`S21_1_decide`, `S21_1_refused`,
//! `S21_1_restart_decide`).

use super::acquire::{Acquire, Last, acquire};
use super::classify::{Classified, classify};
use super::error::{LockCode, LockError, LockResult, refuse};
use super::held::Held;
use super::record::LockRecord;
use super::recover::{Recovered, recover};
use super::site::LockSite;
use super::takeover::{Claimed, TakeOver, take_over};
use flux_fs::{DirHandle, LockCapability};
use std::ffi::OsString;

/// Passes through acquire-or-classify before giving up (decision 2).
pub const MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A run without `--break-lock`.
    Plain,
    /// `--restart --break-lock`: an uncertain lock is taken over in place (§240.5).
    BreakLock,
}

pub enum Obtained<'a, D: DirHandle> {
    /// A lock this run holds with no record yet: create the state, then `Held::write_record` (F5).
    Held { held: Held<'a, D>, leftover_broken: Option<OsString> },
    /// §240.5 steps 1-5 done: report the prior holder, create the state, then `Claimed::overwrite`.
    Claimed(Claimed<'a, D>),
}

/// The run's step 1: only the LOCAL allowlist proceeds; anything else, or a failure to tell, is `REMOTE_LOCK_UNSAFE`
/// with the cause (F1; plan decision 6 of Part 1).
pub fn check_capability<D: DirHandle>(dir: &D) -> LockResult<LockCapability> {
    match dir.lock_capability() {
        Ok(c) if c.allows_exclusive() => Ok(c),
        Ok(c) => Err(refuse(
            LockCode::RemoteLockUnsafe,
            None,
            format!("the destination filesystem is not supported for locking ({c:?})"),
        )),
        Err(e) => Err(refuse(
            LockCode::RemoteLockUnsafe,
            None,
            format!(
                "the destination filesystem's lock capability could not be determined: {}",
                e.source
            ),
        )),
    }
}

const UNTRUSTED_DETAIL: &str = "the dead owner's record names a workspace that is missing or not one Flux trusts; Flux never deletes state it cannot read: inspect it, and remove it by hand if it is not needed";

/// The run's step 3. `operation_id` names a recovery's move-aside file.
pub fn obtain<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    mode: Mode,
    operation_id: &str,
) -> LockResult<Obtained<'a, D>> {
    let mut last = Last::Other;
    for _ in 0..MAX_ATTEMPTS {
        match acquire(site)? {
            Acquire::Acquired(held) => return Ok(Obtained::Held { held, leftover_broken: None }),
            Acquire::Restart(l) => {
                last = l;
                continue;
            }
            Acquire::Exists => {}
        }
        // S21_1_decide (a plain run) and S21_1_restart_decide (--restart --break-lock); S21_1_refused is each refusal.
        match classify(site)? {
            Classified::Vanished => last = Last::Other,
            Classified::Busy(holder) => {
                return Err(refuse(
                    LockCode::TargetLockBusy,
                    holder,
                    "another run holds the lock; wait for it to finish",
                ));
            }
            Classified::Foreign => {
                return Err(refuse(
                    LockCode::ControlPlaneNamespaceConflict,
                    None,
                    "a non-Flux object occupies the lock path; move it away",
                ));
            }
            Classified::Untrusted(record) => {
                return Err(refuse(
                    LockCode::ArtifactOwnershipUncertain,
                    Some(record),
                    UNTRUSTED_DETAIL,
                ));
            }
            Classified::Orphan { record, .. } => {
                return Err(refuse(
                    LockCode::ArtifactOwnershipUncertain,
                    Some(record),
                    format!(
                        "{UNTRUSTED_DETAIL}; flux cleanup DEST removes a dead owner's lock whose workspace is gone"
                    ),
                ));
            }
            Classified::Uncertain(why) => match mode {
                Mode::Plain => {
                    return Err(refuse(
                        LockCode::TargetLockUncertain,
                        None,
                        format!(
                            "the lock's record is unreadable ({why:?}); if no Flux run is active on this destination, run again with --restart --break-lock"
                        ),
                    ));
                }
                Mode::BreakLock => match take_over(site, capability, why)? {
                    TakeOver::Claimed(c) => return Ok(Obtained::Claimed(c)),
                    TakeOver::Restart => last = Last::EmptyOrTornUnheld,
                },
            },
            Classified::Dead { lock, identity, record } => {
                match recover(site, lock, identity, &record, operation_id)? {
                    Recovered::Held { held, leftover } => {
                        return Ok(Obtained::Held { held, leftover_broken: leftover });
                    }
                    Recovered::Restart(l) => last = l,
                }
            }
        }
    }
    Err(give_up(last))
}

/// What `obtain_cleanup_lock` acquired: a held lock with no record yet, and what the acquisition reclaimed.
pub struct CleanupObtained<'a, D: DirHandle> {
    /// No record yet: the caller writes one with workspace_path "none".
    pub held: Held<'a, D>,
    pub leftover_broken: Option<OsString>,
    /// The orphan record the acquisition reclaimed (reported as the `root-lock` row's action).
    pub reclaimed: Option<LockRecord>,
}

/// Cut 9b: `obtain` for cleanup. Differs in exactly one arm: an `Orphan` whose heartbeat is not in the future and is at least
/// `lease_threshold_ns` old is recovered (section 240.3); younger, or future, it is refused `ARTIFACT_OWNERSHIP_UNCERTAIN` with the
/// detail "the dead owner's lock is younger than the lease threshold". `Uncertain` is always refused (`TARGET_LOCK_UNCERTAIN`; no
/// --break-lock).
pub fn obtain_cleanup_lock<'a, D: DirHandle>(
    site: &LockSite<'a, D>,
    capability: LockCapability,
    operation_id: &str,
    now_ns: u64,
    lease_threshold_ns: u64,
) -> LockResult<CleanupObtained<'a, D>> {
    let _ = capability;
    let mut last = Last::Other;
    for _ in 0..MAX_ATTEMPTS {
        match acquire(site)? {
            Acquire::Acquired(held) => {
                return Ok(CleanupObtained { held, leftover_broken: None, reclaimed: None });
            }
            Acquire::Restart(l) => {
                last = l;
                continue;
            }
            Acquire::Exists => {}
        }
        // S251_1_classify: classify the existing lock; an old enough orphan is recovered, anything else uncertain is refused.
        match classify(site)? {
            Classified::Vanished => last = Last::Other,
            Classified::Busy(holder) => {
                return Err(refuse(
                    LockCode::TargetLockBusy,
                    holder,
                    "another run holds the lock; wait for it to finish",
                ));
            }
            Classified::Foreign => {
                return Err(refuse(
                    LockCode::ControlPlaneNamespaceConflict,
                    None,
                    "a non-Flux object occupies the lock path; move it away",
                ));
            }
            Classified::Untrusted(record) => {
                return Err(refuse(
                    LockCode::ArtifactOwnershipUncertain,
                    Some(record),
                    UNTRUSTED_DETAIL,
                ));
            }
            Classified::Uncertain(why) => {
                return Err(refuse(
                    LockCode::TargetLockUncertain,
                    None,
                    format!(
                        "the lock's record is unreadable ({why:?}); if no Flux run is active on this destination, run again with --restart --break-lock"
                    ),
                ));
            }
            Classified::Orphan { lock, identity, record } => {
                let beat = record.last_heartbeat_wall_time;
                if beat > now_ns || now_ns - beat < lease_threshold_ns {
                    return Err(refuse(
                        LockCode::ArtifactOwnershipUncertain,
                        Some(record),
                        "the dead owner's lock is younger than the lease threshold; wait, or retry once it is older",
                    ));
                }
                match recover(site, lock, identity, &record, operation_id)? {
                    Recovered::Held { held, leftover } => {
                        return Ok(CleanupObtained {
                            held,
                            leftover_broken: leftover,
                            reclaimed: Some(record),
                        });
                    }
                    Recovered::Restart(l) => last = l,
                }
            }
            Classified::Dead { lock, identity, record } => {
                // The same lease gate as an orphan: a dead owner whose record is young may be an operation that was
                // resumed and crashed seconds ago, and the caller's manifest-age reading would not see that.
                let beat = record.last_heartbeat_wall_time;
                if beat > now_ns || now_ns - beat < lease_threshold_ns {
                    return Err(refuse(
                        LockCode::TargetLockBusy,
                        Some(record),
                        "the previous owner's lock is younger than the lease threshold; wait, or retry once it is older",
                    ));
                }
                match recover(site, lock, identity, &record, operation_id)? {
                    Recovered::Held { held, leftover } => {
                        return Ok(CleanupObtained {
                            held,
                            leftover_broken: leftover,
                            reclaimed: None,
                        });
                    }
                    Recovered::Restart(l) => last = l,
                }
            }
        }
    }
    Err(give_up(last))
}

/// Decision 2 (fork B): the refusal reports what the last pass saw. Exit 3 either way.
fn give_up(last: Last) -> LockError {
    match last {
        Last::EmptyOrTornUnheld => refuse(
            LockCode::TargetLockUncertain,
            None,
            format!(
                "gave up after {MAX_ATTEMPTS} attempts on an empty or torn lock that nobody holds; another run may be taking it over at the same time"
            ),
        ),
        Last::HeldByOther | Last::Other => refuse(
            LockCode::TargetLockBusy,
            None,
            format!(
                "gave up after {MAX_ATTEMPTS} attempts; another run keeps holding or changing the lock"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FakeDirHandle;
    use crate::lock::test_support::{dead_lock, fake, live_lock, record, refusal};
    use flux_fs::{Code, FileSystem, LockFile};
    use std::ffi::OsStr;
    use std::io::ErrorKind;
    use std::path::Path;

    const NAME: &str = "dest.flux-lock";
    const STRONG: LockCapability = LockCapability::LocalStrong;

    fn site(d: &FakeDirHandle) -> LockSite<'_, FakeDirHandle> {
        LockSite::directory(d, OsStr::new("dest")).unwrap()
    }

    fn me() -> String {
        crate::ids::new_id()
    }

    #[test]
    fn the_capability_gate_passes_only_the_allowlist() {
        let (fs, d) = fake();
        assert_eq!(check_capability(&d).unwrap(), LockCapability::LocalStrong);
        fs.set_lock_capability(LockCapability::Unsupported);
        assert_eq!(refusal(check_capability(&d)).code, LockCode::RemoteLockUnsafe);
        let (fs, d) = fake();
        fs.fail("lock_capability", Code::IoError);
        let r = refusal(check_capability(&d));
        assert_eq!(r.code, LockCode::RemoteLockUnsafe);
        assert!(
            r.detail.contains("could not be determined"),
            "the cause is reported: {}",
            r.detail
        );
    }

    #[test]
    fn a_free_target_is_acquired_with_no_record() {
        let (_fs, d) = fake();
        let Obtained::Held { held, leftover_broken } =
            obtain(&site(&d), STRONG, Mode::Plain, &me()).unwrap()
        else {
            panic!("expected Held")
        };
        assert!(held.record().is_none());
        assert_eq!(leftover_broken, None);
    }

    #[test]
    fn a_live_owner_is_busy_and_reported() {
        let (_fs, d) = fake();
        let site = site(&d);
        let rec = record(&site, &me(), "none");
        let _live = live_lock(&d, NAME, &rec.encode());
        let r = refusal(obtain(&site, STRONG, Mode::Plain, &me()));
        assert_eq!(r.code, LockCode::TargetLockBusy);
        assert_eq!(r.holder, Some(rec));
    }

    #[test]
    fn an_uncertain_lock_refuses_a_plain_run_and_is_claimed_by_break_lock() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let site = site(&d);
        assert_eq!(
            refusal(obtain(&site, STRONG, Mode::Plain, &me())).code,
            LockCode::TargetLockUncertain
        );
        assert!(matches!(
            obtain(&site, STRONG, Mode::BreakLock, &me()).unwrap(),
            Obtained::Claimed(_)
        ));
    }

    #[test]
    fn a_foreign_lock_is_a_namespace_conflict_in_every_mode() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"hello, not a lock");
        let site = site(&d);
        for mode in [Mode::Plain, Mode::BreakLock] {
            assert_eq!(
                refusal(obtain(&site, STRONG, mode, &me())).code,
                LockCode::ControlPlaneNamespaceConflict
            );
        }
    }

    #[test]
    fn a_dead_owner_is_recovered_in_every_mode() {
        for mode in [Mode::Plain, Mode::BreakLock] {
            let (fs, d) = fake();
            let site = site(&d);
            let id = me();
            for dir in [
                "/p/dest",
                "/p/dest/.flux",
                "/p/dest/.flux/operations",
                &format!("/p/dest/.flux/operations/{id}"),
            ] {
                fs.create_dir(Path::new(dir)).unwrap();
            }
            dead_lock(&d, NAME, &record(&site, &id, &format!("operations/{id}")).encode());
            let old = d.metadata(OsStr::new(NAME)).unwrap().identity;
            let Obtained::Held { held, .. } = obtain(&site, STRONG, mode, &me()).unwrap() else {
                panic!("expected Held for {mode:?}")
            };
            assert_ne!(held.identity(), &old, "{mode:?}: a new lock replaced the dead owner's");
        }
    }

    #[test]
    fn a_dead_owner_with_a_missing_workspace_is_artifact_ownership_uncertain() {
        let (_fs, d) = fake();
        let site = site(&d);
        let id = me();
        let rec = record(&site, &id, &format!("operations/{id}"));
        dead_lock(&d, NAME, &rec.encode());
        let r = refusal(obtain(&site, STRONG, Mode::BreakLock, &me()));
        assert_eq!(r.code, LockCode::ArtifactOwnershipUncertain);
        assert_eq!(r.holder, Some(rec));
        assert!(r.detail.contains("flux cleanup"), "the way out is named: {}", r.detail);
    }

    const NOW: u64 = 31_000_000_000;
    const LEASE: u64 = 30_000_000_000;

    fn orphan_at(d: &FakeDirHandle, site: &LockSite<'_, FakeDirHandle>, beat: u64) -> LockRecord {
        let id = me();
        let mut rec = record(site, &id, &format!("operations/{id}"));
        rec.last_heartbeat_wall_time = beat;
        dead_lock(d, NAME, &rec.encode());
        rec
    }

    fn bytes(d: &FakeDirHandle) -> Vec<u8> {
        d.open_lock(OsStr::new(NAME)).unwrap().read_all(crate::lock::record::RECORD_LEN).unwrap()
    }

    #[test]
    fn cleanup_reclaims_an_orphan_lock_past_the_lease_gate() {
        let (fs, d) = fake();
        let site = site(&d);
        let rec = orphan_at(&d, &site, 1);
        let old = d.metadata(OsStr::new(NAME)).unwrap().identity;
        let op = me();
        let got = obtain_cleanup_lock(&site, STRONG, &op, NOW, LEASE).unwrap();
        assert_eq!(got.reclaimed, Some(rec));
        assert_eq!(got.leftover_broken, None);
        assert_ne!(got.held.identity(), &old, "a new lock replaced the orphan's");
        let broken = std::path::Path::new("/p").join(site.broken_name(&op));
        assert!(!fs.exists(broken), "no .broken file remains");
    }

    #[test]
    fn cleanup_refuses_a_young_or_future_orphan_lock() {
        for beat in [NOW - 29_000_000_000, NOW + 1] {
            let (_fs, d) = fake();
            let site = site(&d);
            let rec = orphan_at(&d, &site, beat);
            let before = bytes(&d);
            let r = refusal(obtain_cleanup_lock(&site, STRONG, &me(), NOW, LEASE));
            assert_eq!(r.code, LockCode::ArtifactOwnershipUncertain, "beat {beat}");
            assert_eq!(r.holder, Some(rec));
            assert!(r.detail.contains("younger than the lease threshold"), "{}", r.detail);
            assert_eq!(bytes(&d), before, "the file is untouched");
        }
    }

    #[test]
    fn cleanup_reclaims_an_orphan_lock_exactly_at_the_lease_threshold() {
        let (_fs, d) = fake();
        let site = site(&d);
        orphan_at(&d, &site, NOW - LEASE);
        let got = obtain_cleanup_lock(&site, STRONG, &me(), NOW, LEASE).unwrap();
        assert!(got.reclaimed.is_some(), "age == threshold passes the gate");
    }

    #[test]
    fn cleanup_refuses_an_uncertain_lock_and_is_busy_on_a_live_one() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let r = refusal(obtain_cleanup_lock(&site(&d), STRONG, &me(), NOW, LEASE));
        assert_eq!(r.code, LockCode::TargetLockUncertain);
        let (_fs, d) = fake();
        let live_site = site(&d);
        let rec = record(&live_site, &me(), "none");
        let _live = live_lock(&d, NAME, &rec.encode());
        let r = refusal(obtain_cleanup_lock(&live_site, STRONG, &me(), NOW, LEASE));
        assert_eq!(r.code, LockCode::TargetLockBusy);
        assert_eq!(r.holder, Some(rec));
    }

    #[test]
    fn cleanup_acquires_a_free_path_like_a_run() {
        let (_fs, d) = fake();
        let got = obtain_cleanup_lock(&site(&d), STRONG, &me(), NOW, LEASE).unwrap();
        assert!(got.held.record().is_none());
        assert_eq!(got.reclaimed, None);
        assert_eq!(got.leftover_broken, None);
    }

    #[test]
    fn cleanup_recovers_a_dead_owner_with_its_workspace_past_the_lease_gate() {
        let (fs, d) = fake();
        let site = site(&d);
        let id = me();
        for dir in [
            "/p/dest",
            "/p/dest/.flux",
            "/p/dest/.flux/operations",
            &format!("/p/dest/.flux/operations/{id}"),
        ] {
            fs.create_dir(Path::new(dir)).unwrap();
        }
        let mut rec = record(&site, &id, &format!("operations/{id}"));
        rec.last_heartbeat_wall_time = NOW - LEASE;
        dead_lock(&d, NAME, &rec.encode());
        let got = obtain_cleanup_lock(&site, STRONG, &me(), NOW, LEASE).unwrap();
        assert_eq!(got.reclaimed, None, "only an orphan is reported as reclaimed");
    }

    #[test]
    fn cleanup_refuses_a_young_or_future_dead_lock_as_busy() {
        for beat in [NOW - 5_000_000_000, NOW + 1] {
            let (fs, d) = fake();
            let site = site(&d);
            let id = me();
            for dir in [
                "/p/dest",
                "/p/dest/.flux",
                "/p/dest/.flux/operations",
                &format!("/p/dest/.flux/operations/{id}"),
            ] {
                fs.create_dir(Path::new(dir)).unwrap();
            }
            let mut rec = record(&site, &id, &format!("operations/{id}"));
            rec.last_heartbeat_wall_time = beat;
            dead_lock(&d, NAME, &rec.encode());
            let before = bytes(&d);
            let r = refusal(obtain_cleanup_lock(&site, STRONG, &me(), NOW, LEASE));
            assert_eq!(r.code, LockCode::TargetLockBusy, "beat {beat}");
            assert_eq!(r.holder, Some(rec));
            assert!(r.detail.contains("younger than the lease threshold"), "{}", r.detail);
            assert_eq!(bytes(&d), before, "the file is untouched");
        }
    }

    #[test]
    fn cleanup_refuses_an_untrusted_and_a_foreign_lock_as_a_run_does() {
        let (_fs, d) = fake();
        let site1 = site(&d);
        dead_lock(&d, NAME, &record(&site1, &me(), "garbage").encode());
        let r = refusal(obtain_cleanup_lock(&site1, STRONG, &me(), NOW, LEASE));
        assert_eq!(r.code, LockCode::ArtifactOwnershipUncertain);
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"hello, not a lock");
        let r = refusal(obtain_cleanup_lock(&site(&d), STRONG, &me(), NOW, LEASE));
        assert_eq!(r.code, LockCode::ControlPlaneNamespaceConflict);
    }

    #[test]
    fn a_race_that_never_settles_gives_up_busy_after_the_budget() {
        let (fs, d) = fake();
        // Every create finds the name taken, and every classification finds it gone again.
        fs.fail_kind("create_lock", Code::IoError, ErrorKind::AlreadyExists);
        fs.fail_always("create_lock", Code::IoError);
        let r = refusal(obtain(&site(&d), STRONG, Mode::Plain, &me()));
        assert_eq!(r.code, LockCode::TargetLockBusy);
        let creates = fs.calls().iter().filter(|c| c.starts_with("create_lock(")).count();
        assert_eq!(creates, MAX_ATTEMPTS as usize);
    }

    #[test]
    fn a_takeover_that_restarts_every_pass_gives_up_uncertain() {
        let (fs, d) = fake();
        dead_lock(&d, NAME, b"");
        // The lock path's identity never matches the opened file's, so every takeover's step 4 starts again.
        fs.set_identity(
            "/p/dest.flux-lock",
            flux_fs::FileIdentity::Strong(flux_fs::ObjectId { volume: 9, index: 9 }),
        );
        let r = refusal(obtain(&site(&d), STRONG, Mode::BreakLock, &me()));
        assert_eq!(r.code, LockCode::TargetLockUncertain, "{}", r.detail);
        let opens = fs.calls().iter().filter(|c| c.starts_with("open_lock")).count();
        assert_eq!(opens, 2 * MAX_ATTEMPTS as usize, "each pass classified and tried a takeover");
    }

    #[test]
    fn giving_up_reports_what_the_last_pass_saw() {
        let code = |l| match give_up(l) {
            LockError::Refused(r) => r.code,
            LockError::Io(e) => panic!("{e:?}"),
        };
        assert_eq!(code(Last::EmptyOrTornUnheld), LockCode::TargetLockUncertain);
        assert_eq!(code(Last::HeldByOther), LockCode::TargetLockBusy);
        assert_eq!(code(Last::Other), LockCode::TargetLockBusy);
    }

    #[test]
    fn a_claimed_lock_is_still_held_against_a_plain_run() {
        let (_fs, d) = fake();
        dead_lock(&d, NAME, b"");
        let site = site(&d);
        let claim = obtain(&site, STRONG, Mode::BreakLock, &me()).unwrap();
        assert!(matches!(claim, Obtained::Claimed(_)));
        assert_eq!(
            refusal(obtain(&site, STRONG, Mode::Plain, &me())).code,
            LockCode::TargetLockBusy
        );
        drop(claim);
        assert!(d.open_lock(OsStr::new(NAME)).unwrap().try_lock().unwrap());
    }

    #[test]
    fn a_released_lock_still_open_elsewhere_under_legacy_delete_is_busy_until_it_closes() {
        let (fs, d) = fake();
        fs.set_legacy_delete(true);
        let site = site(&d);
        let Obtained::Held { held: mut theirs, .. } =
            obtain(&site, STRONG, Mode::Plain, &me()).unwrap()
        else {
            panic!("expected Held")
        };
        theirs.write_record(record(&site, &me(), "none")).unwrap();
        // Another run's classification handle, still open when the holder releases.
        let classifier = d.open_lock(OsStr::new(NAME)).unwrap();
        assert_eq!(theirs.release().unwrap(), crate::lock::Released::Unlinked);
        let r = refusal(obtain(&site, STRONG, Mode::Plain, &me()));
        assert_eq!(r.code, LockCode::TargetLockBusy, "{}", r.detail);
        drop(classifier);
        assert!(matches!(
            obtain(&site, STRONG, Mode::Plain, &me()).unwrap(),
            Obtained::Held { .. }
        ));
    }
}
