// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The glue between `flux copy` and the live progress display: a `Live` that either draws the
//! engine's `Progress` and prints record lines above the bar, or is plain and prints each
//! record line through the caller's sink exactly as before.

use flux_core::progress::Progress;
use flux_core::{TreeFailure, TreeFailureCause};
use flux_ui::{Display, ProgressDrawTarget};
use std::sync::Arc;

/// Whether a live display is wanted at all: not for a dry run and not for `--json`.
#[must_use]
pub fn display_wanted(dry_run: bool, json: bool) -> bool {
    !dry_run && !json
}

/// Whether a streamed record counts toward the live display's error tally; the variants the
/// failure tally counts, and none of the non-failure skips or plans.
#[must_use]
pub fn counts_as_failure(cause: &TreeFailureCause) -> bool {
    match cause {
        TreeFailureCause::Walk(_)
        | TreeFailureCause::CreateDir(_)
        | TreeFailureCause::Copy(_)
        | TreeFailureCause::PublishedWithComplaints(_)
        | TreeFailureCause::ClaimNotRecorded(_) => true,
        TreeFailureCause::SpecialFileSkipped
        | TreeFailureCause::SymlinkSkipped
        | TreeFailureCause::MountSkipped
        | TreeFailureCause::Planned(_) => false,
    }
}

/// A running display with its progress handle, or the plain path that prints each line.
pub struct Live {
    display: Option<Display>,
    progress: Option<Arc<Progress>>,
    plain: fn(&str),
}

impl Live {
    /// Start a display when `enabled` and the target is visible; otherwise the plain path
    /// (no handle, no thread).
    #[must_use]
    pub fn start(enabled: bool, target: ProgressDrawTarget, plain: fn(&str)) -> Live {
        if enabled {
            let progress = Progress::new();
            if let Some(display) = Display::start(Arc::clone(&progress), target) {
                return Live { display: Some(display), progress: Some(progress), plain };
            }
        }
        Live { display: None, progress: None, plain }
    }

    /// The display `flux copy` runs under: wanted unless `--dry-run` or `--json`. The engine's
    /// handle is stored in `progress` (`RunConfig.progress`), `None` on the plain path.
    #[must_use]
    pub fn for_copy(
        dry_run: bool,
        json: bool,
        target: ProgressDrawTarget,
        plain: fn(&str),
        progress: &mut Option<Arc<Progress>>,
    ) -> Live {
        let live = Live::start(display_wanted(dry_run, json), target, plain);
        *progress = live.progress();
        live
    }

    /// The handle the engine ticks; `None` when no display runs.
    #[must_use]
    pub fn progress(&self) -> Option<Arc<Progress>> {
        self.progress.clone()
    }

    /// Print one record line: above the bar when live (a counted failure also ticks the error
    /// tally), through the plain sink otherwise.
    pub fn record(&self, line: &str, counted_failure: bool) {
        match (&self.display, &self.progress) {
            (Some(display), Some(progress)) => {
                display.println(line);
                if counted_failure {
                    progress.add_error();
                }
            }
            _ => (self.plain)(line),
        }
    }

    /// Print every line of one streamed failure record; a counted failure ticks the error tally
    /// once per record, not once per line of it.
    pub fn record_failure(&self, f: &TreeFailure) {
        let counted = counts_as_failure(&f.cause);
        for (index, line) in crate::report::record_lines(f).into_iter().enumerate() {
            self.record(&line, counted && index == 0);
        }
    }

