// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Sorted runs of directory entries in the scratch space, and their k-way merge (spec 6.2).
//!
//! A record is `u16` LE name length, `u8` file type (0 File, 1 Dir, 2 Symlink, 3 Other), then the name's
//! `as_encoded_bytes()`. The `ino` hint is not stored: a decoded entry carries `None`.
// Nothing outside this module uses the crate-private surface until the walk is built on it. Remove in Task 4.
#![allow(dead_code)]

use crate::scratch::{Region, ScratchSpace};
use flux_fs::{Code, DirEntry, FileType, FsError};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::ffi::OsString;

fn fatal(message: impl Into<String>) -> FsError {
    FsError::new(Code::IoError, std::io::Error::other(message.into()))
}

/// Appends the record of `entry` to `out`. A name longer than `u16::MAX` bytes is an error and leaves `out` as it was.
pub(crate) fn encode(entry: &DirEntry, out: &mut Vec<u8>) -> Result<(), FsError> {
    let name = entry.name.as_encoded_bytes();
    let Ok(len) = u16::try_from(name.len()) else {
        return Err(fatal(format!(
            "a directory entry name of {} bytes is too long to spill (limit {})",
            name.len(),
            u16::MAX
        )));
    };
    out.extend_from_slice(&len.to_le_bytes());
    out.push(match entry.file_type {
        FileType::File => 0,
        FileType::Dir => 1,
        FileType::Symlink => 2,
        FileType::Other => 3,
    });
    out.extend_from_slice(name);
    Ok(())
}

/// The first record of `bytes` and its length, with `ino: None`; `None` on a short buffer or an unknown type byte.
pub(crate) fn decode(bytes: &[u8]) -> Option<(DirEntry, usize)> {
    let header = bytes.get(..3)?;
    let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let file_type = match header[2] {
        0 => FileType::File,
        1 => FileType::Dir,
        2 => FileType::Symlink,
        3 => FileType::Other,
        _ => return None,
    };
    let name = bytes.get(3..3 + len)?;
    Some((DirEntry { name: os_name(name), file_type, ino: None }, 3 + len))
}

#[cfg(unix)]
fn os_name(bytes: &[u8]) -> OsString {
    std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
}

#[cfg(windows)]
fn os_name(bytes: &[u8]) -> OsString {
    std::os::windows::ffi::OsStringExt::from_wide(&wtf8_to_wide(bytes))
}

#[cfg(not(any(unix, windows)))]
fn os_name(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Decodes generalized UTF-8 (WTF-8) into UTF-16 units. A 3-byte sequence for a surrogate (`ED A0 80`..`ED BF BF`)
/// is one unit; a supplementary scalar is a surrogate pair. Input comes from `OsStr::as_encoded_bytes`, so it is
/// well formed; a malformed or truncated byte becomes U+FFFD and decoding resumes at the next byte (no overlong
/// check).
pub(crate) fn wtf8_to_wide(bytes: &[u8]) -> Vec<u16> {
    let cont = |i: usize| bytes.get(i).is_some_and(|b| b & 0xC0 == 0x80);
    let c = |i: usize| u32::from(bytes[i] & 0x3F);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = u32::from(bytes[i]);
        let (cp, n) = match bytes[i] {
            0x00..=0x7F => (b, 1),
            0xC0..=0xDF if cont(i + 1) => (((b & 0x1F) << 6) | c(i + 1), 2),
            0xE0..=0xEF if cont(i + 1) && cont(i + 2) => {
                (((b & 0x0F) << 12) | (c(i + 1) << 6) | c(i + 2), 3)
            }
            0xF0..=0xF4 if cont(i + 1) && cont(i + 2) && cont(i + 3) => {
                let cp = ((b & 0x07) << 18) | (c(i + 1) << 12) | (c(i + 2) << 6) | c(i + 3);
                if cp > 0x10FFFF { (0xFFFD, 1) } else { (cp, 4) }
            }
            _ => (0xFFFD, 1),
        };
        if cp >= 0x10000 {
            let v = cp - 0x10000;
            out.push((0xD800 + (v >> 10)) as u16);
            out.push((0xDC00 + (v & 0x3FF)) as u16);
        } else {
            out.push(cp as u16);
        }
        i += n;
    }
    out
}

