extern crate std;

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{
    Audio, ChannelMap, Frames, JackState, Rate, SampleFormat, StreamDirection,
};
use tairix_abi::driver::audio_channel::ConfigureParams;
use tairix_abi::driver::audio_ring::{aligned_region, PcmGeometry, PcmRing, REGION_ALIGN_PADDING};
use tairix_abi::DriverError;

use super::{Hda, MAX_PERIOD_FRAMES, PERIODS, PERIOD_STEP};
use crate::model::{
    config, kind, pin, Bench, ModelCodec, ModelController, ModelDelay, ModelWait, ModelWidget,
    QEMU_AMP,
};
use crate::regs::sd;
use crate::verb::{self, pin_control};

type TestHda<'h> = Hda<'h, ModelController, ModelWait, ModelDelay>;

fn open(
    bench: &Bench,
    codecs: impl IntoIterator<Item = (u8, ModelCodec)>,
) -> Result<TestHda<'_>, DriverError> {
    Hda::open(
        bench.controller(codecs),
        bench.wait.clone(),
        bench.delay.clone(),
        &bench.dma,
        &bench.clock,
    )
}

fn model<'a>(hda: &'a mut TestHda<'_>) -> &'a mut ModelController {
    hda.controller.registers_mut()
}

fn params(period_frames: u32) -> ConfigureParams {
    ConfigureParams {
        endpoint: 0,
        rate: Rate::new(48_000).expect("a rate"),
        format: SampleFormat::S16,
        channel_map: ChannelMap::STEREO,
        period_frames,
    }
}

/// A ring and the padded storage it lives in.
struct Ring {
    storage: Vec<u8>,
    geometry: PcmGeometry,
}

impl Ring {
    fn new(frames: u32) -> Self {
        let geometry = PcmGeometry::new(frames, SampleFormat::S16, 2).expect("a geometry");
        Self {
            storage: vec![0; geometry.region_len() + REGION_ALIGN_PADDING],
            geometry,
        }
    }

    fn bind(&mut self) -> PcmRing<'_> {
        let region = aligned_region(&mut self.storage, self.geometry.region_len()).expect("padded");
        PcmRing::bind(region, self.geometry).expect("a ring")
    }

    /// Queue frames `from..to`, each spelling its own index.
    fn queue(&mut self, from: u32, to: u32) {
        let samples: Vec<u8> = (from..to).flat_map(counting_frame).collect();
        assert_eq!(self.bind().write(&samples).expect("room"), to - from);
    }
}

fn counting_frame(frame: u32) -> [u8; 4] {
    let word = ((frame & 0xFFFF) as u16).to_le_bytes();
    [word[0], word[1], word[0], word[1]]
}

fn counted(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|frame| u32::from(u16::from_le_bytes([frame[0], frame[1]])))
        .collect()
}

/// The first output descriptor, as QEMU's controller orders them.
const OUT0: u8 = 4;

#[test]
fn qemus_controller_comes_up_with_one_stereo_line_out() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    assert_eq!(hda.device_facts().expect("facts").endpoints, 1);
    let facts = hda.endpoint_facts(0).expect("the line out");
    assert_eq!(facts.direction, StreamDirection::Playback);
    assert_eq!(facts.channel_map, ChannelMap::STEREO);
    assert!(facts.formats.contains(SampleFormat::S16));
    assert_eq!(facts.jack, JackState::Unknown, "QEMU's jack cannot tell");
    assert_eq!(facts.name.as_str(), "Line Out (Green)");
    let gain = facts.gain.expect("QEMU's converter amplifier");
    assert_eq!(
        (
            gain.min_millibel(),
            gain.max_millibel(),
            gain.step_millibel()
        ),
        (-7_400, 0, 100)
    );
    assert_eq!(
        (facts.min_period_frames, facts.max_period_frames),
        (PERIOD_STEP, MAX_PERIOD_FRAMES)
    );
    assert_eq!(
        bench.dma.quiesced.get(),
        1,
        "the reset frees an earlier instance's memory"
    );
    let codec = &model(&mut hda).link[&0];
    assert_eq!(codec.afg_power, verb::POWER_D0);
    assert_eq!(codec.widget(3).pin_control, pin_control::OUT);
    assert_eq!(codec.widget(2).amp(true, 0, true), Some(0x4A), "0 dB, open");
}

