//! The prefix locks of a several-sources run (spec 3.4): one `<name>.flux-lock` per prefix name in the destination
//! directory, each with its record written before the next is created, so an alias (two names one lock file) is told
//! from a lock another operation holds (R1, R7).

use crate::lock::record::LockRecord;
use crate::lock::{Held, LockCode, LockError, LockSite, Mode, Obtained, Released, obtain};
use flux_fs::{DirHandle, LockCapability};
use std::ffi::OsString;

/// The identity every prefix lock record carries.
#[allow(dead_code)] // used by run::multi (plan task 6)
pub(crate) struct PrefixId<'a> {
    pub operation_id: &'a str,
    pub owner_instance_id: &'a str,
    pub boot_session_id: &'a str,
    pub now: u64,
}

/// `PrefixId` with owned strings, for a caller that must keep it across calls.
#[allow(dead_code)] // used by run::multi (plan task 6)
pub(crate) struct PrefixIdOwned {
    pub operation_id: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub now: u64,
}

impl PrefixIdOwned {
    #[allow(dead_code)] // used by run::multi (plan task 6)
    pub(crate) fn as_ref(&self) -> PrefixId<'_> {
        PrefixId {
            operation_id: &self.operation_id,
            owner_instance_id: &self.owner_instance_id,
            boot_session_id: &self.boot_session_id,
            now: self.now,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // used by run::multi (plan task 6)
pub(crate) enum PrefixRefusal {
    /// Two names of this run are one lock file (a case or normalization alias): `name` met the lock `other` holds.
    Collision {
        name: OsString,
        other: OsString,
    },
    Lock(LockError),
}

/// The prefix locks a run holds, in the order they were taken.
#[allow(dead_code)] // used by run::multi (plan task 6)
pub(crate) struct PrefixLocks<'a, D: DirHandle> {
    held: Vec<(OsString, Held<'a, D>)>,
}

/// Release every lock in `held`, collecting the failures. A `Held` with a record is never `discard`ed.
fn release_held<D: DirHandle>(held: Vec<(OsString, Held<'_, D>)>) -> Vec<(OsString, LockError)> {
    let mut failed = Vec::new();
    for (name, h) in held {
        let result: Result<Released, LockError> = h.release();
        if let Err(e) = result {
            failed.push((name, e));
        }
    }
    failed
}

impl<'a, D: DirHandle> PrefixLocks<'a, D> {
    /// Lock each of `names` in `dir`, in order, writing each record before the next lock is created. On any refusal
    /// the locks already taken are released. The second value is the `.broken` leftovers a recovery reported.
    #[allow(dead_code)] // used by run::multi (plan task 6)
    pub(crate) fn acquire(
        dir: &'a D,
        cap: LockCapability,
        names: &[OsString],
        id: &PrefixId<'_>,
    ) -> Result<(Self, Vec<OsString>), PrefixRefusal> {
        let mut held: Vec<(OsString, Held<'a, D>)> = Vec::new();
        let mut leftovers = Vec::new();
        for name in names {
            match Self::take(dir, cap, name, id, &held) {
                Ok((h, leftover)) => {
                    leftovers.extend(leftover);
                    held.push((name.clone(), h));
                }
                Err(refusal) => {
                    // Failures to release cannot be reported beside the refusal that caused them.
                    let _ = release_held(held);
                    return Err(refusal);
                }
            }
        }
        Ok((Self { held }, leftovers))
    }

    fn take(
        dir: &'a D,
        cap: LockCapability,
        name: &OsString,
        id: &PrefixId<'_>,
        held: &[(OsString, Held<'a, D>)],
    ) -> Result<(Held<'a, D>, Option<OsString>), PrefixRefusal> {
        let site = LockSite::directory(dir, name).map_err(PrefixRefusal::Lock)?;
        // Built before `obtain`: once a `Held` exists, the only fallible step left is `write_record`, so no error
        // path can leave a recordless lock file behind (`Held` has no `Drop`).
        let complete_lock_key = site.complete_lock_key().map_err(PrefixRefusal::Lock)?;
        match obtain(&site, cap, Mode::Plain, id.operation_id) {
            Ok(Obtained::Held { held: mut h, leftover_broken }) => {
                let record = LockRecord {
                    complete_lock_key,
                    operation_id: id.operation_id.to_string(),
                    owner_instance_id: id.owner_instance_id.to_string(),
                    boot_session_id: id.boot_session_id.to_string(),
                    target_path_key: site.target_path_key(),
                    workspace_path: "none".to_string(),
                    creation_wall_time: id.now,
                    last_heartbeat_wall_time: id.now,
                };
                if let Err(e) = h.write_record(record) {
                    // No record landed: `discard` is the right release for a lock without one.
                    let _ = h.discard();
                    return Err(PrefixRefusal::Lock(e));
                }
                Ok((h, leftover_broken))
            }
            Ok(Obtained::Claimed(_)) => {
                unreachable!("Mode::Plain never claims a lock (lock/obtain.rs:126)")
            }
            Err(LockError::Refused(r)) => {
                if r.code == LockCode::TargetLockBusy
                    && let Some(holder) = r.holder.as_ref()
                    && holder.operation_id == id.operation_id
                {
                    // Our own record holds this lock file, so an earlier name of ours is the same file. The earlier
                    // name is found by key; an unmatched key (not reachable: only this run writes this operation id)
                    // names the current one twice rather than invent a name.
                    let other = held
                        .iter()
                        .find(|(_, h)| {
                            h.record()
                                .is_some_and(|rec| rec.target_path_key == holder.target_path_key)
                        })
                        .map_or_else(|| name.clone(), |(n, _)| n.clone());
                    return Err(PrefixRefusal::Collision { name: name.clone(), other });
                }
                Err(PrefixRefusal::Lock(LockError::Refused(r)))
            }
            Err(e) => Err(PrefixRefusal::Lock(e)),
        }
    }

    /// Release every lock; the failures only.
    #[allow(dead_code)] // used by run::multi (plan task 6)
    pub(crate) fn release_all(self) -> Vec<(OsString, LockError)> {
        release_held(self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::record::{Decoded, decode};
    use crate::lock::test_support::fake;
    use std::path::Path;

    const CAP: LockCapability = LockCapability::LocalStrong;

    /// A well-formed operation id (32 lowercase hex digits, `ids::is_id`) derived from the one-letter tag.
    fn op(tag: char) -> String {
        tag.to_string().repeat(32)
    }

    fn id(tag: char, now: u64) -> PrefixIdOwned {
        PrefixIdOwned {
            operation_id: op(tag),
            owner_instance_id: format!("owner-{tag}"),
            boot_session_id: "boot".to_string(),
            now,
        }
    }

    fn names(n: &[&str]) -> Vec<OsString> {
        n.iter().map(OsString::from).collect()
    }

    fn record_of(fs: &crate::fault_fs::FaultFs, file: &str) -> LockRecord {
        match decode(&fs.read_file(Path::new("/p").join(file)).expect("lock file exists")) {
            Decoded::Record(r) => r,
            other => panic!("expected a record, got {other:?}"),
        }
    }

    fn position(calls: &[String], prefix: &str) -> usize {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no call {prefix}"))
    }

    #[test]
    fn each_lock_gets_its_record_before_the_next_is_created() {
        let (fs, d) = fake();
        let x = id('a', 5);
        let (locks, _) = PrefixLocks::acquire(&d, CAP, &names(&["a", "b"]), &x.as_ref()).unwrap();
        let calls = fs.calls();
        let create_a = position(&calls, "create_lock(/p/a.flux-lock)");
        let write_a = position(&calls, "write_at_start(/p/a.flux-lock)");
        let create_b = position(&calls, "create_lock(/p/b.flux-lock)");
        assert!(create_a < write_a && write_a < create_b, "{calls:?}");
        let rec = record_of(&fs, "a.flux-lock");
        assert_eq!(rec.workspace_path, "none");
        assert_eq!(rec.operation_id, op('a'));
        assert_eq!(rec.creation_wall_time, 5);
        assert_eq!(rec.last_heartbeat_wall_time, 5);
        assert!(locks.release_all().is_empty());
    }

    #[test]
    fn an_alias_is_a_collision_not_a_busy_lock() {
        let (fs, d) = fake();
        fs.set_case_insensitive(true);
        let x = id('a', 5);
        let r = PrefixLocks::acquire(&d, CAP, &names(&["Data", "data"]), &x.as_ref());
        match r {
            Err(PrefixRefusal::Collision { name, other }) => {
                assert_eq!(name, OsString::from("data"));
                assert_eq!(other, OsString::from("Data"));
            }
            Err(PrefixRefusal::Lock(e)) => panic!("expected a collision, got {e:?}"),
            Ok(_) => panic!("expected a collision"),
        }
        assert!(!fs.exists("/p/Data.flux-lock"));
        assert!(!fs.exists("/p/data.flux-lock"));
    }

    #[test]
    fn a_lock_held_by_another_operation_is_busy() {
        let (fs, d) = fake();
        let x = id('a', 5);
        let y = id('b', 6);
        let (_held, _) = PrefixLocks::acquire(&d, CAP, &names(&["a"]), &x.as_ref()).unwrap();
        match PrefixLocks::acquire(&d, CAP, &names(&["a", "b"]), &y.as_ref()) {
            Err(PrefixRefusal::Lock(LockError::Refused(r))) => {
                assert_eq!(r.code, LockCode::TargetLockBusy);
            }
            Err(_) => panic!("expected a busy lock"),
            Ok(_) => panic!("expected a refusal"),
        }
        assert!(!fs.exists("/p/b.flux-lock"));
        assert!(fs.exists("/p/a.flux-lock"), "the other operation's lock is untouched");
    }

    #[test]
    fn a_failure_releases_the_locks_already_taken() {
        let (fs, d) = fake();
        let x = id('a', 5);
        let y = id('b', 6);
        let (_held, _) = PrefixLocks::acquire(&d, CAP, &names(&["b"]), &x.as_ref()).unwrap();
        let r = PrefixLocks::acquire(&d, CAP, &names(&["a", "b"]), &y.as_ref());
        assert!(matches!(r, Err(PrefixRefusal::Lock(_))));
        assert!(!fs.exists("/p/a.flux-lock"));
        assert!(fs.exists("/p/b.flux-lock"));
    }

    #[test]
    fn a_dead_prefix_lock_with_a_none_record_is_recovered() {
        let (fs, d) = fake();
        let x = id('a', 5);
        let (first, _) = PrefixLocks::acquire(&d, CAP, &names(&["a"]), &x.as_ref()).unwrap();
        drop(first);
        let y = id('b', 6);
        let (second, _) = PrefixLocks::acquire(&d, CAP, &names(&["a"]), &y.as_ref()).unwrap();
        assert_eq!(record_of(&fs, "a.flux-lock").operation_id, op('b'));
        assert!(second.release_all().is_empty());
    }

    #[test]
    fn release_all_removes_every_file() {
        let (fs, d) = fake();
        let x = id('a', 5);
        let (locks, _) =
            PrefixLocks::acquire(&d, CAP, &names(&["a", "b", "c"]), &x.as_ref()).unwrap();
        assert!(fs.exists("/p/c.flux-lock"));
        assert!(locks.release_all().is_empty());
        for n in ["a", "b", "c"] {
            assert!(!fs.exists(format!("/p/{n}.flux-lock")));
        }
    }

    #[test]
    fn a_live_prefix_lock_with_an_old_heartbeat_is_busy_and_the_same_lock_unheld_is_dead() {
        let (_fs, d) = fake();
        // The record's heartbeat is 1 ns after the epoch: as old as a heartbeat gets.
        let x = id('a', 1);
        let y = id('b', 10_000_000_000_000_000_000);
        let (first, _) = PrefixLocks::acquire(&d, CAP, &names(&["a"]), &x.as_ref()).unwrap();
        match PrefixLocks::acquire(&d, CAP, &names(&["a"]), &y.as_ref()) {
            Err(PrefixRefusal::Lock(LockError::Refused(r))) => {
                assert_eq!(r.code, LockCode::TargetLockBusy);
            }
            Err(_) => panic!("expected a busy lock"),
            Ok(_) => panic!("a live lock is busy whatever its heartbeat age"),
        }
        drop(first);
        let (again, _) = PrefixLocks::acquire(&d, CAP, &names(&["a"]), &y.as_ref()).unwrap();
        assert!(again.release_all().is_empty());
    }
}
