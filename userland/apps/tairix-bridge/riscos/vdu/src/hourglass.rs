//! Hourglass 2.19 (`plans/RISCOS-EMULATOR.md` §10): the pointer a busy
//! program shows, its SWIs RISC OS 5's as recorded. It appears once its
//! delay passes, selects pointer shapes 3 and 4 in turn every [`FRAME`]
//! centiseconds, and wears its own colours as pointer colours 1 and 3; what a
//! program sets of the pointer meanwhile is what it gives back. Its frames
//! are drawn for TAIRiX, never written into the program's shapes.
//!
//! Each context's hourglass is its own, kept with its screen, so a program
//! that exits or fails with it on takes it with it. A new application lets
//! go of what the one before it left on, and one a killed incarnation of the
//! module left is given back once its context next runs.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_riscos_kernel::module::{Builtin, Services, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::os::{Fail, Os, Regs, Unwind};
use tairix_riscos_kernel::service::NEW_APPLICATION;
use tairix_riscos_kernel::swi::os::BYTE;
use tairix_riscos_kernel::swi::{chunk, X};

use crate::palette::{kind, read_through, set_through, write_through};
use crate::state::{read, with, Vdu};

/// Its SWIs, in their chunk's order.
const NAMES: &[&str] = &[
    "On",
    "Off",
    "Smash",
    "Start",
    "Percentage",
    "LEDs",
    "Colours",
];

/// The centiseconds `Hourglass_On` waits before it appears.
const DELAY: u64 = 33;
/// The centiseconds each frame shows for.
const FRAME: u64 = 15;
/// The pointer shapes its frames select in turn.
const SHAPES: [u8; 2] = [3, 4];
/// `OS_Byte` 106's bit for a pointer left where it was set.
const UNLINKED: u32 = 0x80;
/// The pointer colours it wears.
const WORN: [u8; 2] = [1, 3];
/// Its colours until a program sets them, each word's low three bytes red,
/// green and blue: cyan sand in a blue glass.
const DEFAULT_COLOURS: [u32; 2] = [0x00FF_FF00, 0x00FF_0000];
/// What `Hourglass_Colours` takes to keep a colour as it is.
const UNCHANGED: u32 = u32::MAX;
/// The most `Hourglass_Percentage` shows.
const PERCENT_MOST: u32 = 99;

/// `Hourglass`.
pub(crate) static HOURGLASS: Builtin = Builtin {
    title: "Hourglass",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: chunk::HOURGLASS,
        prefix: "Hourglass",
        names: NAMES,
        handler: perform,
    }),
    services: Some(Services {
        numbers: &[NEW_APPLICATION],
        handler: service,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[],
    vectors: &[],
    resources: &[],
    code: None,
};

/// The module's incarnation, the session's: which is live, so a context's
/// hourglass that a killed one left is known for what it is.
struct Lifecycle {
    generation: u32,
    live: bool,
}

/// One context's hourglass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hourglass {
    /// The incarnation it belongs to; nought before any touched it.
    generation: u32,
    /// `On` and `Start` calls no `Off` has matched yet.
    calls: u32,
    /// When it appears or next turns over; none while nothing will.
    due: Option<u64>,
    shown: Option<Shown>,
    /// The shape its last frame selected, which its next turns from.
    last: u8,
    percentage: Option<u8>,
    /// Bit 0 the upper light, bit 1 the lower; the byte as set.
    leds: u8,
    /// Its colours 1 and 3 as `Hourglass_Colours` keeps them.
    colours: [u32; 2],
}

/// What an hourglass showing took from the pointer, and set on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shown {
    /// Its frames since it appeared.
    frames: u32,
    /// The selection it gives back, as the program last made it.
    selection: u32,
    /// Pointer colours 1 and 3 as it gives them back, and as it last set
    /// them.
    saved: [[u32; 2]; 2],
    set: [Option<[u32; 2]>; 2],
}

/// A frame fallen due: the shape it selects, and whether the hourglass
/// appears with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Turn {
    shape: u8,
    appearing: bool,
}

