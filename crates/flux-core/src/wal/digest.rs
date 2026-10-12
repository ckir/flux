//! Per-chunk BLAKE3 digests from arbitrary-sized writes.

use super::{CHUNK_SIZE, Digest};

/// Feeds bytes of any size and yields one BLAKE3 digest per completed
/// `CHUNK_SIZE` chunk; the digest covers exactly that chunk's bytes.
pub(crate) struct ChunkHasher {
    hasher: blake3::Hasher,
    in_chunk: u64,
    index: u64,
}

impl ChunkHasher {
    pub(crate) fn new() -> Self {
        Self { hasher: blake3::Hasher::new(), in_chunk: 0, index: 0 }
    }

    /// The `(chunk_index, digest)` pairs completed by this call, in order.
    pub(crate) fn update(&mut self, mut bytes: &[u8]) -> Vec<(u64, Digest)> {
        let mut done = Vec::new();
        while !bytes.is_empty() {
            let room = usize::try_from(CHUNK_SIZE - self.in_chunk).unwrap_or(usize::MAX);
            let take = room.min(bytes.len());
            self.hasher.update(&bytes[..take]);
            self.in_chunk += take as u64;
            bytes = &bytes[take..];
            if self.in_chunk == CHUNK_SIZE {
                done.push((self.index, *self.hasher.finalize().as_bytes()));
                self.hasher.reset();
                self.in_chunk = 0;
                self.index += 1;
            }
        }
        done
    }

    /// The final short chunk, if any bytes are pending.
    pub(crate) fn finish(self) -> Option<(u64, Digest)> {
        (self.in_chunk > 0).then(|| (self.index, *self.hasher.finalize().as_bytes()))
    }

    /// Total bytes fed so far.
    pub(crate) fn bytes_seen(&self) -> u64 {
        self.index * CHUNK_SIZE + self.in_chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1 << 20;

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn b3(bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }

    #[test]
    fn one_mib_yields_one_digest_equal_to_blake3_of_it() {
        let d = data(MIB);
        let mut h = ChunkHasher::new();
        assert_eq!(h.update(&d), vec![(0, b3(&d))]);
        assert_eq!(h.bytes_seen(), MIB as u64);
        assert_eq!(h.finish(), None);
    }

    #[test]
    fn four_256_kib_updates_complete_a_chunk_on_the_fourth() {
        let d = data(MIB);
        let mut h = ChunkHasher::new();
        for part in d.chunks(MIB / 4).take(3) {
            assert!(h.update(part).is_empty());
        }
        assert_eq!(h.update(&d[3 * MIB / 4..]), vec![(0, b3(&d))]);
        assert_eq!(h.finish(), None);
    }

    #[test]
    fn a_short_tail_is_finished_separately() {
        let d = data(MIB + 10);
        let mut h = ChunkHasher::new();
        assert_eq!(h.update(&d), vec![(0, b3(&d[..MIB]))]);
        assert_eq!(h.bytes_seen(), (MIB + 10) as u64);
        assert_eq!(h.finish(), Some((1, b3(&d[MIB..]))));
    }

    #[test]
    fn an_exact_multiple_has_no_tail() {
        let d = data(3 * MIB);
        let mut h = ChunkHasher::new();
        let got = h.update(&d);
        assert_eq!(
            got,
            vec![(0, b3(&d[..MIB])), (1, b3(&d[MIB..2 * MIB])), (2, b3(&d[2 * MIB..]))]
        );
        assert_eq!(h.finish(), None);
    }

    #[test]
    fn an_update_spanning_a_boundary_splits_at_it() {
        let d = data(MIB + MIB / 2);
        let mut h = ChunkHasher::new();
        assert_eq!(h.update(&d), vec![(0, b3(&d[..MIB]))]);
        assert_eq!(h.finish(), Some((1, b3(&d[MIB..]))));
    }

    #[test]
    fn two_chunks_in_one_update() {
        let d = data(2 * MIB + 1);
        let mut h = ChunkHasher::new();
        assert_eq!(h.update(&d), vec![(0, b3(&d[..MIB])), (1, b3(&d[MIB..2 * MIB]))]);
        assert_eq!(h.finish(), Some((2, b3(&d[2 * MIB..]))));
    }
}
