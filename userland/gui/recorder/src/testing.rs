//! Fakes of everything the recorder reaches outside itself, for the host
//! tests: the session's pipes, the log, the file, the slots, the sound and the
//! encoding thread.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::audio::{CaptureSource, ClockReport};
use tairix_abi::driver::audio::{ChannelMap, Frames};
use tairix_abi::recorder_ipc::{RecorderMessage, SessionMessage, RECORDER_SESSION_MESSAGE_MAX};
use tairix_abi::time::Time64;
use tairix_abi::Errno;
use tairix_fscopy::Target;
use tairix_log::{Event, Sink};
use tairix_sandbox::{SessionDescriptors, SessionTransport};

use crate::encode::{carry_out, Answer, Encoding, Job, Output, SlotMemory};
use crate::recorder::{Desk, Host};
use crate::sound::{News, SoundSource};
use tairix_parallel::Serial;

/// When every test's recording begins.
pub const STARTED_NS: u64 = 5_000_000_000;

/// The session's ends of the pipes.
#[derive(Default)]
struct Wire {
    /// What the session sent, framed.
    sent: Vec<u8>,
    /// How much of it the recorder has read.
    read: usize,
    /// What the recorder wrote.
    said: Vec<u8>,
    closed: bool,
}

/// The pipes, shared between the recorder and the test playing the session.
#[derive(Clone, Default)]
pub struct Pipes(Rc<RefCell<Wire>>);

impl Pipes {
    /// Have the session send `message`.
    pub fn send(&self, message: &SessionMessage) {
        let mut out = [0u8; RECORDER_SESSION_MESSAGE_MAX];
        let len = message.encode(&mut out).expect("a session message encodes");
        self.send_raw(&out[..len]);
    }

    /// Have the session send `payload` as one frame, whatever it holds.
    pub fn send_raw(&self, payload: &[u8]) {
        let mut wire = self.0.borrow_mut();
        let len = u32::try_from(payload.len()).expect("a small payload");
        wire.sent.extend_from_slice(&len.to_le_bytes());
        wire.sent.extend_from_slice(payload);
    }

    /// Have the session close its pipe.
    pub fn close(&self) {
        self.0.borrow_mut().closed = true;
    }

    /// Every message the recorder has said, each checked to decode.
    pub fn said(&self) -> Vec<RecorderMessage> {
        let wire = self.0.borrow();
        let mut said = Vec::new();
        let mut at = 0;
        while at + 4 <= wire.said.len() {
            let head = wire.said[at..at + 4].try_into().expect("four bytes");
            let end = at + 4 + u32::from_le_bytes(head) as usize;
            let message = RecorderMessage::decode(&wire.said[at + 4..end]);
            said.push(message.expect("a recorder message"));
            at = end;
        }
        said
    }
}

impl SessionTransport for Pipes {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
        let mut wire = self.0.borrow_mut();
        let start = wire.read;
        let left = wire.sent.len() - start;
        if left == 0 {
            return if wire.closed {
                Ok(0)
            } else {
                Err(Errno::WouldBlock)
            };
        }
        let taken = left.min(buf.len());
        buf[..taken].copy_from_slice(&wire.sent[start..start + taken]);
        wire.read += taken;
        Ok(taken)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Errno> {
        self.0.borrow_mut().said.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn descriptors(&self) -> Option<SessionDescriptors> {
        None
    }

    fn dispose(self) -> Option<i32> {
        None
    }
}

/// A log keeping the id of every event it is given.
#[derive(Clone, Default)]
pub struct Records(Rc<RefCell<Vec<u32>>>);

impl Records {
    /// Every event's id, in the order logged.
    pub fn ids(&self) -> Vec<u32> {
        self.0.borrow().clone()
    }
}

impl Sink for Records {
    fn write_event(&self, event: &Event<'_>) {
        self.0.borrow_mut().push(event.id.0);
    }
}

/// The volume the output lies on.
#[derive(Default)]
pub struct Volume {
    /// The file's bytes.
    pub bytes: Vec<u8>,
    /// Bytes past which a write is refused, as a full volume refuses it.
    pub capacity: Option<usize>,
    /// Whether the file has been made durable.
    pub synced: bool,
}

/// The session's delegated output, shared with the test that reads it back.
#[derive(Clone, Default)]
pub struct Disk(pub Rc<RefCell<Volume>>);

impl Disk {
    /// The file as it stands.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.borrow().bytes.clone()
    }
}

