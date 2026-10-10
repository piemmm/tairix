//! Host tests for the pure per-endpoint device-channel handler, driven
//! against a mock [`Audio`] device.
//!
//! The mock is a *device*, not a second implementation of the contract: it
//! records what it was asked to do and answers what a real converter would,
//! so what these tests exercise is the server's state machine, its geometry
//! validation, and its fail-closed refusals.

use super::*;
use crate::mock_audio::{
    MockAudio, MOCK_ENDPOINTS, MOCK_MAX_RING, MOCK_PERIOD, MOCK_RATE_HZ, SINK, SOURCE,
};
use tairix_abi::driver::audio::{
    AudioDeviceFacts, AudioEndpointFacts, AudioInterrupt, ChannelMap, Rate, SampleFormat,
    StreamDirection,
};
use tairix_abi::driver::audio_ring::{aligned_region, REGION_ALIGN_PADDING};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::time::Time64;

/// Frames the tests' shared region holds: the smallest power of two that can
/// carry a mock period.
const RING_FRAMES: u32 = 512;

/// A region large enough for [`RING_FRAMES`] stereo 16-bit frames, plus the
/// padding an aligned view is cut from.
struct Region {
    bytes: [u8; RING_BYTES + REGION_ALIGN_PADDING],
}

/// Bytes [`RING_FRAMES`] stereo 16-bit frames need, header included.
const RING_BYTES: usize =
    tairix_abi::driver::audio_ring::PCM_RING_HEADER_LEN + RING_FRAMES as usize * 2 * 2;

impl Region {
    fn new() -> Self {
        Self {
            bytes: [0u8; RING_BYTES + REGION_ALIGN_PADDING],
        }
    }

    fn view(&mut self) -> &mut [u8] {
        aligned_region(&mut self.bytes, RING_BYTES).expect("padded for alignment")
    }
}

fn params(endpoint: u16) -> ConfigureParams {
    ConfigureParams {
        endpoint,
        rate: Rate::new(MOCK_RATE_HZ).expect("in range"),
        format: SampleFormat::S16,
        channel_map: ChannelMap::STEREO,
        period_frames: MOCK_PERIOD,
    }
}

fn attach_params(endpoint: u16, ring_frames: u32) -> AttachParams {
    AttachParams {
        endpoint,
        ring_frames,
        region_grant: 0x5AFE,
        notify_endpoint: 0xACE0 + u64::from(endpoint),
    }
}

/// Configure and attach `endpoint` with a [`RING_FRAMES`] ring.
fn ready(server: &mut AudioChannelServer<MockAudio>, endpoint: u16) {
    assert!(tairix_abi::driver::audio_channel::decode_configure_reply(
        &server.configure_reply(&params(endpoint))
    )
    .is_ok());
    assert_eq!(
        decode_status_reply(&server.attach(&attach_params(endpoint, RING_FRAMES))),
        Ok(())
    );
}

#[test]
fn a_fresh_server_answers_facts_and_refuses_everything_that_needs_a_region() {
    let mut server = AudioChannelServer::new(MockAudio::new());

    let facts = tairix_abi::driver::audio_channel::decode_facts_reply(&server.facts_reply())
        .expect("the device answers its facts before anything is configured");
    assert_eq!(facts.endpoints, MOCK_ENDPOINTS);

    let endpoint = tairix_abi::driver::audio_channel::decode_endpoint_reply(
        &server.endpoint_facts_reply(SINK),
    )
    .expect("and each endpoint's");
    assert_eq!(endpoint.direction, StreamDirection::Playback);

    // Everything that would clock the device refuses: there is nowhere for
    // frames to come from or go.
    assert_eq!(
        decode_status_reply(&server.start(SINK, Frames::ZERO)),
        Err(Errno::NotConnected)
    );
    assert_eq!(
        decode_status_reply(&server.stop(SINK, Frames::ZERO)),
        Err(Errno::NotConnected)
    );
    assert_eq!(
        decode_status_reply(&server.drain(SINK)),
        Err(Errno::NotConnected)
    );
    let mut region = Region::new();
    assert_eq!(
        server.service(SINK, region.view()),
        Err(Errno::NotConnected)
    );
    assert!(!server.any_attached());
}

#[test]
fn an_endpoint_the_contract_does_not_admit_is_refused_before_the_device_is_touched() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    let past = MAX_DEVICE_ENDPOINTS;
    assert_eq!(
        tairix_abi::driver::audio_channel::decode_configure_reply(
            &server.configure_reply(&params(past))
        ),
        Err(Errno::NotFound)
    );
    assert_eq!(
        decode_status_reply(&server.attach(&attach_params(past, RING_FRAMES))),
        Err(Errno::NotFound)
    );
    assert_eq!(
        decode_status_reply(&server.start(past, Frames::ZERO)),
        Err(Errno::NotFound)
    );
    assert_eq!(
        decode_status_reply(&server.set_gain(past, 0, false)),
        Err(Errno::NotFound)
    );
    assert_eq!(
        decode_status_reply(&server.detach(past)),
        Err(Errno::NotFound)
    );
    // The device never saw any of it.
    assert!(server.audio().endpoints.iter().all(|e| !e.configured()));
}

