//! The sound a recording hears — the microphone, the desktop's own sound, or
//! both — mixed on the output's clock and coded as Opus as it arrives.
//!
//! Every source is a capture stream of F32 frames at 48 kHz, each device
//! converted to it by the audio service. The desktop's sound, where it is
//! heard, is the clock the recording's sound keeps: each period of it is mixed
//! as soon as it has arrived. The microphone beside it runs on another
//! device's clock, so it is carried onto the desktop's by `lib/audio`'s one
//! resampler, at the ratio of the two devices' rates as the service measured
//! them, and summed by its one mixer. Until both rates are known the
//! microphone is silent, so it joins a fraction of a second in rather than
//! drifting. Each period mixed is coded as one Opus packet, timed from the
//! first frame heard, and kept in a batch for the encoding thread.

use alloc::vec::Vec;

use tairix_abi::audio::{CaptureSource, ClockReport};
use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat, MAX_CHANNELS, STANDARD_RATES};
use tairix_abi::recorder_ipc::RecordSound;
use tairix_abi::Errno;
use tairix_audio::clock::PLAUSIBLE_PPM;
use tairix_audio::convert;
use tairix_audio::resample::{MAX_PHASES, MAX_TAPS};
use tairix_audio::{ChannelMatrix, FilterBank, Mixer, Ratio, Resampler, SinkFormat, StreamMix};
use tairix_collections::ByteQueue;
use tairix_sound::opus_encode::{self, EncodeError, FRAME_SAMPLES, PRE_SKIP};
use tairix_util::fallible;
use tairix_video::webm::{OpusChannels, OpusTrack};

/// The rate every source is opened at: the one Opus codes.
pub const RATE: Rate = Rate::HZ_48000;

/// Frames one packet holds: an Opus frame, 20 ms.
pub const PERIOD: usize = FRAME_SAMPLES;

/// Packets a batch holds: ten seconds of sound, which no picture takes to
/// code. A packet that finds the batch full is lost, and the loss stated.
pub const BATCH_PACKETS: usize = 500;

/// Bytes one sample takes.
const SAMPLE_BYTES: usize = SampleFormat::F32.bytes_per_sample();

/// Frames a source holds before they are mixed: half a second, which a thread
/// woken by every period never comes near.
const FEED_FRAMES: usize = 24_000;

/// The bitrate a mono recording's sound is coded at.
const MONO_BITRATE: u32 = 64_000;

/// The bitrate a stereo recording's sound is coded at.
const STEREO_BITRATE: u32 = 128_000;

/// Frames each source is heard for before the microphone joins: half a
/// second, by which the service has measured both devices' rates.
const JOIN_AFTER_FRAMES: u64 = 24_000;

/// Frames after which the microphone joins on the nominal rates, where the
/// service has measured none it can believe: two seconds.
const JOIN_BY_FRAMES: u64 = 96_000;

/// Frames the microphone may run ahead of the mix before its oldest are let
/// go: two periods of what a residual drift gathers.
const SLACK_FRAMES: usize = 2 * PERIOD;

/// The denominator a drift between two clocks is stated over.
const MILLIONTHS: u32 = 1_000_000;

/// One capture stream, as the audio service serves it: F32 frames at
/// [`RATE`], in the layout it was opened in.
pub trait SoundSource {
    /// Set it moving.
    ///
    /// # Errors
    ///
    /// The service's refusal.
    fn start(&mut self) -> Result<(), Errno>;

    /// Move the frames that have arrived into `out`, as many whole ones as it
    /// holds, answering how many frames.
    ///
    /// # Errors
    ///
    /// What the ring refuses where the service has corrupted it.
    fn read(&mut self, out: &mut [u8]) -> Result<usize, Errno>;

    /// What the service has said of it since last asked.
    ///
    /// # Errors
    ///
    /// The refusal of a notification.
    fn take_news(&mut self) -> Result<News, Errno>;

    /// The device clock it is in the domain of.
    ///
    /// # Errors
    ///
    /// The service's refusal: [`Errno::WouldBlock`] before the device has
    /// reported a period.
    fn clock(&mut self) -> Result<ClockReport, Errno>;
}

/// What the service said of a stream.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct News {
    /// Frames it lost, heard as silence so the frames after them keep their
    /// time.
    pub lost_frames: u64,
    /// Whether it has gone: its device lost, or its ring faulted.
    pub gone: bool,
}

