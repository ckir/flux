//! The resume decision: which bytes of a crashed run's partial file a resumed run may trust (spec decision 7).
//!
//! `decide` is pure over `Read`ers. It re-hashes the source and the partial chunk by chunk and compares both with
//! the digest the WAL recorded; a chunk is trusted only when BOTH sides match.

use crate::copy::Heartbeat;
use crate::wal::replay::FileState;
use crate::wal::{CHUNK_SIZE, Digest};
use flux_fs::FsError;
use std::io::{self, Read};

/// The source as it is now.
pub(crate) struct Gate<'a> {
    pub len: u64,
    pub mtime: Option<(i64, u32)>,
    pub identity: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Start {
    Fresh,
    Resume { offset: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discard {
    SourceChanged(&'static str),
    PartialMissing,
    MismatchAt(u64),
    PartialShorter {
        kept: u64,
    },
    /// The checkpoints (or the partial) could not be read: "checkpoints unreadable: {why}".
    Unreadable(String),
}

/// `carry` is the source bytes `decide` consumed that the caller must write first. For both starts the source
/// reader is left at `start_offset + carry.len()`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub start: Start,
    pub carry: Vec<u8>,
    pub discard: Option<Discard>,
}

/// Reads up to `buf.len()` bytes, looping over short reads and retrying `Interrupted`; fewer only at end of stream.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

fn hash(bytes: &[u8]) -> Digest {
    *blake3::hash(bytes).as_bytes()
}

pub(crate) fn decide<S: Read, P: Read>(
    state: &FileState,
    gate: &Gate<'_>,
    partial_len: Option<u64>,
    source: &mut S,
    partial: Option<&mut P>,
    beat: &Heartbeat<'_>,
) -> Result<Verdict, FsError> {
    let fresh = |discard| Verdict { start: Start::Fresh, carry: Vec::new(), discard };
    let begin = &state.begin;
    if begin.source_len != gate.len {
        return Ok(fresh(Some(Discard::SourceChanged("length"))));
    }
    if begin.source_mtime != gate.mtime {
        return Ok(fresh(Some(Discard::SourceChanged("modified time"))));
    }
    if begin.source_identity != gate.identity {
        return Ok(fresh(Some(Discard::SourceChanged("identity"))));
    }
    let (Some(partial_len), Some(partial)) = (partial_len, partial) else {
        return Ok(fresh(Some(Discard::PartialMissing)));
    };
    let Some(highest) = state.digests.keys().next_back().copied() else {
        return Ok(fresh(None));
    };
    let want = highest + 1;
    let have = partial_len.div_ceil(CHUNK_SIZE);
    // `pos` is the end of the validated prefix; `shorter` records that the partial ended the validation.
    let mut pos = 0u64;
    let mut shorter = have < want;
    let mut source_buf = vec![0u8; CHUNK_SIZE as usize];
    let mut partial_buf = vec![0u8; CHUNK_SIZE as usize];
    for k in 0..want.min(have) {
        let Some(recorded) = state.digests.get(&k) else {
            shorter = false;
            break;
        };
        let start = k * CHUNK_SIZE;
        if start >= gate.len {
            shorter = false;
            break;
        }
        let len = (gate.len - start).min(CHUNK_SIZE);
        if partial_len < start + len {
            shorter = true;
            break;
        }
        beat()?;
        let len = len as usize;
        let got = read_full(source, &mut source_buf[..len]).map_err(FsError::from_io)?;
        let source_ok = got == len && hash(&source_buf[..len]) == *recorded;
        let partial_ok = matches!(read_full(partial, &mut partial_buf[..len]), Ok(n) if n == len)
            && hash(&partial_buf[..len]) == *recorded;
        if !(source_ok && partial_ok) {
            let carry = source_buf[..got].to_vec();
            let discard = Some(Discard::MismatchAt(start));
            let start = if k == 0 { Start::Fresh } else { Start::Resume { offset: start } };
            return Ok(Verdict { start, carry, discard });
        }
        pos = start + len as u64;
    }
    let discard = shorter.then_some(Discard::PartialShorter { kept: pos });
    let start = if pos == 0 { Start::Fresh } else { Start::Resume { offset: pos } };
    Ok(Verdict { start, carry: Vec::new(), discard })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal::digest::ChunkHasher;
    use crate::wal::record::FileBegin;
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    const MIB: usize = 1 << 20;
    const IDENTITY: &[u8] = b"dev:1 ino:2";
    const MTIME: Option<(i64, u32)> = Some((1_700_000_000, 5));

    fn source_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn digests_of(bytes: &[u8]) -> BTreeMap<u64, Digest> {
        let mut h = ChunkHasher::new();
        let mut map: BTreeMap<u64, Digest> = h.update(bytes).into_iter().collect();
        map.extend(h.finish());
        map
    }

    fn state(len: usize, digests: BTreeMap<u64, Digest>) -> FileState {
        FileState {
            begin: FileBegin {
                file_id: b"f".to_vec(),
                attempt_id: 1,
                destination: b"d".to_vec(),
                source_len: len as u64,
                source_mtime: MTIME,
                source_identity: IDENTITY.to_vec(),
                chunk_size: CHUNK_SIZE,
                valid_through: 0,
            },
            digests,
        }
    }

    fn gate(len: usize) -> Gate<'static> {
        Gate { len: len as u64, mtime: MTIME, identity: IDENTITY }
    }

    fn ok_beat() -> flux_fs::Result<()> {
        Ok(())
    }

    /// Counts the bytes handed out.
    struct Counting<R> {
        inner: R,
        read: usize,
    }

    impl<R: Read> Read for Counting<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read += n;
            Ok(n)
        }
    }

    fn counting(bytes: &[u8]) -> Counting<Cursor<Vec<u8>>> {
        Counting { inner: Cursor::new(bytes.to_vec()), read: 0 }
    }

    /// Runs `decide` over cursors; returns the verdict and the source reader.
    fn run(
        st: &FileState,
        len: usize,
        source: &[u8],
        partial: &[u8],
    ) -> (Verdict, Counting<Cursor<Vec<u8>>>) {
        let mut s = counting(source);
        let mut p = Cursor::new(partial.to_vec());
        let v = decide(st, &gate(len), Some(partial.len() as u64), &mut s, Some(&mut p), &ok_beat)
            .expect("decide");
        (v, s)
    }

    fn start_offset(v: &Verdict) -> usize {
        match v.start {
            Start::Fresh => 0,
            Start::Resume { offset } => offset as usize,
        }
    }

    fn assert_positioned(v: &Verdict, mut s: Counting<Cursor<Vec<u8>>>, source: &[u8]) {
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).unwrap();
        let mut joined = v.carry.clone();
        joined.extend(rest);
        assert_eq!(joined, source[start_offset(v)..]);
    }

    #[test]
    fn a_matching_partial_resumes_at_its_last_whole_chunk() {
        let src = source_bytes(5 * MIB);
        let st = state(src.len(), digests_of(&src));
        let (v, _) = run(&st, src.len(), &src, &src);
        assert_eq!(v.start, Start::Resume { offset: 5 * MIB as u64 });
        assert!(v.carry.is_empty());
        assert_eq!(v.discard, None);
    }

    #[test]
    fn a_partial_longer_than_its_checkpoints_is_cut_to_the_proven_offset() {
        let src = source_bytes(5 * MIB);
        let mut d = digests_of(&src);
        d.retain(|k, _| *k < 3);
        let st = state(src.len(), d);
        let (v, s) = run(&st, src.len(), &src, &src);
        assert_eq!(v.start, Start::Resume { offset: 3 * MIB as u64 });
        assert!(v.carry.is_empty());
        assert_positioned(&v, s, &src);
    }

    #[test]
    fn a_mid_file_mismatch_resumes_at_the_end_of_the_last_matching_chunk() {
        let src = source_bytes(5 * MIB);
        let st = state(src.len(), digests_of(&src));
        let mut partial = src.clone();
        partial[2 * MIB + 7] ^= 1;
        let (v, s) = run(&st, src.len(), &src, &partial);
        assert_eq!(v.start, Start::Resume { offset: 2 * MIB as u64 });
        assert_eq!(v.carry, src[2 * MIB..3 * MIB]);
        assert_eq!(v.discard, Some(Discard::MismatchAt(2 * MIB as u64)));
        assert_positioned(&v, s, &src);
    }

    #[test]
    fn a_chunk_zero_mismatch_is_fresh_but_still_carries_the_consumed_source_bytes() {
        let src = source_bytes(5 * MIB);
        let st = state(src.len(), digests_of(&src));
        let mut partial = src.clone();
        partial[3] ^= 1;
        let (v, s) = run(&st, src.len(), &src, &partial);
        assert_eq!(v.start, Start::Fresh);
        assert_eq!(v.carry, src[..MIB]);
        assert_eq!(v.discard, Some(Discard::MismatchAt(0)));
        assert_eq!(s.read, MIB);
    }

    #[test]
    fn the_source_is_positioned_at_start_plus_carry() {
        let src = source_bytes(5 * MIB);
        let full = digests_of(&src);
        let flipped = |at: usize| {
            let mut p = src.clone();
            p[at] ^= 1;
            p
        };
        let mut three = full.clone();
        three.retain(|k, _| *k < 3);
        let cases: Vec<(FileState, Vec<u8>)> = vec![
            (state(src.len(), full.clone()), flipped(0)),
            (state(src.len(), full.clone()), flipped(2 * MIB)),
            (state(src.len(), full.clone()), src.clone()),
            (state(src.len(), full.clone()), src[..2 * MIB + 5].to_vec()),
            (state(src.len(), three), src.clone()),
        ];
        for (st, partial) in cases {
            let (v, s) = run(&st, src.len(), &src, &partial);
            assert_positioned(&v, s, &src);
        }
    }

    #[test]
    fn a_source_mismatch_is_detected_even_when_the_partial_matches() {
        let src = source_bytes(3 * MIB);
        let mut d = digests_of(&src);
        // Chunk 1's recorded digest comes from other bytes; the partial is a copy of the source, so
        // neither side matches it, and the source side alone must be enough to fail.
        d.insert(1, hash(&vec![9u8; MIB]));
        let st = state(src.len(), d);
        let (v, _) = run(&st, src.len(), &src, &src);
        assert_eq!(v.start, Start::Resume { offset: MIB as u64 });
        assert_eq!(v.discard, Some(Discard::MismatchAt(MIB as u64)));
        // The same, with the partial being the bytes that match the record while the source differs.
        let mut d = digests_of(&src);
        let other = vec![9u8; MIB];
        d.insert(1, hash(&other));
        let mut partial = src.clone();
        partial[MIB..2 * MIB].copy_from_slice(&other);
        let st = state(src.len(), d);
        let (v, _) = run(&st, src.len(), &src, &partial);
        assert_eq!(v.start, Start::Resume { offset: MIB as u64 });
        assert_eq!(v.carry, src[MIB..2 * MIB]);
        assert_eq!(v.discard, Some(Discard::MismatchAt(MIB as u64)));
    }

    #[test]
    fn a_changed_source_len_mtime_or_identity_is_fresh_with_its_reason() {
        let src = source_bytes(2 * MIB);
        let st = state(src.len(), digests_of(&src));
        let cases = [
            (Gate { len: src.len() as u64 + 1, mtime: MTIME, identity: IDENTITY }, "length"),
            (
                Gate { len: src.len() as u64, mtime: Some((1, 1)), identity: IDENTITY },
                "modified time",
            ),
            (Gate { len: src.len() as u64, mtime: MTIME, identity: b"other" }, "identity"),
        ];
        for (g, why) in cases {
            let mut s = counting(&src);
            let mut p = Cursor::new(src.clone());
            let v =
                decide(&st, &g, Some(src.len() as u64), &mut s, Some(&mut p), &ok_beat).unwrap();
            assert_eq!(v.start, Start::Fresh);
            assert!(v.carry.is_empty());
            assert_eq!(v.discard, Some(Discard::SourceChanged(why)));
            assert_eq!(s.read, 0);
        }
    }

    #[test]
    fn a_missing_partial_is_fresh_and_reads_nothing() {
        let src = source_bytes(2 * MIB);
        let st = state(src.len(), digests_of(&src));
        let mut s = counting(&src);
        let v = decide::<_, Cursor<Vec<u8>>>(&st, &gate(src.len()), None, &mut s, None, &ok_beat)
            .unwrap();
        assert_eq!(v.start, Start::Fresh);
        assert!(v.carry.is_empty());
        assert_eq!(v.discard, Some(Discard::PartialMissing));
        assert_eq!(s.read, 0);
    }

    #[test]
    fn a_shorter_partial_keeps_the_chunks_that_fit() {
        let src = source_bytes(5 * MIB);
        let st = state(src.len(), digests_of(&src));
        let (v, s) = run(&st, src.len(), &src, &src[..2 * MIB + 100]);
        assert_eq!(v.start, Start::Resume { offset: 2 * MIB as u64 });
        assert!(v.carry.is_empty());
        assert_eq!(v.discard, Some(Discard::PartialShorter { kept: 2 * MIB as u64 }));
        assert_positioned(&v, s, &src);
        // A partial that ends exactly on a chunk boundary short of the highest checkpoint.
        let (v, _) = run(&st, src.len(), &src, &src[..2 * MIB]);
        assert_eq!(v.start, Start::Resume { offset: 2 * MIB as u64 });
        assert_eq!(v.discard, Some(Discard::PartialShorter { kept: 2 * MIB as u64 }));
        // No whole chunk at all.
        let (v, _) = run(&st, src.len(), &src, &src[..MIB - 1]);
        assert_eq!(v.start, Start::Fresh);
        assert_eq!(v.discard, Some(Discard::PartialShorter { kept: 0 }));
    }

    #[test]
    fn a_short_last_chunk_validates_only_when_the_partial_reaches_the_source_length() {
        let src = source_bytes(2 * MIB + 1000);
        let st = state(src.len(), digests_of(&src));
        let (v, _) = run(&st, src.len(), &src, &src);
        assert_eq!(v.start, Start::Resume { offset: src.len() as u64 });
        assert_eq!(v.discard, None);
        let (v, _) = run(&st, src.len(), &src, &src[..2 * MIB + 999]);
        assert_eq!(v.start, Start::Resume { offset: 2 * MIB as u64 });
        assert_eq!(v.discard, Some(Discard::PartialShorter { kept: 2 * MIB as u64 }));
    }

    #[test]
    fn the_heartbeat_runs_once_per_chunk_and_its_failure_aborts() {
        let src = source_bytes(5 * MIB);
        let st = state(src.len(), digests_of(&src));
        let calls = Cell::new(0u32);
        let counted = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let mut s = counting(&src);
        let mut p = Cursor::new(src.clone());
        decide(&st, &gate(src.len()), Some(src.len() as u64), &mut s, Some(&mut p), &counted)
            .unwrap();
        assert_eq!(calls.get(), 5);

        calls.set(0);
        let failing = || {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                Err(FsError::from_io(io::Error::other("lease lost")))
            } else {
                Ok(())
            }
        };
        let mut s = counting(&src);
        let mut p = Cursor::new(src.clone());
        let r =
            decide(&st, &gate(src.len()), Some(src.len() as u64), &mut s, Some(&mut p), &failing);
        assert!(r.is_err());
        assert_eq!(calls.get(), 3);
    }

    /// Delivers `good` bytes of `inner`, then fails.
    struct FailAfter {
        inner: Cursor<Vec<u8>>,
        good: usize,
    }

    impl Read for FailAfter {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let pos = self.inner.position() as usize;
            if pos >= self.good {
                return Err(io::Error::other("boom"));
            }
            let room = (self.good - pos).min(buf.len());
            self.inner.read(&mut buf[..room])
        }
    }

    #[test]
    fn a_partial_read_error_stops_validation_at_that_chunk() {
        let src = source_bytes(4 * MIB);
        let st = state(src.len(), digests_of(&src));
        let mut s = counting(&src);
        let mut p = FailAfter { inner: Cursor::new(src.clone()), good: 2 * MIB + 10 };
        let v =
            decide(&st, &gate(src.len()), Some(src.len() as u64), &mut s, Some(&mut p), &ok_beat)
                .unwrap();
        assert_eq!(v.start, Start::Resume { offset: 2 * MIB as u64 });
        assert_eq!(v.carry, src[2 * MIB..3 * MIB]);
        assert_eq!(v.discard, Some(Discard::MismatchAt(2 * MIB as u64)));
    }

    /// `chunks` MiB of one repeating 1 MiB pattern: every chunk has the same bytes, so an off-by-one-chunk
    /// source position cannot be told apart by content.
    fn identical_chunks(chunks: usize) -> Vec<u8> {
        let one = source_bytes(MIB);
        one.iter().copied().cycle().take(chunks * MIB).collect()
    }

    #[test]
    fn a_hole_in_the_digest_map_ends_validation_at_the_hole() {
        let src = identical_chunks(4);
        let full = digests_of(&src);
        // Case A: chunks {0, 2} recorded; chunk 1 is a hole.
        let mut d = full.clone();
        d.retain(|k, _| *k == 0 || *k == 2);
        let (v, s) = run(&state(src.len(), d), src.len(), &src, &src);
        assert_eq!(v.start, Start::Resume { offset: MIB as u64 });
        assert!(v.carry.is_empty());
        assert_eq!(s.read, MIB);
        assert_positioned(&v, s, &src);
        // Case B: only chunk 1 recorded; chunk 0 is a hole.
        let mut d = full;
        d.retain(|k, _| *k == 1);
        let (v, s) = run(&state(src.len(), d), src.len(), &src, &src);
        assert_eq!(v.start, Start::Fresh);
        assert!(v.carry.is_empty());
        assert_eq!(s.read, 0);
        assert_positioned(&v, s, &src);
    }

    #[test]
    fn a_source_that_ends_inside_a_chunk_is_a_mismatch_with_the_short_carry() {
        let src = identical_chunks(3);
        let st = state(src.len(), digests_of(&src));
        let short = &src[..2 * MIB + MIB / 2];
        let mut s = counting(short);
        let mut p = Cursor::new(src.clone());
        let v =
            decide(&st, &gate(src.len()), Some(src.len() as u64), &mut s, Some(&mut p), &ok_beat)
                .unwrap();
        assert_eq!(v.start, Start::Resume { offset: 2 * MIB as u64 });
        assert_eq!(v.discard, Some(Discard::MismatchAt(2 * MIB as u64)));
        assert_eq!(v.carry.len(), MIB / 2);
        assert_eq!(v.carry, src[2 * MIB..2 * MIB + MIB / 2]);
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty());
    }

    #[test]
    fn a_digest_key_beyond_the_source_is_ignored_and_does_not_underflow() {
        let src = source_bytes(3 * MIB);
        let mut d = digests_of(&src);
        d.insert(7, [7u8; 32]);
        let st = state(src.len(), d);
        let (v, s) = run(&st, src.len(), &src, &src);
        assert_eq!(v.start, Start::Resume { offset: 3 * MIB as u64 });
        assert!(v.carry.is_empty());
        assert_positioned(&v, s, &src);
        // A forged key just past the source, with a partial long enough to reach it.
        let mut d = digests_of(&src);
        d.insert(3, [3u8; 32]);
        let mut long = src.clone();
        long.extend(vec![1u8; MIB]);
        let (v, s) = run(&state(src.len(), d), src.len(), &src, &long);
        assert_eq!(v.start, Start::Resume { offset: 3 * MIB as u64 });
        assert!(v.carry.is_empty());
        assert_eq!(v.discard, None);
        assert_positioned(&v, s, &src);
    }
}
