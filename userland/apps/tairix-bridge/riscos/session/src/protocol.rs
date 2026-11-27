//! The protocol between the front end and a session's worker
//! (`plans/RISCOS-EMULATOR.md` §15.2), carried over a `lib/sandbox` duplex
//! session whose spawn share is the screens' damage ring ([`crate::ring`]).
//!
//! - The front end sends [`Command`]s: run and hold, input for a context,
//!   the ring's space released, relays answered, launches and quits.
//! - The worker sends [`Event`]s: input taken in, launches made or refused,
//!   screens and their damage, relays to carry, and contexts ended.
//!
//! Every decoder is total: a payload that is not exactly one message within
//! its bounds is a [`WireError`], and the side that reads one ends the
//! session.

use tairix_riscos_arm::cpu::Unmodelled;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::os::Ended;
use tairix_riscos_kernel::relay::Side;
use tairix_sandbox::proto::{FRAME_HEADER_LEN, MAX_FRAME};
use tairix_sandbox::wire::{Reader, Room, WireError, Writer, BYTES_PREFIX};

/// A context as the protocol names it: the worker numbers each context it
/// makes above the one before, and never gives a number twice.
pub type Context = u32;

/// The context no launch makes: the boot's, which the session's own relays
/// are made for.
pub const BOOT: Context = 0;

/// The most bytes a relay's request or reply carries.
pub const RELAY_MOST: usize = 1 << 20;
/// The largest payload either side sends: a relay's request and its tag,
/// serial, context, flag and side.
pub const PAYLOAD_MOST: usize = RELAY_MOST + 1 + 4 + 4 + 1 + 1 + BYTES_PREFIX;
/// The most launches and quits a front end has unanswered: what the worker
/// keeps while they wait for the session to let them be made.
pub const WAITING_MOST: usize = 64;
/// The longest command line a launch carries, its terminator apart.
pub const LINE_MOST: usize = tairix_obey::command::MOST_LINE - 1;
/// The most pixels across or down a screen has.
pub const SCREEN_MOST: u32 = 1 << 14;
/// The largest eigen factor a screen has.
const EIG_MOST: u32 = 3;
/// The mouse buttons an input event holds: Adjust, Menu and Select.
const BUTTONS: u32 = 0b111;

const _: () = assert!(PAYLOAD_MOST + FRAME_HEADER_LEN <= MAX_FRAME);

/// What the front end sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command<'a> {
    /// The session's contexts run.
    Run,
    /// Every context holds where it stands.
    Hold,
    /// An input event for `context`, within its credit
    /// ([`crate::credit`]).
    Input {
        /// The context.
        context: Context,
        /// The event.
        event: InputEvent,
    },
    /// The damage ring's oldest `bytes` copied out and given back.
    Released {
        /// The bytes.
        bytes: u32,
    },
    /// The reply to relay `serial`.
    Answer {
        /// The relay.
        serial: u32,
        /// Its reply.
        reply: &'a [u8],
    },
    /// Relay `serial` could not be carried.
    Unanswered {
        /// The relay.
        serial: u32,
    },
    /// A new context, its screen made, running `line`: one of the
    /// [`WAITING_MOST`] launches and quits unanswered at most.
    Launch {
        /// The launch, as its answer names it.
        serial: u32,
        /// The command line, no control character in it.
        line: &'a [u8],
    },
    /// `context`, one launched, ended, its program with it: one of the
    /// [`WAITING_MOST`] launches and quits unanswered at most.
    Quit {
        /// The context.
        context: Context,
    },
}

/// A context's screen as it stands: what its damage is drawn on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Screen {
    /// Pixels across, from one to [`SCREEN_MOST`].
    pub width: u32,
    /// Pixels down, likewise.
    pub height: u32,
    /// A pixel is `1 << x_eig` OS units across, the aspect it is scaled to
    /// with `y_eig`.
    pub x_eig: u32,
    /// And `1 << y_eig` up.
    pub y_eig: u32,
    /// The border's colour, as `&RRGGBB`.
    pub border: u32,
    /// The screen is blanked: shown black.
    pub blanked: bool,
}

