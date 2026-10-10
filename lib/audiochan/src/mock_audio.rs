//! A mock audio device for the crate's host tests: it records what it was
//! asked and answers as a real converter would, so the tests exercise the
//! channel's state machine rather than an agreement with the mock.

use tairix_abi::driver::audio::{
    Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioName, AudioServiced,
    ChannelMap, Frames, GainRange, JackState, Rate, RateSet, RateSupport, SampleFormat,
    SampleFormats, StreamDirection,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::time::Time64;
use tairix_abi::DriverError;

/// Endpoints the mock presents: one sink, one source.
pub(crate) const MOCK_ENDPOINTS: u16 = 2;
/// The mock's playback endpoint.
pub(crate) const SINK: u16 = 0;
/// The mock's capture endpoint.
pub(crate) const SOURCE: u16 = 1;
/// The only rate the mock's converter runs at, so a request for another rate
/// exercises the substitution path.
pub(crate) const MOCK_RATE_HZ: u32 = 48_000;
/// Frames the mock interrupts on, whatever was asked for.
pub(crate) const MOCK_PERIOD: u32 = 256;
/// Frames the mock can hold in flight.
pub(crate) const MOCK_MAX_RING: u32 = 4_096;

/// Where one endpoint of the mock device stands in its own lifecycle.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum MockState {
    /// Nothing programmed.
    #[default]
    Idle,
    /// Programmed but not clocking.
    Configured,
    /// Clocking.
    Running,
}

/// What one endpoint of the mock device is doing.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MockEndpoint {
    pub(crate) state: MockState,
    pub(crate) drained: bool,
    pub(crate) released: u32,
    pub(crate) position: Frames,
    pub(crate) xrun_frames: u64,
    pub(crate) gain_millibel: i32,
    pub(crate) muted: bool,
}

impl MockEndpoint {
    pub(crate) fn configured(&self) -> bool {
        !matches!(self.state, MockState::Idle)
    }

    pub(crate) fn running(&self) -> bool {
        matches!(self.state, MockState::Running)
    }
}

/// A two-endpoint audio device that answers like a real converter and records
/// what it was told.
pub(crate) struct MockAudio {
    pub(crate) endpoints: [MockEndpoint; MOCK_ENDPOINTS as usize],
    /// Frames the next `service` reports as moved.
    pub(crate) transfer: u32,
    /// Frames the next `service` adds to the endpoint's loss tally.
    pub(crate) add_xrun: u64,
    /// Whether the device reports a gain control.
    pub(crate) has_gain: bool,
    /// What the next `take_interrupt` reports.
    pub(crate) pending: AudioInterrupt,
    /// Event sources armed.
    pub(crate) events_enabled: bool,
    /// Every call the device engine refuses, so the server's error paths are
    /// reachable without a broken mock.
    pub(crate) fault: Option<DriverError>,
}

impl MockAudio {
    pub(crate) fn new() -> Self {
        Self {
            endpoints: [MockEndpoint::default(); MOCK_ENDPOINTS as usize],
            transfer: 0,
            add_xrun: 0,
            has_gain: true,
            pending: AudioInterrupt::NONE,
            events_enabled: false,
            fault: None,
        }
    }

    pub(crate) fn slot(&self, endpoint: u16) -> Result<&MockEndpoint, DriverError> {
        self.endpoints
            .get(usize::from(endpoint))
            .ok_or(DriverError::NotFound)
    }

    fn slot_mut(&mut self, endpoint: u16) -> Result<&mut MockEndpoint, DriverError> {
        if let Some(err) = self.fault {
            return Err(err);
        }
        self.endpoints
            .get_mut(usize::from(endpoint))
            .ok_or(DriverError::NotFound)
    }
}

