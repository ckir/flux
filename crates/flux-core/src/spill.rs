// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Sorted runs of directory entries in the scratch space, and their k-way merge (spec 6.2).
//!
//! A record is `K` key bytes (`K` is 0, 8 or 24; the walk uses 0), then `u16` LE name length, `u8` file type
//! (0 File, 1 Dir, 2 Symlink, 3 Other), then the name's `as_encoded_bytes()`. The `ino` hint is not stored: a
//! decoded entry carries `None`. Order is the key bytes lexicographically, then `key_cmp` on the name bytes.

use crate::scratch::{Lane, Region, ScratchSpace, read_fully_at, write_all_at};
use flux_fs::{Code, DirEntry, FileType, FsError};
use std::cmp::Ordering;
use std::ffi::OsString;

fn fatal(message: impl Into<String>) -> FsError {
    FsError::new(Code::IoError, std::io::Error::other(message.into()))
}

/// A directory entry with the fixed-width key it sorts by first; only the first `k` bytes of the family are
/// meaningful, the rest are zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keyed {
    pub key: [u8; 24],
    pub entry: DirEntry,
}

impl Keyed {
    /// A keyless item (`K = 0`), as the walk lists.
    pub(crate) fn plain(entry: DirEntry) -> Keyed {
        Keyed { key: [0; 24], entry }
    }
}

/// Appends the record of `item` (its first `k` key bytes, then the name record) to `out`. A name longer than
/// `u16::MAX` bytes is an error and leaves `out` as it was.
pub(crate) fn encode(k: usize, item: &Keyed, out: &mut Vec<u8>) -> Result<(), FsError> {
    let name = item.entry.name.as_encoded_bytes();
    let Ok(len) = u16::try_from(name.len()) else {
        return Err(fatal(format!(
            "a directory entry name of {} bytes is too long to spill (limit {})",
            name.len(),
            u16::MAX
        )));
    };
    out.extend_from_slice(&item.key[..k]);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(match item.entry.file_type {
        FileType::File => 0,
        FileType::Dir => 1,
        FileType::Symlink => 2,
        FileType::Other => 3,
    });
    out.extend_from_slice(name);
    Ok(())
}

/// The first record of `bytes` and its length (key bytes included), with `ino: None` and the key bytes past `k`
/// zero; `None` on a short buffer or an unknown type byte.
pub(crate) fn decode(k: usize, bytes: &[u8]) -> Option<(Keyed, usize)> {
    let mut key = [0u8; 24];
    key[..k].copy_from_slice(bytes.get(..k)?);
    let header = bytes.get(k..k + 3)?;
    let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let file_type = match header[2] {
        0 => FileType::File,
        1 => FileType::Dir,
        2 => FileType::Symlink,
        3 => FileType::Other,
        _ => return None,
    };
    let name = bytes.get(k + 3..k + 3 + len)?;
    Some((
        Keyed { key, entry: DirEntry { name: os_name(name), file_type, ino: None } },
        k + 3 + len,
    ))
}

/// Rebuilds a name from the bytes `as_encoded_bytes` gave.
#[cfg(unix)]
pub(crate) fn os_name(bytes: &[u8]) -> OsString {
    std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
}

#[cfg(windows)]
pub(crate) fn os_name(bytes: &[u8]) -> OsString {
    std::os::windows::ffi::OsStringExt::from_wide(&wtf8_to_wide(bytes))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn os_name(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Decodes generalized UTF-8 (WTF-8) into UTF-16 units. A 3-byte sequence for a surrogate (`ED A0 80`..`ED BF BF`)
/// is one unit; a supplementary scalar is a surrogate pair. Input comes from `OsStr::as_encoded_bytes`, so it is
/// well formed; a malformed or truncated byte becomes U+FFFD and decoding resumes at the next byte (no overlong
/// check).
// Used by `os_name` on Windows only; tests exercise it everywhere.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
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
pub(crate) fn prefix(bytes: &[u8]) -> u64 {
    let mut p = [0u8; 8];
    let n = bytes.len().min(8);
    p[..n].copy_from_slice(&bytes[..n]);
    u64::from_be_bytes(p)
}

/// Equal to `a.cmp(b)`: the prefix as a `u64` decides most pairs without touching the rest.
pub(crate) fn key_cmp(a: &[u8], b: &[u8]) -> Ordering {
    prefix(a).cmp(&prefix(b)).then_with(|| a.cmp(b))
}

/// The first eight key bytes (fewer when `k < 8`), big-endian, zero padded.
fn key_prefix(k: usize, key: &[u8; 24]) -> u64 {
    prefix(&key[..k.min(8)])
}

/// Key bytes first (the first eight as a big-endian `u64`, then the whole `k` bytes), then `key_cmp` on the names.
pub(crate) fn keyed_cmp(k: usize, a: &Keyed, b: &Keyed) -> Ordering {
    let head =
        if k >= 8 { key_prefix(k, &a.key).cmp(&key_prefix(k, &b.key)) } else { Ordering::Equal };
    head.then_with(|| a.key[..k].cmp(&b.key[..k]))
        .then_with(|| key_cmp(a.entry.name.as_encoded_bytes(), b.entry.name.as_encoded_bytes()))
}

/// Sorts by `keyed_cmp`, comparing cached `u64` prefixes of the key and of the name before the bytes (equal items
/// may land in either order).
pub(crate) fn sort_keyed(k: usize, v: &mut [Keyed]) {
    let mut keys: Vec<(u64, u64, u32)> = Vec::with_capacity(v.len());
    for (i, e) in v.iter().enumerate() {
        let Ok(i) = u32::try_from(i) else {
            // More entries than an index holds: sort directly.
            v.sort_unstable_by(|a, b| keyed_cmp(k, a, b));
            return;
        };
        // Past eight key bytes the name must not be compared before the rest of the key, so its cached prefix is
        // left out and a tie goes through `keyed_cmp`.
        let name = if k > 8 { 0 } else { prefix(e.entry.name.as_encoded_bytes()) };
        keys.push((key_prefix(k, &e.key), name, i));
    }
    keys.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| keyed_cmp(k, &v[a.2 as usize], &v[b.2 as usize]))
    });
    // Apply the permutation in place (the method of `slice::sort_by_cached_key`).
    for i in 0..v.len() {
        let mut index = keys[i].2 as usize;
        while index < i {
            index = keys[index].2 as usize;
        }
        keys[i].2 = index as u32;
        v.swap(i, index);
    }
}

