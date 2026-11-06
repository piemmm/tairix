//! The encoding thread's work: each frame coded as VP8 from the slot holding
//! it, each batch of Opus packets, and the file finished, all written through
//! the muxer to the session's delegated output.

use alloc::vec::Vec;

use tairix_abi::recorder_ipc::{
    RecordEnding, RecorderDamage, RecorderSlot, RECORDER_DAMAGE_RECTS, RECORDER_SLOTS,
};
use tairix_abi::Errno;
use tairix_fscopy::Target;
use tairix_parallel::JobRunner;
use tairix_util::fallible;
use tairix_video::vp8::{self, Changed, Vp8Error};
use tairix_video::webm::{Layout, MuxError, Muxer};

use crate::sound::PacketBatch;

/// How far apart keyframes come in recording time: a reader seeks to within
/// ten seconds of anywhere.
pub const KEYFRAME_INTERVAL_NS: u64 = 10_000_000_000;

/// The memory a slot is: the session's, granted to the recorder.
pub trait SlotMemory {
    /// The slot's bytes: rows of RGBA8 pixels as the session composed them.
    fn bytes(&self) -> &[u8];
}

/// The file a recording is written to: the session's delegated output.
pub trait Output: Target {
    /// Its length where it is a regular file, `None` for anything else.
    ///
    /// # Errors
    ///
    /// The refusal of the stat.
    fn regular_len(&self) -> Result<Option<u64>, Errno>;

    /// Make what has been written durable.
    ///
    /// # Errors
    ///
    /// The refusal of the sync.
    fn sync(&mut self) -> Result<(), Errno>;
}

/// What failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The muxer refused for good: the file refused a write or a cut, or a
    /// frame broke the format.
    Mux(MuxError),
    /// The encoder refused a picture.
    Encode(Vp8Error),
    /// The finished file could not be made durable.
    Sync(Errno),
}

impl Failure {
    /// What failed, as it is stated.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Mux(MuxError::Sink(_)) => "the file refused a write",
            Self::Mux(_) => "the recording could not be written",
            Self::Encode(_) => "a picture could not be coded",
            Self::Sync(_) => "the file could not be made durable",
        }
    }

    /// The error beneath it, where there is one.
    #[must_use]
    pub const fn errno(self) -> Option<Errno> {
        match self {
            Self::Mux(MuxError::Sink(errno)) | Self::Sync(errno) => Some(errno),
            Self::Mux(MuxError::OutOfMemory) | Self::Encode(Vp8Error::OutOfMemory) => {
                Some(Errno::OutOfMemory)
            }
            Self::Mux(_) | Self::Encode(_) => None,
        }
    }
}

/// Why a recording cannot go on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Halt {
    /// The volume filled, or the extent the delegation allows was reached: the
    /// file ends at the last block written whole.
    DiskFull,
    /// Something else failed.
    Failed(Failure),
}

/// A refusal of the muxer's as the recording takes it: a full volume, or a
/// delegation's extent reached, is the disk filling; anything else a failure.
const fn halt_of(error: MuxError) -> Halt {
    match error {
        MuxError::Sink(Errno::NoSpace | Errno::LimitExceeded) => Halt::DiskFull,
        other => Halt::Failed(Failure::Mux(other)),
    }
}

/// What came of one frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Coded {
    /// It is in the file.
    Kept,
    /// It is not: the memory it needed could not be had. The next frame is
    /// coded whole, as a keyframe, so none of its changes is lost.
    Dropped,
}

/// A finished file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Closed {
    /// The bytes it holds.
    pub bytes: u64,
    /// Why it finished.
    pub ending: RecordEnding,
}

/// Work for the encoding thread.
// A job is queued and taken at once, a few at a time; boxing a frame's damage
// would cost an allocation a frame.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Job {
    /// Code the frame a slot holds, and write it.
    Frame {
        /// The slot.
        slot: usize,
        /// When it is shown, from the recording's start.
        at_ns: u64,
        /// What changed since the frame before it.
        damage: RecorderDamage,
    },
    /// Write a batch of Opus packets.
    Sound(PacketBatch),
    /// Finish the file.
    Finish {
        /// When the recording ends, from its start.
        ends_at_ns: u64,
    },
}

/// What the encoding thread did.
#[derive(Debug)]
pub enum Answer {
    /// A frame was coded, or could not be, and its slot is the session's again.
    Frame {
        /// The slot.
        slot: usize,
        /// What came of the frame, or why the recording cannot go on.
        outcome: Result<Coded, Halt>,
    },
    /// A batch was written.
    Sound {
        /// The batch, for the service thread to fill again.
        batch: PacketBatch,
        /// Why the recording cannot go on, where it cannot.
        outcome: Result<(), Halt>,
    },
    /// The file is finished, or why it could not be.
    Finished(Result<Closed, Failure>),
}

