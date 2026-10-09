//! Commit recovery at `--resume` (cut 9c, spec "Recovery at `--resume`"). A Strict tree publication leaves a prepared
//! note in the claim store before its rename and turns it into the claim after it; a crash in between leaves the note.
//! Adoption classifies every note from the evidence at its two names (phase 1, read-only), then applies every verdict
//! in ONE claim-store transaction (phase 2), or refuses with COMMIT_STATE_UNCERTAIN having changed nothing.

use super::RunWarning;
use crate::cleanup::artifacts::remove_validated;
use crate::state::{from_native_hex, parse_identity};
use flux_fs::{
    ClaimKey, ClaimStore, Code, DirHandle, FileIdentity, FileType, FsError, PreparedRecord,
    RecoveryOp, check_component,
};
use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// The evidence that kept a note undecided (spec: the four classes the refusal names).
pub(crate) const IDENTITY_UNAVAILABLE: &str = "identity unavailable";
pub(crate) const ANOTHER_OBJECT: &str = "destination holds another object";
pub(crate) const UNREADABLE_DIRECTORY: &str = "unreadable directory";
pub(crate) const INVALID_NOTE: &str = "invalid note";

/// The refusal names at most this many undecided targets, then the total.
const NAMED: usize = 10;

/// One note's verdict (the spec's matrix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Row 1: the rename happened. `remove_temp`: the temporary's name still holds the same object (the
    /// `link()`+`unlink()` fallback), so it is removed after the transaction.
    Renamed { remove_temp: bool },
    /// Rows 2, 3, 3b: the rename did not happen.
    NotRenamed,
    /// Row 4: no object of the operation's exists at either name.
    Gone,
    /// Row 5. The string is the evidence class.
    Uncertain(String),
}

/// What phase 2 did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Recovered {
    /// RENAMED notes, whose claims are now `Created`.
    pub recovered: u64,
}

#[derive(Debug)]
pub(crate) enum RecoverError {
    /// At least one note is UNCERTAIN: nothing was applied. Each undecided target as DEST-joined path, with its
    /// evidence class, in the store's note order.
    Uncertain(Vec<(PathBuf, String)>),
    /// The claim store failed (`prepared` or the phase-2 transaction).
    Store(FsError),
}

/// A name's state in the note's directory.
type Seen = Option<FileIdentity>;

fn uncertain(why: &str) -> Verdict {
    Verdict::Uncertain(why.to_string())
}

/// Stored name bytes as a name. On Windows only UTF-8 is accepted: anything else would need an unchecked conversion of
/// bytes read from disk, so such a note is invalid.
fn name_of(bytes: &[u8]) -> Option<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(OsStr::from_bytes(bytes).to_os_string())
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(OsString::from)
    }
}

/// A single normal path component, or `None`.
fn component(bytes: &[u8]) -> Option<OsString> {
    let name = name_of(bytes)?;
    check_component(&name).ok()?;
    Some(name)
}

/// What phase 1 needs from a valid note: its directory relative to DEST, the entry, the temporary, and R.
struct Valid {
    dir: PathBuf,
    name: OsString,
    temp: OsString,
    recorded: FileIdentity,
}

/// Spec decision 5's validation, before any filesystem access.
fn validate(operation_id: &str, key: &ClaimKey, note: &PreparedRecord) -> Option<Valid> {
    // The key is what phase 2 commits; the evidence must be about that entry.
    if key.name != note.name {
        return None;
    }
    let name = component(&note.name)?;
    if !note.planned_name.is_empty() {
        component(&note.planned_name)?;
    }
    let used = if note.planned_name.is_empty() { &note.name } else { &note.planned_name };
    let mut expected = used.clone();
    expected.extend_from_slice(b".flux-partial.");
    expected.extend_from_slice(operation_id.as_bytes());
    if note.temp_name != expected {
        return None;
    }
    let temp = component(&note.temp_name)?;
    let dir = if note.dir_path.is_empty() {
        PathBuf::new()
    } else {
        let dir = from_native_hex(&note.dir_path)?;
        if !dir.components().all(|c| matches!(c, Component::Normal(_))) {
            return None;
        }
        dir
    };
    let recorded = parse_identity(&note.identity)?;
    Some(Valid { dir, name, temp, recorded })
}

/// A missing component, or one that is not a directory: both real arms and the fake answer `DestinationError` (a link
/// is `SafetyRejected`).
fn missing(e: &FsError) -> bool {
    e.code == Code::DestinationError || e.source.kind() == ErrorKind::NotFound
}

