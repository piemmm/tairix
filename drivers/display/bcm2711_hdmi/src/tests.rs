//! The transmitter over the register model: what it finds on its connector,
//! the mode it chooses, every step of a mode set and its teardown, and the
//! rule that no register is reached without the state machine clock.

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, Rate, SampleFormat};
use tairix_abi::driver::audio_channel::ConfigureParams;
use tairix_abi::driver::display::{Axis, Polarity, Scan, Timing};
use tairix_abi::driver::encoder::{Encoder, Phase, PipeLimits};
use tairix_abi::{DriverError, Errno};
use tairix_drvrt::testkit::Records;
use tairix_edid::synth::{dtd, dtd_1080p, dtd_720p, Base, Cta, SYNC_POSITIVE};
use tairix_edid::{Aspect, Avi, Quantization, VideoCode};

use crate::model::{Model, Tx, Window};
use crate::regs::{core, csc, dvp, hd, packet, phy};
use crate::{Record, VARIANTS};

const VIDEO: u8 = 2;
const VENDOR: u8 = 3;

/// An HDMI 1.4 sink's vendor block: its physical address and 340 MHz.
const HDMI_14: [u8; 7] = [0x03, 0x0C, 0x00, 0x10, 0x00, 0x00, 68];
/// An HDMI 2.0 sink's Forum block: 600 MHz, SCDC present.
const FORUM: [u8; 7] = [0xD8, 0x5D, 0xC4, 1, 120, 0x80, 0];

const HDMI0_VID_CTL: usize = 0x44;

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

const UHD_60: Timing = Timing {
    clock_khz: 594_000,
    horizontal: Axis {
        active: 3840,
        front: 176,
        sync: 88,
        back: 296,
    },
    vertical: Axis {
        active: 2160,
        front: 8,
        sync: 10,
        back: 72,
    },
    ..FULL_HD
};

fn uhd_dtd(clock_khz: u32) -> [u8; 18] {
    dtd(
        clock_khz,
        [3840, 176, 88, 296],
        [2160, 8, 10, 72],
        SYNC_POSITIVE,
        (1210, 680),
    )
}

/// A monitor speaking HDMI 1.4, 1080p preferred.
fn hdmi_monitor() -> Vec<[u8; 128]> {
    vec![
        Base::new().descriptor(0, dtd_1080p()).extensions(1).build(),
        Cta::new()
            .block(VIDEO, &[16 | 0x80, 4])
            .block(VENDOR, &HDMI_14)
            .build(),
    ]
}

/// A monitor with no CTA extension: a DVI sink.
fn dvi_monitor() -> Vec<[u8; 128]> {
    vec![Base::new().descriptor(0, dtd_1080p()).build()]
}

/// An HDMI 2.0 television, 4K at 60 Hz preferred.
fn uhd_television() -> Vec<[u8; 128]> {
    vec![
        Base::new()
            .descriptor(0, uhd_dtd(594_000))
            .extensions(1)
            .build(),
        Cta::new()
            .block(VIDEO, &[97, 16])
            .block(VENDOR, &HDMI_14)
            .block(VENDOR, &FORUM)
            .build(),
    ]
}

fn records() -> Records<Record> {
    Records::default()
}

fn open(model: &Model, variant: usize) -> Tx {
    model
        .open(VARIANTS[variant], false, &records())
        .expect("opened")
}

fn bring_up(tx: &mut Tx, timing: &Timing) {
    tx.configure(timing).expect("configured");
    tx.prepare().expect("prepared");
    tx.enable().expect("enabled");
}

/// `value` placed at bit `shift`.
const fn at(value: u32, shift: u32) -> u32 {
    value << shift
}

/// The words of `InfoFrame` type `0x80 + id`'s packet slot.
fn slot(model: &Model, id: usize) -> Vec<u32> {
    (0..packet::STRIDE / 4)
        .map(|word| model.reg(Window::Packet, id * packet::STRIDE + word * 4))
        .collect()
}

