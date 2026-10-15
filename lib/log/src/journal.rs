//! The journal persistence engine — turning admitted records into durable,
//! hash-chained, per-stream segments.
//!
//! [`Journal`] sits above the admission decision ([`crate::Ingress`]) and below
//! the concrete storage the userland journal service provides. It owns the
//! per-stream state that must persist across every committed record — the
//! append sequence (through its [`Ingress`]), the running record hash chain,
//! and the current open [`SegmentWriter`] — and drives the segment lifecycle:
//! open a stream's first segment from its genesis, append committed record
//! bodies to it, and when it fills, close it (sealing audit/security streams),
//! hand the immutable bytes to the [`SegmentStore`], and reopen a fresh segment
//! that chains onto the one just closed.
//!
//! The engine is deliberately storage-agnostic: it never names a filesystem
//! syscall. A caller supplies one working buffer per stream it will write and a
//! [`SegmentStore`] sink; the FS-backed store and the IPC ingress endpoint are
//! the userland service's concern, layered on top. Everything here is `no_std`
//! and allocation-free, and every path fails closed — a record that cannot be
//! encoded or does not fit an empty segment is rejected, never partially
//! written.

use tairix_abi::{BootId, Duration64, Errno, FieldName, FieldValue, Origin, WallClockReading};
use tairix_crypto::Sha256Digest;

use crate::attest::{stream_genesis, LogAttestationKey};
use crate::authority::{derive_source, resolve_stream, SourceName};
use crate::bootring::{BootRing, LossRange};
use crate::dict::DictionaryBuilder;
use crate::ingress::{Admission, Ingress};
use crate::ratelimit::{DropReport, RateDecision, RateLimiter};
use crate::record::{CallerContent, LogRecord};
use crate::segment::{
    FinishedSegment, SegmentError, SegmentHeader, SegmentWriter, MAX_RECORD_PAYLOAD,
    SEGMENT_FOOTER_LEN, SEGMENT_HEADER_LEN,
};
use crate::stream::Stream;
use crate::{Level, STREAM_COUNT};

/// A sink for closed, immutable segment images.
///
/// The [`Journal`] calls [`store_segment`](Self::store_segment) exactly once
/// per closed segment, in append order within each stream, passing the whole
/// segment image (`[header .. footer]`). The concrete store — a directory under
/// `/System/Logs/<stream>/` reached over the filesystem syscalls — is the
/// userland journal service's concern; the engine never names it.
pub trait SegmentStore {
    /// A store-specific persistence error (an I/O failure, a full volume).
    type Error;