/// Everything the encoding thread holds: the slots it reads, the encoder and
/// the cores it codes on, the muxer and the file.
pub struct Encoding<O, M, R> {
    output: O,
    /// The file's layout until the first job writes its header, so a
    /// recording that never begins leaves its file empty.
    unwritten: Option<Layout>,
    /// `None` until the header is written, and once the file is finished.
    muxer: Option<Muxer>,
    video: vp8::Encoder,
    runner: R,
    slots: Vec<M>,
    stride: usize,
    /// The whole picture, as the encoder names a region.
    whole: Changed,
    /// The frame last coded.
    coded: Vec<u8>,
    last_key_ns: Option<u64>,
    /// A frame was not coded, or never reached the file: the next is coded
    /// whole, as a keyframe, so the file and the encoder's reference agree.
    rekey: bool,
    halted: Option<Halt>,
}

impl<O: Output, M: SlotMemory, R: JobRunner> Encoding<O, M, R> {
    /// The encoding of `slot`-sized pictures from `slots` through `video` on
    /// `runner`'s cores into `output`, laid out as `layout`.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for slots that are not the ring's or do not
    /// hold the picture, and [`Errno::OutOfMemory`] where the room for a coded
    /// frame cannot be had.
    pub fn new(
        output: O,
        layout: Layout,
        (video, runner): (vp8::Encoder, R),
        slots: Vec<M>,
        slot: RecorderSlot,
    ) -> Result<Self, Errno> {
        let short = slots.iter().any(|memory| memory.bytes().len() < slot.len());
        if slots.len() != RECORDER_SLOTS || short {
            return Err(Errno::LengthOutOfRange);
        }
        let mut coded = Vec::new();
        if !fallible::reserve(&mut coded, frame_room(slot)) {
            return Err(Errno::OutOfMemory);
        }
        Ok(Self {
            output,
            unwritten: Some(layout),
            muxer: None,
            video,
            runner,
            slots,
            stride: slot.stride(),
            whole: Changed {
                x: 0,
                y: 0,
                width: slot.width(),
                height: slot.height(),
            },
            coded,
            last_key_ns: None,
            rekey: false,
            halted: None,
        })
    }

    /// Code the frame slot `slot` holds, shown `at_ns` into the recording,
    /// where `damage` changed, and write it.
    fn frame(&mut self, slot: usize, at_ns: u64, damage: &RecorderDamage) -> Result<Coded, Halt> {
        if let Some(halt) = self.halted {
            return Err(halt);
        }
        let key = self.rekey
            || self
                .last_key_ns
                .is_none_or(|key| at_ns.saturating_sub(key) >= KEYFRAME_INTERVAL_NS);
        let mut changed = [self.whole; RECORDER_DAMAGE_RECTS];
        let named = if self.rekey {
            1
        } else {
            changes(damage, &mut changed)
        };
        let result = match self.slots.get(slot) {
            Some(memory) => self.video.encode(
                memory.bytes(),
                self.stride,
                &changed[..named],
                key,
                &mut self.coded,
                &self.runner,
            ),
            None => Err(Vp8Error::InvalidGeometry),
        };
        let encoded = match result {
            Ok(encoded) => encoded,
            Err(Vp8Error::OutOfMemory) => {
                self.rekey = true;
                return Ok(Coded::Dropped);
            }
            Err(error) => return Err(self.stop(Halt::Failed(Failure::Encode(error)))),
        };
        let pushed = match muxing(&mut self.unwritten, &mut self.muxer, &mut self.output) {
            Ok(muxer) => muxer.push_video(&mut self.output, at_ns, encoded.keyframe, &self.coded),
            Err(error) => Err(error),
        };
        match pushed {
            Ok(()) => {
                self.rekey = false;
                if encoded.keyframe {
                    self.last_key_ns = Some(at_ns);
                }
                Ok(Coded::Kept)
            }
            Err(MuxError::OutOfMemory) => {
                self.rekey = true;
                Ok(Coded::Dropped)
            }
            Err(error) => Err(self.stop(halt_of(error))),
        }
    }

