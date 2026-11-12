//! What `EDIT` hooks while it runs. BREAK is set off, so no call it makes
//! looks for Ctrl+C, and INT 23h is ignored and INT 24h fails the call
//! should one still come. INT 09h leads to a filter in the program's own
//! memory: Alt going down or coming up, and Ctrl+Ins, each put a key of
//! the editor's own in the BIOS's buffer, in guest code, before the BIOS's
//! handler takes the scan code as it would. A mouse driver's event handler
//! is a routine there too, which puts the editor's mouse key in the
//! buffer only while none is waiting. All is put back as it was found.

use super::keys::own;
use super::ui::{Input, Pointer};
use super::{at, handler, keep, load, ui_of, with_ui, Hooks};
use crate::asm::{Asm, AsmError, Cond, Reg16, Reg8};
use crate::bios::{bda, shift, Ring};
use crate::dos::program::{handler_entry, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::frames::{locals, take, Far, Outcome};
use alloc::vec::Vec;

/// The scan codes the filter looks for: Alt's make and break, and Ins's.
const ALT_MAKE: u8 = 0x38;
const RELEASE: u8 = 0x80;
const INS_MAKE: u8 = 0x52;
/// The vector table's entries.
const VECTOR_09: u16 = 0x09 * 4;
const VECTOR_33: u16 = 0x33 * 4;
/// The mouse driver's events the handler is called for: the pointer
/// moving, the left button pressed and released.
const EVENTS: u16 = 0x07;
/// A mouse driver's answer to its reset.
const INSTALLED: u16 = 0xFFFF;

/// The INT 09h filter, run from the program's segment, chaining to the
/// handler whose address is kept at `old`.
///
/// # Errors
///
/// [`AsmError`] should it not assemble, a defect of its own.
pub fn filter(old: u16) -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(at::FILTER);
    let (alt_make, alt_break, ins, chain, stuff) = (
        asm.label(),
        asm.label(),
        asm.label(),
        asm.label(),
        asm.label(),
    );
    asm.push(Reg16::Ax);
    asm.push(Reg16::Cx);
    asm.in_al(0x60);
    asm.cmp_imm8(Reg8::Al, ALT_MAKE);
    asm.jcc(Cond::E, &alt_make);
    asm.cmp_imm8(Reg8::Al, ALT_MAKE | RELEASE);
    asm.jcc(Cond::E, &alt_break);
    asm.cmp_imm8(Reg8::Al, INS_MAKE);
    asm.jcc(Cond::E, &ins);
    asm.bind(&chain);
    asm.pop(Reg16::Cx);
    asm.pop(Reg16::Ax);
    asm.jmp_far_through(old);
    asm.bind(&alt_make);
    asm.mov_imm8(Reg8::Ah, 0x02);
    asm.int(0x16);
    // A make while Alt is already down is the key repeating.
    asm.test_imm8(Reg8::Al, shift::ALT);
    asm.jcc(Cond::Ne, &chain);
    asm.mov_imm16(Reg16::Cx, u16::from(own::ALT_DOWN) << 8);
    asm.jmp_short(&stuff);
    asm.bind(&alt_break);
    asm.mov_imm16(Reg16::Cx, u16::from(own::ALT_UP) << 8);
    asm.jmp_short(&stuff);
    asm.bind(&ins);
    asm.mov_imm8(Reg8::Ah, 0x02);
    asm.int(0x16);
    asm.test_imm8(Reg8::Al, shift::CTRL);
    asm.jcc(Cond::E, &chain);
    asm.mov_imm16(Reg16::Cx, u16::from(own::COPY) << 8);
    asm.bind(&stuff);
    asm.mov_imm8(Reg8::Ah, 0x05);
    asm.int(0x16);
    asm.jmp_short(&chain);
    asm.finish()
}

/// The mouse driver's event handler, run from the program's segment.
///
/// # Errors
///
/// [`AsmError`] should it not assemble, a defect of its own.
pub fn mouse_handler() -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(at::MOUSE);
    let (done, stored) = (asm.label(), asm.label());
    asm.cmp_cs_byte(at::PENDING, 0);
    asm.jcc(Cond::Ne, &done);
    asm.mov_cs_byte(at::PENDING, 1);
    asm.push(Reg16::Ax);
    asm.push(Reg16::Cx);
    asm.mov_imm16(Reg16::Cx, u16::from(own::MOUSE) << 8);
    asm.mov_imm8(Reg8::Ah, 0x05);
    asm.int(0x16);
    asm.test8(Reg8::Al);
    asm.jcc(Cond::E, &stored);
    // The buffer was full: no key waits, so the next event may try again.
    asm.mov_cs_byte(at::PENDING, 0);
    asm.bind(&stored);
    asm.pop(Reg16::Cx);
    asm.pop(Reg16::Ax);
    asm.bind(&done);
    asm.retf();
    asm.finish()
}