    /// Stop the display and clear the bar; nothing for the plain path.
    pub fn finish(self) {
        if let Some(display) = self.display {
            display.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::TreeFailureCause as C;
    use flux_core::copy::{CopyError, CopyStep};
    use flux_core::dry_run::{PlannedAction, PlannedEntry, PlannedObject};
    use flux_fs::{Code, FsError, MetadataFailure, MetadataItem};
    use flux_ui::ProgressDrawTarget;
    use indicatif::InMemoryTerm;
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn plain(line: &str) {
        LINES.with(|l| l.borrow_mut().push(line.to_owned()));
    }

    fn taken() -> Vec<String> {
        LINES.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    fn target(term: &InMemoryTerm) -> ProgressDrawTarget {
        ProgressDrawTarget::term_like_with_hz(Box::new(term.clone()), 255)
    }

    fn io(msg: &str) -> std::io::Error {
        std::io::Error::other(msg.to_owned())
    }

    #[test]
    fn display_wanted_truth_table() {
        assert!(display_wanted(false, false));
        assert!(!display_wanted(true, false));
        assert!(!display_wanted(false, true));
        assert!(!display_wanted(true, true));
    }

    #[test]
    fn record_prints_above_the_bar_and_counts_a_failure() {
        let term = InMemoryTerm::new(20, 80);
        let live = Live::start(true, target(&term), plain);
        assert!(live.progress().is_some());
        live.record("a record line", true);
        assert!(term.contents().contains("a record line"), "{}", term.contents());
        assert_eq!(live.progress().unwrap().snapshot().errors, 1);
        live.record("another line", false);
        assert_eq!(live.progress().unwrap().snapshot().errors, 1);
        assert!(taken().is_empty());
    }

    #[test]
    fn finish_clears_the_bar_before_the_next_write() {
        let term = InMemoryTerm::new(20, 80);
        let live = Live::start(true, target(&term), plain);
        assert!(term.contents().contains("Discovered"), "{}", term.contents());
        live.finish();
        assert!(!term.contents().contains("Discovered"), "{}", term.contents());
    }

    #[test]
    fn a_disabled_live_writes_exactly_the_lines_it_is_given() {
        let term = InMemoryTerm::new(20, 80);
        let live = Live::start(false, target(&term), plain);
        assert!(live.progress().is_none());
        live.record("one", true);
        live.record("two", false);
        live.finish();
        assert_eq!(taken(), ["one", "two"]);
        assert!(term.contents().trim().is_empty(), "{}", term.contents());
    }

    #[test]
    fn a_hidden_target_is_the_plain_path() {
        let live = Live::start(true, ProgressDrawTarget::hidden(), plain);
        assert!(live.progress().is_none());
        live.record("x", true);
        assert_eq!(taken(), ["x"]);
    }

    #[test]
    fn counts_as_failure_matches_the_failure_tally() {
        let copy = CopyError {
            cause: FsError::new(Code::IoError, io("c")),
            leftover: None,
            step: CopyStep::Gate,
        };
        let complaint = MetadataFailure {
            item: MetadataItem::Times,
            error: FsError::new(Code::PermissionDenied, io("no")),
        };
        let planned = PlannedEntry { object: PlannedObject::File, action: PlannedAction::Copy };
        assert!(counts_as_failure(&C::Walk(FsError::new(Code::PermissionDenied, io("w")))));
        assert!(counts_as_failure(&C::CreateDir(FsError::new(Code::IoError, io("d")))));
        assert!(counts_as_failure(&C::Copy(copy)));
        assert!(counts_as_failure(&C::PublishedWithComplaints(vec![complaint])));
        assert!(counts_as_failure(&C::ClaimNotRecorded(FsError::new(Code::IoError, io("s")))));
        assert!(!counts_as_failure(&C::SpecialFileSkipped));
        assert!(!counts_as_failure(&C::SymlinkSkipped));
        assert!(!counts_as_failure(&C::MountSkipped));
        assert!(!counts_as_failure(&C::Planned(planned)));
    }

    fn complaint(item: MetadataItem) -> MetadataFailure {
        MetadataFailure { item, error: FsError::new(Code::PermissionDenied, io("no")) }
    }

    #[test]
    fn for_copy_hands_the_handle_to_the_config_only_when_wanted() {
        for (dry_run, json, wanted) in
            [(false, false, true), (true, false, false), (false, true, false), (true, true, false)]
        {
            let term = InMemoryTerm::new(20, 80);
            let mut slot = None;
            let live = Live::for_copy(dry_run, json, target(&term), plain, &mut slot);
            assert_eq!(slot.is_some(), wanted, "dry_run={dry_run} json={json}");
            assert_eq!(live.progress().is_some(), wanted);
            if let (Some(a), Some(b)) = (&slot, live.progress()) {
                assert!(Arc::ptr_eq(a, &b));
            }
        }
    }

    #[test]
    fn record_failure_counts_one_error_per_record_and_prints_every_line() {
        let term = InMemoryTerm::new(20, 80);
        let live = Live::start(true, target(&term), plain);
        let two = TreeFailure {
            path: "a".into(),
            cause: C::PublishedWithComplaints(vec![
                complaint(MetadataItem::Times),
                complaint(MetadataItem::Permissions),
            ]),
        };
        live.record_failure(&two);
        let seen = term.contents();
        assert_eq!(live.progress().unwrap().snapshot().errors, 1);
        assert_eq!(seen.matches("a:").count(), 2, "{seen}");
        let skip = TreeFailure { path: "b".into(), cause: C::SymlinkSkipped };
        live.record_failure(&skip);
        assert_eq!(live.progress().unwrap().snapshot().errors, 1);
        assert!(term.contents().contains("b: object_type=symlink"), "{}", term.contents());
    }

    #[test]
    fn record_failure_on_the_plain_path_prints_the_record_lines_unchanged() {
        let live = Live::start(false, ProgressDrawTarget::hidden(), plain);
        let f = TreeFailure { path: "c".into(), cause: C::MountSkipped };
        live.record_failure(&f);
        assert_eq!(taken(), crate::report::record_lines(&f));
    }
}
