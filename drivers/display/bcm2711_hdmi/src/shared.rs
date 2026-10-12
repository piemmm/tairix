//! One transmitter served on two endpoints by one thread: the encoder its
//! engine drives and the audio endpoint its mixer drives each reach it
//! through a share.
//!
//! Neither facet calls the other while it holds the transmitter, so a share
//! never finds it held; one that did would refuse rather than wait.

use alloc::rc::Rc;
use core::cell::RefCell;

use tairix_abi::driver::audio::{
    Audio, AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, AudioServiced, Frames,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::driver::display::Timing;
use tairix_abi::driver::encoder::{Connector, Encoder, Phase, PipeLimits};
use tairix_abi::DriverError;

/// A share of a device both protocols drive.
pub struct Shared<T>(Rc<RefCell<T>>);

impl<T> Shared<T> {
    /// Two shares of `device`.
    pub fn pair(device: T) -> (Self, Self) {
        let device = Rc::new(RefCell::new(device));
        (Self(Rc::clone(&device)), Self(device))
    }

    fn read<R>(&self, f: impl FnOnce(&T) -> R) -> Result<R, DriverError> {
        self.0
            .try_borrow()
            .map(|device| f(&device))
            .map_err(|_| DriverError::Busy)
    }

    fn write<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, DriverError> {
        self.0
            .try_borrow_mut()
            .map(|mut device| f(&mut device))
            .map_err(|_| DriverError::Busy)
    }
}

/// A connector no reading could be had of: nothing to run.
const UNREAD: Connector = Connector {
    connected: false,
    mode: None,
    link: 0,
};

impl<T: Encoder> Encoder for Shared<T> {
    fn connector(&self) -> Connector {
        self.read(Encoder::connector).unwrap_or(UNREAD)
    }

    fn phase(&self) -> Phase {
        // The server reads the phase once, before either facet runs.
        self.read(Encoder::phase).unwrap_or(Phase::Off)
    }

    fn limit(&mut self, limits: &PipeLimits) -> Result<bool, DriverError> {
        self.write(|device| device.limit(limits))?
    }

    fn service_event(&mut self, index: usize) -> Result<bool, DriverError> {
        self.write(|device| device.service_event(index))?
    }

    fn deadline_us(&self) -> Option<u64> {
        self.read(Encoder::deadline_us).ok().flatten()
    }

    fn expired(&mut self) -> Result<bool, DriverError> {
        self.write(Encoder::expired)?
    }

    fn configure(&mut self, timing: &Timing) -> Result<(), DriverError> {
        self.write(|device| device.configure(timing))?
    }

    fn prepare(&mut self) -> Result<(), DriverError> {
        self.write(Encoder::prepare)?
    }

    fn enable(&mut self) -> Result<(), DriverError> {
        self.write(Encoder::enable)?
    }

    fn disable(&mut self) -> Result<(), DriverError> {
        self.write(Encoder::disable)?
    }

    fn power_down(&mut self) -> Result<(), DriverError> {
        self.write(Encoder::power_down)?
    }
}

impl<T: Audio> Audio for Shared<T> {
    fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
        self.read(Audio::device_facts)?
    }

    fn endpoint_facts(&self, endpoint: u16) -> Result<AudioEndpointFacts, DriverError> {
        self.read(|device| device.endpoint_facts(endpoint))?
    }

    fn configure(
        &mut self,
        endpoint: u16,
        params: &ConfigureParams,
    ) -> Result<ConfigureGrant, DriverError> {
        self.write(|device| Audio::configure(device, endpoint, params))?
    }

    fn start(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        self.write(|device| device.start(endpoint, at))?
    }

    fn stop(&mut self, endpoint: u16, at: Frames) -> Result<(), DriverError> {
        self.write(|device| device.stop(endpoint, at))?
    }

    fn drain(&mut self, endpoint: u16) -> Result<(), DriverError> {
        self.write(|device| device.drain(endpoint))?
    }

    fn service(
        &mut self,
        endpoint: u16,
        ring: &mut PcmRing<'_>,
    ) -> Result<AudioServiced, DriverError> {
        self.write(|device| device.service(endpoint, ring))?
    }

    fn set_gain(&mut self, endpoint: u16, millibel: i32, mute: bool) -> Result<(), DriverError> {
        self.write(|device| device.set_gain(endpoint, millibel, mute))?
    }

    fn release(&mut self, endpoint: u16) -> Result<(), DriverError> {
        self.write(|device| device.release(endpoint))?
    }

    fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
        self.write(Audio::take_interrupt)?
    }

    fn set_event_interrupts(&mut self, enabled: bool) -> Result<(), DriverError> {
        self.write(|device| device.set_event_interrupts(enabled))?
    }
}
