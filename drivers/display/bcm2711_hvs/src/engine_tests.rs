//! The engine over the register model, its encoders answered by the real
//! `encoder-v1` server, so every step it takes meets the order the encoder
//! holds it to.

use std::vec;
use std::vec::Vec;

use tairix_abi::driver::display::{
    AccelLayer, AcceleratedDisplay, AllocatedBuffer, BufferId, DamageRect, Display, DisplayFormat,
    DisplayMode, DisplayPower, HardwareCursor, Presented, Scan, Timing,
};
use tairix_abi::driver::encoder::{Phase, PipeLimits};
use tairix_abi::seat::ReleaseSurface;
use tairix_abi::DriverError;
use tairix_drvrt::testkit::Records;
use tairix_inline::ArrayVec;

use crate::dlist::{Surface, UNITY_POINTER, UNITY_POSITION, UNITY_SIZE};
use crate::engine::{pipe_limits, Engine, Parts, Record};
use crate::hvs::{slot, List, KERNEL_AT};
use crate::model::{
    axis, Block, Core, Encoders, Link, Model, ModelBuffers, ModelDelay, FULL_HD, HD, UHD,
};
use crate::regs::{hvs, pv};
use crate::{Load, VALVES};

type Hvs = Engine<Block, Block, Link, ModelBuffers, ModelDelay, Records<Record>>;

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
    buffers: ModelBuffers,
    budget: usize,
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
            buffers: ModelBuffers::default(),
            budget: 256 << 20,
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
            alloc: self.buffers.clone(),
            budget: self.budget,
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

/// A buffer of `mode`'s size from `engine`, filled with `byte` by its
/// presenter.
fn filled(rig: &Rig, engine: &mut Hvs, mode: &DisplayMode, byte: u8) -> AllocatedBuffer {
    let buffer = engine
        .alloc_buffer(mode.width_px, mode.height_px)
        .expect("a buffer");
    let len = (buffer.stride_bytes * mode.height_px) as usize;
    rig.buffers.draw(buffer.region, 0, &vec![byte; len]);
    buffer
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

/// The buffer's bus address the HVS reads for `channel` once its next frame
/// starts, the engine told of the frame's end.
fn shown(rig: &Rig, engine: &mut Hvs, channel: usize) -> Option<u32> {
    rig.model.next_frame(channel);
    engine.service_events().expect("serviced");
    rig.model.scanned(channel).filter(|&bus| bus != CONSOLE.bus)
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
fn a_present_scans_the_presenters_buffer_where_it_lies_once_the_frame_starts() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    assert_eq!(engine.frame_count(), 2, "one shown while the next is drawn");
    let first = filled(&rig, &mut engine, &mode, 0x11);
    let second = filled(&rig, &mut engine, &mode, 0x22);
    assert_eq!(
        engine.present(first.id, &[DamageRect::full(&mode)]),
        Ok(Presented::Queued)
    );
    assert_eq!(
        engine.present(second.id, &[DamageRect::full(&mode)]),
        Err(DriverError::Busy),
        "one present waits on the frame start at a time"
    );
    assert_ne!(
        rig.model.read(None, hvs::DISPCTRL) & hvs::dispctrl_dspeieof(0),
        0,
        "the frame's end is raised while a present waits on it"
    );
    rig.model.frame_end(0);
    assert_eq!(
        engine.service_events().map(|serviced| serviced.latched),
        Ok(false),
        "the frame that ended was the old one"
    );
    rig.model.frame(0);
    rig.model.frame_end(0);
    assert_eq!(
        engine.service_events().map(|serviced| serviced.latched),
        Ok(true)
    );
    assert_eq!(rig.model.scanned(0), Some(rig.buffers.bus_of(first.region)));
    assert_eq!(
        rig.model.read(None, hvs::DISPCTRL) & hvs::dispctrl_dspeieof(0),
        0,
        "and is raised no more once nothing waits"
    );
    engine
        .present(second.id, &[rect(0, 0, 8, 8)])
        .expect("queued");
    assert_eq!(
        rig.model.scanned(0),
        Some(rig.buffers.bus_of(first.region)),
        "the buffer shown is read until the next frame starts"
    );
    assert_eq!(
        shown(&rig, &mut engine, 0),
        Some(rig.buffers.bus_of(second.region))
    );
}

#[test]
fn a_present_with_no_output_or_a_buffer_of_another_size_is_refused() {
    let rig = Rig {
        hdmi0: Encoders::new(40, false, FULL_HD, Phase::Off),
        ..Rig::new()
    };
    let mut engine = rig.open().expect("opened");
    assert_eq!(engine.output(), None);
    let small = engine.alloc_buffer(64, 64).expect("a buffer");
    assert_eq!(
        engine.present(small.id, &[rect(0, 0, 1, 1)]),
        Err(DriverError::Stale)
    );
    rig.hdmi0.plug(true, FULL_HD);
    engine.service_events().expect("serviced");
    assert_eq!(
        engine.present(small.id, &[rect(0, 0, 1, 1)]),
        Err(DriverError::Stale),
        "a frame is the output's size"
    );
    let unknown = BufferId::new(999).expect("an id");
    assert_eq!(
        engine.present(unknown, &[rect(0, 0, 1, 1)]),
        Err(DriverError::NotFound)
    );
}

#[test]
fn a_buffer_given_back_while_a_list_names_it_is_kept_until_none_does() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let first = filled(&rig, &mut engine, &mode, 0x11);
    engine
        .present(first.id, &[DamageRect::full(&mode)])
        .expect("queued");
    let bus = rig.buffers.bus_of(first.region);
    assert_eq!(shown(&rig, &mut engine, 0), Some(bus));
    engine.free_buffer(first.id).expect("given back");
    assert!(
        rig.buffers.live(bus),
        "the HVS still reads it: its memory is not given back"
    );
    assert_eq!(
        engine.present(first.id, &[DamageRect::full(&mode)]),
        Err(DriverError::NotFound),
        "but the presenter can name it no more"
    );
    let second = filled(&rig, &mut engine, &mode, 0x22);
    engine
        .present(second.id, &[DamageRect::full(&mode)])
        .expect("queued");
    assert!(
        rig.buffers.live(bus),
        "still read until the next list latches"
    );
    assert_eq!(
        shown(&rig, &mut engine, 0),
        Some(rig.buffers.bus_of(second.region))
    );
    assert!(!rig.buffers.live(bus), "given back once no list names it");
}