#[test]
fn a_block_left_without_its_clock_is_never_reached_until_it_runs() {
    let model = Model::new(0);
    let tx = open(&model, 0);
    assert_eq!(model.stalls(), 0);
    assert!(!tx.connector().connected);
    assert_eq!(tx.phase(), Phase::Off);
    let state = model.0.borrow();
    assert_eq!(
        state.hsm.asked,
        [120_000_000, 0],
        "floored to read, released after"
    );
    assert!(
        state.posted.iter().all(Option::is_some),
        "both lines watched"
    );
}

#[test]
fn an_hdmi_monitor_is_found_with_its_preferred_mode() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let tx = open(&model, 0);
    let connector = tx.connector();
    assert!(connector.connected);
    assert_eq!(connector.mode, Some(FULL_HD));
    assert_eq!(connector.link, 0);
}

#[test]
fn a_mode_set_programs_clocks_phy_timing_colour_and_infoframes() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    bring_up(&mut tx, &FULL_HD);
    assert_eq!(model.stalls(), 0);
    {
        let state = model.0.borrow();
        assert_eq!(
            state.hsm.asked.last(),
            Some(&149_985_000),
            "one percent over"
        );
        assert_eq!(state.bvb.asked, [75_000_000]);
    }
    assert_eq!(
        model.reg(Window::Phy, phy::CLK_DIV),
        3 << 8,
        "three tens to the band"
    );
    assert_eq!(model.reg(Window::Phy, phy::TMDS_CLK_WORD_SEL), 0);
    assert_eq!(model.reg(Window::Phy, phy::CHANNEL_SWAP), 0x3210);
    assert_eq!(
        model.reg(Window::Core, core::HORZA),
        core::HORZA_VPOS | core::HORZA_HPOS | at(88, 16) | at(1920, 0)
    );
    assert_eq!(
        model.reg(Window::Core, core::HORZB),
        at(148, 16) | at(44, 0)
    );
    assert_eq!(
        model.reg(Window::Core, core::VERTA0),
        at(5, 24) | at(4, 16) | at(1080, 0)
    );
    assert_eq!(model.reg(Window::Core, core::VERTB0), 36);
    assert_eq!(
        model.reg(Window::Core, core::VERTB1),
        at(1100, 16) | at(36, 0)
    );
    assert_eq!(model.reg(Window::Dvp, dvp::CLOCK_STOP), 0);
    // Code 16 is a consumer timing: limited range, 0.8594 and 16.
    assert_eq!(model.reg(Window::Csc, csc::COEFFICIENTS[0]), 0x1B80);
    assert_eq!(model.reg(Window::Csc, csc::COEFFICIENTS[1]), 0x0400 << 16);
    assert_eq!(
        model.reg(Window::Dvp, dvp::VEC_INTERFACE_XBAR),
        dvp::XBAR_RGB
    );
    let video = model.reg(Window::Hd, HDMI0_VID_CTL);
    assert_ne!(video & hd::VID_CTL_ENABLE, 0);
    assert_eq!(
        video & (hd::VID_CTL_BLANKPIX | hd::VID_CTL_HSYNC_LOW | hd::VID_CTL_VSYNC_LOW),
        0
    );
    assert_ne!(
        model.reg(Window::Core, core::SCHEDULER_CONTROL) & core::SCHEDULER_MODE_HDMI,
        0
    );
    assert_eq!(
        model.reg(Window::Core, core::RAM_PACKET_CONFIG),
        core::RAM_PACKET_ENABLE | 1 << 2,
        "the AVI frame alone"
    );
    let avi = Avi {
        code: Some(VideoCode::Cta {
            vic: 16,
            aspect: Aspect::SixteenNine,
        }),
        quantization: Quantization::Limited,
    }
    .pack();
    let b = avi.as_bytes();
    assert_eq!(b.len(), 17);
    let mut expected = vec![
        u32::from_le_bytes([b[0], b[1], b[2], 0]),
        u32::from_le_bytes([b[3], b[4], b[5], b[6]]),
        u32::from_le_bytes([b[7], b[8], b[9], 0]),
        u32::from_le_bytes([b[10], b[11], b[12], b[13]]),
        u32::from_le_bytes([b[14], b[15], b[16], 0]),
    ];
    expected.resize(packet::STRIDE / 4, 0);
    assert_eq!(
        slot(&model, 2),
        expected,
        "three bytes, four, three, four, the rest zero"
    );
    assert_eq!(model.reg(Window::Core, core::SCRAMBLER_CTL), 0);
    assert_eq!(tx.deadline_us(), None);
}

