use std::process::Command;
use tempfile::TempDir;

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

#[test]
fn it_copies_a_single_file() {
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"hello").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&dst).unwrap(), b"hello");

    // §44.1 is the reason this cut exists; assert it against a real filesystem, not
    // only through the fake's call ordering.
    let (sm, dm) = (std::fs::metadata(&src).unwrap(), std::fs::metadata(&dst).unwrap());
    assert_eq!(dm.modified().unwrap(), sm.modified().unwrap(), "mtime must be preserved");
}

#[test]
fn a_successful_copy_consumes_its_temporary() {
    // Named for what it proves. On the happy path the publishing rename consumes the
    // temporary, so this does NOT exercise the failure-path cleanup -- that is
    // `a_temporary_that_cannot_be_removed_is_named_in_the_error` and the several
    // `!fs.exists("/dst.flux-partial.op1")` assertions in `flux-core`, which can
    // inject the failures a real filesystem will not produce on demand.
    let d = TempDir::new().unwrap();
    let (src, dst) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&src, b"hello").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();
    // Without this the test passes even when the binary fails outright, since a run
    // that never starts leaves no temporary either.
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let leftovers: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("flux-partial"))
        .collect();
    assert!(leftovers.is_empty(), "found {leftovers:?}");
}
