// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! The per-run hardlink index (cut 12): source object -> what the run did with the first member of its group.
//! Keys and values are byte strings so the redb file and the in-memory fake share one encoding.

use crate::{FluxPathKey, ObjectId, Result};

/// What the run recorded for one source object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HardlinkEntry {
    /// The group's first member was written (or adopted) at `target`; `object` is its destination object when known.
    Anchor { target: FluxPathKey, object: Option<ObjectId> },
    /// The group's first member failed; later members name `canonical` in their failure.
    Failed { canonical: FluxPathKey },
    /// Members the existing-file policy skipped before the group had an anchor (ruling 16): how many, the destination
    /// object of the first of them, and that member's claim key (the example a later degradation names).
    Skipped { object: Option<ObjectId>, members: u64, example: FluxPathKey },
}

fn object_from(bytes: &[u8; 24]) -> ObjectId {
    let (volume, index) = bytes.split_first_chunk::<8>().expect("24 bytes");
    ObjectId {
        volume: u64::from_be_bytes(*volume),
        index: u128::from_be_bytes((*index).try_into().expect("16 bytes")),
    }
}

impl HardlinkEntry {
    /// Tag byte, then: `0` target; `1` object (24) ++ target; `2` canonical; `3` members (u64 BE) ++ example;
    /// `4` object (24) ++ members (u64 BE) ++ example.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            HardlinkEntry::Anchor { target, object: None } => [&[0u8][..], &target.0].concat(),
            HardlinkEntry::Anchor { target, object: Some(o) } => {
                [&[1u8][..], &encode_object(o), &target.0].concat()
            }
            HardlinkEntry::Failed { canonical } => [&[2u8][..], &canonical.0].concat(),
            HardlinkEntry::Skipped { object: None, members, example } => {
                [&[3u8][..], &members.to_be_bytes(), &example.0].concat()
            }
            HardlinkEntry::Skipped { object: Some(o), members, example } => {
                [&[4u8][..], &encode_object(o), &members.to_be_bytes(), &example.0].concat()
            }
        }
    }

    /// `None` for an unknown tag, a tag-1 value under 25 bytes, a tag-3 value under 9 or a tag-4 value under 33.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (tag, rest) = bytes.split_first()?;
        match tag {
            0 => Some(HardlinkEntry::Anchor { target: FluxPathKey(rest.to_vec()), object: None }),
            1 => {
                let (o, target) = rest.split_first_chunk::<24>()?;
                Some(HardlinkEntry::Anchor {
                    target: FluxPathKey(target.to_vec()),
                    object: Some(object_from(o)),
                })
            }
            2 => Some(HardlinkEntry::Failed { canonical: FluxPathKey(rest.to_vec()) }),
            3 => {
                let (m, example) = rest.split_first_chunk::<8>()?;
                Some(HardlinkEntry::Skipped {
                    object: None,
                    members: u64::from_be_bytes(*m),
                    example: FluxPathKey(example.to_vec()),
                })
            }
            4 => {
                let (o, rest) = rest.split_first_chunk::<24>()?;
                let (m, example) = rest.split_first_chunk::<8>()?;
                Some(HardlinkEntry::Skipped {
                    object: Some(object_from(o)),
                    members: u64::from_be_bytes(*m),
                    example: FluxPathKey(example.to_vec()),
                })
            }
            _ => None,
        }
    }
}

/// `volume` u64 BE ++ `index` u128 BE: 24 bytes, the index key.
pub fn encode_object(id: &ObjectId) -> [u8; 24] {
    let mut out = [0u8; 24];
    out[..8].copy_from_slice(&id.volume.to_be_bytes());
    out[8..].copy_from_slice(&id.index.to_be_bytes());
    out
}

/// The per-run map from a source object to its group's entry. Created by `DirHandle::create_hardlink_index`.
pub trait HardlinkIndex {
    fn get(&self, id: &ObjectId) -> Result<Option<HardlinkEntry>>;

    /// Upsert: a re-registered canonical (resume rule 3's recopied anchor) replaces the entry.
    fn insert(&mut self, id: &ObjectId, entry: &HardlinkEntry) -> Result<()>;

    fn count(&self) -> Result<u64>;
}

/// Backend-independent cases every `HardlinkIndex` must pass.
pub mod conformance {
    use super::*;

    fn obj(volume: u64, index: u128) -> ObjectId {
        ObjectId { volume, index }
    }

    fn anchor(target: &str) -> HardlinkEntry {
        HardlinkEntry::Anchor { target: FluxPathKey(target.as_bytes().to_vec()), object: None }
    }

    pub fn run_all<I: HardlinkIndex>(new_index: impl Fn() -> I) {
        a_fresh_index_is_empty(new_index());
        insert_then_get_returns_the_entry(new_index());
        insert_replaces_an_earlier_entry_for_the_same_object(new_index());
        objects_on_different_volumes_are_different_keys(new_index());
        count_follows_inserts_and_replacements(new_index());
        many_entries_round_trip(new_index());
    }

    fn a_fresh_index_is_empty<I: HardlinkIndex>(index: I) {
        assert_eq!(index.count().unwrap(), 0);
        assert_eq!(index.get(&obj(1, 1)).unwrap(), None);
    }