#[test]
fn configuring_programs_the_descriptor_its_buffer_and_the_converter() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    let grant = hda.configure(0, &params(480)).expect("configured");
    assert_eq!(grant.period_frames, 512, "rounded to whole 128-byte pieces");
    assert_eq!(grant.rate.hz(), 48_000);
    let model = model(&mut hda);
    let period_bytes = 512 * 4;
    assert_eq!(
        model.descriptor_register(OUT0, sd::CBL),
        period_bytes * PERIODS
    );
    assert_eq!(
        model.descriptor_register(OUT0, sd::LVI) & 0xFFFF,
        PERIODS - 1
    );
    assert_eq!(model.descriptor_register(OUT0, sd::FMT) & 0xFFFF, 0x0011);
    let tag = model.descriptor_register(OUT0, sd::CTL) >> 20 & 0xF;
    assert_eq!(tag, 1);
    let converter = model.link[&0].widget(2).clone();
    assert_eq!(converter.format, 0x0011);
    assert_eq!(
        converter.stream_channel, 0x10,
        "stream one, channels from zero"
    );
    let bdl = u64::from(model.descriptor_register(OUT0, sd::BDPL))
        | (u64::from(model.descriptor_register(OUT0, sd::BDPU)) << 32);
    let entries = model.memory.read(bdl, 16 * PERIODS as usize);
    for entry in entries.as_chunks::<16>().0 {
        assert_eq!(
            u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]),
            period_bytes
        );
        assert_eq!(entry[12] & 1, 1, "each period interrupts");
    }
}

/// Four periods are staged before the start, each boundary refills the one
/// just played, and what reaches the codec is the ring in order.
#[test]
fn playback_moves_the_ring_through_the_buffer_in_order() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    hda.set_event_interrupts(true).expect("armed");
    let mut ring = Ring::new(1024);
    ring.queue(0, 1024);
    assert_eq!(
        hda.service(0, &mut ring.bind())
            .expect("staged")
            .transferred,
        256
    );
    hda.start(0, Frames::ZERO).expect("started");
    for period in 0..8u32 {
        model(&mut hda).tick(64 * 4);
        let causes = hda.take_interrupt().expect("causes");
        assert_eq!(causes.period_elapsed, 1, "period {period}");
        let serviced = hda.service(0, &mut ring.bind()).expect("refilled");
        assert_eq!(serviced.position, Frames::new(u64::from(period + 1) * 64));
        assert!(serviced.running);
        assert_eq!(serviced.xrun_frames, 0);
    }
    let played = &model(&mut hda).played[&OUT0];
    assert_eq!(counted(played), (0..512).collect::<Vec<u32>>());
}

#[test]
fn a_ring_short_of_a_due_period_is_padded_with_silence_and_counted() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    let mut ring = Ring::new(1024);
    ring.queue(0, 64);
    hda.service(0, &mut ring.bind()).expect("staged");
    hda.start(0, Frames::ZERO).expect("started");
    let serviced = hda.service(0, &mut ring.bind()).expect("serviced");
    assert_eq!(
        serviced.xrun_frames, 64,
        "the second period was due and empty"
    );
    model(&mut hda).tick(64 * 4);
    let serviced = hda.service(0, &mut ring.bind()).expect("serviced");
    assert_eq!(serviced.xrun_frames, 128, "and then the third");
}

