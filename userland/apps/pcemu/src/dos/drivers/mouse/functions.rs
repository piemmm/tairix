//! The INT 33h functions, `00h` to `24h`, grouped by what they reach: the
//! cursor, what the mouse has done, the handlers, the saved state, the
//! settings, and the driver itself.

use super::{at, event, state, Answer, Mouse, BUTTONS, SERIAL, THRESHOLD, VERSION};
use crate::dos::engine::Far;

/// A register as the signed number the function takes it for.
fn signed(value: u16) -> i16 {
    i16::from_le_bytes(value.to_le_bytes())
}

impl Mouse<'_, '_> {
    /// Function `ax` with the registers `regs` — BX, CX, DX, SI, DI — and
    /// ES; a function past `24h` changes nothing.
    pub(super) fn function(&mut self, ax: u16, regs: [u16; 5], es: u16) -> Answer {
        let mut answer = Answer::default();
        match ax {
            0x00 | 0x21 => {
                self.reset();
                answer.ax = Some(0xFFFF);
                answer.bx = Some(BUTTONS);
            }
            0x01 | 0x02 | 0x04 | 0x07..=0x0A | 0x10 | 0x1D => self.cursor_function(ax, regs, es),
            0x03 | 0x05 | 0x06 | 0x0B => self.reading(ax, regs[0], &mut answer),
            0x0C | 0x14 | 0x18 | 0x19 => self.handlers(ax, regs, es, &mut answer),
            0x15..=0x17 => self.saved_state(ax, regs[2], es, &mut answer),
            0x0D..=0x0F | 0x13 | 0x1A | 0x1B | 0x1E | 0x22..=0x24 => {
                self.settings(ax, regs, &mut answer);
            }
            0x1F => self.disable(&mut answer),
            0x20 if self.get8(at::ENABLED) == 0 => self.connect(),
            _ => {}
        }
        answer
    }

    /// The functions that show, hide, move, shape and confine the cursor.
    fn cursor_function(&mut self, ax: u16, [bx, cx, dx, si, di]: [u16; 5], es: u16) {
        match ax {
            0x01 => {
                let visible = self.signed(state::VISIBLE);
                if visible < 0 {
                    self.set_signed(state::VISIBLE, visible + 1);
                }
                self.set_state(state::EXCLUDING, 0);
                self.update();
            }
            0x02 => {
                let visible = self.signed(state::VISIBLE).saturating_sub(1);
                self.set_signed(state::VISIBLE, visible);
                self.update();
            }
            0x04 => self.place(signed(cx), signed(dx)),
            0x07 | 0x08 => {
                let (low, high) = if ax == 0x07 {
                    (state::MIN_X, state::MAX_X)
                } else {
                    (state::MIN_Y, state::MAX_Y)
                };
                let (a, b) = (signed(cx), signed(dx));
                self.set_signed(low, a.min(b));
                self.set_signed(high, a.max(b));
                let (x, y) = (self.signed(state::X), self.signed(state::Y));
                self.place(x, y);
            }
            0x09 => {
                self.erase();
                self.set_signed(state::HOT_X, signed(bx).clamp(-16, 16));
                self.set_signed(state::HOT_Y, signed(cx).clamp(-16, 16));
                let mut masks = [0u8; 64];
                self.g.read_bytes(es, dx, &mut masks);
                self.g
                    .write_bytes(self.own, at::STATE + state::SCREEN_MASK, &masks);
                self.update();
            }
            0x0A if bx <= 1 => {
                self.erase();
                self.set_state(state::TEXT_KIND, bx);
                self.set_state(state::TEXT_FIRST, cx);
                self.set_state(state::TEXT_SECOND, dx);
                self.update();
            }
            0x10 => {
                for (n, value) in (0u16..).zip([cx, dx, si, di]) {
                    self.set_state(state::EXCLUSION + n * 2, value);
                }
                self.set_state(state::EXCLUDING, 1);
                self.update();
            }
            0x1D => {
                self.erase();
                self.set_state(state::PAGE, bx & 7);
                self.update();
            }
            _ => {}
        }
    }

    /// The functions that report the position, the buttons and the motion.
    fn reading(&mut self, ax: u16, bx: u16, answer: &mut Answer) {
        match ax {
            0x03 => {
                answer.bx = Some(self.state(state::BUTTONS));
                answer.cx = Some(self.state(state::X));
                answer.dx = Some(self.state(state::Y));
            }
            0x05 | 0x06 => {
                let table = if ax == 0x05 {
                    state::PRESSES
                } else {
                    state::RELEASES
                };
                answer.ax = Some(self.state(state::BUTTONS));
                answer.bx = Some(0);
                if bx < BUTTONS {
                    let at = table + bx * 6;
                    answer.bx = Some(self.state(at));
                    answer.cx = Some(self.state(at + 2));
                    answer.dx = Some(self.state(at + 4));
                    self.set_state(at, 0);
                }
            }
            _ => {
                answer.cx = Some(self.state(state::MOTION_X));
                answer.dx = Some(self.state(state::MOTION_Y));
                self.set_state(state::MOTION_X, 0);
                self.set_state(state::MOTION_Y, 0);
            }
        }
    }