/// Sorts keyless items (`K = 0`) by name bytes: the walk's sort.
pub(crate) fn sort_entries(v: &mut [Keyed]) {
    sort_keyed(0, v);
}

/// Where a sorted run lies in the scratch file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunSpan {
    pub start: u64,
    pub len: u64,
    pub count: u64,
}

/// Where a run is read from: a lane of the scratch space, or a transient file a worker owns.
#[derive(Clone, Copy)]
pub(crate) enum Source<'s, 'h> {
    Lane(&'s ScratchSpace<'h>, Lane),
    File(&'s std::fs::File),
}

impl Source<'_, '_> {
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match self {
            Source::Lane(space, lane) => space.read_at(*lane, offset, buf),
            Source::File(file) => read_fully_at(file, buf, offset).map_err(FsError::from_io),
        }
    }
}

/// Where runs are appended: a lane of the scratch space, or a transient file with its append offset.
#[derive(Clone, Copy)]
pub(crate) enum Sink<'s, 'h> {
    Lane(&'s ScratchSpace<'h>, Lane),
    File(&'s std::fs::File, &'s std::cell::Cell<u64>),
}

impl Sink<'_, '_> {
    /// The offset the next byte goes to.
    fn end(&self) -> u64 {
        match self {
            Sink::Lane(space, lane) => space.len(*lane),
            Sink::File(_, offset) => offset.get(),
        }
    }

    fn write(&self, offset: u64, bytes: &[u8]) -> Result<(), FsError> {
        match self {
            Sink::Lane(space, lane) => space.write_at(*lane, offset, bytes),
            Sink::File(file, cell) => {
                write_all_at(file, bytes, offset).map_err(FsError::from_io)?;
                cell.set(cell.get().max(offset + bytes.len() as u64));
                Ok(())
            }
        }
    }
}

/// Appends one run at the end of the sink through a buffer flushed as it fills.
struct RunWriter<'s, 'h> {
    sink: Sink<'s, 'h>,
    k: usize,
    start: u64,
    offset: u64,
    buf: Vec<u8>,
    cap: usize,
    count: u64,
}

impl<'s, 'h> RunWriter<'s, 'h> {
    fn new(sink: Sink<'s, 'h>, k: usize, merge_buffer: usize) -> Self {
        let start = sink.end();
        let cap = merge_buffer.saturating_mul(4);
        RunWriter { sink, k, start, offset: start, buf: Vec::new(), cap, count: 0 }
    }

    fn flush(&mut self, n: usize) -> Result<(), FsError> {
        if n == 0 {
            return Ok(());
        }
        self.sink.write(self.offset, &self.buf[..n])?;
        self.offset += n as u64;
        self.buf.drain(..n);
        Ok(())
    }

    fn push(&mut self, item: &Keyed) -> Result<(), FsError> {
        let mark = self.buf.len();
        encode(self.k, item, &mut self.buf)?;
        self.appended(mark)
    }

    /// Appends a record already in the run's layout (as `encode` writes it).
    fn push_encoded(&mut self, record: &[u8]) -> Result<(), FsError> {
        let mark = self.buf.len();
        self.buf.extend_from_slice(record);
        self.appended(mark)
    }

    /// Counts the record that starts at `mark` and flushes as the buffer fills.
    fn appended(&mut self, mark: usize) -> Result<(), FsError> {
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

/// Appends the (already sorted) `items` as one run at the end of the sink; `merge_buffer` is the threshold of that
/// name (the writer buffers four times as much).
pub(crate) fn write_run(
    sink: Sink,
    k: usize,
    merge_buffer: usize,
    items: &[Keyed],
) -> Result<RunSpan, FsError> {
    let mut w = RunWriter::new(sink, k, merge_buffer);
    for item in items {
        w.push(item)?;
    }
    w.finish()
}

/// Appends the already encoded `records` (each in the layout `encode` writes, in sorted order) as one run at the
/// end of the sink: the bytes are those `write_run` would write for the same items.
pub(crate) fn write_encoded_run<'r>(
    sink: Sink,
    k: usize,
    merge_buffer: usize,
    records: impl Iterator<Item = &'r [u8]>,
) -> Result<RunSpan, FsError> {
    let mut w = RunWriter::new(sink, k, merge_buffer);
    for record in records {
        w.push_encoded(record)?;
    }
    w.finish()
}

/// The length of the first record of `bytes` (key bytes included) when it is whole and its type byte is known:
/// `decode` without building the item.
fn record_len(k: usize, bytes: &[u8]) -> Option<usize> {
    let header = bytes.get(k..k + 3)?;
    let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
    if header[2] > 3 {
        return None;
    }
    bytes.get(k + 3..k + 3 + len)?;
    Some(k + 3 + len)
}

/// The read position in one run. The record at `pos` is the run's head: it stays in `buf` until it is consumed.
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

