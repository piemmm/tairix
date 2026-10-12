//! The engine over the register model, its encoders answered by the real
//! `encoder-v1` server, so every step it takes meets the order the encoder
//! holds it to.

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::display::{
    DamageRect, Display, DisplayFormat, DisplayMode, DisplayPower, Scan, Timing,
};
use tairix_abi::driver::encoder::{Phase, PipeLimits};
use tairix_abi::seat::ReleaseSurface;
use tairix_abi::DriverError;
use tairix_drvrt::testkit::Records;
use tairix_inline::ArrayVec;

use crate::dlist::Surface;
use crate::engine::{pipe_limits, Engine, Parts, Record};
use crate::hvs::{slot, List, KERNEL_AT};
use crate::model::{
    axis, Block, Core, Encoders, Link, Model, ModelDelay, ModelFrames, FULL_HD, HD, UHD,
};
use crate::regs::{hvs, pv};
use crate::{Load, VALVES};

type Hvs = Engine<Block, Block, Link, ModelFrames, ModelDelay, Records<Record>>;

const CONSOLE: Surface = Surface {
    bus: 0xFE00_0000,
    stride: 4096,
    width: 1024,
    height: 768,
    format: DisplayFormat::Rgba8888,
};

/// The core clock as the firmware runs it with `hdmi_enable_4kp60`.
const CORE_4KP60: u64 = 550_000_000;

struct Rig {
    model: Model,
    frames: ModelFrames,
    delay: ModelDelay,
    core: Core,
    hdmi0: Encoders,
    hdmi1: Encoders,
    records: Records<Record>,
}

impl Rig {
    fn new() -> Self {
        Self {
            model: Model::new(),
            frames: ModelFrames::default(),
            delay: ModelDelay::default(),
            core: Core::new(500_000_000, CORE_4KP60),
            hdmi0: Encoders::new(40, true, FULL_HD, Phase::Off),
            hdmi1: Encoders::new(41, false, HD, Phase::Off),
            records: Records::default(),
        }
    }

    fn open(&self) -> Result<Hvs, DriverError> {
        let mut valves = ArrayVec::new();
        for (index, (kind, encoder)) in VALVES
            .into_iter()
            .zip([&self.hdmi0, &self.hdmi1])
            .enumerate()
        {
            valves
                .try_push((self.model.valve(index), kind, encoder.client()))
                .unwrap_or_else(|_| unreachable!());
        }
        Engine::open(Parts {
            hvs: self.model.hvs(),
            valves,
            clock: self.core.client(),
            console: Some(CONSOLE),
            alloc: self.frames.clone(),
            delay: self.delay.clone(),
            log: self.records.clone(),
        })
    }

    fn records(&self) -> Vec<Record> {
        self.records.all()
    }

    /// The firmware left HDMI0 sending from channel 2.
    fn firmware_running(&self) {
        self.model.set(Some(0), pv::CONTROL, pv::CONTROL_EN);
        self.model.set(Some(0), pv::V_CONTROL, pv::VCONTROL_VIDEN);
        self.model
            .set(None, hvs::DISPEOLN, 2 << hvs::DISPEOLN_DSP4_MUX_SHIFT);
        self.model
            .set(None, hvs::dispctrlx(2), hvs::DISPCTRLX_ENABLE);
    }
}

fn mode(width: u32, height: u32) -> DisplayMode {
    DisplayMode {
        width_px: width,
        height_px: height,
        stride_bytes: (width * 4).next_multiple_of(64),
        format: DisplayFormat::Bgra8888,
    }
}

/// A whole frame of `mode` filled with `byte`.
fn frame(mode: &DisplayMode, byte: u8) -> Vec<u8> {
    vec![byte; (mode.stride_bytes * mode.height_px) as usize]
}

/// Paint `rect` of `frame` with `byte`.
fn paint(frame: &mut [u8], mode: &DisplayMode, rect: &DamageRect, byte: u8) {
    for row in rect.y..rect.y + rect.height_px {
        let at = (row * mode.stride_bytes + rect.x * 4) as usize;
        frame[at..at + rect.width_px as usize * 4].fill(byte);
    }
}

