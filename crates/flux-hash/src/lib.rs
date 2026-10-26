// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Streaming hashing (spec §3.4). MVP algorithm: BLAKE3.

use std::io::Read;

/// An incremental BLAKE3 hasher.
#[derive(Default)]
pub struct Hasher(blake3::Hasher);

impl Hasher {
    #[must_use]
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The digest of everything fed so far; the hasher stays usable.
    #[must_use]
    pub fn finalize(&self) -> Digest {
        Digest(*self.0.finalize().as_bytes())
    }
}

/// A 32-byte BLAKE3 digest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Digest([u8; 32]);

impl Digest {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        use std::fmt::Write;
        self.0.iter().fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
    }
}

/// Reads `reader` to EOF in `buf`-sized reads, calling `each()` after every successful read (the caller's heartbeat; an
/// `Err` from it stops the read and is returned). Returns the digest and the byte count.
pub fn hash_reader<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    each: &mut dyn FnMut() -> std::io::Result<()>,
) -> std::io::Result<(Digest, u64)> {
    let mut hasher = Hasher::new();
    let mut total = 0u64;
    loop {
        let n = match reader.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        total += n as u64;
        each()?;
    }
    Ok((hasher.finalize(), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Error, ErrorKind, Read};

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn empty_input_has_the_known_blake3_digest() {
        assert_eq!(
            Hasher::new().finalize().to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn incremental_equals_one_shot() {
        for len in [0usize, 1, 1023, 1024, 1025, 300_000] {
            let input = pattern(len);
            let want = *blake3::hash(&input).as_bytes();
            for split in [0, 1, len / 3, len / 2, len.saturating_sub(1), len] {
                let split = split.min(len);
                let mut hasher = Hasher::new();
                hasher.update(&input[..split]);
                hasher.update(&input[split..]);
                assert_eq!(hasher.finalize().as_bytes(), &want, "len {len} split {split}");
            }
        }
    }

    #[test]
    fn hash_reader_counts_bytes_and_matches_one_shot() {
        let input = pattern(300_000);
        let mut buf = vec![0u8; 4096];
        let mut calls = 0usize;
        let (digest, count) = hash_reader(&mut Cursor::new(&input), &mut buf, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 300_000);
        assert_eq!(digest.as_bytes(), blake3::hash(&input).as_bytes());
        assert_eq!(calls, 300_000usize.div_ceil(4096));
    }

    struct CountingReader {
        inner: Cursor<Vec<u8>>,
        reads: usize,
    }

    impl Read for CountingReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            self.inner.read(out)
        }
    }

    #[test]
    fn hash_reader_stops_when_the_callback_fails() {
        let mut reader = CountingReader { inner: Cursor::new(pattern(300_000)), reads: 0 };
        let mut buf = vec![0u8; 4096];
        let mut calls = 0usize;
        let err = hash_reader(&mut reader, &mut buf, &mut || {
            calls += 1;
            if calls == 2 { Err(Error::other("stop")) } else { Ok(()) }
        })
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
        assert_eq!(reader.reads, 2);
    }

    #[test]
    fn a_different_byte_gives_a_different_digest() {
        let input = pattern(10_000);
        let mut flipped = input.clone();
        flipped[5_000] ^= 1;
        let digest = |bytes: &[u8]| {
            let mut hasher = Hasher::new();
            hasher.update(bytes);
            hasher.finalize()
        };
        assert_ne!(digest(&input), digest(&flipped));
    }
}