/// The first eight bytes, big-endian, zero padded.
fn prefix(bytes: &[u8]) -> u64 {
    let mut p = [0u8; 8];
    let n = bytes.len().min(8);
    p[..n].copy_from_slice(&bytes[..n]);
    u64::from_be_bytes(p)
}

/// Equal to `a.cmp(b)`: the prefix as a `u64` decides most pairs without touching the rest.
pub(crate) fn key_cmp(a: &[u8], b: &[u8]) -> Ordering {
    prefix(a).cmp(&prefix(b)).then_with(|| a.cmp(b))
}

/// Sorts by name bytes, comparing cached `u64` prefixes before the bytes (no two names are equal, so the order of
/// equals does not matter).
pub(crate) fn sort_entries(v: &mut [DirEntry]) {
    let mut keys: Vec<(u64, u32)> = Vec::with_capacity(v.len());
    for (i, e) in v.iter().enumerate() {
        let Ok(i) = u32::try_from(i) else {
            // More entries than an index holds: sort directly.
            v.sort_unstable_by(|a, b| {
                key_cmp(a.name.as_encoded_bytes(), b.name.as_encoded_bytes())
            });
            return;
        };
        keys.push((prefix(e.name.as_encoded_bytes()), i));
    }
    keys.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            v[a.1 as usize].name.as_encoded_bytes().cmp(v[b.1 as usize].name.as_encoded_bytes())
        })
    });
    // Apply the permutation in place (the method of `slice::sort_by_cached_key`).
    for i in 0..v.len() {
        let mut index = keys[i].1 as usize;
        while index < i {
            index = keys[index].1 as usize;
        }
        keys[i].1 = index as u32;
        v.swap(i, index);
    }
}

/// Where a sorted run lies in the scratch file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunSpan {
    pub start: u64,
    pub len: u64,
    pub count: u64,
}

/// Appends one run at the end of the file through a buffer flushed as it fills.
struct RunWriter<'s, 'h> {
    space: &'s ScratchSpace<'h>,
    start: u64,
    offset: u64,
    buf: Vec<u8>,
    cap: usize,
    count: u64,
}

impl<'s, 'h> RunWriter<'s, 'h> {
    fn new(space: &'s ScratchSpace<'h>) -> Self {
        let start = space.len();
        let cap = space.thresholds().merge_buffer.saturating_mul(4);
        RunWriter { space, start, offset: start, buf: Vec::new(), cap, count: 0 }
    }

    fn flush(&mut self, n: usize) -> Result<(), FsError> {
        if n == 0 {
            return Ok(());
        }
        self.space.write_at(self.offset, &self.buf[..n])?;
        self.offset += n as u64;
        self.buf.drain(..n);
        Ok(())
    }

