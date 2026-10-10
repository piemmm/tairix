//! The audio class over the PCM block: its DMA channel, the bit clock it
//! drives or follows, and the codec on the far side of its link.
//!
//! A stream starts with transmit off and the FIFO cleared, so the first word
//! the DMA channel delivers is the first channel's; transmit goes on once the
//! channel runs. Where the codec drives the bit clock it comes up first, since
//! a clear completes only on a running bit clock. A stream ends parked:
//! silence reaches the block before its channel halts, and only then does
//! transmit go off and the codec down, so the last frames still play.
//!
//! The endpoint's format is the widest sample the codec takes that a FIFO word
//! carries exactly as a ring holds it, so a frame is copied as it is and any
//! narrowing is the mixer's, dithered.

use tairix_abi::driver::audio::{
    Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName, AudioServiced,
    ChannelMap, Frames, GainRange, JackState, Rate, RateSet, RateSupport, SampleFormat,
    SampleFormats, StreamDirection, MAX_DEVICE_RATES,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::driver::codec::SampleWidths;
use tairix_abi::{DriverError, Errno, RegisterBlock};
use tairix_audiochan::cyclic::{
    sole_endpoint, CyclicPlayback, DmaPort, FrameCodec, CHANNELS, MAX_PERIOD_FRAMES,
    MAX_RING_FRAMES,
};
use tairix_linkclient::{ClockClient, CodecClient, LinkCall};

use crate::pcm::{Framing, Pcm};

/// Fewest frames a period may hold: 2.7 ms at 48 kHz, two FIFOs' worth.
pub const MIN_PERIOD_FRAMES: u32 = 128;

/// The rates the block carries, as Linux's driver states them.
const BLOCK_MIN_HZ: u32 = 8_000;
const BLOCK_MAX_HZ: u32 = 384_000;

/// How far the bit clock may run from the rate asked: as near as a
/// converter's own crystal holds, which the mixer's clock fit follows.
const CLOCK_TOLERANCE_PPM: u64 = 100;

/// The block's FIFO words as a ring of the endpoint's format holds them: a
/// 32-bit or a 24-bit sample a word, right-justified, or two 16-bit samples a
/// word, the first channel's in the low half.
pub struct Slots {
    format: SampleFormat,
}

impl FrameCodec for Slots {
    fn format(&self) -> SampleFormat {
        self.format
    }

    fn encode(&mut self, _frame: &mut [u8]) {}

    fn silence(&mut self, frame: &mut [u8]) {
        frame.fill(0);
    }

    fn parked(&self, frame: &mut [u8]) {
        frame.fill(0);
    }

    fn reset(&mut self) {}
}

/// What the endpoint offers.
#[derive(Copy, Clone, Debug)]
struct Offer {
    rates: RateSupport,
    format: SampleFormat,
    width: u8,
    gain: Option<GainRange>,
}

/// The widest sample the codec takes whose FIFO word is a ring's own.
fn widest(widths: SampleWidths) -> Option<(SampleFormat, u8)> {
    [
        (SampleFormat::S32, 32),
        (SampleFormat::S24In32, 24),
        (SampleFormat::S16, 16),
    ]
    .into_iter()
    .find(|&(_, width)| widths.contains(width))
}

/// The codec's rates the block carries.
fn carried(codec: RateSupport) -> Option<RateSupport> {
    match codec {
        RateSupport::Continuous { min, max } => {
            let min = Rate::new(min.hz().max(BLOCK_MIN_HZ)).ok()?;
            let max = Rate::new(max.hz().min(BLOCK_MAX_HZ)).ok()?;
            (min <= max).then_some(RateSupport::Continuous { min, max })
        }
        RateSupport::Discrete(set) => {
            let mut kept = [Rate::HZ_48000; MAX_DEVICE_RATES];
            let mut count = 0;
            let within = set
                .rates()
                .iter()
                .filter(|rate| (BLOCK_MIN_HZ..=BLOCK_MAX_HZ).contains(&rate.hz()));
            for (slot, rate) in kept.iter_mut().zip(within) {
                *slot = *rate;
                count += 1;
            }
            RateSet::new(&kept[..count]).ok().map(RateSupport::Discrete)
        }
    }
}

/// Which parts of the output path are up.
#[derive(Copy, Clone, Debug, Default)]
struct Up {
    transmit: bool,
    codec: bool,
    clock: bool,
}

/// The refusal a clock controller's reason stands for.
fn clock_refusal(reason: Errno) -> DriverError {
    match reason {
        Errno::Busy => DriverError::Busy,
        Errno::NotSupported => DriverError::Unsupported,
        other => DriverError::from_errno(other),
    }
}

/// The audio class over the block.
pub struct Interface<'r, R, D, K, C>
where
    R: RegisterBlock + ?Sized,
    D: DmaPort,
    K: LinkCall,
    C: LinkCall,
{
    pcm: Pcm<'r, R>,
    playback: CyclicPlayback<D, Slots>,
    clock: Option<ClockClient<K>>,
    codec: CodecClient<C>,
    offer: Offer,
    configured: bool,
    up: Up,
}

impl<'r, R, D, K, C> Interface<'r, R, D, K, C>
where
    R: RegisterBlock + ?Sized,
    D: DmaPort,
    K: LinkCall,
    C: LinkCall,
{
    /// The interface over `pcm`, streaming through `dma`, composed with
    /// `codec`; `clock` runs the bit clock where this side drives it.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] for no clock where this side drives the
    ///   bit clock.
    /// * [`DriverError::Unsupported`] for a codec that does not take the
    ///   link's framing or clock sides, or takes no width or rate the block
    ///   carries.
    /// * The codec's refusal to describe itself, or a register access's
    ///   failure.
    pub fn new(
        mut pcm: Pcm<'r, R>,
        dma: D,
        clock: Option<ClockClient<K>>,
        mut codec: CodecClient<C>,
    ) -> Result<Self, DriverError> {
        let dai = *codec.dai();
        if !dai.codec_drives_bit_clock && clock.is_none() {
            return Err(DriverError::NotFound);
        }
        let facts = codec.describe()?;
        let codec_clocks = dai.codec_drives_bit_clock || dai.codec_drives_frame_clock;
        if !facts.formats.contains(dai.format) || (codec_clocks && !facts.drives_clocks) {
            return Err(DriverError::Unsupported);
        }
        let (format, width) = widest(facts.widths).ok_or(DriverError::Unsupported)?;
        let rates = carried(facts.rates).ok_or(DriverError::Unsupported)?;
        pcm.enable()?;
        Ok(Self {
            pcm,
            playback: CyclicPlayback::new(dma, Slots { format }),
            clock,
            codec,
            offer: Offer {
                rates,
                format,
                width,
                gain: facts.gain,
            },
            configured: false,
            up: Up::default(),
        })
    }

    /// The DMA channel the interface streams through.
    pub fn dma_mut(&mut self) -> &mut D {
        self.playback.dma_mut()
    }

    fn transmit(&mut self, on: bool) -> Result<(), DriverError> {
        if self.up.transmit != on {
            self.pcm.transmit(on)?;
            self.up.transmit = on;
        }
        Ok(())
    }

    fn start_codec(&mut self) -> Result<(), DriverError> {
        if !self.up.codec {
            self.codec.start()?;
            self.up.codec = true;
        }
        Ok(())
    }

    /// Take the codec's output down; it is taken to be down whether or not
    /// it answered, since nothing would bring it up again unasked.
    fn stop_codec(&mut self) -> Result<(), DriverError> {
        if !self.up.codec {
            return Ok(());
        }
        self.up.codec = false;
        self.codec.stop()
    }

    /// Once the channel has halted nothing more reaches the block, so the
    /// output goes down.
    fn settle(&mut self) -> Result<(), DriverError> {
        if self.playback.is_clocking() {
            return Ok(());
        }
        let codec = self.stop_codec();
        self.transmit(false)?;
        codec
    }

    /// Run the bit clock for `framing`'s frames at `rate`.
    fn run_clock(&mut self, rate: Rate, framing: &Framing) -> Result<(), DriverError> {
        let clock = self.clock.as_mut().ok_or(DriverError::NotFound)?;
        let wanted = u64::from(rate.hz()) * u64::from(framing.frame_bits());
        let made = clock.run(wanted).map_err(clock_refusal)?;
        self.up.clock = true;
        let off = u128::from(made.abs_diff(wanted)) * 1_000_000;
        if off > u128::from(wanted) * u128::from(CLOCK_TOLERANCE_PPM) {
            return Err(DriverError::Unsupported);
        }
        Ok(())
    }

    fn release_clock(&mut self) -> Result<(), DriverError> {
        if !self.up.clock {
            return Ok(());
        }
        self.up.clock = false;
        match self.clock.as_mut() {
            Some(clock) => clock.release().map_err(clock_refusal),
            None => Ok(()),
        }
    }

    /// Bring a configured stream up from position `at`.
    fn begin(&mut self, at: Frames) -> Result<(), DriverError> {
        let follows = self.codec.dai().codec_drives_bit_clock;
        if follows {
            self.start_codec()?;
        }
        self.transmit(false)?;
        self.pcm.clear()?;
        self.playback.start(at)?;
        self.transmit(true)?;
        if !follows {
            self.start_codec()?;
        }
        Ok(())
    }
}

impl<R, D, K, C> Audio for Interface<'_, R, D, K, C>
where
    R: RegisterBlock + ?Sized,
    D: DmaPort,
    K: LinkCall,
    C: LinkCall,
{
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        Ok(AudioDeviceFacts {
            endpoints: 1,
            name: AudioName::new("I2S").map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        sole_endpoint(endpoint)?;
        Ok(AudioEndpointFacts {
            index: 0,
            direction: StreamDirection::Playback,
            jack: JackState::Unknown,
            formats: SampleFormats::EMPTY.with(self.offer.format),
            channel_map: ChannelMap::STEREO,
            rates: self.offer.rates,
            min_period_frames: MIN_PERIOD_FRAMES,
            max_period_frames: MAX_PERIOD_FRAMES,
            max_ring_frames: MAX_RING_FRAMES,
            gain: self.offer.gain,
            name: AudioName::new("Line out").map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        sole_endpoint(endpoint)?;
        if params.channel_map.channels() != CHANNELS {
            return Err(DriverError::Unsupported);
        }
        if self.playback.is_running() {
            return Err(DriverError::Busy);
        }
        let dai = *self.codec.dai();
        let framing = Framing::new(&dai, self.offer.width)?;
        let rate = self.offer.rates.nearest(params.rate);
        let period_frames = params
            .period_frames
            .clamp(MIN_PERIOD_FRAMES, MAX_PERIOD_FRAMES);
        self.configured = false;
        // The block's control registers change only while it is idle.
        self.playback.halt_parking()?;
        self.stop_codec()?;
        self.transmit(false)?;
        if !dai.codec_drives_bit_clock {
            self.run_clock(rate, &framing)?;
        }
        self.codec.configure(rate, self.offer.width)?;
        self.pcm.frame(&framing)?;
        self.playback.configure(rate, period_frames)?;
        self.configured = true;
        Ok(ConfigureGrant {
            rate,
            format: self.offer.format,
            channel_map: ChannelMap::STEREO,
            period_frames,
            max_ring_frames: MAX_RING_FRAMES,
        })
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        if !self.configured {
            return Err(DriverError::DeviceFault);
        }
        if self.playback.is_running() {
            return Ok(());
        }
        self.playback.halt_parking()?;
        let begun = self.begin(at);
        if begun.is_err() {
            // Nothing half-started is left clocking.
            let _ = self.playback.stop(at);
            let _ = self.playback.halt_parking();
            let _ = self.settle();
        }
        begun
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        // Muted first, so the cut is silent.
        let muted = self.stop_codec();
        self.playback.stop(at)?;
        self.settle()?;
        muted
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.playback.drain()?;
        self.settle()
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        sole_endpoint(endpoint)?;
        let serviced = self.playback.service(ring)?;
        self.settle()?;
        Ok(serviced)
    }

    fn set_gain(&mut self, endpoint: u16, millibel: i32, mute: bool) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.codec.gain(millibel, mute).map(|_| ())
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.configured = false;
        let codec = self.stop_codec();
        if self.playback.is_running() {
            self.playback.stop(Frames::ZERO)?;
        }
        self.playback.halt_parking()?;
        self.playback.release()?;
        self.transmit(false)?;
        self.release_clock()?;
        codec
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        let interrupt = self.playback.take_interrupt();
        let settled = self.settle();
        let interrupt = interrupt?;
        settled.map(|()| interrupt)
    }

    fn set_event_interrupts(&mut self, enabled: bool) -> Result<(), DriverError> {
        self.playback.set_events(enabled)
    }
}

#[cfg(test)]
#[path = "interface_tests.rs"]
mod tests;
