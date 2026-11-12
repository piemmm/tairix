//! The shell's INT 24h and INT 23h handlers, each a root on the shell's own
//! stack. INT 24h says what failed and asks how to go on, through the
//! console functions a critical-error handler may call. INT 23h asks
//! whether a batch file running is to end, then stops the command broken
//! into: the shell's own, which goes back to the prompt, or a program's,
//! which DOS ends.

use super::{at, flag, letter, set_flag, Flow, Frame, Id, Int, Program, Returned, Routine};
use crate::dos::engine::Saved;
use crate::dos::fs::critical;
use crate::dos::program::{say_console, Key, ENDED};
use crate::dos::rom::entry;
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;

/// The words DOS 5 names each device error by.
const ERRORS: [&[u8]; 22] = [
    b"Write protect error",
    b"Invalid unit",
    b"Not ready",
    b"Invalid device request",
    b"Data error",
    b"Invalid device request parameters",
    b"Seek error",
    b"Invalid media type",
    b"Sector not found",
    b"Printer out of paper error",
    b"Write fault error",
    b"Read fault error",
    b"General failure",
    b"Sharing violation",
    b"Lock violation",
    b"Invalid disk change",
    b"FCB unavailable",
    b"System resource exhausted",
    b"Code page mismatch",
    b"Out of input",
    b"Insufficient disk space",
    b"Critical error",
];

/// The words DOS names device error `code`, from 0, by: the last for one
/// past them.
pub(crate) fn device_words(code: usize) -> &'static [u8] {
    ERRORS[code.min(ERRORS.len() - 1)]
}

locals! {
    /// Points the standard input and output of the process a handler
    /// interrupted at its standard error, the console, while it asks
    /// something, as COMMAND.COM's handlers do: a stage redirected to a file
    /// or a failing device would take the question and answer it. The
    /// process's own two are duplicated first and put back from there, so
    /// its handle table counts every reference it holds, and its end closes
    /// them all however it ends. `1` says the swap was made, `0` that the
    /// console was in place already or could not be put there.
    ///
    /// `undo` puts the two back: a swap of the process running now, as the
    /// handler that made it finishes or the shell finds one a break left;
    /// one of a process that has ended since is forgotten.
    pub struct Console {
        undo: bool,
        psp: u16,
    }
}

mod swap {
    pub const PSP: u8 = 1;
    pub const INPUT_KEPT: u8 = 2;
    pub const OUTPUT_KEPT: u8 = 3;
    pub const INPUT_SET: u8 = 4;
    pub const OUTPUT_SET: u8 = 5;
    pub const UNDO_PSP: u8 = 6;
    pub const INPUT_BACK: u8 = 7;
    pub const OUTPUT_BACK: u8 = 8;
    pub const INPUT_CLOSED: u8 = 9;
    pub const OUTPUT_CLOSED: u8 = 10;
}

impl Console {
    #[must_use]
    pub const fn swap() -> Self {
        Self {
            undo: false,
            psp: 0,
        }
    }

    #[must_use]
    pub const fn undo() -> Self {
        Self { undo: true, psp: 0 }
    }

    fn call(p: &mut Program<'_, '_>, phase: &mut u8, next: u8, ax: u16, bx: u16, cx: u16) -> Flow {
        *phase = next;
        Flow::Call(Frame::of(&Int {
            bx,
            cx,
            ..Int::dos(p.own, ax)
        }))
    }

    /// The kept handles, input then output.
    fn kept(p: &mut Program<'_, '_>) -> (u16, u16) {
        (
            u16::from(p.get8(at::ASKING)),
            u16::from(p.get8(at::ASKING + 1)),
        )
    }
}

