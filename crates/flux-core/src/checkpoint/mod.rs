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

pub use decide::Discard;
pub(crate) use decide::{Gate, Start, Verdict, decide, decide_full};
pub(crate) use sink::{FileSink, SinkError, mtime_pair};

/// Files shorter than this are not checkpointed.
pub(crate) const CHECKPOINT_MIN_LEN: u64 = 64 << 20;
/// Chunks coalesced into one `Range` message.
pub(crate) const RANGE_CHUNKS: usize = 16;

/// How much of a checkpointed file a resume re-reads before it trusts the log (cut 9f-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResumeVerify {
    #[default]
    Chunks,
    Full,
}

/// Everything a run decides about checkpointing; the fields are injectable so tests can use small fixtures.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CheckpointPolicy {
    pub min_len: u64,
    pub range_chunks: usize,
    pub wal: WalConfig,
    pub verify: ResumeVerify,
}

impl CheckpointPolicy {
    pub(crate) const DEFAULT: CheckpointPolicy = CheckpointPolicy {
        min_len: CHECKPOINT_MIN_LEN,
        range_chunks: RANGE_CHUNKS,
        wal: WalConfig::DEFAULT,
        verify: ResumeVerify::Chunks,
    };
}

#[derive(Debug)]
pub(crate) enum CheckpointOpenError {
    /// The segment's identity is unavailable, so it cannot be reopened safely.
    Unavailable(String),
    /// Mid-log damage (`WAL_CORRUPT`) or a sequence conflict (`WAL_SEQUENCE_CONFLICT`): the segment is not usable, and
    /// the run sets it aside (owner ruling 23). `why` is the `WalError` text.
    Corrupt {
        code: &'static str,
        why: String,
    },
    /// An unknown format or checksum version: a newer Flux wrote it, so the run stops (owner ruling 23).
    FormatUnsupported(String),
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
            Err(e @ WalError::Corrupt { .. }) => {
                Err(CheckpointOpenError::Corrupt { code: "WAL_CORRUPT", why: e.to_string() })
            }
            Err(e @ WalError::SequenceConflict { .. }) => Err(CheckpointOpenError::Corrupt {
                code: "WAL_SEQUENCE_CONFLICT",
                why: e.to_string(),
            }),
            Err(e @ WalError::Unsupported { .. }) => {
                Err(CheckpointOpenError::FormatUnsupported(e.to_string()))
            }
        }
    }

    /// The tail the open-time replay cut from the segment, if any (spec decision 7.1: every cut is reported).
    pub(crate) fn discarded_tail(&self) -> Option<crate::wal::replay::DiscardedTail> {
        self.scan.as_ref().and_then(|s| s.discarded_tail.clone())
    }

    pub(crate) fn min_len(&self) -> u64 {
        self.policy.min_len
    }

    pub(crate) fn durability(&self) -> Durability {
        self.policy.wal.durability
    }

    pub(crate) fn resume_verify(&self) -> ResumeVerify {
        self.policy.verify
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
    ///
    /// A heartbeat error from sending the final `Stop` is SWALLOWED here: the signature has no place for it. The run
    /// records it in its pulse, so the caller must check `pulse.failed()` after this returns and end the run Failed,
    /// never Completed, even when the copy itself succeeded (`run::tree_with`).
    pub(crate) fn finish(self, beat: &Heartbeat<'_>) -> Failure {
        let Checkpoints { sender, thread, .. } = self;
        let _ = sender.send(Msg::Stop, beat);
        drop(sender);
        thread.join()
    }

    /// Tell the writer thread the run is abandoned, now: it begins no new flush from here on. A flush already in flight
    /// may complete or tear. The final `abandon` or `finish` still follows.
    pub(crate) fn mark_abandoned(&self) {
        self.sender.mark_abandoned();
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

/// Cut 9g: what a file checkpoints through.
pub(crate) enum Hook<'r, 'a, D: DirHandle + Send + 'static> {
    /// A WAL-backed context (tree files, cuts 9f-1/9f-2).
    Logged(&'r FileCkpt<'a, D>),
    /// No log (a single file). `plan_resume` runs as `full`; no `FileSink` exists.
    LogFree { min_len: u64 },
}

// Manual impls: a derive would demand `D: Clone`, and the hook holds only a reference.
impl<D: DirHandle + Send + 'static> Clone for Hook<'_, '_, D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D: DirHandle + Send + 'static> Copy for Hook<'_, '_, D> {}

impl<D: DirHandle + Send + 'static> Hook<'_, '_, D> {
    /// The smallest source length that checkpoints (and so resumes).
    pub(crate) fn min_len(&self) -> u64 {
        match self {
            Hook::Logged(fc) => fc.run.min_len(),
            Hook::LogFree { min_len } => *min_len,
        }
    }
}