/// The filter and the mouse handler laid in the program's memory and
/// INT 09h led to the filter, written straight into the vector table, its
/// handler before kept: no key is taken between the two.
fn hook_keyboard(p: &mut Program<'_, '_>) -> bool {
    let (Ok(filter), Ok(mouse)) = (filter(at::OLD_09), mouse_handler()) else {
        return false;
    };
    if filter.len() > usize::from(at::FILTER_LEN) || mouse.len() > usize::from(at::MOUSE_LEN) {
        return false;
    }
    p.g.write_bytes(p.own, at::FILTER, &filter);
    p.g.write_bytes(p.own, at::MOUSE, &mouse);
    let before = Far::new(p.g.read16(0, VECTOR_09 + 2), p.g.read16(0, VECTOR_09));
    p.set_far(at::OLD_09, before);
    let mut hooks: Hooks = load(p, at::HOOKS);
    hooks.keyboard = true;
    keep(p, at::HOOKS, hooks);
    p.g.write16(0, VECTOR_09, at::FILTER);
    p.g.write16(0, VECTOR_09 + 2, p.own);
    true
}

locals! {
    /// BREAK set off, the handlers set, the keyboard's filter laid, and a
    /// mouse driver, where one answers, made to report to the editor.
    pub struct EditHook {}
}

mod phase {
    pub const BREAK_ASKED: u8 = 1;
    pub const BREAK_SET: u8 = 2;
    pub const INT23_SET: u8 = 3;
    pub const INT24_SET: u8 = 4;
    pub const RESET: u8 = 5;
    pub const ACROSS: u8 = 6;
    pub const DOWN: u8 = 7;
    pub const HANDLER: u8 = 8;
    pub const SHOWN: u8 = 9;
}

impl EditHook {
    /// The keyboard hooked, then the mouse driver asked to reset, where its
    /// vector leads anywhere.
    fn keyboard(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if !hook_keyboard(p) {
            return Flow::Return(Ok(0));
        }
        if p.g.read16(0, VECTOR_33) == 0 && p.g.read16(0, VECTOR_33 + 2) == 0 {
            return Flow::Return(Ok(0));
        }
        *phase = phase::RESET;
        Flow::Call(Frame::of(&Int::bios(p.own, 0x33, 0x0000)))
    }

    /// The mouse driver's range made the screen's cells, eight of its
    /// points to each.
    fn ranges(p: &mut Program<'_, '_>, phase: &mut u8, across: bool) -> Flow {
        let screen = ui_of(p).screen;
        let (function, cells) = if across {
            (0x0007, screen.columns)
        } else {
            (0x0008, screen.rows)
        };
        *phase = if across { phase::ACROSS } else { phase::DOWN };
        Flow::Call(Frame::of(&Int {
            cx: 0,
            dx: u16::from(cells).saturating_mul(8).saturating_sub(1),
            ..Int::bios(p.own, 0x33, function)
        }))
    }
}

impl Routine for EditHook {
    const ID: Id = Id::EditHook;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::BREAK_ASKED => {
                take!(child);
                let mut hooks: Hooks = load(p, at::HOOKS);
                hooks.broken = p.result(Returned::Dx).to_le_bytes()[0];
                keep(p, at::HOOKS, hooks);
                *phase = phase::BREAK_SET;
                Flow::Call(Frame::of(&Int {
                    dx: 0,
                    ..Int::dos(p.own, 0x3301)
                }))
            }
            phase::BREAK_SET => {
                take!(child);
                *phase = phase::INT23_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::BREAK),
                    ..Int::dos(p.own, 0x2523)
                }))
            }
            phase::INT23_SET => {
                take!(child);
                *phase = phase::INT24_SET;
                Flow::Call(Frame::of(&Int {
                    dx: handler_entry(handler::CRITICAL),
                    ..Int::dos(p.own, 0x2524)
                }))
            }
            phase::INT24_SET => {
                take!(child);
                Self::keyboard(p, phase)
            }
            phase::RESET => {
                if take!(child) != u32::from(INSTALLED) {
                    return Flow::Return(Ok(0));
                }
                let mut hooks: Hooks = load(p, at::HOOKS);
                hooks.mouse = true;
                keep(p, at::HOOKS, hooks);
                Self::ranges(p, phase, true)
            }
            phase::ACROSS => {
                take!(child);
                Self::ranges(p, phase, false)
            }
            phase::DOWN => {
                take!(child);
                *phase = phase::HANDLER;
                Flow::Call(Frame::of(&Int {
                    cx: EVENTS,
                    dx: at::MOUSE,
                    ..Int::bios(p.own, 0x33, 0x000C)
                }))
            }
            phase::HANDLER => {
                take!(child);
                *phase = phase::SHOWN;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x33, 0x0001)))
            }
            phase::SHOWN => {
                take!(child);
                Flow::Return(Ok(0))
            }
            _ => {
                *phase = phase::BREAK_ASKED;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x3300)))
            }
        }
    }
}