    /// Makes the record at `pos` whole in the buffer and returns its length without consuming it; `None` at the end
    /// of the run. Refills by `chunk` bytes (a record longer than that grows the buffer).
    fn fill(&mut self, source: Source, k: usize, chunk: usize) -> Result<Option<usize>, FsError> {
        loop {
            if let Some(n) = record_len(k, &self.buf[self.pos..]) {
                return Ok(Some(n));
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
            let got = source.read(self.next_off, &mut self.buf[old..])?;
            if got < want {
                return Err(fatal("a scratch run ends before its recorded length"));
            }
            self.next_off += want as u64;
        }
    }

    /// The head record, `len` bytes as `fill` returned.
    fn record(&self, len: usize) -> &[u8] {
        &self.buf[self.pos..self.pos + len]
    }
}

/// A run's current head in the merge heap: where it is (the record is in the run's cursor buffer) and the cached
/// words that decide most comparisons.
struct Head {
    run: usize,
    len: usize,
    /// The first `min(k, 8)` key bytes, big-endian.
    key: u64,
    /// The first eight name bytes, big-endian; `0` when `k > 8` (the name must not count before the rest of the key).
    name: u64,
}

/// The order of the merge: key bytes, then `key_cmp` on the names (as `keyed_cmp`), then the run index.
fn head_cmp(cursors: &[RunCursor], k: usize, a: &Head, b: &Head) -> Ordering {
    a.key
        .cmp(&b.key)
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| {
            let (ra, rb) = (cursors[a.run].record(a.len), cursors[b.run].record(b.len));
            ra[..k].cmp(&rb[..k]).then_with(|| key_cmp(&ra[k + 3..], &rb[k + 3..]))
        })
        .then_with(|| a.run.cmp(&b.run))
}

fn sift_up(heap: &mut [Head], cursors: &[RunCursor], k: usize, mut i: usize) {
    while i > 0 {
        let parent = (i - 1) / 2;
        if head_cmp(cursors, k, &heap[i], &heap[parent]) != Ordering::Less {
            break;
        }
        heap.swap(i, parent);
        i = parent;
    }
}

fn sift_down(heap: &mut [Head], cursors: &[RunCursor], k: usize, mut i: usize) {
    loop {
        let (l, r) = (2 * i + 1, 2 * i + 2);
        let mut min = i;
        if l < heap.len() && head_cmp(cursors, k, &heap[l], &heap[min]) == Ordering::Less {
            min = l;
        }
        if r < heap.len() && head_cmp(cursors, k, &heap[r], &heap[min]) == Ordering::Less {
            min = r;
        }
        if min == i {
            return;
        }
        heap.swap(i, min);
        i = min;
    }
}

/// The merge itself, with no region of its own (so `cascade` can append inside its caller's).
pub(crate) struct Merger<'s, 'h> {
    source: Source<'s, 'h>,
    k: usize,
    cursors: Vec<RunCursor>,
    /// A binary min-heap by `head_cmp`.
    heap: Vec<Head>,
    chunk: usize,
    /// The record `next_entry` decodes.
    scratch: Vec<u8>,
}

impl<'s, 'h> Merger<'s, 'h> {
    pub(crate) fn new(
        source: Source<'s, 'h>,
        k: usize,
        chunk: usize,
        runs: &[RunSpan],
    ) -> Result<Self, FsError> {
        let mut cursors: Vec<RunCursor> = runs.iter().map(|r| RunCursor::new(*r)).collect();
        let mut heap = Vec::with_capacity(cursors.len());
        for run in 0..cursors.len() {
            if let Some(len) = cursors[run].fill(source, k, chunk)? {
                heap.push(Self::head(&cursors[run], k, run, len));
                let last = heap.len() - 1;
                sift_up(&mut heap, &cursors, k, last);
            }
        }
        Ok(Merger { source, k, cursors, heap, chunk, scratch: Vec::new() })
    }

    fn head(cursor: &RunCursor, k: usize, run: usize, len: usize) -> Head {
        let record = cursor.record(len);
        let key = prefix(&record[..k.min(8)]);
        let name = if k > 8 { 0 } else { prefix(&record[k + 3..]) };
        Head { run, len, key, name }
    }

