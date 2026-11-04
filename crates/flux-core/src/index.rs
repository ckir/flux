// Copyright (c) 2026 Costas Kirgoussios
//
// This software is licensed under the PolyForm Noncommercial License 1.0.0.
// You may obtain a copy of the License at:
// https://polyformproject.org/licenses/noncommercial/1.0.0/

//! Tier 1 of the destination index: sorted families packed into blocks with a static multi-level block index
//! (spec 6.3), built from sorted runs on worker threads, and looked up through a block cache.
//!
//! A family is a sequence of FRAMES, then its index levels. A frame is `B` bytes (`B = index_block`): a `u16` LE
//! record count, then whole keyed records (the spill format), zero padded. A record longer than `B - 2` gets a frame
//! of `ceil((2 + len) / B) * B` bytes of its own. A level is a sequence of `B`-byte blocks: a `u16` LE entry count,
//! then entries, one per frame (level 0) or per block of the level below. An entry is `u16` LE key length, the first
//! `B / 8` bytes of the first record's key then name (the CAPPED key), `u64` LE offset and `u32` LE length of what it
//! points at. The top level is one block.

use crate::scratch::{
    INDEX_OWNER, Lane, Region, ScratchSpace, Thresholds, read_fully_at, write_all_at,
};
#[cfg(test)]
use crate::spill::sort_keyed;
use crate::spill::{
    Keyed, Merger, RunSpan, Sink, Source, cascade_pass, decode, encode, key_cmp, os_name,
    passes_for, prefix, write_encoded_run, write_run,
};
use crate::walk::Tick;
use flux_fs::{Code, FsError};
use std::cell::{Cell, Ref};
use std::ffi::OsString;
use std::fs::File;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

#[cfg(test)]
use std::sync::Arc;

fn fatal(message: impl Into<String>) -> FsError {
    FsError::new(Code::IoError, std::io::Error::other(message.into()))
}

fn corrupt() -> FsError {
    fatal("an index block is corrupt")
}

/// The same error again: `FsError` is not `Clone` and its source must never be rebuilt with context.
fn remade(e: &FsError) -> FsError {
    FsError::new(e.code, std::io::Error::new(e.source.kind(), e.source.to_string()))
}

/// Where a packed family lies in its lane: the data frames occupy `[start, data_end)`, level `i` occupies
/// `[levels[i], levels[i + 1])` (the last level ends at `end`), and the top level is one block.
#[derive(Debug, Clone)]
pub(crate) struct FamilyMeta {
    pub lane: Lane,
    pub k: usize,
    pub start: u64,
    pub end: u64,
    pub data_end: u64,
    pub levels: Vec<u64>,
    // Read by the tests; the hybrid index reports counts from its own bookkeeping.
    #[allow(dead_code)]
    pub count: u64,
}

/// Why `PendingRuns::finish` returned without a family.
#[derive(Debug)]
pub(crate) enum FinishError {
    /// Returned only because `stop` was set.
    Stopped,
    Failed(FsError),
}

// ---------------------------------------------------------------------------------------------------------------
// Building: sorted runs in transient files.

/// One buffered record: where it starts in the arena and the cached big-endian prefixes it sorts by.
#[derive(Clone, Copy)]
struct Descriptor {
    start: u64,
    key: u64,
    name: u64,
}

/// The whole encoded record at `start`.
fn record_bytes(k: usize, arena: &[u8], start: u64) -> &[u8] {
    let start = start as usize;
    let len = usize::from(u16::from_le_bytes([arena[start + k], arena[start + k + 1]]));
    &arena[start..start + k + 3 + len]
}

/// The key bytes and the name bytes of the record at `start`.
fn record_parts(k: usize, arena: &[u8], start: u64) -> (&[u8], &[u8]) {
    let record = record_bytes(k, arena, start);
    (&record[..k], &record[k + 3..])
}

/// Collects the items of one family into sorted runs in a transient file. It creates no file until the first push.
pub(crate) struct FamilyBuilder<'s, 'h> {
    space: &'s ScratchSpace<'h>,
    lane: Lane,
    k: usize,
    th: Thresholds,
    /// The run being collected, encoded as `spill::encode` writes it; `descriptors` point into it.
    arena: Vec<u8>,
    descriptors: Vec<Descriptor>,
    file: Option<File>,
    end: Cell<u64>,
    runs: Vec<RunSpan>,
    count: u64,
}

impl<'s, 'h> FamilyBuilder<'s, 'h> {
    pub(crate) fn new(space: &'s ScratchSpace<'h>, lane: Lane, k: usize) -> Self {
        FamilyBuilder {
            space,
            lane,
            k,
            th: space.thresholds(),
            arena: Vec::new(),
            descriptors: Vec::new(),
            file: None,
            end: Cell::new(0),
            runs: Vec::new(),
            count: 0,
        }
    }

    pub(crate) fn push(&mut self, item: Keyed, tick: &mut Tick<'_>) -> Result<(), FsError> {
        if self.file.is_none() {
            self.file = Some(self.space.transient()?);
        }
        let start = self.arena.len();
        encode(self.k, &item, &mut self.arena)?;
        let name = item.entry.name.as_encoded_bytes();
        self.descriptors.push(Descriptor {
            start: start as u64,
            key: prefix(&item.key[..self.k.min(8)]),
            // Past eight key bytes the name must not be compared before the rest of the key.
            name: if self.k > 8 { 0 } else { prefix(name) },
        });
        self.count += 1;
        if self.descriptors.len() >= self.th.run_entries {
            self.write_buffer()?;
        }
        if self.th.tick_entries > 0 && self.count.is_multiple_of(self.th.tick_entries as u64) {
            tick()?;
        }
        Ok(())
    }

    fn write_buffer(&mut self) -> Result<(), FsError> {
        let Some(file) = self.file.as_ref() else { return Ok(()) };
        if self.descriptors.is_empty() {
            return Ok(());
        }
        let (k, arena) = (self.k, &self.arena);
        self.descriptors.sort_unstable_by(|a, b| {
            a.key.cmp(&b.key).then_with(|| a.name.cmp(&b.name)).then_with(|| {
                let (key_a, name_a) = record_parts(k, arena, a.start);
                let (key_b, name_b) = record_parts(k, arena, b.start);
                key_a.cmp(key_b).then_with(|| key_cmp(name_a, name_b))
            })
        });
        let records = self.descriptors.iter().map(|d| record_bytes(k, arena, d.start));
        let span =
            write_encoded_run(Sink::File(file, &self.end), k, self.th.merge_buffer, records)?;
        self.runs.push(span);
        self.arena.clear();
        self.descriptors.clear();
        Ok(())
    }

    #[cfg(test)]
    fn buffered(&self) -> (usize, usize, usize, usize) {
        (
            self.arena.len(),
            self.arena.capacity(),
            self.descriptors.len(),
            self.descriptors.capacity(),
        )
    }

    pub(crate) fn into_pending(mut self) -> Result<PendingFamily, FsError> {
        if self.file.is_none() {
            return Ok(PendingFamily::Empty);
        }
        self.write_buffer()?;
        let second = if self.runs.len() > self.th.merge_fan_in {
            Some(self.space.transient()?)
        } else {
            None
        };
        let Some(first) = self.file.take() else { return Ok(PendingFamily::Empty) };
        Ok(PendingFamily::Runs(PendingRuns {
            lane: self.lane,
            k: self.k,
            count: self.count,
            runs: std::mem::take(&mut self.runs),
            first,
            first_end: self.end.get(),
            second,
            th: self.th,
            #[cfg(test)]
            hook: None,
        }))
    }
}

pub(crate) enum PendingFamily {
    Empty,
    Runs(PendingRuns),
}

#[cfg(test)]
enum Hook {
    Fail(u64),
    Panic(u64),
    Hold(u64, Arc<AtomicBool>),
}

/// Sorted runs waiting for their cascade and final merge. Owns its transient files; moves to a worker thread.
pub(crate) struct PendingRuns {
    lane: Lane,
    k: usize,
    count: u64,
    runs: Vec<RunSpan>,
    first: File,
    first_end: u64,
    second: Option<File>,
    th: Thresholds,
    #[cfg(test)]
    hook: Option<Hook>,
}

#[cfg(test)]
impl PendingRuns {
    /// After `n` items the worker fails with `DiskFull`.
    pub(crate) fn fail_after_items(mut self, n: u64) -> Self {
        self.hook = Some(Hook::Fail(n));
        self
    }

    /// After `n` items the worker panics with the payload `"injected"`.
    pub(crate) fn panic_after_items(mut self, n: u64) -> Self {
        self.hook = Some(Hook::Panic(n));
        self
    }

    /// After `n` items the worker waits for `stop`, sets `observed` and returns `Stopped`.
    pub(crate) fn hold_until_stop(mut self, n: u64, observed: Arc<AtomicBool>) -> Self {
        self.hook = Some(Hook::Hold(n, observed));
        self
    }
}

impl PendingRuns {
    #[cfg(test)]
    fn hook_at(&self, items: u64, stop: &AtomicBool) -> Result<(), FinishError> {
        match &self.hook {
            Some(Hook::Fail(n)) if *n == items => Err(FinishError::Failed(FsError::new(
                Code::DiskFull,
                std::io::Error::other("injected index failure"),
            ))),
            Some(Hook::Panic(n)) if *n == items => panic!("injected"),
            Some(Hook::Hold(n, observed)) if *n == items => {
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                observed.store(true, Ordering::SeqCst);
                Err(FinishError::Stopped)
            }
            _ => Ok(()),
        }
    }

    #[cfg(not(test))]
    fn hook_at(&self, _items: u64, _stop: &AtomicBool) -> Result<(), FinishError> {
        Ok(())
    }

    /// Runs the cascade (at most `merge_fan_in` runs remain), then merges the rest straight into the packer, which
    /// writes the packed family at `start` in `lane_file`. Checks `stop` every `tick_entries` items. Never touches
    /// the scratch space: it runs on a worker thread.
    pub(crate) fn finish(
        self,
        lane_file: &File,
        start: u64,
        stop: &AtomicBool,
    ) -> Result<FamilyMeta, FinishError> {
        self.finish_with(lane_file, start, stop, None)
    }