/// Opus packets waiting for the encoding thread, each with its time in the
/// recording, in one buffer whose room is committed when it is made.
#[derive(Debug, Default)]
pub struct PacketBatch {
    bytes: Vec<u8>,
    /// Each packet's time, and where its bytes end.
    ends: Vec<(u64, usize)>,
}

impl PacketBatch {
    /// A batch with room for `packets` packets of `packet_len` bytes; `None`
    /// where that room cannot be had.
    #[must_use]
    pub fn with_room(packets: usize, packet_len: usize) -> Option<Self> {
        let mut batch = Self::default();
        let bytes = packets.checked_mul(packet_len)?;
        let room = fallible::reserve(&mut batch.bytes, bytes)
            && fallible::reserve(&mut batch.ends, packets);
        room.then_some(batch)
    }

    /// Add `packet`, `at_ns` into the recording; `false`, and nothing added,
    /// once the batch is full. It never grows past the room it was made with.
    #[must_use]
    pub fn push(&mut self, at_ns: u64, packet: &[u8]) -> bool {
        let room = self.bytes.capacity() - self.bytes.len();
        if self.ends.len() == self.ends.capacity() || room < packet.len() {
            return false;
        }
        self.bytes.extend_from_slice(packet);
        self.ends.push((at_ns, self.bytes.len()));
        true
    }

    /// Each packet with its time, in the order added.
    pub fn packets(&self) -> impl Iterator<Item = (u64, &[u8])> + '_ {
        let mut start = 0;
        self.ends.iter().map(move |&(at_ns, end)| {
            let packet = self.bytes.get(start..end).unwrap_or_default();
            start = end;
            (at_ns, packet)
        })
    }

    /// How many packets it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether it holds none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Let every packet go, keeping the room.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.ends.clear();
    }
}

/// The layout a recording of `choice` is coded in, `None` for a silent one.
const fn layout_of(choice: RecordSound) -> Option<ChannelMap> {
    match choice {
        RecordSound::None => None,
        RecordSound::Microphone => Some(ChannelMap::MONO),
        RecordSound::Desktop | RecordSound::Both => Some(ChannelMap::STEREO),
    }
}

/// The bitrate sound of `channels` is coded at.
const fn bitrate(channels: u8) -> u32 {
    if channels == 1 {
        MONO_BITRATE
    } else {
        STEREO_BITRATE
    }
}

/// The Opus track a recording of `choice` carries, `None` for a silent one.
#[must_use]
pub fn track_of(choice: RecordSound) -> Option<OpusTrack> {
    let channels = match layout_of(choice)?.channels() {
        1 => OpusChannels::Mono,
        _ => OpusChannels::Stereo,
    };
    Some(OpusTrack {
        channels,
        pre_skip: PRE_SKIP,
        input_rate: RATE.hz(),
        output_gain: 0,
    })
}

/// One source, and the frames it delivered that are not yet mixed.
struct Feed<S> {
    source: S,
    frame_bytes: usize,
    queue: ByteQueue,
    /// Frames heard from the start, those lost among them.
    heard: u64,
    /// Frames lost that the queue had no room to hear, which the timeline
    /// passes over.
    passed: u64,
    gone: bool,
}

impl<S: SoundSource> Feed<S> {
    fn new(source: S, layout: &ChannelMap) -> Result<Self, Errno> {
        let frame_bytes = usize::from(layout.channels()) * SAMPLE_BYTES;
        let queue =
            ByteQueue::committed(FEED_FRAMES * frame_bytes).map_err(|_| Errno::OutOfMemory)?;
        Ok(Self {
            source,
            frame_bytes,
            queue,
            heard: 0,
            passed: 0,
            gone: false,
        })
    }

    /// Frames queued.
    fn frames(&self) -> usize {
        self.queue.len() / self.frame_bytes
    }

    /// Take what the service has said of the stream, then every frame that has
    /// arrived, as far as the queue has room.
    fn gather(&mut self) -> Result<(), Errno> {
        let news = self.source.take_news()?;
        self.gone |= news.gone;
        self.silence(news.lost_frames);
        loop {
            let room = self.queue.room() / self.frame_bytes * self.frame_bytes;
            if room == 0 || self.queue.reserve(room).is_err() {
                return Ok(());
            }
            let (source, frame_bytes) = (&mut self.source, self.frame_bytes);
            let read = self.queue.fill(|window| {
                let whole = window.len() / frame_bytes * frame_bytes;
                source
                    .read(&mut window[..whole])
                    .map(|frames| frames.saturating_mul(frame_bytes))
            })?;
            if read == 0 {
                return Ok(());
            }
            self.heard = self.heard.saturating_add((read / self.frame_bytes) as u64);
        }
    }