    /// Persist one closed segment image.
    ///
    /// A segment is immutable once written, so a store refuses an image whose
    /// place is already taken rather than overwrite it.
    ///
    /// # Errors
    ///
    /// Returns the store's own error if the segment cannot be persisted. The
    /// journal keeps the segment and offers it again before accepting another
    /// record on its stream.
    fn store_segment(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

/// Why a [`Journal`] operation failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JournalError<E> {
    /// The record body could not be encoded — invalid or oversized caller
    /// content (a message/field past its bound, too many fields). The record
    /// is rejected whole; nothing is written.
    Encode(Errno),
    /// A segment could not be built or the record does not fit even a fresh,
    /// empty segment — rejected whole rather than partially written.
    Segment(SegmentError),
    /// The underlying [`SegmentStore`] failed to persist a closed segment.
    Store(E),
}

impl<E> From<SegmentError> for JournalError<E> {
    fn from(e: SegmentError) -> Self {
        JournalError::Segment(e)
    }
}

/// What a stream's working buffer holds.
enum Slot<'a> {
    /// Nothing: no segment is open.
    Free(&'a mut [u8]),
    /// The open segment.
    Open(SegmentWriter<'a>),
    /// A closed segment the store has not yet taken. The stream accepts
    /// nothing more until the store takes it.
    Unstored(FinishedSegment<'a>),
}

/// Per-stream persistence state.
///
/// One per closed [`Stream`], indexed by [`Stream::as_u8`]. It holds the
/// stream's working buffer, the segment-local string dictionary the open
/// segment encodes through, the running chain hash a fresh segment continues
/// from (the stream genesis until the first segment closes, then the last
/// closed segment's hash), and the next segment id.
struct StreamState<'a> {
    /// The working buffer. Empty only if a segment could not be opened or
    /// closed over it, which the journal's checks before both rule out.
    slot: Option<Slot<'a>>,
    /// The open segment's string dictionary (reset on each segment open).
    dict: DictionaryBuilder,
    /// The hash a fresh segment chains from: the stream genesis, then the last
    /// closed segment's `segment_hash`.
    prev_hash: Sha256Digest,
    /// The next segment id to assign for this stream.
    next_segment_id: u64,
}

/// The working buffer the journal service gives each stream, which bounds the
/// segment it builds there: a full buffer rotates the segment. 64 KiB keeps
/// rotation infrequent at a modest heap cost across the streams, and the
/// system volume's reserve is sized over it.
pub const SEGMENT_BUF_BYTES: usize = 64 * 1024;

/// What ingress stamps one record with: the CPU it arrived on, that CPU's own
/// record sequence, and the clock readings taken as it was admitted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stamp {
    /// The CPU the record arrived on.
    pub cpu: u32,
    /// The record's place in that CPU's own sequence.
    pub cpu_seq: u64,
    /// Monotonic time at admission.
    pub monotonic: Duration64,
    /// The wall-clock reading at admission, with its trust state.
    pub wall: WallClockReading,
}

/// Where and when one record is appended: its stream and the sequence
/// ingress reserved for it, and the CPU and monotonic time that stamp it.
#[derive(Copy, Clone)]
struct Placement {
    stream: Stream,
    seq: u64,
    cpu: u32,
    monotonic: Duration64,
}

/// The journal persistence engine.
///
/// Owns the append-sequence authority ([`Ingress`]) and per-stream segment
/// state, and drives the segment lifecycle over a caller-supplied
/// [`SegmentStore`]. Construct one with [`Journal::new`], giving it one working
/// buffer per stream; then [`admit`](Self::admit) + [`commit`](Self::commit)
/// records, [`import_boot`](Self::import_boot) early-boot rings, and
/// [`flush`](Self::flush) to close open segments on shutdown or before an
/// anchor.
pub struct Journal<'a, S: SegmentStore> {
    ingress: Ingress,
    store: S,
    seal_key: Option<LogAttestationKey>,
    machine_id_hash: Sha256Digest,
    boot_id: BootId,
    /// The journal's own attested origin, for the self-events it authors.
    own_origin: Origin,
    /// The system-derived source name for the journal's own records.
    own_source: SourceName,
    /// Per-stream ingress rate limiter (SYSLOG §11). Defaults to
    /// [`RateLimiter::unlimited`]; the service installs a policy with
    /// [`Self::with_rate_limit`].
    limiter: RateLimiter,
    streams: [StreamState<'a>; STREAM_COUNT],
}

impl<'a, S: SegmentStore> Journal<'a, S> {
    /// Build a journal.
    ///
    /// * `store` — the sink closed segments are handed to.
    /// * `machine_id_hash` / `boot_id` — bind every segment header and the
    ///   stream genesis to this installation and boot.
    /// * `seal_key` — the log-attestation key; required to close `audit`/
    ///   `security` segments (their close fails closed without it).
    /// * `journal_origin` — the journal's own attested identity, used for the
    ///   self-events (loss records) it authors.
    /// * `buffers` — one working buffer per stream, indexed by
    ///   [`Stream::as_u8`]. A stream's segment is built in its buffer, so the
    ///   buffer bounds the segment size (a full segment rotates).
    #[must_use]
    pub fn new(
        store: S,
        machine_id_hash: Sha256Digest,
        boot_id: BootId,
        seal_key: Option<LogAttestationKey>,
        journal_origin: Origin,
        buffers: [&'a mut [u8]; STREAM_COUNT],
    ) -> Self {
        let mut bufs = buffers.map(Some);
        let streams = core::array::from_fn(|i| {
            // `Stream::ALL[i].as_u8() as usize == i`, so the state slot and its
            // stream agree by construction.
            let stream = Stream::ALL[i];
            StreamState {
                slot: bufs[i].take().map(Slot::Free),
                dict: DictionaryBuilder::new(),
                prev_hash: stream_genesis(&machine_id_hash, stream.genesis_label(), &boot_id),
                next_segment_id: 0,
            }
        });
        let own_source = derive_source(&journal_origin, Some("journal"));
        Self {
            ingress: Ingress::new(),
            store,
            seal_key,
            machine_id_hash,
            boot_id,
            own_origin: journal_origin,
            own_source,
            limiter: RateLimiter::unlimited(),
            streams,
        }
    }

    /// Install a rate-limiting policy for the caller-writable streams
    /// (SYSLOG §11).
    ///
    /// A journal starts with [`RateLimiter::unlimited`] (no dropping); the
    /// service configures a real policy so a log flood on the `runtime`/`debug`
    /// streams is bounded. The system-authority streams are never gated. Use
    /// as a builder: `Journal::new(..).with_rate_limit(limiter)`.
    #[must_use]
    pub fn with_rate_limit(mut self, limiter: RateLimiter) -> Self {
        self.limiter = limiter;
        self
    }

    /// Number each stream's segments on from `next_ids`, indexed by
    /// [`Stream::as_u8`]: one past the highest segment a stream already holds
    /// on disk, so a restart neither reuses nor overwrites one. Use as a
    /// builder: `Journal::new(..).continuing_segments(ids)`.
    #[must_use]
    pub fn continuing_segments(mut self, next_ids: [u64; STREAM_COUNT]) -> Self {
        for (state, next) in self.streams.iter_mut().zip(next_ids) {
            state.next_segment_id = next;
        }
        self
    }

    /// The append-sequence authority, for anchoring and resume.
    #[must_use]
    pub fn ingress(&self) -> &Ingress {
        &self.ingress
    }

    /// Run the admission decision for one record.
    ///
    /// A thin pass-through to the owned [`Ingress`]: the caller (the userland
    /// journal service) holds the attested `origin` from the kernel and the
    /// caller's advisory requests, and gets back an [`Admission`] to inspect
    /// (its spoof flags) and to pass straight to [`Self::commit`]. Reserving
    /// the append sequence happens here, so admit and commit are 1:1.
    pub fn admit(
        &mut self,
        origin: &Origin,
        subsystem: Option<&str>,
        requested_stream: Option<Stream>,
        requested_source: Option<&str>,
        caller_level: Option<Level>,
    ) -> Admission {
        self.ingress.admit(
            origin,
            subsystem,
            requested_stream,
            requested_source,
            caller_level,
        )
    }

    /// Admit one record subject to the rate limit (SYSLOG §11).
    ///
    /// This is the caller-facing admission path: it resolves the record's
    /// *effective* stream (the same authority decision [`Self::admit`] makes)
    /// and offers it to the [`RateLimiter`] **before** reserving an append
    /// sequence. If the record is within the rate — always, for the
    /// system-authority streams and under an [`RateLimiter::unlimited`]
    /// limiter — it is admitted exactly as [`Self::admit`] would and returns
    /// `Some(Admission)`. If a caller-writable stream (`runtime`/`debug`) is
    /// over its rate the record is dropped and returns `None`: no append
    /// sequence is consumed (so the stream's sequence stays gap-free), the drop
    /// is folded into the stream's tally, and no spoof note is authored — a
    /// spoof flood is thereby bounded at the runtime rate rather than being
    /// amplified into a flood of `security` records. Drain the accumulated
    /// drops with [`Self::emit_rate_loss`].
    ///
    /// `now` is the monotonic reading used to drive the token bucket.
    pub fn admit_limited(
        &mut self,
        origin: &Origin,
        subsystem: Option<&str>,
        requested_stream: Option<Stream>,
        requested_source: Option<&str>,
        caller_level: Option<Level>,
        now: Duration64,
    ) -> Option<Admission> {
        // Gate on the *effective* stream (the resolver's decision), so an
        // untrusted caller's downgraded-to-`runtime` record is limited by the
        // runtime bucket. Reserving the append sequence must happen only for an
        // admitted record, so the rate check precedes `self.ingress.admit`
        // (which reserves); the extra `resolve_stream` here is a pure,
        // side-effect-free repeat of the resolution `admit` performs.
        let effective = resolve_stream(origin, requested_stream).effective;
        match self.limiter.admit(effective, now) {
            RateDecision::Drop => None,
            RateDecision::Admit => Some(self.ingress.admit(
                origin,
                subsystem,
                requested_stream,
                requested_source,
                caller_level,
            )),
        }
    }

    /// Author trusted `journal`-stream loss records for any rate-limit drops
    /// that have matured past the reporting interval (SYSLOG §11).
    ///
    /// A drop is never silent: [`Self::admit_limited`] folds each dropped
    /// record into its stream's tally, and this drains every tally whose
    /// reporting window has elapsed into one coalesced `journal.rate.loss`
    /// record naming the stream, the number of records dropped, and the window
    /// — so a sustained flood produces at most one loss record per interval per
    /// stream. Call it on the ingress cadence (and before shutdown) with the
    /// current `now`/`wall`; it is a no-op when nothing is due.
    ///
    /// `cpu` is the caller's ingest-lane CPU; `next_cpu_seq` yields the per-CPU
    /// sequence for each authored record (the caller owns that counter, so a
    /// loss record leaves a detectable per-CPU gap like any other).
    ///
    /// # Errors
    ///
    /// [`JournalError::Encode`] if a loss record body is invalid or oversized,
    /// [`JournalError::Segment`] if a `journal` segment cannot be opened, or
    /// [`JournalError::Store`] if persisting a rotated segment fails.
    pub fn emit_rate_loss<F: FnMut() -> u64>(
        &mut self,
        cpu: u32,
        mut next_cpu_seq: F,
        now: Duration64,
        wall: WallClockReading,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        // `take_due_report` returns `None` for the four non-rate-limitable
        // streams, so iterating all streams authors at most one record per
        // gated stream that has a matured tally.
        for stream in Stream::ALL {
            if let Some(report) = self.limiter.take_due_report(stream, now) {
                self.emit_rate_loss_record(&report, cpu, next_cpu_seq(), now, wall, scratch)?;
            }
        }
        Ok(())
    }

    /// Commit one admitted record to durable storage.
    ///
    /// The record is encoded and appended to its stream's open segment,
    /// rotating (closing, persisting, and reopening) when the segment fills.
    /// `cpu`/`cpu_seq`/`monotonic`/`wall` are the container-owned, attested
    /// facts the kernel ingress path supplies; `caller`/`data` are the caller's
    /// own content.
    ///
    /// # Errors
    ///
    /// [`JournalError::Encode`] if the record body is invalid or oversized,
    /// [`JournalError::Segment`] if it cannot fit a fresh segment, or
    /// [`JournalError::Store`] if persisting a rotated segment fails. On any
    /// error nothing of this record is committed.
    pub fn commit(
        &mut self,
        admission: &Admission,
        stamp: Stamp,
        caller: CallerContent<'_>,
        data: &[(FieldName<'_>, FieldValue<'_>)],
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let Stamp {
            cpu,
            cpu_seq,
            monotonic,
            wall,
        } = stamp;
        let at = Placement {
            stream: admission.stream(),
            seq: admission.seq(),
            cpu,
            monotonic,
        };
        let record = LogRecord {
            effective_level: admission.effective_level(),
            cpu_seq,
            wall,
            origin: admission.origin(),
            source_name: admission.source().as_str(),
            caller,
            data,
        };
        self.place(at, &record, scratch)
    }

    /// Close every open segment, persisting each to the store.
    ///
    /// Called on shutdown and before anchoring. Each stream keeps its running
    /// chain hash, so the next record reopens a segment that chains onto the
    /// one just closed. Every stream is offered to the store even when one
    /// fails, and a segment the store refuses is kept for the next attempt.
    ///
    /// # Errors
    ///
    /// The first stream's [`JournalError::Store`] when a closed segment cannot
    /// be persisted.
    pub fn flush(&mut self) -> Result<(), JournalError<S::Error>> {
        let mut first = Ok(());
        for stream in Stream::ALL {
            let closed = self.close(stream);
            if first.is_ok() {
                first = closed;
            }
        }
        first
    }

    /// Import a CPU's early-boot ring into the `boot` stream.
    ///
    /// The retained records are drained oldest-first and appended verbatim (an
    /// early-boot body is a self-contained, dictionary-free record body, so it
    /// is stored opaque and needs no re-encoding). If the ring evicted records
    /// before this import could reach them, a single trusted loss record is
    /// authored on the `journal` stream naming the lost CPU-sequence range, so
    /// a boot-log reader sees an explicit gap rather than a silent one.
    ///
    /// `monotonic`/`wall` timestamp the import (used for the boot segment's
    /// creation time and any loss record); `scratch` must be at least
    /// [`crate::MAX_BOOT_RECORD_BODY`] bytes to receive each drained body.
    ///
    /// # Errors
    ///
    /// [`JournalError::Encode`] if a drained body does not fit `scratch`,
    /// [`JournalError::Segment`] if a body cannot fit a fresh boot segment, or
    /// [`JournalError::Store`] if persisting a rotated segment fails.
    pub fn import_boot<const N: usize>(
        &mut self,
        ring: &mut BootRing<N>,
        monotonic: Duration64,
        wall: WallClockReading,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let cpu = ring.cpu_id();
        while let Some(drained) = ring.pop_oldest(scratch).map_err(JournalError::Encode)? {
            let seq = self.ingress.reserve(Stream::Boot);
            // `pop_oldest` wrote the body to `scratch[..body_len]`; append it
            // opaque at the producer's monotonic time. The body already carries
            // its own `cpu_seq`; the ring's `drained.cpu_seq` is consumed only
            // by the eviction loss accounting below.
            let at = Placement {
                stream: Stream::Boot,
                seq,
                cpu,
                monotonic: drained.monotonic,
            };
            let body = scratch.get(..drained.body_len);
            let appended = match body {
                Some(body) => self.append_opaque_at(at, wall, body),
                None => Err(JournalError::Encode(Errno::BufferTooSmall)),
            };
            if appended.is_err() {
                self.ingress.release(Stream::Boot, seq);
            }
            appended?;
        }
        if let Some(loss) = ring.take_loss() {
            self.emit_boot_loss(&loss, monotonic, wall, scratch)?;
        }
        Ok(())
    }

    /// Author a trusted `security`-stream record noting a caller's rejected
    /// spoof attempt.
    ///
    /// The journal calls this when an [`Admission`] came back
    /// [`spoofed`](Admission::spoofed) — the caller asked for a privileged
    /// stream it was not trusted for, or a source name that impersonates a
    /// reserved namespace. The authoritative record was already committed
    /// under the caller's *derived* source and *downgraded* stream (preserving
    /// the request as a caller claim); this separate trusted record, authored
    /// under the journal's own origin on the `security` stream, records the
    /// attempt itself so it is auditable independently of the record it
    /// concerned. The offending principal's uid and the exact claims are
    /// preserved as fields.
    ///
    /// `cpu`/`cpu_seq` are the journal's own ingest-context facts;
    /// `monotonic`/`wall` timestamp the note; `scratch` receives the encoded
    /// body.
    ///
    /// # Errors
    ///
    /// [`JournalError::Encode`] if the record body is invalid or oversized,
    /// [`JournalError::Segment`] if a `security` segment cannot be opened or
    /// sealed (e.g. no seal key — it fails closed, never silently drops), or
    /// [`JournalError::Store`] if persisting a rotated segment fails.
    pub fn note_spoof(
        &mut self,
        admission: &Admission,
        requested_stream: Option<Stream>,
        requested_source: Option<&str>,
        stamp: Stamp,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let Stamp {
            cpu,
            cpu_seq,
            monotonic,
            wall,
        } = stamp;
        let seq = self.ingress.reserve(Stream::Security);
        let data = [
            (
                FieldName::new("uid").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(u64::from(admission.origin().uid())),
            ),
            (
                FieldName::new("stream_spoofed").map_err(JournalError::Encode)?,
                FieldValue::Bool(admission.stream_spoofed()),
            ),
            (
                FieldName::new("source_spoofed").map_err(JournalError::Encode)?,
                FieldValue::Bool(admission.source_spoofed()),
            ),
            (
                FieldName::new("effective_stream").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(u64::from(admission.stream().as_u8())),
            ),
        ];
        let caller = CallerContent {
            level: None,
            component: None,
            tag: None,
            event_id: Some("journal.spoof.detected"),
            // Preserve the caller's exact claims as evidence under this
            // trusted record.
            requested_source,
            requested_stream,
            message: "caller spoof attempt rejected",
        };
        let source = self.own_source;
        let at = Placement {
            stream: Stream::Security,
            seq,
            cpu,
            monotonic,
        };
        let record = LogRecord {
            effective_level: Level::Warn,
            cpu_seq,
            wall,
            origin: self.own_origin,
            source_name: source.as_str(),
            caller,
            data: &data,
        };
        self.place(at, &record, scratch)
    }

    /// Encode and append `record` at `at`, rotating on a full segment. A
    /// record refused here hands its sequence back, so the stream's sequence
    /// stays gap-free.
    fn place(
        &mut self,
        at: Placement,
        record: &LogRecord<'_>,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let placed = self.place_at(at, record, scratch);
        if placed.is_err() {
            self.ingress.release(at.stream, at.seq);
        }
        placed
    }

    fn place_at(
        &mut self,
        at: Placement,
        record: &LogRecord<'_>,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let Placement {
            stream,
            seq,
            cpu,
            monotonic,
        } = at;
        let wall = record.wall;
        // Never let a single record's payload exceed the segment record cap,
        // so `append_record` never rejects a record that fits `scratch` for
        // being oversized; a genuinely too-large record fails on encode.
        let cap = scratch.len().min(MAX_RECORD_PAYLOAD);
        let i = stream.as_u8() as usize;
        loop {
            self.ensure_open(stream, seq, monotonic, wall)?;
            let st = &mut self.streams[i];
            let Some(Slot::Open(writer)) = st.slot.as_mut() else {
                return Err(JournalError::Segment(SegmentError::MissingFooter));
            };
            match record.encode(&mut scratch[..cap], &mut st.dict) {
                Ok(n) => match writer.append_record(cpu, monotonic, &scratch[..n]) {
                    Ok(_) => return Ok(()),
                    Err(SegmentError::BufferTooSmall) => {
                        if writer.record_count() == 0 {
                            // A fresh, empty segment still cannot hold it: the
                            // record is too large for any segment. Reject it
                            // rather than rotate forever.
                            return Err(JournalError::Segment(SegmentError::BufferTooSmall));
                        }
                        // Segment full: close it and reopen a fresh one, then
                        // re-encode against the new segment's dictionary.
                        self.rotate(stream, seq, monotonic, wall)?;
                    }
                    Err(e) => return Err(JournalError::Segment(e)),
                },
                Err(e) => {
                    // The encode may have partially mutated the dictionary; drop
                    // it so the segment's dictionary stays consistent with the
                    // records it actually contains, then reject the record.
                    self.discard_dirty_dict(stream, seq, monotonic, wall)?;
                    return Err(JournalError::Encode(e));
                }
            }
        }
    }

