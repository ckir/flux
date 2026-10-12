// No caller until cut 9f-1 plan 2 task 6 wires it into the copy loop; task 10 removes this line.
#![allow(dead_code, unused_imports)]

//! The run's WAL handle and the per-file sink (cut 9f-1 plan 2).
//!
//! `Checkpoints` owns the WAL writer thread for one run; `FileSink` turns the bytes of one file being copied
//! into chunk digests and sends them as `Range` records.

mod decide;
mod sink;

use crate::copy::Heartbeat;
use crate::wal::replay::{FileState, Scan, WalError, load_file};
use crate::wal::writer::{
    self, DIR_NAME, Failure, Msg, SEGMENT_NAME, WalConfig, WalSender, WalThread,
};
use flux_fs::{DirHandle, Durability, FsError};
use std::ffi::OsStr;
use std::io;

pub(crate) use decide::{Discard, Gate, Start, Verdict, decide};
pub(crate) use sink::{FileSink, SinkError};

/// Files shorter than this are not checkpointed.
pub(crate) const CHECKPOINT_MIN_LEN: u64 = 64 << 20;
/// Chunks coalesced into one `Range` message.
pub(crate) const RANGE_CHUNKS: usize = 16;

/// Everything a run decides about checkpointing; the fields are injectable so tests can use small fixtures.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckpointPolicy {
    pub min_len: u64,
    pub range_chunks: usize,
    pub wal: WalConfig,
}

impl CheckpointPolicy {
    pub(crate) const DEFAULT: CheckpointPolicy = CheckpointPolicy {
        min_len: CHECKPOINT_MIN_LEN,
        range_chunks: RANGE_CHUNKS,
        wal: WalConfig::DEFAULT,
    };
}

#[derive(Debug)]
pub(crate) enum CheckpointOpenError {
    /// The segment's identity is unavailable, so it cannot be reopened safely.
    Unavailable(String),
    /// The segment is corrupt, unsupported, or continues another run's sequence.
    Corrupt(String),
    Fatal(FsError),
}

/// The run's handle on the WAL.
pub(crate) struct Checkpoints<D: DirHandle + Send + 'static> {
    sender: WalSender<D::Writer>,
    thread: WalThread,
    scan: Option<Scan>,
    /// A second handle on the workspace, kept for `load`: the writer thread owns `workspace`.
    read_dir: D,
    policy: CheckpointPolicy,
}

impl<D: DirHandle + Send + 'static> Checkpoints<D> {
    pub(crate) fn open(
        workspace: D,
        read_dir: D,
        operation_id: &str,
        generation: u64,
        policy: CheckpointPolicy,
    ) -> Result<Checkpoints<D>, CheckpointOpenError> {
        match writer::open(workspace, operation_id.as_bytes().to_vec(), generation, policy.wal) {
            Ok((sender, thread, scan)) => {
                Ok(Checkpoints { sender, thread, scan, read_dir, policy })
            }
            Err(WalError::Io(e)) if e.source.kind() == io::ErrorKind::Unsupported => {
                Err(CheckpointOpenError::Unavailable(e.source.to_string()))
            }
            Err(WalError::Io(e)) => Err(CheckpointOpenError::Fatal(e)),
            Err(other) => Err(CheckpointOpenError::Corrupt(format!("{other:?}"))),
        }
    }

    pub(crate) fn min_len(&self) -> u64 {
        self.policy.min_len
    }

    pub(crate) fn durability(&self) -> Durability {
        self.policy.wal.durability
    }

    /// The index of the segment that existed when the run opened.
    pub(crate) fn scan(&self) -> Option<&Scan> {
        self.scan.as_ref()
    }

    /// Pass two for one file, over a fresh read of the segment through `read_dir`. With no segment at open
    /// there is nothing to read, and the filesystem is not touched.
    pub(crate) fn load(&self, file_id: &[u8]) -> Result<Option<FileState>, WalError> {
        let Some(scan) = &self.scan else {
            return Ok(None);
        };
        if !scan.per_file.contains_key(file_id) {
            return Ok(None);
        }
        let dir = self.read_dir.open_dir(OsStr::new(DIR_NAME)).map_err(WalError::Io)?;
        let reader = dir.open_read(OsStr::new(SEGMENT_NAME)).map_err(WalError::Io)?;
        load_file(reader, scan, file_id)
    }

    /// `Stop` (which flushes), join, and the final failure. When `Stop` cannot be delivered (the heartbeat
    /// failed, or the thread is gone) the sender is dropped undelivered, which abandons the run's log.
    pub(crate) fn finish(self, beat: &Heartbeat<'_>) -> Failure {
        let Checkpoints { sender, thread, .. } = self;
        let _ = sender.send(Msg::Stop, beat);
        drop(sender);
        thread.join()
    }

    /// Drop the sender WITHOUT `Stop` and join: the thread exits without flushing what is pending.
    pub(crate) fn abandon(self) {
        let Checkpoints { sender, thread, .. } = self;
        drop(sender);
        let _ = thread.join();
    }
}

/// What a file needs to checkpoint itself.
pub(crate) struct FileCkpt<'a, D: DirHandle + Send + 'static> {
    pub run: &'a Checkpoints<D>,
    pub file_id: Vec<u8>,
    pub destination: Vec<u8>,
}