    fn finish_with(
        self,
        lane_file: &File,
        start: u64,
        stop: &AtomicBool,
        mut ticker: Option<&mut Tick<'_>>,
    ) -> Result<FamilyMeta, FinishError> {
        self.hook_at(0, stop)?;
        let stopped = Cell::new(false);
        let mut check = || -> Result<(), FsError> {
            if stop.load(Ordering::SeqCst) {
                stopped.set(true);
                return Err(fatal("the index build was stopped"));
            }
            if let Some(t) = ticker.as_mut() {
                t()?;
            }
            Ok(())
        };
        let classify = |e: FsError| {
            if stopped.get() { FinishError::Stopped } else { FinishError::Failed(e) }
        };
        let th = self.th;
        let k = self.k;
        let fan = th.merge_fan_in;
        let files = [&self.first, self.second.as_ref().unwrap_or(&self.first)];
        let ends = [Cell::new(self.first_end), Cell::new(0)];
        let mut runs = self.runs.clone();
        let passes = passes_for(runs.len(), fan);
        let mut cur = 0usize;
        let mut pass = 0u32;
        while runs.len() > fan {
            pass += 1;
            let (src, dst) = (files[cur], files[1 - cur]);
            let sink = Sink::File(dst, &ends[1 - cur]);
            // `cascade_pass` carries a lone trailing run over as it is, which would leave it in the other file.
            let lone = if runs.len() % fan == 1 { runs.pop() } else { None };
            let mut next = cascade_pass(
                Source::File(src),
                sink,
                k,
                th.merge_buffer,
                &runs,
                fan,
                th.tick_entries,
                pass,
                passes,
                &mut |_| check(),
            )
            .map_err(classify)?;
            if let Some(lone) = lone {
                next.push(
                    copy_run(Source::File(src), sink, k, &th, lone, &mut check)
                        .map_err(classify)?,
                );
            }
            runs = next;
            cur = 1 - cur;
        }
        let mut merger =
            Merger::new(Source::File(files[cur]), k, th.merge_buffer, &runs).map_err(classify)?;
        // The file no run is being read from: the other one after a cascade, else the tail of the only one.
        let (spill, spill_at) = match self.second {
            Some(_) => (files[1 - cur], ends[1 - cur].get()),
            None => (files[0], ends[0].get()),
        };
        let mut packer =
            Packer::new(lane_file, spill, spill_at, self.lane, k, th.index_block, start);
        let mut items = 0u64;
        let mut record = Vec::new();
        while merger.next_record_into(&mut record).map_err(classify)? {
            if th.tick_entries > 0 && items.is_multiple_of(th.tick_entries as u64) {
                check().map_err(classify)?;
            }
            self.hook_at(items, stop)?;
            packer.push_record(&record).map_err(FinishError::Failed)?;
            items += 1;
        }
        self.hook_at(items, stop)?;
        packer.finish().map_err(FinishError::Failed)
    }
}

/// Copies one run to the end of `sink` in chunks of `run_entries` items; the chunks are contiguous.
fn copy_run(
    source: Source,
    sink: Sink,
    k: usize,
    th: &Thresholds,
    run: RunSpan,
    check: &mut dyn FnMut() -> Result<(), FsError>,
) -> Result<RunSpan, FsError> {
    let mut merger = Merger::new(source, k, th.merge_buffer, &[run])?;
    let mut out: Option<RunSpan> = None;
    let mut chunk = Vec::new();
    let mut seen = 0u64;
    loop {
        chunk.clear();
        while chunk.len() < th.run_entries {
            let Some(item) = merger.next_entry()? else { break };
            chunk.push(item);
            seen += 1;
            if th.tick_entries > 0 && seen.is_multiple_of(th.tick_entries as u64) {
                check()?;
            }
        }
        if chunk.is_empty() {
            break;
        }
        let span = write_run(sink, k, th.merge_buffer, &chunk)?;
        out = Some(match out {
            None => span,
            Some(o) => {
                RunSpan { start: o.start, len: o.len + span.len, count: o.count + span.count }
            }
        });
    }
    out.ok_or_else(|| fatal("a scratch run holds no records"))
}

// ---------------------------------------------------------------------------------------------------------------
// Packing.

fn put_entry(out: &mut Vec<u8>, key: &[u8], offset: u64, len: u32) {
    out.extend_from_slice(&(key.len() as u16).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
}

/// The entry at `pos` of a level block: its capped key, offset, length, and the position after it.
fn entry_at(block: &[u8], pos: usize) -> Result<(&[u8], u64, u32, usize), FsError> {
    let head = block.get(pos..pos + 2).ok_or_else(corrupt)?;
    let key_len = usize::from(u16::from_le_bytes([head[0], head[1]]));
    let key = block.get(pos + 2..pos + 2 + key_len).ok_or_else(corrupt)?;
    let at = pos + 2 + key_len;
    let tail = block.get(at..at + 12).ok_or_else(corrupt)?;
    let offset = u64::from_le_bytes(tail[..8].try_into().map_err(|_| corrupt())?);
    let len = u32::from_le_bytes(tail[8..].try_into().map_err(|_| corrupt())?);
    Ok((key, offset, len, at + 12))
}

fn block_count(block: &[u8]) -> Result<usize, FsError> {
    let head = block.get(..2).ok_or_else(corrupt)?;
    Ok(usize::from(u16::from_le_bytes([head[0], head[1]])))
}

/// The key bytes, name bytes and total length of the record at the start of `buf` (no allocation).
fn record_at(k: usize, buf: &[u8]) -> Option<(&[u8], &[u8], usize)> {
    let key = buf.get(..k)?;
    let head = buf.get(k..k + 3)?;
    let len = usize::from(u16::from_le_bytes([head[0], head[1]]));
    let name = buf.get(k + 3..k + 3 + len)?;
    Some((key, name, k + 3 + len))
}

/// Blocks of a level and the range of each block's first entry.
type Packed = (Vec<u8>, Vec<(usize, usize)>);

/// Splits serialized level entries into `B`-byte blocks; returns the blocks and, per block, the range of its first
/// entry in `entries`.
fn pack_level(entries: &[u8], b: usize) -> Result<Packed, FsError> {
    let mut blocks: Vec<u8> = Vec::new();
    let mut firsts: Vec<(usize, usize)> = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    let mut count = 0u16;
    let mut pos = 0;
    let close = |blocks: &mut Vec<u8>, body: &mut Vec<u8>, count: &mut u16| {
        let at = blocks.len();
        blocks.resize(at + b, 0);
        blocks[at..at + 2].copy_from_slice(&count.to_le_bytes());
        blocks[at + 2..at + 2 + body.len()].copy_from_slice(body);
        body.clear();
        *count = 0;
    };
    while pos < entries.len() {
        let (_, _, _, next) = entry_at(entries, pos)?;
        let size = next - pos;
        if count > 0 && 2 + body.len() + size > b {
            close(&mut blocks, &mut body, &mut count);
        }
        if count == 0 {
            firsts.push((pos, next));
        }
        body.extend_from_slice(&entries[pos..next]);
        count += 1;
        pos = next;
    }
    if count > 0 {
        close(&mut blocks, &mut body, &mut count);
    }
    Ok((blocks, firsts))
}

#[cfg(test)]
thread_local! {
    /// The most bytes of level-0 entries (or of the blocks built from them) the packer held at once, on this thread.
    static L0_RESIDENT_HIGH: Cell<usize> = const { Cell::new(0) };
}

/// Records `n` resident level-0 bytes (tests read the high-water mark).
#[cfg(test)]
fn note_resident(n: usize) {
    L0_RESIDENT_HIGH.with(|h| h.set(h.get().max(n)));
}

#[cfg(not(test))]
fn note_resident(_n: usize) {}

/// Writes the packed layout of a sorted stream of items at `start` in a lane's file.
struct Packer<'f> {
    file: &'f File,
    lane: Lane,
    k: usize,
    b: usize,
    start: u64,
    /// Offset of the next frame.
    pos: u64,
    /// Frames not yet written, from `out_at`.
    out: Vec<u8>,
    out_at: u64,
    frame: Vec<u8>,
    frame_count: usize,
    first_key: Vec<u8>,
    /// A transient file no run is being read from, which holds the level-0 entries until the data is written.
    spill: &'f File,
    spill_start: u64,
    /// Entries not yet written to `spill`, from `spill_at`; at most one block's worth plus one entry.
    spill_buf: Vec<u8>,
    spill_at: u64,
    count: u64,
}

impl<'f> Packer<'f> {
    const FLUSH: usize = 64 * 1024;

    fn new(
        file: &'f File,
        spill: &'f File,
        spill_at: u64,
        lane: Lane,
        k: usize,
        b: usize,
        start: u64,
    ) -> Self {
        Packer {
            file,
            lane,
            k,
            b,
            start,
            pos: start,
            out: Vec::new(),
            out_at: start,
            frame: Vec::new(),
            frame_count: 0,
            first_key: Vec::new(),
            spill,
            spill_start: spill_at,
            spill_buf: Vec::new(),
            spill_at,
            count: 0,
        }
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), FsError> {
        write_all_at(self.file, bytes, at).map_err(FsError::from_io)
    }

    fn flush_out(&mut self) -> Result<(), FsError> {
        if !self.out.is_empty() {
            let out = std::mem::take(&mut self.out);
            self.write(self.out_at, &out)?;
            self.out_at += out.len() as u64;
        }
        Ok(())
    }

    fn flush_spill(&mut self) -> Result<(), FsError> {
        if !self.spill_buf.is_empty() {
            write_all_at(self.spill, &self.spill_buf, self.spill_at).map_err(FsError::from_io)?;
            self.spill_at += self.spill_buf.len() as u64;
            self.spill_buf.clear();
        }
        Ok(())
    }