    /// Hear `frames` lost frames as silence, so the frames after them keep
    /// their time; those past the queue's room are passed over.
    fn silence(&mut self, frames: u64) {
        let room = self.queue.room() / self.frame_bytes;
        let fits = usize::try_from(frames).map_or(room, |frames| frames.min(room));
        if let Ok(slot) = self.queue.append_slot(fits * self.frame_bytes) {
            slot.fill(SampleFormat::F32.silence_byte());
        }
        self.passed = self.passed.saturating_add(frames - fits as u64);
        self.heard = self.heard.saturating_add(frames);
    }
}

/// The resampler carrying the microphone onto the desktop's clock.
struct Drift {
    bank: FilterBank,
    resampler: Resampler,
}

/// The microphone beside the desktop's sound.
struct Beside<S> {
    feed: Feed<S>,
    channels: usize,
    matrix: ChannelMatrix,
    /// `None` until it joins the mix.
    drift: Option<Drift>,
    /// What is offered the resampler: the queued frames as samples.
    input: Vec<f32>,
    /// One period on the desktop's clock.
    output: Vec<f32>,
    /// The same as F32 bytes, as the mixer takes it.
    bytes: Vec<u8>,
}

impl<S: SoundSource> Beside<S> {
    fn new(source: S, layout: &ChannelMap, mix: &ChannelMap) -> Result<Self, Errno> {
        let channels = usize::from(layout.channels());
        let samples = PERIOD * channels;
        let feed = Feed::new(source, layout)?;
        let matrix = ChannelMatrix::derive(layout, mix)?;
        let input = fallible::filled((PERIOD + SLACK_FRAMES) * channels, 0.0_f32)
            .ok_or(Errno::OutOfMemory)?;
        let output = fallible::filled(samples, 0.0_f32).ok_or(Errno::OutOfMemory)?;
        let bytes = fallible::zeroed(samples * SAMPLE_BYTES).ok_or(Errno::OutOfMemory)?;
        Ok(Self {
            feed,
            channels,
            matrix,
            drift: None,
            input,
            output,
            bytes,
        })
    }

    /// Join the mix at `ratio`: the microphone's frames to each of the clock's.
    fn join(&mut self, ratio: Ratio) -> Result<(), Errno> {
        let bank = FilterBank::for_ratio(ratio)?;
        let resampler = Resampler::new(&bank, self.channels)?;
        self.drift = Some(Drift { bank, resampler });
        Ok(())
    }

    /// Bring one period of the microphone onto the clock's timeline: silence
    /// until it joins, once it has gone, and wherever it ran short.
    fn period(&mut self) -> Result<(), Errno> {
        let frame_bytes = self.feed.frame_bytes;
        let output = &mut self.output[..];
        output.fill(0.0);
        if !self.feed.gone {
            if let Some(drift) = self.drift.as_mut() {
                let offered = self.feed.frames().min(self.input.len() / self.channels);
                let pending = self.feed.queue.pending();
                let held = pending.get(..offered * frame_bytes).unwrap_or_default();
                let decoded = convert::decode(SampleFormat::F32, held, &mut self.input);
                let input = self.input.get(..decoded).unwrap_or_default();
                let (consumed, _) = drift.resampler.process(&drift.bank, input, output)?;
                self.feed.queue.consume(consumed * frame_bytes);
            }
        }
        // Before it joins, what it heard is let go, so it joins on what it
        // hears next rather than on what it heard while the clock was unknown.
        let keep = if self.drift.is_some() {
            SLACK_FRAMES
        } else {
            0
        };
        let excess = self.feed.frames().saturating_sub(keep);
        self.feed.queue.consume(excess * frame_bytes);
        convert::encode(SampleFormat::F32, output, &mut self.bytes);
        Ok(())
    }

