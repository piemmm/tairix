//! The audio endpoint over the register model: what it offers for the sink,
//! the MAI set up for a stream, N, CTS and the `InfoFrame` once the link
//! sends, the samples framed in the DMA buffer, and the clocks it holds.

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{
    Audio, ChannelMap, ChannelPosition, Frames, JackState, Rate, SampleFormat,
};
use tairix_abi::driver::audio_channel::{ConfigureGrant, ConfigureParams};
use tairix_abi::driver::audio_ring::{PcmGeometry, PcmRing};
use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::Encoder;
use tairix_abi::DriverError;
use tairix_audiochan::cyclic::PERIODS;
use tairix_drvrt::testkit::Records;
use tairix_edid::synth::{dtd_1080p, Base, Cta};
use tairix_edid::AudioInfo;

use super::{n_cts, sample_divider, MIN_PERIOD_FRAMES};
use crate::hdmi::HSM_MIN_HZ;
use crate::iec958::PREAMBLE_BLOCK;
use crate::model::{Model, Tx, Window, AUDIO_CLOCK_HZ};
use crate::regs::{core, hd, packet, phy};
use crate::VARIANTS;

const AUDIO: u8 = 1;
const VIDEO: u8 = 2;
const VENDOR: u8 = 3;
const SPEAKERS: u8 = 4;
const HDMI_14: [u8; 7] = [0x03, 0x0C, 0x00, 0x10, 0x00, 0x00, 68];

/// HDMI0's MAI block.
const MAI: usize = 0x10;

/// Stereo linear PCM at 32, 44.1 and 48 kHz, 16, 20 and 24 bits.
const STEREO_SAD: [u8; 3] = [0x09, 0x07, 0x07];
/// Six channels at 48 and 96 kHz.
const SURROUND_SAD: [u8; 3] = [0x0D, 0x14, 0x07];
/// Stereo at the basic rates, sixteen bits alone.
const NARROW_SAD: [u8; 3] = [0x09, 0x07, 0x01];
/// Front pair, LFE, centre and rear pair.
const SURROUND_SPEAKERS: u8 = 0x0F;

const FULL_HD: Timing = Timing {
    clock_khz: 148_500,
    horizontal: Axis {
        active: 1920,
        front: 88,
        sync: 44,
        back: 148,
    },
    vertical: Axis {
        active: 1080,
        front: 4,
        sync: 5,
        back: 36,
    },
    hsync: Polarity::Positive,
    vsync: Polarity::Positive,
    scan: Scan::Progressive,
    repetition: 1,
};

fn television(sads: &[u8], speakers: u8) -> Vec<[u8; 128]> {
    vec![
        Base::new().descriptor(0, dtd_1080p()).extensions(1).build(),
        Cta::new()
            .flags(0x40)
            .block(VIDEO, &[16 | 0x80])
            .block(AUDIO, sads)
            .block(SPEAKERS, &[speakers, 0, 0])
            .block(VENDOR, &HDMI_14)
            .build(),
    ]
}

fn open(model: &Model) -> Tx {
    model
        .open(VARIANTS[0], false, &Records::default())
        .expect("opened")
}

fn rate(hz: u32) -> Rate {
    Rate::new(hz).expect("a rate")
}

fn configure(tx: &mut Tx, hz: u32, channel_map: ChannelMap) -> Result<ConfigureGrant, DriverError> {
    Audio::configure(
        tx,
        0,
        &ConfigureParams {
            endpoint: 0,
            rate: rate(hz),
            format: SampleFormat::S24In32,
            channel_map,
            period_frames: MIN_PERIOD_FRAMES,
        },
    )
}

fn bring_up(tx: &mut Tx) {
    Encoder::configure(tx, &FULL_HD).expect("configured");
    Encoder::prepare(tx).expect("prepared");
    Encoder::enable(tx).expect("enabled");
}

fn mai(model: &Model, offset: usize) -> u32 {
    model.reg(Window::Hd, MAI + offset)
}

/// The clock regeneration configuration sending `n`, its CTS supplied.
fn crp(n: u32) -> u32 {
    core::CRP_EXTERNAL_CTS | n
}

