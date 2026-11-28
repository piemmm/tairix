//! The protocol between the front end and a session's worker
//! (`plans/RISCOS-EMULATOR.md` §15.2), carried over a `lib/sandbox` duplex
//! session whose spawn share is the screens' damage ring ([`crate::ring`]).
//!
//! - The front end sends [`Command`]s: the session's [`Begin`] first, then
//!   run and hold, input for a context, the ring's space released, relays
//!   answered, the boot, launches, quits and pauses, the machine's memory
//!   pressure and the desktop's text.
//! - The worker sends [`Event`]s: input taken in, launches made or refused,
//!   screens, their damage and what is drawn over them, the guest setting its
//!   pointer, the memory each context and the session hold, relays to carry,
//!   the boot's failures, and contexts ended.
//!
//! Every decoder is total: a payload that is not exactly one message within
//! its bounds is a [`WireError`], and the side that reads one ends the
//! session.

use tairix_abi::font_ipc::{FamilyKey, FONT_FAMILY_KEY_LEN};
use tairix_abi::zone::{is_zone_name, ZONE_NAME_MAX};
use tairix_reclaim::PressureBand;
use tairix_riscos_arm::cpu::Unmodelled;
use tairix_riscos_arm::mmu::PAGE_SIZE;
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::input::{buttons, InputEvent};
use tairix_riscos_kernel::memory::policy::MOST_PAGES;
use tairix_riscos_kernel::memory::Shared;
use tairix_riscos_kernel::os::Ended;
use tairix_riscos_kernel::relay::Side;
pub use tairix_riscos_vdu::display::Flash;
use tairix_riscos_vdu::display::{POINTER_HEIGHT_MOST, POINTER_WIDTH_MOST};
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
/// The most bytes of zone store a [`Begin`] carries: what a relay carries,
/// so the start costs the worker no larger frame than its relays do.
pub const ZONE_STORE_MOST: usize = RELAY_MOST;
/// The most decoded pages a [`Begin`] has each processor keep.
pub const MEMO_MOST: u32 = 1 << 12;
/// The largest payload a relay or its answer takes: its tag, serial,
/// context, flag and side, and its bytes.
const RELAY_PAYLOAD: usize = RELAY_MOST + 1 + 4 + 4 + 1 + 1 + BYTES_PREFIX;
/// The largest [`Begin`]: its tag, sizes and real time, its zone's flag,
/// store and name, its keyboard, desktop font and pressure.
const BEGIN_PAYLOAD: usize = 1
    + 3 * 4
    + 8
    + 1
    + 2 * BYTES_PREFIX
    + ZONE_STORE_MOST
    + ZONE_NAME_MAX
    + 4
    + FONT_FAMILY_KEY_LEN
    + 2
    + 1;
/// The largest payload either side sends.
pub const PAYLOAD_MOST: usize = if BEGIN_PAYLOAD > RELAY_PAYLOAD {
    BEGIN_PAYLOAD
} else {
    RELAY_PAYLOAD
};
/// The largest frame either side sends, header and all.
pub const FRAME_MOST: usize = PAYLOAD_MOST + FRAME_HEADER_LEN;
/// The most launches and quits a front end has unanswered: what the worker
/// keeps while they wait for the session to let them be made.
pub const WAITING_MOST: usize = 64;
/// The longest command line a launch carries, its terminator apart.
pub const LINE_MOST: usize = tairix_obey::command::MOST_LINE - 1;
/// The most pixels across or down a screen has.
pub const SCREEN_MOST: u32 = 1 << 14;
/// The most failures the boot tells: those its report keeps.
pub const BOOT_FAILURES_MOST: usize = tairix_riscos_filing::boot::FAILURES_MOST;
/// The largest eigen factor a screen has.
const EIG_MOST: u32 = 3;
/// The most a colour word holds: `&RRGGBB`.
const COLOUR_MOST: u32 = 0xFF_FFFF;
/// The colours a pointer's pixel may be: clear, then its three.
const POINTER_COLOURS: u8 = 3;

const _: () = assert!(FRAME_MOST <= MAX_FRAME);

/// Why a worker that ended by itself stopped, beside the ends its sandbox
/// names, carried as its exit status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stopped {
    /// The front end sent what the protocol does not allow.
    Violation,
    /// The kernel would not start.
    Refused,
    /// The kernel faulted.
    Fault,
}

