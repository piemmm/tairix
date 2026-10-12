//! The transmitter's audio endpoint: linear PCM from a cyclic DMA buffer into
//! the MAI, framed as IEC 60958 subframes, its clock regenerated at the sink
//! from N and CTS, the audio `InfoFrame` placing its channels — as Linux's
//! `vc4_hdmi` audio prepares a BCM2711 block — and its facts following the
//! sink on the connector.
//!
//! The MAI runs on the audio clock whether or not the link sends, so a stream
//! outlives an output brought down: when the link is up again its N and CTS
//! follow the new TMDS clock and the `InfoFrame` is sent again.

use tairix_abi::driver::audio::{
    Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName, AudioServiced,
    ChannelMap, Frames, JackState, Rate, RateSet, RateSupport, SampleFormat, SampleFormats,
    StreamDirection, MAX_DEVICE_RATES,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::driver::clock;
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::{Delay, DriverError};
use tairix_audiochan::cyclic::{
    sole_endpoint, CyclicPlayback, DmaPort, MAX_PERIOD_FRAMES, MAX_RING_FRAMES,
};
use tairix_drvrt::Recorder;
use tairix_edid::{
    allocate, widest_layout, Allocation, AudioFormat, AudioInfo, Edid, SampleRates, Speakers, SLOTS,
};
use tairix_linkclient::{ClockClient, LinkCall};

use crate::hdmi::{Hdmi, Record};
use crate::iec958::{Framing, Iec958, PREAMBLE_BLOCK};
use crate::mode::Chosen;
use crate::regs::{core, field, hd, phy};
use crate::sink::Ddc;

/// Fewest frames a period may hold: 5.3 ms at 48 kHz.
pub(crate) const MIN_PERIOD_FRAMES: u32 = 256;

/// The rates the MAI carries and their codes in its format register.
const MAI_RATES: [(u32, u32); 7] = [
    (32_000, 7),
    (44_100, 8),
    (48_000, 9),
    (88_200, 11),
    (96_000, 12),
    (176_400, 14),
    (192_000, 15),
];

/// The MAI's FIFO thresholds on the BCM2711: its panic and DMA request
/// levels, as Linux sets them for a fifth-generation block.
const MAI_THRESHOLDS: u32 = (0x10 << 24) | (0x10 << 16) | (0x1C << 8) | 0x1C;

/// The largest numerator and denominator the MAI's sample clock divides by.
const SMP_N_MAX: u64 = (1 << 24) - 1;
const SMP_M_MAX: u64 = 256;

/// What a sink plays a channel count at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Support {
    rates: SampleRates,
    /// The widest sample, in bits.
    word_bits: u8,
}

impl Support {
    const NONE: Self = Self {
        rates: SampleRates::NONE,
        word_bits: 0,
    };
    const BASIC: Self = Self {
        rates: SampleRates::BASIC,
        word_bits: 16,
    };

    fn with(self, other: Self) -> Self {
        Self {
            rates: self.rates.union(other.rates),
            word_bits: self.word_bits.max(other.word_bits),
        }
    }

    /// The rates as an endpoint offers them, or `None` for none.
    fn rates(self) -> Option<RateSupport> {
        let mut kept = [Rate::HZ_48000; MAX_DEVICE_RATES];
        let mut count = 0;
        for (slot, hz) in kept.iter_mut().zip(self.rates.iter()) {
            *slot = Rate::new(hz).ok()?;
            count += 1;
        }
        RateSet::new(&kept[..count]).ok().map(RateSupport::Discrete)
    }
}

/// What the sink on the connector plays, which the endpoint offers.
#[derive(Copy, Clone, Debug)]
struct Offer {
    /// What it plays each channel count at, from one.
    support: [Support; SLOTS],
    speakers: Option<Speakers>,
    /// The widest layout it places.
    layout: ChannelMap,
}

impl Offer {
    /// A sink's basic audio alone.
    const BASIC: Self = Self {
        support: [
            Support::BASIC,
            Support::BASIC,
            Support::NONE,
            Support::NONE,
            Support::NONE,
            Support::NONE,
            Support::NONE,
            Support::NONE,
        ],
        speakers: None,
        layout: ChannelMap::STEREO,
    };

