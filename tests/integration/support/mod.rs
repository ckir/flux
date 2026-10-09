//! Shared helpers: run the engine against the real filesystem, and read a tree back as a comparable map.

use flux_core::TreeOutcome;
use flux_core::run::{self, RunConfig};
use flux_fs::{CopyOptions, Durability, ExistingPolicy, OperationId, Preserve, Publish, Safety};
use std::collections::BTreeMap;
use std::path::Path;

pub fn options(durability: Durability) -> CopyOptions {
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

pub fn config() -> RunConfig {
    RunConfig {
        restart: false,
        break_lock: false,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: None,
        heartbeat_interval: run::HEARTBEAT_INTERVAL,
        resume: false,
    }
}

/// Copy `src` to `dst` as the CLI would, and require a clean run.
pub fn copy_tree(src: &Path, dst: &Path) -> TreeOutcome {
    let fs = flux_platform::StdFileSystem;
    let run =
        run::tree(&fs, src, dst, &options(Durability::Normal), &config(), &mut |f| panic!("{f:?}"));
    assert!(run.stop.is_none(), "run stopped: {:?}", run.stop);
    run.copy.expect("the copy ran").unwrap_or_else(|a| panic!("tree aborted: {}", a.error))
}

pub fn copy_file(src: &Path, dst: &Path) {
    let fs = flux_platform::StdFileSystem;
    let run = run::file(&fs, src, dst, &options(Durability::Normal), &config());
    assert!(run.stop.is_none(), "run stopped: {:?}", run.stop);
    run.copy.expect("the copy ran").unwrap_or_else(|e| panic!("file copy failed: {e}"));
}

/// Every regular file under `root`, keyed by its `/`-joined relative path, valued by its bytes' BLAKE3 hash.
/// Directories are keyed with a trailing `/` and an empty hash so an empty directory is part of the comparison.
pub fn tree_bytes(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if e.file_type().unwrap().is_dir() {
                out.insert(format!("{rel}/"), String::new());
                walk(&e.path(), &rel, out);
            } else {
                let h = blake3::hash(&std::fs::read(e.path()).unwrap());
                out.insert(rel, h.to_hex().to_string());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, "", &mut out);
    out
}
