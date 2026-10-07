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
}