    /// Adds the next record in sorted order, already encoded as `spill::encode` writes it.
    fn push_record(&mut self, record: &[u8]) -> Result<(), FsError> {
        let k = self.k;
        let header = record.get(k..k + 3).ok_or_else(corrupt)?;
        let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
        let name = record.get(k + 3..k + 3 + len).ok_or_else(corrupt)?;
        if self.frame_count > 0
            && (2 + self.frame.len() + record.len() > self.b
                || self.frame_count == usize::from(u16::MAX))
        {
            self.flush_frame()?;
        }
        if self.frame_count == 0 {
            let cap = self.b / 8;
            self.first_key.clear();
            self.first_key.extend_from_slice(&record[..k]);
            self.first_key.extend_from_slice(name);
            self.first_key.truncate(cap);
        }
        self.frame.extend_from_slice(record);
        self.frame_count += 1;
        self.count += 1;
        if 2 + self.frame.len() > self.b {
            self.flush_frame()?;
        }
        Ok(())
    }

    fn flush_frame(&mut self) -> Result<(), FsError> {
        if self.frame_count == 0 {
            return Ok(());
        }
        let total = 2 + self.frame.len();
        let size = total.div_ceil(self.b) * self.b;
        let at = self.out.len();
        self.out.resize(at + size, 0);
        self.out[at..at + 2].copy_from_slice(&(self.frame_count as u16).to_le_bytes());
        self.out[at + 2..at + total].copy_from_slice(&self.frame);
        let len = u32::try_from(size).map_err(|_| fatal("an index frame is too long"))?;
        put_entry(&mut self.spill_buf, &self.first_key, self.pos, len);
        note_resident(self.spill_buf.len());
        if self.spill_buf.len() >= self.b {
            self.flush_spill()?;
        }
        self.pos += size as u64;
        self.frame.clear();
        self.frame_count = 0;
        if self.out.len() >= Self::FLUSH {
            self.flush_out()?;
        }
        Ok(())
    }

    /// Packs level 0 by reading the spilled entries back in bounded chunks and writing each block straight to the
    /// lane at `offset`. Returns the number of blocks and the entries of level 1 (one per block).
    ///
    /// MEMORY: level 0 is never resident (a read chunk of four blocks and one block of body). Level 1 and above are
    /// built in memory: level 1 is about one entry per `B / (entry size)` level-0 entries, i.e. about 1/100 of
    /// level 0, so it is O(frames / 100) and below the 1 B per entry the spec allows for every realistic family.
    fn pack_level0(&mut self, offset: u64) -> Result<(usize, Vec<u8>), FsError> {
        let b = self.b;
        let chunk = 4 * b;
        let spill_end = self.spill_at;
        let mut read_at = self.spill_start;
        let mut buf: Vec<u8> = Vec::new();
        let mut pos = 0usize;
        let mut body: Vec<u8> = Vec::new();
        let mut count = 0u16;
        let mut first: Vec<u8> = Vec::new();
        let mut blocks = 0usize;
        let mut upper: Vec<u8> = Vec::new();
        let mut block = vec![0u8; b];
        loop {
            // The next whole entry, refilling the chunk when it is cut off.
            let next = match entry_at(&buf, pos) {
                Ok((_, _, _, next)) => Some(next),
                Err(_) if read_at < spill_end => None,
                Err(_) if pos >= buf.len() => break,
                Err(e) => return Err(e),
            };
            let Some(next) = next else {
                buf.drain(..pos);
                pos = 0;
                let want = (spill_end - read_at).min(chunk as u64) as usize;
                let old = buf.len();
                buf.resize(old + want, 0);
                let got = read_fully_at(self.spill, &mut buf[old..], read_at)
                    .map_err(FsError::from_io)?;
                if got < want {
                    return Err(fatal("the index level-0 spill ends early"));
                }
                read_at += want as u64;
                note_resident(buf.len() + body.len());
                continue;
            };
            let size = next - pos;
            if count > 0 && 2 + body.len() + size > b {
                self.write_block(&mut block, &body, count, offset + (blocks * b) as u64)?;
                blocks += 1;
                body.clear();
                count = 0;
            }
            if count == 0 {
                let (key, _, _, _) = entry_at(&buf, pos)?;
                first.clear();
                first.extend_from_slice(key);
                put_entry(&mut upper, &first, offset + (blocks * b) as u64, b as u32);
            }
            body.extend_from_slice(&buf[pos..next]);
            count += 1;
            pos = next;
        }
        if count > 0 {
            self.write_block(&mut block, &body, count, offset + (blocks * b) as u64)?;
            blocks += 1;
        }
        Ok((blocks, upper))
    }

    fn write_block(
        &self,
        block: &mut [u8],
        body: &[u8],
        count: u16,
        at: u64,
    ) -> Result<(), FsError> {
        block.fill(0);
        block[..2].copy_from_slice(&count.to_le_bytes());
        block[2..2 + body.len()].copy_from_slice(body);
        write_all_at(self.file, block, at).map_err(FsError::from_io)
    }

