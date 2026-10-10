//! A playback stream over a cyclic DMA buffer, the engine of every audio
//! device a DMA controller feeds (`plans/SOUND.md` SND8).
//!
//! The buffer holds [`PERIODS`] periods, which the channel plays in turn and
//! reports the boundaries of through a posted wait. Each boundary frees the
//! period just finished, and the next period of the mixer's ring goes into
//! it. The period after the one playing is always written: when the mixer has
//! not supplied it, it is written as silence and counted lost, because a
//! period left alone would replay a lap-old sound.
//!
//! Between streams the stream is parked: the buffer is filled with the
//! device's parked frame and the channel stopped at its next boundary, once
//! that frame has reached the device. What a frame looks like in the buffer is
//! the device's own, through its [`FrameCodec`]; everything else here is the
//! same for every such device.
//!
//! A buffer frame is as many bytes as a ring frame of two channels in the
//! codec's [`format`](FrameCodec::format), so a period is read from the ring
//! straight into its place in the buffer and encoded where it lies.

use tairix_abi::driver::audio::{AudioInterrupt, AudioServiced, Frames, Rate, SampleFormat};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::driver::dmaengine::{WaitEnd, WaitReport};
use tairix_abi::time::Time64;
use tairix_abi::DriverError;

/// Periods the buffer holds.
pub const PERIODS: u32 = 4;

/// Channels a frame carries.
pub const CHANNELS: u8 = 2;

/// Most frames a period may hold: the buffer then spans at most 512 KiB.
pub const MAX_PERIOD_FRAMES: u32 = 16_384;

/// Most frames the mixer's ring may hold ahead of the buffer, four buffers'
/// worth at the largest period.
pub const MAX_RING_FRAMES: u32 = MAX_PERIOD_FRAMES * PERIODS;

/// Bytes a frame of `format` occupies, which must be whole FIFO words.
///
/// # Errors
///
/// [`DriverError::Unsupported`] for a frame that is not.
pub fn frame_bytes(format: SampleFormat) -> Result<u32, DriverError> {
    let sample = u32::try_from(format.bytes_per_sample()).map_err(|_| DriverError::Unsupported)?;
    let bytes = sample * u32::from(CHANNELS);
    if !bytes.is_multiple_of(4) {
        return Err(DriverError::Unsupported);
    }
    Ok(bytes)
}

/// Check `endpoint` names the one endpoint a cyclic stream is, index 0.
///
/// # Errors
///
/// [`DriverError::NotFound`] for any other.
pub const fn sole_endpoint(endpoint: u16) -> Result<(), DriverError> {
    if endpoint == 0 {
        Ok(())
    } else {
        Err(DriverError::NotFound)
    }
}

/// `frames` of `frame_bytes` each as a period's bytes, refused when empty or
/// past [`MAX_PERIOD_FRAMES`].
const fn period_bytes(frames: u32, frame_bytes: u32) -> Result<u32, DriverError> {
    if frames == 0 || frames > MAX_PERIOD_FRAMES {
        return Err(DriverError::OutOfRange);
    }
    Ok(frames * frame_bytes)
}

/// What a stream needs from its DMA channel.
pub trait DmaPort {
    /// Have the controller carve a buffer of `periods` periods of
    /// `period_bytes` each, and map it.
    ///
    /// # Errors
    ///
    /// The controller's refusal.
    fn prepare(&mut self, period_bytes: u32, periods: u32) -> Result<(), DriverError>;

    /// The mapped buffer.
    fn buffer(&mut self) -> &mut [u8];

    /// Start the channel from its first period.
    ///
    /// # Errors
    ///
    /// The controller's refusal.
    fn start(&mut self) -> Result<(), DriverError>;

    /// Stop the channel.
    ///
    /// # Errors
    ///
    /// The controller's refusal.
    fn stop(&mut self) -> Result<(), DriverError>;

    /// Block until the first boundary past byte position `after`.
    ///
    /// # Errors
    ///
    /// The controller's refusal.
    fn wait(&mut self, after: u64) -> Result<WaitReport, DriverError>;

    /// Post a wait for the first boundary past byte position `after`, to be
    /// answered within `deadline_ns`.
    ///
    /// # Errors
    ///
    /// The transport's refusal.
    fn post_wait(&mut self, after: u64, deadline_ns: u64) -> Result<(), DriverError>;

    /// Collect the posted wait's answer, [`None`] while it is pending or when
    /// none is posted.
    ///
    /// # Errors
    ///
    /// The controller's or the transport's refusal; the wait is spent.
    fn reap_wait(&mut self) -> Result<Option<WaitReport>, DriverError>;

