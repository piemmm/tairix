//! The mouse (`plans/RISCOS-EMULATOR.md` §4.5): its position on the screen
//! and its buttons as the host delivers them, kept inside the bounding box
//! `OS_Word` 21,1 sets, moved by the multipliers of 21,2 where the host
//! reports movement rather than a place, and read through `MouseV` relative
//! to the graphics origin of the moment, as the PRM's pointer chapter has it.

use crate::os::{Fail, Os, Regs, Unwind};

/// `OS_Word` 21's reasons the mouse answers: its bounds, its multipliers, and
/// its position set and read.
pub const MOUSE_REASONS: core::ops::RangeInclusive<u8> = 1..=4;
/// A mouse buffer packet: the position, the buttons and the time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Packet {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) buttons: u32,
    pub(crate) time: u32,
}

impl Packet {
    /// The bytes a packet takes in the buffer.
    pub(crate) const BYTES: usize = 9;

    /// The packet as the buffer holds it: each coordinate's low 16 bits, the
    /// buttons' low byte, then the time.
    pub(crate) fn to_bytes(self) -> [u8; Self::BYTES] {
        let mut bytes = [0u8; Self::BYTES];
        bytes[..2].copy_from_slice(&self.x.to_le_bytes()[..2]);
        bytes[2..4].copy_from_slice(&self.y.to_le_bytes()[..2]);
        bytes[4] = self.buttons.to_le_bytes()[0];
        bytes[5..].copy_from_slice(&self.time.to_le_bytes());
        bytes
    }

    /// The packet `bytes` hold.
    pub(crate) fn from_bytes(bytes: [u8; Self::BYTES]) -> Self {
        Self {
            x: i32::from(i16::from_le_bytes([bytes[0], bytes[1]])),
            y: i32::from(i16::from_le_bytes([bytes[2], bytes[3]])),
            buttons: u32::from(bytes[4]),
            time: u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
        }
    }
}
/// The mouse event.
const MOUSE_EVENT: u32 = 10;
/// A box no position can leave: the most a packet's 16-bit coordinates hold.
const UNBOUNDED: Bounds = Bounds {
    left: i16::MIN as i32,
    bottom: i16::MIN as i32,
    right: i16::MAX as i32,
    top: i16::MAX as i32,
};

/// A box on the screen, inclusive at every edge, from the screen's bottom
/// left: in OS units, or in pixels where its holder counts them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bounds {
    /// The leftmost position.
    pub left: i32,
    /// The lowest.
    pub bottom: i32,
    /// The rightmost.
    pub right: i32,
    /// The highest.
    pub top: i32,
}

impl Bounds {
    /// `(x, y)` moved to the nearest point within.
    const fn clamp(self, x: i32, y: i32) -> (i32, i32) {
        (
            clamp_between(x, self.left, self.right),
            clamp_between(y, self.bottom, self.top),
        )
    }

    /// Whether `(x, y)` lies within it.
    #[must_use]
    pub const fn holds(self, x: i32, y: i32) -> bool {
        x >= self.left && x <= self.right && y >= self.bottom && y <= self.top
    }
}

const fn clamp_between(value: i32, low: i32, high: i32) -> i32 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

/// Where the mouse is and what bounds it.
pub(crate) struct Mouse {
    /// From the screen's bottom left, in OS units.
    pub(crate) x: i32,
    pub(crate) y: i32,
    /// The buttons held: Adjust, Menu, Select from bit 0.
    pub(crate) buttons: u32,
    bounds: Bounds,
    /// What a movement the host reports is multiplied by, across and up.
    multipliers: (i32, i32),
}

impl Mouse {
    pub(crate) const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            buttons: 0,
            bounds: UNBOUNDED,
            multipliers: (1, 1),
        }
    }
}