    fn finish(mut self) -> Result<FamilyMeta, FsError> {
        self.flush_frame()?;
        self.flush_out()?;
        self.flush_spill()?;
        if self.count == 0 {
            return Err(fatal("an index family holds no records"));
        }
        let data_end = self.pos;
        let b = self.b;
        let mut levels = vec![data_end];
        let (blocks, mut entries) = self.pack_level0(data_end)?;
        let mut offset = data_end + (blocks * b) as u64;
        let end = if blocks == 1 {
            offset
        } else {
            loop {
                levels.push(offset);
                let (packed, firsts) = pack_level(&entries, b)?;
                self.write(offset, &packed)?;
                if firsts.len() == 1 {
                    break offset + b as u64;
                }
                let mut upper = Vec::new();
                for (j, (from, to)) in firsts.iter().enumerate() {
                    let (key, _, _, _) = entry_at(&entries[*from..*to], 0)?;
                    put_entry(&mut upper, key, offset + (j * b) as u64, b as u32);
                }
                offset += packed.len() as u64;
                entries = upper;
            }
        };
        Ok(FamilyMeta {
            lane: self.lane,
            k: self.k,
            start: self.start,
            end,
            data_end,
            levels,
            count: self.count,
        })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Reading.

/// A finished family in its lane. Dropping it truncates the lane back to `start` and invalidates the cache from there.
pub(crate) struct Family<'s, 'h> {
    meta: FamilyMeta,
    // Dropped with the family: it cuts the lane back.
    _region: Region<'s, 'h>,
    space: &'s ScratchSpace<'h>,
}

/// The level-0 position after the entry a descent chose.
struct Leaf {
    block: Rc<[u8]>,
    block_off: u64,
    remaining: usize,
    pos: usize,
    frame_off: u64,
    frame_len: u32,
}

impl<'s, 'h> Family<'s, 'h> {
    /// Whether a record with exactly this name exists (a `K = 0` family).
    pub(crate) fn contains_name(&self, name: &[u8]) -> Result<bool, FsError> {
        debug_assert_eq!(self.meta.k, 0);
        self.search(&[], name, false, 0, &mut Vec::new())
    }

    /// The names of the records with this key, in name order, at most `limit + 1` of them (so the caller can tell
    /// "more than `limit`").
    pub(crate) fn names_with_key(
        &self,
        key: &[u8],
        limit: usize,
    ) -> Result<Vec<OsString>, FsError> {
        debug_assert_eq!(self.meta.k, key.len());
        let mut out = Vec::new();
        self.search(key, &[], true, limit, &mut out)?;
        Ok(out)
    }

    /// Every record in frame order, one frame in memory at a time.
    pub(crate) fn names(&self) -> impl Iterator<Item = Result<Keyed, FsError>> + '_ {
        self.frames().flat_map(|frame| match frame {
            Ok(records) => records.into_iter().map(Ok).collect::<Vec<_>>(),
            Err(e) => vec![Err(e)],
        })
    }

    /// The records of each frame in order. A frame describes itself: an oversized one holds one record whose length
    /// says how many blocks it spans.
    fn frames(&self) -> impl Iterator<Item = Result<Vec<Keyed>, FsError>> + '_ {
        let b = self.space.thresholds().index_block;
        let k = self.meta.k;
        let mut off = self.meta.start;
        let mut failed = false;
        std::iter::from_fn(move || {
            if failed || off >= self.meta.data_end {
                return None;
            }
            let result = (|| {
                let mut buf = vec![0u8; b];
                if self.space.read_at(self.meta.lane, off, &mut buf)? < b {
                    return Err(corrupt());
                }
                let name_len = buf
                    .get(2 + k..2 + k + 2)
                    .map(|h| usize::from(u16::from_le_bytes([h[0], h[1]])))
                    .ok_or_else(corrupt)?;
                let size = (2 + k + 3 + name_len).div_ceil(b) * b;
                if size > b {
                    buf.resize(size, 0);
                    if self.space.read_at(self.meta.lane, off, &mut buf)? < size {
                        return Err(corrupt());
                    }
                }
                let count = block_count(&buf)?;
                let mut records = Vec::with_capacity(count);
                let mut pos = 2;
                for _ in 0..count {
                    let (item, n) =
                        decode(k, buf.get(pos..).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
                    records.push(item);
                    pos += n;
                }
                off += size as u64;
                Ok(records)
            })();
            failed = result.is_err();
            Some(result)
        })
    }

    /// From the top block down: the last entry whose capped key is strictly below `cap_t`, or the first entry of the
    /// level when there is none (never an equal one: equal capped keys say nothing about order).
    fn descend(&self, cap_t: &[u8]) -> Result<Leaf, FsError> {
        let b = self.space.thresholds().index_block;
        let levels = &self.meta.levels;
        let mut offset = *levels.last().ok_or_else(corrupt)?;
        let mut len = b as u32;
        for level in (0..levels.len()).rev() {
            let block = self.space.read_block(self.meta.lane, offset, len as usize)?;
            let count = block_count(&block)?;
            if count == 0 {
                return Err(corrupt());
            }
            let mut pos = 2;
            let mut chosen: Option<(u64, u32, usize, usize)> = None;
            for i in 0..count {
                let (key, eo, el, next) = entry_at(&block, pos)?;
                if key < cap_t {
                    chosen = Some((eo, el, i, next));
                    pos = next;
                } else {
                    if chosen.is_none() {
                        chosen = Some((eo, el, i, next));
                    }
                    break;
                }
            }
            let (eo, el, i, next) = chosen.ok_or_else(corrupt)?;
            if level == 0 {
                return Ok(Leaf {
                    block,
                    block_off: offset,
                    remaining: count - i - 1,
                    pos: next,
                    frame_off: eo,
                    frame_len: el,
                });
            }
            offset = eo;
            len = el;
        }
        Err(corrupt())
    }

    /// The level-0 entry after the one the leaf is at (its key, frame offset and length), reading the next level-0
    /// block when this one is used up; `None` at the end of the data.
    fn next_frame(&self, leaf: &mut Leaf) -> Result<Option<(Vec<u8>, u64, u32)>, FsError> {
        let b = self.space.thresholds().index_block;
        if leaf.remaining == 0 {
            let level0_end = self.meta.levels.get(1).copied().unwrap_or(self.meta.end);
            let next = leaf.block_off + b as u64;
            if next >= level0_end {
                return Ok(None);
            }
            leaf.block = self.space.read_block(self.meta.lane, next, b)?;
            leaf.block_off = next;
            leaf.remaining = block_count(&leaf.block)?;
            leaf.pos = 2;
            if leaf.remaining == 0 {
                return Err(corrupt());
            }
        }
        let (key, off, len, next) = entry_at(&leaf.block, leaf.pos)?;
        leaf.pos = next;
        leaf.remaining -= 1;
        Ok(Some((key.to_vec(), off, len)))
    }

    /// `keys_mode` collects the names of the records with `key` (at most `limit + 1`); otherwise it looks for the
    /// record `(key, name)`. Walks the chosen frame and then on into the following frames for as long as their capped
    /// first key can still hold the target.
    fn search(
        &self,
        key: &[u8],
        name: &[u8],
        keys_mode: bool,
        limit: usize,
        out: &mut Vec<OsString>,
    ) -> Result<bool, FsError> {
        let b = self.space.thresholds().index_block;
        let k = self.meta.k;
        let mut target = key.to_vec();
        target.extend_from_slice(name);
        let cap_t = &target[..target.len().min(b / 8)];
        let mut leaf = self.descend(cap_t)?;
        let (mut off, mut len) = (leaf.frame_off, leaf.frame_len);
        loop {
            let frame = self.space.read_block(self.meta.lane, off, len as usize)?;
            let count = block_count(&frame)?;
            let mut pos = 2;
            for _ in 0..count {
                let (rk, rn, n) =
                    record_at(k, frame.get(pos..).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
                pos += n;
                let order =
                    if keys_mode { rk.cmp(key) } else { rk.cmp(key).then_with(|| rn.cmp(name)) };
                if order.is_lt() {
                    continue;
                }
                if order.is_gt() {
                    return Ok(false);
                }
                if !keys_mode {
                    return Ok(true);
                }
                out.push(os_name(rn));
                if out.len() > limit {
                    return Ok(true);
                }
            }
            match self.next_frame(&mut leaf)? {
                Some((ekey, eo, el)) if ekey.starts_with(cap_t) => {
                    off = eo;
                    len = el;
                }
                _ => return Ok(!out.is_empty()),
            }
        }
    }

    /// The level-0 entries (capped key, offset, length), in order.
    #[cfg(test)]
    fn level0_entries(&self) -> Vec<(Vec<u8>, u64, u32)> {
        let b = self.space.thresholds().index_block;
        let end = self.meta.levels.get(1).copied().unwrap_or(self.meta.end);
        let mut all = Vec::new();
        let mut off = self.meta.levels[0];
        while off < end {
            let block = self.space.read_block(self.meta.lane, off, b).unwrap();
            let mut pos = 2;
            for _ in 0..block_count(&block).unwrap() {
                let (key, eo, el, next) = entry_at(&block, pos).unwrap();
                all.push((key.to_vec(), eo, el));
                pos = next;
            }
            off += b as u64;
        }
        all
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The finish.

/// Sends on drop, so a worker that panics still wakes the poller.
struct Done(mpsc::Sender<()>);

impl Drop for Done {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        String::from("a non-text payload")
    }
}

type WorkerResult = std::thread::Result<Result<FamilyMeta, FinishError>>;

/// Finishes the two families on two threads while the listing thread keeps ticking. `names` gets its region first.
/// An `Empty` family gets no region and no thread and yields `None`. On any error the regions drop (the lanes are
/// back at their starts) and a worker's failure or panic poisons the space; a tick error does not (the scratch is
/// sound, the run is what stops).
pub(crate) fn build_two<'s, 'h>(
    space: &'s ScratchSpace<'h>,
    names: PendingFamily,
    inodes: PendingFamily,
    tick: &mut Tick<'_>,
) -> Result<(Option<Family<'s, 'h>>, Option<Family<'s, 'h>>), FsError> {
    let mut regions: Vec<(Lane, Region<'s, 'h>)> = Vec::new();
    let mut jobs: Vec<PendingRuns> = Vec::new();
    for (lane, pending, label) in [(Lane::Names, names, "names"), (Lane::Inodes, inodes, "inodes")]
    {
        if let PendingFamily::Runs(p) = pending {
            let (entries, runs) = (p.count, p.runs.len());
            tracing::debug!(entries, family = label, runs, "indexing");
            regions.push((lane, space.begin(lane, INDEX_OWNER)?));
            jobs.push(p);
        }
    }
    if jobs.is_empty() {
        return Ok((None, None));
    }
    let guards: Vec<Ref<'_, File>> =
        regions.iter().map(|(lane, _)| space.lane_file(*lane)).collect::<Result<_, _>>()?;
    let starts: Vec<u64> = regions.iter().map(|(_, r)| r.start()).collect();

    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<()>();
    let mut tick_error: Option<FsError> = None;
    let results: Vec<WorkerResult> = std::thread::scope(|scope| {
        let mut handles: Vec<Option<std::thread::ScopedJoinHandle<'_, _>>> = Vec::new();
        for ((pending, guard), start) in jobs.into_iter().zip(&guards).zip(&starts) {
            let file: &File = guard;
            let (done, stop, start) = (Done(tx.clone()), &stop, *start);
            handles.push(Some(scope.spawn(move || {
                let _done = done;
                pending.finish(file, start, stop)
            })));
        }
        let mut results: Vec<Option<WorkerResult>> = handles.iter().map(|_| None).collect();
        // Completions signalled so far: a worker signals just before its thread ends, so a signal means its handle
        // is about to report finished.
        let mut signals = 0usize;
        loop {
            if let Err(e) = tick() {
                tick_error = Some(e);
                break;
            }
            loop {
                for (slot, result) in handles.iter_mut().zip(results.iter_mut()) {
                    if slot.as_ref().is_some_and(|h| h.is_finished())
                        && let Some(h) = slot.take()
                    {
                        *result = Some(h.join());
                    }
                }
                if results.iter().flatten().count() >= signals {
                    break;
                }
                std::thread::yield_now();
            }
            let failed = results
                .iter()
                .flatten()
                .any(|r| matches!(r, Err(_) | Ok(Err(FinishError::Failed(_)))));
            if failed || handles.iter().all(Option::is_none) {
                break;
            }
            if rx.recv_timeout(Duration::from_millis(250)).is_ok() {
                signals += 1;
            }
        }
        stop.store(true, Ordering::SeqCst);
        for (slot, result) in handles.iter_mut().zip(results.iter_mut()) {
            if let Some(h) = slot.take() {
                *result = Some(h.join());
            }
        }
        results.into_iter().flatten().collect()
    });
    drop(guards);

    if let Some(e) = tick_error {
        return Err(e);
    }
    let mut metas = Vec::new();
    let mut failure: Option<FsError> = None;
    let mut panicked: Option<String> = None;
    for result in results {
        match result {
            Ok(Ok(meta)) => metas.push(meta),
            Ok(Err(FinishError::Failed(e))) => {
                failure.get_or_insert(e);
            }
            Ok(Err(FinishError::Stopped)) => {}
            Err(payload) => {
                panicked.get_or_insert_with(|| panic_text(payload.as_ref()));
            }
        }
    }
    if let Some(e) = failure {
        let returned = remade(&e);
        space.poison(e);
        return Err(returned);
    }
    if let Some(text) = panicked {
        let message = format!("index worker panicked: {text}");
        space.poison(fatal(message.clone()));
        return Err(fatal(message));
    }
    if metas.len() != regions.len() {
        return Err(fatal("an index worker stopped without a cause"));
    }
    let (mut names_family, mut inodes_family) = (None, None);
    for ((lane, region), meta) in regions.into_iter().zip(metas) {
        space.set_end(lane, meta.end);
        let family = Family { meta, _region: region, space };
        if lane == Lane::Names {
            names_family = Some(family);
        } else {
            inodes_family = Some(family);
        }
    }
    Ok((names_family, inodes_family))
}

/// The single-threaded form, on the listing thread: ticks every `tick_entries` items through a `stop` flag it never
/// sets.
pub(crate) fn build_one<'s, 'h>(
    space: &'s ScratchSpace<'h>,
    lane: Lane,
    pending: PendingFamily,
    tick: &mut Tick<'_>,
) -> Result<Option<Family<'s, 'h>>, FsError> {
    let PendingFamily::Runs(p) = pending else { return Ok(None) };
    let region = space.begin(lane, INDEX_OWNER)?;
    let stop = AtomicBool::new(false);
    let meta = {
        let file = space.lane_file(lane)?;
        match p.finish_with(&file, region.start(), &stop, Some(tick)) {
            Ok(meta) => meta,
            Err(FinishError::Failed(e)) => return Err(e),
            Err(FinishError::Stopped) => return Err(fatal("the index build was stopped")),
        }
    };
    space.set_end(lane, meta.end);
    Ok(Some(Family { meta, _region: region, space }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault_fs::FaultFs;
    use flux_fs::{DestinationRoot, DirEntry, FileSystem, FileType};
    use proptest::prelude::*;
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn thresholds() -> Thresholds {
        Thresholds::new(4, 8, 2, 16, 3).unwrap().with_index(4, 2, 4, 1 << 20, 2, 128).unwrap()
    }

    fn placed(fs: &FaultFs, t: Thresholds) -> ScratchSpace<'static> {
        fs.create_dir(Path::new("/s")).unwrap();
        let home = fs.destination_root(Path::new("/s")).unwrap();
        let space = ScratchSpace::unplaced(t, None);
        space.place(home, PathBuf::from("/s"));
        space
    }

    fn item(k: usize, key: &[u8], name: &str) -> Keyed {
        let mut bytes = [0u8; 24];
        bytes[..k].copy_from_slice(&key[..k]);
        Keyed {
            key: bytes,
            entry: DirEntry { name: OsString::from(name), file_type: FileType::File, ino: None },
        }
    }

    fn plain(name: &str) -> Keyed {
        item(0, &[], name)
    }

    /// A key of `k` bytes: the first sixteen constant for `k = 24` (as on Linux), the last eight `v` big-endian.
    fn key_of(k: usize, v: u64) -> Vec<u8> {
        let mut key = vec![7u8; k.saturating_sub(8)];
        if k >= 8 {
            key.extend_from_slice(&v.to_be_bytes());
        }
        key
    }

    fn pending(space: &ScratchSpace<'_>, lane: Lane, k: usize, items: Vec<Keyed>) -> PendingFamily {
        let mut builder = FamilyBuilder::new(space, lane, k);
        let mut tick = || Ok(());
        for it in items {
            builder.push(it, &mut tick).unwrap_or_else(|e| panic!("{e}"));
        }
        builder.into_pending().unwrap_or_else(|e| panic!("{e}"))
    }

    fn one<'s, 'h>(
        space: &'s ScratchSpace<'h>,
        lane: Lane,
        k: usize,
        items: Vec<Keyed>,
    ) -> Family<'s, 'h> {
        let p = pending(space, lane, k, items);
        let mut tick = || Ok(());
        build_one(space, lane, p, &mut tick).unwrap_or_else(|e| panic!("{e}")).expect("a family")
    }

    fn names(n: usize) -> Vec<Keyed> {
        (0..n).map(|i| plain(&format!("n{i:05}"))).collect()
    }

    fn runs_of(p: PendingFamily) -> PendingRuns {
        match p {
            PendingFamily::Runs(r) => r,
            PendingFamily::Empty => panic!("an empty family"),
        }
    }

    fn with_runs(p: PendingFamily, f: impl FnOnce(PendingRuns) -> PendingRuns) -> PendingFamily {
        PendingFamily::Runs(f(runs_of(p)))
    }

    #[test]
    fn a_names_family_answers_contains_for_every_name_and_no_other() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let family = one(&space, Lane::Names, 0, names(200));
        assert!(family.meta.levels.len() >= 2, "{:?}", family.meta);
        for i in 0..200 {
            assert!(family.contains_name(format!("n{i:05}").as_bytes()).unwrap(), "{i}");
        }
        for i in 0..200 {
            assert!(!family.contains_name(format!("n{i:05}x").as_bytes()).unwrap(), "{i}");
        }
        assert!(!family.contains_name(b"a").unwrap());
        assert!(!family.contains_name(b"z").unwrap());
        assert_eq!(family.meta.count, 200);
    }

    #[test]
    fn names_sharing_a_prefix_longer_than_the_cap_are_found() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let all: Vec<String> = (0..200).map(|i| format!("aaaaaaaaaaaaaaaa{i:03}")).collect();
        let family = one(&space, Lane::Names, 0, all.iter().map(|n| plain(n)).collect());
        for n in &all {
            assert!(family.contains_name(n.as_bytes()).unwrap(), "{n}");
        }
        assert!(family.contains_name(all.last().unwrap().as_bytes()).unwrap());
        for i in 0..200 {
            assert!(!family.contains_name(format!("n{i:05}").as_bytes()).unwrap());
        }
        assert!(!family.contains_name(b"aaaaaaaaaaaaaaaa").unwrap());
        assert!(!family.contains_name(b"aaaaaaaaaaaaaaaa200").unwrap());
        drop(family);

        let key = key_of(8, 42);
        let keyed: Vec<Keyed> = all.iter().take(40).map(|n| item(8, &key, n)).collect();
        let family = one(&space, Lane::Inodes, 8, keyed);
        let found = family.names_with_key(&key, 100).unwrap();
        let expected: Vec<OsString> = all.iter().take(40).map(OsString::from).collect();
        assert_eq!(found, expected);
        assert!(family.meta.levels.len() >= 2);
        assert!(family.names_with_key(&key_of(8, 41), 100).unwrap().is_empty());
        assert!(family.names_with_key(&key_of(8, 43), 100).unwrap().is_empty());
    }

    #[test]
    fn an_inodes_family_lists_every_name_of_a_key_in_name_order() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let items: Vec<Keyed> =
            (0..300).map(|i| item(8, &key_of(8, (i / 3) as u64), &format!("n{i:05}"))).collect();
        let family = one(&space, Lane::Inodes, 8, items);
        for g in 0..100usize {
            let expected: Vec<OsString> =
                (0..3).map(|j| OsString::from(format!("n{:05}", g * 3 + j))).collect();
            assert_eq!(family.names_with_key(&key_of(8, g as u64), 16).unwrap(), expected, "{g}");
        }
        // The key whose first name is the last record of a frame has its names on two frames.
        let mut straddling = None;
        for frame in family.frames() {
            let records = frame.unwrap();
            let last = records.last().unwrap();
            let key = last.key[..8].to_vec();
            let group = u64::from_be_bytes(key.clone().try_into().unwrap()) as usize;
            if last.entry.name == OsString::from(format!("n{:05}", group * 3)) {
                straddling = Some(key);
            }
        }
        let key = straddling.expect("a key straddles a frame boundary");
        assert_eq!(family.names_with_key(&key, 16).unwrap().len(), 3);
        assert_eq!(family.names_with_key(&key, 2).unwrap().len(), 3, "more than the limit");
        assert_eq!(family.names_with_key(&key, 1).unwrap().len(), 2);
        assert!(family.names_with_key(&key_of(8, 100), 16).unwrap().is_empty());
    }

    #[test]
    fn an_oversized_record_spans_blocks_and_is_found() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let long = format!("n00100{}", "z".repeat(494));
        assert_eq!(long.len(), 500);
        let mut items = names(200);
        items.push(plain(&long));
        let family = one(&space, Lane::Names, 0, items);
        assert!(family.contains_name(long.as_bytes()).unwrap());
        assert!(family.contains_name(b"n00100").unwrap());
        assert!(family.contains_name(b"n00101").unwrap());
        assert!(!family.contains_name(format!("{long}y").as_bytes()).unwrap());
        let span = family.meta.data_end - family.meta.start;
        assert_eq!(span % 128, 0);
        let entries = family.level0_entries();
        let big: Vec<_> = entries.iter().filter(|e| e.2 > 128).collect();
        assert_eq!(big.len(), 1, "{entries:?}");
        assert_eq!(big[0].2, 512);
        assert!(big[0].0.len() <= 16);
        assert_eq!(family.meta.count, 201);
    }