    /// This period's part of the mix.
    fn mix(&self) -> StreamMix<'_> {
        StreamMix {
            format: SampleFormat::F32,
            matrix: &self.matrix,
            gain: 1.0,
            resampled: self
                .drift
                .as_ref()
                .is_some_and(|drift| !drift.bank.is_unity()),
            samples: &self.bytes,
        }
    }
}

/// A recording's sound: its sources, the mix, the coder, and the packets
/// waiting for the encoding thread.
pub struct Sound<S> {
    /// The source whose clock the sound keeps: the desktop's where it is
    /// heard, else the microphone.
    clock: Feed<S>,
    /// The microphone beside the desktop's sound.
    beside: Option<Beside<S>>,
    clock_matrix: ChannelMatrix,
    mixer: Mixer,
    coder: opus_encode::Encoder,
    /// One period as the mixer wrote it.
    mixed: Vec<u8>,
    /// The same, as the coder takes it.
    pcm: Vec<f32>,
    packet: Vec<u8>,
    /// Frames of the sound coded or passed over, from its first.
    timeline: u64,
    /// When its first frame was heard, from the recording's start.
    origin_ns: u64,
    filling: PacketBatch,
    spare: Option<PacketBatch>,
    /// Packets lost to a full batch since last asked.
    lost: u64,
}

impl<S: SoundSource> Sound<S> {
    /// The sound `choice` hears, each source opened by `open` in the layout it
    /// names; `None` for a silent recording.
    ///
    /// # Errors
    ///
    /// The refusal of a source, or [`Errno::OutOfMemory`].
    pub fn open(
        choice: RecordSound,
        mut open: impl FnMut(CaptureSource, ChannelMap) -> Result<S, Errno>,
    ) -> Result<Option<Self>, Errno> {
        let Some(layout) = layout_of(choice) else {
            return Ok(None);
        };
        let (source, clock_layout) = if choice.desktop() {
            (CaptureSource::Monitor(0), ChannelMap::STEREO)
        } else {
            (CaptureSource::Input(0), ChannelMap::MONO)
        };
        let clock = Feed::new(open(source, clock_layout)?, &clock_layout)?;
        let beside = if choice.desktop() && choice.microphone() {
            let microphone = open(CaptureSource::Input(0), ChannelMap::MONO)?;
            Some(Beside::new(microphone, &ChannelMap::MONO, &layout)?)
        } else {
            None
        };
        Self::mixing(clock, &clock_layout, beside, &layout).map(Some)
    }

    /// The sound of `clock`, and of what is `beside` it, mixed into `layout`.
    fn mixing(
        clock: Feed<S>,
        clock_layout: &ChannelMap,
        beside: Option<Beside<S>>,
        layout: &ChannelMap,
    ) -> Result<Self, Errno> {
        let sink = SinkFormat {
            format: SampleFormat::F32,
            resolution: SampleFormat::F32.valid_bits(),
            rate: RATE,
            channel_map: *layout,
        };
        let samples = PERIOD * usize::from(layout.channels());
        let clock_matrix = ChannelMatrix::derive(clock_layout, layout)?;
        let mixer = Mixer::new(sink, PERIOD, 0)?;
        let coder = opus_encode::Encoder::new(layout.channels(), bitrate(layout.channels()))
            .map_err(coder_errno)?;
        let packet_len = coder.packet_len();
        let mixed_bytes = fallible::zeroed(samples * SAMPLE_BYTES).ok_or(Errno::OutOfMemory)?;
        let pcm = fallible::filled(samples, 0.0_f32).ok_or(Errno::OutOfMemory)?;
        let packet = fallible::zeroed(packet_len).ok_or(Errno::OutOfMemory)?;
        let batch = || PacketBatch::with_room(BATCH_PACKETS, packet_len).ok_or(Errno::OutOfMemory);
        let filling = batch()?;
        let spare = Some(batch()?);
        Ok(Self {
            clock,
            beside,
            clock_matrix,
            mixer,
            coder,
            mixed: mixed_bytes,
            pcm,
            packet,
            timeline: 0,
            origin_ns: 0,
            filling,
            spare,
            lost: 0,
        })
    }

    /// Set the sources moving, the first frame heard at `now_ns`, the
    /// recording having begun at `started_ns`.
    ///
    /// # Errors
    ///
    /// A source's refusal.
    pub fn start(&mut self, started_ns: u64, now_ns: u64) -> Result<(), Errno> {
        self.clock.source.start()?;
        if let Some(beside) = self.beside.as_mut() {
            beside.feed.source.start()?;
        }
        self.origin_ns = now_ns.saturating_sub(started_ns);
        Ok(())
    }