/// What a frame of a showing hourglass draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// How far its sand has run, nought to [`PHASES`] less one.
    phase: u32,
    percentage: Option<u8>,
    leds: u8,
}

const fn other(shape: u8) -> u8 {
    if shape == SHAPES[0] {
        SHAPES[1]
    } else {
        SHAPES[0]
    }
}

impl Hourglass {
    pub(crate) const fn new() -> Self {
        Self {
            generation: 0,
            calls: 0,
            due: None,
            shown: None,
            last: SHAPES[0],
            percentage: None,
            leds: 0,
            colours: DEFAULT_COLOURS,
        }
    }

    /// Brought to incarnation `live`: one of an earlier left as a fresh
    /// one, what it showed with handed back to give back.
    fn adopt(&mut self, live: u32) -> Option<Shown> {
        if self.generation == live {
            return None;
        }
        let shown = self.shown.take();
        *self = Self::new();
        self.generation = live;
        shown
    }

    /// `Hourglass_On` or `Hourglass_Start`: a call counted, and the first
    /// appearing `delay` centiseconds from `now`. A start of no delay holds
    /// it back, and stops one showing turning over.
    fn on(&mut self, now: u64, delay: u64) {
        if delay == 0 {
            self.due = None;
        } else if self.calls == 0 {
            self.due = Some(now.saturating_add(delay));
        }
        self.calls = self.calls.saturating_add(1);
    }

    /// `Hourglass_Off`: a call matched; what it gives back where the last
    /// was and it showed.
    fn off(&mut self) -> Option<Shown> {
        let left = self.calls.checked_sub(1)?;
        self.calls = left;
        if left == 0 {
            self.smash()
        } else {
            None
        }
    }

    /// `Hourglass_Smash`: every call matched at once.
    fn smash(&mut self) -> Option<Shown> {
        self.calls = 0;
        self.due = None;
        self.shown.take()
    }

    /// The frames fallen due by `now`, counted from when each was due: a
    /// late look turns over as many times as went by.
    fn turn(&mut self, now: u64) -> Option<Turn> {
        let due = self.due.filter(|&due| due <= now)?;
        let frames = (now - due) / FRAME + 1;
        if frames % 2 == 1 {
            self.last = other(self.last);
        }
        self.due = Some(due.saturating_add(frames.saturating_mul(FRAME)));
        let frames = u32::try_from(frames).unwrap_or(u32::MAX);
        let appearing = self.shown.is_none();
        let shown = self.shown.get_or_insert(Shown {
            frames: 0,
            selection: 0,
            saved: [[0; 2]; 2],
            set: [None; 2],
        });
        shown.frames = if appearing {
            frames - 1
        } else {
            shown.frames.saturating_add(frames)
        };
        Some(Turn {
            shape: self.last,
            appearing,
        })
    }

    /// The selection a frame's own replaced, `old`: kept to give back where
    /// the hourglass appeared with it or the program made it since.
    fn selected_over(&mut self, appearing: bool, old: u32) {
        if let Some(shown) = &mut self.shown {
            let program = !SHAPES
                .iter()
                .any(|&shape| u32::from(shape) == old & !UNLINKED);
            if appearing || program {
                shown.selection = old;
            }
        }
    }

    /// Pointer colour `index` as it stands, `current`: kept to give back
    /// where the hourglass has not set it or the program has since.
    fn colour_over(&mut self, index: usize, current: [u32; 2]) {
        if let Some(shown) = &mut self.shown {
            if shown.set[index] != Some(current) {
                shown.saved[index] = current;
            }
        }
    }

    fn colour_set(&mut self, index: usize, set: [u32; 2]) {
        if let Some(shown) = &mut self.shown {
            shown.set[index] = Some(set);
        }
    }

    /// `Hourglass_LEDs`: the lights masked with `and` then flipped by `eor`,
    /// kept as a byte; the lights as they were.
    fn leds(&mut self, eor: u32, and: u32) -> u32 {
        let old = u32::from(self.leds);
        self.leds = ((old & and) ^ eor).to_le_bytes()[0];
        old
    }