    /// Whether a wait is posted and not yet collected.
    fn is_waiting(&self) -> bool;

    /// The monotonic clock.
    fn now(&self) -> Time64;
}

/// How a device's frames sit in its buffer.
pub trait FrameCodec {
    /// The ring's sample format, two channels of which make a frame.
    fn format(&self) -> SampleFormat;

    /// Turn `frame`, a ring frame, left then right, into the buffer's form
    /// where it lies.
    fn encode(&mut self, frame: &mut [u8]);

    /// Write a frame of silence where the mixer supplied none, mid-stream.
    fn silence(&mut self, frame: &mut [u8]);

    /// Write the frame a parked buffer holds, which the device is left on.
    fn parked(&self, frame: &mut [u8]);

    /// Forget what the last stream left, as a stream starts afresh.
    fn reset(&mut self);
}

/// One configured stream.
#[derive(Debug)]
struct Stream {
    period_frames: u32,
    frame_bytes: u32,
    /// Periods written into the buffer since it was last set up.
    written: u64,
    /// Periods the channel has finished since it started.
    played: u64,
    running: bool,
    draining: bool,
    /// Where the last frame the mixer supplied lies: its period, and the
    /// frames of that period that are the mixer's.
    supplied_through: Option<(u64, u32)>,
    /// The stream position of the first frame played since the start.
    base: u64,
    xrun_frames: u64,
    /// The latest boundary: the position reached, and when.
    sampled: Option<(u64, Time64)>,
}

impl Stream {
    const fn new(period_frames: u32, frame_bytes: u32) -> Self {
        Self {
            period_frames,
            frame_bytes,
            written: 0,
            played: 0,
            running: false,
            draining: false,
            supplied_through: None,
            base: 0,
            xrun_frames: 0,
            sampled: None,
        }
    }

    const fn period_bytes(&self) -> u64 {
        self.period_frames as u64 * self.frame_bytes as u64
    }

    /// Forget the progress through the buffer, as the stream stops.
    fn rewind(&mut self) {
        self.running = false;
        self.draining = false;
        self.written = 0;
        self.played = 0;
        self.supplied_through = None;
    }

    /// Whether a drain has played the last frame the mixer supplied, by
    /// `played` periods; the position it fell silent at when it has.
    fn drained_at(&self, played: u64) -> Option<u64> {
        if !self.draining {
            return None;
        }
        let period_frames = u64::from(self.period_frames);
        match self.supplied_through {
            None => Some(self.base),
            Some((period, frames)) => {
                (played > period).then_some(self.base + period * period_frames + u64::from(frames))
            }
        }
    }
}

/// A playback stream over a cyclic DMA buffer.
pub struct CyclicPlayback<D: DmaPort, C: FrameCodec> {
    dma: D,
    codec: C,
    stream: Option<Stream>,
    /// How long a boundary may be in coming before the channel is taken to
    /// have stalled: the last configured buffer, twice over. It outlives the
    /// stream, so a release still parks against it.
    deadline_ns: u64,
    /// The serve loop's say over boundary waits: masked while there is
    /// nowhere to put frames.
    events: bool,
    /// The buffer is parked and the channel is to stop at its next boundary.
    parking: bool,
    /// The channel is running, for a stream or for its parking.
    clocking: bool,
    /// Bytes the channel had moved at the latest boundary it reported.
    reached: u64,
}

fn readable(ring: &PcmRing<'_>) -> Result<u32, DriverError> {
    ring.readable_frames().map_err(|_| DriverError::BadMagic)
}

impl<D: DmaPort, C: FrameCodec> CyclicPlayback<D, C> {
    /// The stream over `dma`, its frames laid out by `codec`.
    pub const fn new(dma: D, codec: C) -> Self {
        Self {
            dma,
            codec,
            stream: None,
            deadline_ns: 0,
            events: false,
            parking: false,
            clocking: false,
            reached: 0,
        }
    }

    /// The DMA channel the stream drives.
    pub fn dma_mut(&mut self) -> &mut D {
        &mut self.dma
    }