/// The MAI's sample clock divided from the audio clock by `n` over `m`.
fn smp(n: u32, m: u32) -> u32 {
    n << 8 | (m - 1)
}

fn audio_frame_words(channels: u8, allocation: u8) -> Vec<u32> {
    let frame = AudioInfo {
        channels,
        allocation,
    }
    .pack();
    let b = frame.as_bytes();
    let mut words = vec![
        u32::from_le_bytes([b[0], b[1], b[2], 0]),
        u32::from_le_bytes([b[3], b[4], b[5], b[6]]),
        u32::from_le_bytes([b[7], b[8], b[9], 0]),
        u32::from_le_bytes([b[10], b[11], b[12], b[13]]),
    ];
    words.resize(packet::STRIDE / 4, 0);
    words
}

fn slot(model: &Model, id: usize) -> Vec<u32> {
    (0..packet::STRIDE / 4)
        .map(|word| model.reg(Window::Packet, id * packet::STRIDE + word * 4))
        .collect()
}

#[test]
fn the_endpoint_offers_what_the_sink_plays_and_keeps_it_when_the_sink_leaves() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    let facts = tx.endpoint_facts(0).expect("facts");
    assert_eq!(facts.jack, JackState::Present);
    assert_eq!(facts.channel_map, ChannelMap::STEREO);
    assert!(facts.formats.contains(SampleFormat::S24In32));
    assert_eq!(facts.resolution, None, "it takes 24 bits");
    for (hz, offered) in [(44_100, true), (48_000, true), (96_000, false)] {
        assert_eq!(facts.rates.nearest(rate(hz)) == rate(hz), offered, "{hz}");
    }
    assert!(facts.validate().is_ok());
    model.unplug();
    model.fire(1);
    tx.service_event(0).expect("serviced");
    let left = tx.endpoint_facts(0).expect("facts");
    assert_eq!(left.jack, JackState::Absent);
    assert_eq!(
        (left.rates, left.channel_map),
        (facts.rates, facts.channel_map)
    );
}

#[test]
fn a_surround_sink_is_offered_5_1_placed_by_its_allocation() {
    let model = Model::new(0);
    model.plug(&television(&SURROUND_SAD, SURROUND_SPEAKERS));
    let mut tx = open(&model);
    let layout = tx.endpoint_facts(0).expect("facts").channel_map;
    assert_eq!(layout.channels(), 6);
    configure(&mut tx, 48_000, layout).expect("configured");
    assert_eq!(mai(&model, hd::MAI_CTL) >> 4 & 0xF, 6);
    assert_eq!(
        model.reg(Window::Core, core::AUDIO_PACKET_CONFIG) & 0xFF,
        0x3F
    );
    bring_up(&mut tx);
    assert_eq!(slot(&model, 4), audio_frame_words(6, 0x0B));
}

#[test]
fn each_layout_is_offered_the_rates_its_own_descriptors_name() {
    let model = Model::new(0);
    model.plug(&television(&SURROUND_SAD, SURROUND_SPEAKERS));
    let mut tx = open(&model);
    let facts = tx.endpoint_facts(0).expect("facts");
    assert_eq!(facts.channel_map.channels(), 6);
    assert_eq!(
        facts.rates.nearest(rate(44_100)),
        rate(48_000),
        "no 44.1 kHz in 5.1"
    );
    assert_eq!(facts.rates.nearest(rate(96_000)), rate(96_000));
    let surround = configure(&mut tx, 44_100, facts.channel_map).expect("configured");
    assert_eq!(surround.rate, rate(48_000));
    tx.release(0).expect("released");
    let stereo = configure(&mut tx, 44_100, ChannelMap::STEREO).expect("configured");
    assert_eq!(stereo.rate, rate(44_100), "basic audio's");
}