impl Stopped {
    /// The exit status it is carried as: none the sandbox's own ends use.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Violation => 16,
            Self::Refused => 17,
            Self::Fault => 18,
        }
    }

    /// The stop exit status `code` carries, if it is one.
    #[must_use]
    pub const fn of_code(code: i32) -> Option<Self> {
        match code {
            16 => Some(Self::Violation),
            17 => Some(Self::Refused),
            18 => Some(Self::Fault),
            _ => None,
        }
    }

    /// A terse statement of it, for the user.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Violation => "the session was sent what it could not take",
            Self::Refused => "RISC OS would not start",
            Self::Fault => "RISC OS faulted",
        }
    }
}

/// What a session starts with: the first command, sent once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Begin<'a> {
    /// The pages of RAM the guest has, at most a 32-bit physical address
    /// space's.
    pub ram_pages: u32,
    /// The decoded pages each processor keeps, from one to [`MEMO_MOST`].
    pub memo_pages: u32,
    /// The bytes of the slot each launch is given: whole pages, at least
    /// one.
    pub slot_bytes: u32,
    /// The real time the session starts at, in centiseconds since 1900.
    pub real_time: u64,
    /// The session's time zone, none for UTC.
    pub zone: Option<Zoned<'a>>,
    /// The RISC OS keyboard the host's keys are laid out as: a country's
    /// number, below 128.
    pub keyboard: u32,
    /// The TAIRiX desktop's interface text.
    pub font: DesktopFont,
    /// The machine's memory pressure.
    pub pressure: PressureBand,
}

/// A time zone: the store its rules are in, and its name there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Zoned<'a> {
    /// The store, at most [`ZONE_STORE_MOST`] bytes, which the worker reads
    /// with the zone reader.
    pub store: &'a [u8],
    /// The zone's name, of a zone name's shape.
    pub name: &'a str,
}