#[test]
fn a_drain_plays_out_and_reports_the_frame_it_fell_silent_at() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    let mut ring = Ring::new(1024);
    ring.queue(0, 160);
    hda.service(0, &mut ring.bind()).expect("staged");
    hda.start(0, Frames::new(1_000)).expect("started");
    hda.service(0, &mut ring.bind()).expect("the tail");
    hda.drain(0).expect("draining");
    for _ in 0..2 {
        model(&mut hda).tick(64 * 4);
        let serviced = hda.service(0, &mut ring.bind()).expect("serviced");
        assert!(serviced.running);
    }
    model(&mut hda).tick(64 * 4);
    let serviced = hda.service(0, &mut ring.bind()).expect("drained");
    assert!(!serviced.running);
    assert_eq!(serviced.position, Frames::new(1_160));
    assert!(!model(&mut hda).running(OUT0));
}

#[test]
fn a_stop_rewinds_and_a_restart_plays_from_the_first_period() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    let mut ring = Ring::new(1024);
    ring.queue(0, 512);
    hda.service(0, &mut ring.bind()).expect("staged");
    hda.start(0, Frames::ZERO).expect("started");
    model(&mut hda).tick(100 * 4);
    hda.stop(0, Frames::new(100)).expect("stopped");
    assert!(!model(&mut hda).running(OUT0));
    let serviced = hda.service(0, &mut ring.bind()).expect("staged again");
    assert!(!serviced.running);
    assert_eq!(serviced.position, Frames::new(100));
    hda.start(0, Frames::new(100)).expect("restarted");
    model(&mut hda).tick(64 * 4);
    let serviced = hda.service(0, &mut ring.bind()).expect("serviced");
    assert_eq!(serviced.position, Frames::new(164));
}

#[test]
fn a_level_rounds_up_to_the_next_step_and_mutes_on_the_amplifier() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.set_gain(0, -650, false).expect("set");
    assert_eq!(
        model(&mut hda).link[&0].widget(2).amp(true, 0, false),
        Some(0x4A - 6)
    );
    hda.set_gain(0, -9_000, true).expect("set");
    assert_eq!(
        model(&mut hda).link[&0].widget(2).amp(true, 0, true),
        Some(0x80),
        "the floor, muted"
    );
}

#[test]
fn an_endpoint_with_no_amplifier_leaves_its_level_to_the_mixer() {
    let bench = Bench::new();
    let mut codec = ModelCodec::qemu_output();
    codec.widgets[0] =
        ModelWidget::new(2, kind::OUTPUT | kind::STEREO).pcm(crate::model::QEMU_PCM, 1);
    let mut hda = open(&bench, [(0, codec)]).expect("comes up");
    assert!(hda.endpoint_facts(0).expect("facts").gain.is_none());
    assert_eq!(
        hda.set_gain(0, -600, false),
        Err(DriverError::NotImplemented)
    );
}

/// A command parks for its answer; a period that ends meanwhile is cleared
/// in the controller so its line raises again, and kept for the engine.
#[test]
fn a_period_that_ends_while_a_command_waits_is_kept() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    let mut ring = Ring::new(1024);
    ring.queue(0, 512);
    hda.service(0, &mut ring.bind()).expect("staged");
    hda.start(0, Frames::ZERO).expect("started");
    model(&mut hda).hold_responses = Some(bench.wait.parks.clone());
    model(&mut hda).tick(64 * 4);
    let parks = bench.wait.parks.get();
    hda.set_gain(0, -100, false).expect("set");
    assert!(
        bench.wait.parks.get() > parks,
        "the command parked for its answer"
    );
    assert_eq!(
        model(&mut hda).descriptor_register(OUT0, sd::CTL) >> 24 & u32::from(sd::BCIS),
        0,
        "cleared in the controller"
    );
    assert_eq!(
        hda.take_interrupt().expect("causes").period_elapsed,
        1,
        "kept for the engine"
    );
}

