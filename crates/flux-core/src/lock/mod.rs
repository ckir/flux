//! The destination lock (§96.1, §240, §259.6): the record codec, and the protocol that the TLA+ model in
//! `models/lockproto/` checks. Each protocol function names the model labels it implements; `impl-map.toml` there
//! maps every label to its function.

mod acquire;
mod classify;
pub mod error;
mod held;
mod obtain;
pub mod record;
mod recover;
pub mod site;
mod takeover;
#[cfg(test)]
pub(crate) mod test_support;

pub use error::{LockCode, LockError, LockResult, Refusal};
pub use held::{Held, Released};
pub use obtain::{
    CleanupObtained, MAX_ATTEMPTS, Mode, Obtained, check_capability, obtain, obtain_cleanup_lock,
};
pub use site::{LockSite, SiteKind};
pub use takeover::{Claimed, Overwritten};