/// What the front end sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command<'a> {
    /// The session started: the first command, and only the first.
    Begin(Begin<'a>),
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
    /// A new context, its screen made, running the session's boot sequence:
    /// answered as a launch is, and one of the [`WAITING_MOST`] launches and
    /// quits unanswered at most.
    Boot {
        /// The launch, as its answer names it.
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
    /// `context`, one launched, held where it stands while the others run,
    /// or let go on.
    Pause {
        /// The context.
        context: Context,
        /// Held rather than let go.
        paused: bool,
    },
    /// The machine's memory pressure is now `band`.
    Pressure(PressureBand),
    /// The TAIRiX desktop's interface text is now this.
    Font(DesktopFont),
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

/// The text cursor drawn over a screen: the lines `top` to `bottom` of the
/// cell at `x`, `y`, `width` pixels across.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cursor {
    /// Its cell's left pixel, from the screen's left.
    pub x: u32,
    /// Its cell's top line, from the screen's top.
    pub y: u32,
    /// The pixels across it covers, at least one.
    pub width: u32,
    /// The first line of the cell it covers.
    pub top: u32,
    /// The last, no higher than the first.
    pub bottom: u32,
    /// How it flashes.
    pub flash: Flash,
}

/// A pointer's shape: `width` × `height` pixels, each a colour 0 to 3, nought
/// clear.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shape<'a> {
    /// Its pixels across, from one to the most a pointer has.
    pub width: u32,
    /// Its lines down, likewise.
    pub height: u32,
    /// The pixel within it the pointer's place names, as the guest set it.
    pub active: (u8, u8),
    /// Its colours 1 to 3, as `&RRGGBB`.
    pub colours: [u32; 3],
    /// Its pixels a row at a time, the top first.
    pub pixels: &'a [u8],
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
    /// Launch `serial` made `context`, before its command line or the boot
    /// runs: every event of the context follows this, and its
    /// [`Event::Ended`] comes last.
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
    /// `context`'s text cursor as it now stands, none where none is drawn:
    /// told after its screen.
    Cursor {
        /// The context.
        context: Context,
        /// The cursor.
        cursor: Option<Cursor>,
    },
    /// `context`'s pointer shape as it now stands, none where none is shown:
    /// told after its screen.
    Pointer {
        /// The context.
        context: Context,
        /// The shape.
        shape: Option<Shape<'a>>,
    },
    /// `context`'s pointer's active point is now on screen pixel `x`, `y`,
    /// from the screen's top left, which may lie beyond it.
    PointerAt {
        /// The context.
        context: Context,
        /// Across.
        x: i32,
        /// Down.
        y: i32,
    },
    /// `context`'s guest has set the mouse or the pointer itself since it
    /// was last told, so the front end's pointer no longer stands where the
    /// guest's does.
    PointerSet {
        /// The context.
        context: Context,
    },
    /// `context`'s hourglass now shows, or no longer does: its program says
    /// it is busy.
    Hourglass {
        /// The context.
        context: Context,
        /// Whether it shows.
        shown: bool,
    },
    /// What `context` holds of its own as it now stands, in bytes.
    Own {
        /// The context.
        context: Context,
        /// Its application slot.
        slot: u32,
        /// Its screen.
        screen: u32,
    },
    /// What the areas every context shares hold as they now stand.
    Shared(Shared),
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
    /// A line the boot `context` ran could not run: one of at most
    /// [`BOOT_FAILURES_MOST`], in the order met, told before its end.
    BootFailed {
        /// The context.
        context: Context,
        /// The line, at most [`LINE_MOST`] bytes.
        line: &'a [u8],
        /// The error it gave.
        error: Error<'a>,
    },
    /// The boot `context` met `count` failures more than it told, told once
    /// before its end.
    BootOmitted {
        /// The context.
        context: Context,
        /// How many.
        count: u32,
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
    pub(super) const BEGIN: u8 = 9;
    pub(super) const BOOT: u8 = 10;
    pub(super) const PAUSE: u8 = 11;
    pub(super) const PRESSURE: u8 = 12;
    pub(super) const FONT: u8 = 13;

    pub(super) const CREDIT: u8 = 1;
    pub(super) const LAUNCHED: u8 = 2;
    pub(super) const REFUSED: u8 = 3;
    pub(super) const SCREEN: u8 = 4;
    pub(super) const DAMAGE: u8 = 5;
    pub(super) const RELAY: u8 = 6;
    pub(super) const ENDED: u8 = 7;
    pub(super) const CURSOR: u8 = 8;
    pub(super) const POINTER: u8 = 9;
    pub(super) const POINTER_AT: u8 = 10;
    pub(super) const POINTER_SET: u8 = 11;
    pub(super) const OWN: u8 = 12;
    pub(super) const SHARED: u8 = 13;
    pub(super) const BOOT_FAILED: u8 = 14;
    pub(super) const BOOT_OMITTED: u8 = 15;
    pub(super) const HOURGLASS: u8 = 16;

    pub(super) const KEY: u8 = 1;
    pub(super) const TRANSITION: u8 = 2;
    pub(super) const POINTER_EVENT: u8 = 3;
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
    let held = r.u32()?;
    if held & !buttons::ALL != 0 {
        return Err(WireError::Malformed);
    }
    Ok(held)
}

/// A colour word: `&RRGGBB`.
fn colour(r: &mut Reader<'_>) -> Result<u32, WireError> {
    let colour = r.u32()?;
    if colour > COLOUR_MOST {
        return Err(WireError::Malformed);
    }
    Ok(colour)
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
            w.u8(tag::POINTER_EVENT);
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
        tag::POINTER_EVENT => InputEvent::Pointer {
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

fn put_font<R: Room>(w: &mut Writer<R>, font: DesktopFont) {
    w.raw(&font.family.to_wire());
    w.u16(font.size.unwrap_or(0));
}

/// A desktop font: its family's key, and its size in sixteenths of a point,
/// nought for none of its own.
fn font(r: &mut Reader<'_>) -> Result<DesktopFont, WireError> {
    let mut key = [0; FONT_FAMILY_KEY_LEN];
    key.copy_from_slice(r.take(FONT_FAMILY_KEY_LEN)?);
    let family = FamilyKey::from_wire(key).map_err(|_| WireError::Malformed)?;
    let size = r.u16()?;
    Ok(DesktopFont {
        family,
        size: (size != 0).then_some(size),
    })
}

fn pressure(r: &mut Reader<'_>) -> Result<PressureBand, WireError> {
    PressureBand::from_known_depth(r.u8()?).ok_or(WireError::Malformed)
}

fn put_begin<R: Room>(w: &mut Writer<R>, begin: &Begin<'_>) {
    w.u8(tag::BEGIN);
    for word in [begin.ram_pages, begin.memo_pages, begin.slot_bytes] {
        w.u32(word);
    }
    w.u64(begin.real_time);
    put_flag(w, begin.zone.is_some());
    if let Some(zone) = begin.zone {
        w.bytes(zone.store);
        w.bytes(zone.name.as_bytes());
    }
    w.u32(begin.keyboard);
    put_font(w, begin.font);
    w.u8(begin.pressure.depth());
}

fn begin<'a>(r: &mut Reader<'a>) -> Result<Begin<'a>, WireError> {
    let (ram_pages, memo_pages, slot_bytes) = (r.u32()?, r.u32()?, r.u32()?);
    let real_time = r.u64()?;
    let zone = if flag(r)? {
        let store = r.bytes(ZONE_STORE_MOST)?;
        let name = core::str::from_utf8(r.bytes(ZONE_NAME_MAX)?)
            .ok()
            .filter(|name| is_zone_name(name))
            .ok_or(WireError::Malformed)?;
        Some(Zoned { store, name })
    } else {
        None
    };
    let keyboard = r.u32()?;
    let begin = Begin {
        ram_pages,
        memo_pages,
        slot_bytes,
        real_time,
        zone,
        keyboard,
        font: font(r)?,
        pressure: pressure(r)?,
    };
    let whole_pages = slot_bytes != 0 && slot_bytes % PAGE_SIZE == 0;
    if u64::from(ram_pages) > MOST_PAGES
        || !(1..=MEMO_MOST).contains(&memo_pages)
        || !whole_pages
        || keyboard >= 0x80
    {
        return Err(WireError::Malformed);
    }
    Ok(begin)
}

impl Command<'_> {
    /// The command appended to `w`.
    pub fn encode<R: Room>(&self, w: &mut Writer<R>) {
        match *self {
            Self::Begin(ref begin) => put_begin(w, begin),
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
            Self::Boot { serial } => {
                w.u8(tag::BOOT);
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
            Self::Pause { context, paused } => {
                w.u8(tag::PAUSE);
                w.u32(context);
                put_flag(w, paused);
            }
            Self::Pressure(band) => w.raw(&[tag::PRESSURE, band.depth()]),
            Self::Font(desktop) => {
                w.u8(tag::FONT);
                put_font(w, desktop);
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
                tag::BEGIN => Self::Begin(begin(r)?),
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
                tag::BOOT => Self::Boot { serial: r.u32()? },
                tag::LAUNCH => Self::Launch {
                    serial: r.u32()?,
                    line: line(r)?,
                },
                tag::QUIT => Self::Quit { context: r.u32()? },
                tag::PAUSE => Self::Pause {
                    context: r.u32()?,
                    paused: flag(r)?,
                },
                tag::PRESSURE => Self::Pressure(pressure(r)?),
                tag::FONT => Self::Font(font(r)?),
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
        border: colour(r)?,
        blanked: flag(r)?,
    };
    let sized = |side: u32| (1..=SCREEN_MOST).contains(&side);
    if !sized(screen.width)
        || !sized(screen.height)
        || screen.x_eig > EIG_MOST
        || screen.y_eig > EIG_MOST
    {
        return Err(WireError::Malformed);
    }
    Ok(screen)
}

fn put_cursor<R: Room>(w: &mut Writer<R>, cursor: Option<Cursor>) {
    put_flag(w, cursor.is_some());
    if let Some(c) = cursor {
        for word in [c.x, c.y, c.width, c.top, c.bottom] {
            w.u32(word);
        }
        w.u8(match c.flash {
            Flash::Steady => 0,
            Flash::Fast => 1,
            Flash::Slow => 2,
        });
    }
}

fn flash(r: &mut Reader<'_>) -> Result<Flash, WireError> {
    match r.u8()? {
        0 => Ok(Flash::Steady),
        1 => Ok(Flash::Fast),
        2 => Ok(Flash::Slow),
        _ => Err(WireError::Malformed),
    }
}

fn cursor(r: &mut Reader<'_>) -> Result<Option<Cursor>, WireError> {
    if !flag(r)? {
        return Ok(None);
    }
    let cursor = Cursor {
        x: r.u32()?,
        y: r.u32()?,
        width: r.u32()?,
        top: r.u32()?,
        bottom: r.u32()?,
        flash: flash(r)?,
    };
    let within = |at: u32| at < SCREEN_MOST;
    if !within(cursor.x)
        || !within(cursor.y)
        || !(1..=SCREEN_MOST).contains(&cursor.width)
        || cursor.top > cursor.bottom
        || !within(cursor.bottom)
    {
        return Err(WireError::Malformed);
    }
    Ok(Some(cursor))
}

fn put_shape<R: Room>(w: &mut Writer<R>, shape: Option<Shape<'_>>) {
    put_flag(w, shape.is_some());
    if let Some(s) = shape {
        w.u32(s.width);
        w.u32(s.height);
        w.raw(&[s.active.0, s.active.1]);
        for colour in s.colours {
            w.u32(colour);
        }
        w.bytes(s.pixels);
    }
}

fn shape<'a>(r: &mut Reader<'a>) -> Result<Option<Shape<'a>>, WireError> {
    if !flag(r)? {
        return Ok(None);
    }
    let (width, height) = (r.u32()?, r.u32()?);
    let active = (r.u8()?, r.u8()?);
    let colours = [colour(r)?, colour(r)?, colour(r)?];
    let most = (POINTER_WIDTH_MOST * POINTER_HEIGHT_MOST) as usize;
    let pixels = r.bytes(most)?;
    if !(1..=POINTER_WIDTH_MOST).contains(&width)
        || !(1..=POINTER_HEIGHT_MOST).contains(&height)
        || pixels.len() != (width * height) as usize
        || pixels.iter().any(|&pixel| pixel > POINTER_COLOURS)
    {
        return Err(WireError::Malformed);
    }
    Ok(Some(Shape {
        width,
        height,
        active,
        colours,
        pixels,
    }))
}

fn put_shared<R: Room>(w: &mut Writer<R>, shared: &Shared) {
    for word in [
        shared.system_heap,
        shared.rma,
        shared.sprites,
        shared.font_cache,
        shared.ram_disc,
        shared.made,
        shared.free,
    ] {
        w.u32(word);
    }
}

fn shared(r: &mut Reader<'_>) -> Result<Shared, WireError> {
    Ok(Shared {
        system_heap: r.u32()?,
        rma: r.u32()?,
        sprites: r.u32()?,
        font_cache: r.u32()?,
        ram_disc: r.u32()?,
        made: r.u32()?,
        free: r.u32()?,
    })
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
            Self::Screen { .. }
            | Self::Damage { .. }
            | Self::Cursor { .. }
            | Self::Pointer { .. }
            | Self::PointerAt { .. }
            | Self::PointerSet { .. }
            | Self::Hourglass { .. } => self.encode_shown(w),
            Self::Own {
                context,
                slot,
                screen,
            } => {
                w.u8(tag::OWN);
                for word in [context, slot, screen] {
                    w.u32(word);
                }
            }
            Self::Shared(ref shared) => {
                w.u8(tag::SHARED);
                put_shared(w, shared);
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
            Self::BootFailed {
                context,
                line,
                ref error,
            } => {
                w.u8(tag::BOOT_FAILED);
                w.u32(context);
                w.bytes(line);
                put_error(w, error);
            }
            Self::BootOmitted { context, count } => {
                w.u8(tag::BOOT_OMITTED);
                w.u32(context);
                w.u32(count);
            }
            Self::Ended { context, ending } => {
                w.u8(tag::ENDED);
                w.u32(context);
                put_ending(w, &ending);
            }
        }
    }

    /// A screen's event appended to `w`: its geometry, damage, cursor or
    /// pointer.
    fn encode_shown<R: Room>(&self, w: &mut Writer<R>) {
        match *self {
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
            Self::Cursor { context, cursor } => {
                w.u8(tag::CURSOR);
                w.u32(context);
                put_cursor(w, cursor);
            }
            Self::Pointer { context, shape } => {
                w.u8(tag::POINTER);
                w.u32(context);
                put_shape(w, shape);
            }
            Self::PointerAt { context, x, y } => {
                w.u8(tag::POINTER_AT);
                w.u32(context);
                w.i32(x);
                w.i32(y);
            }
            Self::PointerSet { context } => {
                w.u8(tag::POINTER_SET);
                w.u32(context);
            }
            Self::Hourglass { context, shown } => {
                w.u8(tag::HOURGLASS);
                w.u32(context);
                put_flag(w, shown);
            }
            _ => {}
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
                tag::CURSOR => Self::Cursor {
                    context: r.u32()?,
                    cursor: cursor(r)?,
                },
                tag::POINTER => Self::Pointer {
                    context: r.u32()?,
                    shape: shape(r)?,
                },
                tag::POINTER_AT => Self::PointerAt {
                    context: r.u32()?,
                    x: r.i32()?,
                    y: r.i32()?,
                },
                tag::POINTER_SET => Self::PointerSet { context: r.u32()? },
                tag::HOURGLASS => Self::Hourglass {
                    context: r.u32()?,
                    shown: flag(r)?,
                },
                tag::OWN => Self::Own {
                    context: r.u32()?,
                    slot: r.u32()?,
                    screen: r.u32()?,
                },
                tag::SHARED => Self::Shared(shared(r)?),
                tag::RELAY => Self::Relay {
                    serial: r.u32()?,
                    context: r.u32()?,
                    parked: flag(r)?,
                    side: side(r)?,
                    request: r.bytes(RELAY_MOST)?,
                },
                tag::BOOT_FAILED => Self::BootFailed {
                    context: r.u32()?,
                    line: r.bytes(LINE_MOST)?,
                    error: error(r)?,
                },
                tag::BOOT_OMITTED => Self::BootOmitted {
                    context: r.u32()?,
                    count: r.u32()?,
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