impl Routine for Console {
    const ID: Id = Id::Console;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if let Err(fault @ (Fault::Abort | Fault::Break)) = child {
            return Flow::Return(Err(fault));
        }
        let handle = |child: Outcome| child.ok().and_then(|ax| u8::try_from(ax).ok());
        match *phase {
            0 if self.undo => Self::call(p, phase, swap::UNDO_PSP, 0x6200, 0, 0),
            0 => Self::call(p, phase, swap::PSP, 0x6200, 0, 0),
            swap::PSP => {
                self.psp = p.result(Returned::Bx);
                if p.get16(at::ASKING_PSP) == self.psp {
                    return Flow::Return(Ok(0));
                }
                p.set16(at::ASKING_PSP, 0);
                Self::call(p, phase, swap::INPUT_KEPT, 0x4500, 0, 0)
            }
            swap::INPUT_KEPT => {
                let Some(input) = handle(child) else {
                    return Flow::Return(Ok(0));
                };
                p.set8(at::ASKING, input);
                Self::call(p, phase, swap::OUTPUT_KEPT, 0x4500, 1, 0)
            }
            swap::OUTPUT_KEPT => {
                let Some(output) = handle(child) else {
                    let input = u16::from(p.get8(at::ASKING));
                    return Self::call(p, phase, swap::OUTPUT_CLOSED, 0x3E00, input, 0);
                };
                p.set8(at::ASKING + 1, output);
                p.set16(at::ASKING_PSP, self.psp);
                Self::call(p, phase, swap::INPUT_SET, 0x4600, 2, 0)
            }
            swap::INPUT_SET if child.is_ok() => {
                Self::call(p, phase, swap::OUTPUT_SET, 0x4600, 2, 1)
            }
            swap::OUTPUT_SET if child.is_ok() => Flow::Return(Ok(1)),
            swap::INPUT_SET | swap::OUTPUT_SET => {
                let (input, _) = Self::kept(p);
                Self::call(p, phase, swap::INPUT_BACK, 0x4600, input, 0)
            }
            swap::UNDO_PSP => {
                let asked = p.get16(at::ASKING_PSP);
                if asked == 0 {
                    return Flow::Return(Ok(0));
                }
                if asked != p.result(Returned::Bx) {
                    p.set16(at::ASKING_PSP, 0);
                    return Flow::Return(Ok(0));
                }
                let (input, _) = Self::kept(p);
                Self::call(p, phase, swap::INPUT_BACK, 0x4600, input, 0)
            }
            swap::INPUT_BACK => {
                let (_, output) = Self::kept(p);
                Self::call(p, phase, swap::OUTPUT_BACK, 0x4600, output, 1)
            }
            swap::OUTPUT_BACK => {
                let (input, _) = Self::kept(p);
                Self::call(p, phase, swap::INPUT_CLOSED, 0x3E00, input, 0)
            }
            swap::INPUT_CLOSED => {
                let (_, output) = Self::kept(p);
                Self::call(p, phase, swap::OUTPUT_CLOSED, 0x3E00, output, 0)
            }
            _ => {
                p.set16(at::ASKING_PSP, 0);
                Flow::Return(Ok(0))
            }
        }
    }
}

locals! {
    /// The shell's INT 24h: `<error> reading|writing drive X` or `device
    /// NAME`, then the answers DOS allows, asked until one is given, all on
    /// the console.
    pub struct Critical {
        swapped: bool,
    }
}

/// [`Critical`]'s phases.
mod critical_phase {
    pub const ON_CONSOLE: u8 = 1;
    pub const TOLD: u8 = 2;
    pub const ASKED: u8 = 3;
    pub const ANSWERED: u8 = 4;
    pub const DONE: u8 = 5;
}

impl Critical {
    /// The answers the error allows, as the question lists them.
    fn options(flags: u8) -> Vec<u8> {
        let mut out = b"Abort".to_vec();
        for (allowed, word) in [
            (critical::RETRY, &b"Retry"[..]),
            (critical::IGNORE, b"Ignore"),
            (critical::FAIL, b"Fail"),
        ] {
            if flags & allowed != 0 {
                out.extend_from_slice(b", ");
                out.extend_from_slice(word);
            }
        }
        out.push(b'?');
        out
    }
}

