// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Shared progress counters: the engine bumps these as it works and a
//! display samples them to render live progress.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A bundle of relaxed atomic counters.
#[derive(Debug, Default)]
pub struct Progress {
    discovered: AtomicU64,
    copied: AtomicU64,
    resumed: AtomicU64,
    skipped: AtomicU64,
    errors: AtomicU64,
    bytes: AtomicU64,
    expected_bytes: AtomicU64,
}

/// A point-in-time copy of the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub discovered: u64,
    pub copied: u64,
    pub resumed: u64,
    pub skipped: u64,
    pub errors: u64,
    pub bytes: u64,
    /// `None` when never set or set to 0.
    pub expected_bytes: Option<u64>,
}

impl Progress {
    /// A fresh, all-zero, shareable counter bundle.
    #[must_use]
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress::default())
    }

    pub fn add_discovered(&self) {
        self.discovered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_copied(&self) {
        self.copied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_resumed(&self) {
        self.resumed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_skipped(&self) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Store the expected total size; 0 means "unknown".
    pub fn set_expected_bytes(&self, n: u64) {
        self.expected_bytes.store(n, Ordering::Relaxed);
    }

    /// Load each counter once.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let expected = self.expected_bytes.load(Ordering::Relaxed);
        Snapshot {
            discovered: self.discovered.load(Ordering::Relaxed),
            copied: self.copied.load(Ordering::Relaxed),
            resumed: self.resumed.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            expected_bytes: if expected == 0 { None } else { Some(expected) },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_progress_is_all_zero() {
        assert_eq!(Progress::new().snapshot(), Snapshot::default());
    }

    #[test]
    fn each_add_moves_only_its_own_field() {
        type Act = fn(&Progress);
        let cases: [(&str, Act, Snapshot); 7] = [
            (
                "discovered",
                |p| p.add_discovered(),
                Snapshot { discovered: 1, ..Snapshot::default() },
            ),
            ("copied", |p| p.add_copied(), Snapshot { copied: 1, ..Snapshot::default() }),
            ("resumed", |p| p.add_resumed(), Snapshot { resumed: 1, ..Snapshot::default() }),
            ("skipped", |p| p.add_skipped(), Snapshot { skipped: 1, ..Snapshot::default() }),
            ("errors", |p| p.add_error(), Snapshot { errors: 1, ..Snapshot::default() }),
            ("bytes", |p| p.add_bytes(7), Snapshot { bytes: 7, ..Snapshot::default() }),
            (
                "expected_bytes",
                |p| p.set_expected_bytes(9),
                Snapshot { expected_bytes: Some(9), ..Snapshot::default() },
            ),
        ];
        for (name, act, expected) in cases {
            let p = Progress::new();
            act(&p);
            assert_eq!(p.snapshot(), expected, "{name}");
        }
    }

    #[test]
    fn add_bytes_accumulates() {
        let p = Progress::new();
        p.add_bytes(3);
        p.add_bytes(4);
        assert_eq!(p.snapshot().bytes, 7);
    }

    #[test]
    fn a_zero_expected_size_is_unknown() {
        let p = Progress::new();
        p.set_expected_bytes(0);
        assert_eq!(p.snapshot().expected_bytes, None);
        p.set_expected_bytes(9);
        assert_eq!(p.snapshot().expected_bytes, Some(9));
    }

    #[test]
    fn counters_are_shared_across_threads() {
        let p = Progress::new();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let p = Arc::clone(&p);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        p.add_discovered();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(p.snapshot().discovered, 4000);
    }
}