#[test]
fn a_departing_presenter_s_buffers_are_never_shown_to_the_next() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let left = filled(&rig, &mut engine, &mode, 0x55);
    engine
        .present(left.id, &[DamageRect::full(&mode)])
        .expect("queued");
    assert!(shown(&rig, &mut engine, 0).is_some());

    engine.released(ReleaseSurface::Handover);
    engine.free_buffer(left.id).expect("given back");
    let bus = rig.buffers.bus_of(left.region);
    assert!(rig.buffers.live(bus), "until the black latches");
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    let black = u32::try_from(slot(0, List::Black)).expect("small");
    assert_eq!(rig.model.read(None, hvs::displact(0)), black);
    assert!(!rig.buffers.live(bus), "and never read again");
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
    let frame = filled(&rig, &mut engine, &mode, 1);
    engine
        .present(frame.id, &[DamageRect::full(&mode)])
        .expect("queued");
    assert_ne!(shown(&rig, &mut engine, 0), None);
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
fn a_second_sink_mirrors_the_surface_scaled_and_a_present_waits_for_both() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    assert_eq!(
        engine.output(),
        Some(mode),
        "the first output's mode stands"
    );
    assert_eq!(rig.hdmi1.steps(), ["configure", "prepare", "enable"]);
    let frame = filled(&rig, &mut engine, &mode, 1);
    engine
        .present(frame.id, &[DamageRect::full(&mode)])
        .expect("queued");
    let at = slot(1, List::Stack(0));
    assert_eq!(rig.model.list_word(at) & (1 << 15), 0, "scaled, not unity");
    assert_eq!(rig.model.list_word(at + 3), pair(720, 1280));

    rig.model.next_frame(0);
    assert_eq!(
        engine.service_events().map(|serviced| serviced.latched),
        Ok(false),
        "the second output has not latched it"
    );
    rig.model.next_frame(1);
    assert_eq!(
        engine.service_events().map(|serviced| serviced.latched),
        Ok(true)
    );
}