    /// Append an already-encoded, dictionary-free body verbatim, rotating on a
    /// full segment. Used by boot import, where a body carries no dictionary
    /// back-references (so it may be re-appended to a fresh segment unchanged)
    /// and already carries its own `cpu_seq` in the encoded record body — the
    /// ring's separately-tracked per-record `cpu_seq` drives only the eviction
    /// loss accounting, not this append.
    fn append_opaque_at(
        &mut self,
        at: Placement,
        wall: WallClockReading,
        body: &[u8],
    ) -> Result<(), JournalError<S::Error>> {
        let Placement {
            stream,
            seq,
            cpu,
            monotonic,
        } = at;
        let i = stream.as_u8() as usize;
        loop {
            self.ensure_open(stream, seq, monotonic, wall)?;
            let Some(Slot::Open(writer)) = self.streams[i].slot.as_mut() else {
                return Err(JournalError::Segment(SegmentError::MissingFooter));
            };
            match writer.append_record(cpu, monotonic, body) {
                Ok(_) => return Ok(()),
                Err(SegmentError::BufferTooSmall) => {
                    if writer.record_count() == 0 {
                        return Err(JournalError::Segment(SegmentError::BufferTooSmall));
                    }
                    self.rotate(stream, seq, monotonic, wall)?;
                }
                Err(e) => return Err(JournalError::Segment(e)),
            }
        }
    }