/// An error as the guest gave it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error<'a> {
    /// Its number.
    pub number: u32,
    /// Its text, at most an error block's, no zero in it.
    pub text: &'a [u8],
}

/// How a context ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ending<'a> {
    /// Its program exited with this return code.
    Exit {
        /// The code.
        code: u32,
    },
    /// An error reached the default error handler.
    Error(Error<'a>),
    /// The kernel could not carry it on.
    Fault,
    /// Its program reset the machine.
    Reset {
        /// A hard reset.
        hard: bool,
    },
    /// It reached something the processor does not model.
    Unmodelled(Unmodelled),
    /// The front end quit it.
    Quit,
}

impl Ending<'_> {
    /// The ending a program's `ended` is, its error's `text` read from
    /// guest memory.
    #[must_use]
    pub const fn of(ended: Ended, text: &[u8]) -> Ending<'_> {
        match ended {
            Ended::Exit { code } => Ending::Exit { code },
            Ended::Error { number, .. } => Ending::Error(Error { number, text }),
            Ended::Fault => Ending::Fault,
            Ended::Reset { hard, .. } => Ending::Reset { hard },
        }
    }
}

/// What the worker sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event<'a> {
    /// `context` has taken in `applied` of the input events it was sent,
    /// counted from its making, modulo 2³².
    Credit {
        /// The context.
        context: Context,
        /// The events taken in.
        applied: u32,
    },
    /// Launch `serial` made `context`, before its command line runs: every
    /// event of the context follows this, and its [`Event::Ended`] comes
    /// last.
    Launched {
        /// The launch.
        serial: u32,
        /// Its context.
        context: Context,
    },
    /// Launch `serial` made no context: the error that refused it, none
    /// where the session had no room.
    Refused {
        /// The launch.
        serial: u32,
        /// The error.
        error: Option<Error<'a>>,
    },
    /// `context`'s screen as it now stands.
    Screen {
        /// The context.
        context: Context,
        /// The screen.
        screen: Screen,
    },
    /// `lines` lines of `context`'s screen from line `first`, each its
    /// pixels as `&RRGGBB` words, in the damage ring at `offset`.
    Damage {
        /// The context.
        context: Context,
        /// The first line, from the top.
        first: u32,
        /// The lines, at least one.
        lines: u32,
        /// Where they are in the ring.
        offset: u32,
    },
    /// A relay for the front end to carry to `side`, answered as `serial`.
    /// One the worker waits for in place is the only one of its kind
    /// unanswered, as the worker does nothing else until it is answered; a
    /// context has one at most unanswered that it is parked on.
    Relay {
        /// The relay.
        serial: u32,
        /// The context it is made for: the boot's, `0`, for the session's
        /// own.
        context: Context,
        /// Whether its context is parked on it, the session's other contexts
        /// running meanwhile, rather than the worker waiting for it.
        parked: bool,
        /// Where it goes.
        side: Side,
        /// Its request.
        request: &'a [u8],
    },
    /// `context` ended.
    Ended {
        /// The context.
        context: Context,
        /// How.
        ending: Ending<'a>,
    },
}

mod tag {
    pub(super) const RUN: u8 = 1;
    pub(super) const HOLD: u8 = 2;
    pub(super) const INPUT: u8 = 3;
    pub(super) const RELEASED: u8 = 4;
    pub(super) const ANSWER: u8 = 5;
    pub(super) const UNANSWERED: u8 = 6;
    pub(super) const LAUNCH: u8 = 7;
    pub(super) const QUIT: u8 = 8;