#[test]
fn a_dvi_monitor_is_sent_full_range_and_no_infoframes() {
    let model = Model::new(0);
    model.plug(&dvi_monitor());
    let mut tx = open(&model, 0);
    bring_up(&mut tx, &FULL_HD);
    assert_eq!(model.reg(Window::Core, core::RAM_PACKET_CONFIG), 0);
    assert_eq!(
        model.reg(Window::Core, core::SCHEDULER_CONTROL) & core::SCHEDULER_MODE_HDMI,
        0
    );
    assert_eq!(
        model.reg(Window::Csc, csc::COEFFICIENTS[0]),
        0x2000,
        "unity"
    );
    assert_eq!(model.reg(Window::Csc, csc::COEFFICIENTS[1]), 0);
}

#[test]
fn a_4k_television_at_60_hz_is_scrambled_and_checked_until_it_sees_it() {
    let model = Model::new(0);
    model.plug(&uhd_television());
    let records = records();
    let mut tx = model.open(VARIANTS[0], false, &records).expect("opened");
    assert_eq!(tx.connector().mode, Some(UHD_60));
    bring_up(&mut tx, &UHD_60);
    assert_eq!(model.stalls(), 0);
    assert_eq!(
        model.0.borrow().sink.scdc[0x20],
        0b11,
        "a quarter clock, scrambled"
    );
    assert_eq!(
        model.reg(Window::Core, core::SCRAMBLER_CTL),
        core::SCRAMBLER_ENABLE
    );
    assert_eq!(model.reg(Window::Phy, phy::TMDS_CLK_WORD_SEL), 3);
    assert_eq!(model.0.borrow().bvb.asked, [300_000_000]);
    let first = tx.deadline_us().expect("a check due");
    // The sink has not seen it yet, and has forgotten what it was told.
    {
        let mut state = model.0.borrow_mut();
        state.sink.scdc[0x20] = 0;
        state.now_us = first;
    }
    assert_eq!(tx.expired(), Ok(false));
    assert_eq!(model.0.borrow().sink.scdc[0x20], 0b11, "told again");
    assert!(tx.deadline_us().is_some_and(|next| next > first));
    assert_eq!(records.all(), [Record::Unscrambled]);
    model.0.borrow_mut().sink.scdc[0x21] = 1;
    assert_eq!(tx.expired(), Ok(false));
    assert_eq!(tx.deadline_us(), None, "seen: no further check");
    assert_eq!(records.all().len(), 1);
}

#[test]
fn hdmi1_stops_at_hdmi_1_4_rates_and_its_lanes_are_swapped() {
    let model = Model::new(0);
    model.plug(&uhd_television());
    let mut tx = open(&model, 1);
    assert_eq!(
        tx.connector().mode,
        Some(FULL_HD),
        "4K at 60 Hz is past 340 MHz"
    );
    bring_up(&mut tx, &FULL_HD);
    assert_eq!(
        model.reg(Window::Phy, phy::CHANNEL_SWAP),
        at(1, 0) | at(0, 4) | at(3, 8) | at(2, 12)
    );
    assert_ne!(model.reg(Window::Hd, 0x48) & hd::VID_CTL_ENABLE, 0);
    assert_eq!(
        model.reg(Window::Hd, HDMI0_VID_CTL),
        0,
        "HDMI0's control untouched"
    );
}

#[test]
fn the_engine_s_pipe_bounds_the_mode_and_moves_the_connector() {
    let model = Model::new(0);
    model.plug(&uhd_television());
    let mut tx = open(&model, 0);
    let pipe = PipeLimits {
        max_clock_khz: 300_000,
        max_active_khz: 300_000,
    };
    assert_eq!(tx.limit(&pipe), Ok(true));
    assert_eq!(tx.connector().mode, Some(FULL_HD));
    assert_eq!(tx.limit(&pipe), Ok(false), "the same bound moves nothing");
}