/// Two sixteen-bit fields in one word, `high` above `low`.
const fn pair(high: u32, low: u32) -> u32 {
    (high << 16) | low
}

const fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
    DamageRect {
        x,
        y,
        width_px,
        height_px,
    }
}

/// The frame the HVS reads for `channel` now, once its next frame starts.
fn shown(rig: &Rig, channel: usize) -> Option<Vec<u8>> {
    rig.model.frame(channel);
    rig.model
        .scanned(channel)
        .filter(|&bus| bus != CONSOLE.bus)
        .map(|bus| rig.frames.seen(bus))
}

#[test]
fn a_running_firmware_pipe_comes_down_in_order_before_the_engine_sets_its_mode() {
    let rig = Rig {
        hdmi0: Encoders::new(40, true, FULL_HD, Phase::Enabled),
        ..Rig::new()
    };
    rig.firmware_running();
    let engine = rig.open().expect("opened");
    assert_eq!(
        rig.hdmi0.steps(),
        ["disable", "power_down", "configure", "prepare", "enable"],
        "the firmware's output down, then the engine's up"
    );
    assert!(rig.records().contains(&Record::TookOver {
        valve: VALVES[0].window
    }));
    assert!(
        rig.delay.slept_us.get() >= 20_000,
        "the valve's last pixels drained"
    );
    let writes = rig.model.writes();
    let stopped = writes
        .iter()
        .position(|&(block, offset, value)| {
            block.is_none() && offset == hvs::dispctrlx(2) && value == hvs::DISPCTRLX_RESET
        })
        .expect("the firmware's channel stopped");
    let video_off = writes
        .iter()
        .position(|&(block, offset, value)| {
            block == Some(0) && offset == pv::V_CONTROL && value & pv::VCONTROL_VIDEN == 0
        })
        .expect("its video stopped");
    assert!(video_off < stopped, "the valve stops before its channel");
    assert_eq!(engine.output(), Some(mode(1920, 1080)));
    assert_eq!(
        rig.model.read(None, hvs::DISPEOLN) >> hvs::DISPEOLN_DSP4_MUX_SHIFT,
        0,
        "HDMI0 fed from channel 0"
    );
    assert_eq!(
        rig.model.read(None, hvs::DISPDITHER) >> hvs::DISPDITHER_DSP5_MUX_SHIFT,
        hvs::MUX_DISABLED,
        "HDMI1 has no sink"
    );
    assert!(rig.model.read(Some(0), pv::CONTROL) & pv::CONTROL_EN != 0);
    assert!(rig.model.read(Some(0), pv::V_CONTROL) & pv::VCONTROL_VIDEN != 0);
    assert!(rig.records().contains(&Record::Up {
        encoder: VALVES[0].encoder,
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000
    }));
}

#[test]
fn the_hvs_is_set_up_with_the_kernel_and_the_composite_buffer_divided() {
    let rig = Rig::new();
    let _engine = rig.open().expect("opened");
    let control = rig.model.read(None, hvs::DISPCTRL);
    assert!(control & hvs::DISPCTRL_ENABLE != 0);
    for channel in 0..hvs::CHANNELS {
        assert_eq!(
            control & hvs::dispctrl_dspeieof(channel),
            0,
            "no interrupt the engine ignores"
        );
        let (base, top) = hvs::COB[channel];
        assert_eq!(
            rig.model.read(None, hvs::dispbase(channel)),
            pair(top, base)
        );
    }
    let kernel = crate::dlist::kernel();
    for (at, word) in kernel.iter().enumerate() {
        assert_eq!(rig.model.list_word(KERNEL_AT + at), *word);
    }
}