    pub(super) const CREDIT: u8 = 1;
    pub(super) const LAUNCHED: u8 = 2;
    pub(super) const REFUSED: u8 = 3;
    pub(super) const SCREEN: u8 = 4;
    pub(super) const DAMAGE: u8 = 5;
    pub(super) const RELAY: u8 = 6;
    pub(super) const ENDED: u8 = 7;

    pub(super) const KEY: u8 = 1;
    pub(super) const TRANSITION: u8 = 2;
    pub(super) const POINTER: u8 = 3;
    pub(super) const MOTION: u8 = 4;

    pub(super) const FILES: u8 = 0;
    pub(super) const FONTS: u8 = 1;

    pub(super) const EXIT: u8 = 1;
    pub(super) const ERROR: u8 = 2;
    pub(super) const FAULT: u8 = 3;
    pub(super) const RESET: u8 = 4;
    pub(super) const UNMODELLED: u8 = 5;
    pub(super) const QUITTED: u8 = 6;
}

fn flag(r: &mut Reader<'_>) -> Result<bool, WireError> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(WireError::Malformed),
    }
}

fn put_flag<R: Room>(w: &mut Writer<R>, on: bool) {
    w.u8(u8::from(on));
}

fn buttons(r: &mut Reader<'_>) -> Result<u32, WireError> {
    let buttons = r.u32()?;
    if buttons & !BUTTONS != 0 {
        return Err(WireError::Malformed);
    }
    Ok(buttons)
}

fn put_event<R: Room>(w: &mut Writer<R>, event: InputEvent) {
    match event {
        InputEvent::Key(code) => w.raw(&[tag::KEY, code]),
        InputEvent::Transition { key, down } => {
            w.u8(tag::TRANSITION);
            w.u32(key);
            put_flag(w, down);
        }
        InputEvent::Pointer { x, y, buttons } => {
            w.u8(tag::POINTER);
            w.i32(x);
            w.i32(y);
            w.u32(buttons);
        }
        InputEvent::Motion { dx, dy, buttons } => {
            w.u8(tag::MOTION);
            w.i32(dx);
            w.i32(dy);
            w.u32(buttons);
        }
    }
}

fn event(r: &mut Reader<'_>) -> Result<InputEvent, WireError> {
    Ok(match r.u8()? {
        tag::KEY => InputEvent::Key(r.u8()?),
        tag::TRANSITION => InputEvent::Transition {
            key: r.u32()?,
            down: flag(r)?,
        },
        tag::POINTER => InputEvent::Pointer {
            x: r.i32()?,
            y: r.i32()?,
            buttons: buttons(r)?,
        },
        tag::MOTION => InputEvent::Motion {
            dx: r.i32()?,
            dy: r.i32()?,
            buttons: buttons(r)?,
        },
        _ => return Err(WireError::Malformed),
    })
}

/// Whether a launch carries `line`: at most [`LINE_MOST`] bytes, none of
/// them a control character, which would end it part way.
#[must_use]
pub fn carries(line: &[u8]) -> bool {
    line.len() <= LINE_MOST && !line.iter().any(|&b| b < b' ' || b == 0x7F)
}

/// A command line a launch carries.
fn line<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], WireError> {
    let line = r.bytes(LINE_MOST)?;
    if !carries(line) {
        return Err(WireError::Malformed);
    }
    Ok(line)
}

impl Command<'_> {
    /// The command appended to `w`.
    pub fn encode<R: Room>(&self, w: &mut Writer<R>) {
        match *self {
            Self::Run => w.u8(tag::RUN),
            Self::Hold => w.u8(tag::HOLD),
            Self::Input { context, event } => {
                w.u8(tag::INPUT);
                w.u32(context);
                put_event(w, event);
            }
            Self::Released { bytes } => {
                w.u8(tag::RELEASED);
                w.u32(bytes);
            }
            Self::Answer { serial, reply } => {
                w.u8(tag::ANSWER);
                w.u32(serial);
                w.bytes(reply);
            }
            Self::Unanswered { serial } => {
                w.u8(tag::UNANSWERED);
                w.u32(serial);
            }
            Self::Launch { serial, line } => {
                w.u8(tag::LAUNCH);
                w.u32(serial);
                w.bytes(line);
            }
            Self::Quit { context } => {
                w.u8(tag::QUIT);
                w.u32(context);
            }
        }
    }
}

