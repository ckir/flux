// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

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
// The refusal constructors are crate-internal, so they cannot ride the `pub use` above.
#[allow(unused_imports)] // only `busy_refusal` still has no user outside `obtain`.
pub(crate) use obtain::{
    UNTRUSTED_DETAIL, busy_refusal, foreign_refusal, orphan_refusal, uncertain_refusal,
    untrusted_refusal,
};
pub use site::{LockSite, SiteKind, prefix_lock_fits};
pub(crate) use takeover::takeover_unavailable_refusal;
pub use takeover::{Claimed, Overwritten};