/// Whether `key` is one of the editor's own.
fn is_own(key: u16) -> bool {
    let [ascii, scan] = key.to_le_bytes();
    ascii == 0 && matches!(scan, own::ALT_DOWN | own::ALT_UP | own::MOUSE | own::COPY)
}

/// The editor's own keys taken out of the BIOS's buffer, the others kept
/// in their order, so none reaches the program after it.
fn drop_own_keys(p: &mut Program<'_, '_>) {
    let ring = Ring::read(p.g);
    if !ring.is_sound() {
        return;
    }
    let mut kept = Vec::new();
    let mut at = ring.head;
    for _ in 0..(ring.end - ring.start) / 2 {
        if at == ring.tail {
            break;
        }
        let key = p.g.read16(bda::SEGMENT, at);
        if !is_own(key) {
            kept.push(key);
        }
        at = ring.after(at);
    }
    let mut tail = ring.head;
    for key in kept {
        p.g.write16(bda::SEGMENT, tail, key);
        tail = ring.after(tail);
    }
    p.g.write16(bda::SEGMENT, bda::KEY_TAIL, tail);
}

locals! {
    /// All the editor hooked put back: INT 09h's handler, the mouse driver
    /// reset so it calls nothing of the editor's, its own keys left in the
    /// BIOS's buffer taken out, and BREAK as it was.
    pub struct EditUnhook {}
}

mod unhook {
    pub const RESET: u8 = 1;
    pub const BREAK_BACK: u8 = 2;
}

impl Routine for EditUnhook {
    const ID: Id = Id::EditUnhook;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        let hooks: Hooks = load(p, at::HOOKS);
        match *phase {
            unhook::RESET => {
                take!(child);
                *phase = unhook::BREAK_BACK;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(hooks.broken),
                    ..Int::dos(p.own, 0x3301)
                }))
            }
            unhook::BREAK_BACK => {
                take!(child);
                // Last, once neither hook can put in another.
                drop_own_keys(p);
                Flow::Return(Ok(0))
            }
            _ => {
                if hooks.keyboard {
                    let before = p.get_far(at::OLD_09);
                    p.g.write16(0, VECTOR_09, before.offset);
                    p.g.write16(0, VECTOR_09 + 2, before.segment);
                }
                if hooks.mouse {
                    *phase = unhook::RESET;
                    return Flow::Call(Frame::of(&Int::bios(p.own, 0x33, 0x0000)));
                }
                *phase = unhook::BREAK_BACK;
                Flow::Call(Frame::of(&Int {
                    dx: u16::from(hooks.broken),
                    ..Int::dos(p.own, 0x3301)
                }))
            }
        }
    }
}

locals! {
    /// The mouse's key read: the driver asked for the left button's
    /// presses and releases since, and where it is now; what the editor
    /// makes of them returned as its action's number.
    pub struct EditPointer {
        presses: u16,
        x: u16,
        y: u16,
        releases: u16,
    }
}

mod pointer {
    pub const PRESSES: u8 = 1;
    pub const RELEASES: u8 = 2;
    pub const POSITION: u8 = 3;
}

/// The cell a driver's point `x`, `y` falls in.
fn cell(x: u16, y: u16) -> (u8, u8) {
    (
        u8::try_from(y / 8).unwrap_or(u8::MAX),
        u8::try_from(x / 8).unwrap_or(u8::MAX),
    )
}

impl Routine for EditPointer {
    const ID: Id = Id::EditPointer;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            pointer::PRESSES => {
                take!(child);
                self.presses = p.result(Returned::Bx);
                self.x = p.result(Returned::Cx);
                self.y = p.result(Returned::Dx);
                *phase = pointer::RELEASES;
                Flow::Call(Frame::of(&Int {
                    bx: 0,
                    ..Int::bios(p.own, 0x33, 0x0006)
                }))
            }
            pointer::RELEASES => {
                take!(child);
                self.releases = p.result(Returned::Bx);
                *phase = pointer::POSITION;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x33, 0x0003)))
            }
            pointer::POSITION => {
                take!(child);
                let held = p.result(Returned::Bx) & 1 != 0;
                let at = cell(p.result(Returned::Cx), p.result(Returned::Dx));
                let flags = p.g.bda8(bda::SHIFT_FLAGS);
                let now = p.g.bda16(bda::TICKS);
                let pointer = Pointer {
                    presses: self.presses,
                    pressed_at: cell(self.x, self.y),
                    releases: self.releases,
                    held,
                    at,
                    flags,
                    now,
                };
                let action = with_ui(p, |ui, text| ui.input(Input::Pointer(pointer), text));
                Flow::Return(Ok(action.number()))
            }
            _ => {
                p.set8(at::PENDING, 0);
                *phase = pointer::PRESSES;
                Flow::Call(Frame::of(&Int {
                    bx: 0,
                    ..Int::bios(p.own, 0x33, 0x0005)
                }))
            }
        }
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