impl<'a> Command<'a> {
    /// The command `payload` is, whole.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a payload that is not exactly one command within
    /// its bounds.
    pub fn decode(payload: &'a [u8]) -> Result<Self, WireError> {
        Reader::new(payload).whole(|r| {
            Ok(match r.u8()? {
                tag::RUN => Self::Run,
                tag::HOLD => Self::Hold,
                tag::INPUT => Self::Input {
                    context: r.u32()?,
                    event: event(r)?,
                },
                tag::RELEASED => Self::Released { bytes: r.u32()? },
                tag::ANSWER => Self::Answer {
                    serial: r.u32()?,
                    reply: r.bytes(RELAY_MOST)?,
                },
                tag::UNANSWERED => Self::Unanswered { serial: r.u32()? },
                tag::LAUNCH => Self::Launch {
                    serial: r.u32()?,
                    line: line(r)?,
                },
                tag::QUIT => Self::Quit { context: r.u32()? },
                _ => return Err(WireError::Malformed),
            })
        })
    }
}

fn put_side<R: Room>(w: &mut Writer<R>, side: Side) {
    w.u8(match side {
        Side::Files => tag::FILES,
        Side::Fonts => tag::FONTS,
    });
}

fn side(r: &mut Reader<'_>) -> Result<Side, WireError> {
    match r.u8()? {
        tag::FILES => Ok(Side::Files),
        tag::FONTS => Ok(Side::Fonts),
        _ => Err(WireError::Malformed),
    }
}

fn put_screen<R: Room>(w: &mut Writer<R>, screen: &Screen) {
    for word in [
        screen.width,
        screen.height,
        screen.x_eig,
        screen.y_eig,
        screen.border,
    ] {
        w.u32(word);
    }
    put_flag(w, screen.blanked);
}

fn screen(r: &mut Reader<'_>) -> Result<Screen, WireError> {
    let screen = Screen {
        width: r.u32()?,
        height: r.u32()?,
        x_eig: r.u32()?,
        y_eig: r.u32()?,
        border: r.u32()?,
        blanked: flag(r)?,
    };
    let sized = |side: u32| (1..=SCREEN_MOST).contains(&side);
    if !sized(screen.width)
        || !sized(screen.height)
        || screen.x_eig > EIG_MOST
        || screen.y_eig > EIG_MOST
        || screen.border > 0xFF_FFFF
    {
        return Err(WireError::Malformed);
    }
    Ok(screen)
}

fn put_error<R: Room>(w: &mut Writer<R>, error: &Error<'_>) {
    w.u32(error.number);
    w.bytes(error.text);
}

fn error<'a>(r: &mut Reader<'a>) -> Result<Error<'a>, WireError> {
    let number = r.u32()?;
    let text = r.bytes(tairix_riscos_kernel::error::TEXT)?;
    if text.contains(&0) {
        return Err(WireError::Malformed);
    }
    Ok(Error { number, text })
}

fn put_ending<R: Room>(w: &mut Writer<R>, ending: &Ending<'_>) {
    match *ending {
        Ending::Exit { code } => {
            w.u8(tag::EXIT);
            w.u32(code);
        }
        Ending::Error(ref error) => {
            w.u8(tag::ERROR);
            put_error(w, error);
        }
        Ending::Fault => w.u8(tag::FAULT),
        Ending::Reset { hard } => {
            w.u8(tag::RESET);
            put_flag(w, hard);
        }
        Ending::Unmodelled(Unmodelled::System { register, write }) => {
            w.u8(tag::UNMODELLED);
            w.raw(&register);
            put_flag(w, write);
        }
        Ending::Quit => w.u8(tag::QUITTED),
    }
}