    /// Replaces `out` with the next record in merge order (as `encode` writes it); `false` at the end. It allocates
    /// nothing per record: the record is copied out of the run's buffer before the run advances.
    pub(crate) fn next_record_into(&mut self, out: &mut Vec<u8>) -> Result<bool, FsError> {
        let Some(top) = self.heap.first() else {
            return Ok(false);
        };
        let (run, len) = (top.run, top.len);
        out.clear();
        out.extend_from_slice(self.cursors[run].record(len));
        self.cursors[run].pos += len;
        match self.cursors[run].fill(self.source, self.k, self.chunk)? {
            Some(next) => {
                self.heap[0] = Self::head(&self.cursors[run], self.k, run, next);
            }
            None => {
                self.heap.swap_remove(0);
            }
        }
        sift_down(&mut self.heap, &self.cursors, self.k, 0);
        Ok(true)
    }

    pub(crate) fn next_entry(&mut self) -> Result<Option<Keyed>, FsError> {
        let mut record = std::mem::take(&mut self.scratch);
        let result = self.next_record_into(&mut record);
        let item = match result {
            Ok(true) => match decode(self.k, &record) {
                Some((item, _)) => Ok(Some(item)),
                None => Err(fatal("a scratch run holds a corrupt record")),
            },
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        };
        self.scratch = record;
        item
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
        lane: Lane,
        k: usize,
        runs: Vec<RunSpan>,
        region: Region<'s, 'h>,
    ) -> Result<Self, FsError> {
        let merger =
            Merger::new(Source::Lane(space, lane), k, space.thresholds().merge_buffer, &runs)?;
        Ok(MergeReader { merger, done: false, _region: region })
    }
}