impl Audio for MockAudio {
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        if let Some(err) = self.fault {
            return Err(err);
        }
        Ok(AudioDeviceFacts {
            endpoints: MOCK_ENDPOINTS,
            name: AudioName::new("Mock Converter").expect("fits"),
        })
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        if let Some(err) = self.fault {
            return Err(err);
        }
        if endpoint >= MOCK_ENDPOINTS {
            return Err(DriverError::NotFound);
        }
        Ok(AudioEndpointFacts {
            index: endpoint,
            direction: if endpoint == SINK {
                StreamDirection::Playback
            } else {
                StreamDirection::Capture
            },
            jack: JackState::Present,
            formats: SampleFormats::EMPTY.with(SampleFormat::S16),
            channel_map: ChannelMap::STEREO,
            rates: RateSupport::Discrete(
                RateSet::new(&[Rate::new(MOCK_RATE_HZ).expect("in range")]).expect("ascending"),
            ),
            min_period_frames: MOCK_PERIOD,
            max_period_frames: MOCK_PERIOD,
            max_ring_frames: MOCK_MAX_RING,
            gain: self
                .has_gain
                .then(|| GainRange::new(-6_000, 0, 50).expect("ordered")),
            name: AudioName::new("Line Out").expect("fits"),
        })
    }

    fn configure(
        &mut self,
        endpoint: u16,
        _params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        self.slot_mut(endpoint)?.state = MockState::Configured;
        Ok(ConfigureGrant {
            rate: Rate::new(MOCK_RATE_HZ).expect("in range"),
            format: SampleFormat::S16,
            channel_map: ChannelMap::STEREO,
            period_frames: MOCK_PERIOD,
            max_ring_frames: MOCK_MAX_RING,
        })
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        let slot = self.slot_mut(endpoint)?;
        if !slot.configured() {
            return Err(DriverError::DeviceFault);
        }
        slot.state = MockState::Running;
        slot.position = at;
        Ok(())
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        let slot = self.slot_mut(endpoint)?;
        slot.state = MockState::Configured;
        slot.position = at;
        Ok(())
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        self.slot_mut(endpoint)?.drained = true;
        Ok(())
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        let transfer = self.transfer;
        let add_xrun = self.add_xrun;
        let direction = if endpoint == SINK {
            StreamDirection::Playback
        } else {
            StreamDirection::Capture
        };
        let slot = self.slot_mut(endpoint)?;
        if !slot.configured() {
            return Err(DriverError::DeviceFault);
        }
        // A real converter reads the ring (playback) or writes it (capture);
        // the mock does the same so a torn geometry would surface here.
        let moved = match direction {
            StreamDirection::Playback => {
                ring.discard(transfer).map_err(|_| DriverError::BadMagic)?
            }
            StreamDirection::Capture => ring
                .write_silence(transfer)
                .map_err(|_| DriverError::BadMagic)?,
        };
        slot.xrun_frames += add_xrun;
        slot.position = Frames::new(slot.position.get() + u64::from(moved));
        Ok(AudioServiced {
            transferred: moved,
            running: slot.running(),
            position: slot.position,
            xrun_frames: slot.xrun_frames,
            sampled_at: Time64::from_secs(7),
        })
    }

    fn set_gain(&mut self, endpoint: u16, millibel: i32, mute: bool) -> Result<(), DriverError> {
        if !self.has_gain {
            return Err(DriverError::NotImplemented);
        }
        let slot = self.slot_mut(endpoint)?;
        slot.gain_millibel = millibel;
        slot.muted = mute;
        Ok(())
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        let slot = self.slot_mut(endpoint)?;
        slot.state = MockState::Idle;
        slot.released += 1;
        Ok(())
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        if let Some(err) = self.fault {
            return Err(err);
        }
        Ok(core::mem::replace(&mut self.pending, AudioInterrupt::NONE))
    }

    fn set_event_interrupts(&mut self, enabled: bool) -> Result<(), DriverError> {
        if let Some(err) = self.fault {
            return Err(err);
        }
        self.events_enabled = enabled;
        Ok(())
    }
}