    /// Author a trusted boot-loss record on the `journal` stream.
    fn emit_boot_loss(
        &mut self,
        loss: &LossRange,
        monotonic: Duration64,
        wall: WallClockReading,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let seq = self.ingress.reserve(Stream::Journal);
        let data = [
            (
                FieldName::new("cpu").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(u64::from(loss.cpu_id)),
            ),
            (
                FieldName::new("first_seq").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(loss.first_seq),
            ),
            (
                FieldName::new("last_seq").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(loss.last_seq),
            ),
            (
                FieldName::new("count").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(loss.count),
            ),
        ];
        let caller = CallerContent {
            level: None,
            component: None,
            tag: None,
            event_id: Some("journal.boot.loss"),
            requested_source: None,
            requested_stream: Some(Stream::Journal),
            message: "early-boot records lost before import",
        };
        // `own_source` is `Copy`; copied out so the record does not borrow
        // `self` across the `&mut self` call.
        let source = self.own_source;
        let at = Placement {
            stream: Stream::Journal,
            seq,
            cpu: loss.cpu_id,
            monotonic,
        };
        let record = LogRecord {
            effective_level: Level::Warn,
            cpu_seq: loss.last_seq,
            wall,
            origin: self.own_origin,
            source_name: source.as_str(),
            caller,
            data: &data,
        };
        self.place(at, &record, scratch)
    }

    /// Author one trusted `journal`-stream loss record for a coalesced
    /// rate-limit [`DropReport`].
    fn emit_rate_loss_record(
        &mut self,
        report: &DropReport,
        cpu: u32,
        cpu_seq: u64,
        monotonic: Duration64,
        wall: WallClockReading,
        scratch: &mut [u8],
    ) -> Result<(), JournalError<S::Error>> {
        let seq = self.ingress.reserve(Stream::Journal);
        let data = [
            (
                FieldName::new("stream").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(u64::from(report.stream.as_u8())),
            ),
            (
                FieldName::new("dropped").map_err(JournalError::Encode)?,
                FieldValue::UnsignedInt(report.count),
            ),
            (
                FieldName::new("window").map_err(JournalError::Encode)?,
                FieldValue::Duration(report.window),
            ),
        ];
        let caller = CallerContent {
            level: None,
            component: None,
            tag: None,
            event_id: Some("journal.rate.loss"),
            requested_source: None,
            requested_stream: Some(Stream::Journal),
            message: "records dropped by rate limit",
        };
        let source = self.own_source;
        let at = Placement {
            stream: Stream::Journal,
            seq,
            cpu,
            monotonic,
        };
        let record = LogRecord {
            effective_level: Level::Warn,
            cpu_seq,
            wall,
            origin: self.own_origin,
            source_name: source.as_str(),
            caller,
            data: &data,
        };
        self.place(at, &record, scratch)
    }