fn ending<'a>(r: &mut Reader<'a>) -> Result<Ending<'a>, WireError> {
    Ok(match r.u8()? {
        tag::EXIT => Ending::Exit { code: r.u32()? },
        tag::ERROR => Ending::Error(error(r)?),
        tag::FAULT => Ending::Fault,
        tag::RESET => Ending::Reset { hard: flag(r)? },
        tag::UNMODELLED => {
            let mut register = [0; 4];
            register.copy_from_slice(r.take(4)?);
            Ending::Unmodelled(Unmodelled::System {
                register,
                write: flag(r)?,
            })
        }
        tag::QUITTED => Ending::Quit,
        _ => return Err(WireError::Malformed),
    })
}

impl Event<'_> {
    /// The event appended to `w`.
    pub fn encode<R: Room>(&self, w: &mut Writer<R>) {
        match *self {
            Self::Credit { context, applied } => {
                w.u8(tag::CREDIT);
                w.u32(context);
                w.u32(applied);
            }
            Self::Launched { serial, context } => {
                w.u8(tag::LAUNCHED);
                w.u32(serial);
                w.u32(context);
            }
            Self::Refused { serial, error } => {
                w.u8(tag::REFUSED);
                w.u32(serial);
                put_flag(w, error.is_some());
                if let Some(error) = error {
                    put_error(w, &error);
                }
            }
            Self::Screen { context, screen } => {
                w.u8(tag::SCREEN);
                w.u32(context);
                put_screen(w, &screen);
            }
            Self::Damage {
                context,
                first,
                lines,
                offset,
            } => {
                w.u8(tag::DAMAGE);
                for word in [context, first, lines, offset] {
                    w.u32(word);
                }
            }
            Self::Relay {
                serial,
                context,
                parked,
                side,
                request,
            } => {
                w.u8(tag::RELAY);
                w.u32(serial);
                w.u32(context);
                put_flag(w, parked);
                put_side(w, side);
                w.bytes(request);
            }
            Self::Ended { context, ending } => {
                w.u8(tag::ENDED);
                w.u32(context);
                put_ending(w, &ending);
            }
        }
    }
}

impl<'a> Event<'a> {
    /// The event `payload` is, whole.
    ///
    /// # Errors
    ///
    /// [`WireError`] for a payload that is not exactly one event within its
    /// bounds.
    pub fn decode(payload: &'a [u8]) -> Result<Self, WireError> {
        Reader::new(payload).whole(|r| {
            Ok(match r.u8()? {
                tag::CREDIT => Self::Credit {
                    context: r.u32()?,
                    applied: r.u32()?,
                },
                tag::LAUNCHED => Self::Launched {
                    serial: r.u32()?,
                    context: r.u32()?,
                },
                tag::REFUSED => {
                    let serial = r.u32()?;
                    let error = if flag(r)? { Some(error(r)?) } else { None };
                    Self::Refused { serial, error }
                }
                tag::SCREEN => Self::Screen {
                    context: r.u32()?,
                    screen: screen(r)?,
                },
                tag::DAMAGE => {
                    let (context, first, lines, offset) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
                    if lines == 0 || first.checked_add(lines).is_none_or(|end| end > SCREEN_MOST) {
                        return Err(WireError::Malformed);
                    }
                    Self::Damage {
                        context,
                        first,
                        lines,
                        offset,
                    }
                }
                tag::RELAY => Self::Relay {
                    serial: r.u32()?,
                    context: r.u32()?,
                    parked: flag(r)?,
                    side: side(r)?,
                    request: r.bytes(RELAY_MOST)?,
                },
                tag::ENDED => Self::Ended {
                    context: r.u32()?,
                    ending: ending(r)?,
                },
                _ => return Err(WireError::Malformed),
            })
        })
    }
}

#[cfg(test)]
mod tests;