#[test]
fn a_layout_is_refused_without_the_speakers_it_plays_on() {
    let model = Model::new(0);
    model.plug(&television(&SURROUND_SAD, 0x01));
    let mut tx = open(&model);
    assert_eq!(
        tx.endpoint_facts(0).expect("facts").channel_map,
        ChannelMap::STEREO
    );
    let two_one = ChannelMap::new(&[
        ChannelPosition::FrontLeft,
        ChannelPosition::FrontRight,
        ChannelPosition::LowFrequency,
    ])
    .expect("2.1");
    for map in [ChannelMap::conventional(6).expect("5.1"), two_one] {
        assert!(matches!(
            configure(&mut tx, 48_000, map),
            Err(DriverError::Unsupported)
        ));
    }
    configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
}

#[test]
fn a_stream_set_up_with_the_output_down_programs_the_mai_and_holds_its_clocks() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    let grant = configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
    assert_eq!(
        (grant.rate, grant.format),
        (rate(48_000), SampleFormat::S24In32)
    );
    assert_eq!(
        mai(&model, hd::MAI_SMP),
        smp(2_250, 1),
        "108 MHz over 48 kHz, exactly"
    );
    assert_eq!(
        mai(&model, hd::MAI_CTL),
        2 << 4 | hd::MAI_WHOLSMP | hd::MAI_CHALIGN | hd::MAI_ENABLE
    );
    assert_eq!(mai(&model, hd::MAI_FMT), 9 << 8 | hd::MAI_FORMAT_PCM << 16);
    assert_eq!(
        model.reg(Window::Core, core::AUDIO_PACKET_CONFIG),
        core::AUDIO_PACKET_ZERO_FLAT
            | core::AUDIO_PACKET_ZERO_INACTIVE
            | PREAMBLE_BLOCK << 10
            | 0x3
    );
    assert_eq!(model.reg(Window::Core, core::MAI_CONFIG) & 0xFFFF, 0x3);
    assert_eq!(
        model.reg(Window::Phy, phy::POWERDOWN_CTL) & phy::POWERDOWN_RNDGEN,
        0
    );
    assert_eq!(
        model.reg(Window::Core, core::CRP_CFG),
        0,
        "no link to regenerate a clock over"
    );
    let state = model.0.borrow();
    assert!(state.hsm.held, "the MAI's registers need it");
    assert_eq!(state.audio.asked, [AUDIO_CLOCK_HZ]);
    assert_eq!(state.stalls, 0);
}

#[test]
fn the_link_coming_up_sends_n_cts_and_the_audio_infoframe() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
    bring_up(&mut tx);
    assert_eq!(model.reg(Window::Core, core::CRP_CFG), crp(6_144));
    assert_eq!(model.reg(Window::Core, core::CTS_0), 148_500);
    assert_eq!(model.reg(Window::Core, core::CTS_1), 148_500);
    assert_eq!(
        model.reg(Window::Core, core::RAM_PACKET_CONFIG),
        core::RAM_PACKET_ENABLE | 1 << 2 | 1 << 4,
        "the AVI and the audio frames"
    );
    assert_eq!(slot(&model, 4), audio_frame_words(2, 0x00));
    assert_eq!(
        model.reg(Window::Phy, phy::POWERDOWN_CTL) & phy::POWERDOWN_RNDGEN,
        0
    );
}

#[test]
fn a_stream_set_up_on_a_link_already_sending_is_regenerated_at_once() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    bring_up(&mut tx);
    configure(&mut tx, 44_100, ChannelMap::STEREO).expect("configured");
    assert_eq!(model.reg(Window::Core, core::CRP_CFG), crp(6_272));
    assert_eq!(
        model.reg(Window::Core, core::CTS_0),
        165_000,
        "exact: 148.5 MHz over 900 Hz"
    );
    assert_eq!(
        mai(&model, hd::MAI_SMP),
        smp(120_000, 49),
        "108 MHz over 44.1 kHz is 120000/49"
    );
}