    fn push(&mut self, entry: &DirEntry) -> Result<(), FsError> {
        let mark = self.buf.len();
        encode(entry, &mut self.buf)?;
        self.count += 1;
        if mark > 0 && self.buf.len() > self.cap {
            self.flush(mark)?;
        }
        if self.buf.len() >= self.cap {
            self.flush(self.buf.len())?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<RunSpan, FsError> {
        self.flush(self.buf.len())?;
        Ok(RunSpan { start: self.start, len: self.offset - self.start, count: self.count })
    }
}

/// Appends the (already sorted) `entries` as one run at the end of the file.
pub(crate) fn write_run(space: &ScratchSpace, entries: &[DirEntry]) -> Result<RunSpan, FsError> {
    let mut w = RunWriter::new(space);
    for e in entries {
        w.push(e)?;
    }
    w.finish()
}

/// The read position in one run.
struct RunCursor {
    next_off: u64,
    end: u64,
    buf: Vec<u8>,
    pos: usize,
}

impl RunCursor {
    fn new(span: RunSpan) -> Self {
        RunCursor { next_off: span.start, end: span.start + span.len, buf: Vec::new(), pos: 0 }
    }

    /// The next entry of the run, refilling by `chunk` bytes (a record longer than that grows the buffer).
    fn next_entry(
        &mut self,
        space: &ScratchSpace,
        chunk: usize,
    ) -> Result<Option<DirEntry>, FsError> {
        loop {
            if let Some((entry, n)) = decode(&self.buf[self.pos..]) {
                self.pos += n;
                return Ok(Some(entry));
            }
            if self.next_off >= self.end {
                if self.pos < self.buf.len() {
                    return Err(fatal("a scratch run holds a corrupt record"));
                }
                return Ok(None);
            }
            self.buf.drain(..self.pos);
            self.pos = 0;
            let want = (self.end - self.next_off).min(chunk as u64) as usize;
            let old = self.buf.len();
            self.buf.resize(old + want, 0);
            let got = space.read_at(self.next_off, &mut self.buf[old..])?;
            if got < want {
                return Err(fatal("a scratch run ends before its recorded length"));
            }
            self.next_off += want as u64;
        }
    }
}

/// A run's current head in the merge heap; `BinaryHeap` is a max-heap, so the order is reversed.
struct Head {
    entry: DirEntry,
    run: usize,
}

impl Ord for Head {
    fn cmp(&self, other: &Self) -> Ordering {
        key_cmp(other.entry.name.as_encoded_bytes(), self.entry.name.as_encoded_bytes())
            .then_with(|| other.run.cmp(&self.run))
    }
}

impl PartialOrd for Head {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Head {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Head {}

/// The merge itself, with no region of its own (so `cascade` can append inside its caller's).
struct Merger<'s, 'h> {
    space: &'s ScratchSpace<'h>,
    cursors: Vec<RunCursor>,
    heap: BinaryHeap<Head>,
    chunk: usize,
}

impl<'s, 'h> Merger<'s, 'h> {
    fn new(space: &'s ScratchSpace<'h>, runs: &[RunSpan]) -> Result<Self, FsError> {
        let chunk = space.thresholds().merge_buffer;
        let mut cursors: Vec<RunCursor> = runs.iter().map(|r| RunCursor::new(*r)).collect();
        let mut heap = BinaryHeap::with_capacity(cursors.len());
        for (run, cursor) in cursors.iter_mut().enumerate() {
            if let Some(entry) = cursor.next_entry(space, chunk)? {
                heap.push(Head { entry, run });
            }
        }
        Ok(Merger { space, cursors, heap, chunk })
    }

    fn next_entry(&mut self) -> Result<Option<DirEntry>, FsError> {
        let Some(head) = self.heap.pop() else {
            return Ok(None);
        };
        if let Some(entry) = self.cursors[head.run].next_entry(self.space, self.chunk)? {
            self.heap.push(Head { entry, run: head.run });
        }
        Ok(Some(head.entry))
    }
}

/// The sorted union of `runs`. It owns the directory's `Region`, so dropping it truncates the runs. After an error
/// the iterator ends.
pub(crate) struct MergeReader<'s, 'h> {
    merger: Merger<'s, 'h>,
    done: bool,
    // Dropped last: the file is truncated only after the merger is gone.
    _region: Region<'s, 'h>,
}

impl<'s, 'h> MergeReader<'s, 'h> {
    pub(crate) fn new(
        space: &'s ScratchSpace<'h>,
        runs: Vec<RunSpan>,
        region: Region<'s, 'h>,
    ) -> Result<Self, FsError> {
        let merger = Merger::new(space, &runs)?;
        Ok(MergeReader { merger, done: false, _region: region })
    }
}

impl Iterator for MergeReader<'_, '_> {
    type Item = Result<DirEntry, FsError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.merger.next_entry() {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// Merges consecutive groups of `merge_fan_in` runs into new runs appended at the end of the file (the inputs stay
/// until the caller's region drops) while more than `merge_fan_in` remain. A lone run at the end of a pass is
/// carried over as it is. `tick` gets the entries merged so far every `tick_entries` entries and once at the end
/// of each pass.
pub(crate) fn cascade(
    space: &ScratchSpace,
    mut runs: Vec<RunSpan>,
    tick: &mut dyn FnMut(u64) -> Result<(), FsError>,
) -> Result<Vec<RunSpan>, FsError> {
    let t = space.thresholds();
    let fan = t.merge_fan_in;
    let mut passes = 0u32;
    let mut n = runs.len();
    while n > fan {
        n = n.div_ceil(fan);
        passes += 1;
    }
    let mut merged = 0u64;
    let mut pass = 0u32;
    while runs.len() > fan {
        pass += 1;
        let mut next = Vec::with_capacity(runs.len().div_ceil(fan));
        let mut bytes = 0u64;
        for group in runs.chunks(fan) {
            if let [only] = group {
                next.push(*only);
                continue;
            }
            let mut merger = Merger::new(space, group)?;
            let mut writer = RunWriter::new(space);
            while let Some(entry) = merger.next_entry()? {
                writer.push(&entry)?;
                merged += 1;
                if t.tick_entries > 0 && merged.is_multiple_of(t.tick_entries as u64) {
                    tick(merged)?;
                }
            }
            let span = writer.finish()?;
            bytes += span.len;
            next.push(span);
        }
        tracing::debug!(pass, passes, bytes, "merge pass");
        tick(merged)?;
        runs = next;
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use crate::scratch::Thresholds;
    use flux_fs::{Code, DestinationRoot, FileSystem};
    use proptest::prelude::*;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    fn placed(fs: &FaultFs, t: Thresholds) -> ScratchSpace<'static> {
        fs.create_dir(Path::new("/s")).unwrap();
        let home = fs.destination_root(Path::new("/s")).unwrap();
        let space = ScratchSpace::unplaced(t, None);
        space.place(home, PathBuf::from("/s"));
        space
    }

    fn entry(name: OsString, file_type: FileType) -> DirEntry {
        DirEntry { name, file_type, ino: None }
    }

    fn named(n: usize) -> DirEntry {
        entry(OsString::from(format!("n{n:05}")), FileType::File)
    }

    const TYPES: [FileType; 4] =
        [FileType::File, FileType::Dir, FileType::Symlink, FileType::Other];

    #[test]
    fn a_record_round_trips() {
        let mut names: Vec<OsString> = vec![OsString::from("a"), OsString::from("x".repeat(255))];
        #[cfg(unix)]
        names.push(std::os::unix::ffi::OsStringExt::from_vec(vec![0x80, 0xFF, b'a']));
        #[cfg(windows)]
        names.push(std::os::windows::ffi::OsStringExt::from_wide(&[0xD800, 0xD83D, 0xDE00, 0x61]));
        for ft in TYPES {
            for name in &names {
                let e = entry(name.clone(), ft);
                let mut buf = Vec::new();
                encode(&e, &mut buf).unwrap();
                assert_eq!(buf.len(), 3 + name.as_encoded_bytes().len());
                assert_eq!(decode(&buf), Some((e, buf.len())));
                assert_eq!(decode(&buf[..buf.len() - 1]), None, "a short buffer");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_name_longer_than_u16_is_an_error_not_a_panic() {
        let name: OsString = std::os::unix::ffi::OsStringExt::from_vec(vec![b'a'; 65_536]);
        let mut buf = Vec::new();
        assert!(encode(&entry(name, FileType::File), &mut buf).is_err());
        assert!(buf.is_empty(), "nothing is written for a refused name");
        let ok: OsString = std::os::unix::ffi::OsStringExt::from_vec(vec![b'a'; 65_535]);
        assert!(encode(&entry(ok, FileType::File), &mut buf).is_ok());
    }

    #[test]
    fn wtf8_decodes_ascii_bmp_supplementary_and_lone_surrogates() {
        assert_eq!(wtf8_to_wide("a".as_bytes()), [0x61]);
        assert_eq!(wtf8_to_wide("é".as_bytes()), [0xE9]);
        assert_eq!(wtf8_to_wide("😀".as_bytes()), [0xD83D, 0xDE00]);
        assert_eq!(wtf8_to_wide(&[0xED, 0xA0, 0x80]), [0xD800]);
        assert_eq!(wtf8_to_wide(&[0xED, 0xBF, 0xBF]), [0xDFFF]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]
        #[test]
        fn key_cmp_agrees_with_byte_order(
            a in proptest::collection::vec(prop::sample::select(vec![0x00u8, 0x61, 0x62, 0xFF]), 0..=20),
            b in proptest::collection::vec(prop::sample::select(vec![0x00u8, 0x61, 0x62, 0xFF]), 0..=20),
        ) {
            prop_assert_eq!(key_cmp(&a, &b), a.cmp(&b));
        }
    }

    #[cfg(unix)]
    fn name_set() -> impl Strategy<Value = Vec<OsString>> {
        let name = proptest::collection::vec(1u8..=255, 1..12).prop_map(|v| {
            v.into_iter().map(|b| if b == b'/' { b'a' } else { b }).collect::<Vec<u8>>()
        });
        proptest::collection::btree_set(name, 0..300)
            .prop_map(|s| s.into_iter().map(std::os::unix::ffi::OsStringExt::from_vec).collect())
    }

    #[cfg(windows)]
    fn name_set() -> impl Strategy<Value = Vec<OsString>> {
        proptest::collection::btree_set("[^\\\\/\\x00]{1,12}", 0..300)
            .prop_map(|s| s.into_iter().map(OsString::from).collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn a_spilled_sequence_equals_sorted(names in name_set()) {
            let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let entries: Vec<DirEntry> = names
                .into_iter()
                .enumerate()
                .map(|(i, n)| entry(n, TYPES[i % 4]))
                .collect();
            let mut expected = entries.clone();
            expected.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));

            let region = space.begin(space.next_owner()).unwrap();
            let mut runs = Vec::new();
            for chunk in entries.chunks(t.run_entries) {
                let mut c = chunk.to_vec();
                sort_entries(&mut c);
                runs.push(write_run(&space, &c).unwrap());
            }
            let runs = cascade(&space, runs, &mut |_| Ok(())).unwrap();
            prop_assert!(runs.len() <= t.merge_fan_in);
            let got: Vec<DirEntry> = MergeReader::new(&space, runs, region)
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            prop_assert_eq!(got, expected);
        }
    }

    #[test]
    fn a_cascade_makes_the_expected_number_of_passes() {
        let t = Thresholds::new(4, 4, 2, 16, 1_000_000).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let region = space.begin(space.next_owner()).unwrap();
        let mut runs = Vec::new();
        for c in 0..25 {
            let mut chunk: Vec<DirEntry> = (0..4).map(|i| named(i * 25 + c)).collect();
            sort_entries(&mut chunk);
            runs.push(write_run(&space, &chunk).unwrap());
        }
        assert_eq!(runs.len(), 25);
        let mut ticks = 0;
        let runs = cascade(&space, runs, &mut |_| {
            ticks += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(ticks, 4, "25 -> 13 -> 7 -> 4 -> 2 is four passes");
        let names: Vec<OsString> =
            MergeReader::new(&space, runs, region).unwrap().map(|e| e.unwrap().name).collect();
        let want: Vec<OsString> = (0..100).map(|i| named(i).name).collect();
        assert_eq!(names, want);
    }

    #[test]
    fn dropping_a_reader_truncates_its_runs() {
        let t = Thresholds::new(4, 4, 2, 16, 3).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let owner = space.next_owner();
        let outer = space.begin(owner).unwrap();
        space.write_at(outer.start(), &[7u8; 10]).unwrap();
        let region = space.begin(owner).unwrap();
        let start = region.start();
        assert_eq!(start, 10);
        let runs: Vec<RunSpan> = (0..3)
            .map(|r| {
                let c: Vec<DirEntry> = (0..4).map(|i| named(i * 3 + r)).collect();
                write_run(&space, &c).unwrap()
            })
            .collect();
        assert!(space.len() > start);
        let reader = MergeReader::new(&space, runs, region).unwrap();
        drop(reader);
        assert_eq!(space.len(), start);
        drop(outer);
        assert_eq!(space.len(), 0);
    }

    #[test]
    fn a_read_error_mid_merge_is_returned_and_the_reader_ends() {
        let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let region = space.begin(space.next_owner()).unwrap();
        let runs: Vec<RunSpan> = (0..3)
            .map(|r| {
                let c: Vec<DirEntry> = (0..8).map(|i| named(i * 3 + r)).collect();
                write_run(&space, &c).unwrap()
            })
            .collect();
        let mut reader = MergeReader::new(&space, runs, region).unwrap();
        space.fail_reads_after(0);
        let mut items = Vec::new();
        for _ in 0..40 {
            match reader.next() {
                Some(r) => items.push(r),
                None => break,
            }
        }
        assert_eq!(items.iter().filter(|r| r.is_err()).count(), 1, "exactly one error");
        assert!(items.last().unwrap().is_err(), "the error ends the sequence");
        assert!(reader.next().is_none());
        assert!(reader.next().is_none());
    }

    /// Sorts, writes, cascades and merges `runs`, each one run of the file; the merged entries.
    fn spill_and_merge(t: Thresholds, runs: Vec<Vec<DirEntry>>) -> Vec<DirEntry> {
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let region = space.begin(space.next_owner()).unwrap();
        let mut spans = Vec::new();
        for mut run in runs {
            sort_entries(&mut run);
            spans.push(write_run(&space, &run).unwrap());
        }
        let spans = cascade(&space, spans, &mut |_| Ok(())).unwrap();
        assert!(spans.len() <= t.merge_fan_in);
        MergeReader::new(&space, spans, region).unwrap().collect::<Result<_, _>>().unwrap()
    }

    fn by_bytes(mut v: Vec<DirEntry>) -> Vec<DirEntry> {
        v.sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
        v
    }

    #[test]
    fn names_sharing_an_eight_byte_prefix_merge_in_byte_order() {
        let mut names: Vec<String> = vec![String::from("commonprefix_")];
        for len in 1..=3 {
            for bits in 0..(1u32 << len) {
                let suffix: String =
                    (0..len).map(|i| if bits >> i & 1 == 0 { 'a' } else { 'b' }).collect();
                names.push(format!("commonprefix_{suffix}"));
            }
        }
        // The zero-padding boundary: "a" pads to the same u64 as "a\0" and "a\0\0\0\0\0\0\0".
        for zeros in [0usize, 1, 2, 7, 8] {
            names.push(format!("a{}", "\0".repeat(zeros)));
        }
        let n = names.len();
        assert_eq!(n, 20);
        // 7 is coprime with 20: a deterministic scramble, so ties meet inside a run and across runs.
        let entries: Vec<DirEntry> =
            (0..n).map(|i| entry(OsString::from(&names[i * 7 % n]), TYPES[i % 4])).collect();
        let want = by_bytes(entries.clone());
        let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
        let runs: Vec<Vec<DirEntry>> = entries.chunks(8).map(<[DirEntry]>::to_vec).collect();
        assert_eq!(runs.len(), 3);
        assert_eq!(spill_and_merge(t, runs), want);
    }

    #[test]
    fn records_larger_than_the_merge_buffer_survive_the_merge() {
        let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
        let lens = [40usize, 255, 65_535];
        let runs: Vec<Vec<DirEntry>> = (0..3)
            .map(|r| {
                lens.iter()
                    .enumerate()
                    .map(|(k, &len)| {
                        let fill = char::from(b'a' + (k * 3 + r) as u8);
                        entry(OsString::from(fill.to_string().repeat(len)), TYPES[(k + r) % 4])
                    })
                    .collect()
            })
            .collect();
        let want = by_bytes(runs.concat());
        assert_eq!(want.len(), 9);
        assert_eq!(spill_and_merge(t, runs), want);
    }

    #[test]
    fn a_truncated_wtf8_sequence_decodes_to_one_replacement_per_byte() {
        assert_eq!(wtf8_to_wide(&[0xE2, 0x82]), [0xFFFD, 0xFFFD]);
        assert_eq!(wtf8_to_wide(&[0xF0, 0x9F, 0x98]), [0xFFFD, 0xFFFD, 0xFFFD]);
        assert_eq!(wtf8_to_wide(&[0xED]), [0xFFFD]);
        assert_eq!(wtf8_to_wide(&[0x61, 0xE2, 0x82]), [0x61, 0xFFFD, 0xFFFD]);
    }

    #[test]
    fn a_failing_tick_ends_the_cascade_and_the_region_truncates_the_runs() {
        let t = Thresholds::new(4, 4, 2, 16, 3).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let owner = space.next_owner();
        let outer = space.begin(owner).unwrap();
        space.write_at(outer.start(), &[7u8; 10]).unwrap();
        let region = space.begin(owner).unwrap();
        let start = region.start();
        let runs: Vec<RunSpan> = (0..3)
            .map(|r| {
                let c: Vec<DirEntry> = (0..4).map(|i| named(i * 3 + r)).collect();
                write_run(&space, &c).unwrap()
            })
            .collect();
        let mut calls = 0;
        let err = cascade(&space, runs, &mut |_| {
            calls += 1;
            Err(FsError::new(Code::IoError, std::io::Error::other("stop")))
        })
        .unwrap_err();
        assert_eq!(err.code, Code::IoError);
        assert_eq!(calls, 1, "the error stops the cascade at the first tick");
        assert!(space.len() > start, "the partial pass is still in the file");
        drop(region);
        assert_eq!(space.len(), start);
        drop(outer);
        assert_eq!(space.len(), 0);
    }
}