    /// Take what each source has said and heard, and code every whole period
    /// the clock now holds.
    ///
    /// # Errors
    ///
    /// The failure of the clock's source, which ends the sound, or of the
    /// mixer or the coder. A microphone that fails beside the desktop's sound
    /// is heard as silence from then on.
    pub fn hear(&mut self) -> Result<(), Errno> {
        self.clock.gather()?;
        if self.clock.gone {
            return Err(Errno::DeviceOffline);
        }
        if let Some(beside) = self.beside.as_mut() {
            if beside.feed.gather().is_err() {
                beside.feed.gone = true;
            }
        }
        self.code()
    }

    /// Code what is left: the part-period the clock holds, padded with
    /// silence, then a period of silence the coder's delay plays out in.
    ///
    /// # Errors
    ///
    /// As [`Self::hear`]; what was coded before stays in the batch.
    pub fn finish(&mut self) -> Result<(), Errno> {
        let heard = self.hear();
        let pad = (PERIOD - self.clock.frames() % PERIOD) % PERIOD + PERIOD;
        if let Ok(slot) = self.clock.queue.append_slot(pad * self.clock.frame_bytes) {
            slot.fill(SampleFormat::F32.silence_byte());
        }
        self.code()?;
        heard
    }

    /// The packets waiting for the encoding thread, the spare batch taking
    /// their place; `None` while none waits or the spare is out.
    pub fn take_batch(&mut self) -> Option<PacketBatch> {
        if self.filling.is_empty() {
            return None;
        }
        let spare = self.spare.take()?;
        Some(core::mem::replace(&mut self.filling, spare))
    }

    /// Have back a batch the encoding thread has written.
    pub fn give_back(&mut self, mut batch: PacketBatch) {
        batch.clear();
        self.spare = Some(batch);
    }

    /// Packets lost to a full batch since last asked.
    pub fn take_lost(&mut self) -> u64 {
        core::mem::take(&mut self.lost)
    }

    /// The last period mixed, as the coder took it.
    #[cfg(test)]
    pub(crate) fn last_mix(&self) -> &[f32] {
        &self.pcm
    }

    /// Mix and code every whole period the clock holds.
    fn code(&mut self) -> Result<(), Errno> {
        let period_bytes = PERIOD * self.clock.frame_bytes;
        while self.clock.queue.len() >= period_bytes {
            self.join();
            if let Some(beside) = self.beside.as_mut() {
                beside.period()?;
            }
            let clock = StreamMix {
                format: SampleFormat::F32,
                matrix: &self.clock_matrix,
                gain: 1.0,
                resampled: false,
                samples: self
                    .clock
                    .queue
                    .pending()
                    .get(..period_bytes)
                    .unwrap_or_default(),
            };
            let beside = self.beside.as_ref().map(Beside::mix);
            let streams = core::iter::once(clock).chain(beside);
            let written = self.mixer.mix(streams, PERIOD, &mut self.mixed)?;
            self.clock.queue.consume(period_bytes);
            self.emit(written)?;
        }
        Ok(())
    }

    /// Bring the microphone into the mix once both rates are known: at their
    /// ratio as the service measured them, or on the nominal rates where it
    /// has measured none it can believe in time.
    fn join(&mut self) {
        let Some(beside) = self.beside.as_mut() else {
            return;
        };
        let heard = beside.feed.heard.min(self.clock.heard);
        if beside.drift.is_some() || beside.feed.gone || heard < JOIN_AFTER_FRAMES {
            return;
        }
        let measured = match (beside.feed.source.clock(), self.clock.source.clock()) {
            (Ok(microphone), Ok(output)) => drift_between(&microphone, &output),
            _ => None,
        };
        let ratio = match measured {
            Some(ratio) => ratio,
            None if heard >= JOIN_BY_FRAMES => Ratio::between(RATE, RATE),
            None => return,
        };
        // A microphone that cannot be carried onto the clock is not heard.
        if beside.join(ratio).is_err() {
            beside.feed.gone = true;
        }
    }