#[test]
fn a_stream_s_samples_reach_the_mai_as_subframes() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
    let geometry =
        PcmGeometry::new(MIN_PERIOD_FRAMES * 8, SampleFormat::S24In32, 2).expect("geometry");
    let mut bytes = vec![0; geometry.region_len()];
    let mut ring = PcmRing::bind(&mut bytes, geometry).expect("ring");
    let mut frames = Vec::new();
    for _ in 0..MIN_PERIOD_FRAMES * PERIODS {
        frames.extend_from_slice(&0x0012_3456i32.to_le_bytes());
        frames.extend_from_slice(&(-1i32).to_le_bytes());
    }
    ring.write(&frames).expect("written");
    let staged = tx.service(0, &mut ring).expect("serviced");
    assert_eq!(staged.transferred, MIN_PERIOD_FRAMES * PERIODS);
    tx.set_event_interrupts(true).expect("unmasked");
    tx.start(0, Frames::ZERO).expect("started");
    let channel = tx
        .audio
        .as_mut()
        .expect("it carries sound")
        .playback
        .dma_mut();
    channel.advance(1);
    let [left, right] = [channel.sent[0], channel.sent[1]];
    assert_eq!(
        left & 0xF,
        PREAMBLE_BLOCK,
        "the block starts on the first frame"
    );
    assert_eq!(left >> 4 & 0xFF_FFFF, 0x12_3456);
    assert_eq!(right >> 4 & 0xFF_FFFF, 0xFF_FFFF);
    assert_eq!(
        channel.sent[2] & 0xF,
        0x2,
        "the next frame's first subframe"
    );
}

#[test]
fn a_sink_taking_narrower_samples_gets_them_without_the_bits_below() {
    let model = Model::new(0);
    model.plug(&television(&NARROW_SAD, 0x01));
    let mut tx = open(&model);
    let facts = tx.endpoint_facts(0).expect("facts");
    assert_eq!(
        facts.resolution,
        Some(16),
        "the mixer dithers to what it takes"
    );
    configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
    let geometry =
        PcmGeometry::new(MIN_PERIOD_FRAMES * 8, SampleFormat::S24In32, 2).expect("geometry");
    let mut bytes = vec![0; geometry.region_len()];
    let mut ring = PcmRing::bind(&mut bytes, geometry).expect("ring");
    let frames: Vec<u8> = (0..MIN_PERIOD_FRAMES * PERIODS * 2)
        .flat_map(|_| 0x0012_3456i32.to_le_bytes())
        .collect();
    ring.write(&frames).expect("written");
    tx.service(0, &mut ring).expect("serviced");
    tx.start(0, Frames::ZERO).expect("started");
    let channel = tx
        .audio
        .as_mut()
        .expect("it carries sound")
        .playback
        .dma_mut();
    channel.advance(1);
    assert!(channel
        .sent
        .iter()
        .all(|word| word >> 4 & 0xFF_FFFF == 0x12_3400));
}

#[test]
fn releasing_takes_the_mai_down_and_lets_its_clocks_go() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    bring_up(&mut tx);
    configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
    tx.release(0).expect("released");
    assert_eq!(
        mai(&model, hd::MAI_CTL),
        hd::MAI_FLUSH,
        "reset, errors cleared, flushed"
    );
    assert_eq!(
        model.reg(Window::Core, core::RAM_PACKET_CONFIG) & 1 << 4,
        0,
        "its frame stopped"
    );
    assert_ne!(
        model.reg(Window::Phy, phy::POWERDOWN_CTL) & phy::POWERDOWN_RNDGEN,
        0
    );
    let state = model.0.borrow();
    assert!(!state.audio.held);
    assert!(state.hsm.held, "the output still runs");
    assert_eq!(state.stalls, 0);
}

#[test]
fn a_stream_outlives_the_output_brought_down_and_follows_it_back_up() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    bring_up(&mut tx);
    configure(&mut tx, 48_000, ChannelMap::STEREO).expect("configured");
    Encoder::disable(&mut tx).expect("disabled");
    Encoder::power_down(&mut tx).expect("powered down");
    {
        let state = model.0.borrow();
        assert!(state.hsm.held, "the MAI still runs");
        assert_eq!(
            state.hsm.asked.last(),
            Some(&HSM_MIN_HZ),
            "lowered to its least"
        );
    }
    model
        .0
        .borrow_mut()
        .regs
        .remove(&(Window::Core, core::CRP_CFG));
    bring_up(&mut tx);
    assert_eq!(model.reg(Window::Core, core::CRP_CFG), crp(6_144), "again");
    tx.release(0).expect("released");
    Encoder::disable(&mut tx).expect("disabled");
    Encoder::power_down(&mut tx).expect("powered down");
    let state = model.0.borrow();
    assert!(!state.hsm.held, "nothing holds it now");
    assert_eq!(state.stalls, 0);
}