    /// The functions that set and exchange the event handlers.
    fn handlers(&mut self, ax: u16, [_, cx, dx, _, _]: [u16; 5], es: u16, answer: &mut Answer) {
        let handler = at::STATE + state::HANDLER;
        match ax {
            0x0C => {
                self.set_state(state::EVENT_MASK, cx);
                self.set_far(handler, Far::new(es, dx));
            }
            0x14 => {
                answer.cx = Some(self.state(state::EVENT_MASK));
                let old = self.far(handler);
                answer.dx = Some(old.offset);
                answer.es = Some(old.segment);
                self.set_state(state::EVENT_MASK, cx);
                self.set_far(handler, Far::new(es, dx));
            }
            0x18 => {
                answer.ax = Some(if self.set_alternate(cx, Far::new(es, dx)) {
                    0x0018
                } else {
                    0xFFFF
                });
            }
            _ => {
                let found = self.alternate(cx);
                answer.cx = Some(found.map_or(0, |_| cx));
                let handler = found.unwrap_or_default();
                answer.bx = Some(handler.segment);
                answer.dx = Some(handler.offset);
            }
        }
    }

    /// The functions that size, save and restore the state, at ES:DX.
    fn saved_state(&mut self, ax: u16, dx: u16, es: u16, answer: &mut Answer) {
        let mut saved = [0u8; state::LEN as usize];
        match ax {
            0x15 => answer.bx = Some(state::LEN),
            0x16 => {
                self.g.read_bytes(self.own, at::STATE, &mut saved);
                self.g.write_bytes(es, dx, &saved);
            }
            _ => {
                self.erase();
                self.g.read_bytes(es, dx, &mut saved);
                self.g.write_bytes(self.own, at::STATE, &saved);
                self.update();
            }
        }
    }

    /// The functions that set and report how motion is taken, the page and
    /// the language, and the version.
    fn settings(&mut self, ax: u16, [bx, cx, dx, _, _]: [u16; 5], answer: &mut Answer) {
        match ax {
            0x0D | 0x0E => self.set_state(state::LIGHT_PEN, u16::from(ax == 0x0D)),
            0x0F => {
                for (field, ratio) in [(state::RATIO_X, cx), (state::RATIO_Y, dx)] {
                    if (1..=0x7FFF).contains(&ratio) {
                        self.set_state(field, ratio);
                    }
                }
            }
            0x13 => self.set_state(state::THRESHOLD, if dx == 0 { THRESHOLD } else { dx }),
            0x1A => {
                for (n, value) in (0u16..).zip([bx, cx, dx]) {
                    self.set_state(state::SENSITIVITY + n * 2, value.min(100));
                }
            }
            0x1B => {
                answer.bx = Some(self.state(state::SENSITIVITY));
                answer.cx = Some(self.state(state::SENSITIVITY + 2));
                answer.dx = Some(self.state(state::SENSITIVITY + 4));
            }
            0x1E => answer.bx = Some(self.state(state::PAGE)),
            0x22 => self.set_state(state::LANGUAGE, bx),
            0x23 => answer.bx = Some(self.state(state::LANGUAGE)),
            _ => {
                answer.bx = Some(VERSION);
                answer.cx = Some(u16::from_le_bytes([self.get8(at::LINE), SERIAL]));
            }
        }
    }

    /// Function `1Fh`: the cursor hidden, the port let go, and INT 33h's
    /// vector before the driver handed back in ES:BX.
    fn disable(&mut self, answer: &mut Answer) {
        if self.get8(at::ENABLED) == 0 {
            answer.ax = Some(0xFFFF);
            return;
        }
        self.set_signed(state::VISIBLE, -1);
        self.erase();
        self.disconnect();
        let old = self.far(at::OLD_33);
        answer.ax = Some(0x001F);
        answer.bx = Some(old.offset);
        answer.es = Some(old.segment);
    }

    /// The alternate handler for `mask`, if one is set.
    fn alternate(&mut self, mask: u16) -> Option<Far> {
        (0..3).find_map(|n| {
            let at = at::STATE + state::ALTERNATES + n * state::ALTERNATE_LEN;
            (self.get16(at) == mask && mask != 0).then(|| self.far(at + 2))
        })
    }

    /// Sets the alternate handler for `mask`, which names a shift key, in
    /// its own slot or a free one; a null handler clears it. `false` when
    /// no slot is left or the mask names no shift key.
    fn set_alternate(&mut self, mask: u16, handler: Far) -> bool {
        if mask & event::KEYS == 0 {
            return false;
        }
        let slot = |n: u16| at::STATE + state::ALTERNATES + n * state::ALTERNATE_LEN;
        let found = (0..3)
            .find(|&n| self.get16(slot(n)) == mask)
            .or_else(|| (0..3).find(|&n| self.get16(slot(n)) == 0));
        let Some(n) = found else {
            return false;
        };
        let clear = handler == Far::default();
        self.set16(slot(n), if clear { 0 } else { mask });
        self.set_far(slot(n) + 2, if clear { Far::default() } else { handler });
        true
    }
}