/// Phase 1 for one note. Read-only. `dest` is DEST's handle.
pub(crate) fn classify<D: DirHandle>(
    dest: &D,
    operation_id: &str,
    key: &ClaimKey,
    note: &PreparedRecord,
) -> Verdict {
    let Some(v) = validate(operation_id, key, note) else {
        return uncertain(INVALID_NOTE);
    };
    // Component by component through link-refusing handles (the `artifacts::remove_validated` pattern).
    let mut held: Option<D> = None;
    for part in v.dir.iter() {
        match held.as_ref().unwrap_or(dest).open_dir(part) {
            Ok(next) => held = Some(next),
            Err(e) if missing(&e) => return decide(None, None, v.recorded, note.replacement),
            Err(_) => return uncertain(UNREADABLE_DIRECTORY),
        }
    }
    let dir = held.as_ref().unwrap_or(dest);
    match dir.identity() {
        Ok(FileIdentity::Strong(o)) if o == key.parent => {}
        // Another directory is at the recorded path: that is an object, not a missing identity.
        Ok(FileIdentity::Strong(_)) => return uncertain(ANOTHER_OBJECT),
        // Weak or unavailable: nothing can show it is the directory the note was written in.
        Ok(_) => return uncertain(IDENTITY_UNAVAILABLE),
        Err(_) => return uncertain(UNREADABLE_DIRECTORY),
    }
    let t: Seen = match dir.metadata(&v.temp) {
        Ok(m) if m.file_type == FileType::File => Some(m.identity),
        // A link, a directory or anything else at the temporary's name is another object, not an unreadable one.
        Ok(_) => return uncertain(ANOTHER_OBJECT),
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(_) => return uncertain(UNREADABLE_DIRECTORY),
    };
    let d: Seen = match dir.metadata(&v.name) {
        Ok(m) => Some(m.identity),
        Err(e) if e.source.kind() == ErrorKind::NotFound => None,
        Err(_) => return uncertain(UNREADABLE_DIRECTORY),
    };
    decide(t, d, v.recorded, note.replacement)
}

fn strong(i: FileIdentity) -> bool {
    matches!(i, FileIdentity::Strong(_))
}

/// The matrix, rows in order; the first that matches decides. `t` is T (the temporary, a regular file), `d` is D (the
/// entry), `r` is R (the recorded identity).
fn decide(t: Seen, d: Seen, r: FileIdentity, replacement: bool) -> Verdict {
    // Row 1: D is R, both Strong.
    if let Some(i) = d
        && strong(i)
        && strong(r)
        && i == r
    {
        return Verdict::Renamed { remove_temp: t == Some(r) };
    }
    // Row 2: T present, D absent.
    if t.is_some() && d.is_none() {
        return Verdict::NotRenamed;
    }
    // Row 3: T present, a replacement (`rename_replace` never leaves two names).
    if t.is_some() && replacement {
        return Verdict::NotRenamed;
    }
    // Row 3b: T is R and D another object, all Strong.
    if let (Some(j), Some(i)) = (t, d)
        && strong(j)
        && strong(r)
        && j == r
        && strong(i)
        && i != r
    {
        return Verdict::NotRenamed;
    }
    // Row 4: T absent, and D absent or another Strong object.
    if t.is_none()
        && match d {
            None => true,
            Some(i) => strong(i) && strong(r) && i != r,
        }
    {
        return Verdict::Gone;
    }
    // Row 5.
    if [Some(r), t, d].into_iter().flatten().all(strong) {
        uncertain(ANOTHER_OBJECT)
    } else {
        uncertain(IDENTITY_UNAVAILABLE)
    }
}

/// A note's target (Normal components joined by 0x00) as a path for messages.
fn target_path(note: &PreparedRecord) -> PathBuf {
    note.target.0.split(|b| *b == 0).map(|c| String::from_utf8_lossy(c).into_owned()).collect()
}