    /// What it draws, while it shows.
    fn frame(&self) -> Option<Frame> {
        self.shown.map(|shown| Frame {
            phase: shown.frames % PHASES,
            percentage: self.percentage,
            leds: self.leds,
        })
    }
}

/// The live incarnation's generation, or nought while none is.
fn live(os: &mut Os<'_>) -> u32 {
    os.extension::<Lifecycle>()
        .filter(|lifecycle| lifecycle.live)
        .map_or(0, |lifecycle| lifecycle.generation)
}

fn init(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let generation = os
        .extension::<Lifecycle>()
        .map_or(1, |lifecycle| lifecycle.generation.wrapping_add(1).max(1));
    os.hold_extension(Lifecycle {
        generation,
        live: true,
    })
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let shown = read(os, |vdu, _| vdu.hourglass.smash()).flatten();
    if let Some(lifecycle) = os.extension::<Lifecycle>() {
        lifecycle.live = false;
    }
    give_back(os, shown)
}

/// The current context's hourglass brought to the live incarnation, what
/// one killed since left showing given back.
fn adopted(os: &mut Os<'_>) -> Result<(), Fail> {
    let live = live(os);
    let stale = with(os, |vdu, _| Ok(vdu.hourglass.adopt(live)))?;
    give_back(os, stale)
}

fn perform(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    adopted(os)?;
    let now = os.now();
    match offset {
        0 => with(os, |vdu, _| {
            vdu.hourglass.on(now, DELAY);
            Ok(())
        }),
        1 => {
            let shown = with(os, |vdu, _| Ok(vdu.hourglass.off()))?;
            give_back(os, shown)
        }
        2 => smash(os),
        3 => with(os, |vdu, _| {
            vdu.hourglass.on(now, u64::from(regs.r[0]));
            Ok(())
        }),
        4 => with(os, |vdu, _| {
            vdu.hourglass.percentage = u8::try_from(regs.r[0])
                .ok()
                .filter(|&percent| u32::from(percent) <= PERCENT_MOST);
            Ok(())
        }),
        5 => {
            let (eor, and) = (regs.r[0], regs.r[1]);
            regs.r[0] = with(os, |vdu, _| Ok(vdu.hourglass.leds(eor, and)))?;
            Ok(())
        }
        6 => colours(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[HOURGLASS.title.as_bytes()])),
    }
}

fn smash(os: &mut Os<'_>) -> Result<(), Fail> {
    let shown = with(os, |vdu, _| Ok(vdu.hourglass.smash()))?;
    give_back(os, shown)
}

/// `Hourglass_Colours`: R0 and R1 its colours 1 and 3, each kept as it is
/// for [`UNCHANGED`], worn from its next frame; the colours as they were.
fn colours(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let given = [regs.r[0], regs.r[1]];
    let old = with(os, |vdu, _| {
        let old = vdu.hourglass.colours;
        for (kept, new) in vdu.hourglass.colours.iter_mut().zip(given) {
            if new != UNCHANGED {
                *kept = new;
            }
        }
        Ok(old)
    })?;
    [regs.r[0], regs.r[1]] = old;
    Ok(())
}