impl Os<'_> {
    /// The multipliers at reset: the configured mouse step, both ways.
    pub(crate) fn reset_mouse(&mut self) {
        let step = i32::from(self.world.cmos.read(crate::cmos::MOUSE_STEP).cast_signed());
        self.world.input.mouse.multipliers = (step, step);
    }

    /// The graphics origin the VDU drivers keep, which the mouse is read
    /// relative to: the screen's corner where there are none.
    fn mouse_origin(&mut self) -> (i32, i32) {
        self.world.vdu.map_or((0, 0), |vdu| (vdu.origin)(self))
    }

    /// The mouse at `x`, `y` on the screen with `buttons`, kept within its
    /// box: a change of buttons buffered as a packet, with the mouse event.
    pub(crate) fn pointer_moved(&mut self, x: i32, y: i32, buttons: u32) -> Result<(), Unwind> {
        let mouse = &mut self.world.input.mouse;
        let (x, y) = mouse.bounds.clamp(x, y);
        let changed = mouse.buttons != buttons;
        (mouse.x, mouse.y, mouse.buttons) = (x, y, buttons);
        if !changed {
            return Ok(());
        }
        let time = self.now32();
        let packet = Packet {
            x,
            y,
            buttons,
            time,
        };
        self.world.input.buffer_mouse_packet(packet.to_bytes());
        let (ox, oy) = self.mouse_origin();
        let mut event = Regs::new([
            MOUSE_EVENT,
            x.wrapping_sub(ox).cast_unsigned(),
            y.wrapping_sub(oy).cast_unsigned(),
            buttons,
            time,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        self.raise_event(&mut event)?;
        Ok(())
    }

    /// The mouse moved by `dx`, `dy` as the host measured it, which the
    /// multipliers scale, with `buttons`.
    pub(crate) fn mouse_moved_by(&mut self, dx: i32, dy: i32, buttons: u32) -> Result<(), Unwind> {
        let mouse = &self.world.input.mouse;
        let (mx, my) = mouse.multipliers;
        let x = mouse.x.saturating_add(dx.saturating_mul(mx));
        let y = mouse.y.saturating_add(dy.saturating_mul(my));
        self.pointer_moved(x, y, buttons)
    }

    /// `MouseV`'s default: the next buffered button change, or where the
    /// mouse is now, either kept within the box as it is now: R0 and R1 its
    /// position from the graphics origin, R2 the buttons, R3 the time.
    #[expect(clippy::unnecessary_wraps, reason = "a vector default's signature")]
    pub(crate) fn default_mouse(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (x, y, buttons, time) = if let Some(packet) = self.world.input.take_mouse_packet() {
            (packet.x, packet.y, packet.buttons, packet.time)
        } else {
            let mouse = &self.world.input.mouse;
            (mouse.x, mouse.y, mouse.buttons, self.now32())
        };
        let (x, y) = self.world.input.mouse.bounds.clamp(x, y);
        let (ox, oy) = self.mouse_origin();
        regs.r[0] = x.wrapping_sub(ox).cast_unsigned();
        regs.r[1] = y.wrapping_sub(oy).cast_unsigned();
        regs.r[2] = buttons;
        regs.r[3] = time;
        Ok(())
    }

    /// Where the mouse is on the screen, in OS units from its bottom left:
    /// what a pointer linked to it follows.
    #[must_use]
    pub fn mouse_position(&self) -> (i32, i32) {
        let mouse = &self.world.input.mouse;
        (mouse.x, mouse.y)
    }

    /// Bounds the mouse to `bounds`, homing it to the nearest point within:
    /// what a mode change does with the whole screen.
    pub fn set_mouse_bounds(&mut self, bounds: Bounds) {
        let mouse = &mut self.world.input.mouse;
        mouse.bounds = bounds;
        (mouse.x, mouse.y) = bounds.clamp(mouse.x, mouse.y);
    }

    /// `OS_Word` 21's mouse reasons, 1 to 4, at `block`: whether `reason`
    /// is one.
    pub(crate) fn mouse_word(&mut self, reason: u8, block: u32) -> Result<bool, Fail> {
        match reason {
            1 => {
                let (ox, oy) = self.mouse_origin();
                let (left, bottom) = self.read_coordinates(block.wrapping_add(1))?;
                let (right, top) = self.read_coordinates(block.wrapping_add(5))?;
                let bounds = Bounds {
                    left: left + ox,
                    bottom: bottom + oy,
                    right: right + ox,
                    top: top + oy,
                };
                if bounds.left <= bounds.right && bounds.bottom <= bounds.top {
                    self.set_mouse_bounds(bounds);
                }
            }
            2 => {
                let across = i32::from(self.read_byte(block.wrapping_add(1))?.cast_signed());
                let up = i32::from(self.read_byte(block.wrapping_add(2))?.cast_signed());
                self.world.input.mouse.multipliers = (across, up);
            }
            3 => {
                let (ox, oy) = self.mouse_origin();
                let (x, y) = self.read_coordinates(block.wrapping_add(1))?;
                let (x, y) = (x + ox, y + oy);
                let mouse = &mut self.world.input.mouse;
                if mouse.bounds.holds(x, y) {
                    (mouse.x, mouse.y) = (x, y);
                }
            }
            4 => {
                let (ox, oy) = self.mouse_origin();
                let (x, y) = self.mouse_position();
                self.write_coordinates(
                    block.wrapping_add(1),
                    (x.wrapping_sub(ox), y.wrapping_sub(oy)),
                )?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
