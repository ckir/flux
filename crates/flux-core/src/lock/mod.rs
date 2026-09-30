//! The destination lock (§96.1, §240, §259.6): the record codec, and the protocol that the TLA+ model in
//! `models/lockproto/` checks. Each protocol function names the model labels it implements; `impl-map.toml` there
//! maps every label to its function.

// Until Task 7 of the Part 2 plan adds `obtain`, the crate-internal protocol steps (and the test scaffolding's later
// helpers) are not all reached yet. Task 7 deletes this line, and its `just check` then proves nothing is unused.
#![allow(dead_code)]

mod acquire;
mod classify;
pub mod error;
mod held;
pub mod record;
mod recover;
pub mod site;
#[cfg(test)]
pub(crate) mod test_support;

pub use error::{LockCode, LockError, LockResult, Refusal};
pub use held::{Held, Released};
pub use site::{LockSite, SiteKind};