#[test]
fn a_stack_one_output_cannot_take_is_refused_before_either_is_written() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    let frame = filled(&rig, &mut engine, &mode, 1);
    engine
        .present(frame.id, &[DamageRect::full(&mode)])
        .expect("queued");
    for channel in 0..2 {
        rig.model.next_frame(channel);
    }
    engine.service_events().expect("serviced");
    // Scaled down to the second output, five layers the surface's size
    // outrun its share of the line buffer; the first shows them unscaled.
    let writes = rig.model.write_count();
    let whole = layer(frame.id, 0, 0, mode.width_px, mode.height_px);
    assert_eq!(
        engine.present_layers(&[whole; 5]),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        rig.model.write_count(),
        writes,
        "no list written, none latched"
    );
    rig.hdmi1.plug(false, HD);
    engine.service_events().expect("serviced");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    assert_eq!(rig.hdmi1.steps().last(), Some(&"enable"));
    assert_eq!(
        shown(&rig, &mut engine, 1),
        Some(rig.buffers.bus_of(frame.region)),
        "an output coming up shows the stack that stood"
    );
    assert_eq!(
        engine.present(frame.id, &[DamageRect::full(&mode)]),
        Ok(Presented::Queued)
    );
}

#[test]
fn the_core_clock_a_stack_needs_is_had_before_it_latches_and_kept_until_none_composites_it() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let base = filled(&rig, &mut engine, &mode, 1);
    let over = filled(&rig, &mut engine, &mode, 2);
    rig.core.beside(&rig.model);
    let whole = |id| layer(id, 0, 0, mode.width_px, mode.height_px);
    assert_eq!(
        engine.present_layers(&[whole(base.id), whole(over.id)]),
        Ok(Presented::Queued)
    );
    // Two layers' pixels at sixty frames, of which the core composites
    // three in five.
    assert_eq!(rig.core.floors().last(), Some(&149_299_200));
    let stack = u32::try_from(slot(0, List::Stack(0))).expect("small");
    let latch = (None, hvs::displist(0), stack);
    let latched_at = rig
        .model
        .writes()
        .iter()
        .rposition(|&write| write == latch)
        .expect("latched");
    assert!(
        rig.core.asked_at()[0] <= latched_at,
        "the clock is asked for before the channel is told to latch"
    );
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    let raised = rig.core.floors();

    engine
        .present(base.id, &[DamageRect::full(&mode)])
        .expect("queued");
    engine.service_events().expect("serviced");
    rig.model.frame_end(0);
    engine.service_events().expect("serviced");
    assert_eq!(
        rig.core.floors(),
        raised,
        "kept while the channel composites both layers"
    );
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    assert_eq!(rig.core.floors().last(), Some(&137_600_000));
}

#[test]
fn a_present_the_presenter_stopped_waiting_for_is_withdrawn_and_the_next_is_taken() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let first = filled(&rig, &mut engine, &mode, 1);
    let second = filled(&rig, &mut engine, &mode, 2);
    let full = [DamageRect::full(&mode)];
    engine.present(first.id, &full).expect("queued");
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    assert_eq!(engine.present(second.id, &full), Ok(Presented::Queued));
    assert_eq!(engine.present(first.id, &full), Err(DriverError::Busy));

    engine.abandon_queued();
    assert_eq!(
        rig.model.read(None, hvs::displist(0)),
        u32::try_from(slot(0, List::Stack(0))).expect("small"),
        "withdrawn to the list the channel composites"
    );
    let bus = rig.buffers.bus_of(second.region);
    engine.free_buffer(second.id).expect("given back");
    assert!(!rig.buffers.live(bus), "no list names it");
    rig.model.next_frame(0);
    assert_eq!(rig.model.scanned(0), Some(rig.buffers.bus_of(first.region)));
    assert_eq!(engine.present(first.id, &full), Ok(Presented::Queued));
}

/// A stack taken while the outputs were down, which one of them cannot
/// composite once it comes up, leaves that output up and showing black; the
/// presenter's next stack is built for every output.
#[test]
fn an_output_that_cannot_show_the_stack_it_comes_up_to_shows_black() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    let frame = filled(&rig, &mut engine, &mode, 1);
    assert_eq!(engine.set_power(DisplayPower::Off), Ok(()));
    let whole = layer(frame.id, 0, 0, mode.width_px, mode.height_px);
    assert_eq!(engine.present_layers(&[whole; 5]), Ok(Presented::Shown));
    assert_eq!(engine.set_power(DisplayPower::On), Ok(()));
    assert_eq!(rig.hdmi1.steps().last(), Some(&"enable"), "up, not failed");
    let black = u32::try_from(slot(1, List::Black)).expect("small");
    assert_eq!(rig.model.read(None, hvs::displist(1)), black);
    assert_eq!(engine.present_layers(&[whole]), Ok(Presented::Queued));
}