    /// Write the packets `batch` holds.
    fn sound(&mut self, batch: &PacketBatch) -> Result<(), Halt> {
        if let Some(halt) = self.halted {
            return Err(halt);
        }
        let pushed = match muxing(&mut self.unwritten, &mut self.muxer, &mut self.output) {
            Ok(muxer) => push_packets(muxer, &mut self.output, batch),
            Err(error) => Err(error),
        };
        pushed.map_err(|error| self.stop(halt_of(error)))
    }

    /// Finish the file, its recording ending `ends_at_ns` in, and make it
    /// durable.
    fn finish(&mut self, ends_at_ns: u64) -> Result<Closed, Failure> {
        if self.halted.is_none() {
            // A recording stopped before its first frame still makes a file.
            if let Err(error) = muxing(&mut self.unwritten, &mut self.muxer, &mut self.output) {
                self.halted = Some(halt_of(error));
            }
        }
        let Some(muxer) = self.muxer.take() else {
            // No header stands: the file is empty.
            return match self.halted {
                Some(Halt::DiskFull) => Ok(Closed {
                    bytes: 0,
                    ending: RecordEnding::DiskFull,
                }),
                Some(Halt::Failed(failure)) => Err(failure),
                None => Err(Failure::Mux(MuxError::Stopped)),
            };
        };
        let finished = muxer
            .finish(&mut self.output, ends_at_ns)
            .map_err(Failure::Mux)?;
        self.output.sync().map_err(Failure::Sync)?;
        let cut = finished.cut_short || self.halted == Some(Halt::DiskFull);
        let ending = if cut {
            RecordEnding::DiskFull
        } else {
            RecordEnding::Stopped
        };
        Ok(Closed {
            bytes: finished.len,
            ending,
        })
    }

    /// Note that the recording cannot go on, for `halt`, answering it.
    fn stop(&mut self, halt: Halt) -> Halt {
        self.halted = Some(halt);
        halt
    }
}

/// The muxer, the file's header written first where no job has written it.
/// A header refused empties the file, which would otherwise be kept as a
/// recording.
fn muxing<'a>(
    unwritten: &mut Option<Layout>,
    muxer: &'a mut Option<Muxer>,
    output: &mut impl Target,
) -> Result<&'a mut Muxer, MuxError> {
    if let Some(layout) = unwritten.take() {
        match Muxer::begin(output, &layout) {
            Ok(begun) => *muxer = Some(begun),
            Err(error) => {
                // The refusal stands whether or not the cut lands.
                let _ = output.set_len(0);
                return Err(error);
            }
        }
    }
    muxer.as_mut().ok_or(MuxError::Stopped)
}

/// Write every packet `batch` holds through `muxer`; one whose copy could not
/// be held is lost alone.
fn push_packets(
    muxer: &mut Muxer,
    output: &mut impl Target,
    batch: &PacketBatch,
) -> Result<(), MuxError> {
    for (at_ns, packet) in batch.packets() {
        match muxer.push_audio(output, at_ns, packet) {
            Ok(()) | Err(MuxError::OutOfMemory) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// What `damage` names, as the encoder's regions, into the front of
/// `changed`; how many.
fn changes(damage: &RecorderDamage, changed: &mut [Changed; RECORDER_DAMAGE_RECTS]) -> usize {
    for (region, rect) in changed.iter_mut().zip(damage.rects()) {
        *region = Changed {
            x: rect.x,
            y: rect.y,
            width: rect.width_px,
            height: rect.height_px,
        };
    }
    damage.len()
}

/// The room a coded frame is given before `Ready`: its picture's raw 4:2:0
/// size, room enough for a frame at any setting a recording uses; a larger
/// one grows it, fallibly.
#[must_use]
pub fn frame_room(slot: RecorderSlot) -> usize {
    let (width, height) = (slot.width() as usize, slot.height() as usize);
    let chroma = width.div_ceil(2).saturating_mul(height.div_ceil(2));
    width
        .saturating_mul(height)
        .saturating_add(chroma.saturating_mul(2))
}

/// Carry `job` out: the encoding thread's whole work, and what came of it.
pub fn carry_out<O: Output, M: SlotMemory, R: JobRunner>(
    encoding: &mut Encoding<O, M, R>,
    job: &mut Job,
) -> Answer {
    match job {
        Job::Frame {
            slot,
            at_ns,
            damage,
        } => Answer::Frame {
            slot: *slot,
            outcome: encoding.frame(*slot, *at_ns, damage),
        },
        Job::Sound(batch) => {
            let outcome = encoding.sound(batch);
            Answer::Sound {
                batch: core::mem::take(batch),
                outcome,
            }
        }
        Job::Finish { ends_at_ns } => Answer::Finished(encoding.finish(*ends_at_ns)),
    }
}
