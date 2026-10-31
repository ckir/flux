// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! GATE-C (spec 7): the walk's resident memory does not grow with the size of a directory it spills. Its own
//! binary, so the counting allocator is local to it. `#[ignore]`d: it makes 3,000,000 files.
//!
//! Run: `cargo nextest run -p flux-core --run-ignored only --no-fail-fast walk_cascade`

use flux_core::scratch::{ScratchSpace, Thresholds};
use flux_core::walk::{MountPolicy, WalkEvent, WalkOptions, walk_with};
use flux_fs::DestinationRoot;
use flux_platform::StdFileSystem;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let now = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` unchanged and only adjusts counters around the call.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            grow(new);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// A directory under the user's cache directory (a real disk: `/tmp` may be a small `tmpfs`).
fn scratch_tempdir() -> TempDir {
    let home = std::env::var_os("HOME").expect("HOME");
    let base = Path::new(&home).join(".cache");
    std::fs::create_dir_all(&base).unwrap();
    TempDir::new_in(base).unwrap()
}

/// `n` zero-byte files `f0000000..`, created in a permuted order so the walk has to sort them. 7919 is prime and
/// does not divide `n`, so `i * 7919 % n` visits every number once.
fn build(root: &Path, n: u64) {
    for i in 0..n {
        std::fs::File::create(root.join(format!("f{:07}", i * 7919 % n))).unwrap();
    }
}

/// Walk `root` with a space placed on `home`: the most bytes held live above the starting baseline, the count of
/// names, and how far `LIVE` is from the baseline once the walk and the space are gone. Every name is checked
/// against the sorted sequence as it arrives (the zero-padded names sort numerically), so nothing is retained.
fn walk_peak(root: &Path, home: &Path, thresholds: Thresholds, n: u64) -> (usize, usize) {
    let fs = StdFileSystem;
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    {
        let space = ScratchSpace::unplaced(thresholds, Some(home.to_path_buf()));
        space.place(fs.destination_root(home).unwrap(), home.to_path_buf());
        let mut walk = walk_with(
            &fs,
            root,
            WalkOptions { max_depth: 8, mounts: MountPolicy::Cross },
            Some(&space),
        )
        .unwrap();
        let mut seen = 0u64;
        for event in walk.by_ref() {
            let WalkEvent::File { path } = event.unwrap() else { panic!("unexpected event") };
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            assert_eq!(name, format!("f{seen:07}"), "entry {seen} out of order");
            seen += 1;
        }
        assert_eq!(seen, n);
        assert_eq!(walk.spilled_directories(), 1);
        drop(walk);
        space.close();
    }
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    let leaked = LIVE.load(Ordering::Relaxed).abs_diff(baseline);
    (peak, leaked)
}

#[test]
#[ignore = "makes 3,000,000 files; run explicitly (spec 7, GATE-C)"]
fn walk_cascade_memory_does_not_grow_with_the_directory() {
    // 512 inline, runs of 4,096 (488 runs at 2M), fan-in 8: two cascade levels at 2M.
    let t = Thresholds::new(512, 4_096, 8, 16_384, 65_536).unwrap();
    let (src_1m, src_2m, home) = (scratch_tempdir(), scratch_tempdir(), scratch_tempdir());
    build(src_1m.path(), 1_000_000);
    build(src_2m.path(), 2_000_000);

    // Warm the process once (lazy statics, thread-locals) so the first measured walk is not charged for them.
    let warm = scratch_tempdir();
    build(warm.path(), 1_000);
    let _ = walk_peak(warm.path(), home.path(), t, 1_000);

    let (peak_1m, leaked_1m) = walk_peak(src_1m.path(), home.path(), t, 1_000_000);
    let (peak_2m, leaked_2m) = walk_peak(src_2m.path(), home.path(), t, 2_000_000);
    eprintln!(
        "GATE-C peak_1m={peak_1m} B peak_2m={peak_2m} B leaked_1m={leaked_1m} B leaked_2m={leaked_2m} B"
    );

    const MIB: usize = 1024 * 1024;
    assert!(
        peak_2m <= peak_1m + MIB,
        "peak grew with the directory: 1M {peak_1m} B, 2M {peak_2m} B"
    );
    // A leak on drop does not move the peak: the live bytes must be back where they started.
    assert!(leaked_1m <= 64 * 1024, "{leaked_1m} B still live after the 1M walk");
    assert!(leaked_2m <= 64 * 1024, "{leaked_2m} B still live after the 2M walk");
}
