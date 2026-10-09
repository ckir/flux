//! The claim model (cut 8b): keys, records, the `ClaimStore` trait and its
//! conformance suite. A claim is never released.

use std::ffi::OsStr;
use std::path::{Component, Path};

use crate::{Code, FsError, ObjectId, Result};

/// A destination path relative to the root, as Normal components' encoded
/// bytes joined by 0x00 (section 103).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FluxPathKey(pub Vec<u8>);

impl FluxPathKey {
    /// An empty path is the empty key; a component containing 0x00 is
    /// `Code::DestinationError`.
    ///
    /// Callers must pass a normalized relative path: ParentDir, RootDir and Prefix
    /// components are dropped, not rejected.
    pub fn from_relative(rel: &Path) -> Result<Self> {
        let mut out = Vec::new();
        let mut first = true;
        for c in rel.components() {
            if let Component::Normal(name) = c {
                let bytes = name.as_encoded_bytes();
                if bytes.contains(&0) {
                    return Err(FsError::new(
                        Code::DestinationError,
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "path component contains a NUL byte",
                        ),
                    ));
                }
                if !first {
                    out.push(0);
                }
                first = false;
                out.extend_from_slice(bytes);
            }
        }
        Ok(FluxPathKey(out))
    }
}

/// `(parent directory identity, entry name bytes as stored)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimKey {
    pub parent: ObjectId,
    pub name: Vec<u8>,
}

impl ClaimKey {
    pub fn new(parent: ObjectId, name: &OsStr) -> Self {
        ClaimKey { parent, name: name.as_encoded_bytes().to_vec() }
    }

    /// `parent.volume` (u64 BE) ++ `parent.index` (u128 BE) ++ name bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 16 + self.name.len());
        out.extend_from_slice(&self.parent.volume.to_be_bytes());
        out.extend_from_slice(&self.parent.index.to_be_bytes());
        out.extend_from_slice(&self.name);
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimStatus {
    Existing,
    Created,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimRecord {
    pub target: FluxPathKey,
    pub status: ClaimStatus,
}

impl ClaimRecord {
    /// One status byte (0 Existing, 1 Created) ++ target bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.target.0.len());
        out.push(match self.status {
            ClaimStatus::Existing => 0,
            ClaimStatus::Created => 1,
        });
        out.extend_from_slice(&self.target.0);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&first, rest) = bytes.split_first()?;
        let status = match first {
            0 => ClaimStatus::Existing,
            1 => ClaimStatus::Created,
            _ => return None,
        };
        Some(ClaimRecord { target: FluxPathKey(rest.to_vec()), status })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    Inserted,
    Present(ClaimRecord),
}

/// A publication's note (cut 9c, spec decision 5), keyed like the claim it becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedRecord {
    pub target: FluxPathKey,
    pub temp_name: Vec<u8>,
    /// `identity_text` of the temporary's identity, parsed by the core.
    pub identity: String,
    /// `native_hex` of the entry's directory relative to DEST; empty for DEST itself.
    pub dir_path: String,
    pub name: Vec<u8>,
    /// Empty when equal to `name`.
    pub planned_name: Vec<u8>,
    pub replacement: bool,
}

