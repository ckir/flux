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

#[test]
fn a_copy_onto_a_hardlink_of_its_own_source_is_refused() {
    // The live defect 4a fixes: two names, one object. Before the gate, the file was
    // copied onto itself.
    let d = TempDir::new().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::write(&a, b"hello").unwrap();
    std::fs::hard_link(&a, &b).unwrap();

    let out = flux().arg("copy").arg(&a).arg(&b).output().unwrap();

    assert!(!out.status.success(), "must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SAFETY_REJECTED"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&b).unwrap(), b"hello");
    assert_eq!(std::fs::read(&a).unwrap(), b"hello");
}

#[test]
fn a_copy_onto_its_own_source_by_another_spelling_is_refused() {
    // `..` is not normalized away by Path comparison, so the two spellings differ
    // lexically and only identity can tell they are one file.
    let d = TempDir::new().unwrap();
    std::fs::create_dir(d.path().join("sub")).unwrap();
    let a = d.path().join("a");
    let same = d.path().join("sub").join("..").join("a");
    std::fs::write(&a, b"hello").unwrap();
    assert_ne!(a, same, "the fixture must be lexically different");

    let out = flux().arg("copy").arg(&a).arg(&same).output().unwrap();

    assert!(!out.status.success(), "must be refused");
    assert!(String::from_utf8_lossy(&out.stderr).contains("SAFETY_REJECTED"));
    assert_eq!(std::fs::read(&a).unwrap(), b"hello");
}

#[test]
fn a_copy_between_bare_names_writes_into_the_working_directory() {
    // The one real-filesystem path through `split_destination`'s "empty parent
    // becomes ." rule. `current_dir` sets the CHILD's working directory only, so this
    // is safe under parallel tests.
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();

    let out = flux().current_dir(d.path()).arg("copy").arg("a").arg("b").output().unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(d.path().join("b")).unwrap(), b"hello");
}