#[test]
fn the_valve_clocks_the_mode_two_pixels_at_a_time() {
    let rig = Rig::new();
    let _engine = rig.open().expect("opened");
    let valve = |offset| rig.model.read(Some(0), offset);
    assert_eq!(
        valve(pv::HORZA),
        pair(74, 22),
        "back porch and sync, halved"
    );
    assert_eq!(
        valve(pv::HORZB),
        pair(44, 960),
        "front porch and active, halved"
    );
    assert_eq!(valve(pv::VERTA), pair(36, 5));
    assert_eq!(valve(pv::VERTB), pair(4, 1080));
    let control = valve(pv::CONTROL);
    let level = 256 - 18;
    assert_eq!(control >> pv::CONTROL_FIFO_LEVEL_SHIFT & 0x3F, level & 0x3F);
    assert_eq!(control >> pv::CONTROL_FIFO_LEVEL_HIGH_SHIFT & 3, level >> 6);
    assert_eq!(valve(pv::MUX_CFG), pv::MUX_CFG_NO_SWAP);
}

#[test]
fn an_interlaced_mode_clocks_two_fields() {
    let rig = Rig::new();
    let interlaced = Timing {
        clock_khz: 74_250,
        horizontal: axis(1920, 88, 44, 148),
        vertical: axis(1080, 4, 10, 31),
        scan: Scan::Interlaced,
        ..FULL_HD
    };
    let rig = Rig {
        hdmi0: Encoders::new(40, true, interlaced, Phase::Off),
        ..rig
    };
    let _engine = rig.open().expect("opened");
    let valve = |offset| rig.model.read(Some(0), offset);
    // Field edges: active 540, sync from 542 to 547, total 562.
    assert_eq!(valve(pv::VERTA_EVEN), pair(15, 5));
    assert_eq!(valve(pv::VERTB_EVEN), pair(2, 540));
    assert_eq!(
        valve(pv::VERTA),
        pair(16, 5),
        "the odd field a line further back"
    );
    let vertical = valve(pv::V_CONTROL);
    assert!(vertical & pv::VCONTROL_INTERLACE != 0);
    assert_eq!(
        vertical >> pv::VCONTROL_ODD_DELAY_SHIFT & 0x1_FFFF,
        550,
        "half a line late"
    );
    assert!(rig.model.read(None, hvs::dispbkgnd(0)) & hvs::DISPBKGND_INTERLACE != 0);
}

#[test]
fn a_line_the_valve_cannot_clock_two_pixels_at_a_time_fails_the_output() {
    let rig = Rig::new();
    let odd = Timing {
        horizontal: axis(1366, 70, 143, 213),
        ..FULL_HD
    };
    let rig = Rig {
        hdmi0: Encoders::new(40, true, odd, Phase::Off),
        ..rig
    };
    let engine = rig.open().expect("opened");
    assert!(rig.records().contains(&Record::Failed {
        encoder: VALVES[0].encoder,
        reason: DriverError::Unsupported
    }));
    assert_eq!(rig.model.read(Some(0), pv::CONTROL) & pv::CONTROL_EN, 0);
    assert_eq!(
        rig.hdmi0.steps(),
        ["configure", "power_down"],
        "brought back down in order"
    );
    assert_eq!(engine.output(), None, "no output it can drive");
}

#[test]
fn a_present_is_scanned_out_whole_and_the_next_never_writes_the_frame_shown() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let first = frame(&mode, 0x11);
    engine.present(&first).expect("presented");
    assert_eq!(shown(&rig, 0).as_deref(), Some(&first[..]));

    let mut second = first.clone();
    let corner = rect(0, 0, 64, 64);
    paint(&mut second, &mode, &corner, 0x22);
    engine.present_rects(&second, &[corner]).expect("presented");
    let shown_before = rig.model.scanned(0);
    assert_eq!(shown(&rig, 0).as_deref(), Some(&second[..]));
    assert_ne!(
        rig.model.scanned(0),
        shown_before,
        "a frame other than the one shown"
    );

    // Two presents inside one frame: the latest wins, the frame shown is
    // never written.
    let mut third = second.clone();
    let band = rect(100, 500, 800, 20);
    paint(&mut third, &mode, &band, 0x33);
    let scanned = rig.model.scanned(0).expect("a frame");
    let before = rig.frames.seen(scanned);
    engine.present_rects(&third, &[band]).expect("presented");
    let mut fourth = third.clone();
    paint(&mut fourth, &mode, &corner, 0x44);
    engine.present_rects(&fourth, &[corner]).expect("presented");
    assert_eq!(
        rig.frames.seen(scanned),
        before,
        "the frame being shown was not touched"
    );
    assert_eq!(shown(&rig, 0).as_deref(), Some(&fourth[..]));
    assert_eq!(
        rig.frames.frames.borrow().len(),
        3,
        "one shown, one latched, one written"
    );
}