/// A stack naming a buffer its presenter has since given back shows black
/// on an output coming up, rather than failing it.
#[test]
fn an_output_coming_up_to_a_stack_whose_buffer_is_gone_shows_black() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let frame = filled(&rig, &mut engine, &mode, 1);
    engine
        .present(frame.id, &[DamageRect::full(&mode)])
        .expect("queued");
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    assert_eq!(engine.set_power(DisplayPower::Off), Ok(()));
    engine.free_buffer(frame.id).expect("given back");
    assert_eq!(engine.set_power(DisplayPower::On), Ok(()));
    assert_eq!(rig.hdmi0.steps().last(), Some(&"enable"), "up, not failed");
    let black = u32::try_from(slot(0, List::Black)).expect("small");
    assert_eq!(rig.model.read(None, hvs::displist(0)), black);
}

/// A present writes only the stack slot neither the composited nor the
/// pending list names, and waits while both are stack slots.
#[test]
fn a_present_writes_no_slot_a_channel_may_latch_and_waits_when_none_is_free() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let frame = filled(&rig, &mut engine, &mode, 1);
    let full = [DamageRect::full(&mode)];
    let stack = |index| u32::try_from(slot(0, List::Stack(index))).expect("small");
    // Composing the console, about to latch slot 0.
    rig.model.set(None, hvs::displist(0), stack(0));
    let writes = rig.model.write_count();
    assert_eq!(engine.present(frame.id, &full), Ok(Presented::Queued));
    let slot_0 = slot(0, List::Stack(0))..slot(0, List::Stack(1));
    let into_slot_0 = rig.model.writes()[writes..]
        .iter()
        .any(|&(block, offset, _)| {
            block.is_none() && offset >= hvs::DLIST && slot_0.contains(&((offset - hvs::DLIST) / 4))
        });
    assert!(!into_slot_0, "the pending list is left as it is");
    assert_eq!(rig.model.read(None, hvs::displist(0)), stack(1));

    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    rig.model.set(None, hvs::displist(0), stack(0));
    assert_eq!(
        engine.present(frame.id, &full),
        Err(DriverError::Busy),
        "both slots named until the next frame starts"
    );
}

/// A present whose last waiting output is brought down is reported latched
/// there and then, not left for the presenter's deadline.
#[test]
fn a_present_whose_waiting_output_goes_down_is_latched() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    rig.hdmi1.plug(true, HD);
    engine.service_events().expect("serviced");
    let frame = filled(&rig, &mut engine, &mode, 1);
    engine
        .present(frame.id, &[DamageRect::full(&mode)])
        .expect("queued");
    rig.model.next_frame(0);
    assert_eq!(
        engine.service_events().map(|serviced| serviced.latched),
        Ok(false)
    );
    rig.hdmi1.plug(false, HD);
    assert_eq!(
        engine.service_events().map(|serviced| serviced.latched),
        Ok(true)
    );
}

/// The clock stays where it was when a queued present is abandoned: the
/// channel goes on compositing the stack before it.
#[test]
fn an_abandoned_present_keeps_the_clock_its_predecessor_needs() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let base = filled(&rig, &mut engine, &mode, 1);
    let over = filled(&rig, &mut engine, &mode, 2);
    let whole = |id| layer(id, 0, 0, mode.width_px, mode.height_px);
    engine
        .present_layers(&[whole(base.id), whole(over.id)])
        .expect("queued");
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    engine
        .present(base.id, &[DamageRect::full(&mode)])
        .expect("queued");
    let floors = rig.core.floors();
    engine.abandon_queued();
    engine.service_events().expect("serviced");
    assert_eq!(rig.core.floors(), floors);
}

/// A floor the clock refused is not asked again, nor recorded again, until
/// another is needed.
#[test]
fn a_refused_clock_floor_is_not_asked_again_for_the_same_rate() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let base = filled(&rig, &mut engine, &mode, 1);
    let over = filled(&rig, &mut engine, &mode, 2);
    let whole = |id| layer(id, 0, 0, mode.width_px, mode.height_px);
    rig.core.refuse(true);
    let asked = rig.core.floors().len();
    engine
        .present_layers(&[whole(base.id), whole(over.id)])
        .expect("queued");
    for _ in 0..3 {
        rig.model.next_frame(0);
        engine.service_events().expect("serviced");
    }
    assert_eq!(rig.core.floors().len(), asked + 1, "asked once");
    let refused = rig
        .records()
        .iter()
        .filter(|record| matches!(record, Record::ClockRefused { .. }))
        .count();
    assert_eq!(refused, 1);
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
fn the_manifest_requests_the_calls_to_its_links_and_its_frame_ends() {
    assert!(crate::REQUIRED_CAPABILITIES.contains(&tairix_abi::CapabilityId::IPC_ENDPOINT));
    assert!(crate::REQUIRED_CAPABILITIES.contains(&tairix_abi::CapabilityId::IRQ_BIND));
}

