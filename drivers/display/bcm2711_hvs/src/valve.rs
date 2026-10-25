//! A pixel valve: the timing generator between an HVS output and the encoder
//! it feeds, as the BCM2711's HDMI-feeding valves are set up.

use tairix_abi::driver::display::{Scan, Timing};
use tairix_abi::driver::mmio::RegisterBlock;
use tairix_abi::{Delay, DriverError};

use crate::hvs::Output;
use crate::regs::field;
use crate::regs::pv as reg;

/// How a valve's output loads the HVS core clock.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Load {
    /// A deep FIFO lets the HVS fill only a line's active pixels, with a
    /// margin, and never less than nine tenths of the pixel clock.
    Active,
    /// A shallow FIFO needs the HVS to keep pace with the pixel clock.
    Clock,
}

/// One of the HDMI-feeding valves, as the silicon fixes it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ValveKind {
    /// Its register window's name on the engine's node.
    pub window: &'static [u8],
    /// The name of the node's link to the encoder it feeds.
    pub encoder: &'static [u8],
    /// The HVS output it reads.
    pub output: Output,
    /// The HVS channel the engine feeds that output from.
    pub channel: usize,
    /// Bytes of its FIFO.
    pub fifo_depth: u32,
    /// A FIFO level the valve needs whatever its depth.
    pub fixed_level: Option<u32>,
    /// How its output loads the core clock.
    pub load: Load,
}

/// Bytes of a pixel the HVS fetches ahead of a valve's FIFO.
const FETCH_LATENCY: u32 = 3 * 6;

/// Pixels a valve clocks out each cycle.
const PIXELS_PER_CLOCK: u16 = 2;

/// How long a valve may take to finish the frame it is sending once told to
/// stop, in microseconds.
const STOP_BUDGET_US: u64 = 1_000;

/// How long a stopped valve's last pixels are given to leave the FIFO between
/// it and the encoder before the valve is reset.
const DRAIN_US: u32 = 20_000;

/// A valve behind its window.
pub(crate) struct Valve<R> {
    regs: R,
    kind: ValveKind,
}

impl<R> Valve<R> {
    pub const fn kind(&self) -> &ValveKind {
        &self.kind
    }
}

impl<R: RegisterBlock> Valve<R> {
    /// The valve `kind` behind `regs`.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a window short of the valve's
    /// registers.
    pub fn new(regs: R, kind: ValveKind) -> Result<Self, DriverError> {
        if regs.block_len() < reg::WINDOW_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(Self { regs, kind })
    }

    fn modify(&self, offset: usize, clear: u32, set: u32) -> Result<(), DriverError> {
        let value = self.regs.read32(offset)?;
        self.regs.write32(offset, value & !clear | set)
    }

    /// Whether the valve is sending video: enabled with its video on.
    pub fn sending(&self) -> Result<bool, DriverError> {
        Ok(self.regs.read32(reg::CONTROL)? & reg::CONTROL_EN != 0
            && self.regs.read32(reg::V_CONTROL)? & reg::VCONTROL_VIDEN != 0)
    }

    /// Stop the video at the end of the frame it is sending, and wait for
    /// the pixels it sent to drain; `false` when it outran its budget.
    pub fn stop_video(&self, delay: &dyn Delay) -> Result<bool, DriverError> {
        self.modify(reg::V_CONTROL, reg::VCONTROL_VIDEN, 0)?;
        let started = delay.now_us();
        let stopped = loop {
            if self.regs.read32(reg::V_CONTROL)? & reg::VCONTROL_VIDEN == 0 {
                break true;
            }
            if delay.now_us().saturating_sub(started) >= STOP_BUDGET_US {
                break false;
            }
        };
        delay.delay_us(DRAIN_US);
        Ok(stopped)
    }

    /// Disable the valve and clear its FIFO; it raises no interrupt, which
    /// the engine has no use for.
    pub fn reset(&self) -> Result<(), DriverError> {
        self.regs.write32(reg::INTEN, 0)?;
        self.modify(reg::CONTROL, reg::CONTROL_EN, 0)?;
        self.modify(reg::CONTROL, 0, reg::CONTROL_FIFO_CLR)
    }

