// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Cut 11a: a dry run holds no per-entry state. Its own test binary, so the counting allocator is local to it.

use flux_core::run::{self, RunConfig};
use flux_fs::{
    CopyOptions, Durability, ExistingPolicy, Links, OperationId, Preserve, Publish, Safety,
};
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

/// The most bytes `f` held live above what was live when it started.
fn peak_of(f: impl FnOnce()) -> usize {
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    f();
    PEAK.load(Ordering::Relaxed).saturating_sub(baseline)
}

fn opts() -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability: Durability::Normal,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new("t"),
        existing: ExistingPolicy::Overwrite,
        links: Links::Copy,
        hardlinks: flux_fs::Hardlinks::Auto,
        cross_filesystems: false,
        verify: flux_fs::VerifyLevel::None,
        hash: flux_fs::HashAlgorithm::Blake3,
        selection: flux_fs::Selection::default(),
    }
}

fn config() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: None,
        heartbeat_interval: run::HEARTBEAT_INTERVAL,
        resume: false,
        descriptor_limit: None,
        resume_verify: run::ResumeVerify::Chunks,
        dry_run: true,
        dry_temp_root: None,
        progress: None,
    }
}

fn build(root: &Path, dirs: usize, files: usize) {
    for d in 0..dirs {
        let dir = root.join(format!("d{d:03}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..files {
            std::fs::write(dir.join(format!("f{f:03}.txt")), b"x").unwrap();
        }
    }
}

/// A dry run of `src` into an absent DEST under `tmp`; the report callback only counts.
fn dry_peak(src: &Path, tmp: &Path) -> (usize, u64) {
    let dst = tmp.join("dst");
    let cfg = config();
    let o = opts();
    let mut count = 0u64;
    let peak = peak_of(|| {
        let r = run::tree(&StdFileSystem, src, &dst, &o, &cfg, &mut |_| count += 1);
        assert!(matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy.map(|c| c.is_ok()));
        assert!(r.dry.is_some());
    });
    assert!(!dst.exists(), "the dry run created DEST");
    (peak, count)
}

#[test]
fn a_dry_runs_resident_memory_does_not_grow_with_the_entry_count() {
    let (a, b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (src_a, src_b) = (a.path().join("src"), b.path().join("src"));
    build(&src_a, 10, 100);
    build(&src_b, 100, 100);

    // Warm the process once (lazy statics, thread-locals) so the first measured run is not charged for them.
    let w = TempDir::new().unwrap();
    build(&w.path().join("src"), 1, 1);
    dry_peak(&w.path().join("src"), w.path());

    let (peak_a, count_a) = dry_peak(&src_a, a.path());
    let (peak_b, count_b) = dry_peak(&src_b, b.path());

    assert!(count_a >= 1_000 && count_b >= 10_000, "{count_a} {count_b}: every entry is reported");
    assert!(peak_b < 2 * peak_a, "{peak_a} {peak_b}");
}