#[test]
fn attach_refuses_a_ring_the_devices_own_grant_does_not_admit() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    assert!(tairix_abi::driver::audio_channel::decode_configure_reply(
        &server.configure_reply(&params(SINK))
    )
    .is_ok());

    for bad in [
        MOCK_PERIOD / 2,   // smaller than one period
        MOCK_MAX_RING * 2, // past the device's ceiling
        RING_FRAMES + 1,   // not a power of two
    ] {
        assert_eq!(
            decode_status_reply(&server.attach(&attach_params(SINK, bad))),
            Err(Errno::OutOfRange),
            "ring of {bad} frames must be refused"
        );
        assert!(!server.is_attached(SINK), "a refused attach never binds");
    }

    assert_eq!(
        decode_status_reply(&server.attach(&attach_params(SINK, RING_FRAMES))),
        Ok(())
    );
    assert!(server.is_attached(SINK));
    assert_eq!(server.geometry(SINK).map(|g| g.frames()), Some(RING_FRAMES));
    assert_eq!(server.notify_endpoint(SINK), Some(0xACE0));
}

#[test]
fn attaching_before_configuring_is_refused() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    assert_eq!(
        decode_status_reply(&server.attach(&attach_params(SINK, RING_FRAMES))),
        Err(Errno::NotConnected)
    );
}

#[test]
fn a_reconfiguration_drops_the_attached_region_rather_than_leaving_it_the_wrong_shape() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);
    assert!(server.is_attached(SINK));

    assert!(tairix_abi::driver::audio_channel::decode_configure_reply(
        &server.configure_reply(&params(SINK))
    )
    .is_ok());
    assert!(
        !server.is_attached(SINK),
        "the region's size follows the grant, so a re-grant invalidates it"
    );
    let mut region = Region::new();
    assert_eq!(
        server.service(SINK, region.view()),
        Err(Errno::NotConnected)
    );
}

#[test]
fn a_grant_no_power_of_two_ring_could_hold_is_refused_as_a_device_fault() {
    struct ImpossibleGrant;
    impl Audio for ImpossibleGrant {
        fn device_facts(&self) -> Result<AudioDeviceFacts, DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn endpoint_facts(&self, _: u16) -> Result<AudioEndpointFacts, DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn configure(
            &mut self,
            _: u16,
            _: &ConfigureParams,
        ) -> Result<ConfigureGrant, DriverError> {
            // 1000 rounds up to a 1024-frame ring, which its own ceiling of
            // 1023 cannot hold: no attach could ever succeed.
            Ok(ConfigureGrant {
                rate: Rate::HZ_48000,
                format: SampleFormat::S16,
                channel_map: ChannelMap::STEREO,
                period_frames: 1_000,
                max_ring_frames: 1_023,
            })
        }
        fn start(&mut self, _: u16, _: Frames) -> Result<(), DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn stop(&mut self, _: u16, _: Frames) -> Result<(), DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn drain(&mut self, _: u16) -> Result<(), DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn service(&mut self, _: u16, _: &mut PcmRing<'_>) -> Result<AudioServiced, DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn set_gain(&mut self, _: u16, _: i32, _: bool) -> Result<(), DriverError> {
            Err(DriverError::NotImplemented)
        }
        fn release(&mut self, _: u16) -> Result<(), DriverError> {
            Ok(())
        }
        fn take_interrupt(&mut self) -> Result<AudioInterrupt, DriverError> {
            Ok(AudioInterrupt::NONE)
        }
        fn set_event_interrupts(&mut self, _: bool) -> Result<(), DriverError> {
            Ok(())
        }
    }

    let mut server = AudioChannelServer::new(ImpossibleGrant);
    assert_eq!(
        tairix_abi::driver::audio_channel::decode_configure_reply(
            &server.configure_reply(&params(SINK))
        ),
        Err(Errno::DeviceFault)
    );
    assert!(!server.is_attached(SINK));
}