#[test]
fn a_sink_that_answers_late_is_read_and_one_that_never_does_is_given_the_fallback() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    model.0.borrow_mut().sink.slow = 3;
    assert_eq!(
        open(&model, 0).connector().mode,
        Some(FULL_HD),
        "the fourth try"
    );

    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    model.0.borrow_mut().sink.slow = 4;
    let records = records();
    let mut tx = model.open(VARIANTS[0], false, &records).expect("opened");
    let mode = tx.connector().mode.expect("the fallback");
    assert_eq!(
        (mode.horizontal.active, mode.vertical.active, mode.clock_khz),
        (1024, 768, 65_000)
    );
    assert_eq!(records.all(), [Record::Unreadable]);
    bring_up(&mut tx, &mode);
    assert_eq!(
        model.reg(Window::Core, core::RAM_PACKET_CONFIG),
        0,
        "sent as DVI"
    );
    let video = model.reg(Window::Hd, HDMI0_VID_CTL);
    assert_eq!(
        video & (hd::VID_CTL_HSYNC_LOW | hd::VID_CTL_VSYNC_LOW),
        hd::VID_CTL_HSYNC_LOW | hd::VID_CTL_VSYNC_LOW,
        "both syncs negative"
    );
}

#[test]
fn a_third_block_is_read_through_the_segment_pointer() {
    let model = Model::new(0);
    model.plug(&[
        Base::new().descriptor(0, dtd_1080p()).extensions(2).build(),
        Cta::new().block(VIDEO, &[16 | 0x80]).build(),
        Cta::new().block(VENDOR, &HDMI_14).build(),
    ]);
    let mut tx = open(&model, 0);
    assert_eq!(model.0.borrow().sink.segments, [1]);
    bring_up(&mut tx, &FULL_HD);
    assert_ne!(
        model.reg(Window::Core, core::RAM_PACKET_CONFIG),
        0,
        "the third block made it an HDMI sink"
    );
}

#[test]
fn a_hotplug_rereads_the_connector_and_watches_the_line_again() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    assert_eq!(tx.service_event(0), Ok(false), "nothing fired");
    model.unplug();
    model.fire(1);
    assert_eq!(tx.service_event(0), Ok(true));
    assert!(!tx.connector().connected);
    assert_eq!(tx.connector().mode, None);
    let state = model.0.borrow();
    assert!(state.posted.iter().all(Option::is_some));
    assert!(!state.hsm.held, "the clock let go again");
    assert_eq!(state.stalls, 0);
}

/// A read of the connector its clock refused leaves no hold behind for the
/// clock's next holder to inherit.
#[test]
fn a_refused_connector_read_leaves_no_hold_behind() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    model.0.borrow_mut().hsm.refuses = Some(Errno::Busy);
    model.fire(0);
    assert!(tx.service_event(0).is_err());
    model.0.borrow_mut().hsm.refuses = None;
    tairix_abi::driver::audio::Audio::configure(
        &mut tx,
        0,
        &ConfigureParams {
            endpoint: 0,
            rate: Rate::HZ_48000,
            format: SampleFormat::S24In32,
            channel_map: ChannelMap::STEREO,
            period_frames: 256,
        },
    )
    .expect("configured");
    tairix_abi::driver::audio::Audio::release(&mut tx, 0).expect("released");
    let state = model.0.borrow();
    assert!(!state.hsm.held, "nothing holds the clock");
    assert_eq!(state.stalls, 0);
}

#[test]
fn a_sink_replugged_under_a_scrambled_link_has_lost_it() {
    let model = Model::new(0);
    model.plug(&uhd_television());
    let records = records();
    let mut tx = model.open(VARIANTS[0], false, &records).expect("opened");
    bring_up(&mut tx, &UHD_60);
    // Out and in again before the removal is read: its SCDC is reset.
    model.plug(&uhd_television());
    model.fire(0);
    model.fire(1);
    assert_eq!(tx.service_event(0), Ok(true));
    let connector = tx.connector();
    assert_eq!((connector.mode, connector.link), (Some(UHD_60), 1));
    assert_eq!(records.all(), [Record::LinkLost { link: 1 }]);
}

#[test]
fn a_configure_the_pixel_clock_refuses_leaves_the_block_powered_down() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    model.0.borrow_mut().bvb.refuses = Some(Errno::Busy);
    assert_eq!(tx.configure(&FULL_HD), Err(DriverError::Busy));
    let state = model.0.borrow();
    assert!(!state.hsm.held && !state.bvb.held);
    assert_eq!(state.stalls, 0);
}