    fn inodes(n: usize) -> Vec<Keyed> {
        (0..n).map(|i| item(8, &key_of(8, (i / 2) as u64), &format!("m{i:05}"))).collect()
    }

    #[test]
    fn build_two_finishes_both_families_on_two_threads_and_ticks_while_waiting() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let a = pending(&space, Lane::Names, 0, names(200));
        let b = pending(&space, Lane::Inodes, 8, inodes(200));
        let ticks = Cell::new(0u32);
        let mut tick = || {
            ticks.set(ticks.get() + 1);
            Ok(())
        };
        let (fa, fb) = build_two(&space, a, b, &mut tick).unwrap_or_else(|e| panic!("{e}"));
        let (fa, fb) = (fa.expect("names"), fb.expect("inodes"));
        assert!(ticks.get() >= 1);
        for i in 0..200 {
            assert!(fa.contains_name(format!("n{i:05}").as_bytes()).unwrap());
        }
        for g in 0..100u64 {
            assert_eq!(fb.names_with_key(&key_of(8, g), 16).unwrap().len(), 2);
        }
        assert_eq!(space.len(Lane::Names), fa.meta.end);
        assert_eq!(space.len(Lane::Inodes), fb.meta.end);
        drop((fa, fb));
        assert_eq!((space.len(Lane::Names), space.len(Lane::Inodes)), (0, 0));
    }

    type Rec = (Vec<u8>, Vec<u8>, String);

    fn rec(k: usize, it: &Keyed) -> Rec {
        (
            it.key[..k].to_vec(),
            it.entry.name.as_encoded_bytes().to_vec(),
            format!("{:?}", it.entry.file_type),
        )
    }

    /// The records of one run, read back from a transient file.
    fn read_span(k: usize, t: &Thresholds, file: &File, span: RunSpan) -> Vec<Rec> {
        let mut merger = Merger::new(Source::File(file), k, t.merge_buffer, &[span]).unwrap();
        let mut out = Vec::new();
        while let Some(item) = merger.next_entry().unwrap() {
            out.push(rec(k, &item));
        }
        out
    }

    /// A deterministic xorshift stream.
    fn stream(seed: u64) -> impl FnMut() -> u64 {
        let mut x = seed | 1;
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        }
    }

    /// `n` items with distinct (key[..k], name) pairs: keys share their first eight bytes, many names share an
    /// eight-byte prefix, some are shorter than eight bytes or empty.
    fn adversarial_items(k: usize, n: usize, seed: u64) -> Vec<Keyed> {
        let mut next = stream(seed);
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        while out.len() < n {
            let r = next();
            let name: Vec<u8> = match r % 5 {
                0 => format!("sharedpr{}", next() % 400).into_bytes(),
                1 => format!("{}", next() % 5000).into_bytes(),
                2 => Vec::new(),
                3 => format!("sharedpr-{:x}-{}", next() % 3, next() % 50).into_bytes(),
                _ => format!("n{:x}", next()).into_bytes(),
            };
            let mut key = [0u8; 24];
            if k > 0 {
                let mut bytes = [0u8; 24];
                // A handful of distinct first words, and for k = 24 the rest varies independently.
                bytes[..8].copy_from_slice(&(next() % 4).to_be_bytes());
                for b in &mut bytes[8..] {
                    *b = (next() % 3) as u8;
                }
                key[..k].copy_from_slice(&bytes[..k]);
            }
            if !seen.insert((key[..k].to_vec(), name.clone())) {
                continue;
            }
            let file_type = match next() % 4 {
                0 => FileType::File,
                1 => FileType::Dir,
                2 => FileType::Symlink,
                _ => FileType::Other,
            };
            out.push(Keyed { key, entry: DirEntry { name: os_name(&name), file_type, ino: None } });
        }
        out
    }

    fn runs_thresholds(run_entries: usize) -> Thresholds {
        Thresholds::new(4, run_entries, 2, 16, 3)
            .unwrap()
            .with_index(4, 2, 4, 1 << 20, 2, 128)
            .unwrap()
    }

    /// The old `Packer::push` (before `push_record`), kept as the reference the new path is compared with.
    fn push_reference(p: &mut Packer<'_>, item: &Keyed) -> Result<(), FsError> {
        let mut record = Vec::new();
        encode(p.k, item, &mut record)?;
        if p.frame_count > 0
            && (2 + p.frame.len() + record.len() > p.b || p.frame_count == usize::from(u16::MAX))
        {
            p.flush_frame()?;
        }
        if p.frame_count == 0 {
            let cap = p.b / 8;
            p.first_key.clear();
            p.first_key.extend_from_slice(&item.key[..p.k]);
            p.first_key.extend_from_slice(item.entry.name.as_encoded_bytes());
            p.first_key.truncate(cap);
        }
        p.frame.extend_from_slice(&record);
        p.frame_count += 1;
        p.count += 1;
        if 2 + p.frame.len() > p.b {
            p.flush_frame()?;
        }
        Ok(())
    }

    /// Packs `items` (sorted) with the reference path or with `push_record`; the meta and every byte written.
    fn packed(
        space: &ScratchSpace<'_>,
        k: usize,
        items: &[Keyed],
        reference: bool,
    ) -> (String, Vec<u8>) {
        let file = space.transient().unwrap();
        let spill = space.transient().unwrap();
        let mut packer = Packer::new(&file, &spill, 0, Lane::Names, k, 128, 0);
        for it in items {
            if reference {
                push_reference(&mut packer, it).unwrap();
            } else {
                let mut record = Vec::new();
                encode(k, it, &mut record).unwrap();
                packer.push_record(&record).unwrap();
            }
        }
        let meta = packer.finish().unwrap();
        let mut bytes = vec![0u8; meta.end as usize];
        read_fully_at(&file, &mut bytes, 0).unwrap();
        (format!("{meta:?}"), bytes)
    }

    #[test]
    fn push_record_packs_the_bytes_and_first_keys_the_old_push_packed() {
        for k in [0usize, 8, 24] {
            let fs = FaultFs::new();
            let space = placed(&fs, runs_thresholds(500));
            let mut items = adversarial_items(k, 700, 0xD1B5_4A32_D192_ED03 ^ k as u64);
            // Long names so some first keys are cut at the cap and some frames hold a single record.
            items.push(item(k, &key_of(k, 3), &"long".repeat(60)));
            items.push(item(k, &key_of(k, 3), &"longer".repeat(40)));
            sort_keyed(k, &mut items);
            let (want_meta, want_bytes) = packed(&space, k, &items, true);
            let (got_meta, got_bytes) = packed(&space, k, &items, false);
            assert!(want_meta.contains("levels"), "{want_meta}");
            assert_eq!(got_meta, want_meta, "k = {k}");
            assert!(got_bytes == want_bytes, "k = {k}: the packed bytes differ");
        }
    }

    #[test]
    fn every_run_equals_its_chunk_sorted_by_sort_keyed_and_write_run() {
        for k in [0usize, 8, 24] {
            let t = runs_thresholds(500);
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let items = adversarial_items(k, 5003, 0x9E37_79B9_7F4A_7C15 ^ k as u64);
            let runs = runs_of(pending(&space, Lane::Names, k, items.clone()));
            assert_eq!(runs.runs.len(), 11, "k = {k}");
            let reference = space.transient().unwrap();
            let end = Cell::new(0u64);
            for (i, span) in runs.runs.iter().enumerate() {
                let mut chunk: Vec<Keyed> =
                    items[i * 500..((i + 1) * 500).min(items.len())].to_vec();
                sort_keyed(k, &mut chunk);
                let want_span =
                    write_run(Sink::File(&reference, &end), k, t.merge_buffer, &chunk).unwrap();
                let want = read_span(k, &t, &reference, want_span);
                let got = read_span(k, &t, &runs.first, *span);
                assert_eq!(span.count, chunk.len() as u64, "k = {k} run {i}");
                assert_eq!(got.len(), want.len(), "k = {k} run {i}");
                assert!(got == want, "k = {k} run {i} differs");
            }
        }
    }

    #[test]
    fn records_equal_in_key_and_name_keep_every_file_type() {
        for k in [0usize, 8, 24] {
            let t = runs_thresholds(500);
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let mut items = Vec::new();
            for i in 0..40u64 {
                for ty in [FileType::File, FileType::Dir, FileType::Symlink, FileType::Other] {
                    let mut key = [0u8; 24];
                    key[..k].copy_from_slice(&[(i % 3) as u8; 24][..k]);
                    items.push(Keyed {
                        key,
                        entry: DirEntry {
                            name: OsString::from(format!("dup{}", i % 7)),
                            file_type: ty,
                            ino: None,
                        },
                    });
                }
            }
            let runs = runs_of(pending(&space, Lane::Names, k, items.clone()));
            assert_eq!(runs.runs.len(), 1);
            let mut got = read_span(k, &t, &runs.first, runs.runs[0]);
            let mut want: Vec<Rec> = items.iter().map(|i| rec(k, i)).collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "k = {k}");
        }
    }

    #[test]
    fn an_over_long_name_is_refused_and_leaves_the_builder_usable() {
        for k in [0usize, 8, 24] {
            let t = runs_thresholds(500);
            let fs = FaultFs::new();
            let space = placed(&fs, t);
            let mut builder = FamilyBuilder::new(&space, Lane::Names, k);
            let mut tick = || Ok(());
            let key = vec![5u8; k];
            for i in 0..10 {
                builder.push(item(k, &key, &format!("early{i}")), &mut tick).unwrap();
            }
            let before = builder.buffered();
            let long = "x".repeat(u16::MAX as usize + 1);
            let err = builder.push(item(k, &key, &long), &mut tick).unwrap_err();
            assert!(err.to_string().contains("too long to spill"), "{err}");
            assert_eq!(builder.buffered(), before, "k = {k}");
            builder.push(item(k, &key, "late"), &mut tick).unwrap();
            let runs = runs_of(builder.into_pending().unwrap());
            assert_eq!(runs.count, 11, "the refused item is not counted");
            let got = read_span(k, &t, &runs.first, runs.runs[0]);
            let mut want: Vec<Rec> = (0..10)
                .map(|i| rec(k, &item(k, &key, &format!("early{i}"))))
                .chain([rec(k, &item(k, &key, "late"))])
                .collect();
            want.sort_by(|a, b| a.1.cmp(&b.1));
            assert_eq!(got, want, "k = {k}");
        }
    }

    #[test]
    fn a_flushed_run_keeps_the_buffers_capacity_and_the_next_run_is_correct() {
        let t = runs_thresholds(50);
        let fs = FaultFs::new();
        let space = placed(&fs, t);
        let mut builder = FamilyBuilder::new(&space, Lane::Names, 8);
        let mut tick = || Ok(());
        let items = adversarial_items(8, 120, 42);
        for it in &items[..49] {
            builder.push(it.clone(), &mut tick).unwrap();
        }
        let (arena_len, _, descs, _) = builder.buffered();
        assert!(arena_len > 0 && descs == 49);
        builder.push(items[49].clone(), &mut tick).unwrap();
        let (arena_after, arena_cap, descs_after, desc_cap) = builder.buffered();
        assert_eq!((arena_after, descs_after), (0, 0));
        assert!(arena_cap >= arena_len && desc_cap >= 49, "{arena_cap} {desc_cap}");
        for it in &items[50..100] {
            builder.push(it.clone(), &mut tick).unwrap();
        }
        for it in &items[100..] {
            builder.push(it.clone(), &mut tick).unwrap();
        }
        let runs = runs_of(builder.into_pending().unwrap());
        assert_eq!(runs.runs.len(), 3);
        for (i, span) in runs.runs.iter().enumerate() {
            let mut chunk: Vec<Keyed> = items[i * 50..((i + 1) * 50).min(items.len())].to_vec();
            sort_keyed(8, &mut chunk);
            let want: Vec<Rec> = chunk.iter().map(|c| rec(8, c)).collect();
            assert_eq!(read_span(8, &t, &runs.first, *span), want, "run {i}");
        }
    }

    #[test]
    fn an_empty_family_has_no_region_no_thread_and_no_file() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let builder = FamilyBuilder::new(&space, Lane::Inodes, 8);
        let empty = builder.into_pending().unwrap();
        assert!(matches!(empty, PendingFamily::Empty));
        let a = pending(&space, Lane::Names, 0, names(20));
        let creations =
            |fs: &FaultFs| fs.calls().iter().filter(|c| c.starts_with("create_scratch(")).count();
        let before = creations(&fs);
        let mut tick = || Ok(());
        let (fa, fb) = build_two(&space, a, empty, &mut tick).unwrap();
        assert!(fa.is_some() && fb.is_none());
        assert_eq!(creations(&fs), before + 1, "only the names lane file");
        let mut tick = || Ok(());
        assert!(
            build_one(&space, Lane::Identity, PendingFamily::Empty, &mut tick).unwrap().is_none()
        );
    }

    /// Runs `body` on a helper thread (the space is not `Send`, so it is made there) and fails if it has not
    /// finished within ten seconds.
    fn within_ten_seconds(body: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            body();
            let _ = tx.send(());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(()) => handle.join().unwrap(),
            Err(e) => panic!("the build did not finish within 10 s: {e:?}"),
        }
    }

    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn a_failed_tick_during_the_finish_stops_both_workers() {
        within_ten_seconds(|| {
            let fs = FaultFs::new();
            let space = placed(&fs, thresholds());
            let (oa, ob) = (flag(), flag());
            let a = pending(&space, Lane::Names, 0, names(200));
            let a = with_runs(a, |r| r.hold_until_stop(0, Arc::clone(&oa)));
            let b = pending(&space, Lane::Inodes, 8, inodes(200));
            let b = with_runs(b, |r| r.hold_until_stop(0, Arc::clone(&ob)));
            let mut tick =
                || Err(FsError::new(Code::PermissionDenied, std::io::Error::other("cancelled")));
            let err = build_two(&space, a, b, &mut tick).err().expect("the tick's error");
            assert_eq!(err.code, Code::PermissionDenied);
            assert!(oa.load(Ordering::SeqCst) && ob.load(Ordering::SeqCst), "both saw the stop");
            assert_eq!(space.len(Lane::Names), 0);
            assert_eq!(space.len(Lane::Inodes), 0);
            assert!(!space.poisoned(), "a tick error does not poison");
        });
    }

    /// `which` = 0 fails the names worker, 1 the inodes worker; the sibling holds until stopped.
    fn failing_pair(
        which: usize,
        space: &ScratchSpace<'_>,
    ) -> (Result<(), FsError>, Arc<AtomicBool>) {
        let observed = flag();
        let a = pending(space, Lane::Names, 0, names(200));
        let b = pending(space, Lane::Inodes, 8, inodes(200));
        let (a, b) = if which == 0 {
            (
                with_runs(a, |r| r.fail_after_items(3)),
                with_runs(b, |r| r.hold_until_stop(0, Arc::clone(&observed))),
            )
        } else {
            (
                with_runs(a, |r| r.hold_until_stop(0, Arc::clone(&observed))),
                with_runs(b, |r| r.fail_after_items(3)),
            )
        };
        let mut tick = || Ok(());
        let result = build_two(space, a, b, &mut tick).map(|_| ());
        (result, observed)
    }

    #[test]
    fn a_worker_error_is_returned_and_the_sibling_is_stopped() {
        for which in 0..2 {
            let fs = FaultFs::new();
            let space = placed(&fs, thresholds());
            let (result, observed) = failing_pair(which, &space);
            let err = result.expect_err("the worker's error");
            assert_eq!(err.code, Code::DiskFull, "which {which}: {err}");
            assert!(observed.load(Ordering::SeqCst), "the sibling saw the stop");
            assert_eq!(space.len(Lane::Names), 0);
            assert_eq!(space.len(Lane::Inodes), 0);
        }
    }

    #[test]
    fn a_worker_error_poisons_the_space() {
        for which in 0..2 {
            let fs = FaultFs::new();
            let space = placed(&fs, thresholds());
            let (result, _) = failing_pair(which, &space);
            assert!(result.is_err());
            let poison = space.poison_error().expect("poisoned");
            assert_eq!(poison.code, Code::DiskFull);
            assert!(space.begin(Lane::Walk, 1).is_err());
        }
    }

    #[test]
    fn a_worker_panic_is_fatal_and_poisons() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let observed = flag();
        let a = pending(&space, Lane::Names, 0, names(200));
        let a = with_runs(a, |r| r.panic_after_items(3));
        let b = pending(&space, Lane::Inodes, 8, inodes(200));
        let b = with_runs(b, |r| r.hold_until_stop(0, Arc::clone(&observed)));
        let mut tick = || Ok(());
        let err = build_two(&space, a, b, &mut tick).err().expect("a fatal error");
        let text = err.to_string();
        assert!(text.contains("index worker panicked"), "{text}");
        assert!(text.contains("injected"), "{text}");
        assert!(space.poison_error().is_some());
        assert!(observed.load(Ordering::SeqCst));
        assert_eq!((space.len(Lane::Names), space.len(Lane::Inodes)), (0, 0));
    }

    #[test]
    fn the_block_cache_counts_misses_and_hits() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let family = one(&space, Lane::Names, 0, names(200));
        space.reset_cache_misses();
        assert!(family.contains_name(b"n00000").unwrap());
        let first = space.cache_misses();
        assert_eq!(first, family.meta.levels.len() as u64 + 1);
        assert!(family.contains_name(b"n00000").unwrap());
        assert_eq!(space.cache_misses(), first, "the second lookup is all hits");
    }

    #[test]
    fn the_block_cache_evicts_when_a_big_family_is_walked() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let family = one(&space, Lane::Names, 0, names(2_000));
        assert!(family.meta.levels.len() >= 3);
        space.reset_cache_misses();
        assert!(family.contains_name(b"n00000").unwrap());
        let first = space.cache_misses();
        assert_eq!(first, family.meta.levels.len() as u64 + 1);
        assert!(family.contains_name(b"n00000").unwrap());
        assert_eq!(space.cache_misses(), first, "warm");
        for i in (100..2_000).step_by(97) {
            assert!(family.contains_name(format!("n{i:05}").as_bytes()).unwrap());
        }
        let before = space.cache_misses();
        assert!(family.contains_name(b"n00000").unwrap());
        assert!(space.cache_misses() > before, "four slots cannot hold the first path any more");
    }

    #[test]
    fn a_truncated_lanes_blocks_are_not_served_to_the_next_family() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let a: Vec<Keyed> = (0..200).map(|i| plain(&format!("a{i:03}"))).collect();
        let family_a = one(&space, Lane::Names, 0, a);
        for i in (0..200).step_by(10) {
            assert!(family_a.contains_name(format!("a{i:03}").as_bytes()).unwrap());
        }
        let start_a = family_a.meta.start;
        drop(family_a);
        let b: Vec<Keyed> = (0..200).map(|i| plain(&format!("b{i:03}"))).collect();
        let family_b = one(&space, Lane::Names, 0, b);
        assert_eq!(family_b.meta.start, start_a, "built at the same offsets");
        space.reset_cache_misses();
        assert!(family_b.contains_name(b"b000").unwrap());
        assert!(space.cache_misses() > 0, "B's first lookup read the file");
        for i in 0..200 {
            assert!(!family_b.contains_name(format!("a{i:03}").as_bytes()).unwrap(), "a{i:03}");
            assert!(family_b.contains_name(format!("b{i:03}").as_bytes()).unwrap(), "b{i:03}");
        }
    }

    #[test]
    fn a_family_of_one_run_has_one_level_and_a_family_of_many_has_more() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let small = one(&space, Lane::Names, 0, names(4));
        assert_eq!(small.meta.levels.len(), 1);
        for i in 0..4 {
            assert!(small.contains_name(format!("n{i:05}").as_bytes()).unwrap());
        }
        drop(small);
        let big = one(&space, Lane::Names, 0, names(2_000));
        assert!(big.meta.levels.len() >= 3, "{:?}", big.meta.levels);
        for i in 0..2_000 {
            assert!(big.contains_name(format!("n{i:05}").as_bytes()).unwrap(), "{i}");
        }
    }

    #[test]
    fn the_packer_holds_only_a_few_blocks_of_level_zero_at_once() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        L0_RESIDENT_HIGH.with(|h| h.set(0));
        let family = one(&space, Lane::Names, 0, names(2_000));
        let high = L0_RESIDENT_HIGH.with(Cell::get);
        assert!(family.meta.levels.len() >= 3, "{:?}", family.meta.levels);
        assert_eq!(family.level0_entries().len(), 143, "the level-0 entries are all on disk");
        assert!(high <= 8 * 128, "level 0 was resident: {high} bytes");
        for i in 0..2_000 {
            assert!(family.contains_name(format!("n{i:05}").as_bytes()).unwrap(), "{i}");
        }
        assert!(!family.contains_name(b"n01999x").unwrap());
    }

    #[test]
    fn names_yields_every_record_in_order() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let family = one(&space, Lane::Names, 0, names(200));
        let got: Vec<OsString> = family.names().map(|r| r.unwrap().entry.name).collect();
        let expected: Vec<OsString> =
            (0..200).map(|i| OsString::from(format!("n{i:05}"))).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn a_stopped_single_thread_build_reports_a_tick_error() {
        let fs = FaultFs::new();
        let space = placed(&fs, thresholds());
        let p = pending(&space, Lane::Identity, 0, names(50));
        let mut tick =
            || Err(FsError::new(Code::PermissionDenied, std::io::Error::other("cancelled")));
        let err = build_one(&space, Lane::Identity, p, &mut tick).err().expect("tick error");
        assert_eq!(err.code, Code::PermissionDenied);
        assert_eq!(space.len(Lane::Identity), 0);
        assert!(!space.poisoned());
    }

    fn model_strategy() -> impl Strategy<Value = (usize, bool, Vec<(u8, String)>)> {
        (
            prop_oneof![Just(0usize), Just(8usize), Just(24usize)],
            any::<bool>(),
            proptest::collection::vec((0u8..8, "[a-c]{1,14}"), 1..600),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test]
        fn family_lookups_agree_with_a_btreemap((k, shared, raw) in model_strategy()) {
            let fs = FaultFs::new();
            let space = placed(&fs, thresholds());
            let mut oracle: BTreeMap<Vec<u8>, BTreeSet<String>> = BTreeMap::new();
            for (pool, name) in &raw {
                let name = if shared { format!("pppppppppppppppp{name}") } else { name.clone() };
                let key = if k == 0 { Vec::new() } else { key_of(k, u64::from(*pool)) };
                oracle.entry(key).or_default().insert(name);
            }
            let mut items = Vec::new();
            for (key, set) in &oracle {
                for name in set {
                    items.push(item(k, key, name));
                }
            }
            // Insert in a scrambled order: the builder sorts.
            items.reverse();
            let family = one(&space, Lane::Names, k, items);
            for (key, set) in &oracle {
                if k == 0 {
                    for name in set {
                        prop_assert!(family.contains_name(name.as_bytes()).unwrap(), "{name}");
                        let absent = format!("{name}\u{1}");
                        prop_assert!(!family.contains_name(absent.as_bytes()).unwrap());
                    }
                    prop_assert!(!family.contains_name(b"").unwrap());
                } else {
                    for limit in [1usize, 4, 16, 1000] {
                        let expected: Vec<OsString> =
                            set.iter().take(limit + 1).map(OsString::from).collect();
                        prop_assert_eq!(family.names_with_key(key, limit).unwrap(), expected);
                    }
                }
            }
            if k > 0 {
                prop_assert!(family.names_with_key(&key_of(k, 9), 16).unwrap().is_empty());
            }
        }
    }
}