    /// Code the period the mixer wrote as one packet, timed on the recording's
    /// timeline.
    fn emit(&mut self, written: usize) -> Result<(), Errno> {
        let mixed = self.mixed.get(..written).unwrap_or_default();
        let samples = convert::decode(SampleFormat::F32, mixed, &mut self.pcm);
        let pcm = self.pcm.get(..samples).unwrap_or_default();
        let len = self
            .coder
            .encode(pcm, &mut self.packet)
            .map_err(coder_errno)?;
        self.timeline = self
            .timeline
            .saturating_add(core::mem::take(&mut self.clock.passed));
        let at_ns = self.origin_ns.saturating_add(frames_ns(self.timeline));
        if !self
            .filling
            .push(at_ns, self.packet.get(..len).unwrap_or_default())
        {
            self.lost = self.lost.saturating_add(1);
        }
        self.timeline = self.timeline.saturating_add(PERIOD as u64);
        Ok(())
    }
}

/// The microphone's frames to each of the output's, from the rates the
/// service measured their devices at against the standard rates nearest
/// them; `None` where that ratio is further from one than any clock drifts.
fn drift_between(microphone: &ClockReport, output: &ClockReport) -> Option<Ratio> {
    let ahead = u128::from(microphone.rate_millihertz)
        * u128::from(nominal_millihertz(output.rate_millihertz));
    let behind = u128::from(nominal_millihertz(microphone.rate_millihertz))
        * u128::from(output.rate_millihertz);
    let millionths = (ahead * u128::from(MILLIONTHS) + behind / 2).checked_div(behind)?;
    if millionths.abs_diff(u128::from(MILLIONTHS)) > u128::from(PLAUSIBLE_PPM) {
        return None;
    }
    Ratio::new(u32::try_from(millionths).ok()?, MILLIONTHS)
}

/// The standard rate nearest `measured`, in thousandths of a hertz: what a
/// device the service clocks is configured at.
fn nominal_millihertz(measured: u32) -> u32 {
    STANDARD_RATES
        .iter()
        .map(|rate| rate.hz().saturating_mul(1_000))
        .min_by_key(|nominal| nominal.abs_diff(measured))
        .unwrap_or(measured)
}

/// `frames` at [`RATE`], in nanoseconds.
fn frames_ns(frames: u64) -> u64 {
    let nanos = u128::from(frames) * 1_000_000_000 / u128::from(RATE.hz());
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

/// The coder's refusal, as an error.
const fn coder_errno(error: EncodeError) -> Errno {
    match error {
        EncodeError::Unsupported => Errno::NotSupported,
        EncodeError::BadSamples => Errno::OutOfRange,
        EncodeError::BufferTooSmall => Errno::BufferTooSmall,
        EncodeError::OutOfMemory => Errno::OutOfMemory,
        EncodeError::Overrun => Errno::DeviceFault,
    }
}

/// The bytes the sound of `choice` commits as it opens: its queues, the
/// mixer's and the coder's scratch, its two batches and, beside the desktop's
/// sound, the microphone's resampler at its largest.
#[must_use]
pub fn footprint(choice: RecordSound) -> u64 {
    let Some(layout) = layout_of(choice) else {
        return 0;
    };
    let channels = u64::from(layout.channels());
    let (period, sample) = (PERIOD as u64, SAMPLE_BYTES as u64);
    let clock_channels = if choice.desktop() { 2 } else { 1 };
    let packet = packet_bytes(layout.channels());
    let index = core::mem::size_of::<(u64, usize)>() as u64;
    let mut total = FEED_FRAMES as u64 * clock_channels * sample
        + period * (MAX_CHANNELS as u64 + 4 * channels) * sample
        + 2 * BATCH_PACKETS as u64 * (packet + index)
        + packet;
    if choice.desktop() && choice.microphone() {
        let bank = (u64::from(MAX_PHASES) + 1) * MAX_TAPS as u64 * sample;
        let resampler = 2 * MAX_TAPS as u64 * sample;
        let scratch = (3 * period + SLACK_FRAMES as u64) * sample;
        total += FEED_FRAMES as u64 * sample + scratch + bank + resampler;
    }
    total
}

/// The bytes one packet of `channels` takes, as the coder rounds its bitrate
/// down to whole bytes.
fn packet_bytes(channels: u8) -> u64 {
    u64::from(bitrate(channels)) * PERIOD as u64 / u64::from(RATE.hz()) / 8
}

#[cfg(test)]
#[path = "sound_tests.rs"]
mod tests;