impl Target for Disk {
    fn write_all_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Errno> {
        let mut volume = self.0.borrow_mut();
        let start = usize::try_from(offset).map_err(|_| Errno::OutOfRange)?;
        let end = start + bytes.len();
        let fits = volume.capacity.map_or(end, |capacity| end.min(capacity));
        if fits > start {
            if volume.bytes.len() < fits {
                volume.bytes.resize(fits, 0);
            }
            volume.bytes[start..fits].copy_from_slice(&bytes[..fits - start]);
        }
        if fits < end {
            Err(Errno::NoSpace)
        } else {
            Ok(())
        }
    }

    fn set_len(&mut self, len: u64) -> Result<(), Errno> {
        let len = usize::try_from(len).map_err(|_| Errno::OutOfRange)?;
        self.0.borrow_mut().bytes.resize(len, 0);
        Ok(())
    }
}

impl Output for Disk {
    fn regular_len(&self) -> Result<Option<u64>, Errno> {
        Ok(Some(self.0.borrow().bytes.len() as u64))
    }

    fn sync(&mut self) -> Result<(), Errno> {
        self.0.borrow_mut().synced = true;
        Ok(())
    }
}

/// A slot on the heap.
pub struct Heap(Vec<u8>);

impl SlotMemory for Heap {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A capture stream, as the test drives the audio service behind it.
#[derive(Default)]
pub struct Capture {
    frame_bytes: usize,
    frames: Vec<u8>,
    /// Whether the recorder set it moving.
    pub started: bool,
    /// Frames the service lost, said in the next news.
    pub lost: u64,
    clock: Option<ClockReport>,
}

/// A capture stream the test captures into.
#[derive(Clone, Default)]
pub struct FakeSound(pub Rc<RefCell<Capture>>);

impl FakeSound {
    /// A stream of frames in `layout`.
    pub fn open(layout: ChannelMap) -> Self {
        let frame_bytes = usize::from(layout.channels()) * core::mem::size_of::<f32>();
        Self(Rc::new(RefCell::new(Capture {
            frame_bytes,
            ..Capture::default()
        })))
    }