#[test]
fn an_eight_channel_output_tunes_each_converter_to_its_pair() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::desktop())]).expect("comes up");
    let facts = hda.endpoint_facts(0).expect("the rear panel");
    assert_eq!(facts.channel_map.channels(), 8);
    let mut eight = params(64);
    eight.channel_map = facts.channel_map;
    hda.configure(0, &eight).expect("configured");
    let codec = &model(&mut hda).link[&0];
    let channels: Vec<u8> = [0x02, 0x04, 0x03, 0x05]
        .iter()
        .map(|&nid| codec.widget(nid).stream_channel)
        .collect();
    assert_eq!(channels, [0x10, 0x12, 0x14, 0x16]);
    assert!(codec
        .widgets
        .iter()
        .filter(|widget| [2, 3, 4, 5].contains(&widget.nid))
        .all(|widget| widget.format == 0x0017));
    assert_eq!(
        codec.widget(0x1B).pin_control,
        pin_control::OUT,
        "the front jack mirrors the front pair; it cannot drive headphones"
    );
}

#[test]
fn inputs_that_share_a_converter_cannot_run_together() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::desktop())]).expect("comes up");
    let inputs: Vec<u16> = (0..hda.device_facts().expect("facts").endpoints)
        .filter(|&index| {
            hda.endpoint_facts(index).expect("facts").direction == StreamDirection::Capture
        })
        .collect();
    assert_eq!(inputs.len(), 3);
    let (internal, rear, line) = (inputs[0], inputs[1], inputs[2]);
    let mut capture = params(64);
    capture.endpoint = internal;
    hda.configure(internal, &capture)
        .expect("the internal microphone");
    capture.endpoint = rear;
    hda.configure(rear, &capture)
        .expect("the rear microphone has its own converter");
    capture.endpoint = line;
    assert_eq!(hda.configure(line, &capture), Err(DriverError::Busy));
    hda.release(internal).expect("released");
    hda.configure(line, &capture).expect("free once released");
    let selector = model(&mut hda).link[&0].widget(0x23).select;
    assert_eq!(selector, 1, "the line input's entry");
}

#[test]
fn capture_hands_each_finished_period_to_the_ring() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::desktop())]).expect("comes up");
    let source = (0..hda.device_facts().expect("facts").endpoints)
        .find(|&index| {
            hda.endpoint_facts(index).expect("facts").direction == StreamDirection::Capture
        })
        .expect("an input");
    let mut capture = params(64);
    capture.endpoint = source;
    hda.configure(source, &capture).expect("configured");
    hda.start(source, Frames::ZERO).expect("started");
    let mut ring = Ring::new(1024);
    model(&mut hda).tick(64 * 4);
    let serviced = hda.service(source, &mut ring.bind()).expect("delivered");
    assert_eq!(serviced.transferred, 64);
    let mut captured = vec![0u8; 64 * 4];
    assert_eq!(ring.bind().read(&mut captured).expect("readable"), 64);
    assert_eq!(captured, (0..=255u8).collect::<Vec<u8>>());
}

#[test]
fn a_digital_output_is_switched_on_when_configured_and_off_when_released() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::desktop())]).expect("comes up");
    let spdif = (0..hda.device_facts().expect("facts").endpoints)
        .find(|&index| {
            hda.endpoint_facts(index)
                .expect("facts")
                .name
                .as_str()
                .starts_with("S/PDIF")
        })
        .expect("S/PDIF");
    let mut digital = params(64);
    digital.endpoint = spdif;
    hda.configure(spdif, &digital).expect("configured");
    assert_eq!(model(&mut hda).link[&0].widget(0x06).digital, 1);
    hda.release(spdif).expect("released");
    let converter = model(&mut hda).link[&0].widget(0x06).clone();
    assert_eq!((converter.digital, converter.stream_channel), (0, 0));
}