    fn insert_then_get_returns_the_entry<I: HardlinkIndex>(mut index: I) {
        let entries = [
            anchor("a"),
            HardlinkEntry::Anchor { target: FluxPathKey(b"b".to_vec()), object: Some(obj(3, 4)) },
            HardlinkEntry::Failed { canonical: FluxPathKey(b"c".to_vec()) },
            HardlinkEntry::Skipped {
                object: Some(obj(5, 6)),
                members: 2,
                example: FluxPathKey(b"d".to_vec()),
            },
        ];
        for (i, e) in entries.iter().enumerate() {
            index.insert(&obj(1, i as u128), e).unwrap();
        }
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(index.get(&obj(1, i as u128)).unwrap().as_ref(), Some(e));
        }
    }

    fn insert_replaces_an_earlier_entry_for_the_same_object<I: HardlinkIndex>(mut index: I) {
        index.insert(&obj(1, 1), &anchor("first")).unwrap();
        let second = HardlinkEntry::Failed { canonical: FluxPathKey(b"second".to_vec()) };
        index.insert(&obj(1, 1), &second).unwrap();
        assert_eq!(index.get(&obj(1, 1)).unwrap(), Some(second));
    }

    fn objects_on_different_volumes_are_different_keys<I: HardlinkIndex>(mut index: I) {
        index.insert(&obj(1, 9), &anchor("one")).unwrap();
        index.insert(&obj(2, 9), &anchor("two")).unwrap();
        assert_eq!(index.get(&obj(1, 9)).unwrap(), Some(anchor("one")));
        assert_eq!(index.get(&obj(2, 9)).unwrap(), Some(anchor("two")));
        assert_eq!(index.get(&obj(3, 9)).unwrap(), None);
        assert_eq!(index.count().unwrap(), 2);
    }

    fn count_follows_inserts_and_replacements<I: HardlinkIndex>(mut index: I) {
        index.insert(&obj(1, 1), &anchor("a")).unwrap();
        assert_eq!(index.count().unwrap(), 1);
        index.insert(&obj(1, 2), &anchor("b")).unwrap();
        assert_eq!(index.count().unwrap(), 2);
        index.insert(&obj(1, 1), &anchor("c")).unwrap();
        assert_eq!(index.count().unwrap(), 2, "a replacement is not a new entry");
    }

    fn many_entries_round_trip<I: HardlinkIndex>(mut index: I) {
        for i in 0..5000u128 {
            index.insert(&obj(1, i), &anchor(&format!("t{i}"))).unwrap();
        }
        assert_eq!(index.count().unwrap(), 5000);
        for i in 0..5000u128 {
            assert_eq!(index.get(&obj(1, i)).unwrap(), Some(anchor(&format!("t{i}"))), "{i}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_round_trips_every_variant() {
        let o = ObjectId { volume: u64::MAX, index: u128::MAX - 1 };
        let entries = [
            HardlinkEntry::Anchor { target: FluxPathKey(b"a\0b".to_vec()), object: None },
            HardlinkEntry::Anchor { target: FluxPathKey(Vec::new()), object: None },
            HardlinkEntry::Anchor { target: FluxPathKey(b"a\0b".to_vec()), object: Some(o) },
            HardlinkEntry::Anchor { target: FluxPathKey(Vec::new()), object: Some(o) },
            HardlinkEntry::Failed { canonical: FluxPathKey(b"x".to_vec()) },
            HardlinkEntry::Failed { canonical: FluxPathKey(Vec::new()) },
            HardlinkEntry::Skipped {
                object: None,
                members: 7,
                example: FluxPathKey(b"e\0f".to_vec()),
            },
            HardlinkEntry::Skipped { object: None, members: 0, example: FluxPathKey(Vec::new()) },
            HardlinkEntry::Skipped {
                object: Some(o),
                members: u64::MAX,
                example: FluxPathKey(b"e".to_vec()),
            },
            HardlinkEntry::Skipped {
                object: Some(o),
                members: 1,
                example: FluxPathKey(Vec::new()),
            },
        ];
        for e in entries {
            assert_eq!(HardlinkEntry::decode(&e.encode()), Some(e.clone()), "{e:?}");
        }
    }

    #[test]
    fn decode_rejects_an_unknown_tag_and_a_short_object() {
        assert_eq!(HardlinkEntry::decode(&[]), None);
        assert_eq!(HardlinkEntry::decode(&[5]), None);
        assert_eq!(HardlinkEntry::decode(&[5; 40]), None);
        let mut short = vec![1u8];
        short.extend([0u8; 10]);
        assert_eq!(HardlinkEntry::decode(&short), None, "tag 1 with 10 bytes");
        let mut short = vec![3u8];
        short.extend([0u8; 7]);
        assert_eq!(HardlinkEntry::decode(&short), None, "tag 3 with 8 bytes in all");
        let mut short = vec![4u8];
        short.extend([0u8; 31]);
        assert_eq!(HardlinkEntry::decode(&short), None, "tag 4 with 32 bytes in all");
    }

    #[test]
    fn object_key_is_volume_then_index_big_endian() {
        let key = encode_object(&ObjectId { volume: 1, index: 2 });
        let mut want = vec![0, 0, 0, 0, 0, 0, 0, 1];
        want.extend([0u8; 15]);
        want.push(2);
        assert_eq!(key.to_vec(), want);
    }
}
