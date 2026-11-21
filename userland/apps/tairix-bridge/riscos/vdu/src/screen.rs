//! Screen memory (`plans/RISCOS-EMULATOR.md` §3, §8): dynamic area 2,
//! mapped twice so a screen turned by a hardware scroll reads on unbroken
//! past its end; its banks, the one the drivers write and the one shown;
//! and the area's handler, which keeps the current screen's banks from being
//! taken away.

use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_kernel::memory::{Bits, Create};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::READ_DYNAMIC_AREA;
use tairix_riscos_kernel::swi::X;

use crate::canvas::Canvas;
use crate::mode::{flag, Format};
use crate::state::read;

/// The screen's dynamic area.
pub(crate) const AREA: u32 = 2;
/// Its flags: not cacheable, mapped twice, its pages its handler's to choose.
const AREA_FLAGS: u32 = (1 << 5) | (1 << 6) | (1 << 8);
/// The most it may reach, as RISC OS 5 has it, and in a layout that admits
/// 26-bit code the most the 26-bit window spares it.
const MOST: u32 = 0x80_0000;
const MOST_26BIT: u32 = 0x20_0000;
/// Its title.
const TITLE: &[u8] = b"Screen memory";

/// A dynamic area handler's reasons.
mod reason {
    pub(super) const PRE_SHRINK: u32 = 2;
    pub(super) const POST_GROW: u32 = 1;
    pub(super) const POST_SHRINK: u32 = 3;
}

/// How the current mode was named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    /// By number, shadow bit apart.
    Number(u32),
    /// By the selector the drivers copied, at this address.
    Selector(u32),
}

/// The screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Screen {
    pub(crate) format: Format,
    pub(crate) named: Named,
    /// Frames a second.
    pub(crate) frame_rate: u32,
    /// The area's first byte and its size.
    pub(crate) base: u32,
    pub(crate) size: u32,
    /// Where the screen written and the screen shown start, from the area's
    /// first byte, a hardware scroll's turn included.
    pub(crate) vdu_start: u32,
    pub(crate) display_start: u32,
    /// The bank a mode change chose, which bank nought names.
    pub(crate) own_bank: u32,
    /// `OS_Byte` 144's vertical shift and interlace, kept for the next mode.
    pub(crate) tv: [u32; 2],
    /// Where `OS_ScreenMode` 1 and `OS_CheckModeValid` hand selectors out.
    pub(crate) selector_copy: u32,
    pub(crate) check_copy: u32,
    /// `OS_ClaimScreenMemory` has lent the screen's spare memory.
    pub(crate) lent: bool,
}

impl Screen {
    /// The bytes a bank takes.
    pub(crate) const fn bank_bytes(&self) -> u32 {
        self.format.screen_bytes()
    }

    /// The banks the area holds.
    pub(crate) const fn banks(&self) -> u32 {
        match self.size.checked_div(self.bank_bytes()) {
            Some(banks) => banks,
            None => 0,
        }
    }

    /// The offset bank `n` starts at.
    pub(crate) const fn bank_offset(&self, n: u32) -> u32 {
        (n - 1) * self.bank_bytes()
    }

    /// The screen the drivers write, as they write it.
    pub(crate) const fn canvas(&self) -> Canvas {
        Canvas {
            start: self.base + self.vdu_start,
            line: self.format.line_length(),
            width: self.format.width(),
            height: self.format.height(),
            log2_bpc: self.format.log2_bpc(),
        }
    }

    /// The screen shown, as the display reads it.
    pub(crate) const fn shown(&self) -> Canvas {
        Canvas {
            start: self.base + self.display_start,
            ..self.canvas()
        }
    }

    /// `ScreenStart` and `DisplayStart`.
    pub(crate) const fn screen_start(&self) -> u32 {
        self.base + self.vdu_start
    }

    pub(crate) const fn display_start_address(&self) -> u32 {
        self.base + self.display_start
    }

    /// The bank the drivers write is the second.
    pub(crate) const fn writing_second(&self) -> bool {
        let bytes = self.bank_bytes();
        bytes != 0 && self.vdu_start / bytes == 1
    }