    /// Whether a stream is configured and clocking.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.stream.as_ref().is_some_and(|stream| stream.running)
    }

    /// Whether the channel runs, for a stream or while it parks; once it
    /// stops, nothing more reaches the device.
    #[must_use]
    pub const fn is_clocking(&self) -> bool {
        self.clocking
    }

    /// Stop a parking channel now rather than at its next boundary.
    ///
    /// # Errors
    ///
    /// [`DriverError::Busy`] while a stream runs, which [`stop`](Self::stop)
    /// ends; or the DMA channel's refusal.
    pub fn halt_parking(&mut self) -> Result<(), DriverError> {
        if self.is_running() {
            return Err(DriverError::Busy);
        }
        self.halt()
    }

    /// Play the `frames` frames `frame_at` writes once, then park.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for no frames or more than
    /// [`MAX_PERIOD_FRAMES`], [`DriverError::Unsupported`] for a format whose
    /// frame is not whole FIFO words, or the DMA channel's refusal.
    pub fn play_once(
        &mut self,
        frames: u32,
        mut frame_at: impl FnMut(u32, &mut [u8]),
    ) -> Result<(), DriverError> {
        let frame_bytes = frame_bytes(self.codec.format())?;
        let period_bytes = period_bytes(frames, frame_bytes)?;
        self.halt()?;
        self.stream = None;
        self.dma.prepare(period_bytes, 2)?;
        let Self { dma, codec, .. } = self;
        let buffer = dma.buffer();
        let (once, rest) = buffer.split_at_mut((period_bytes as usize).min(buffer.len()));
        for (frame, words) in (0..).zip(once.chunks_exact_mut(frame_bytes as usize)) {
            frame_at(frame, words);
        }
        for words in rest.chunks_exact_mut(frame_bytes as usize) {
            codec.parked(words);
        }
        self.dma.start()?;
        self.clocking = true;
        self.reached = 0;
        // The answer comes once the frames have played and the parked frame
        // is playing.
        let played = self.dma.wait(0);
        self.halt()?;
        played.map(|_| ())
    }

    fn stream(&self) -> Result<&Stream, DriverError> {
        self.stream.as_ref().ok_or(DriverError::DeviceFault)
    }

    fn stream_mut(&mut self) -> Result<&mut Stream, DriverError> {
        self.stream.as_mut().ok_or(DriverError::DeviceFault)
    }

    /// Stop the channel now, collecting the answer its stop gives a posted
    /// wait, so the next wait can be posted.
    fn halt(&mut self) -> Result<(), DriverError> {
        self.parking = false;
        if !self.clocking {
            return Ok(());
        }
        self.clocking = false;
        let stopped = self.dma.stop();
        if self.dma.is_waiting() {
            let _ = self.dma.reap_wait();
        }
        stopped
    }

    fn fill_parked(&mut self) {
        let Ok(frame_bytes) = frame_bytes(self.codec.format()) else {
            return;
        };
        let Self { dma, codec, .. } = self;
        for words in dma.buffer().chunks_exact_mut(frame_bytes as usize) {
            codec.parked(words);
        }
    }

    /// Fill the buffer with the parked frame, and stop the channel once it
    /// has played some.
    fn park(&mut self) -> Result<(), DriverError> {
        self.fill_parked();
        if !self.clocking {
            return Ok(());
        }
        self.parking = true;
        if !self.dma.is_waiting() {
            self.dma.post_wait(self.reached, self.deadline_ns)?;
        }
        Ok(())
    }

    fn post_boundary_wait(&mut self) -> Result<(), DriverError> {
        if self.events && !self.dma.is_waiting() {
            self.dma.post_wait(self.reached, self.deadline_ns)?;
        }
        Ok(())
    }

    /// Write the period the stream's next period is due in, taking `take`
    /// frames from `ring` and making silence of the rest.
    fn write_period(
        &mut self,
        ring: Option<&mut PcmRing<'_>>,
        take: u32,
    ) -> Result<u32, DriverError> {
        let (slot, period_frames, frame_bytes) = {
            let stream = self.stream()?;
            (
                stream.written % u64::from(PERIODS),
                stream.period_frames,
                stream.frame_bytes as usize,
            )
        };
        let period_bytes = period_frames as usize * frame_bytes;
        let start = usize::try_from(slot).map_err(|_| DriverError::OutOfRange)? * period_bytes;
        let Self { dma, codec, .. } = self;
        let period = dma
            .buffer()
            .get_mut(start..start + period_bytes)
            .ok_or(DriverError::DeviceFault)?;
        let (supplied, silent) = period.split_at_mut(take as usize * frame_bytes);
        let taken = match ring {
            Some(ring) if take > 0 => ring.read(supplied).map_err(|_| DriverError::BadMagic)?,
            _ => 0,
        };
        if taken != take {
            return Err(DriverError::BadMagic);
        }
        for words in supplied.chunks_exact_mut(frame_bytes) {
            codec.encode(words);
        }
        for words in silent.chunks_exact_mut(frame_bytes) {
            codec.silence(words);
        }
        let stream = self.stream_mut()?;
        if take > 0 {
            stream.supplied_through = Some((stream.written, take));
        }
        stream.written += 1;
        Ok(take)
    }

    /// Before the start: stage whole periods, as many as the buffer holds.
    fn stage(&mut self, ring: &mut PcmRing<'_>) -> Result<u32, DriverError> {
        let mut transferred = 0;
        loop {
            let stream = self.stream()?;
            let period_frames = stream.period_frames;
            if stream.written >= u64::from(PERIODS) || readable(ring)? < period_frames {
                return Ok(transferred);
            }
            transferred += self.write_period(Some(ring), period_frames)?;
        }
    }

    /// While clocking: keep the period after the one playing written, and
    /// write ahead as far as the ring's whole periods reach.
    fn refill(&mut self, ring: &mut PcmRing<'_>) -> Result<u32, DriverError> {
        let mut transferred = 0;
        loop {
            let stream = self.stream()?;
            let (written, played, period_frames, draining) = (
                stream.written,
                stream.played,
                stream.period_frames,
                stream.draining,
            );
            if written >= played + u64::from(PERIODS) {
                return Ok(transferred);
            }
            let available = readable(ring)?;
            let due = written < played + 2;
            if available >= period_frames {
                transferred += self.write_period(Some(ring), period_frames)?;
            } else if draining {
                // A drain's tail is short, not lost: it is the end.
                transferred += self.write_period(Some(ring), available)?;
            } else if due {
                transferred += self.write_period(Some(ring), available)?;
                self.stream_mut()?.xrun_frames += u64::from(period_frames - available);
            } else {
                return Ok(transferred);
            }
        }
    }

    /// Set the stream up at `rate` for periods of `period_frames`, the buffer
    /// parked.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for an empty period or one past
    /// [`MAX_PERIOD_FRAMES`], [`DriverError::Unsupported`] for a format whose
    /// frame is not whole FIFO words, [`DriverError::Busy`] while a stream
    /// clocks, or the DMA channel's refusal.
    pub fn configure(&mut self, rate: Rate, period_frames: u32) -> Result<(), DriverError> {
        let frame_bytes = frame_bytes(self.codec.format())?;
        let period_bytes = period_bytes(period_frames, frame_bytes)?;
        if self.is_running() {
            return Err(DriverError::Busy);
        }
        self.halt()?;
        self.stream = None;
        let period_ns = u64::from(period_frames) * 1_000_000_000 / u64::from(rate.hz());
        self.deadline_ns = period_ns * u64::from(PERIODS) * 2;
        self.dma.prepare(period_bytes, PERIODS)?;
        self.fill_parked();
        self.codec.reset();
        self.stream = Some(Stream::new(period_frames, frame_bytes));
        Ok(())
    }

    /// Begin clocking, the first frame at stream position `at`.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when nothing is configured, or the DMA
    /// channel's refusal.
    pub fn start(&mut self, at: Frames) -> Result<(), DriverError> {
        if self.stream()?.running {
            return Ok(());
        }
        // A parking channel is still clocking its parked frame; it starts
        // again from its first period.
        self.halt()?;
        {
            let stream = self.stream_mut()?;
            stream.base = at.get();
            stream.played = 0;
            stream.sampled = None;
            stream.draining = false;
        }
        // The channel plays the first two periods before the first boundary
        // can be answered, so both are written, silence counted lost where
        // the mixer staged nothing.
        while self.stream()?.written < 2 {
            self.write_period(None, 0)?;
            let stream = self.stream_mut()?;
            stream.xrun_frames += u64::from(stream.period_frames);
        }
        self.dma.start()?;
        self.clocking = true;
        self.reached = 0;
        self.stream_mut()?.running = true;
        self.post_boundary_wait()
    }

    /// Stop clocking, keeping `at` as the position a resume starts from.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when nothing is configured, or the DMA
    /// channel's refusal.
    pub fn stop(&mut self, at: Frames) -> Result<(), DriverError> {
        let stream = self.stream_mut()?;
        stream.rewind();
        stream.base = at.get();
        stream.sampled = None;
        self.codec.reset();
        self.park()
    }

    /// Accept no more frames and stop once those queued have played.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when nothing is configured, or the DMA
    /// channel's refusal.
    pub fn drain(&mut self) -> Result<(), DriverError> {
        let stream = self.stream_mut()?;
        if !stream.running {
            return Ok(());
        }
        stream.draining = true;
        if stream.supplied_through.is_none() {
            // Nothing the mixer supplied is queued, so the drain is already
            // over.
            stream.rewind();
            return self.park();
        }
        Ok(())
    }

    /// Move frames from `ring` into the buffer — staged before the start,
    /// refilled after — and report the position.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a ring of another shape than two
    /// channels in the codec's format or counters that fail validation,
    /// [`DriverError::DeviceFault`] when nothing is configured, or the DMA
    /// channel's refusal.
    pub fn service(&mut self, ring: &mut PcmRing<'_>) -> Result<AudioServiced, DriverError> {
        let geometry = ring.geometry();
        if geometry.format() != self.codec.format() || geometry.channels() != CHANNELS {
            return Err(DriverError::BadMagic);
        }
        let transferred = if self.stream()?.running {
            self.refill(ring)?
        } else {
            // Staged frames follow, so a parking channel need not wait for
            // its parked frame to reach the device.
            self.halt()?;
            self.stage(ring)?
        };
        let stream = self.stream()?;
        let (position, sampled_at) = match stream.sampled {
            Some((position, at)) => (Frames::new(position), at),
            None => (Frames::new(stream.base), self.dma.now()),
        };
        Ok(AudioServiced {
            transferred,
            running: stream.running,
            position,
            xrun_frames: stream.xrun_frames,
            sampled_at,
        })
    }

    /// Forget the configuration, the buffer parked.
    ///
    /// # Errors
    ///
    /// The DMA channel's refusal.
    pub fn release(&mut self) -> Result<(), DriverError> {
        if self.stream.take().is_some() {
            self.park()?;
        }
        Ok(())
    }

    /// Collect a boundary the channel reported, if one is waiting.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a channel that faulted or let the
    /// wait's deadline pass, which ends the stream; or another refusal of
    /// the wait.
    pub fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        let report = match self.dma.reap_wait() {
            Ok(Some(report)) => report,
            Ok(None) => return Ok(AudioInterrupt::NONE),
            Err(err) => {
                let _ = self.halt();
                if let Some(stream) = self.stream.as_mut() {
                    stream.running = false;
                }
                return Err(err);
            }
        };
        match report.end {
            WaitEnd::Boundary => self.boundary(&report),
            WaitEnd::Stopped => Ok(AudioInterrupt::NONE),
            WaitEnd::Faulted(_) => {
                // The controller stopped the channel itself.
                self.clocking = false;
                self.parking = false;
                if let Some(stream) = self.stream.as_mut() {
                    stream.running = false;
                }
                Err(DriverError::DeviceFault)
            }
        }
    }

    fn boundary(&mut self, report: &WaitReport) -> Result<AudioInterrupt, DriverError> {
        if self.parking {
            self.halt()?;
            return Ok(AudioInterrupt::NONE);
        }
        self.reached = report.position;
        let Some(stream) = self.stream.as_mut().filter(|stream| stream.running) else {
            return Ok(AudioInterrupt::NONE);
        };
        let played = report.position / stream.period_bytes();
        stream.played = played;
        // The controller's monotonic reading, stamped the way the clock is.
        let at = Time64::UNIX_EPOCH.saturating_add(report.serviced);
        if let Some(silent_at) = stream.drained_at(played) {
            stream.rewind();
            stream.sampled = Some((silent_at, at));
            self.park()?;
        } else {
            stream.sampled = Some((stream.base + played * u64::from(stream.period_frames), at));
            self.post_boundary_wait()?;
        }
        Ok(AudioInterrupt {
            period_elapsed: 1,
            ..AudioInterrupt::NONE
        })
    }

    /// Take the serve loop's say over boundary waits: posted while it has
    /// somewhere to put frames.
    ///
    /// # Errors
    ///
    /// The transport's refusal of a wait.
    pub fn set_events(&mut self, enabled: bool) -> Result<(), DriverError> {
        self.events = enabled;
        if !self.is_running() {
            return Ok(());
        }
        if enabled {
            self.post_boundary_wait()
        } else {
            // With nowhere to put frames the buffer would replay what it last
            // held, a lap at a time.
            self.fill_parked();
            Ok(())
        }
    }
}

#[cfg(target_os = "none")]
mod link;
#[cfg(target_os = "none")]
pub use link::LinkDma;

#[cfg(any(test, feature = "mock-dma"))]
pub mod mock;

#[cfg(test)]
#[path = "cyclic_tests.rs"]
mod tests;