    /// Set the valve up for `timing`, disabled.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] for a line whose parts the valve cannot
    /// clock out two pixels at a time, and [`DriverError::OutOfRange`] for a
    /// part no field holds.
    pub fn configure(&self, timing: &Timing) -> Result<(), DriverError> {
        let line = timing.horizontal;
        let parts = [line.active, line.front, line.sync, line.back];
        if parts.iter().any(|part| part % PIXELS_PER_CLOCK != 0) {
            return Err(DriverError::Unsupported);
        }
        let [active, front, sync, back] = parts.map(|part| u32::from(part / PIXELS_PER_CLOCK));
        let field_lines = Field::of(timing);
        self.reset()?;
        self.regs.write32(reg::HORZA, pair(back, sync)?)?;
        self.regs.write32(reg::HORZB, pair(front, active)?)?;
        let interlaced = timing.scan == Scan::Interlaced;
        let mut vertical = reg::VCONTROL_CONTINUOUS;
        let mut back = field_lines.back;
        if interlaced {
            // The odd field starts half a line late and a line further back.
            self.regs
                .write32(reg::VERTA_EVEN, pair(back, field_lines.sync)?)?;
            self.regs.write32(
                reg::VERTB_EVEN,
                pair(field_lines.front, field_lines.active)?,
            )?;
            let delay = line.total() / u32::from(2 * PIXELS_PER_CLOCK);
            vertical |= reg::VCONTROL_INTERLACE | field(delay, reg::VCONTROL_ODD_DELAY_SHIFT, 17);
            back += 1;
        }
        self.regs.write32(reg::V_CONTROL, vertical)?;
        self.regs.write32(reg::VSYNCD_EVEN, 0)?;
        self.regs
            .write32(reg::VERTA, pair(back, field_lines.sync)?)?;
        self.regs
            .write32(reg::VERTB, pair(field_lines.front, field_lines.active)?)?;
        self.regs.write32(reg::MUX_CFG, reg::MUX_CFG_NO_SWAP)?;
        let level = self
            .kind
            .fixed_level
            .unwrap_or(self.kind.fifo_depth.saturating_sub(FETCH_LATENCY));
        self.regs.write32(
            reg::CONTROL,
            reg::CONTROL_FIFO_CLR
                | field(level >> 6, reg::CONTROL_FIFO_LEVEL_HIGH_SHIFT, 2)
                | field(level, reg::CONTROL_FIFO_LEVEL_SHIFT, 6)
                | field(reg::CONTROL_FORMAT_24, reg::CONTROL_FORMAT_SHIFT, 3)
                | reg::CONTROL_CLR_AT_START
                | reg::CONTROL_TRIGGER_UNDERFLOW
                | reg::CONTROL_WAIT_HSTART,
        )
    }

    /// Enable the configured valve.
    pub fn enable(&self) -> Result<(), DriverError> {
        self.modify(reg::CONTROL, 0, reg::CONTROL_EN)
    }

    /// Start the enabled valve's video.
    pub fn start_video(&self) -> Result<(), DriverError> {
        self.modify(reg::V_CONTROL, 0, reg::VCONTROL_VIDEN)
    }
}

/// Two sixteen-bit timing parts in one register, `high` above `low`.
fn pair(high: u32, low: u32) -> Result<u32, DriverError> {
    if high > 0xFFFF || low > 0xFFFF {
        return Err(DriverError::OutOfRange);
    }
    Ok(high << 16 | low)
}

/// The lines of one field: the frame's for a progressive timing, half its
/// edges' for an interlaced one, whose fields carry half the frame's lines.
struct Field {
    active: u32,
    front: u32,
    sync: u32,
    back: u32,
}

impl Field {
    fn of(timing: &Timing) -> Self {
        let frame = timing.vertical;
        let halve = u32::from(timing.scan == Scan::Interlaced);
        let active = u32::from(frame.active);
        let sync_start = active + u32::from(frame.front);
        let sync_end = sync_start + u32::from(frame.sync);
        let total = frame.total();
        let [active, sync_start, sync_end, total] =
            [active, sync_start, sync_end, total].map(|edge| edge >> halve);
        Self {
            active,
            front: sync_start - active,
            sync: sync_end - sync_start,
            back: total - sync_end,
        }
    }
}
