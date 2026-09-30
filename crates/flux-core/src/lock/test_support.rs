//! Scaffolding the protocol tests share: a fake filesystem with a directory `/p`, records, and simulated owners.

use super::error::{LockError, LockResult, Refusal};
use super::record::LockRecord;
use super::site::LockSite;
use crate::fault_fs::{FakeDirHandle, FakeLock, FaultFs};
use flux_fs::{DestinationRoot, DirHandle, FileSystem, LockFile};
use std::ffi::OsStr;
use std::path::Path;

/// A fake filesystem and a handle on its directory `/p`, which holds every test's lock.
pub(crate) fn fake() -> (FaultFs, FakeDirHandle) {
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/p")).unwrap();
    let d = fs.destination_root(Path::new("/p")).unwrap();
    (fs, d)
}

/// A record for `site` naming `operation_id` and `workspace_path`.
pub(crate) fn record(
    site: &LockSite<'_, FakeDirHandle>,
    operation_id: &str,
    workspace_path: &str,
) -> LockRecord {
    LockRecord {
        complete_lock_key: site.complete_lock_key().unwrap(),
        operation_id: operation_id.to_string(),
        owner_instance_id: crate::ids::new_id(),
        boot_session_id: "test-boot".to_string(),
        target_path_key: site.target_path_key(),
        workspace_path: workspace_path.to_string(),
        creation_wall_time: 1,
        last_heartbeat_wall_time: 1,
    }
}

/// A lock whose owner wrote `bytes` and then died: the file stays, and nobody holds its OS-native lock.
pub(crate) fn dead_lock(d: &FakeDirHandle, name: &str, bytes: &[u8]) {
    let lock = d.create_lock(OsStr::new(name)).unwrap();
    assert!(lock.try_lock().unwrap());
    lock.write_at_start(bytes).unwrap();
}

/// A live owner: the returned handle holds the OS-native lock on a file holding `bytes` until it is dropped.
pub(crate) fn live_lock(d: &FakeDirHandle, name: &str, bytes: &[u8]) -> FakeLock {
    let lock = d.create_lock(OsStr::new(name)).unwrap();
    assert!(lock.try_lock().unwrap());
    lock.write_at_start(bytes).unwrap();
    lock
}

/// The refusal a protocol call returned; panics on success or an I/O error.
pub(crate) fn refusal<T>(r: LockResult<T>) -> Refusal {
    match r {
        Err(LockError::Refused(x)) => *x,
        Err(LockError::Io(e)) => panic!("expected a refusal, got an I/O error: {e:?}"),
        Ok(_) => panic!("expected a refusal, got success"),
    }
}