#[test]
fn a_present_with_no_output_or_too_short_a_frame_is_refused() {
    let rig = Rig {
        hdmi0: Encoders::new(40, false, FULL_HD, Phase::Off),
        ..Rig::new()
    };
    let mut engine = rig.open().expect("opened");
    assert_eq!(engine.output(), None);
    assert_eq!(engine.present(&[0; 16]), Err(DriverError::Stale));
    rig.hdmi0.plug(true, FULL_HD);
    engine.service_events().expect("serviced");
    let mode = engine.output().expect("an output");
    assert_eq!(
        engine.present(&frame(&mode, 1)[..1000]),
        Err(DriverError::BufferTooSmall)
    );
    assert!(
        rig.frames.frames.borrow().is_empty(),
        "nothing allocated for it"
    );
}

#[test]
fn a_departing_presenter_s_frames_are_never_shown_to_the_next() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    engine.present(&frame(&mode, 0x55)).expect("presented");
    assert!(shown(&rig, 0).is_some());

    engine.released(ReleaseSurface::Handover);
    rig.model.frame(0);
    let black = u32::try_from(slot(0, List::Black)).expect("small");
    assert_eq!(
        rig.model.read(None, hvs::displact(0)),
        black,
        "black until the next frame"
    );

    // The newcomer's first present names one corner; the frame shown must
    // still be wholly its own.
    let mut next = frame(&mode, 0x66);
    let corner = rect(0, 0, 8, 8);
    paint(&mut next, &mode, &corner, 0x77);
    engine.present_rects(&next, &[corner]).expect("presented");
    assert_eq!(shown(&rig, 0).as_deref(), Some(&next[..]));
}

#[test]
fn the_seat_going_back_to_text_shows_the_console_scaled_to_the_output() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    rig.model.frame(0);
    assert_eq!(
        rig.model.scanned(0),
        Some(CONSOLE.bus),
        "the console before any present"
    );
    let mode = engine.output().expect("an output");
    engine.present(&frame(&mode, 1)).expect("presented");
    assert_ne!(
        {
            rig.model.frame(0);
            rig.model.scanned(0)
        },
        Some(CONSOLE.bus)
    );
    engine.released(ReleaseSurface::Text);
    rig.model.frame(0);
    assert_eq!(rig.model.scanned(0), Some(CONSOLE.bus));
    let at = slot(0, List::Console);
    assert_eq!(
        rig.model.list_word(at + 3),
        pair(1080, 1440),
        "1024x768 drawn 1440x1080, pillarboxed"
    );
    assert_eq!(rig.model.list_word(at + 1), 240, "centred");
}

#[test]
fn a_second_sink_mirrors_the_surface_scaled_and_presents_stay_tear_free() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    engine.present(&frame(&mode, 1)).expect("presented");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    assert_eq!(
        engine.output(),
        Some(mode),
        "the first output's mode stands"
    );
    assert_eq!(rig.hdmi1.steps(), ["configure", "prepare", "enable"]);
    let at = slot(1, List::Frame(0));
    assert_eq!(rig.model.list_word(at) & (1 << 15), 0, "scaled, not unity");
    assert_eq!(rig.model.list_word(at + 3), pair(720, 1280));

    // Each channel latches at its own frame start; a present must find a
    // frame neither reads nor latches.
    for byte in 2..8 {
        engine.present(&frame(&mode, byte)).expect("presented");
        if byte.is_multiple_of(2) {
            rig.model.frame(0);
        } else {
            rig.model.frame(1);
        }
    }
    assert!(rig.frames.frames.borrow().len() <= 4);
    let last = frame(&mode, 9);
    engine.present(&last).expect("presented");
    assert_eq!(shown(&rig, 0).as_deref(), Some(&last[..]));
    assert_eq!(shown(&rig, 1).as_deref(), Some(&last[..]));
}

