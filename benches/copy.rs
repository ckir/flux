//! The copy pipeline, in process: `flux_core::run::tree` / `run::file` on the real filesystem, each under
//! `Durability::Normal` and `Durability::Strict`. Cut 9d adds fsync work to the Strict path, so the Normal/Strict
//! pair is the number to compare before and after.
//!
//! WHERE THE DATA GOES decides the result. `FLUX_BENCH_DIR` names the directory (default `target/bench-data`, on
//! the repository's disk). A tmpfs/ramfs directory is refused: fsync costs nothing there and the Strict cost this
//! file exists to show would vanish. Do not point it at `/tmp` on a box where that is tmpfs.
//!
//! Each iteration copies into a FRESH destination (`iter_batched`), so skip-existing and identity checks never
//! short-circuit the copy. Sources are built once, outside the timed region. Sample counts are small on purpose:
//! a fsync-bound run takes seconds, and criterion's defaults would take minutes per group.
//!
//! The headline wall times against cp/rsync (cache dropped, one process per run) come from `benches/publish`, not
//! from here. A number from this file is within-run only: compare Normal against Strict in the same invocation.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use flux_core::run::{self, RunConfig};
use flux_fs::{CopyOptions, Durability, ExistingPolicy, OperationId, Preserve, Publish, Safety};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MIB: usize = 1 << 20;

fn options(durability: Durability) -> CopyOptions {
    CopyOptions {
        preserve_times: Preserve::Default,
        preserve_permissions: Preserve::Default,
        durability,
        publish: Publish::Replace,
        safety: Safety::Default,
        operation_id: OperationId::new(String::new()),
        existing: ExistingPolicy::Overwrite,
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
    }
}

/// The data directory, created and checked: refuse a RAM-backed filesystem.
fn data_dir() -> PathBuf {
    let dir = std::env::var_os("FLUX_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-data"));
    std::fs::create_dir_all(&dir).expect("create the bench data directory");
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("stat")
            .args(["-f", "-c", "%T"])
            .arg(&dir)
            .output()
            .expect("run `stat -f` to learn the filesystem type");
        let kind = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        assert!(
            kind != "tmpfs" && kind != "ramfs",
            "{} is on {kind}: fsync is free there and Strict would look as cheap as Normal. Set FLUX_BENCH_DIR to a real disk.",
            dir.display()
        );
        eprintln!("bench data: {} ({kind})", dir.display());
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("bench data: {} (filesystem type not checked on this OS)", dir.display());
    dir
}

/// Fixed pseudo-random bytes: incompressible, so nothing downstream shortcuts on content.
fn fill(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn durabilities() -> [(&'static str, Durability); 2] {
    [("normal", Durability::Normal), ("strict", Durability::Strict)]
}

fn group<'a>(
    c: &'a mut Criterion,
    name: &str,
    bytes: u64,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(10)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(5));
    g.throughput(Throughput::Bytes(bytes));
    g
}

fn single_file(c: &mut Criterion, name: &str, len: usize) {
    let root = data_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let src = root.join("src.bin");
    std::fs::write(&src, fill(len, 7)).unwrap();
    let fs = flux_platform::StdFileSystem;
    let mut g = group(c, name, len as u64);
    for (label, durability) in durabilities() {
        let opts = options(durability);
        let mut n = 0u64;
        g.bench_function(label, |b| {
            b.iter_batched(
                || {
                    n += 1;
                    root.join(format!("dst-{label}-{n}.bin"))
                },
                |dst| {
                    let run = run::file(&fs, &src, &dst, &opts, &config());
                    run.copy.expect("the copy ran").expect("the copy succeeded");
                    dst
                },
                BatchSize::PerIteration,
            )
        });
    }
    g.finish();
    let _ = std::fs::remove_dir_all(&root);
}

fn small_files(c: &mut Criterion) {
    let (files, each) = (1000usize, 4096usize);
    let root = data_dir().join("small_1000x4kib");
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("src");
    for i in 0..files {
        let dir = src.join(format!("d{:02}", i % 20));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("f{i:04}")), fill(each, i as u64 + 1)).unwrap();
    }
    let fs = flux_platform::StdFileSystem;
    let mut g = group(c, "small_1000x4kib", (files * each) as u64);
    for (label, durability) in durabilities() {
        let opts = options(durability);
        let mut n = 0u64;
        g.bench_function(label, |b| {
            b.iter_batched(
                || {
                    n += 1;
                    root.join(format!("dst-{label}-{n}"))
                },
                |dst| {
                    let run =
                        run::tree(&fs, &src, &dst, &opts, &config(), &mut |f| panic!("{f:?}"));
                    run.copy.expect("the copy ran").unwrap_or_else(|a| panic!("{}", a.error));
                    dst
                },
                BatchSize::PerIteration,
            )
        });
    }
    g.finish();
    let _ = std::fs::remove_dir_all(&root);
}

fn single_1mib(c: &mut Criterion) {
    single_file(c, "single_1mib", MIB);
}

fn large_64mib(c: &mut Criterion) {
    single_file(c, "large_64mib", 64 * MIB);
}

criterion_group!(benches, single_1mib, small_files, large_64mib);
criterion_main!(benches);