impl Routine for Critical {
    const ID: Id = Id::Critical;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        use critical_phase as phase;
        let [drive, flags] = p.caller(Saved::Ax).to_le_bytes();
        match *phase {
            0 => {
                *phase = phase::ON_CONSOLE;
                Flow::Call(Frame::of(&Console::swap()))
            }
            phase::ON_CONSOLE => {
                self.swapped = take!(child) == 1;
                let code = usize::from(p.caller(Saved::Di).to_le_bytes()[0]);
                let mut text = b"\r\n".to_vec();
                text.extend_from_slice(device_words(code));
                text.extend_from_slice(if flags & critical::WRITING != 0 {
                    b" writing "
                } else {
                    b" reading "
                });
                if flags & critical::NOT_DISK == 0 {
                    text.extend_from_slice(b"drive ");
                    text.push(letter(drive));
                } else {
                    let (segment, offset) = (p.caller(Saved::Bp), p.caller(Saved::Si));
                    let mut name = [0u8; 8];
                    p.g.read_bytes(segment, offset.wrapping_add(10), &mut name);
                    let shown: Vec<u8> = name
                        .iter()
                        .copied()
                        .filter(|&b| b != b' ' && b != b'$')
                        .collect();
                    text.extend_from_slice(b"device ");
                    text.extend_from_slice(&shown);
                }
                text.extend_from_slice(b"\r\n");
                text.extend_from_slice(&Self::options(flags));
                *phase = phase::TOLD;
                Flow::Call(say_console(p, &text))
            }
            // Keys typed before the question are no answer to it.
            phase::TOLD => {
                *phase = phase::ASKED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            // A console with no answer left fails the call where it may,
            // neither retrying it nor passing it off as done.
            phase::ASKED => {
                let read = take!(child);
                let action = if read == ENDED {
                    Some(if flags & critical::FAIL != 0 {
                        critical::ACTION_FAIL
                    } else {
                        critical::ACTION_ABORT
                    })
                } else {
                    match read.to_le_bytes()[0].to_ascii_uppercase() {
                        b'A' => Some(critical::ACTION_ABORT),
                        b'R' if flags & critical::RETRY != 0 => Some(critical::ACTION_RETRY),
                        b'I' if flags & critical::IGNORE != 0 => Some(critical::ACTION_IGNORE),
                        b'F' if flags & critical::FAIL != 0 => Some(critical::ACTION_FAIL),
                        _ => None,
                    }
                };
                let Some(action) = action else {
                    *phase = phase::TOLD;
                    let again = [&b"\r\n"[..], &Self::options(flags)].concat();
                    return Flow::Call(say_console(p, &again));
                };
                let ax = p.caller(Saved::Ax);
                p.set_caller(Saved::Ax, (ax & 0xFF00) | u16::from(action));
                *phase = phase::ANSWERED;
                Flow::Call(say_console(p, b"\r\n"))
            }
            phase::ANSWERED if self.swapped => {
                *phase = phase::DONE;
                Flow::Call(Frame::of(&Console::undo()))
            }
            _ => Flow::Return(Ok(0)),
        }
    }
}

locals! {
    /// The shell's INT 23h.
    pub struct Breaker {
        swapped: bool,
    }
}

mod breaking {
    pub const ON_CONSOLE: u8 = 1;
    pub const ASKED: u8 = 2;
    pub const ANSWERED: u8 = 3;
    pub const ANSWER_SHOWN: u8 = 4;
    pub const SAID: u8 = 5;
    pub const WHOSE: u8 = 6;
    pub const SWAPPED: u8 = 7;
}

impl Routine for Breaker {
    const ID: Id = Id::Breaker;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 if p.get16(at::BATCH) == 0 => {
                *phase = breaking::SAID;
                self.step(p, phase, Ok(0))
            }
            0 => {
                *phase = breaking::SWAPPED;
                Flow::Call(Frame::of(&Console::swap()))
            }
            breaking::SWAPPED => {
                self.swapped = take!(child) == 1;
                *phase = breaking::ON_CONSOLE;
                self.step(p, phase, Ok(0))
            }
            breaking::ON_CONSOLE => {
                *phase = breaking::ASKED;
                Flow::Call(super::say(p, b"\r\nTerminate batch job (Y/N)? "))
            }
            breaking::ASKED => {
                *phase = breaking::ANSWERED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            // A console with no answer left ends the batch job, as the break
            // asked.
            breaking::ANSWERED => {
                let read = take!(child);
                let key = if read == ENDED {
                    b'Y'
                } else {
                    read.to_le_bytes()[0].to_ascii_uppercase()
                };
                match key {
                    b'Y' => set_flag(p, flag::BATCH_BROKEN, true),
                    b'N' => {}
                    _ => {
                        *phase = breaking::ON_CONSOLE;
                        return self.step(p, phase, Ok(0));
                    }
                }
                *phase = breaking::ANSWER_SHOWN;
                Flow::Call(super::say(p, super::text::NEW_LINE))
            }
            breaking::ANSWER_SHOWN if self.swapped => {
                *phase = breaking::SAID;
                Flow::Call(Frame::of(&Console::undo()))
            }
            breaking::ANSWER_SHOWN => {
                *phase = breaking::SAID;
                self.step(p, phase, Ok(0))
            }
            breaking::SAID => {
                *phase = breaking::WHOSE;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x6200)))
            }
            _ => {
                if p.result(Returned::Bx) == p.own {
                    return p.abandon();
                }
                p.set_exit(entry::PROGRAM_EXIT_BREAK);
                Flow::Return(Ok(0))
            }
        }
    }
}
