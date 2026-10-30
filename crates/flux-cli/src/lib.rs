// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The logic behind `flux copy` (cut 5), in a library so each part is unit-tested and
//! lands before `main.rs` uses it: `exit_code` (§55), `report` (what is printed) and
//! `resolve` (the two command-line paths to what the engine is asked to do).

pub mod cleanup_report;
pub mod exit_code;
pub mod live;
pub mod logging;
pub mod report;
pub mod resolve;
pub mod verbosity;
pub mod verify_report;
