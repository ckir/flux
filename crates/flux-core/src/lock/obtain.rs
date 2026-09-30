//! The run's steps 1 and 3 ("The run"): the capability gate, then acquisition, classifying and recovering or taking
//! over as the model's `plain`, `rec` and `brk` processes do (`S21_1_decide`, `S21_1_refused`,
//! `S21_1_restart_decide`).

use super::acquire::{Acquire, Last, acquire};
use super::classify::{Classified, classify};
use super::error::{LockCode, LockError, LockResult, refuse};
use super::held::Held;
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
                    "the dead owner's record names a workspace that is missing or not one Flux trusts",
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
}