#[test]
fn an_enable_the_scheduler_never_takes_is_undone() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    tx.configure(&FULL_HD).expect("configured");
    tx.prepare().expect("prepared");
    model.0.borrow_mut().scheduler_stuck = true;
    assert_eq!(tx.enable(), Err(DriverError::DeviceFault));
    assert_eq!(model.reg(Window::Hd, HDMI0_VID_CTL) & hd::VID_CTL_ENABLE, 0);
    assert_eq!(model.reg(Window::Core, core::RAM_PACKET_CONFIG), 0);
}

#[test]
fn powering_down_lets_both_clocks_go_and_the_next_hotplug_holds_one_only_to_read() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    bring_up(&mut tx, &FULL_HD);
    tx.disable().expect("disabled");
    tx.power_down().expect("powered down");
    assert_ne!(
        model.reg(Window::Hd, HDMI0_VID_CTL) & hd::VID_CTL_BLANKPIX,
        0
    );
    assert_eq!(model.reg(Window::Hd, HDMI0_VID_CTL) & hd::VID_CTL_ENABLE, 0);
    assert_eq!(model.reg(Window::Phy, phy::RESET_CTL), phy::RESET_LANES);
    assert_eq!(
        model.reg(Window::Phy, phy::POWERDOWN_CTL),
        phy::POWERDOWN_ALL
    );
    assert_ne!(
        model.reg(Window::Dvp, dvp::CLOCK_STOP) & dvp::CLOCK_STOP_PIXEL,
        0
    );
    {
        let state = model.0.borrow();
        assert!(!state.hsm.held && !state.bvb.held);
        assert_eq!(state.bvb.asked, [75_000_000, 0]);
    }
    model.fire(0);
    assert_eq!(tx.service_event(0), Ok(false), "the same sink");
    let state = model.0.borrow();
    assert_eq!(
        state.hsm.asked[state.hsm.asked.len() - 2..],
        [120_000_000, 0]
    );
    assert_eq!(state.stalls, 0);
}

#[test]
fn an_output_the_firmware_left_running_keeps_its_clock_until_brought_down() {
    let model = Model::new(163_682_864);
    model.set(Window::Hd, HDMI0_VID_CTL, hd::VID_CTL_ENABLE);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    assert_eq!(tx.phase(), Phase::Enabled);
    assert_eq!(
        model.0.borrow().hsm.asked,
        [163_682_864],
        "held at its rate"
    );
    tx.disable().expect("disabled");
    tx.power_down().expect("powered down");
    let state = model.0.borrow();
    assert!(!state.hsm.held);
    assert!(
        state.bvb.asked.is_empty(),
        "the firmware's pixel clock was never ours"
    );
    assert_eq!(state.stalls, 0);
}

#[test]
fn a_board_sharing_the_radio_band_keeps_the_link_out_of_it() {
    // 2560x1440 at 60 Hz, reduced blanking: 241.5 MHz, inside channel 1.
    let qhd = || {
        vec![
            Base::new()
                .descriptor(
                    0,
                    dtd(
                        241_500,
                        [2560, 48, 32, 80],
                        [1440, 3, 5, 33],
                        0x1A,
                        (600, 340),
                    ),
                )
                .extensions(1)
                .build(),
            Cta::new().block(VENDOR, &HDMI_14).build(),
        ]
    };
    let model = Model::new(0);
    model.plug(&qhd());
    let tx = model.open(VARIANTS[0], true, &records()).expect("opened");
    assert_eq!(
        tx.connector().mode.map(|mode| mode.clock_khz),
        Some(238_560)
    );
    let model = Model::new(0);
    model.plug(&qhd());
    assert_eq!(
        open(&model, 0).connector().mode.map(|mode| mode.clock_khz),
        Some(241_500)
    );
}