    /// Open a fresh segment for `stream` if none is open, seeding its first
    /// append sequence with `seq` so the segment chain and the [`Ingress`]
    /// counter stay in lockstep. A closed segment the store refused is offered
    /// to it again first; until it is taken, nothing more is accepted.
    fn ensure_open(
        &mut self,
        stream: Stream,
        seq: u64,
        monotonic: Duration64,
        wall: WallClockReading,
    ) -> Result<(), JournalError<S::Error>> {
        // An audit or security record that could never be sealed is refused
        // now, rather than accepted into a segment no key can close.
        if stream.requires_seal() && self.seal_key.is_none() {
            return Err(JournalError::Segment(SegmentError::SealKeyRequired));
        }
        let i = stream.as_u8() as usize;
        let Self {
            store,
            streams,
            machine_id_hash,
            boot_id,
            ..
        } = self;
        let st = &mut streams[i];
        let buf = match st.slot.take() {
            Some(Slot::Free(buf)) => buf,
            Some(Slot::Unstored(closed)) => match store.store_segment(&closed.buf[..closed.len]) {
                Ok(()) => closed.buf,
                Err(e) => {
                    st.slot = Some(Slot::Unstored(closed));
                    return Err(JournalError::Store(e));
                }
            },
            open @ Some(Slot::Open(_)) => {
                st.slot = open;
                return Ok(());
            }
            None => return Err(JournalError::Segment(SegmentError::BufferTooSmall)),
        };
        if buf.len() < SEGMENT_HEADER_LEN + SEGMENT_FOOTER_LEN {
            st.slot = Some(Slot::Free(buf));
            return Err(JournalError::Segment(SegmentError::BufferTooSmall));
        }
        let header = SegmentHeader {
            stream,
            segment_id: st.next_segment_id,
            machine_id_hash: *machine_id_hash,
            boot_id: *boot_id,
            first_seq: seq,
            prev_segment_hash: st.prev_hash,
            creation_monotonic: monotonic,
            creation_wall: wall,
        };
        // The buffer holds a header and a footer, which is all `begin` asks.
        let writer = SegmentWriter::begin(buf, &header).map_err(JournalError::Segment)?;
        st.slot = Some(Slot::Open(writer));
        st.dict = DictionaryBuilder::new();
        st.next_segment_id += 1;
        Ok(())
    }

    /// Close the open segment for `stream` (if any), advance the stream's
    /// running chain hash, and persist the segment. A segment the store
    /// refuses is kept, and offered again before the stream accepts more.
    fn close(&mut self, stream: Stream) -> Result<(), JournalError<S::Error>> {
        let i = stream.as_u8() as usize;
        let Self {
            store,
            streams,
            seal_key,
            ..
        } = self;
        let st = &mut streams[i];
        let closed = match st.slot.take() {
            // A sealed stream only opens with its key, so the close can seal.
            Some(Slot::Open(writer)) => {
                let finished = writer
                    .finish(seal_key.as_ref())
                    .map_err(JournalError::Segment)?;
                st.prev_hash = finished.segment_hash;
                st.dict = DictionaryBuilder::new();
                finished
            }
            Some(Slot::Unstored(closed)) => closed,
            free => {
                st.slot = free;
                return Ok(());
            }
        };
        match store.store_segment(&closed.buf[..closed.len]) {
            Ok(()) => {
                st.slot = Some(Slot::Free(closed.buf));
                Ok(())
            }
            Err(e) => {
                st.slot = Some(Slot::Unstored(closed));
                Err(JournalError::Store(e))
            }
        }
    }

    /// Close and immediately reopen `stream`'s segment, seeding the new one's
    /// first sequence with `seq`.
    fn rotate(
        &mut self,
        stream: Stream,
        seq: u64,
        monotonic: Duration64,
        wall: WallClockReading,
    ) -> Result<(), JournalError<S::Error>> {
        self.close(stream)?;
        self.ensure_open(stream, seq, monotonic, wall)
    }