#[test]
fn a_display_is_named_for_its_monitor_and_told_what_it_carries() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(2, ModelCodec::display(Some("DELL U2723QE")))]).expect("comes up");
    let facts = hda.endpoint_facts(0).expect("the display");
    assert_eq!(facts.name.as_str(), "HDMI: DELL U2723QE");
    assert_eq!(facts.jack, JackState::Present);
    hda.configure(0, &params(64)).expect("configured");
    let connector = model(&mut hda).link[&2].widget(0x03).clone();
    assert_eq!(connector.dip[..3], [0x84, 0x01, 0x0A]);
    let sum = connector
        .dip
        .iter()
        .fold(0u8, |sum, &byte| sum.wrapping_add(byte));
    assert_eq!(sum, 0, "the infoframe's checksum");
    assert_eq!(connector.dip[4], 1, "two channels");
    assert_eq!(model(&mut hda).link[&2].widget(0x02).channel_count, 1);
}

#[test]
fn a_display_with_no_monitor_is_absent_under_its_connectors_name() {
    let bench = Bench::new();
    let hda = open(&bench, [(2, ModelCodec::display(None))]).expect("comes up");
    let facts = hda.endpoint_facts(0).expect("the display");
    assert_eq!(
        (facts.name.as_str(), facts.jack),
        ("HDMI", JackState::Absent)
    );
}

/// A laptop's speaker and headphone jack share its one converter: plugging
/// headphones in silences the speaker, and the jack change is reported.
#[test]
fn headphones_take_over_from_the_speaker_they_share_a_converter_with() {
    let bench = Bench::new();
    let laptop = ModelCodec {
        vendor: 0x10EC_0256,
        afg: 1,
        afg_pcm: crate::model::QEMU_PCM,
        afg_streams: 1,
        widgets: vec![
            ModelWidget::new(0x02, kind::OUTPUT | kind::STEREO).amps(0, QEMU_AMP),
            ModelWidget::new(0x14, kind::PIN | kind::STEREO)
                .sources(&[0x02])
                .pin(pin::OUT | pin::EAPD, config(2, 0x10, 0x1, 0, 1, 0)),
            ModelWidget::new(0x21, kind::PIN | kind::STEREO | kind::UNSOLICITED)
                .sources(&[0x02])
                .pin(
                    pin::OUT | pin::PRESENCE | pin::HEADPHONE,
                    config(0, 0x02, 0x2, 0x1, 2, 0),
                ),
        ],
        ..ModelCodec::default()
    };
    let mut hda = open(&bench, [(0, laptop)]).expect("comes up");
    assert_eq!(hda.device_facts().expect("facts").endpoints, 1);
    assert_eq!(
        hda.endpoint_facts(0).expect("facts").jack,
        JackState::Present,
        "a fixed speaker"
    );
    let codec = &model(&mut hda).link[&0];
    assert_eq!(codec.widget(0x14).pin_control, pin_control::OUT);
    assert_eq!(codec.widget(0x14).eapd, 0b10);
    assert_eq!(
        codec.widget(0x21).pin_control,
        pin_control::OUT | pin_control::HEADPHONE
    );
    assert_eq!(codec.widget(0x21).unsolicited, 0x80 | 1);
    let model = model(&mut hda);
    model
        .link
        .get_mut(&0)
        .expect("codec")
        .widget_mut(0x21)
        .present = true;
    model.unsolicited(0, 1);
    hda.take_interrupt().expect("causes");
    assert_eq!(model_pin(&mut hda, 0x14), 0, "the speaker is silenced");
    let model = hda.controller.registers_mut();
    model
        .link
        .get_mut(&0)
        .expect("codec")
        .widget_mut(0x21)
        .present = false;
    model.unsolicited(0, 1);
    hda.take_interrupt().expect("causes");
    assert_eq!(
        model_pin(&mut hda, 0x14),
        pin_control::OUT,
        "and back when they come out"
    );
}

fn model_pin(hda: &mut TestHda<'_>, nid: u8) -> u8 {
    model(hda).link[&0].widget(nid).pin_control
}

#[test]
fn a_jack_change_is_reported_for_its_endpoint() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::desktop())]).expect("comes up");
    assert_eq!(
        hda.endpoint_facts(0).expect("facts").jack,
        JackState::Absent
    );
    let model = model(&mut hda);
    model
        .link
        .get_mut(&0)
        .expect("codec")
        .widget_mut(0x14)
        .present = true;
    model.unsolicited(0, 1);
    let causes = hda.take_interrupt().expect("causes");
    assert_eq!(causes.jack_changed, 1);
    assert_eq!(
        hda.endpoint_facts(0).expect("facts").jack,
        JackState::Present
    );
}

