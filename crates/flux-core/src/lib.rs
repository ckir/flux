//! Flux engine (spec §3.1).

// Test-only. Nothing outside this crate uses the fake, so it is gated on `test`
// rather than on a `testing` feature that nothing would ever turn on -- and an
// undeclared feature in a `cfg` is a warning the repo's clippy gate would surface.
#[cfg(test)]
pub mod fault_fs;
