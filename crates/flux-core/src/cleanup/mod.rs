//! `flux cleanup` (cut 9b): removes what finished or dead operations left behind.

use crate::state::OpState;
use flux_fs::{DestinationRoot, FsError};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod artifacts;
pub(crate) mod delete;
pub mod discover;
pub mod status;

/// How long a finished operation's state is kept before cleanup may remove it: seven days.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How stale a lock record's heartbeat must be before cleanup treats its owner as gone: 30 s.
pub const LEASE_THRESHOLD: Duration = Duration::from_secs(30);

/// What the operator asked of the cleanup, beyond what it finds.
#[derive(Clone)]
pub struct CleanupConfig {
    pub retention: Duration,
    pub lease_threshold: Duration,
    /// Nanoseconds since the Unix epoch, UTC (`state::wall_time_ns()` in production; tests pin it).
    pub now: u64,
    pub force: bool,
    pub dry_run: bool,
    pub operation_id: String,
    pub owner_instance_id: String,
    pub boot_session_id: String,
    pub before_mutation: Option<crate::run::BeforeMutation>,
    pub heartbeat_interval: Duration,
}

/// The six names of section 251.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Live,
    Resumable,
    Stale,
    CompletedButUnclean,
    Uncertain,
    Corrupt,
}

impl Status {
    /// The spelling the report prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Resumable => "RESUMABLE",
            Self::Stale => "STALE",
            Self::CompletedButUnclean => "COMPLETED_BUT_UNCLEAN",
            Self::Uncertain => "UNCERTAIN",
            Self::Corrupt => "CORRUPT",
        }
    }
}

/// What kind of thing an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Operation,
    Debris,
    RootLock,
}

impl EntryKind {
    /// The spelling the report prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::Debris => "debris",
            Self::RootLock => "root-lock",
        }
    }
}

/// One row of the cleanup report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The operation id; `<id>.creating` / `<id>.removing`; or the lock file's name (`<name>.flux-lock`, `.flux-root.lock`, `<lock>.broken.<id>`).
    pub id: String,
    pub status: Status,
    pub eligible: bool,
    pub state: Option<OpState>,
    pub age_seconds: Option<u64>,
    pub note: String,
}

/// Exit 3: nothing was examined.
#[derive(Debug)]
pub struct Refused {
    pub code: &'static str,
    pub detail: String,
}

/// One thing the deletion pass did, or declined to do.
#[derive(Debug)]
pub enum Action {
    Removed(PathBuf),
    Kept { path: PathBuf, error: FsError },
    Skipped { id: String, reason: String },
}

/// What `flux cleanup` found and did.
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub entries: Vec<Entry>,
    pub actions: Vec<Action>,
    pub listing_failed: Option<(PathBuf, FsError)>,
    /// The pass lost the lock (`Fault`) at this path: exit 1.
    pub lost: Option<(PathBuf, FsError)>,
}

/// The summary line's counts. `entries`, `eligible` and `skipped` count rows; `removed` and `kept` count action lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub entries: u64,
    pub eligible: u64,
    pub removed: u64,
    pub kept: u64,
    pub skipped: u64,
}

impl CleanupReport {
    pub fn summary(&self) -> Summary {
        let count =
            |f: &dyn Fn(&Action) -> bool| self.actions.iter().filter(|a| f(a)).count() as u64;
        Summary {
            entries: self.entries.len() as u64,
            eligible: self.entries.iter().filter(|e| e.eligible).count() as u64,
            removed: count(&|a| matches!(a, Action::Removed(_))),
            kept: count(&|a| matches!(a, Action::Kept { .. })),
            skipped: count(&|a| matches!(a, Action::Skipped { .. })),
        }
    }

    /// Exit 1 (decision 10): a deletion that was attempted failed, a directory could not be listed, or the lock was lost.
    pub fn failed(&self) -> bool {
        self.listing_failed.is_some()
            || self.lost.is_some()
            || self.actions.iter().any(|a| matches!(a, Action::Kept { .. }))
    }
}

/// `flux cleanup DEST`: discover, classify, and unless `cfg.dry_run` run one deletion pass. `Err` is exit 3.
pub fn cleanup<F: DestinationRoot>(
    fs: &F,
    dst_root: &Path,
    cfg: &CleanupConfig,
) -> Result<CleanupReport, Refused> {
    let mut discovered = discover::discover(fs, dst_root, cfg)?;
    let mut report = CleanupReport {
        entries: discovered.entries.iter().map(|(entry, _)| entry.clone()).collect(),
        listing_failed: discovered.listing_failed.take(),
        ..CleanupReport::default()
    };
    if !cfg.dry_run {
        delete::run_pass(fs, &discovered, cfg, &mut report);
    }
    Ok(report)
}