impl PreparedRecord {
    /// Version byte 1; then target, temp_name, identity, dir_path, name, planned_name each as
    /// u32 BE length + bytes; then replacement (0/1).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![1u8];
        for f in [
            self.target.0.as_slice(),
            self.temp_name.as_slice(),
            self.identity.as_bytes(),
            self.dir_path.as_bytes(),
            self.name.as_slice(),
            self.planned_name.as_slice(),
        ] {
            out.extend_from_slice(&(f.len() as u32).to_be_bytes());
            out.extend_from_slice(f);
        }
        out.push(u8::from(self.replacement));
        out
    }

    /// `None` for another version, a short or over-long field, trailing bytes, or a
    /// replacement byte other than 0/1.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        fn field<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
            let (len, tail) = rest.split_first_chunk::<4>()?;
            let len = u32::from_be_bytes(*len) as usize;
            if tail.len() < len {
                return None;
            }
            let (f, tail) = tail.split_at(len);
            *rest = tail;
            Some(f)
        }
        let (&version, mut rest) = bytes.split_first()?;
        if version != 1 {
            return None;
        }
        let target = field(&mut rest)?.to_vec();
        let temp_name = field(&mut rest)?.to_vec();
        let identity = String::from_utf8(field(&mut rest)?.to_vec()).ok()?;
        let dir_path = String::from_utf8(field(&mut rest)?.to_vec()).ok()?;
        let name = field(&mut rest)?.to_vec();
        let planned_name = field(&mut rest)?.to_vec();
        let replacement = match rest {
            [0] => false,
            [1] => true,
            _ => return None,
        };
        Some(PreparedRecord {
            target: FluxPathKey(target),
            temp_name,
            identity,
            dir_path,
            name,
            planned_name,
            replacement,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryOp {
    Commit { key: ClaimKey, target: FluxPathKey, planned: Option<ClaimKey> },
    Discard { key: ClaimKey },
}

pub trait ClaimStore {
    fn insert_if_absent(&mut self, key: &ClaimKey, record: &ClaimRecord) -> Result<ClaimOutcome>;
    fn get(&self, key: &ClaimKey) -> Result<Option<ClaimRecord>>;
    /// Rewrites the claim at `key` with status `Created`, keeping its target.
    ///
    /// Errors when `key` is missing, and when the stored record's target differs from
    /// `target`: that is what a "foreign owner" means here. The claim is left untouched
    /// in both cases.
    fn upgrade_own_claim(&mut self, key: &ClaimKey, target: &FluxPathKey) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    /// The number of claims in the store.
    fn count(&self) -> Result<u64>;
    /// False for a store whose format has no `prepared` table (format 1).
    fn supports_prepared(&self) -> bool;
    /// Inserts the note; a note already at `key` is `Code::IoError`. Always an Immediate commit.
    fn prepare(&mut self, key: &ClaimKey, record: &PreparedRecord) -> Result<()>;
    /// ONE transaction: the claim at `key` becomes `Created` for `target` (inserted if absent,
    /// upgraded if `Existing` of the same target); when `planned` is given a `Created` claim for
    /// it is inserted (an existing claim there owned by the same target is accepted); the note at
    /// `key` is deleted. A claim owned by another target at `key` or `planned` is `Code::IoError`
    /// and nothing changes. Always Immediate.
    fn commit_prepared(
        &mut self,
        key: &ClaimKey,
        target: &FluxPathKey,
        planned: Option<&ClaimKey>,
    ) -> Result<()>;
    /// Removes the note; a key with no note is `Ok` (idempotent). Always Immediate.
    fn discard_prepared(&mut self, key: &ClaimKey) -> Result<()>;
    /// Every note, in the byte order of `ClaimKey::encode` (redb's own order; the fake sorts its
    /// map the same way).
    fn prepared(&self) -> Result<Vec<(ClaimKey, PreparedRecord)>>;
    /// Every `op` in ONE Immediate transaction, each with `commit_prepared`'s /
    /// `discard_prepared`'s rules; nothing changes when any errors.
    fn apply_recovery(&mut self, ops: &[RecoveryOp]) -> Result<()>;
}

/// Backend-independent cases every `ClaimStore` must pass.
pub mod conformance {
    use super::*;

    fn dir(n: u128) -> ObjectId {
        ObjectId { volume: 1, index: n }
    }

    fn key(parent: u128, name: &str) -> ClaimKey {
        ClaimKey::new(dir(parent), OsStr::new(name))
    }

    fn rec(target: &str, status: ClaimStatus) -> ClaimRecord {
        ClaimRecord { target: FluxPathKey(target.as_bytes().to_vec()), status }
    }

    pub fn run_all<S: ClaimStore>(new_store: impl Fn() -> S) {
        insert_if_absent_inserts_then_reports_present(new_store());
        an_own_claim_reads_back_as_present_with_the_same_target(new_store());
        a_foreign_claim_is_returned_not_replaced(new_store());
        upgrade_own_claim_sets_created_and_keeps_the_target(new_store());
        upgrade_of_a_missing_key_or_a_foreign_owner_is_an_error(new_store());
        the_same_name_under_different_parents_are_different_keys(new_store());
        names_differing_only_in_case_are_different_keys(new_store());
        flush_is_idempotent_and_keeps_every_claim(new_store());
        many_keys_round_trip(new_store());
        count_follows_inserts_and_upgrades(new_store());
    }

    fn note(target: &str) -> PreparedRecord {
        PreparedRecord {
            target: FluxPathKey(target.as_bytes().to_vec()),
            temp_name: b"n.flux-partial.x".to_vec(),
            identity: "strong:1:9".to_string(),
            dir_path: String::new(),
            name: b"n".to_vec(),
            planned_name: Vec::new(),
            replacement: false,
        }
    }

    fn tgt(target: &str) -> FluxPathKey {
        FluxPathKey(target.as_bytes().to_vec())
    }

    pub fn run_prepared_all<S: ClaimStore>(new_store: impl Fn() -> S) {
        supports_prepared_is_true_for_a_fresh_store(new_store());
        prepare_then_prepared_lists_the_note_and_a_second_prepare_is_an_error(new_store());
        commit_prepared_writes_a_created_claim_and_removes_the_note(new_store());
        commit_prepared_upgrades_an_own_existing_claim(new_store());
        commit_prepared_with_a_planned_key_inserts_the_second_claim_and_accepts_its_own_repeat(
            new_store(),
        );
        commit_prepared_of_a_foreign_claim_changes_nothing(new_store());
        discard_prepared_is_idempotent(new_store());
        apply_recovery_applies_every_op_or_none(new_store());
        notes_and_claims_share_keys_but_not_tables(new_store());
    }

    pub fn supports_prepared_is_true_for_a_fresh_store<S: ClaimStore>(s: S) {
        assert!(s.supports_prepared(), "prepared: a fresh store must support notes");
    }

    pub fn prepare_then_prepared_lists_the_note_and_a_second_prepare_is_an_error<S: ClaimStore>(
        mut s: S,
    ) {
        let k = key(1, "a");
        let n = note("a");
        assert!(s.prepared().unwrap().is_empty(), "prepare: a fresh store has no notes");
        s.prepare(&k, &n).unwrap();
        assert_eq!(s.prepared().unwrap(), vec![(k.clone(), n.clone())], "prepare: lists the note");
        let err = s.prepare(&k, &note("other")).unwrap_err();
        assert_eq!(err.code, Code::IoError, "prepare: a second note at the key is IoError");
        assert_eq!(s.prepared().unwrap(), vec![(k, n)], "prepare: the first note is kept");
    }

    pub fn commit_prepared_writes_a_created_claim_and_removes_the_note<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        s.prepare(&k, &note("a")).unwrap();
        assert!(s.get(&k).unwrap().is_none(), "commit: no claim before the commit");
        s.commit_prepared(&k, &tgt("a"), None).unwrap();
        assert_eq!(s.get(&k).unwrap(), Some(rec("a", ClaimStatus::Created)), "commit: Created");
        assert!(s.prepared().unwrap().is_empty(), "commit: the note is gone");
    }

    pub fn commit_prepared_upgrades_an_own_existing_claim<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        s.insert_if_absent(&k, &rec("a", ClaimStatus::Existing)).unwrap();
        s.prepare(&k, &note("a")).unwrap();
        s.commit_prepared(&k, &tgt("a"), None).unwrap();
        assert_eq!(s.get(&k).unwrap(), Some(rec("a", ClaimStatus::Created)), "upgrade: Created");
        assert!(s.prepared().unwrap().is_empty(), "upgrade: the note is gone");
        assert_eq!(s.count().unwrap(), 1, "upgrade: still one claim");
    }

    pub fn commit_prepared_with_a_planned_key_inserts_the_second_claim_and_accepts_its_own_repeat<
        S: ClaimStore,
    >(
        mut s: S,
    ) {
        let (k, p) = (key(1, "a"), key(1, "b"));
        s.prepare(&k, &note("a")).unwrap();
        s.commit_prepared(&k, &tgt("a"), Some(&p)).unwrap();
        assert_eq!(s.get(&k).unwrap(), Some(rec("a", ClaimStatus::Created)));
        assert_eq!(s.get(&p).unwrap(), Some(rec("a", ClaimStatus::Created)), "planned: Created");
        assert_eq!(s.count().unwrap(), 2, "planned: two claims");
        s.prepare(&k, &note("a")).unwrap();
        s.commit_prepared(&k, &tgt("a"), Some(&p)).unwrap();
        assert_eq!(s.count().unwrap(), 2, "planned: a repeat keeps exactly two claims");
        assert!(s.prepared().unwrap().is_empty(), "planned: the repeat's note is gone");
    }

    pub fn commit_prepared_of_a_foreign_claim_changes_nothing<S: ClaimStore>(mut s: S) {
        let (k, p) = (key(1, "a"), key(1, "b"));
        // A foreign claim at the key.
        s.insert_if_absent(&k, &rec("other", ClaimStatus::Existing)).unwrap();
        s.prepare(&k, &note("a")).unwrap();
        assert!(s.commit_prepared(&k, &tgt("a"), None).is_err(), "foreign: at key is an error");
        assert_eq!(s.get(&k).unwrap(), Some(rec("other", ClaimStatus::Existing)));
        assert_eq!(s.prepared().unwrap().len(), 1, "foreign: the note is still listed");
        assert_eq!(s.count().unwrap(), 1, "foreign: claims unchanged");
        // A foreign claim at the planned key: the key's own claim must not be written either.
        let k2 = key(2, "a");
        s.insert_if_absent(&p, &rec("other", ClaimStatus::Created)).unwrap();
        s.prepare(&k2, &note("a")).unwrap();
        assert!(
            s.commit_prepared(&k2, &tgt("a"), Some(&p)).is_err(),
            "foreign: at planned is an error"
        );
        assert!(s.get(&k2).unwrap().is_none(), "foreign: nothing written at the key");
        assert_eq!(s.get(&p).unwrap(), Some(rec("other", ClaimStatus::Created)));
        assert_eq!(s.prepared().unwrap().len(), 2, "foreign: both notes still listed");
        assert_eq!(s.count().unwrap(), 2, "foreign: claims unchanged");
    }

    pub fn discard_prepared_is_idempotent<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        s.discard_prepared(&k).unwrap();
        s.prepare(&k, &note("a")).unwrap();
        s.discard_prepared(&k).unwrap();
        assert!(s.prepared().unwrap().is_empty(), "discard: the note is gone");
        s.discard_prepared(&k).unwrap();
        assert_eq!(s.count().unwrap(), 0, "discard: no claim was written");
    }

    pub fn apply_recovery_applies_every_op_or_none<S: ClaimStore>(mut s: S) {
        let (a, b) = (key(1, "a"), key(1, "b"));
        s.prepare(&a, &note("a")).unwrap();
        s.prepare(&b, &note("b")).unwrap();
        s.apply_recovery(&[
            RecoveryOp::Commit { key: a.clone(), target: tgt("a"), planned: None },
            RecoveryOp::Discard { key: b.clone() },
        ])
        .unwrap();
        assert_eq!(s.get(&a).unwrap(), Some(rec("a", ClaimStatus::Created)));
        assert!(s.get(&b).unwrap().is_none(), "recovery: a discard writes no claim");
        assert!(s.prepared().unwrap().is_empty(), "recovery: both notes are gone");

        let (c, f) = (key(1, "c"), key(1, "f"));
        s.prepare(&c, &note("c")).unwrap();
        s.insert_if_absent(&f, &rec("other", ClaimStatus::Existing)).unwrap();
        s.prepare(&f, &note("f")).unwrap();
        let before = s.count().unwrap();
        let res = s.apply_recovery(&[
            RecoveryOp::Commit { key: c.clone(), target: tgt("c"), planned: None },
            RecoveryOp::Commit { key: f.clone(), target: tgt("f"), planned: None },
        ]);
        assert!(res.is_err(), "recovery: a foreign claim fails the whole batch");
        let listed: Vec<ClaimKey> = s.prepared().unwrap().into_iter().map(|(k, _)| k).collect();
        assert!(listed.contains(&c), "recovery: the fresh note is still listed");
        assert!(s.get(&c).unwrap().is_none(), "recovery: no claim was added for it");
        assert_eq!(s.count().unwrap(), before, "recovery: no claim added");
    }

    pub fn notes_and_claims_share_keys_but_not_tables<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        s.insert_if_absent(&k, &rec("a", ClaimStatus::Existing)).unwrap();
        s.prepare(&k, &note("a")).unwrap();
        assert_eq!(s.get(&k).unwrap(), Some(rec("a", ClaimStatus::Existing)), "tables: claim kept");
        assert_eq!(s.prepared().unwrap().len(), 1, "tables: the note coexists");
        assert_eq!(s.count().unwrap(), 1, "tables: notes are not counted as claims");
        s.commit_prepared(&k, &tgt("a"), None).unwrap();
        assert!(s.prepared().unwrap().is_empty(), "tables: the commit removes the note");
    }

    pub fn count_follows_inserts_and_upgrades<S: ClaimStore>(mut s: S) {
        assert_eq!(s.count().unwrap(), 0, "count: a fresh store must count 0");
        let r = rec("t", ClaimStatus::Existing);
        for k in [key(1, "a"), key(1, "b"), key(2, "a")] {
            s.insert_if_absent(&k, &r).unwrap();
        }
        assert_eq!(s.count().unwrap(), 3, "count: three inserts must count 3");
        assert!(
            matches!(s.insert_if_absent(&key(1, "a"), &r).unwrap(), ClaimOutcome::Present(_)),
            "count: a re-insert must be Present"
        );
        assert_eq!(s.count().unwrap(), 3, "count: a Present re-insert must keep 3");
        s.upgrade_own_claim(&key(1, "a"), &r.target).unwrap();
        assert_eq!(s.count().unwrap(), 3, "count: an upgrade must keep 3");
        s.flush().unwrap();
        assert_eq!(s.count().unwrap(), 3, "count: a flush must keep 3");
    }

    pub fn insert_if_absent_inserts_then_reports_present<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        let r = rec("a", ClaimStatus::Existing);
        assert!(s.get(&k).unwrap().is_none(), "insert_if_absent: absent key must read None");
        assert_eq!(
            s.insert_if_absent(&k, &r).unwrap(),
            ClaimOutcome::Inserted,
            "insert_if_absent: first insert must be Inserted"
        );
        assert!(
            matches!(s.insert_if_absent(&k, &r).unwrap(), ClaimOutcome::Present(_)),
            "insert_if_absent: second insert must be Present"
        );
    }

    pub fn an_own_claim_reads_back_as_present_with_the_same_target<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        let r = rec("x\0a", ClaimStatus::Existing);
        s.insert_if_absent(&k, &r).unwrap();
        assert_eq!(
            s.insert_if_absent(&k, &r).unwrap(),
            ClaimOutcome::Present(r.clone()),
            "own claim: re-insert must be Present with the same record"
        );
        assert_eq!(s.get(&k).unwrap(), Some(r), "own claim: get must return the record");
    }

    pub fn a_foreign_claim_is_returned_not_replaced<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        let mine = rec("one", ClaimStatus::Existing);
        let theirs = rec("two", ClaimStatus::Created);
        s.insert_if_absent(&k, &mine).unwrap();
        assert_eq!(
            s.insert_if_absent(&k, &theirs).unwrap(),
            ClaimOutcome::Present(mine.clone()),
            "foreign claim: the stored record must be returned"
        );
        assert_eq!(s.get(&k).unwrap(), Some(mine), "foreign claim: must not be replaced");
    }

    pub fn upgrade_own_claim_sets_created_and_keeps_the_target<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        let r = rec("t", ClaimStatus::Existing);
        s.insert_if_absent(&k, &r).unwrap();
        s.upgrade_own_claim(&k, &r.target).unwrap();
        assert_eq!(
            s.get(&k).unwrap(),
            Some(rec("t", ClaimStatus::Created)),
            "upgrade: status must become Created with the target kept"
        );
    }

    pub fn upgrade_of_a_missing_key_or_a_foreign_owner_is_an_error<S: ClaimStore>(mut s: S) {
        let k = key(1, "a");
        let t = FluxPathKey(b"t".to_vec());
        assert!(s.upgrade_own_claim(&k, &t).is_err(), "upgrade: a missing key must be an error");
        s.insert_if_absent(&k, &rec("other", ClaimStatus::Existing)).unwrap();
        assert!(s.upgrade_own_claim(&k, &t).is_err(), "upgrade: a foreign owner must be an error");
        assert_eq!(
            s.get(&k).unwrap(),
            Some(rec("other", ClaimStatus::Existing)),
            "upgrade: a failed upgrade must leave the claim untouched"
        );
    }

    pub fn the_same_name_under_different_parents_are_different_keys<S: ClaimStore>(mut s: S) {
        let (a, b) = (key(1, "n"), key(2, "n"));
        assert_eq!(
            s.insert_if_absent(&a, &rec("a", ClaimStatus::Existing)).unwrap(),
            ClaimOutcome::Inserted
        );
        assert_eq!(
            s.insert_if_absent(&b, &rec("b", ClaimStatus::Existing)).unwrap(),
            ClaimOutcome::Inserted,
            "parents: the same name under another parent must be a new key"
        );
        assert_eq!(s.get(&a).unwrap(), Some(rec("a", ClaimStatus::Existing)));
        assert_eq!(s.get(&b).unwrap(), Some(rec("b", ClaimStatus::Existing)));
    }

    pub fn names_differing_only_in_case_are_different_keys<S: ClaimStore>(mut s: S) {
        let (a, b) = (key(1, "File"), key(1, "file"));
        assert_eq!(
            s.insert_if_absent(&a, &rec("File", ClaimStatus::Existing)).unwrap(),
            ClaimOutcome::Inserted
        );
        assert_eq!(
            s.insert_if_absent(&b, &rec("file", ClaimStatus::Existing)).unwrap(),
            ClaimOutcome::Inserted,
            "case: names differing only in case must be distinct keys"
        );
        assert_eq!(s.get(&a).unwrap(), Some(rec("File", ClaimStatus::Existing)));
        assert_eq!(s.get(&b).unwrap(), Some(rec("file", ClaimStatus::Existing)));
    }

    pub fn flush_is_idempotent_and_keeps_every_claim<S: ClaimStore>(mut s: S) {
        let keys: Vec<ClaimKey> = (0..10).map(|i| key(1, &format!("f{i}"))).collect();
        for (i, k) in keys.iter().enumerate() {
            s.insert_if_absent(k, &rec(&format!("t{i}"), ClaimStatus::Existing)).unwrap();
        }
        s.flush().unwrap();
        s.flush().unwrap();
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(
                s.get(k).unwrap(),
                Some(rec(&format!("t{i}"), ClaimStatus::Existing)),
                "flush: claim {i} must survive a double flush"
            );
        }
    }

    pub fn many_keys_round_trip<S: ClaimStore>(mut s: S) {
        const N: usize = 5000;
        for i in 0..N {
            let out = s
                .insert_if_absent(
                    &key(1, &format!("k{i}")),
                    &rec(&format!("t{i}"), ClaimStatus::Existing),
                )
                .unwrap();
            assert_eq!(out, ClaimOutcome::Inserted, "many: key {i} must be new");
        }
        for i in 0..N {
            assert_eq!(
                s.get(&key(1, &format!("k{i}"))).unwrap(),
                Some(rec(&format!("t{i}"), ClaimStatus::Existing)),
                "many: key {i} must read back"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::Path;

    #[test]
    fn flux_path_key_joins_components_with_nul() {
        assert_eq!(FluxPathKey::from_relative(Path::new("a/b/c")).unwrap().0, b"a\0b\0c");
        assert_eq!(FluxPathKey::from_relative(Path::new("")).unwrap().0, Vec::<u8>::new());
        assert_eq!(FluxPathKey::from_relative(Path::new("./a")).unwrap().0, b"a");
    }

    #[test]
    fn flux_path_key_rejects_a_component_containing_nul() {
        let p = std::path::PathBuf::from(std::ffi::OsString::from("a\0b"));
        let err = FluxPathKey::from_relative(&p).unwrap_err();
        assert_eq!(err.code, Code::DestinationError);
    }

    #[test]
    fn claim_key_encodes_volume_then_index_then_name_big_endian() {
        let got = ClaimKey::new(ObjectId { volume: 1, index: 2 }, OsStr::new("x")).encode();
        let mut want = vec![0, 0, 0, 0, 0, 0, 0, 1];
        want.extend_from_slice(&[0u8; 15]);
        want.push(2);
        want.extend_from_slice(b"x");
        assert_eq!(got.len(), 8 + 16 + 1);
        assert_eq!(got, want);
    }

    #[test]
    fn claim_record_round_trips_and_rejects_an_empty_or_unknown_status() {
        for status in [ClaimStatus::Existing, ClaimStatus::Created] {
            let r = ClaimRecord { target: FluxPathKey(b"a\0b".to_vec()), status };
            assert_eq!(ClaimRecord::decode(&r.encode()), Some(r));
        }
        assert_eq!(ClaimRecord::decode(&[]), None);
        assert_eq!(ClaimRecord::decode(&[7, 1]), None);
    }

    fn full_note() -> PreparedRecord {
        PreparedRecord {
            target: FluxPathKey(b"d\0n".to_vec()),
            temp_name: b"n.flux-partial.x".to_vec(),
            identity: "strong:1:9".to_string(),
            dir_path: "64".to_string(),
            name: b"n".to_vec(),
            planned_name: b"N".to_vec(),
            replacement: true,
        }
    }

    #[test]
    fn prepared_record_round_trips() {
        let full = full_note();
        assert_eq!(PreparedRecord::decode(&full.encode()), Some(full.clone()));
        let sparse = PreparedRecord {
            dir_path: String::new(),
            planned_name: Vec::new(),
            replacement: false,
            ..full
        };
        assert_eq!(PreparedRecord::decode(&sparse.encode()), Some(sparse));
    }

    #[test]
    fn prepared_record_rejects_a_foreign_version_truncation_trailing_bytes_and_a_bad_flag() {
        let good = full_note().encode();
        let mut v2 = good.clone();
        v2[0] = 2;
        assert_eq!(PreparedRecord::decode(&v2), None, "version 2");
        // The first length (target) claims more than the rest holds.
        let mut long = good.clone();
        long[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(PreparedRecord::decode(&long), None, "over-long field");
        assert_eq!(PreparedRecord::decode(&good[..good.len() - 1]), None, "truncated");
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(PreparedRecord::decode(&trailing), None, "trailing byte");
        let mut flag = good.clone();
        *flag.last_mut().unwrap() = 2;
        assert_eq!(PreparedRecord::decode(&flag), None, "replacement byte 2");
        assert_eq!(PreparedRecord::decode(&[]), None, "empty input");
    }

    #[test]
    fn prepared_record_layout_is_the_specs() {
        let r = PreparedRecord {
            target: FluxPathKey(b"a".to_vec()),
            temp_name: b"bc".to_vec(),
            identity: "d".to_string(),
            dir_path: String::new(),
            name: b"e".to_vec(),
            planned_name: Vec::new(),
            replacement: true,
        };
        let want: Vec<u8> = vec![
            1, // version
            0, 0, 0, 1, b'a', // target
            0, 0, 0, 2, b'b', b'c', // temp_name
            0, 0, 0, 1, b'd', // identity
            0, 0, 0, 0, // dir_path
            0, 0, 0, 1, b'e', // name
            0, 0, 0, 0, // planned_name
            1, // replacement
        ];
        assert_eq!(r.encode(), want);
    }
}
