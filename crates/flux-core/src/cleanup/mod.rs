//! `flux cleanup` (cut 9b): removes what finished or dead operations left behind.

use std::time::Duration;

pub mod artifacts;

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
