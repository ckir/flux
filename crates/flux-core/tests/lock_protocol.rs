//! The lock protocol on a real filesystem, with a real OS-native lock held by ANOTHER PROCESS: while it lives, the
//! target is BUSY and its holder is reported; once it dies, the next run recovers its lock (§240.2, §240.3).

use flux_core::lock::record::LockRecord;
use flux_core::lock::{LockCode, LockError, LockSite, Mode, Obtained, check_capability, obtain};
use flux_fs::DestinationRoot;
use flux_platform::StdFileSystem;
use std::ffi::OsStr;
use std::io::BufRead;
use std::path::Path;

const CHILD_MARKER: &str = "FLUX_CHILD_HOLDS_TARGET_LOCK";

/// Run as a child: obtain the lock on `<dir>/dest`, create its workspace, write the record, report the operation id,
/// and hold the lock until killed.
fn hold_as_child(dir: &str) {
    let parent = StdFileSystem.destination_root(Path::new(dir)).expect("child opens the directory");
    let site = LockSite::directory(&parent, OsStr::new("dest")).unwrap();
    let capability = check_capability(&parent).unwrap();
    let id = flux_core::ids::new_id();
    let Obtained::Held { mut held, .. } = obtain(&site, capability, Mode::Plain, &id).unwrap()
    else {
        panic!("child: expected a held lock")
    };
    std::fs::create_dir_all(Path::new(dir).join("dest").join(".flux").join("operations").join(&id))
        .unwrap();
    held.write_record(LockRecord {
        complete_lock_key: site.complete_lock_key().unwrap(),
        operation_id: id.clone(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        target_path_key: site.target_path_key(),
        workspace_path: format!("operations/{id}"),
        creation_wall_time: 1,
        last_heartbeat_wall_time: 1,
    })
    .unwrap();
    // A line of its own: libtest has already printed `test <name> ... ` without a newline.
    println!("\n{CHILD_MARKER} {id}");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn a_live_owner_in_another_process_is_busy_and_its_death_lets_the_next_run_recover() {
    if let Ok(dir) = std::env::var("FLUX_LOCK_PROTOCOL_CHILD_DIR") {
        hold_as_child(&dir);
        return;
    }
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_live_owner_in_another_process_is_busy_and_its_death_lets_the_next_run_recover",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FLUX_LOCK_PROTOCOL_CHILD_DIR", tmp.path())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Declared after `tmp`, so it drops first: the child is gone before the directory is removed.
    let mut child = KillOnDrop(child);
    let stdout = child.0.stdout.take().unwrap();
    let child_id = std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .find_map(|l| l.trim().strip_prefix(&format!("{CHILD_MARKER} ")).map(str::to_string))
        .expect("the child reports that it holds the lock");

    let parent = StdFileSystem.destination_root(tmp.path()).unwrap();
    let site = LockSite::directory(&parent, OsStr::new("dest")).unwrap();
    let capability = check_capability(&parent).unwrap();
    match obtain(&site, capability, Mode::Plain, &flux_core::ids::new_id()) {
        Err(LockError::Refused(r)) => {
            assert_eq!(r.code, LockCode::TargetLockBusy);
            assert_eq!(
                r.holder.map(|h| h.operation_id),
                Some(child_id),
                "the refusal reports the holder"
            );
        }
        Err(LockError::Io(e)) => panic!("{e:?}"),
        Ok(_) => panic!("a live owner in another process must make the target busy"),
    }

    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let held = loop {
        match obtain(&site, capability, Mode::Plain, &flux_core::ids::new_id()) {
            Ok(Obtained::Held { held, leftover_broken }) => {
                assert_eq!(leftover_broken, None);
                break held;
            }
            Ok(Obtained::Claimed(_)) => panic!("a plain run never takes over"),
            Err(LockError::Refused(r)) if r.code == LockCode::TargetLockBusy => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "a dead process's lock was never released"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("{e:?}"),
        }
    };
    assert!(
        held.record().is_none(),
        "recovered: an empty held lock, awaiting this run's state (F5)"
    );
    let broken = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".broken."))
        .count();
    assert_eq!(broken, 0, "the moved-aside lock was deleted");
}
