//! `flux cleanup` (cut 9b): removes what finished or dead operations left behind.

use crate::state::OpState;
use std::time::Duration;

pub mod artifacts;
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
