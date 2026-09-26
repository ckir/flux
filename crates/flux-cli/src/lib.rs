//! The logic behind `flux copy` (cut 5), in a library so each part is unit-tested and
//! lands before `main.rs` uses it: `exit_code` (§55), `report` (what is printed) and
//! `resolve` (the two command-line paths to what the engine is asked to do).

pub mod exit_code;
pub mod report;