#[test]
fn a_sink_that_lost_its_link_is_set_up_again_and_the_surface_stands() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let first = filled(&rig, &mut engine, &mode, 3);
    engine
        .present(first.id, &[DamageRect::full(&mode)])
        .expect("queued");
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
        shown(&rig, &mut engine, 0),
        Some(rig.buffers.bus_of(first.region)),
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
    let latest = filled(&rig, &mut engine, &mode, 0x5A);
    assert_eq!(
        engine.present(latest.id, &[DamageRect::full(&mode)]),
        Ok(Presented::Shown),
        "no channel reads it, so nothing waits"
    );
    assert_eq!(engine.set_power(DisplayPower::Off), Ok(()), "no change");
    assert_eq!(engine.set_power(DisplayPower::On), Ok(()));
    assert_eq!(rig.hdmi0.steps().last(), Some(&"enable"));
    rig.model.frame(0);
    assert_eq!(
        rig.model.scanned(0),
        Some(rig.buffers.bus_of(latest.region))
    );
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
fn buffers_that_cannot_be_had_are_refused_and_nothing_new_is_shown() {
    let rig = Rig {
        buffers: ModelBuffers {
            limit: Some(1),
            ..ModelBuffers::default()
        },
        ..Rig::new()
    };
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    engine
        .alloc_buffer(mode.width_px, mode.height_px)
        .expect("the one the memory holds");
    assert_eq!(
        engine.alloc_buffer(mode.width_px, mode.height_px).err(),
        Some(DriverError::OutOfMemory)
    );
    let budgeted = Rig {
        budget: 1 << 20,
        ..Rig::new()
    };
    let mut engine = budgeted.open().expect("opened");
    assert_eq!(
        engine.alloc_buffer(mode.width_px, mode.height_px).err(),
        Some(DriverError::NoSpace),
        "past what one presenter may hold"
    );
    budgeted.model.frame(0);
    assert_eq!(budgeted.model.scanned(0), Some(CONSOLE.bus));
}

/// A layer showing all of `buffer` at `(dst_x, dst_y)`, unscaled.
const fn layer(buffer: BufferId, dst_x: i32, dst_y: i32, width: u32, height: u32) -> AccelLayer {
    AccelLayer {
        buffer,
        src: rect(0, 0, width, height),
        dst_x,
        dst_y,
        dst_width_px: width,
        dst_height_px: height,
        opacity: u8::MAX,
    }
}

/// Words the plane starting at `at` takes, as its control word states.
fn plane_len(rig: &Rig, at: usize) -> usize {
    (rig.model.list_word(at) >> 24 & 0x3F) as usize
}

#[test]
fn layers_are_planes_bottom_first_cut_to_the_surface_with_the_pointer_last() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let mode = engine.output().expect("an output");
    let base = filled(&rig, &mut engine, &mode, 0x10);
    let window = engine.alloc_buffer(200, 100).expect("a layer");
    let stack = [
        layer(base.id, 0, 0, mode.width_px, mode.height_px),
        layer(window.id, -50, 20, 200, 100),
    ];
    assert_eq!(engine.present_layers(&stack), Ok(Presented::Queued));
    let at = slot(0, List::Stack(0));
    let second = at + plane_len(&rig, at);
    assert_eq!(
        rig.model.list_word(second + UNITY_POSITION),
        pair(20, 0),
        "cut at the surface's edge"
    );
    assert_eq!(
        rig.model.list_word(second + UNITY_POINTER),
        rig.buffers.bus_of(window.region) + 50 * 4,
        "read from its first column on screen"
    );
    assert_eq!(
        rig.model.list_word(second + 3),
        pair(100, 150),
        "150 of its 200 columns"
    );
    let pointer = second + plane_len(&rig, second);
    assert_ne!(
        rig.model.list_word(pointer) & (1 << 30),
        0,
        "the pointer's plane"
    );
    assert_eq!(
        rig.model.list_word(pointer + plane_len(&rig, pointer)),
        1 << 31,
        "then the end"
    );
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    let too_deep = [layer(base.id, 0, 0, mode.width_px, mode.height_px); 9];
    assert_eq!(
        engine.present_layers(&too_deep),
        Err(DriverError::LengthOutOfRange)
    );
}