#[test]
fn a_4k_timing_an_hdmi_1_4_sink_names_is_sent_by_its_hdmi_code() {
    let model = Model::new(0);
    model.plug(&[
        Base::new()
            .descriptor(0, uhd_dtd(297_000))
            .extensions(1)
            .build(),
        Cta::new()
            .block(VIDEO, &[16])
            .block(
                VENDOR,
                &[0x03, 0x0C, 0x00, 0x10, 0x00, 0x00, 68, 0x20, 0x00, 0x20, 1],
            )
            .build(),
    ]);
    let mut tx = open(&model, 0);
    let mode = tx.connector().mode.expect("4K at 30 Hz");
    assert_eq!(mode.clock_khz, 297_000);
    bring_up(&mut tx, &mode);
    assert_eq!(
        model.reg(Window::Core, core::RAM_PACKET_CONFIG),
        core::RAM_PACKET_ENABLE | 1 << 2 | 1 << 1,
        "the AVI and the vendor frames"
    );
    let vendor = tairix_edid::hdmi_vendor(VideoCode::Hdmi(1)).expect("an HDMI code");
    let b = vendor.as_bytes();
    assert_eq!(
        slot(&model, 1)[..2],
        [
            u32::from_le_bytes([b[0], b[1], b[2], 0]),
            u32::from_le_bytes([b[3], b[4], b[5], b[6]]),
        ]
    );
    let avi = slot(&model, 2);
    assert_eq!(avi[2].to_le_bytes()[0], 0, "no CTA code beside an HDMI one");
    assert_eq!(
        model.reg(Window::Csc, csc::COEFFICIENTS[0]),
        0x1B80,
        "limited range"
    );
}

#[test]
fn a_dvi_sink_is_held_to_a_single_link() {
    let model = Model::new(0);
    model.plug(&[Base::new()
        .descriptor(
            0,
            dtd(
                241_500,
                [2560, 48, 32, 80],
                [1440, 3, 5, 33],
                0x1A,
                (600, 340),
            ),
        )
        .descriptor(1, dtd_1080p())
        .build()]);
    assert_eq!(
        open(&model, 0).connector().mode,
        Some(FULL_HD),
        "165 MHz at most"
    );
}

#[test]
fn a_mode_with_an_odd_part_of_its_line_is_passed_over() {
    let model = Model::new(0);
    model.plug(&[Base::new()
        .descriptor(
            0,
            dtd(
                85_500,
                [1366, 70, 143, 213],
                [768, 3, 3, 24],
                SYNC_POSITIVE,
                (0, 0),
            ),
        )
        .descriptor(1, dtd_720p())
        .build()]);
    let mode = open(&model, 0).connector().mode.expect("a mode");
    assert_eq!(mode.horizontal.active, 1280);
}

#[test]
fn an_interlaced_mode_is_timed_by_its_fields() {
    let model = Model::new(0);
    model.plug(&[Base::new()
        .descriptor(
            0,
            dtd(
                74_250,
                [1920, 88, 44, 148],
                [540, 2, 5, 15],
                SYNC_POSITIVE | 0x80,
                (0, 0),
            ),
        )
        .build()]);
    let mut tx = open(&model, 0);
    let mode = tx.connector().mode.expect("1080i");
    assert_eq!(mode.scan, Scan::Interlaced);
    tx.configure(&mode).expect("configured");
    assert_eq!(
        model.reg(Window::Core, core::VERTA0),
        at(5, 24) | at(2, 16) | at(540, 0)
    );
    assert_eq!(model.reg(Window::Core, core::VERTB0), 15);
    assert_eq!(
        model.reg(Window::Core, core::VERTB1),
        at(1100, 16) | at(16, 0),
        "the odd field a line later"
    );
}

#[test]
fn a_mode_other_than_the_connector_s_is_stale() {
    let model = Model::new(0);
    model.plug(&hdmi_monitor());
    let mut tx = open(&model, 0);
    assert_eq!(tx.configure(&UHD_60), Err(DriverError::Stale));
    assert!(!model.0.borrow().hsm.held, "nothing set up");
}

#[test]
fn the_manifest_requests_the_calls_its_links_take() {
    for cap in [
        tairix_abi::CapabilityId::IPC_ENDPOINT,
        tairix_abi::CapabilityId::SHM,
        tairix_abi::CapabilityId::MMIO_MAP,
    ] {
        assert!(crate::REQUIRED_CAPABILITIES.contains(&cap));
    }
}