    /// Whether whole-screen text scrolls by moving the screen: not in
    /// Teletext, whose page the drivers redraw.
    pub(crate) const fn hardware_scrolls(&self) -> bool {
        !self.format.has(flag::TELETEXT)
    }

    /// Moves both screens' starts a text row of `lines` pixel lines, up or
    /// down, round the area.
    pub(crate) fn hardware_scroll(&mut self, up: bool, lines: u32) {
        if self.size == 0 {
            return;
        }
        let step = (lines * self.format.line_length()) % self.size;
        let turn = |start: u32| {
            if up {
                (start + step) % self.size
            } else {
                (start + self.size - step) % self.size
            }
        };
        self.vdu_start = turn(self.vdu_start);
        self.display_start = turn(self.display_start);
    }

    /// Selects bank `n` to be written, or shown.
    pub(crate) fn set_bank(&mut self, written: bool, n: u32) {
        let offset = self.bank_offset(n);
        if written {
            self.vdu_start = offset;
        } else {
            self.display_start = offset;
        }
    }

    /// `OS_Word` 22: the screen written, the screen shown or both started
    /// `offset` into the area.
    pub(crate) fn set_base(&mut self, written: bool, shown: bool, offset: u32) {
        let offset = if self.size == 0 {
            0
        } else {
            offset % self.size
        };
        if written {
            self.vdu_start = offset;
        }
        if shown {
            self.display_start = offset;
        }
    }

    /// The bytes the current screen needs of the area: every bank either
    /// screen starts in, whole pages.
    pub(crate) const fn needed(&self) -> u32 {
        let bytes = self.bank_bytes();
        if bytes == 0 {
            return 0;
        }
        let furthest = if self.vdu_start > self.display_start {
            self.vdu_start
        } else {
            self.display_start
        };
        ((furthest / bytes + 1) * bytes).next_multiple_of(PAGE_SIZE)
    }
}

/// Makes the screen's area, `size` bytes to begin with, its handler the
/// drivers' routine at `handler`, every page watched for the display; its
/// base and size.
pub(crate) fn make_area(os: &mut Os<'_>, size: u32, handler: u32) -> Result<(u32, u32), Fail> {
    let caller = if os.layout().admits_26bit {
        Bits::Bit26
    } else {
        Bits::Bit32
    };
    let most = if caller == Bits::Bit26 {
        MOST_26BIT
    } else {
        MOST
    };
    os.create_kernel_area(&Create {
        number: Some(AREA),
        size: size.next_multiple_of(PAGE_SIZE).min(most),
        base: None,
        flags: AREA_FLAGS,
        max: Some(most),
        handler: Some(handler),
        workspace: None,
        title: Some(TITLE),
        pool: 0,
        caller,
    })?;
    os.watch_area_for_display(AREA)?;
    area(os)
}

/// The area's base and size as they are now.
pub(crate) fn area(os: &mut Os<'_>) -> Result<(u32, u32), Fail> {
    let mut regs = Regs::new([AREA, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.perform(X | READ_DYNAMIC_AREA, &mut regs)?;
    Ok((regs.r[0], regs.r[1] & 0x7FFF_FFFF))
}

/// The area's handler: a shrink is held to what the current screen does not
/// need, and after any change the drivers learn where the area now is. While
/// the area is first grown the drivers are not yet there, and nothing is
/// held.
pub(crate) fn area_handler(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        reason::PRE_SHRINK => {
            let size = regs.r[4];
            if let Some(needed) = read(os, |vdu, _| vdu.screen.needed()) {
                let spare = size.saturating_sub(needed) & !(PAGE_SIZE - 1);
                regs.r[3] = regs.r[3].min(spare);
            }
        }
        reason::POST_GROW | reason::POST_SHRINK => {
            let (base, size) = area(os)?;
            let _ = read(os, |vdu, _| {
                let screen = &mut vdu.screen;
                screen.base = base;
                screen.size = size;
                if screen.needed() > size {
                    screen.vdu_start = 0;
                    screen.display_start = 0;
                }
            });
            if read(os, |_, _| ()).is_some() {
                os.watch_area_for_display(AREA)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests;