impl Iterator for MergeReader<'_, '_> {
    type Item = Result<Keyed, FsError>;

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

/// The number of passes a cascade of `n` runs makes at `fan_in`.
pub(crate) fn passes_for(mut n: usize, fan_in: usize) -> u32 {
    let mut passes = 0u32;
    while n > fan_in {
        n = n.div_ceil(fan_in);
        passes += 1;
    }
    passes
}

/// One merge pass, counting merged entries in `merged` (so a caller looping over passes keeps one running count
/// for its ticks).
#[allow(clippy::too_many_arguments)]
fn cascade_pass_counting(
    source: Source,
    sink: Sink,
    k: usize,
    merge_buffer: usize,
    runs: &[RunSpan],
    fan_in: usize,
    tick_entries: usize,
    pass: u32,
    passes: u32,
    merged: &mut u64,
    tick: &mut dyn FnMut(u64) -> Result<(), FsError>,
) -> Result<Vec<RunSpan>, FsError> {
    let mut next = Vec::with_capacity(runs.len().div_ceil(fan_in));
    let mut bytes = 0u64;
    for group in runs.chunks(fan_in) {
        if let [only] = group {
            next.push(*only);
            continue;
        }
        let mut merger = Merger::new(source, k, merge_buffer, group)?;
        let mut writer = RunWriter::new(sink, k, merge_buffer);
        while let Some(item) = merger.next_entry()? {
            writer.push(&item)?;
            *merged += 1;
            if tick_entries > 0 && merged.is_multiple_of(tick_entries as u64) {
                tick(*merged)?;
            }
        }
        let span = writer.finish()?;
        bytes += span.len;
        next.push(span);
    }
    tracing::debug!(pass, passes, bytes, "merge pass");
    tick(*merged)?;
    Ok(next)
}

/// Merges ONE pass: consecutive groups of `fan_in` runs of `source` into new runs appended to `sink`; a lone
/// trailing run is carried over as it is. `merge_buffer` and `tick_entries` are the thresholds of those names,
/// passed as values because a worker thread has no `&ScratchSpace`. `tick` gets the entries merged in this pass
/// every `tick_entries` entries and once at the end. Logs the pass (`pass` of `passes`, as its caller counts).
#[allow(clippy::too_many_arguments)]
pub(crate) fn cascade_pass(
    source: Source,
    sink: Sink,
    k: usize,
    merge_buffer: usize,
    runs: &[RunSpan],
    fan_in: usize,
    tick_entries: usize,
    pass: u32,
    passes: u32,
    tick: &mut dyn FnMut(u64) -> Result<(), FsError>,
) -> Result<Vec<RunSpan>, FsError> {
    let mut merged = 0u64;
    cascade_pass_counting(
        source,
        sink,
        k,
        merge_buffer,
        runs,
        fan_in,
        tick_entries,
        pass,
        passes,
        &mut merged,
        tick,
    )
}

/// Merges consecutive groups of `merge_fan_in` runs into new runs appended at the end of the lane's file (the
/// inputs stay until the caller's region drops) while more than `merge_fan_in` remain. A lone run at the end of a
/// pass is carried over as it is. `tick` gets the entries merged so far every `tick_entries` entries and once at
/// the end of each pass.
pub(crate) fn cascade(
    space: &ScratchSpace,
    lane: Lane,
    k: usize,
    mut runs: Vec<RunSpan>,
    tick: &mut dyn FnMut(u64) -> Result<(), FsError>,
) -> Result<Vec<RunSpan>, FsError> {
    let t = space.thresholds();
    let fan = t.merge_fan_in;
    let passes = passes_for(runs.len(), fan);
    let mut merged = 0u64;
    let mut pass = 0u32;
    while runs.len() > fan {
        pass += 1;
        runs = cascade_pass_counting(
            Source::Lane(space, lane),
            Sink::Lane(space, lane),
            k,
            t.merge_buffer,
            &runs,
            fan,
            t.tick_entries,
            pass,
            passes,
            &mut merged,
            tick,
        )?;
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

    fn sort_plain(v: &mut Vec<DirEntry>) {
        let mut k: Vec<Keyed> = v.drain(..).map(Keyed::plain).collect();
        sort_entries(&mut k);
        v.extend(k.into_iter().map(|i| i.entry));
    }

    fn wr(space: &ScratchSpace, items: &[DirEntry]) -> Result<RunSpan, FsError> {
        let k: Vec<Keyed> = items.iter().cloned().map(Keyed::plain).collect();
        write_run(Sink::Lane(space, Lane::Walk), 0, space.thresholds().merge_buffer, &k)
    }

    fn casc(
        space: &ScratchSpace,
        runs: Vec<RunSpan>,
        tick: &mut dyn FnMut(u64) -> Result<(), FsError>,
    ) -> Result<Vec<RunSpan>, FsError> {
        cascade(space, Lane::Walk, 0, runs, tick)
    }

    fn reader<'s, 'h>(
        space: &'s ScratchSpace<'h>,
        runs: Vec<RunSpan>,
        region: Region<'s, 'h>,
    ) -> Result<impl Iterator<Item = Result<DirEntry, FsError>> + use<'s, 'h>, FsError> {
        Ok(MergeReader::new(space, Lane::Walk, 0, runs, region)?.map(|r| r.map(|i| i.entry)))
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
                let e = Keyed::plain(entry(name.clone(), ft));
                let mut buf = Vec::new();
                encode(0, &e, &mut buf).unwrap();
                assert_eq!(buf.len(), 3 + name.as_encoded_bytes().len());
                assert_eq!(decode(0, &buf), Some((e, buf.len())));
                assert_eq!(decode(0, &buf[..buf.len() - 1]), None, "a short buffer");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_name_longer_than_u16_is_an_error_not_a_panic() {
        let name: OsString = std::os::unix::ffi::OsStringExt::from_vec(vec![b'a'; 65_536]);
        let mut buf = Vec::new();
        assert!(encode(0, &Keyed::plain(entry(name, FileType::File)), &mut buf).is_err());
        assert!(buf.is_empty(), "nothing is written for a refused name");
        let ok: OsString = std::os::unix::ffi::OsStringExt::from_vec(vec![b'a'; 65_535]);
        assert!(encode(0, &Keyed::plain(entry(ok, FileType::File)), &mut buf).is_ok());
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

            let region = space.begin(Lane::Walk, space.next_owner()).unwrap();
            let mut runs = Vec::new();
            for chunk in entries.chunks(t.run_entries) {
                let mut c = chunk.to_vec();
                sort_plain(&mut c);
                runs.push(wr(&space, &c).unwrap());
            }
            let runs = casc(&space, runs, &mut |_| Ok(())).unwrap();
            prop_assert!(runs.len() <= t.merge_fan_in);
            let got: Vec<DirEntry> = reader(&space, runs, region)
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
        let region = space.begin(Lane::Walk, space.next_owner()).unwrap();
        let mut runs = Vec::new();
        for c in 0..25 {
            let mut chunk: Vec<DirEntry> = (0..4).map(|i| named(i * 25 + c)).collect();
            sort_plain(&mut chunk);
            runs.push(wr(&space, &chunk).unwrap());
        }
        assert_eq!(runs.len(), 25);
        let mut ticks = 0;
        let runs = casc(&space, runs, &mut |_| {
            ticks += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(ticks, 4, "25 -> 13 -> 7 -> 4 -> 2 is four passes");
        let names: Vec<OsString> =
            reader(&space, runs, region).unwrap().map(|e| e.unwrap().name).collect();
        let want: Vec<OsString> = (0..100).map(|i| named(i).name).collect();
        assert_eq!(names, want);
    }

    #[test]
    fn dropping_a_reader_truncates_its_runs() {
        let t = Thresholds::new(4, 4, 2, 16, 3).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let owner = space.next_owner();
        let outer = space.begin(Lane::Walk, owner).unwrap();
        space.write_at(Lane::Walk, outer.start(), &[7u8; 10]).unwrap();
        let region = space.begin(Lane::Walk, owner).unwrap();
        let start = region.start();
        assert_eq!(start, 10);
        let runs: Vec<RunSpan> = (0..3)
            .map(|r| {
                let c: Vec<DirEntry> = (0..4).map(|i| named(i * 3 + r)).collect();
                wr(&space, &c).unwrap()
            })
            .collect();
        assert!(space.len(Lane::Walk) > start);
        let reader = reader(&space, runs, region).unwrap();
        drop(reader);
        assert_eq!(space.len(Lane::Walk), start);
        drop(outer);
        assert_eq!(space.len(Lane::Walk), 0);
    }

    #[test]
    fn a_read_error_mid_merge_is_returned_and_the_reader_ends() {
        let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let region = space.begin(Lane::Walk, space.next_owner()).unwrap();
        let runs: Vec<RunSpan> = (0..3)
            .map(|r| {
                let c: Vec<DirEntry> = (0..8).map(|i| named(i * 3 + r)).collect();
                wr(&space, &c).unwrap()
            })
            .collect();
        let mut reader = reader(&space, runs, region).unwrap();
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
        let region = space.begin(Lane::Walk, space.next_owner()).unwrap();
        let mut spans = Vec::new();
        for mut run in runs {
            sort_plain(&mut run);
            spans.push(wr(&space, &run).unwrap());
        }
        let spans = casc(&space, spans, &mut |_| Ok(())).unwrap();
        assert!(spans.len() <= t.merge_fan_in);
        reader(&space, spans, region).unwrap().collect::<Result<_, _>>().unwrap()
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
        let outer = space.begin(Lane::Walk, owner).unwrap();
        space.write_at(Lane::Walk, outer.start(), &[7u8; 10]).unwrap();
        let region = space.begin(Lane::Walk, owner).unwrap();
        let start = region.start();
        let runs: Vec<RunSpan> = (0..3)
            .map(|r| {
                let c: Vec<DirEntry> = (0..4).map(|i| named(i * 3 + r)).collect();
                wr(&space, &c).unwrap()
            })
            .collect();
        let mut calls = 0;
        let err = casc(&space, runs, &mut |_| {
            calls += 1;
            Err(FsError::new(Code::IoError, std::io::Error::other("stop")))
        })
        .unwrap_err();
        assert_eq!(err.code, Code::IoError);
        assert_eq!(calls, 1, "the error stops the cascade at the first tick");
        assert!(space.len(Lane::Walk) > start, "the partial pass is still in the file");
        drop(region);
        assert_eq!(space.len(Lane::Walk), start);
        drop(outer);
        assert_eq!(space.len(Lane::Walk), 0);
    }

    fn kitem(key_byte: u8, name: &str) -> Keyed {
        let mut key = [0u8; 24];
        for (i, b) in key.iter_mut().enumerate() {
            *b = key_byte.wrapping_add(i as u8);
        }
        Keyed { key, entry: entry(OsString::from(name), FileType::File) }
    }

    /// What a record keeps for `k`: the first `k` key bytes, the rest zero.
    fn masked(k: usize, mut item: Keyed) -> Keyed {
        item.key[k..].fill(0);
        item
    }

    #[test]
    fn a_keyed_record_round_trips_for_k_0_8_and_24() {
        for k in [0usize, 8, 24] {
            for ft in TYPES {
                let mut item = kitem(7, "name");
                item.entry.file_type = ft;
                let mut buf = Vec::new();
                encode(k, &item, &mut buf).unwrap();
                assert_eq!(buf.len(), k + 3 + 4, "k = {k}");
                assert_eq!(&buf[..k], &item.key[..k]);
                let want = masked(k, item);
                assert_eq!(decode(k, &buf), Some((want, buf.len())), "k = {k}");
                assert_eq!(decode(k, &buf[..buf.len() - 1]), None, "a short buffer, k = {k}");
                if k > 0 {
                    assert_eq!(decode(k, &buf[..k - 1]), None, "a short key, k = {k}");
                }
            }
        }
    }

    #[test]
    fn keyed_order_is_key_bytes_then_name_bytes() {
        for k in [8usize, 24] {
            let mut lo = kitem(0, "b");
            let mut hi = kitem(0, "a");
            lo.key = [0; 24];
            hi.key = [0; 24];
            hi.key[..8].fill(1);
            assert_eq!(keyed_cmp(k, &lo, &hi), Ordering::Less, "the key decides, k = {k}");
            assert_eq!(keyed_cmp(k, &hi, &lo), Ordering::Greater);
            let a = Keyed { key: [3; 24], entry: entry(OsString::from("a"), FileType::File) };
            let b = Keyed { key: [3; 24], entry: entry(OsString::from("b"), FileType::Dir) };
            assert_eq!(keyed_cmp(k, &a, &b), Ordering::Less, "equal keys: the name decides");
            assert_eq!(keyed_cmp(k, &b, &a), Ordering::Greater);
        }
        // A difference after the first K bytes does not count.
        let mut x = kitem(0, "a");
        let mut y = kitem(0, "a");
        x.key[8] = 1;
        y.key[8] = 2;
        assert_eq!(keyed_cmp(8, &x, &y), Ordering::Equal);
        assert_eq!(keyed_cmp(24, &x, &y), Ordering::Less);
    }

    #[test]
    fn sort_keyed_orders_k24_items_whose_first_eight_key_bytes_tie() {
        let item = |tail: u8, name: &str| {
            let mut key = [5u8; 24];
            key[16..].fill(tail);
            Keyed { key, entry: entry(OsString::from(name), FileType::File) }
        };
        // Key order is 1, 2, 3; the names are in the opposite order.
        let mut got = vec![item(3, "a"), item(1, "c"), item(2, "b")];
        let mut want = got.clone();
        want.sort_by(|a, b| keyed_cmp(24, a, b));
        sort_keyed(24, &mut got);
        assert_eq!(got, want);
        assert_eq!(got[0].key[16], 1);
    }

    #[test]
    fn a_keyed_merge_of_three_runs_is_sorted_and_complete() {
        let t = Thresholds::new(4, 4, 2, 16, 3).unwrap();
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let file = space.transient().unwrap();
        let offset = std::cell::Cell::new(0u64);
        let items: Vec<Keyed> =
            (0..12u8).map(|i| kitem(i * 5 % 4, &format!("n{:02}", i * 7 % 12))).collect();
        let mut spans = Vec::new();
        for chunk in items.chunks(4) {
            let mut c = chunk.to_vec();
            sort_keyed(8, &mut c);
            spans.push(write_run(Sink::File(&file, &offset), 8, t.merge_buffer, &c).unwrap());
        }
        assert_eq!(spans.len(), 3);
        let mut merger = Merger::new(Source::File(&file), 8, t.merge_buffer, &spans).unwrap();
        let mut got = Vec::new();
        while let Some(item) = merger.next_entry().unwrap() {
            got.push(item);
        }
        let mut want: Vec<Keyed> = items.into_iter().map(|i| masked(8, i)).collect();
        sort_keyed(8, &mut want);
        assert_eq!(got.len(), 12);
        assert_eq!(got, want);
    }

    /// Items for the merge tests: names sharing an eight-byte prefix, short and empty names, keys sharing their
    /// first eight bytes, and the same (key, name) with different file types.
    fn merge_items(k: usize, n: usize, seed: u64) -> Vec<Keyed> {
        let mut x = seed | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        (0..n)
            .map(|_| {
                let name = match next() % 5 {
                    0 => format!("sharedpr{}", next() % 6),
                    1 => format!("{}", next() % 30),
                    2 => String::new(),
                    3 => format!("sharedpr-{}", next() % 3),
                    _ => format!("n{}", next() % 40),
                };
                let mut key = [0u8; 24];
                key[..8].copy_from_slice(&(next() % 3).to_be_bytes());
                for b in &mut key[8..] {
                    *b = (next() % 2) as u8;
                }
                masked(
                    k,
                    Keyed { key, entry: entry(OsString::from(name), TYPES[(next() % 4) as usize]) },
                )
            })
            .collect()
    }

    fn write_runs(
        file: &std::fs::File,
        k: usize,
        t: &Thresholds,
        runs: &[Vec<Keyed>],
    ) -> Vec<RunSpan> {
        let offset = std::cell::Cell::new(0u64);
        runs.iter()
            .map(|r| write_run(Sink::File(file, &offset), k, t.merge_buffer, r).unwrap())
            .collect()
    }

    #[test]
    fn next_record_into_merges_as_the_reference_merge_does() {
        let t = Thresholds::new(4, 4, 2, 16, 3).unwrap();
        for k in [0usize, 8, 24] {
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let file = space.transient().unwrap();
            let items = merge_items(k, 5200, 0x2545_F491_4F6C_DD1D ^ k as u64);
            let mut runs: Vec<Vec<Keyed>> = items.chunks(400).map(<[Keyed]>::to_vec).collect();
            runs.iter_mut().for_each(|r| sort_keyed(k, r));
            let spans = write_runs(&file, k, &t, &runs);
            // The reference: every (item, run) pair sorted by keyed_cmp, then the run index (equal pairs of one
            // run keep their written order, the sort is stable).
            let mut want: Vec<(Keyed, usize)> = runs
                .iter()
                .enumerate()
                .flat_map(|(i, r)| r.iter().cloned().map(move |x| (x, i)))
                .collect();
            want.sort_by(|a, b| keyed_cmp(k, &a.0, &b.0).then(a.1.cmp(&b.1)));
            let mut merger = Merger::new(Source::File(&file), k, t.merge_buffer, &spans).unwrap();
            let mut record = Vec::new();
            let mut got = Vec::new();
            while merger.next_record_into(&mut record).unwrap() {
                let (item, n) = decode(k, &record).unwrap();
                assert_eq!(n, record.len());
                got.push(item);
            }
            let want: Vec<Keyed> = want.into_iter().map(|p| p.0).collect();
            assert_eq!(got.len(), want.len(), "k = {k}");
            assert!(got == want, "k = {k}: the merge order differs");
            let mut again = Merger::new(Source::File(&file), k, t.merge_buffer, &spans).unwrap();
            let mut via_entry = Vec::new();
            while let Some(item) = again.next_entry().unwrap() {
                via_entry.push(item);
            }
            assert!(via_entry == want, "k = {k}: next_entry differs");
        }
    }

    #[test]
    fn a_cut_last_record_is_the_same_corrupt_run_error_from_both_methods() {
        let t = Thresholds::new(4, 4, 2, 16, 3).unwrap();
        for k in [0usize, 8] {
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let file = space.transient().unwrap();
            let mut run = vec![kitem(1, "alpha"), kitem(2, "bravo"), kitem(3, "charlie")];
            run.iter_mut().for_each(|i| *i = masked(k, i.clone()));
            sort_keyed(k, &mut run);
            let mut span = write_runs(&file, k, &t, &[run])[0];
            span.len -= 2;
            let mut a = Merger::new(Source::File(&file), k, t.merge_buffer, &[span]).unwrap();
            let mut b = Merger::new(Source::File(&file), k, t.merge_buffer, &[span]).unwrap();
            let mut record = Vec::new();
            // The cut record is met while the run advances past the one before it.
            assert!(a.next_record_into(&mut record).unwrap());
            let from_record = a.next_record_into(&mut record).unwrap_err();
            assert!(b.next_entry().unwrap().is_some());
            let from_entry = b.next_entry().unwrap_err();
            assert_eq!(from_record.code, from_entry.code, "k = {k}");
            assert_eq!(from_record.to_string(), from_entry.to_string(), "k = {k}");
            assert!(from_record.to_string().contains("corrupt record"), "{from_record}");
        }
    }

    #[test]
    fn write_encoded_run_writes_the_bytes_write_run_writes() {
        use crate::fault_fs::FaultFs;
        let t = Thresholds::new(2, 4, 2, 4, 1).unwrap();
        for k in [0usize, 8, 24] {
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let mut items: Vec<Keyed> =
                (0..30u8).map(|i| kitem(i % 5, &format!("n{:02}", i * 7 % 30))).collect();
            items.iter_mut().for_each(|i| *i = masked(k, i.clone()));
            sort_keyed(k, &mut items);
            let (a, b) = (space.transient().unwrap(), space.transient().unwrap());
            let (ea, eb) = (std::cell::Cell::new(0u64), std::cell::Cell::new(0u64));
            let sa = write_run(Sink::File(&a, &ea), k, t.merge_buffer, &items).unwrap();
            let encoded: Vec<Vec<u8>> = items
                .iter()
                .map(|i| {
                    let mut v = Vec::new();
                    encode(k, i, &mut v).unwrap();
                    v
                })
                .collect();
            let sb = write_encoded_run(
                Sink::File(&b, &eb),
                k,
                t.merge_buffer,
                encoded.iter().map(|v| v.as_slice()),
            )
            .unwrap();
            assert_eq!(sa, sb, "k = {k}");
            let (mut bytes_a, mut bytes_b) =
                (vec![0u8; sa.len as usize], vec![0u8; sb.len as usize]);
            read_fully_at(&a, &mut bytes_a, 0).unwrap();
            read_fully_at(&b, &mut bytes_b, 0).unwrap();
            assert_eq!(bytes_a, bytes_b, "k = {k}");
        }
    }

    #[test]
    fn the_walks_k0_runs_are_byte_identical_to_plan_2() {
        let names = ["a", "bb", "ccc", "dddd", "eeeee"];
        let mut got = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let mut item = kitem(9, n);
            item.entry.file_type = TYPES[i % 4];
            encode(0, &item, &mut got).unwrap();
        }
        let want: Vec<u8> = vec![
            1, 0, 0, b'a', //
            2, 0, 1, b'b', b'b', //
            3, 0, 2, b'c', b'c', b'c', //
            4, 0, 3, b'd', b'd', b'd', b'd', //
            5, 0, 0, b'e', b'e', b'e', b'e', b'e',
        ];
        assert_eq!(got, want);
    }

    /// Items whose keys share one random eight-byte prefix, so the first cached word ties for every pair.
    #[cfg(unix)]
    fn random_items() -> impl Strategy<Value = Vec<(Vec<u8>, Vec<u8>)>> {
        let name = proptest::collection::vec(1u8..=255, 1..=40);
        let tail = proptest::collection::vec(prop::sample::select(vec![0u8, 1, 255]), 16);
        let shared = proptest::collection::vec(any::<u8>(), 8);
        (shared, proptest::collection::vec((tail, name), 1..=200)).prop_map(|(shared, items)| {
            items.into_iter().map(|(tail, name)| ([shared.clone(), tail].concat(), name)).collect()
        })
    }

    #[cfg(unix)]
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn keyed_merge_equals_sort_for_random_keys_and_names(
            k in prop::sample::select(vec![0usize, 8, 24]),
            raw in random_items(),
        ) {
            let t = Thresholds::new(4, 8, 2, 16, 3).unwrap();
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let items: Vec<Keyed> = raw
                .into_iter()
                .enumerate()
                .map(|(i, (key, name))| {
                    let name = name.into_iter().map(|b| if b == b'/' { b'a' } else { b }).collect();
                    let mut item = Keyed {
                        key: key.try_into().unwrap(),
                        entry: entry(std::os::unix::ffi::OsStringExt::from_vec(name), TYPES[i % 4]),
                    };
                    item = masked(k, item);
                    item
                })
                .collect();
            let mut want = items.clone();
            want.sort_by(|a, b| a.key[..k].cmp(&b.key[..k]).then_with(|| {
                a.entry.name.as_encoded_bytes().cmp(b.entry.name.as_encoded_bytes())
            }));
            // Duplicate (key, name) pairs may repeat; their relative order is not observable but their
            // file types are, so compare as multisets by sorting those too.
            let mut spans = Vec::new();
            let region = space.begin(Lane::Walk, space.next_owner()).unwrap();
            for chunk in items.chunks(t.run_entries) {
                let mut c = chunk.to_vec();
                sort_keyed(k, &mut c);
                spans.push(write_run(Sink::Lane(&space, Lane::Walk), k, t.merge_buffer, &c).unwrap());
            }
            let spans = cascade(&space, Lane::Walk, k, spans, &mut |_| Ok(())).unwrap();
            let got: Vec<Keyed> = MergeReader::new(&space, Lane::Walk, k, spans, region)
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let order = |v: &[Keyed]| -> Vec<(Vec<u8>, Vec<u8>)> {
                v.iter()
                    .map(|i| (i.key[..k].to_vec(), i.entry.name.as_encoded_bytes().to_vec()))
                    .collect()
            };
            prop_assert_eq!(order(&got), order(&want));
            let mut a = got;
            let mut b = want;
            let total = |v: &mut Vec<Keyed>| v.sort_by_key(|i| format!("{i:?}"));
            total(&mut a);
            total(&mut b);
            prop_assert_eq!(a, b);
        }
    }
}
