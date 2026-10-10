//! The jack: the audio class over the PWM's two channels, a cyclic DMA stream
//! ([`tairix_audiochan::cyclic`]) whose frames are the duty words the shaper
//! writes.
//!
//! Between streams the stream parks on silence, the middle duty, and the PWM
//! repeats its last word once its FIFO runs dry, so the jack holds silence
//! with nothing running.

use tairix_abi::driver::audio::{
    Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName, AudioServiced,
    ChannelMap, Frames, JackState, Rate, RateSet, RateSupport, SampleFormat, SampleFormats,
    StreamDirection,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::DriverError;
use tairix_audiochan::cyclic::{
    sole_endpoint, CyclicPlayback, DmaPort, FrameCodec, MAX_PERIOD_FRAMES, MAX_RING_FRAMES,
};

use crate::shaper::NoiseShaper;

/// Fewest frames a period may hold: 1.4 ms at the jack's rate, which bounds
/// the boundaries a second the DMA controller answers.
pub const MIN_PERIOD_FRAMES: u32 = 512;

/// Frames the bring-up ramp takes from the idle low to silence: some 44 ms at
/// the jack's rate, slow enough to pass beneath hearing.
pub const RAMP_FRAMES: u32 = 16_384;

/// The ring channel each FIFO word carries: the first PWM channel drives the
/// jack's right side.
const FIFO_ORDER: [usize; 2] = [1, 0];

/// Write one frame's two duty words, in the FIFO's channel order, where a
/// ring frame of two signed 32-bit samples lay.
fn put_duties(frame: &mut [u8], duties: [u32; 2]) {
    for (word, duty) in frame.as_chunks_mut::<4>().0.iter_mut().zip(duties) {
        *word = duty.to_le_bytes();
    }
}

/// The jack's frames: duty words, shaped, one shaper per FIFO channel.
pub struct Duties {
    shapers: [NoiseShaper; 2],
}

impl FrameCodec for Duties {
    fn format(&self) -> SampleFormat {
        SampleFormat::S32
    }

    fn encode(&mut self, frame: &mut [u8]) {
        let (words, _) = frame.as_chunks::<4>();
        let &[left, right] = words else {
            return;
        };
        let samples = [i32::from_le_bytes(left), i32::from_le_bytes(right)];
        let duties = [
            self.shapers[0].duty(samples[FIFO_ORDER[0]]),
            self.shapers[1].duty(samples[FIFO_ORDER[1]]),
        ];
        put_duties(frame, duties);
    }

    fn silence(&mut self, frame: &mut [u8]) {
        let duties = [self.shapers[0].duty(0), self.shapers[1].duty(0)];
        put_duties(frame, duties);
    }

    fn parked(&self, frame: &mut [u8]) {
        let silence = self.shapers[0].silence();
        put_duties(frame, [silence, silence]);
    }

    fn reset(&mut self) {
        for shaper in &mut self.shapers {
            shaper.reset();
        }
    }
}

/// The 3.5 mm jack's PWM output.
pub struct Jack<D: DmaPort> {
    playback: CyclicPlayback<D, Duties>,
    rate: Rate,
    silence: u32,
}

impl<D: DmaPort> Jack<D> {
    /// The jack over `dma` at `rate`, each period `levels` clock cycles,
    /// its dither fixed by `seed`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for too few levels to shape onto.
    pub fn new(dma: D, rate: Rate, levels: u32, seed: u64) -> Result<Self, DriverError> {
        let shaper = |seed| NoiseShaper::new(levels, seed).ok_or(DriverError::OutOfRange);
        let duties = Duties {
            shapers: [shaper(seed)?, shaper(seed ^ 0x9E37_79B9_7F4A_7C15)?],
        };
        let silence = duties.shapers[0].silence();
        Ok(Self {
            playback: CyclicPlayback::new(dma, duties),
            rate,
            silence,
        })
    }

    /// Ramp the jack from the PWM's idle low to silence, where it stays
    /// between streams.
    ///
    /// # Errors
    ///
    /// The DMA channel's refusal.
    pub fn bring_up(&mut self) -> Result<(), DriverError> {
        let silence = self.silence;
        self.playback.play_once(RAMP_FRAMES, |frame, words| {
            let duty = ramp_duty(silence, frame);
            put_duties(words, [duty, duty]);
        })
    }
}

/// The duty of the bring-up ramp's `frame`: the quintic smoothstep
/// `t³(6t² - 15t + 10)` from zero to `silence`, which starts and ends with
/// neither slope nor curvature, in exact integer arithmetic.
fn ramp_duty(silence: u32, frame: u32) -> u32 {
    let t = u128::from(frame.min(RAMP_FRAMES));
    let n = u128::from(RAMP_FRAMES);
    let rise = t * t * t * (6 * t * t + 10 * n * n - 15 * t * n);
    let duty = u128::from(silence) * rise / n.pow(5);
    u32::try_from(duty).unwrap_or(silence)
}

impl<D: DmaPort> Audio for Jack<D> {
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        Ok(AudioDeviceFacts {
            endpoints: 1,
            name: AudioName::new("Headphone jack").map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        sole_endpoint(endpoint)?;
        Ok(AudioEndpointFacts {
            index: 0,
            direction: StreamDirection::Playback,
            jack: JackState::Unknown,
            formats: SampleFormats::EMPTY.with(SampleFormat::S32),
            channel_map: ChannelMap::STEREO,
            rates: RateSupport::Discrete(
                RateSet::new(&[self.rate]).map_err(|_| DriverError::DeviceFault)?,
            ),
            min_period_frames: MIN_PERIOD_FRAMES,
            max_period_frames: MAX_PERIOD_FRAMES,
            max_ring_frames: MAX_RING_FRAMES,
            gain: None,
            name: AudioName::new("Headphones").map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        sole_endpoint(endpoint)?;
        if params.channel_map.channels() != 2 {
            return Err(DriverError::Unsupported);
        }
        let period_frames = params
            .period_frames
            .clamp(MIN_PERIOD_FRAMES, MAX_PERIOD_FRAMES);
        self.playback.configure(self.rate, period_frames)?;
        Ok(ConfigureGrant {
            rate: self.rate,
            format: SampleFormat::S32,
            channel_map: ChannelMap::STEREO,
            period_frames,
            max_ring_frames: MAX_RING_FRAMES,
        })
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.playback.start(at)
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.playback.stop(at)
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.playback.drain()
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        sole_endpoint(endpoint)?;
        self.playback.service(ring)
    }

    fn set_gain(&mut self, endpoint: u16, _millibel: i32, _mute: bool) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        // The PWM has no gain stage of its own, which is how the mixer learns
        // to apply the gain itself.
        Err(DriverError::NotImplemented)
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.playback.release()
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        self.playback.take_interrupt()
    }

    fn set_event_interrupts(&mut self, enabled: bool) -> Result<(), DriverError> {
        self.playback.set_events(enabled)
    }
}

#[cfg(test)]
#[path = "jack_tests.rs"]
mod tests;