    /// Have the service capture `frames` frames whose every sample is `value`.
    pub fn capture(&self, frames: usize, value: f32) {
        let mut capture = self.0.borrow_mut();
        let samples = frames * capture.frame_bytes / core::mem::size_of::<f32>();
        for _ in 0..samples {
            capture.frames.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// Have the service report the stream's device clocked at
    /// `rate_millihertz`.
    pub fn clocked(&self, rate_millihertz: u32) {
        self.0.borrow_mut().clock = Some(clock_report(rate_millihertz));
    }
}

/// A device clock measured at `rate_millihertz`.
pub fn clock_report(rate_millihertz: u32) -> ClockReport {
    ClockReport {
        position: Frames::ZERO,
        rate_millihertz,
        sampled_at: Time64::UNIX_EPOCH,
    }
}

impl SoundSource for FakeSound {
    fn start(&mut self) -> Result<(), Errno> {
        self.0.borrow_mut().started = true;
        Ok(())
    }

    fn read(&mut self, out: &mut [u8]) -> Result<usize, Errno> {
        let mut capture = self.0.borrow_mut();
        let frame_bytes = capture.frame_bytes;
        let frames = (out.len() / frame_bytes).min(capture.frames.len() / frame_bytes);
        let bytes = frames * frame_bytes;
        out[..bytes].copy_from_slice(&capture.frames[..bytes]);
        let rest = capture.frames.split_off(bytes);
        capture.frames = rest;
        Ok(frames)
    }

    fn take_news(&mut self) -> Result<News, Errno> {
        let lost_frames = core::mem::take(&mut self.0.borrow_mut().lost);
        Ok(News {
            lost_frames,
            gone: false,
        })
    }

    fn clock(&mut self) -> Result<ClockReport, Errno> {
        self.0.borrow().clock.ok_or(Errno::WouldBlock)
    }
}

/// The encoding thread, its work carried out on the caller's thread as it is
/// submitted, or held while the test holds it.
pub struct Inline {
    encoding: Encoding<Disk, Heap, Serial>,
    hold: Rc<Cell<bool>>,
    held: VecDeque<Job>,
    answers: VecDeque<Answer>,
}

impl Inline {
    /// Carry out every job held, unless the test holds them.
    fn carry_held(&mut self) {
        if self.hold.get() {
            return;
        }
        while let Some(mut job) = self.held.pop_front() {
            let answer = carry_out(&mut self.encoding, &mut job);
            self.answers.push_back(answer);
        }
    }
}

impl Desk for Inline {
    fn submit(&mut self, job: Job) -> Result<(), Errno> {
        self.held.push_back(job);
        self.carry_held();
        Ok(())
    }

    fn collect(&mut self) -> Option<Answer> {
        self.carry_held();
        self.answers.pop_front()
    }
}

/// Everything else: the clock, the room, the output, the slots and the
/// streams.
pub struct FakeHost {
    /// The monotonic clock.
    pub now_ns: u64,
    /// The bytes the recorder may map.
    pub room: u64,
    /// The output the session delegated.
    pub disk: Disk,
    /// Why the output cannot be redeemed, where it cannot.
    pub refuse_output: Option<Errno>,
    /// Why a slot cannot be mapped, where it cannot.
    pub refuse_map: Option<Errno>,
    /// The bytes every slot maps, where not the ones asked for.
    pub mapped_len: Option<usize>,
    /// Why the audio service refuses the sound, where it does.
    pub refuse_sound: Option<Errno>,
    /// Every slot's handle, in the order mapped.
    pub mapped: Vec<u64>,
    /// Every stream opened, with what it hears and in what layout.
    pub streams: Vec<(CaptureSource, ChannelMap, FakeSound)>,
    /// Whether the encoding thread holds its work.
    pub hold: Rc<Cell<bool>>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            now_ns: STARTED_NS,
            room: u64::MAX,
            disk: Disk::default(),
            refuse_output: None,
            refuse_map: None,
            mapped_len: None,
            refuse_sound: None,
            mapped: Vec::new(),
            streams: Vec::new(),
            hold: Rc::default(),
        }
    }
}

impl Host for FakeHost {
    type Output = Disk;
    type Slot = Heap;
    type Sound = FakeSound;
    type Desk = Inline;
    type Runner = Serial;

    fn now_ns(&self) -> u64 {
        self.now_ns
    }

    fn memory_room(&self) -> u64 {
        self.room
    }

    fn open_output(&mut self, _handle: u64) -> Result<Disk, Errno> {
        self.refuse_output
            .map_or_else(|| Ok(self.disk.clone()), Err)
    }

    fn map_slot(&mut self, handle: u64, len: usize) -> Result<Heap, Errno> {
        if let Some(errno) = self.refuse_map {
            return Err(errno);
        }
        self.mapped.push(handle);
        Ok(Heap(vec![0x80; self.mapped_len.unwrap_or(len)]))
    }

    fn open_sound(
        &mut self,
        source: CaptureSource,
        layout: ChannelMap,
    ) -> Result<FakeSound, Errno> {
        if let Some(errno) = self.refuse_sound {
            return Err(errno);
        }
        let stream = FakeSound::open(layout);
        self.streams.push((source, layout, stream.clone()));
        Ok(stream)
    }

    fn runner(&mut self) -> Serial {
        Serial
    }

    fn start_encoding(&mut self, encoding: Encoding<Disk, Heap, Serial>) -> Result<Inline, Errno> {
        Ok(Inline {
            encoding,
            hold: Rc::clone(&self.hold),
            held: VecDeque::new(),
            answers: VecDeque::new(),
        })
    }
}