    /// Discard a segment's dictionary after a failed encode may have left it
    /// inconsistent with the records the segment actually holds: reset it in
    /// place for an empty segment, or rotate to a fresh segment otherwise.
    fn discard_dirty_dict(
        &mut self,
        stream: Stream,
        seq: u64,
        monotonic: Duration64,
        wall: WallClockReading,
    ) -> Result<(), JournalError<S::Error>> {
        let i = stream.as_u8() as usize;
        let empty = match &self.streams[i].slot {
            Some(Slot::Open(writer)) => writer.record_count() == 0,
            _ => true,
        };
        if empty {
            self.streams[i].dict = DictionaryBuilder::new();
            Ok(())
        } else {
            self.rotate(stream, seq, monotonic, wall)
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use crate::attest::machine_id_hash;
    use crate::record::{decode as decode_record, CALLER_MESSAGE_MAX};
    use crate::segment::{verify_segment, SegmentReader};
    use crate::DictionaryView;
    use alloc::rc::Rc;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use tairix_abi::{
        CapabilitySummary, ProcId, Time64, TrustDomain, WallTimeState, BOOT_ID_LEN,
        ORIGIN_CONSOLE_NONE, PROC_ID_LEN,
    };

    const MID: [u8; 16] = [0x11; 16];

    fn boot() -> BootId {
        BootId::from_raw([0x5A; BOOT_ID_LEN])
    }

    /// A `SegmentStore` that captures closed segment images in a shared vector,
    /// so a test can inspect what the journal persisted after it moves the
    /// store into the [`Journal`].
    #[derive(Clone)]
    struct TestStore {
        segments: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl TestStore {
        fn new() -> Self {
            Self {
                segments: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl SegmentStore for TestStore {
        type Error = ();
        fn store_segment(&mut self, bytes: &[u8]) -> Result<(), ()> {
            self.segments.borrow_mut().push(bytes.to_vec());
            Ok(())
        }
    }

    fn kernel_origin() -> Origin {
        Origin::new(
            TrustDomain::Kernel,
            0,
            0,
            1,
            ProcId::KERNEL,
            CapabilitySummary::EMPTY,
            ORIGIN_CONSOLE_NONE,
        )
    }

    fn user_origin(uid: u32) -> Origin {
        Origin::new(
            TrustDomain::User,
            uid,
            uid,
            42,
            ProcId::from_raw([0x5A; PROC_ID_LEN]),
            CapabilitySummary::EMPTY,
            ORIGIN_CONSOLE_NONE,
        )
    }

    fn wall() -> WallClockReading {
        WallClockReading::new(Time64::from_secs(1_700_000_000), WallTimeState::Trusted)
    }

    fn simple_caller(message: &str) -> CallerContent<'_> {
        CallerContent {
            level: None,
            component: None,
            tag: None,
            event_id: None,
            requested_source: None,
            requested_stream: None,
            message,
        }
    }

    /// Build a journal over six equally-sized buffers and run `body` with it,
    /// returning the store's captured segments. The buffers must outlive the
    /// journal, so they are owned here and the journal borrows them.
    fn with_journal<F>(seal: Option<LogAttestationKey>, buf_len: usize, body: F) -> Vec<Vec<u8>>
    where
        F: FnOnce(&mut Journal<'_, TestStore>),
    {
        let store = TestStore::new();
        let sink = store.segments.clone();
        with_store_journal(store, seal, buf_len, body);
        let out = sink.borrow().clone();
        out
    }

    /// Build a journal over `store` and six equally-sized buffers, and run
    /// `body` with it.
    fn with_store_journal<S: SegmentStore, F>(
        store: S,
        seal: Option<LogAttestationKey>,
        buf_len: usize,
        body: F,
    ) where
        F: FnOnce(&mut Journal<'_, S>),
    {
        let mut b: [Vec<u8>; STREAM_COUNT] = core::array::from_fn(|_| alloc::vec![0u8; buf_len]);
        let [b0, b1, b2, b3, b4, b5] = &mut b;
        let bufs: [&mut [u8]; STREAM_COUNT] = [
            b0.as_mut_slice(),
            b1.as_mut_slice(),
            b2.as_mut_slice(),
            b3.as_mut_slice(),
            b4.as_mut_slice(),
            b5.as_mut_slice(),
        ];
        let mut journal = Journal::new(
            store,
            machine_id_hash(&MID),
            boot(),
            seal,
            kernel_origin(),
            bufs,
        );
        body(&mut journal);
    }

    /// A store that refuses its next `refusals` segments as a full volume
    /// would, then takes them.
    struct FlakyStore {
        refusals: Rc<core::cell::Cell<u32>>,
        segments: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl SegmentStore for FlakyStore {
        type Error = Errno;
        fn store_segment(&mut self, bytes: &[u8]) -> Result<(), Errno> {
            if self.refusals.get() > 0 {
                self.refusals.set(self.refusals.get() - 1);
                return Err(Errno::NoSpace);
            }
            self.segments.borrow_mut().push(bytes.to_vec());
            Ok(())
        }
    }

    /// Verify `segments` as one stream's run: each verifies, chains onto its
    /// predecessor, and continues its append sequence. Returns the records
    /// they hold.
    fn verify_run(segments: &[Vec<u8>]) -> u64 {
        let mut prev: Option<crate::SegmentSummary> = None;
        let mut total = 0u64;
        for seg in segments {
            let s = verify_segment(seg, None).expect("verify");
            total += s.record_count;
            if let Some(p) = prev {
                assert_eq!(
                    s.header.prev_segment_hash, p.segment_hash,
                    "chain continues"
                );
                assert_eq!(s.first_seq, p.next_seq, "append sequence is continuous");
            }
            prev = Some(s);
        }
        total
    }

    /// Decode every record of one stored segment in order, returning each
    /// decoded record's `(effective_level, source, message, requested_stream)`.
    fn decoded_messages(segment: &[u8]) -> Vec<(Level, String, String, Option<Stream>)> {
        let reader = SegmentReader::open(segment).expect("open");
        let mut view = DictionaryView::new();
        let mut out = Vec::new();
        for block in reader {
            let rec = decode_record(block.payload, &mut view).expect("decode");
            out.push((
                rec.effective_level(),
                String::from(rec.source_name()),
                String::from(rec.caller().message),
                rec.caller().requested_stream,
            ));
        }
        out
    }

    #[test]
    fn commits_verify_and_round_trip() {
        let scratch = &mut [0u8; 512];
        let segments = with_journal(None, 8192, |j| {
            for msg in ["one", "two", "three"] {
                let adm = j.admit(
                    &kernel_origin(),
                    Some("mem"),
                    Some(Stream::Runtime),
                    None,
                    None,
                );
                j.commit(
                    &adm,
                    Stamp {
                        cpu: 0,
                        cpu_seq: 0,
                        monotonic: Duration64::from_secs(1),
                        wall: wall(),
                    },
                    simple_caller(msg),
                    &[],
                    scratch,
                )
                .expect("commit");
            }
            j.flush().expect("flush");
        });
        assert_eq!(segments.len(), 1, "all three fit one segment");
        let summary = verify_segment(&segments[0], None).expect("verifies");
        assert_eq!(summary.record_count, 3);
        assert_eq!(summary.first_seq, 0);
        assert_eq!(summary.next_seq, 3);
        let decoded = decoded_messages(&segments[0]);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].1, "kernel.mem");
        assert_eq!(decoded[0].2, "one");
        assert_eq!(decoded[2].2, "three");
    }

    #[test]
    fn segments_rotate_and_chain_when_full() {
        let scratch = &mut [0u8; 512];
        // A buffer that holds only a couple of records forces rotations while
        // still fitting a single record in a fresh segment.
        let small = SEGMENT_HEADER_LEN + SEGMENT_FOOTER_LEN + 300;
        let segments = with_journal(None, small, |j| {
            for i in 0..8u32 {
                let adm = j.admit(
                    &kernel_origin(),
                    Some("net"),
                    Some(Stream::Runtime),
                    None,
                    None,
                );
                j.commit(
                    &adm,
                    Stamp {
                        cpu: i,
                        cpu_seq: u64::from(i),
                        monotonic: Duration64::from_secs(i64::from(i)),
                        wall: wall(),
                    },
                    simple_caller("link"),
                    &[],
                    scratch,
                )
                .expect("commit");
            }
            j.flush().expect("flush");
        });
        assert!(segments.len() >= 2, "records span several segments");
        assert_eq!(
            verify_run(&segments),
            8,
            "no record is lost across rotations"
        );
    }

    /// A full volume refuses a segment twice: each record that needed it is
    /// answered `NoSpace`, the segment is kept and offered again before the
    /// stream takes another record, and once the volume recovers nothing the
    /// journal accepted is lost, unchained, or out of sequence.
    #[test]
    fn a_refused_segment_is_kept_and_stored_once_the_store_recovers() {
        let refusals = Rc::new(core::cell::Cell::new(0));
        let sink = Rc::new(RefCell::new(Vec::new()));
        let store = FlakyStore {
            refusals: Rc::clone(&refusals),
            segments: Rc::clone(&sink),
        };
        let small = SEGMENT_HEADER_LEN + SEGMENT_FOOTER_LEN + 300;
        let (mut accepted, mut refused) = (0u64, 0u32);
        with_store_journal(store, None, small, |j| {
            for i in 0..12u32 {
                if i == 4 {
                    refusals.set(2);
                }
                let adm = j.admit(
                    &kernel_origin(),
                    Some("net"),
                    Some(Stream::Runtime),
                    None,
                    None,
                );
                match j.commit(
                    &adm,
                    Stamp {
                        cpu: i,
                        cpu_seq: u64::from(i),
                        monotonic: Duration64::from_secs(i64::from(i)),
                        wall: wall(),
                    },
                    simple_caller("link"),
                    &[],
                    &mut [0u8; 512],
                ) {
                    Ok(()) => accepted += 1,
                    Err(JournalError::Store(Errno::NoSpace)) => refused += 1,
                    Err(other) => panic!("unexpected {other:?}"),
                }
            }
            assert_eq!(j.ingress().next_seq(Stream::Runtime), accepted, "no gap");
            j.flush().expect("flush");
        });
        assert_eq!(refused, 2, "one record per refusal, none after recovery");
        let segments = sink.borrow();
        assert_eq!(
            verify_run(&segments),
            accepted,
            "every accepted record is kept"
        );
    }

    /// A restarted journal numbers each stream's segments on from the ids it
    /// is given, so it neither reuses nor overwrites one an earlier run wrote.
    #[test]
    fn segments_continue_from_the_given_ids() {
        let store = TestStore::new();
        let sink = store.segments.clone();
        let mut b: [Vec<u8>; STREAM_COUNT] = core::array::from_fn(|_| alloc::vec![0u8; 8192]);
        let [b0, b1, b2, b3, b4, b5] = &mut b;
        let bufs: [&mut [u8]; STREAM_COUNT] = [
            b0.as_mut_slice(),
            b1.as_mut_slice(),
            b2.as_mut_slice(),
            b3.as_mut_slice(),
            b4.as_mut_slice(),
            b5.as_mut_slice(),
        ];
        let mut next = [0u64; STREAM_COUNT];
        next[Stream::Runtime.as_u8() as usize] = 7;
        let mut j = Journal::new(
            store,
            machine_id_hash(&MID),
            boot(),
            None,
            kernel_origin(),
            bufs,
        )
        .continuing_segments(next);
        for _ in 0..2 {
            let adm = j.admit(
                &kernel_origin(),
                Some("net"),
                Some(Stream::Runtime),
                None,
                None,
            );
            j.commit(
                &adm,
                Stamp {
                    cpu: 0,
                    cpu_seq: 0,
                    monotonic: Duration64::from_secs(1),
                    wall: wall(),
                },
                simple_caller("x"),
                &[],
                &mut [0u8; 512],
            )
            .expect("commit");
            j.flush().expect("flush");
        }
        let ids: Vec<u64> = sink
            .borrow()
            .iter()
            .map(|seg| verify_segment(seg, None).expect("verify").header.segment_id)
            .collect();
        assert_eq!(ids, [7, 8]);
    }

    #[test]
    fn audit_segment_is_sealed_and_verifies_with_the_key() {
        let key = LogAttestationKey::from_key([0x24; 32]);
        let scratch = &mut [0u8; 512];
        let segments = with_journal(Some(LogAttestationKey::from_key([0x24; 32])), 8192, |j| {
            let adm = j.admit(
                &kernel_origin(),
                Some("sec"),
                Some(Stream::Audit),
                None,
                Some(Level::Error),
            );
            j.commit(
                &adm,
                Stamp {
                    cpu: 0,
                    cpu_seq: 0,
                    monotonic: Duration64::from_secs(1),
                    wall: wall(),
                },
                simple_caller("login denied"),
                &[],
                scratch,
            )
            .expect("commit");
            j.flush().expect("flush");
        });
        assert_eq!(segments.len(), 1);
        let s = verify_segment(&segments[0], Some(&key)).expect("sealed verify");
        assert!(s.sealed);
        assert_eq!(s.header.stream, Stream::Audit);
        // Without the key, the sealed segment fails closed.
        assert_eq!(
            verify_segment(&segments[0], None),
            Err(SegmentError::SealKeyRequired)
        );
    }

    /// An audit record no key could ever seal is refused at commit rather than
    /// accepted into a segment that cannot close, and the stream keeps its
    /// buffer and its sequence.
    #[test]
    fn an_audit_record_without_a_key_is_refused_at_commit() {
        let segments = with_journal(None, 8192, |j| {
            for _ in 0..2 {
                let adm = j.admit(
                    &kernel_origin(),
                    Some("sec"),
                    Some(Stream::Audit),
                    None,
                    None,
                );
                assert_eq!(
                    j.commit(
                        &adm,
                        Stamp {
                            cpu: 0,
                            cpu_seq: 0,
                            monotonic: Duration64::from_secs(1),
                            wall: wall(),
                        },
                        simple_caller("x"),
                        &[],
                        &mut [0u8; 512],
                    ),
                    Err(JournalError::Segment(SegmentError::SealKeyRequired))
                );
                assert_eq!(j.ingress().next_seq(Stream::Audit), 0, "no gap left");
            }
            assert_eq!(
                j.flush(),
                Ok(()),
                "nothing was accepted, so nothing is owed"
            );
        });
        assert!(segments.is_empty());
    }

    #[test]
    fn oversize_record_is_rejected_and_nothing_is_written() {
        let scratch = &mut [0u8; 8192];
        let big = core::iter::repeat_n('a', CALLER_MESSAGE_MAX + 1).collect::<String>();
        let segments = with_journal(None, 8192, |j| {
            let adm = j.admit(
                &kernel_origin(),
                Some("mem"),
                Some(Stream::Runtime),
                None,
                None,
            );
            let err = j
                .commit(
                    &adm,
                    Stamp {
                        cpu: 0,
                        cpu_seq: 0,
                        monotonic: Duration64::from_secs(1),
                        wall: wall(),
                    },
                    simple_caller(&big),
                    &[],
                    scratch,
                )
                .expect_err("oversize message rejected");
            assert!(matches!(err, JournalError::Encode(_)));
            // A subsequent good record still commits (the dictionary was not
            // left corrupt by the rejected one).
            let adm2 = j.admit(
                &kernel_origin(),
                Some("mem"),
                Some(Stream::Runtime),
                None,
                None,
            );
            j.commit(
                &adm2,
                Stamp {
                    cpu: 0,
                    cpu_seq: 0,
                    monotonic: Duration64::from_secs(2),
                    wall: wall(),
                },
                simple_caller("ok"),
                &[],
                scratch,
            )
            .expect("good record commits");
            j.flush().expect("flush");
        });
        assert_eq!(segments.len(), 1);
        let s = verify_segment(&segments[0], None).expect("verify");
        assert_eq!(s.record_count, 1, "only the good record was written");
    }

    #[test]
    fn user_stream_spoof_is_downgraded_but_preserved_as_a_claim() {
        let scratch = &mut [0u8; 512];
        let segments = with_journal(None, 8192, |j| {
            let adm = j.admit(&user_origin(1000), None, Some(Stream::Audit), None, None);
            assert_eq!(adm.stream(), Stream::Runtime, "audit request downgraded");
            assert!(adm.stream_spoofed());
            let caller = CallerContent {
                requested_stream: Some(Stream::Audit),
                ..simple_caller("audit disabled")
            };
            j.commit(
                &adm,
                Stamp {
                    cpu: 0,
                    cpu_seq: 0,
                    monotonic: Duration64::from_secs(1),
                    wall: wall(),
                },
                caller,
                &[],
                scratch,
            )
            .expect("commit");
            j.flush().expect("flush");
        });
        // The record landed on runtime, and its decoded content preserves the
        // spoofed request as a caller claim under the derived user source.
        let s = verify_segment(&segments[0], None).expect("verify");
        assert_eq!(s.header.stream, Stream::Runtime);
        let decoded = decoded_messages(&segments[0]);
        assert!(decoded[0].1.starts_with("user.1000.proc."));
        assert_eq!(decoded[0].3, Some(Stream::Audit));
    }

    #[test]
    fn note_spoof_authors_a_sealed_security_record_preserving_the_claim() {
        let key = LogAttestationKey::from_key([0x71; 32]);
        let scratch = &mut [0u8; 512];
        let segments = with_journal(Some(LogAttestationKey::from_key([0x71; 32])), 8192, |j| {
            // A user requests a privileged stream *and* a reserved source: both
            // spoof attempts, downgraded and preserved on the runtime record.
            let adm = j.admit(
                &user_origin(1000),
                None,
                Some(Stream::Audit),
                Some("kernel.mem"),
                None,
            );
            assert!(adm.spoofed());
            j.commit(
                &adm,
                Stamp {
                    cpu: 0,
                    cpu_seq: 0,
                    monotonic: Duration64::from_secs(1),
                    wall: wall(),
                },
                simple_caller("audit disabled"),
                &[],
                scratch,
            )
            .expect("commit downgraded record");
            // The journal authors a trusted security note about the attempt.
            j.note_spoof(
                &adm,
                Some(Stream::Audit),
                Some("kernel.mem"),
                Stamp {
                    cpu: 0,
                    cpu_seq: 0,
                    monotonic: Duration64::from_secs(1),
                    wall: wall(),
                },
                scratch,
            )
            .expect("note_spoof");
            j.flush().expect("flush");
        });
        // Find the sealed security segment among the persisted segments.
        let mut security = None;
        for seg in &segments {
            let s = verify_segment(seg, Some(&key)).expect("verify");
            if s.header.stream == Stream::Security {
                assert!(s.sealed, "the security segment is sealed");
                security = Some(seg.clone());
            }
        }
        let security = security.expect("a security segment was authored");
        let decoded = decoded_messages(&security);
        assert_eq!(decoded.len(), 1);
        // Authored under the journal's own trusted source, not the caller's.
        assert_eq!(decoded[0].1, "kernel.journal");
        assert_eq!(decoded[0].2, "caller spoof attempt rejected");
        // The caller's stream claim is preserved on the trusted note.
        assert_eq!(decoded[0].3, Some(Stream::Audit));
    }

    #[test]
    fn boot_import_stores_records_and_emits_a_loss_record_on_eviction() {
        let scratch = &mut [0u8; 8192];
        // A ring small enough that pushing many records evicts the oldest.
        let mut ring: BootRing<256> = BootRing::new(3);
        // Pushing far more than the ring holds evicts the oldest, so import
        // must see a pending loss range and author exactly one loss record.
        for seq in 0..80u64 {
            ring.push(
                seq,
                Duration64::from_secs(i64::try_from(seq).unwrap()),
                b"early boot line",
            )
            .expect("push");
        }

        let store = TestStore::new();
        let sink = store.segments.clone();
        let mut b: [Vec<u8>; STREAM_COUNT] = core::array::from_fn(|_| alloc::vec![0u8; 8192]);
        let [b0, b1, b2, b3, b4, b5] = &mut b;
        let bufs: [&mut [u8]; STREAM_COUNT] = [
            b0.as_mut_slice(),
            b1.as_mut_slice(),
            b2.as_mut_slice(),
            b3.as_mut_slice(),
            b4.as_mut_slice(),
            b5.as_mut_slice(),
        ];
        let mut j = Journal::new(
            store,
            machine_id_hash(&MID),
            boot(),
            None,
            kernel_origin(),
            bufs,
        );
        j.import_boot(&mut ring, Duration64::from_secs(100), wall(), scratch)
            .expect("import");
        j.flush().expect("flush");

        let segments = sink.borrow();
        // At least one boot segment and one journal segment (the loss record).
        let mut boot_records = 0u64;
        let mut journal_records = 0u64;
        for seg in segments.iter() {
            let s = verify_segment(seg, None).expect("verify");
            match s.header.stream {
                Stream::Boot => boot_records += s.record_count,
                Stream::Journal => journal_records += s.record_count,
                other => panic!("unexpected stream {other:?}"),
            }
        }
        assert!(boot_records > 0, "retained boot records were imported");
        assert_eq!(journal_records, 1, "exactly one loss record was authored");
    }

    /// Build a rate-limited journal over six equal buffers and run `body`,
    /// returning the persisted segments. Mirrors [`with_journal`] but installs
    /// a policy.
    fn with_limited_journal<F>(limiter: RateLimiter, body: F) -> Vec<Vec<u8>>
    where
        F: FnOnce(&mut Journal<'_, TestStore>),
    {
        let store = TestStore::new();
        let sink = store.segments.clone();
        let mut b: [Vec<u8>; STREAM_COUNT] = core::array::from_fn(|_| alloc::vec![0u8; 8192]);
        let [b0, b1, b2, b3, b4, b5] = &mut b;
        let bufs: [&mut [u8]; STREAM_COUNT] = [
            b0.as_mut_slice(),
            b1.as_mut_slice(),
            b2.as_mut_slice(),
            b3.as_mut_slice(),
            b4.as_mut_slice(),
            b5.as_mut_slice(),
        ];
        let mut journal = Journal::new(
            store,
            machine_id_hash(&MID),
            boot(),
            None,
            kernel_origin(),
            bufs,
        )
        .with_rate_limit(limiter);
        body(&mut journal);
        let out = sink.borrow().clone();
        out
    }

    #[test]
    fn rate_limit_drops_excess_runtime_and_authors_one_coalesced_loss_record() {
        use crate::ratelimit::{RateLimit, RateLimiter};
        let scratch = &mut [0u8; 512];
        // Burst of 2 runtime records, then drops at the same instant; report
        // after 1s. `emit_rate_loss` uses a fixed cpu_seq (999) — the value is
        // opaque to this test, which asserts only the loss record's content.
        let limiter = RateLimiter::new(
            RateLimit::per_second(1000, 2),
            RateLimit::per_second(1000, 2),
            Duration64::from_secs(1),
        );
        let mut admitted = 0u32;
        let segments = with_limited_journal(limiter, |j| {
            for _ in 0..10 {
                if let Some(adm) = j.admit_limited(
                    &user_origin(1000),
                    None,
                    Some(Stream::Runtime),
                    None,
                    None,
                    Duration64::ZERO,
                ) {
                    let seq = u64::from(admitted);
                    j.commit(
                        &adm,
                        Stamp {
                            cpu: 0,
                            cpu_seq: seq,
                            monotonic: Duration64::ZERO,
                            wall: wall(),
                        },
                        simple_caller("f"),
                        &[],
                        scratch,
                    )
                    .expect("commit admitted");
                    admitted += 1;
                }
            }
            j.emit_rate_loss(0, || 999, Duration64::from_secs(1), wall(), scratch)
                .expect("emit loss");
            j.flush().expect("flush");
        });
        assert_eq!(admitted, 2, "burst of two admitted, the rest dropped");

        let mut runtime_records = 0u64;
        let mut loss_records = 0u64;
        for seg in &segments {
            let s = verify_segment(seg, None).expect("verify");
            match s.header.stream {
                Stream::Runtime => runtime_records += s.record_count,
                Stream::Journal => {
                    loss_records += s.record_count;
                    let decoded = decoded_messages(seg);
                    assert_eq!(decoded[0].2, "records dropped by rate limit");
                    assert_eq!(decoded[0].1, "kernel.journal");
                }
                other => panic!("unexpected stream {other:?}"),
            }
        }
        assert_eq!(runtime_records, 2, "only admitted records were committed");
        assert_eq!(loss_records, 1, "one coalesced loss record");
    }
}