/// Row 1's stray temp name, removed only while it still holds the published object `recorded`. Anything else at the
/// name (it was replaced between classification and now) is kept and reported like a failed removal.
fn remove_stray<D: DirHandle>(dest: &D, rel: &Path, recorded: FileIdentity) -> Result<(), FsError> {
    let parts: Vec<&OsStr> = rel.iter().collect();
    let Some((leaf, dirs)) = parts.split_last() else { return Ok(()) };
    let mut held: Option<D> = None;
    for name in dirs {
        match held.as_ref().unwrap_or(dest).open_dir(name) {
            Ok(next) => held = Some(next),
            Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    match held.as_ref().unwrap_or(dest).metadata(leaf) {
        Ok(m) if m.identity == recorded => {}
        Ok(_) => {
            return Err(FsError::new(
                Code::SafetyRejected,
                std::io::Error::other("the temporary's name no longer holds the published object"),
            ));
        }
        Err(e) if e.source.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    remove_validated(dest, rel).map(|_| ())
}

/// Both phases: classify every note, then `apply_recovery` once, then remove the stray temp names of RENAMED rows. A
/// failed removal is a `PartialKept` warning (the object IS published) and never fails the adoption.
pub(crate) fn recover_publications<D: DirHandle, S: ClaimStore>(
    dest: &D,
    dest_shown: &Path,
    operation_id: &str,
    store: &mut S,
    warnings: &mut Vec<RunWarning>,
    manifest_shown: &Path,
) -> Result<Recovered, RecoverError> {
    let notes = store.prepared().map_err(RecoverError::Store)?;
    if notes.is_empty() {
        return Ok(Recovered { recovered: 0 });
    }
    let mut ops = Vec::with_capacity(notes.len());
    let mut undecided = Vec::new();
    let mut strays = Vec::new();
    let mut recovered = 0;
    for (key, note) in notes {
        match classify(dest, operation_id, &key, &note) {
            Verdict::Renamed { remove_temp } => {
                recovered += 1;
                // `classify` validated the note, so this is `Some`.
                if remove_temp && let Some(v) = validate(operation_id, &key, &note) {
                    strays.push((v.dir.join(v.temp), v.recorded));
                }
                let planned = (!note.planned_name.is_empty())
                    .then(|| ClaimKey { parent: key.parent, name: note.planned_name.clone() });
                ops.push(RecoveryOp::Commit { key, target: note.target, planned });
            }
            Verdict::NotRenamed | Verdict::Gone => ops.push(RecoveryOp::Discard { key }),
            Verdict::Uncertain(why) => undecided.push((dest_shown.join(target_path(&note)), why)),
        }
    }
    if !undecided.is_empty() {
        return Err(RecoverError::Uncertain(undecided));
    }
    store.apply_recovery(&ops).map_err(RecoverError::Store)?;
    for (rel, recorded) in strays {
        if let Err(error) = remove_stray(dest, &rel, recorded) {
            warnings.push(RunWarning::PartialKept {
                path: dest_shown.join(&rel),
                error,
                kept: manifest_shown.to_path_buf(),
            });
        }
    }
    Ok(Recovered { recovered })
}

/// The COMMIT_STATE_UNCERTAIN detail: up to ten undecided targets with their evidence, then the total.
pub(crate) fn uncertain_detail(list: &[(PathBuf, String)]) -> String {
    let mut named: Vec<String> = list
        .iter()
        .take(NAMED)
        .map(|(path, why)| {
            format!("{}: cannot tell whether this file was published ({why})", path.display())
        })
        .collect();
    if list.len() > NAMED {
        named.push(format!("and {} more", list.len() - NAMED));
    }
    format!(
        "{}; the operation's state is preserved. Move or delete the destination ENTRY you do not want (not the \
         temporary file) so it reads as absent (the next resume then redoes the file), or run again with --restart to \
         supersede this operation ({} undecided in all)",
        named.join("; "),
        list.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_fs::ObjectId;

    fn s(n: u128) -> FileIdentity {
        FileIdentity::Strong(ObjectId { volume: 1, index: n })
    }

    fn w(n: u128) -> FileIdentity {
        FileIdentity::Weak(ObjectId { volume: 1, index: n })
    }

    #[test]
    fn the_matrix_rows_in_order() {
        use Verdict::*;
        let r = s(1);
        // (T, D, replacement) -> verdict
        let cases: [(Seen, Seen, bool, Verdict); 12] = [
            (None, Some(r), false, Renamed { remove_temp: false }),
            (Some(r), Some(r), false, Renamed { remove_temp: true }),
            // Row 1 before row 3: a RENAMED replacement is never read as row 3.
            (Some(s(2)), Some(r), true, Renamed { remove_temp: false }),
            (Some(w(9)), None, false, NotRenamed),
            (Some(w(9)), Some(w(8)), true, NotRenamed),
            (Some(r), Some(s(2)), false, NotRenamed),
            (None, None, false, Gone),
            (None, Some(s(2)), false, Gone),
            (None, Some(w(1)), false, uncertain(IDENTITY_UNAVAILABLE)),
            (Some(s(3)), Some(s(2)), false, uncertain(ANOTHER_OBJECT)),
            (Some(w(1)), Some(s(2)), false, uncertain(IDENTITY_UNAVAILABLE)),
            (Some(r), Some(FileIdentity::Unavailable), false, uncertain(IDENTITY_UNAVAILABLE)),
        ];
        for (t, d, replacement, want) in cases {
            assert_eq!(decide(t, d, r, replacement), want, "{t:?} {d:?} {replacement}");
        }
        // R itself Weak: neither row 1 nor row 4's second form.
        assert_eq!(decide(None, Some(w(1)), w(1), false), uncertain(IDENTITY_UNAVAILABLE));
        assert_eq!(decide(None, Some(s(2)), w(1), false), uncertain(IDENTITY_UNAVAILABLE));
    }

    #[test]
    fn the_detail_names_ten_targets_and_the_total() {
        let list: Vec<(PathBuf, String)> =
            (0..12).map(|i| (PathBuf::from(format!("t{i}")), INVALID_NOTE.to_string())).collect();
        let d = uncertain_detail(&list);
        assert!(
            d.contains("t9: cannot tell whether this file was published (invalid note)"),
            "{d}"
        );
        assert!(!d.contains("t10:"), "{d}");
        assert!(d.contains("and 2 more"), "{d}");
        assert!(d.ends_with("(12 undecided in all)"), "{d}");
    }
}