#[test]
fn a_fifo_error_is_reported_as_a_loss() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    model(&mut hda).starve(OUT0);
    assert_eq!(hda.take_interrupt().expect("causes").xrun, 1);
}

#[test]
fn a_controller_that_never_leaves_reset_is_refused() {
    let bench = Bench::new();
    let mut controller = bench.controller([(0, ModelCodec::qemu_output())]);
    controller.stuck_in_reset = true;
    let opened = Hda::open(
        controller,
        bench.wait.clone(),
        bench.delay.clone(),
        &bench.dma,
        &bench.clock,
    );
    assert!(matches!(opened, Err(DriverError::DeviceFault)));
}

#[test]
fn a_link_with_no_codec_presents_nothing_and_is_refused() {
    let bench = Bench::new();
    assert!(matches!(open(&bench, []), Err(DriverError::NotFound)));
}

#[test]
fn releasing_stops_the_descriptor_and_frees_it_for_the_next_stream() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    hda.start(0, Frames::ZERO).expect("started");
    hda.release(0).expect("released");
    assert!(!model(&mut hda).running(OUT0));
    assert_eq!(model(&mut hda).link[&0].widget(2).stream_channel, 0);
    hda.configure(0, &params(64))
        .expect("the same descriptor serves again");
    assert_eq!(
        model(&mut hda).descriptor_register(OUT0, sd::CBL),
        64 * 4 * PERIODS
    );
}

/// An answer that comes after its command timed out could pass for the next
/// command's, so the rings restart and discard it; the codec is then heard
/// again.
#[test]
fn an_answer_too_late_is_discarded_and_the_codec_heard_again() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    model(&mut hda).hold_responses = Some(bench.wait.parks.clone());
    model(&mut hda).hold_parks = 2;
    assert_eq!(hda.set_gain(0, -100, false), Err(DriverError::DeviceFault));
    model(&mut hda).hold_responses = None;
    hda.set_gain(0, -200, false).expect("heard again");
    assert_eq!(
        model(&mut hda).link[&0].widget(2).amp(true, 0, true),
        Some(0x4A - 2)
    );
}

/// A descriptor's position stays where it stopped until it moves again; a
/// restart must not count that as this stream's progress.
#[test]
fn a_restart_does_not_count_the_last_streams_position() {
    let bench = Bench::new();
    let mut hda = open(&bench, [(0, ModelCodec::qemu_output())]).expect("comes up");
    hda.configure(0, &params(64)).expect("configured");
    let mut ring = Ring::new(1024);
    ring.queue(0, 512);
    hda.service(0, &mut ring.bind()).expect("staged");
    hda.start(0, Frames::ZERO).expect("started");
    model(&mut hda).tick(100 * 4);
    hda.stop(0, Frames::new(100)).expect("stopped");
    hda.start(0, Frames::new(100)).expect("restarted");
    let serviced = hda.service(0, &mut ring.bind()).expect("serviced");
    assert_eq!(serviced.position, Frames::new(100));
}

/// A response names its codec but not its function, so every endpoint's tag
/// is its own across the whole link rather than restarting per function.
#[test]
fn every_endpoint_watches_its_jack_under_a_tag_of_its_own() {
    let bench = Bench::new();
    let hda = open(
        &bench,
        [
            (0, ModelCodec::desktop()),
            (2, ModelCodec::display(Some("DELL U2723QE"))),
        ],
    )
    .expect("comes up");
    let mut tags: Vec<u8> = hda.endpoints.iter().map(|endpoint| endpoint.tag).collect();
    let count = tags.len();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(tags.len(), count);
    assert!(tags.iter().all(|&tag| (1..64).contains(&tag)));
}