/// `Service_NewApplication`: the program the hourglass was shown for is
/// being replaced, so it goes, never claiming the service.
fn service(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Unwind> {
    let live = live(os);
    let shown = read(os, |vdu, _| {
        let stale = vdu.hourglass.adopt(live);
        stale.or(vdu.hourglass.smash())
    })
    .flatten();
    unwound(give_back(os, shown))
}

/// An error at a frame or a service lost, as a timed call's would be: only
/// the unwind leaves.
fn unwound(done: Result<(), Fail>) -> Result<(), Unwind> {
    match done {
        Err(Fail::Unwind(unwind)) => Err(unwind),
        Ok(()) | Err(Fail::Error(_)) => Ok(()),
    }
}

/// `OS_Byte` 106 selecting `value`: the selection as it was.
fn select(os: &mut Os<'_>, value: u32) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[106, value]);
    os.perform(X | BYTE, &mut regs)?;
    Ok(regs.r[1])
}

/// The hourglass's colours worn as pointer colours 1 and 3, what each held
/// before kept to give back where the program set it.
fn wear(os: &mut Os<'_>) -> Result<(), Fail> {
    for (index, colour) in WORN.into_iter().enumerate() {
        let current = read_through(os, u32::from(colour), kind::POINTER)?;
        let own = with(os, |vdu, _| {
            vdu.hourglass.colour_over(index, current);
            Ok(vdu.hourglass.colours[index])
        })?;
        let [red, green, blue, _] = own.to_le_bytes();
        write_through(
            os,
            colour,
            kind::POINTER.to_le_bytes()[0],
            [red, green, blue],
        )?;
        let set = read_through(os, u32::from(colour), kind::POINTER)?;
        with(os, |vdu, _| {
            vdu.hourglass.colour_set(index, set);
            Ok(())
        })?;
    }
    Ok(())
}

/// What an hourglass took from the pointer given back: the selection where
/// the pointer still shows one of its shapes, linked, and each colour it
/// still wears.
fn give_back(os: &mut Os<'_>, shown: Option<Shown>) -> Result<(), Fail> {
    let Some(shown) = shown else {
        return Ok(());
    };
    let selected = read(os, |vdu, _| vdu.pointer.selected).unwrap_or(0);
    if SHAPES.contains(&selected) {
        select(os, shown.selection & !UNLINKED)?;
    }
    for (index, colour) in WORN.into_iter().enumerate() {
        let current = read_through(os, u32::from(colour), kind::POINTER)?;
        if shown.set[index] == Some(current) {
            set_through(os, u32::from(colour), kind::POINTER, shown.saved[index])?;
        }
    }
    Ok(())
}

/// The frames fallen due by `now` shown, from the clock's interrupt: each
/// selects its shape and wears the hourglass's colours again, what the
/// program set meanwhile kept to give back.
pub(crate) fn frames(os: &mut Os<'_>, now: u64) -> Result<(), Unwind> {
    unwound(turn(os, now))
}

fn turn(os: &mut Os<'_>, now: u64) -> Result<(), Fail> {
    let Some((stale, turn)) = read(os, |vdu, os| {
        let stale = vdu.hourglass.adopt(live(os));
        (stale, vdu.hourglass.turn(now))
    }) else {
        return Ok(());
    };
    give_back(os, stale)?;
    let Some(turn) = turn else {
        return Ok(());
    };
    let old = select(os, u32::from(turn.shape))?;
    with(os, |vdu, _| {
        vdu.hourglass.selected_over(turn.appearing, old);
        Ok(())
    })?;
    wear(os)
}

/// When the current context's hourglass next appears or turns over.
pub(crate) fn due(vdu: &Vdu, os: &mut Os<'_>) -> Option<u64> {
    (vdu.hourglass.generation == live(os))
        .then_some(vdu.hourglass.due)
        .flatten()
}

/// What the current context's hourglass draws, where it shows and the
/// pointer shows one of its shapes.
pub(crate) fn showing(vdu: &Vdu, os: &mut Os<'_>) -> Option<Frame> {
    let glass = &vdu.hourglass;
    if glass.generation != live(os) || !SHAPES.contains(&vdu.pointer.selected) {
        return None;
    }
    glass.frame()
}

/// Whether the current context's hourglass shows.
pub(crate) fn shows(vdu: &Vdu, os: &mut Os<'_>) -> bool {
    vdu.hourglass.generation == live(os) && vdu.hourglass.shown.is_some()
}

/// A frame's pixels across and lines down.
pub(crate) const WIDTH: u32 = 16;
pub(crate) const HEIGHT: u32 = 32;
/// The pixel the pointer's place names: the glass's waist.
pub(crate) const ACTIVE: (u32, u32) = (8, 11);
/// The frames the sand takes to run through.
const PHASES: u32 = 8;
/// The pointer colours the glass and the sand are drawn in.
const GLASS: u8 = WORN[1];
const SAND: u8 = WORN[0];
/// The lines of each part, from the top.
const TOP_BAR: Range<u32> = 0..2;
const TOP_BULB: Range<u32> = 2..12;
const BOTTOM_BULB: Range<u32> = 12..22;
const BOTTOM_BAR: Range<u32> = 22..24;
const FIGURES: Range<u32> = 24..32;
/// Each line of the upper bulb's half width from the middle, narrowing to
/// the waist; the lower bulb's mirror it.
const BULB: [u32; 10] = [6, 6, 5, 5, 4, 3, 3, 2, 1, 1];
/// The columns the bars take, and each light within its bar.
const BAR: Range<u32> = 1..15;
const LIGHT: Range<u32> = 2..4;
/// The column left of the glass's middle.
const MIDDLE: u32 = 7;

/// `frame`'s pixels into `pixels`, a line at a time from the top, each a
/// colour nought to three, nought clear.
///
/// # Errors
///
/// "Not enough memory" where `pixels` cannot hold them.
pub(crate) fn paint(os: &mut Os<'_>, frame: Frame, pixels: &mut Vec<u8>) -> Result<(), Fail> {
    pixels.clear();
    if pixels.try_reserve_exact((WIDTH * HEIGHT) as usize).is_err() {
        return Err(os.no_memory());
    }
    draw(frame, pixels);
    Ok(())
}

/// `frame`'s pixels added to `pixels`, as [`paint`] gives them.
fn draw(frame: Frame, pixels: &mut Vec<u8>) {
    let figures = figures(frame.percentage);
    for y in 0..HEIGHT {
        pixels.extend((0..WIDTH).map(|x| pixel(frame, &figures, x, y)));
    }
}

/// The glyphs of `percentage` across the figures' lines, a column each.
fn figures(percentage: Option<u8>) -> [u8; WIDTH as usize * 8] {
    let mut columns = [0; WIDTH as usize * 8];
    let Some(percent) = percentage else {
        return columns;
    };
    let digits: &[u8] = &[percent / 10, percent % 10];
    let digits = if percent < 10 { &digits[1..] } else { digits };
    let left = (WIDTH as usize - 8 * digits.len()) / 2;
    for (n, &digit) in digits.iter().enumerate() {
        let Some(glyph) = tairix_vdu::font::glyph_of(char::from(b'0' + digit)) else {
            continue;
        };
        for (row, bits) in glyph.into_iter().enumerate() {
            for column in 0..8 {
                if bits & (0x80 >> column) != 0 {
                    columns[row * WIDTH as usize + left + 8 * n + column] = GLASS;
                }
            }
        }
    }
    columns
}

/// Pixel `x`, `y` of `frame`.
fn pixel(frame: Frame, figures: &[u8], x: u32, y: u32) -> u8 {
    let lit = |bit: u8| frame.leds & bit != 0 && LIGHT.contains(&x);
    if TOP_BAR.contains(&y) || BOTTOM_BAR.contains(&y) {
        let bit = if TOP_BAR.contains(&y) { 1 } else { 2 };
        return if lit(bit) {
            SAND
        } else if BAR.contains(&x) {
            GLASS
        } else {
            0
        };
    }
    if FIGURES.contains(&y) {
        return figures[((y - FIGURES.start) * WIDTH + x) as usize];
    }
    let (half, sand) = if TOP_BULB.contains(&y) {
        let line = y - TOP_BULB.start;
        (BULB[line as usize], line > frame.phase)
    } else {
        let line = BOTTOM_BULB.end - 1 - y;
        let falling = (MIDDLE..=MIDDLE + 1).contains(&x);
        (BULB[line as usize], line <= frame.phase || falling)
    };
    let (left, right) = (MIDDLE - half, MIDDLE + 1 + half);
    if x == left || x == right {
        GLASS
    } else if (left..right).contains(&x) && sand {
        SAND
    } else {
        0
    }
}

#[cfg(test)]
mod tests;