    /// What `edid`'s basic audio and linear PCM descriptors offer, a
    /// descriptor covering every count up to its own as CTA-861 has it — or
    /// `None` for a sink playing no layout the MAI frames.
    fn of(edid: &Edid) -> Option<Self> {
        let mut support = [Support::NONE; SLOTS];
        if edid.audio.basic {
            support[..2].fill(Support::BASIC);
        }
        let lpcm = edid
            .audio
            .descriptors
            .iter()
            .filter(|sad| sad.format == AudioFormat::Lpcm);
        for sad in lpcm {
            let carried = Support {
                rates: sad.rates,
                word_bits: sad
                    .lpcm_sizes()
                    .and_then(|sizes| sizes.iter().max())
                    .unwrap_or(16),
            };
            for count in support.iter_mut().take(usize::from(sad.channels)) {
                *count = count.with(carried);
            }
        }
        let widest = support.iter().rposition(|count| !count.rates.is_empty())?;
        let layout = widest_layout(edid.audio.speakers, u8::try_from(widest + 1).ok()?);
        let offer = Self {
            support,
            speakers: edid.audio.speakers,
            layout,
        };
        (!offer.at(layout.channels()).rates.is_empty()).then_some(offer)
    }

    /// What the sink plays `channels` channels at.
    fn at(&self, channels: u8) -> Support {
        usize::from(channels)
            .checked_sub(1)
            .and_then(|at| self.support.get(at))
            .copied()
            .unwrap_or(Support::NONE)
    }
}

/// A configured stream.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Setup {
    rate: Rate,
    allocation: Allocation,
}

/// The audio endpoint's half of the transmitter.
pub(crate) struct AudioPath<M: DmaPort, C: LinkCall> {
    playback: CyclicPlayback<M, Iec958>,
    clock: ClockClient<C>,
    clock_held: bool,
    pub(crate) setup: Option<Setup>,
    offer: Offer,
    /// Whether the sink on the connector plays audio.
    present: bool,
    /// What the mixer was last told of the connector.
    reported: JackState,
}

impl<M: DmaPort, C: LinkCall> AudioPath<M, C> {
    pub(crate) const fn new(clock: ClockClient<C>, dma: M) -> Self {
        Self {
            playback: CyclicPlayback::new(dma, Iec958::new()),
            clock,
            clock_held: false,
            setup: None,
            offer: Offer::BASIC,
            present: false,
            reported: JackState::Absent,
        }
    }

    /// Follow the connector: a connected HDMI sink's description is what the
    /// endpoint offers from now on; a sink leaving takes nothing away, so a
    /// stream its successor still plays need not be set up again.
    pub(crate) fn follow(&mut self, connected: bool, edid: Option<&Edid>) {
        let offer = edid
            .filter(|edid| connected && edid.hdmi.is_some())
            .and_then(Offer::of);
        if let Some(offer) = offer {
            self.offer = offer;
        }
        self.present = offer.is_some();
    }

    const fn jack(&self) -> JackState {
        if self.present {
            JackState::Present
        } else {
            JackState::Absent
        }
    }
}

/// N and CTS for audio at `rate` over a link of `clock_khz`: N the HDMI
/// specification's recommended value — 128 per kilohertz of a 32 or 48 kHz
/// family rate, 6272 per 44.1 kHz — and CTS the nearest whole count of TMDS
/// clocks in N regenerated audio clocks: exactly the clock in kilohertz for
/// the first family, the clock over 900 Hz for the second.
pub(crate) fn n_cts(clock_khz: u32, rate: Rate) -> Option<(u32, u32)> {
    let hz = rate.hz();
    let n = if hz.is_multiple_of(44_100) {
        6_272 * (hz / 44_100)
    } else if hz.is_multiple_of(8_000) {
        128 * (hz / 1_000)
    } else {
        return None;
    };
    let regenerated = 128 * u64::from(hz);
    let cts = (u64::from(clock_khz) * 1_000 * u64::from(n) + regenerated / 2) / regenerated;
    let cts = u32::try_from(cts)
        .ok()
        .filter(|&cts| cts < 1 << 20 && cts != 0)?;
    Some((n, cts))
}