/// A stack of `frame` alone, latched on channel 0, answering where its
/// pointer plane starts.
fn latched_frame(rig: &Rig, engine: &mut Hvs) -> usize {
    let mode = engine.output().expect("an output");
    let frame = filled(rig, engine, &mode, 1);
    engine
        .present(frame.id, &[DamageRect::full(&mode)])
        .expect("queued");
    rig.model.next_frame(0);
    engine.service_events().expect("serviced");
    let at = slot(0, List::Stack(0));
    at + plane_len(rig, at)
}

#[test]
fn the_pointer_moves_and_changes_in_the_lists_it_is_read_from_with_no_present() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let pointer = latched_frame(&rig, &mut engine);
    let arrow = engine.alloc_buffer(16, 16).expect("an image");
    rig.buffers.draw(arrow.region, 0, &[0xFF; 16 * 4]);
    engine.set_cursor(arrow.id, 1, 2).expect("shown");
    engine.move_cursor(100, 200).expect("moved");
    assert_eq!(
        rig.model.list_word(pointer + UNITY_POSITION),
        pair(198, 99),
        "its hot spot where it was put"
    );
    let image = rig.model.list_word(pointer + UNITY_POINTER);
    engine
        .free_buffer(arrow.id)
        .expect("the presenter may let it go at once");
    assert!(
        rig.buffers.live(image),
        "the image is the engine's own copy"
    );
    engine.hide_cursor().expect("hidden");
    assert_ne!(
        rig.model.list_word(pointer + UNITY_POINTER),
        image,
        "hidden, it reads the clear image"
    );
    assert_eq!(engine.cursor_caps().max_width_px, 64);
    let big = engine.alloc_buffer(65, 4).expect("a buffer");
    assert_eq!(
        engine.set_cursor(big.id, 0, 0),
        Err(DriverError::LengthOutOfRange),
        "past the images it draws from"
    );
}

#[test]
fn a_pointer_image_a_list_may_still_read_is_not_drawn_into_until_two_frames_end() {
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let pointer = latched_frame(&rig, &mut engine);
    let image = engine.alloc_buffer(8, 8).expect("an image");
    let mut seen = Vec::new();
    for _ in 0..4 {
        engine.set_cursor(image.id, 0, 0).expect("shown");
        seen.push(rig.model.list_word(pointer + UNITY_POINTER));
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        4,
        "every change inside a frame draws into a fresh image"
    );
    for _ in 0..2 {
        rig.model.next_frame(0);
        engine.service_events().expect("serviced");
    }
    engine.set_cursor(image.id, 0, 0).expect("shown");
    assert!(
        seen.contains(&rig.model.list_word(pointer + UNITY_POINTER)),
        "two frame ends on, a retired image is drawn into again"
    );
}

/// A pointer moving into a cut by the top edge and out of it again changes
/// where its plane reads and how much, and no state between the writes reads
/// past its image either way.
#[test]
fn a_pointer_crossing_a_cut_never_reads_past_its_image() {
    const SIDE: u32 = 64;
    let rig = Rig::new();
    let mut engine = rig.open().expect("opened");
    let pointer = latched_frame(&rig, &mut engine);
    let arrow = engine.alloc_buffer(SIDE, SIDE).expect("an image");
    engine.set_cursor(arrow.id, 0, 0).expect("shown");
    engine.move_cursor(100, 100).expect("moved");
    let image = rig.model.list_word(pointer + UNITY_POINTER);
    let stride = 4 * SIDE;
    let end = image + SIDE * stride;
    let reaches = |bus: u32, size: u32| {
        let (width, height) = (size & 0x1FFF, size >> 16 & 0x1FFF);
        bus + (height - 1) * stride + width * 4
    };
    for y in [-40, 100] {
        let mut bus = rig.model.list_word(pointer + UNITY_POINTER);
        let mut size = rig.model.list_word(pointer + UNITY_SIZE);
        let writes = rig.model.write_count();
        engine.move_cursor(100, y).expect("moved");
        for &(block, offset, value) in &rig.model.writes()[writes..] {
            if block.is_some() || offset < hvs::DLIST {
                continue;
            }
            match (offset - hvs::DLIST) / 4 {
                at if at == pointer + UNITY_POINTER => bus = value,
                at if at == pointer + UNITY_SIZE => size = value,
                _ => continue,
            }
            assert!(reaches(bus, size) <= end, "to {y}: {bus:#x} {size:#x}");
        }
        assert_eq!(bus > image, y < 0, "cut only while above the top edge");
    }
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