#[test]
fn the_first_output_leaving_moves_the_surface_to_the_next() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    rig.hdmi0.plug(false, FULL_HD);
    engine.service_events().expect("serviced");
    assert_eq!(engine.output(), Some(mode(1280, 720)));
    assert_eq!(
        rig.hdmi1.steps(),
        [
            "configure",
            "prepare",
            "enable",
            "disable",
            "power_down",
            "configure",
            "prepare",
            "enable"
        ],
        "brought down and up again for the new surface"
    );
    assert_eq!(
        rig.model.read(Some(0), pv::CONTROL) & pv::CONTROL_EN,
        0,
        "HDMI0 dark"
    );
    rig.hdmi1.plug(false, HD);
    engine.service_events().expect("serviced");
    assert_eq!(engine.output(), None, "no output at all");
}

/// Every encoder and the core clock are reached by calls to their endpoints,
/// which the kernel refuses a caller without the authority to call one.
#[test]
fn the_manifest_requests_the_calls_to_its_links() {
    assert!(crate::REQUIRED_CAPABILITIES.contains(&tairix_abi::CapabilityId::IPC_ENDPOINT));
}

#[test]
fn a_sink_that_lost_its_link_is_set_up_again_and_the_surface_stands() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let first = frame(&mode, 3);
    engine.present(&first).expect("presented");
    rig.hdmi0.lose_link();
    engine.service_events().expect("serviced");
    assert_eq!(
        rig.hdmi0.steps(),
        [
            "configure",
            "prepare",
            "enable",
            "disable",
            "power_down",
            "configure",
            "prepare",
            "enable"
        ]
    );
    assert_eq!(engine.output(), Some(mode), "the surface kept");
    assert_eq!(
        shown(&rig, 0).as_deref(),
        Some(&first[..]),
        "the last frame still shown"
    );
    engine.service_events().expect("serviced");
    assert_eq!(rig.hdmi0.steps().len(), 8, "set up once for that loss");
}

#[test]
fn each_encoder_is_held_to_what_its_pipe_carries_at_the_core_ceiling() {
    let rig = Rig {
        hdmi0: Encoders::new(40, true, UHD, Phase::Off),
        core: Core::new(500_000_000, 500_000_000),
        ..Rig::new()
    };
    let engine = rig.open().expect("opened");
    assert_eq!(
        rig.hdmi0.sink.borrow().limits,
        Some(PipeLimits {
            max_clock_khz: 555_555,
            max_active_khz: 492_000
        })
    );
    assert_eq!(
        rig.hdmi1.sink.borrow().limits,
        Some(PipeLimits {
            max_clock_khz: 500_000,
            max_active_khz: 500_000
        })
    );
    assert_eq!(
        engine.output(),
        Some(mode(1280, 720)),
        "4K at 60 Hz outpaces a 500 MHz core"
    );

    let rig = Rig {
        hdmi0: Encoders::new(40, true, UHD, Phase::Off),
        ..Rig::new()
    };
    let engine = rig.open().expect("opened");
    assert_eq!(
        engine.output(),
        Some(mode(3840, 2160)),
        "and fits a 550 MHz one"
    );
    assert_eq!(
        rig.core.floors(),
        [534_600_000],
        "nine tenths of its clock, raised once for the mode set and kept"
    );
}

#[test]
fn the_core_clock_is_raised_for_a_mode_set_and_settles_at_what_the_outputs_need() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    // A line's active pixels and the margin: 129.6 MHz and 8 MHz more.
    assert_eq!(rig.core.floors(), [500_000_000, 137_600_000]);
    rig.hdmi0.plug(false, FULL_HD);
    engine.service_events().expect("serviced");
    assert_eq!(
        rig.core.floors().last(),
        Some(&0),
        "released with the last output"
    );
}