#[test]
fn a_serviced_period_reports_what_moved_and_the_loss_since_the_last_one() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SOURCE);
    assert_eq!(
        decode_status_reply(&server.start(SOURCE, Frames::ZERO)),
        Ok(())
    );

    let mut region = Region::new();
    server.audio_mut().transfer = 128;
    let first = server.service(SOURCE, region.view()).expect("serviced");
    assert_eq!(first.report.transferred, 128);
    assert!(first.report.running);
    assert_eq!(first.lost_frames, 0);

    // A loss is reported once, as the delta: the wire report carries the
    // running total, the notify carries what just happened.
    server.audio_mut().add_xrun = 64;
    let second = server.service(SOURCE, region.view()).expect("serviced");
    assert_eq!(second.report.xrun_frames, 64);
    assert_eq!(second.lost_frames, 64);

    server.audio_mut().add_xrun = 0;
    let third = server.service(SOURCE, region.view()).expect("serviced");
    assert_eq!(third.report.xrun_frames, 64);
    assert_eq!(third.lost_frames, 0, "the same loss is not reported twice");
}

#[test]
fn a_service_over_a_region_that_is_not_the_agreed_shape_is_refused() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);

    let mut short = [0u8; 64];
    assert_eq!(server.service(SINK, &mut short), Err(Errno::BufferTooSmall));
}

#[test]
fn gain_is_device_state_and_is_accepted_before_a_region_exists() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    assert_eq!(
        decode_status_reply(&server.set_gain(SINK, -1_200, true)),
        Ok(())
    );
    assert_eq!(
        server.audio().slot(SINK).expect("present").gain_millibel,
        -1_200
    );
    assert!(server.audio().slot(SINK).expect("present").muted);

    // A device with no control refuses, which is how the mixer learns to
    // apply the gain itself rather than believing the hardware did.
    server.audio_mut().has_gain = false;
    assert_eq!(
        decode_status_reply(&server.set_gain(SINK, -600, false)),
        Err(Errno::NotImplemented)
    );
}

#[test]
fn detach_releases_the_device_and_returns_the_endpoint_to_unconfigured() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);
    ready(&mut server, SOURCE);
    assert!(server.any_attached());

    assert_eq!(decode_status_reply(&server.detach(SINK)), Ok(()));
    assert_eq!(server.audio().slot(SINK).expect("present").released, 1);
    assert!(!server.audio().slot(SINK).expect("present").configured());
    assert!(!server.is_attached(SINK));
    assert_eq!(server.notify_endpoint(SINK), None);

    // The sibling endpoint is untouched: state is per endpoint, not per
    // channel.
    assert!(server.is_attached(SOURCE));
    assert!(server.any_attached());
    assert_eq!(server.audio().slot(SOURCE).expect("present").released, 0);

    assert_eq!(decode_status_reply(&server.detach(SOURCE)), Ok(()));
    assert!(!server.any_attached());
}

#[test]
fn a_release_the_hardware_refused_still_forgets_the_channel_state() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);
    server.audio_mut().fault = Some(DriverError::DeviceFault);

    assert_eq!(
        decode_status_reply(&server.detach(SINK)),
        Err(Errno::DeviceFault)
    );
    // The process is about to unmap the region, so keeping the state would
    // leave a later `Service` binding a mapping that no longer exists.
    assert!(!server.is_attached(SINK));
    assert!(!server.any_attached());
}

#[test]
fn a_device_fault_reaches_the_caller_as_a_typed_refusal_rather_than_a_panic() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);
    server.audio_mut().fault = Some(DriverError::DeviceFault);

    assert_eq!(
        tairix_abi::driver::audio_channel::decode_facts_reply(&server.facts_reply()),
        Err(Errno::DeviceFault)
    );
    assert_eq!(
        decode_status_reply(&server.start(SINK, Frames::ZERO)),
        Err(Errno::DeviceFault)
    );
    let mut region = Region::new();
    assert_eq!(server.service(SINK, region.view()), Err(Errno::DeviceFault));
}

#[test]
fn the_service_reply_carries_the_running_total_the_wire_contract_states() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);
    assert_eq!(
        decode_status_reply(&server.start(SINK, Frames::new(9))),
        Ok(())
    );

    let mut region = Region::new();
    server.audio_mut().add_xrun = 5;
    let reply = server.service_reply(SINK, region.view());
    let report = tairix_abi::driver::audio_channel::decode_service_reply(&reply).expect("decodes");
    assert_eq!(report.xrun_frames, 5);
    assert_eq!(report.position, Frames::new(9));
    assert!(report.running);
    assert_eq!(report.sampled_at, Time64::from_secs(7));
}

/// The process lets the old region go before it offers the new one, so a
/// re-attach the server refuses must not leave the old attach standing over a
/// mapping that no longer exists.
#[test]
fn a_refused_re_attach_leaves_the_endpoint_detached() {
    let mut server = AudioChannelServer::new(MockAudio::new());
    ready(&mut server, SINK);
    assert!(server.is_attached(SINK));
    assert_eq!(
        decode_status_reply(&server.attach(&attach_params(SINK, RING_FRAMES + 1))),
        Err(Errno::OutOfRange)
    );
    assert!(!server.is_attached(SINK));
    assert_eq!(server.notify_endpoint(SINK), None);
}
