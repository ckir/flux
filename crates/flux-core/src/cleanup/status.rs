//! The cleanup status table (cut 9b spec, "Statuses, eligibility and the table"): a pure function from the facts
//! about one entry to its status, eligibility and note. No I/O; the discovery code builds the `Facts`.

use super::Status;
use crate::state::OpState;

/// A reading of a time against `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    Seconds(u64),
    Future,
    Unreadable,
}

/// The one probe of the lock (spec decision 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockProbe {
    Absent,
    /// `try_lock` succeeded and was dropped; `record` is the decoded record if the bytes were one.
    Free {
        record: Option<LockRecordFacts>,
    },
    /// Another process holds it; `names` is the record's `operation_id` when readable.
    Busy {
        names: Option<String>,
    },
    /// Torn, empty, newer-versioned or not a Flux record, and nobody holds it.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRecordFacts {
    pub operation_id: String,
    pub workspace_missing: bool,
    pub lease_age: Age,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestProblem {
    Missing,
    NotRegular(String),
    Corrupt(String),
    WrongOperation,
    UnknownFormat(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Operation {
        id: String,
        manifest: Result<OpState, ManifestProblem>,
        manifest_age: Age,
    },
    Debris,
    /// The root lock itself (`moved_aside: false`), or a `<lock>.broken.*` file (`true`); its record facts travel in `LockProbe`.
    RootLock {
        moved_aside: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub subject: Subject,
    pub lock: LockProbe,
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub retention_secs: u64,
    pub lease_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub status: Status,
    pub eligible: bool,
    pub note: String,
    pub age_seconds: Option<u64>,
}

const LEASE_UNCERTAIN: &str = "LEASE_AGE_UNCERTAIN";

fn seconds(age: Age) -> Option<u64> {
    match age {
        Age::Seconds(s) => Some(s),
        Age::Future | Age::Unreadable => None,
    }
}

/// Classifies one entry, rows in the spec's order, first match wins.
pub fn classify(facts: &Facts, t: &Thresholds) -> Verdict {
    let lock_free = matches!(facts.lock, LockProbe::Absent | LockProbe::Free { .. });
    let mut v = table(facts, t, lock_free);
    // A busy lock naming another operation (or none) holds the whole DEST: classified, never eligible.
    if let LockProbe::Busy { names } = &facts.lock {
        let own = match &facts.subject {
            Subject::Operation { id, .. } => names.as_deref() == Some(id.as_str()),
            // The lock's own row: LIVE when its owner is alive.
            Subject::RootLock { .. } => true,
            Subject::Debris => false,
        };
        if !own {
            v.eligible = false;
            v.note = format!("destination lock held by {}", names.as_deref().unwrap_or("unknown"));
        }
    }
    v
}

fn verdict(status: Status, eligible: bool, note: &str, age: Option<u64>) -> Verdict {
    Verdict { status, eligible, note: note.to_string(), age_seconds: age }
}

fn table(facts: &Facts, t: &Thresholds, lock_free: bool) -> Verdict {
    match &facts.subject {
        Subject::Operation { id, manifest, manifest_age } => {
            let age = seconds(*manifest_age);
            // Row 1.
            if let LockProbe::Busy { names: Some(n) } = &facts.lock
                && n == id
            {
                return verdict(Status::Live, false, "", age);
            }
            // Rows 2 and 3.
            let state = match manifest {
                Err(ManifestProblem::UnknownFormat(n)) => {
                    return verdict(Status::Uncertain, false, &format!("format {n}"), age);
                }
                Err(problem) => {
                    let note = match problem {
                        ManifestProblem::Missing => "manifest missing".to_string(),
                        ManifestProblem::NotRegular(m) => {
                            format!("manifest not a regular file: {m}")
                        }
                        ManifestProblem::Corrupt(m) => format!("manifest corrupt: {m}"),
                        ManifestProblem::WrongOperation => {
                            "manifest names another operation".to_string()
                        }
                        ManifestProblem::UnknownFormat(_) => String::new(),
                    };
                    return verdict(Status::Corrupt, false, &note, age);
                }
                Ok(s) => *s,
            };
            // Rows 4 and 5.
            match state {
                OpState::Completed => {
                    return verdict(Status::CompletedButUnclean, lock_free, "", age);
                }
                OpState::Abandoned => return verdict(Status::Stale, lock_free, "", age),
                OpState::Created | OpState::Transferring | OpState::Failed => {}
            }
            // The lease age: the record's when it is this operation's, else the manifest's mtime.
            let lease = match &facts.lock {
                LockProbe::Free { record: Some(r) } if &r.operation_id == id => r.lease_age,
                _ => *manifest_age,
            };
            // Row 6.
            let (Age::Seconds(retention_age), Age::Seconds(lease_age)) = (*manifest_age, lease)
            else {
                return verdict(Status::Uncertain, false, LEASE_UNCERTAIN, age);
            };
            // Row 7.
            if lease_age < t.lease_secs {
                return verdict(Status::Resumable, false, "", age);
            }
            // Row 8.
            if retention_age >= t.retention_secs {
                return verdict(Status::Stale, lock_free, "", age);
            }
            // Row 9.
            verdict(Status::Resumable, lock_free && facts.force, "", age)
        }
        // Row 10.
        Subject::Debris => verdict(Status::Stale, lock_free, "debris", None),
        Subject::RootLock { moved_aside } => match &facts.lock {
            // Row 1, the lock's own row.
            LockProbe::Busy { .. } => verdict(Status::Live, false, "", None),
            // Row 13.
            LockProbe::Unreadable | LockProbe::Absent | LockProbe::Free { record: None } => {
                verdict(
                    Status::Uncertain,
                    false,
                    "lock bytes are not a readable record; needs --break-lock (not in 9b)",
                    None,
                )
            }
            LockProbe::Free { record: Some(r) } => {
                let age = seconds(r.lease_age);
                // Rows 11 and 12 apply to an orphan lock or a moved-aside one.
                if !*moved_aside && !r.workspace_missing {
                    return verdict(Status::Uncertain, false, "workspace exists", age);
                }
                match r.lease_age {
                    Age::Seconds(s) if s >= t.lease_secs => verdict(
                        Status::Stale,
                        true,
                        if *moved_aside { "moved-aside lock" } else { "orphan lock" },
                        age,
                    ),
                    Age::Seconds(_) => verdict(
                        Status::Uncertain,
                        false,
                        &format!("lease younger than {} s", t.lease_secs),
                        age,
                    ),
                    Age::Future | Age::Unreadable => {
                        verdict(Status::Uncertain, false, LEASE_UNCERTAIN, age)
                    }
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Thresholds = Thresholds { retention_secs: 604_800, lease_secs: 30 };
    const WEEK: Age = Age::Seconds(604_800);

    fn verdict(subject: Subject, lock: LockProbe, force: bool) -> Verdict {
        classify(&Facts { subject, lock, force }, &T)
    }

    fn op(state: OpState, manifest_age: Age) -> Subject {
        Subject::Operation { id: "op1".into(), manifest: Ok(state), manifest_age }
    }

    fn bad(problem: ManifestProblem) -> Subject {
        Subject::Operation {
            id: "op1".into(),
            manifest: Err(problem),
            manifest_age: Age::Seconds(5),
        }
    }

    fn free() -> LockProbe {
        LockProbe::Free { record: None }
    }

    fn busy(name: &str) -> LockProbe {
        LockProbe::Busy { names: Some(name.into()) }
    }

    fn rec(id: &str, workspace_missing: bool, lease_age: Age) -> LockProbe {
        LockProbe::Free {
            record: Some(LockRecordFacts { operation_id: id.into(), workspace_missing, lease_age }),
        }
    }

    #[test]
    fn row_1_a_busy_lock_naming_this_operation_is_live() {
        let v = verdict(op(OpState::Completed, Age::Seconds(5)), busy("op1"), true);
        assert_eq!((v.status, v.eligible), (Status::Live, false));
        // Distractor: busy naming another id keeps the table's status but is ineligible.
        let v = verdict(op(OpState::Completed, Age::Seconds(5)), busy("other"), true);
        assert_eq!((v.status, v.eligible), (Status::CompletedButUnclean, false));
        assert_eq!(v.note, "destination lock held by other");
        let v =
            verdict(op(OpState::Completed, Age::Seconds(5)), LockProbe::Busy { names: None }, true);
        assert_eq!(v.note, "destination lock held by unknown");
        // The lock's own row.
        let v = verdict(Subject::RootLock { moved_aside: false }, busy("anyone"), true);
        assert_eq!((v.status, v.eligible), (Status::Live, false));
        let v = verdict(
            Subject::RootLock { moved_aside: false },
            LockProbe::Busy { names: None },
            true,
        );
        assert_eq!(v.status, Status::Live);
    }

    #[test]
    fn row_2_a_bad_manifest_is_corrupt() {
        for problem in [
            ManifestProblem::Missing,
            ManifestProblem::NotRegular("dir".into()),
            ManifestProblem::Corrupt("eof".into()),
            ManifestProblem::WrongOperation,
        ] {
            for force in [false, true] {
                let v = verdict(bad(problem.clone()), free(), force);
                assert_eq!((v.status, v.eligible), (Status::Corrupt, false), "{problem:?}");
            }
        }
    }

    #[test]
    fn row_3_an_unknown_format_is_uncertain() {
        let v = verdict(bad(ManifestProblem::UnknownFormat(4)), free(), true);
        assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
        assert_eq!(v.note, "format 4");
    }

    #[test]
    fn row_4_completed_is_unclean_and_eligible_when_the_lock_is_free() {
        for lock in [LockProbe::Absent, free()] {
            let v = verdict(op(OpState::Completed, Age::Unreadable), lock, false);
            assert_eq!((v.status, v.eligible), (Status::CompletedButUnclean, true));
        }
        let v = verdict(op(OpState::Completed, Age::Seconds(1)), busy("other"), false);
        assert_eq!((v.status, v.eligible), (Status::CompletedButUnclean, false));
    }

    #[test]
    fn row_5_abandoned_is_stale() {
        for age in [Age::Seconds(1), Age::Unreadable] {
            let v = verdict(op(OpState::Abandoned, age), LockProbe::Absent, false);
            assert_eq!((v.status, v.eligible), (Status::Stale, true));
        }
    }

    #[test]
    fn row_6_a_future_or_unreadable_age_is_uncertain() {
        for force in [false, true] {
            for age in [Age::Future, Age::Unreadable] {
                let v = verdict(op(OpState::Transferring, age), free(), force);
                assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
                assert_eq!(v.note, "LEASE_AGE_UNCERTAIN");
            }
            let v = verdict(op(OpState::Transferring, WEEK), rec("op1", false, Age::Future), force);
            assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
            assert_eq!(v.note, "LEASE_AGE_UNCERTAIN");
        }
    }

    #[test]
    fn row_7_a_young_lease_is_resumable_never_eligible() {
        let v = verdict(op(OpState::Failed, WEEK), rec("op1", false, Age::Seconds(29)), true);
        assert_eq!((v.status, v.eligible), (Status::Resumable, false));
        // Distractor: a lease of exactly 30 s is past the threshold.
        let v = verdict(op(OpState::Failed, WEEK), rec("op1", false, Age::Seconds(30)), true);
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
    }

    #[test]
    fn a_young_lease_keeps_an_old_workspace_resumable() {
        let v = verdict(op(OpState::Failed, WEEK), rec("op1", false, Age::Seconds(29)), true);
        assert_eq!((v.status, v.eligible), (Status::Resumable, false));
        // A record naming another operation does not lend its lease.
        let v = verdict(op(OpState::Failed, WEEK), rec("other", false, Age::Seconds(1)), false);
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
    }

    #[test]
    fn row_8_old_and_past_the_lease_is_stale() {
        let v = verdict(op(OpState::Created, WEEK), free(), false);
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
        let v = verdict(op(OpState::Created, Age::Seconds(604_799)), free(), false);
        assert_eq!(v.status, Status::Resumable);
    }

    #[test]
    fn row_9_resumable_is_eligible_only_with_force() {
        for force in [false, true] {
            let v = verdict(op(OpState::Transferring, Age::Seconds(3600)), free(), force);
            assert_eq!((v.status, v.eligible), (Status::Resumable, force));
            let v = verdict(op(OpState::Transferring, Age::Seconds(3600)), busy("other"), force);
            assert_eq!((v.status, v.eligible), (Status::Resumable, false));
        }
    }

    #[test]
    fn row_10_debris_is_stale() {
        let v = verdict(Subject::Debris, LockProbe::Absent, false);
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
        assert_eq!(v.note, "debris");
        let v = verdict(Subject::Debris, busy("other"), false);
        assert_eq!((v.status, v.eligible), (Status::Stale, false));
    }

    #[test]
    fn row_11_an_orphan_lock_past_the_lease_is_stale() {
        let v = verdict(
            Subject::RootLock { moved_aside: false },
            rec("gone", true, Age::Seconds(30)),
            false,
        );
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
        assert_eq!(v.note, "orphan lock");
        assert_eq!(v.age_seconds, Some(30));
        let v = verdict(
            Subject::RootLock { moved_aside: true },
            rec("gone", false, Age::Seconds(30)),
            false,
        );
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
        assert_eq!(v.note, "moved-aside lock");
    }

    #[test]
    fn row_11_a_root_lock_whose_workspace_exists_is_never_eligible() {
        for force in [false, true] {
            let v = verdict(
                Subject::RootLock { moved_aside: false },
                rec("live", false, Age::Seconds(3600)),
                force,
            );
            assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
            assert_eq!(v.note, "workspace exists");
        }
        // Distractor: a moved-aside file is reclaimed whatever its record says (Decision 8).
        let v = verdict(
            Subject::RootLock { moved_aside: true },
            rec("live", false, Age::Seconds(3600)),
            false,
        );
        assert_eq!((v.status, v.eligible), (Status::Stale, true));
    }

    #[test]
    fn a_young_manifest_age_is_the_lease_when_no_record_names_this_operation() {
        for force in [false, true] {
            let v = verdict(op(OpState::Failed, Age::Seconds(10)), LockProbe::Absent, force);
            assert_eq!((v.status, v.eligible), (Status::Resumable, false));
            let v = verdict(
                op(OpState::Failed, Age::Seconds(10)),
                rec("other", false, Age::Seconds(3600)),
                force,
            );
            assert_eq!((v.status, v.eligible), (Status::Resumable, false));
        }
    }

    #[test]
    fn row_12_a_young_or_future_orphan_lock_is_uncertain() {
        let v = verdict(
            Subject::RootLock { moved_aside: false },
            rec("gone", true, Age::Seconds(29)),
            false,
        );
        assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
        assert_eq!(v.note, "lease younger than 30 s");
        let v = verdict(
            Subject::RootLock { moved_aside: false },
            rec("gone", true, Age::Future),
            false,
        );
        assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
        assert_eq!(v.note, "LEASE_AGE_UNCERTAIN");
    }

    #[test]
    fn row_13_an_unreadable_lock_is_uncertain() {
        let v = verdict(Subject::RootLock { moved_aside: false }, LockProbe::Unreadable, true);
        assert_eq!((v.status, v.eligible), (Status::Uncertain, false));
        assert!(v.note.contains("--break-lock"));
        let v = verdict(op(OpState::Completed, Age::Seconds(5)), LockProbe::Unreadable, true);
        assert_eq!((v.status, v.eligible), (Status::CompletedButUnclean, false));
    }

    #[test]
    fn row_order_first_match_wins() {
        let v = verdict(op(OpState::Completed, Age::Seconds(5)), busy("op1"), false);
        assert_eq!(v.status, Status::Live);
    }

    #[test]
    fn status_and_kind_spellings() {
        use super::super::EntryKind;
        let all = [
            (Status::Live, "LIVE"),
            (Status::Resumable, "RESUMABLE"),
            (Status::Stale, "STALE"),
            (Status::CompletedButUnclean, "COMPLETED_BUT_UNCLEAN"),
            (Status::Uncertain, "UNCERTAIN"),
            (Status::Corrupt, "CORRUPT"),
        ];
        for (s, n) in all {
            assert_eq!(s.as_str(), n);
        }
        assert_eq!(EntryKind::Operation.as_str(), "operation");
        assert_eq!(EntryKind::Debris.as_str(), "debris");
        assert_eq!(EntryKind::RootLock.as_str(), "root-lock");
    }
}