/// The scale harness for the destination index criteria (spec 6.3). Every test is `#[ignore]`: they run only by
/// name under `--run-ignored only --no-capture`, sized by `FLUX_SCALE_N` (default 10,000,000) and placed under
/// `FLUX_SCALE_DIR` (default `~/.cache/flux-scale`, which should be ext4). Rates and latencies are printed, never
/// asserted; correctness always is, and the structural cache-miss bounds only from 5,000,000 names up.
#[cfg(test)]
pub(crate) mod scale_tests {
    use super::*;
    use flux_fs::{DestinationRoot, DirEntry, FileType};
    use flux_platform::StdFileSystem;
    use std::path::PathBuf;
    use std::time::Instant;

    pub(crate) fn mix(mut x: u64) -> u64 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^ (x >> 31)
    }

    /// The benchmark's `name(i)`: 24 pseudo-random printable bytes (0x21..=0x7E).
    pub(crate) fn name(i: u64) -> [u8; 24] {
        let mut n = [0u8; 24];
        for k in 0..3u64 {
            let b = mix(i * 3 + k).to_le_bytes();
            for j in 0..8 {
                n[8 * k as usize + j] = b[j] % 94 + 0x21;
            }
        }
        n
    }

    pub(crate) fn dino(i: u64) -> u64 {
        mix(i) | 1
    }

    pub(crate) fn scale_n() -> u64 {
        std::env::var("FLUX_SCALE_N").ok().map_or(10_000_000, |v| {
            v.replace('_', "")
                .parse()
                .unwrap_or_else(|_| panic!("FLUX_SCALE_N is not a number: {v}"))
        })
    }

    /// A fresh directory under `FLUX_SCALE_DIR`, removed when dropped (also on a failed assertion).
    pub(crate) struct ScaleDir(pub(crate) PathBuf);

    impl ScaleDir {
        pub(crate) fn new(tag: &str) -> ScaleDir {
            let base = std::env::var_os("FLUX_SCALE_DIR").map(PathBuf::from).unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cache/flux-scale")
            });
            let dir = base.join(format!("{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create the scale directory");
            ScaleDir(dir)
        }
    }

    impl Drop for ScaleDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One field of `/proc/self/status` in kB (`RssAnon:`, `VmRSS:`).
    pub(crate) fn status_kb(field: &str) -> u64 {
        let text = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
        text.lines()
            .find_map(|l| l.strip_prefix(field))
            .and_then(|r| r.trim().trim_end_matches("kB").trim().parse().ok())
            .unwrap_or(0)
    }

    /// Median and p99 of `lat` (nanoseconds) in microseconds.
    pub(crate) fn percentiles(lat: &mut [u64]) -> (f64, f64) {
        lat.sort_unstable();
        let at = |q: usize| lat[(lat.len() * q / 100).min(lat.len() - 1)] as f64 / 1000.0;
        (at(50), at(99))
    }

    fn entry(bytes: &[u8]) -> DirEntry {
        DirEntry { name: crate::spill::os_name(bytes), file_type: FileType::File, ino: None }
    }

    fn report(label: &str, lat: &mut [u64], misses: u64) {
        let (median, p99) = percentiles(lat);
        let probes = lat.len() as u64;
        println!(
            "{label}: probes={probes} median_us={median:.1} p99_us={p99:.1} cache_misses={misses} misses_per_probe={:.3}",
            misses as f64 / probes as f64
        );
    }

    #[test]
    #[ignore = "scale harness: run by name with FLUX_SCALE_N, under --no-capture"]
    fn tier1_criteria() {
        let n = scale_n();
        let scale = ScaleDir::new("tier1");
        let space = ScratchSpace::unplaced(Thresholds::default(), None);
        space.place(StdFileSystem.destination_root(&scale.0).unwrap(), scale.0.clone());
        println!("tier1: n={n} dir={}", scale.0.display());

        let gen_started = Instant::now();
        let mut sink = 0u64;
        for i in 0..n {
            sink ^= name(i)[0] as u64 ^ dino(i);
        }
        println!("gen_only_s={:.2} (sink {sink})", gen_started.elapsed().as_secs_f64());
        let started = Instant::now();
        let mut names_builder = FamilyBuilder::new(&space, Lane::Names, 0);
        let mut inodes_builder = FamilyBuilder::new(&space, Lane::Inodes, 8);
        let mut tick = || Ok(());
        for i in 0..n {
            let nm = name(i);
            names_builder.push(Keyed::plain(entry(&nm)), &mut tick).unwrap();
            let mut key = [0u8; 24];
            key[..8].copy_from_slice(&dino(i).to_be_bytes());
            inodes_builder.push(Keyed { key, entry: entry(&nm) }, &mut tick).unwrap();
        }
        let push_s = started.elapsed().as_secs_f64();
        let pending_started = Instant::now();
        let pending_names = names_builder.into_pending().unwrap();
        let pending_inodes = inodes_builder.into_pending().unwrap();
        let pending_s = pending_started.elapsed().as_secs_f64();
        let runs_of = |p: &PendingFamily| match p {
            PendingFamily::Runs(r) => r.runs.len(),
            PendingFamily::Empty => 0,
        };
        let (runs_names, runs_inodes) = (runs_of(&pending_names), runs_of(&pending_inodes));
        let finish_started = Instant::now();
        let (names, inodes) = build_two(&space, pending_names, pending_inodes, &mut tick).unwrap();
        let (names, inodes) = (names.expect("a names family"), inodes.expect("an inodes family"));
        let finish_s = finish_started.elapsed().as_secs_f64();
        let build = started.elapsed().as_secs_f64();
        println!("split: push_s={push_s:.2} pending_s={pending_s:.2} finish_s={finish_s:.2}");
        println!("build_s={build:.2} names_per_s={:.0}", n as f64 / build);
        println!("runs_names={runs_names} runs_inodes={runs_inodes}");
        println!(
            "levels_names={} levels_inodes={}",
            names.meta.levels.len(),
            inodes.meta.levels.len()
        );
        println!(
            "index_bytes_names={} index_bytes_inodes={}",
            space.len(Lane::Names),
            space.len(Lane::Inodes)
        );
        println!(
            "rss_anon_kb_after_build={} vm_rss_kb_after_build={}",
            status_kb("RssAnon:"),
            status_kb("VmRSS:")
        );

        let probes = 100_000usize.min(n as usize);
        // Walk order: the names in frame order, as the walk of the destination lists them.
        let walk: Vec<Vec<u8>> = names
            .names()
            .take(probes)
            .map(|k| k.unwrap().entry.name.as_encoded_bytes().to_vec())
            .collect();
        assert_eq!(walk.len(), probes);

        let mut lat = Vec::with_capacity(probes);
        let before = space.cache_misses();
        for nm in &walk {
            let t = Instant::now();
            let found = names.contains_name(nm).unwrap();
            lat.push(t.elapsed().as_nanos() as u64);
            assert!(found, "a walk-order name is missing");
        }
        let walk_misses = space.cache_misses() - before;
        report("names_walk_order", &mut lat, walk_misses);

        lat.clear();
        let mut s = 12_345u64;
        let before = space.cache_misses();
        for _ in 0..probes {
            s = mix(s);
            let nm = name(s % n);
            let t = Instant::now();
            let found = names.contains_name(&nm).unwrap();
            lat.push(t.elapsed().as_nanos() as u64);
            assert!(found, "a random name is missing");
        }
        let random_misses = space.cache_misses() - before;
        report("names_random", &mut lat, random_misses);

        lat.clear();
        s = 12_345;
        let before = space.cache_misses();
        for _ in 0..probes {
            s = mix(s);
            let i = s % n;
            let t = Instant::now();
            let got = inodes.names_with_key(&dino(i).to_be_bytes(), 16).unwrap();
            lat.push(t.elapsed().as_nanos() as u64);
            let want = crate::spill::os_name(&name(i));
            assert!(got.contains(&want), "the inode lookup of entry {i} lost its name");
        }
        let inode_misses = space.cache_misses() - before;
        report("inodes_random", &mut lat, inode_misses);

        // A byte outside 0x21..=0x7E in front: never generated.
        for j in 0..1_000u64 {
            let mut nm = name(j + n + 7);
            nm[0] = 0x7F;
            assert!(!names.contains_name(&nm).unwrap(), "a never-generated name was found ({j})");
        }
        println!("absent_probes=1000 all_false=true");

        let per = |m: u64| m as f64 / probes as f64;
        if n >= 5_000_000 {
            assert!(per(walk_misses) <= 0.05, "walk-order misses/probe {}", per(walk_misses));
            assert!(per(random_misses) <= 3.0, "names random misses/probe {}", per(random_misses));
            assert!(per(inode_misses) <= 3.0, "inodes random misses/probe {}", per(inode_misses));
            println!("structural assertions: ran");
        } else {
            println!("structural assertions: SKIPPED (n={n} < 5000000)");
        }
        println!("rss_anon_kb_after_lookups={}", status_kb("RssAnon:"));
    }
}