#[test]
fn a_display_switched_off_brings_its_outputs_down_and_back_showing_the_latest_frame() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    assert_eq!(engine.set_power(DisplayPower::Off), Ok(()));
    assert_eq!(rig.hdmi0.steps().last(), Some(&"power_down"));
    assert_eq!(engine.output(), Some(mode), "the output stands while dark");
    let latest = frame(&mode, 0x5A);
    engine.present(&latest).expect("a present is still taken");
    assert_eq!(engine.set_power(DisplayPower::Off), Ok(()), "no change");
    assert_eq!(engine.set_power(DisplayPower::On), Ok(()));
    assert_eq!(rig.hdmi0.steps().last(), Some(&"enable"));
    assert_eq!(shown(&rig, 0).as_deref(), Some(&latest[..]));
}

#[test]
fn a_refused_mode_set_leaves_the_output_dark_until_its_connector_moves() {
    let rig = Rig::new();
    rig.hdmi0.sink.borrow_mut().refuses = Some(DriverError::DeviceFault);
    let mut engine = rig.open().expect("opened");
    assert!(rig.records().contains(&Record::Failed {
        encoder: VALVES[0].encoder,
        reason: DriverError::DeviceFault
    }));
    assert_eq!(rig.model.read(Some(0), pv::CONTROL) & pv::CONTROL_EN, 0);
    engine.service_events().expect("serviced");
    assert!(rig.hdmi0.steps().is_empty(), "no retry while nothing moved");
    rig.hdmi0.sink.borrow_mut().refuses = None;
    rig.hdmi0.plug(true, FULL_HD);
    engine.service_events().expect("serviced");
    assert_eq!(rig.hdmi0.steps(), ["configure", "prepare", "enable"]);
}

#[test]
fn a_valve_that_will_not_stop_still_comes_down() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    rig.model.0.borrow_mut().stuck = Some(0);
    rig.hdmi0.plug(false, FULL_HD);
    engine.service_events().expect("serviced");
    assert!(rig.records().contains(&Record::StopOverran {
        valve: VALVES[0].window
    }));
    assert_eq!(rig.hdmi0.steps().last(), Some(&"power_down"));
    assert_eq!(rig.model.read(Some(0), pv::CONTROL) & pv::CONTROL_EN, 0);
}

#[test]
fn frames_that_cannot_be_had_refuse_the_present_and_show_nothing_new() {
    let rig = Rig {
        frames: ModelFrames {
            limit: Some(1),
            ..ModelFrames::default()
        },
        ..Rig::new()
    };
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    assert_eq!(
        engine.present(&frame(&mode, 1)),
        Err(DriverError::OutOfMemory)
    );
    rig.model.frame(0);
    assert_eq!(rig.model.scanned(0), Some(CONSOLE.bus));
}

#[test]
fn an_encoder_whose_driver_ended_takes_its_output_down() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    // Its endpoint gone, the engine's posted wait is answered by the kernel.
    rig.hdmi0.gone.set(true);
    engine.service_events().expect("serviced");
    assert!(rig.records().contains(&Record::EncoderGone {
        encoder: VALVES[0].encoder
    }));
    assert_eq!(engine.output(), None);
    assert_eq!(rig.model.read(Some(0), pv::CONTROL) & pv::CONTROL_EN, 0);
    engine.service_events().expect("serviced");
    assert_eq!(
        rig.records()
            .iter()
            .filter(|record| **record
                == Record::EncoderGone {
                    encoder: VALVES[0].encoder
                })
            .count(),
        1,
        "a gone encoder is asked nothing more"
    );
}

#[test]
fn the_pipe_limits_invert_the_load_each_valve_puts_on_the_core() {
    assert_eq!(
        pipe_limits(Load::Active, CORE_4KP60),
        PipeLimits {
            max_clock_khz: 611_111,
            max_active_khz: 542_000
        }
    );
    assert_eq!(
        pipe_limits(Load::Clock, CORE_4KP60),
        PipeLimits {
            max_clock_khz: 550_000,
            max_active_khz: 550_000
        }
    );
}
