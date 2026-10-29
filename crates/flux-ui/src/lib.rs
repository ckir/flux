// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Progress and terminal presentation (spec §3.5).
//!
//! Scaffold only — no implementation yet.

mod display;
mod render;

pub use display::Display;
pub use indicatif::ProgressDrawTarget;
pub use render::render;