#[test]
fn a_connector_change_is_reported_once() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    assert_eq!(
        tx.take_interrupt().expect("causes").jack_changed,
        1,
        "found present"
    );
    assert_eq!(tx.take_interrupt().expect("causes").jack_changed, 0);
    model.unplug();
    model.fire(1);
    tx.service_event(0).expect("serviced");
    assert_eq!(tx.take_interrupt().expect("causes").jack_changed, 1, "gone");
    assert_eq!(tx.take_interrupt().expect("causes").jack_changed, 0);
}

#[test]
fn a_layout_the_sink_cannot_place_is_refused_holding_nothing() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = open(&model);
    for map in [
        ChannelMap::conventional(6).expect("5.1"),
        ChannelMap::conventional(4).expect("4.0"),
    ] {
        assert!(matches!(
            configure(&mut tx, 48_000, map),
            Err(DriverError::Unsupported)
        ));
    }
    let state = model.0.borrow();
    assert!(!state.hsm.held && !state.audio.held);
}

#[test]
fn n_is_a_multiple_of_128_and_cts_is_exact_on_whole_kilohertz_clocks() {
    for (clock_khz, hz, expected) in [
        (148_500, 48_000, (6_144, 148_500)),
        (148_500, 44_100, (6_272, 165_000)),
        (74_250, 32_000, (4_096, 74_250)),
        (594_000, 192_000, (24_576, 594_000)),
        (594_000, 176_400, (25_088, 660_000)),
        (25_175, 48_000, (6_144, 25_175)),
    ] {
        let (n, cts) = n_cts(clock_khz, rate(hz)).expect("a regeneration");
        assert_eq!((n, cts), expected, "{clock_khz} kHz at {hz} Hz");
        assert!(n.is_multiple_of(128));
    }
    assert_eq!(n_cts(148_500, rate(22_050)), None, "no MAI rate");
}

#[test]
fn the_sample_divider_is_the_best_fraction_within_its_fields() {
    assert_eq!(sample_divider(108_000_000, 48_000), Some((2_250, 1)));
    assert_eq!(sample_divider(108_000_000, 44_100), Some((120_000, 49)));
    let (n, m) = sample_divider(108_000_000, 7_919).expect("a fraction");
    assert!(n < 1 << 24 && m <= 256);
    let ratio = 108_000_000f64 / 7_919f64;
    let [n, m] = [n, m].map(|value| f64::from(u32::try_from(value).expect("in its field")));
    assert!((n / m - ratio).abs() / ratio < 1e-6);
}

/// A transmitter whose sound cannot be had still drives its video: its links
/// beside the sound's are all it needs, and every audio request is refused
/// rather than reaching a path it does not have.
#[test]
fn a_transmitter_without_sound_still_drives_its_video() {
    let model = Model::new(0);
    model.plug(&television(&STEREO_SAD, 0x01));
    let mut tx = model
        .open_with(VARIANTS[0], false, &Records::default(), false)
        .expect("opened without sound");
    assert!(tx.audio.is_none());
    let connector = Encoder::connector(&tx);
    assert!(connector.connected);
    assert_eq!(connector.mode, Some(FULL_HD), "the sink is read as before");
    bring_up(&mut tx);
    assert_eq!(
        Audio::endpoint_facts(&tx, 0).err(),
        Some(DriverError::NotFound)
    );
    assert_eq!(
        configure(&mut tx, 48_000, ChannelMap::STEREO).err(),
        Some(DriverError::NotFound)
    );
    assert_eq!(
        Audio::start(&mut tx, 0, Frames::ZERO),
        Err(DriverError::NotFound)
    );
    assert!(Audio::take_interrupt(&mut tx).is_err());
}