/// The best fraction of `numerator` over `denominator` within the MAI's
/// bounds, as Linux's `rational_best_approximation` finds it: a convergent of
/// the continued fraction, or the semiconvergent nearer than the last.
pub(crate) fn sample_divider(numerator: u64, denominator: u64) -> Option<(u64, u64)> {
    let (mut n, mut d) = (numerator, denominator);
    let (mut n0, mut d0, mut n1, mut d1) = (0u64, 1u64, 1u64, 0u64);
    while d != 0 {
        let a = n / d;
        let previous = d;
        d = n % d;
        n = previous;
        let n2 = n0.checked_add(a.checked_mul(n1)?)?;
        let d2 = d0.checked_add(a.checked_mul(d1)?)?;
        if n2 > SMP_N_MAX || d2 > SMP_M_MAX {
            let t = [
                (SMP_M_MAX - d0).checked_div(d1),
                (SMP_N_MAX - n0).checked_div(n1),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(u64::MAX);
            if d1 == 0 || 2 * t > a || (2 * t == a && d0 * previous > d1 * d) {
                n1 = n0 + t * n1;
                d1 = d0 + t * d1;
            }
            break;
        }
        (n0, d0, n1, d1) = (n1, d1, n2, d2);
    }
    (n1 != 0 && d1 != 0).then_some((n1, d1))
}

impl<R, C, P, D, L, M> Hdmi<R, C, P, D, L, M>
where
    R: RegisterBlock,
    C: LinkCall,
    P: Ddc,
    D: Delay,
    L: Recorder<Record>,
    M: DmaPort,
{
    /// The audio path, for a transmitter that carries sound.
    fn path(&self) -> Result<&AudioPath<M, C>, DriverError> {
        self.audio.as_ref().ok_or(DriverError::NotFound)
    }

    /// [`Self::path`], to change.
    fn path_mut(&mut self) -> Result<&mut AudioPath<M, C>, DriverError> {
        self.audio.as_mut().ok_or(DriverError::NotFound)
    }

    /// Send the configured stream's N, CTS and `InfoFrame` over the link
    /// `chosen` runs, now that it sends.
    pub(crate) fn audio_link_up(&self, chosen: &Chosen) -> Result<(), DriverError> {
        let Some(setup) = self.audio.as_ref().and_then(|audio| audio.setup) else {
            return Ok(());
        };
        let (n, cts) = n_cts(chosen.timing.clock_khz, setup.rate).ok_or(DriverError::OutOfRange)?;
        let core_regs = &self.regs.core;
        core_regs.write32(core::CRP_CFG, core::CRP_EXTERNAL_CTS | field(n, 0, 20))?;
        core_regs.write32(core::CTS_0, cts)?;
        core_regs.write32(core::CTS_1, cts)?;
        self.rng(true)?;
        self.send_packet(
            &AudioInfo {
                channels: setup.allocation.channels,
                allocation: setup.allocation.code,
            }
            .pack(),
        )
    }

    /// Power the PHY's random number generator the audio packets draw on.
    fn rng(&self, on: bool) -> Result<(), DriverError> {
        let (clear, set) = if on {
            (phy::POWERDOWN_RNDGEN, 0)
        } else {
            (0, phy::POWERDOWN_RNDGEN)
        };
        let value = self.regs.phy.read32(phy::POWERDOWN_CTL)?;
        self.regs
            .phy
            .write32(phy::POWERDOWN_CTL, value & !clear | set)
    }

    fn mai(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        self.regs.hd.write32(self.variant.mai + offset, value)
    }

    /// Hold the clocks the MAI runs on: the state machine clock for its
    /// registers, its own sample clock opened at its rate.
    fn hold_audio_clocks(&mut self) -> Result<u64, DriverError> {
        self.holds.audio = true;
        self.settle_hsm()?;
        let audio = self.path_mut()?;
        let rate = audio.clock.describe().map_err(clock::refusal)?.max_hz;
        let made = audio.clock.floor(rate).map_err(clock::refusal)?;
        audio.clock_held = true;
        Ok(made)
    }

    fn release_audio_clocks(&mut self) -> Result<(), DriverError> {
        let gate = match self.audio.as_mut() {
            Some(audio) if audio.clock_held => {
                audio.clock_held = false;
                audio.clock.release().map_err(clock::refusal)
            }
            _ => Ok(()),
        };
        self.holds.audio = false;
        let hsm = self.settle_hsm();
        gate.and(hsm)
    }

    /// Set the MAI up for `setup`, its sample clock divided from
    /// `audio_hz`, as Linux's `vc4_hdmi_audio_startup` and `_prepare` do.
    fn mai_up(&self, setup: &Setup, audio_hz: u64) -> Result<(), DriverError> {
        let channels = u32::from(setup.allocation.channels);
        let mask = (1u32 << channels) - 1;
        let (n, m) =
            sample_divider(audio_hz, u64::from(setup.rate.hz())).ok_or(DriverError::OutOfRange)?;
        let code = MAI_RATES
            .iter()
            .find(|(hz, _)| *hz == setup.rate.hz())
            .map(|(_, code)| *code)
            .ok_or(DriverError::Unsupported)?;
        self.mai(
            hd::MAI_CTL,
            hd::MAI_RESET | hd::MAI_FLUSH | hd::MAI_DLATE | hd::MAI_ERRORE | hd::MAI_ERRORF,
        )?;
        self.rng(true)?;
        let narrow = |value: u64| u32::try_from(value).map_err(|_| DriverError::OutOfRange);
        self.mai(
            hd::MAI_SMP,
            field(narrow(n)?, 8, 24) | field(narrow(m - 1)?, 0, 8),
        )?;
        self.mai(
            hd::MAI_CTL,
            field(channels, 4, 4) | hd::MAI_WHOLSMP | hd::MAI_CHALIGN | hd::MAI_ENABLE,
        )?;
        self.mai(
            hd::MAI_FMT,
            field(code, 8, 8) | field(hd::MAI_FORMAT_PCM, 16, 8),
        )?;
        self.mai(hd::MAI_THR, MAI_THRESHOLDS)?;
        let core_regs = &self.regs.core;
        core_regs.write32(
            core::MAI_CONFIG,
            core::MAI_CONFIG_BIT_REVERSE | core::MAI_CONFIG_FORMAT_REVERSE | mask,
        )?;
        // The subframes leave the codec in slot order already.
        let identity = (0..channels).fold(0, |map, slot| map | slot << (4 * slot));
        core_regs.write32(core::MAI_CHANNEL_MAP, identity)?;
        core_regs.write32(
            core::AUDIO_PACKET_CONFIG,
            core::AUDIO_PACKET_ZERO_FLAT
                | core::AUDIO_PACKET_ZERO_INACTIVE
                | field(PREAMBLE_BLOCK, 10, 4)
                | field(mask, 0, 8),
        )
    }

    /// Take the MAI down, its `InfoFrame` stopped, as Linux's
    /// `vc4_hdmi_audio_shutdown` and `_reset` do; every step runs whatever
    /// fails.
    fn mai_down(&self) -> Result<(), DriverError> {
        let quiet = self.mai(hd::MAI_CTL, hd::MAI_DLATE | hd::MAI_ERRORE | hd::MAI_ERRORF);
        let rng = self.rng(false);
        let audio_frame = 1 << (AudioInfo::KIND - 0x80);
        let stopped = self
            .regs
            .core
            .read32(core::RAM_PACKET_CONFIG)
            .and_then(|config| {
                self.regs
                    .core
                    .write32(core::RAM_PACKET_CONFIG, config & !audio_frame)
            });
        let reset = [hd::MAI_RESET, hd::MAI_ERRORF, hd::MAI_FLUSH]
            .into_iter()
            .try_for_each(|value| self.mai(hd::MAI_CTL, value));
        quiet.and(rng).and(stopped).and(reset)
    }

    fn set_up_audio(
        &mut self,
        setup: Setup,
        framing: Framing,
        period_frames: u32,
    ) -> Result<(), DriverError> {
        let playback = &mut self.path_mut()?.playback;
        playback.halt_parking()?;
        playback.codec_mut()?.set(framing);
        playback.configure(setup.rate, period_frames)?;
        let audio_hz = self.hold_audio_clocks()?;
        self.mai_up(&setup, audio_hz)?;
        self.path_mut()?.setup = Some(setup);
        match self.chosen.filter(|chosen| self.sending && chosen.hdmi) {
            Some(chosen) => self.audio_link_up(&chosen),
            None => Ok(()),
        }
    }

    fn tear_down_audio(&mut self) -> Result<(), DriverError> {
        if let Some(audio) = self.audio.as_mut() {
            audio.setup = None;
        }
        let mai = if self.reachable() {
            self.mai_down()
        } else {
            Ok(())
        };
        mai.and(self.release_audio_clocks())
    }
}

impl<R, C, P, D, L, M> Audio for Hdmi<R, C, P, D, L, M>
where
    R: RegisterBlock,
    C: LinkCall,
    P: Ddc,
    D: Delay,
    L: Recorder<Record>,
    M: DmaPort,
{
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        Ok(AudioDeviceFacts {
            endpoints: 1,
            name: AudioName::new(self.variant.name).map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        sole_endpoint(endpoint)?;
        let audio = self.path()?;
        let offer = &audio.offer;
        let support = offer.at(offer.layout.channels());
        let narrower = u32::from(support.word_bits) < SampleFormat::S24In32.valid_bits();
        Ok(AudioEndpointFacts {
            index: 0,
            direction: StreamDirection::Playback,
            jack: audio.jack(),
            formats: SampleFormats::EMPTY.with(SampleFormat::S24In32),
            resolution: narrower.then_some(support.word_bits),
            channel_map: offer.layout,
            rates: support.rates().ok_or(DriverError::DeviceFault)?,
            min_period_frames: MIN_PERIOD_FRAMES,
            max_period_frames: MAX_PERIOD_FRAMES,
            max_ring_frames: MAX_RING_FRAMES,
            gain: None,
            name: AudioName::new(self.variant.name).map_err(|_| DriverError::DeviceFault)?,
        })
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        sole_endpoint(endpoint)?;
        let audio = self.path()?;
        if audio.playback.is_running() {
            return Err(DriverError::Busy);
        }
        let offer = audio.offer;
        let allocation = allocate(&params.channel_map)
            .filter(|placed| placed.fits(offer.speakers))
            .ok_or(DriverError::Unsupported)?;
        let support = offer.at(allocation.channels);
        let rate = support
            .rates()
            .ok_or(DriverError::Unsupported)?
            .nearest(params.rate);
        let framing =
            Framing::new(&allocation, rate, support.word_bits).ok_or(DriverError::Unsupported)?;
        let period_frames = params
            .period_frames
            .clamp(MIN_PERIOD_FRAMES, MAX_PERIOD_FRAMES);
        let setup = Setup { rate, allocation };
        self.set_up_audio(setup, framing, period_frames)
            .inspect_err(|_| {
                // Nothing half set up is left holding a buffer or a clock.
                if let Some(audio) = self.audio.as_mut() {
                    let _ = audio.playback.release();
                }
                let _ = self.tear_down_audio();
            })?;
        Ok(ConfigureGrant {
            rate,
            format: SampleFormat::S24In32,
            channel_map: params.channel_map,
            period_frames,
            max_ring_frames: MAX_RING_FRAMES,
        })
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        let audio = self.path_mut()?;
        if audio.setup.is_none() {
            return Err(DriverError::DeviceFault);
        }
        if audio.playback.is_running() {
            return Ok(());
        }
        audio.playback.halt_parking()?;
        audio.playback.start(at)
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.path_mut()?.playback.stop(at)
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        self.path_mut()?.playback.drain()
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        sole_endpoint(endpoint)?;
        self.path_mut()?.playback.service(ring)
    }

    fn set_gain(&mut self, endpoint: u16, _millibel: i32, _mute: bool) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        Err(DriverError::NotImplemented)
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        sole_endpoint(endpoint)?;
        let playback = &mut self.path_mut()?.playback;
        let stopped = if playback.is_running() {
            playback.stop(Frames::ZERO)
        } else {
            Ok(())
        };
        let halted = playback.halt_parking();
        let released = playback.release();
        let torn = self.tear_down_audio();
        stopped.and(halted).and(released).and(torn)
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        let audio = self.path_mut()?;
        let mut causes = audio.playback.take_interrupt()?;
        let jack = audio.jack();
        if jack != audio.reported {
            audio.reported = jack;
            causes.jack_changed |= 1;
        }
        Ok(causes)
    }

    fn set_event_interrupts(&mut self, enabled: bool) -> Result<(), DriverError> {
        self.path_mut()?.playback.set_events(enabled)
    }
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
