//! Selecting a mode (`plans/RISCOS-EMULATOR.md` §8): `VDU 22`,
//! `OS_ScreenMode` and `OS_CheckModeValid`; the monitor's modes, a mode it
//! lacks traded for the one RISC OS 5 trades it for, as recorded; the
//! services around a change; and the drivers' start at reset.
//!
//! The current monitor type starts as the configured one. On a VGA monitor,
//! type 3, a numbered mode it lacks becomes the 640 by 480 mode of the same
//! depth where that keeps the mode's eigen factors and flags, and otherwise a
//! 640 by 480 selector keeping its depth, colours, flags and eigen factors.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::mouse::Bounds;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::{CHANGE_DYNAMIC_AREA, READ_SYS_INFO};
use tairix_riscos_kernel::swi::X;
use tairix_vdu::mode::{Flags, DEFAULT_MODE};

use crate::colour::Colours;
use crate::frames::Frames;
use crate::mode::{var, Format};
use crate::palette::Palette;
use crate::plot::Graphics;
use crate::pointer::Pointer;
use crate::screen::{self, Named, Screen};
use crate::state::{with, Vdu};
use crate::stream::Stream;
use crate::teletext::Teletext;
use crate::text::Text;

/// "Bad mode": a mode the monitor cannot show and nothing provides.
const BAD_MODE: Error = Error {
    number: 0x1ED,
    token: "BadMode",
};
/// "Bad mode string".
const BAD_MODE_STRING: Error = Error {
    number: 0x1EA,
    token: "BadModeString",
};
/// The error for a reason `OS_ScreenMode` has none of.
const BAD_REASON: Error = Error {
    number: 0x1F2,
    token: "BadScreenModeReason",
};

/// The most pairs a selector the drivers copy may carry.
pub(crate) const MOST_PAIRS: usize = 32;
/// The bytes a selector copy takes: its five words, its pairs, its end.
pub(crate) const SELECTOR_BYTES: u32 = 4 * (5 + 2 * len32(MOST_PAIRS) + 1);
/// The frame rate a monitor runs every mode at that names none.
const VGA_RATE: u32 = 60;
const TV_RATE: u32 = 50;

/// A mode named by a program: a number or a selector's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a selector's pairs are held in place, so naming a mode allocates nothing"
)]
pub(crate) enum Spec {
    Number(u32),
    Selector(Selector),
}

/// A selector's words, as read, its pairs held in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selector {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) log2_bpp: u32,
    pub(crate) frame_rate: u32,
    pairs: [(u32, u32); MOST_PAIRS],
    count: usize,
}

impl Selector {
    /// `width` by `height` at `log2_bpp` and `frame_rate`, no variable set.
    pub(crate) const fn new(width: u32, height: u32, log2_bpp: u32, frame_rate: u32) -> Self {
        Self {
            width,
            height,
            log2_bpp,
            frame_rate,
            pairs: [(0, 0); MOST_PAIRS],
            count: 0,
        }
    }

    /// `pairs` set in turn; `None` where there are more than a selector holds.
    pub(crate) fn with(mut self, pairs: impl IntoIterator<Item = (u32, u32)>) -> Option<Self> {
        for pair in pairs {
            *self.pairs.get_mut(self.count)? = pair;
            self.count += 1;
        }
        Some(self)
    }

    /// The variables it sets, in order.
    pub(crate) fn pairs(&self) -> &[(u32, u32)] {
        &self.pairs[..self.count]
    }

    /// Its words as a copy holds them, written into `out`: the five, the
    /// pairs, the end.
    fn bytes<'b>(&self, out: &'b mut [u8; SELECTOR_BYTES as usize]) -> &'b [u8] {
        let words = [1, self.width, self.height, self.log2_bpp, self.frame_rate]
            .into_iter()
            .chain(
                self.pairs()
                    .iter()
                    .flat_map(|&(index, value)| [index, value]),
            )
            .chain([u32::MAX]);
        let mut written = 0;
        for (slot, byte) in out.iter_mut().zip(words.flat_map(u32::to_le_bytes)) {
            *slot = byte;
            written += 1;
        }
        &out[..written]
    }

    /// The mode it describes, if it describes one.
    fn format(&self) -> Option<Format> {
        Format::selector(
            self.width,
            self.height,
            self.log2_bpp,
            self.pairs().iter().copied(),
        )
    }
}

/// A mode resolved: what it is, how it was named, and its frame rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) format: Format,
    pub(crate) spec: Spec,
    pub(crate) frame_rate: u32,
    /// The second bank was asked for.
    pub(crate) shadow: bool,
}

/// What a mode named comes to on the current monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// It can be shown as named.
    Valid(Resolved),
    /// The monitor lacks it: this is what is shown instead.
    Substitute(Spec),
    /// Nothing can show it.
    Unknown,
}

/// The selector at `address`.
fn read_selector(os: &mut Os<'_>, address: u32) -> Result<Option<Selector>, Fail> {
    let flags = os.read_word(address)?;
    if flags & 0xFF != 1 {
        return Ok(None);
    }
    let word = |os: &mut Os<'_>, offset: u32| os.read_word(address.wrapping_add(offset));
    let (width, height) = (word(os, 4)?, word(os, 8)?);
    let (log2_bpp, frame_rate) = (word(os, 12)?, word(os, 16)?);
    let mut selector = Selector::new(width, height, log2_bpp, frame_rate);
    let mut at = 20u32;
    loop {
        let index = word(os, at)?;
        if index == u32::MAX {
            return Ok(Some(selector));
        }
        let Some(more) = selector.with([(index, word(os, at.wrapping_add(4))?)]) else {
            return Ok(None);
        };
        selector = more;
        at = at.wrapping_add(8);
    }
}

/// The mode a specifier names: a number below 256, else a selector's
/// address.
pub(crate) fn read_spec(os: &mut Os<'_>, specifier: u32) -> Result<Option<Spec>, Fail> {
    if specifier < 0x100 {
        return Ok(Some(Spec::Number(specifier)));
    }
    Ok(read_selector(os, specifier)?.map(Spec::Selector))
}

/// The monitor types each numbered mode is shown on, as RISC OS 5 has them.
fn shown_on(mode: u32, monitor: u32) -> bool {
    let types: &[u32] = match mode {
        0..=15 => &[0, 1, 3, 4],
        16 | 17 | 22 | 24 | 33..=36 => &[0, 1],
        18..=21 | 37..=40 => &[1],
        23 => &[2],
        25..=28 => &[1, 3, 4, 5],
        29..=32 => &[1, 4],
        41..=43 | 47..=49 => &[1, 3, 4],
        44..=46 => &[1, 3, 4, 8],
        50..=53 => &[0, 1, 3, 4, 5],
        _ => &[],
    };
    types.contains(&monitor)
}

/// The pixels a monitor shows a mode it lacks in.
const fn standard_size(monitor: u32) -> (u32, u32) {
    match monitor {
        0 => (640, 256),
        2 => (1152, 896),
        _ => (640, 480),
    }
}

/// What numbered mode `mode`, which `monitor` lacks, is shown as there.
fn substitute(mode: u32, monitor: u32) -> Option<Spec> {
    let m = tairix_vdu::mode::numbered(mode)?;
    let format = Format::numbered(m);
    let (width, height) = standard_size(monitor);
    let flags = format.flags() & !Flags::HI_RES_MONO.bits();
    let same = tairix_vdu::mode::MODES
        .iter()
        .enumerate()
        .find(|(n, other)| {
            let other = Format::numbered(other);
            other.width() == width
                && other.height() == height
                && other.log2_bpp() == format.log2_bpp()
                && other.log2_bpc() == format.log2_bpc()
                && other.x_eig() == format.x_eig()
                && other.y_eig() == format.y_eig()
                && other.flags() == flags
                && shown_on(u32::try_from(*n).unwrap_or(u32::MAX), monitor)
        });
    if let Some((n, _)) = same {
        return Some(Spec::Number(u32::try_from(n).ok()?));
    }
    let multisync_only = !shown_on(mode, 0) && !shown_on(mode, 2);
    let frame_rate = if multisync_only && monitor == 3 {
        VGA_RATE
    } else {
        u32::MAX
    };
    Selector::new(width, height, format.log2_bpp(), frame_rate)
        .with([
            (var::MODE_FLAGS, flags),
            (var::N_COLOUR, format.ncolour()),
            (var::X_EIG, format.x_eig()),
            (var::Y_EIG, format.y_eig()),
        ])
        .map(Spec::Selector)
}

/// Whether `monitor` has timings for a selector of this size and format
/// without a module's help: the sizes of the numbered modes it shows, at its
/// own rate, in a format the hardware has.
fn timed(selector: &Selector, format: &Format, monitor: u32) -> bool {
    let rate_ok = selector.frame_rate == u32::MAX || selector.frame_rate == frame_rate(monitor);
    let size_ok = tairix_vdu::mode::MODES.iter().enumerate().any(|(n, m)| {
        shown_on(u32::try_from(n).unwrap_or(u32::MAX), monitor)
            && u32::from(m.width) << (m.log2_bpc - m.log2_bpp) == selector.width
            && u32::from(m.height) == selector.height
    });
    let unsupported = (format.log2_bpp() == 5 && format.has(Flags::RGB))
        || (format.log2_bpp() == 3 && format.has(Flags::GREYSCALE));
    rate_ok && size_ok && !unsupported
}

/// A monitor's frame rate.
const fn frame_rate(monitor: u32) -> u32 {
    if monitor == 0 {
        TV_RATE
    } else {
        VGA_RATE
    }
}

/// What `spec` comes to on `monitor`, asking the modules where the kernel
/// knows no answer.
pub(crate) fn resolve(os: &mut Os<'_>, spec: &Spec, monitor: u32) -> Result<Outcome, Fail> {
    match spec {
        Spec::Number(n) => {
            let shadow = n & 0x80 != 0;
            let mode = n & 0x7F;
            if let Some(m) = tairix_vdu::mode::numbered(mode) {
                if shown_on(mode, monitor) || monitor == 7 {
                    return Ok(Outcome::Valid(Resolved {
                        format: Format::numbered(m),
                        spec: Spec::Number(mode),
                        frame_rate: frame_rate(monitor),
                        shadow,
                    }));
                }
                if let Some(translated) = translate(os, mode, monitor)? {
                    return Ok(Outcome::Substitute(translated));
                }
                return Ok(substitute(mode, monitor).map_or(Outcome::Unknown, Outcome::Substitute));
            }
            extension(os, *n, monitor)
        }
        Spec::Selector(selector) => {
            let Some(format) = selector.format() else {
                return Ok(Outcome::Unknown);
            };
            let valid = Outcome::Valid(Resolved {
                format,
                spec: *spec,
                frame_rate: if selector.frame_rate == u32::MAX {
                    frame_rate(monitor)
                } else {
                    selector.frame_rate
                },
                shadow: false,
            });
            if timed(selector, &format, monitor) {
                return Ok(valid);
            }
            let address = os.scratch_text(selector.bytes(&mut [0; SELECTOR_BYTES as usize]))?;
            let mut regs = Regs::new([
                0,
                service::MODE_EXTENSION,
                address,
                monitor,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            os.service(&mut regs)?;
            Ok(if regs.r[1] == 0 {
                valid
            } else {
                Outcome::Unknown
            })
        }
    }
}

/// `Service_ModeTranslation`, for a monitor type the kernel does not know:
/// the mode a module offers instead.
fn translate(os: &mut Os<'_>, mode: u32, monitor: u32) -> Result<Option<Spec>, Fail> {
    if matches!(monitor, 0..=5 | 8) {
        return Ok(None);
    }
    let mut regs = Regs::new([
        0,
        service::MODE_TRANSLATION,
        mode,
        monitor,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.service(&mut regs)?;
    if regs.r[1] != 0 {
        return Ok(None);
    }
    read_spec(os, regs.r[2])
}

/// `Service_ModeExtension` for a numbered mode RISC OS does not define: the
/// mode a module's workspace list describes from its base mode.
fn extension(os: &mut Os<'_>, number: u32, monitor: u32) -> Result<Outcome, Fail> {
    let mode = number & 0x7F;
    let mut regs = Regs::new([
        0,
        service::MODE_EXTENSION,
        mode,
        monitor,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.service(&mut regs)?;
    if regs.r[1] != 0 || regs.r[4] == 0 {
        return Ok(Outcome::Unknown);
    }
    let list = regs.r[4];
    let base = os.read_word(list.wrapping_add(4))?;
    let Some(base) = tairix_vdu::mode::numbered(base) else {
        return Ok(Outcome::Unknown);
    };
    let base = Format::numbered(base);
    let mut pairs = [(0, 0); MOST_PAIRS];
    let mut count = 0;
    let mut at = list.wrapping_add(8);
    while count < MOST_PAIRS {
        let index = os.read_word(at)?;
        if index == u32::MAX {
            break;
        }
        pairs[count] = (index, os.read_word(at.wrapping_add(4))?);
        count += 1;
        at = at.wrapping_add(8);
    }
    let pairs = &pairs[..count];
    let seeded = (0..crate::mode::VARIABLES - 1)
        .filter_map(|n| base.var(n).map(|v| (n, v)))
        .chain(pairs.iter().copied())
        .filter(|&(n, _)| n != var::LOG2_BPP);
    let log2_bpp = pairs
        .iter()
        .rev()
        .find(|(n, _)| *n == var::LOG2_BPP)
        .map_or(base.log2_bpp(), |&(_, v)| v);
    let pixels = |limit: u32| {
        let set = seeded.clone().rev().find(|&(n, _)| n == limit);
        set.map_or(0, |(_, v)| v.saturating_add(1))
    };
    let (width, height) = (pixels(var::X_WIND_LIMIT), pixels(var::Y_WIND_LIMIT));
    Ok(
        Format::selector(width, height, log2_bpp, seeded).map_or(Outcome::Unknown, |format| {
            Outcome::Valid(Resolved {
                format,
                spec: Spec::Number(mode),
                frame_rate: frame_rate(monitor),
                shadow: number & 0x80 != 0,
            })
        }),
    )
}

/// VDU 22: the mode `mode`, its top bit asking for the second bank.
pub(crate) fn vdu_22(os: &mut Os<'_>, mode: u8) -> Result<(), Fail> {
    select(os, &Spec::Number(u32::from(mode)))
}

/// Selects the mode `spec` names, through the services: one the monitor
/// lacks becomes its substitute; one nothing can show refuses.
pub(crate) fn select(os: &mut Os<'_>, spec: &Spec) -> Result<(), Fail> {
    let mut pre = Regs::new([
        0,
        service::PRE_MODE_CHANGE,
        spec_word(os, spec)?,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    let given = pre.r[2];
    os.service(&mut pre)?;
    let spec = if pre.r[1] == 0 {
        if pre.r[0] == 0 {
            return Ok(());
        }
        return Err(Fail::Error(pre.r[0]));
    } else if pre.r[2] == given {
        *spec
    } else {
        read_spec(os, pre.r[2])?.ok_or_else(|| os.error(BAD_MODE))?
    };
    let monitor = with(os, |vdu, _| Ok(vdu.monitor))?;
    let resolved = match resolve(os, &spec, monitor)? {
        Outcome::Valid(resolved) => resolved,
        Outcome::Substitute(substitute) => match resolve(os, &substitute, monitor)? {
            Outcome::Valid(resolved) => resolved,
            _ => return Err(os.error(BAD_MODE)),
        },
        Outcome::Unknown => return Err(os.error(BAD_MODE)),
    };
    let shadow = resolved.shadow || with(os, |vdu, _| Ok(vdu.shadow))?;
    let banks = if shadow { 2 } else { 1 };
    let needed = (resolved.format.screen_bytes() * banks)
        .next_multiple_of(tairix_riscos_arm::mmu::PAGE_SIZE);
    let size = with(os, |vdu, _| Ok(vdu.screen.size))?;
    if needed > size {
        let mut grow = Regs::new([screen::AREA, needed - size, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        os.perform(X | CHANGE_DYNAMIC_AREA, &mut grow)?;
    }
    let word = spec_word(os, &resolved.spec)?;
    let mut changing = Regs::new([
        0,
        service::MODE_CHANGING,
        word,
        monitor,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.service(&mut changing)?;
    let named = name(os, &resolved.spec)?;
    with(os, |vdu, os| apply(vdu, os, &resolved, named, shadow))?;
    bound_mouse(os, &resolved.format);
    let mut changed = Regs::new([
        0,
        service::MODE_CHANGE,
        word,
        monitor,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    os.service(&mut changed)?;
    Ok(())
}

/// The word a service is given for `spec`: the number, or the address of
/// a copy of the selector.
fn spec_word(os: &mut Os<'_>, spec: &Spec) -> Result<u32, Fail> {
    match spec {
        Spec::Number(n) => Ok(*n),
        Spec::Selector(selector) => {
            os.scratch_text(selector.bytes(&mut [0; SELECTOR_BYTES as usize]))
        }
    }
}

/// How the screen keeps `spec`: a number, or the selector copied where
/// `OS_ScreenMode` 1 hands it out.
fn name(os: &mut Os<'_>, spec: &Spec) -> Result<Named, Fail> {
    match spec {
        Spec::Number(n) => Ok(Named::Number(*n)),
        Spec::Selector(selector) => {
            let copy = with(os, |vdu, _| Ok(vdu.screen.selector_copy))?;
            os.write_bytes(copy, selector.bytes(&mut [0; SELECTOR_BYTES as usize]))?;
            Ok(Named::Selector(copy))
        }
    }
}

/// The mouse bounded by the new screen.
fn bound_mouse(os: &mut Os<'_>, format: &Format) {
    os.set_mouse_bounds(Bounds {
        left: 0,
        bottom: 0,
        right: ((format.width() << format.x_eig()) - 1).cast_signed(),
        top: ((format.height() << format.y_eig()) - 1).cast_signed(),
    });
}

/// The drivers' state for `resolved`: every window, cursor, colour and
/// pattern as a mode change leaves it, the screen cleared.
fn apply(
    vdu: &mut Vdu,
    os: &mut Os<'_>,
    resolved: &Resolved,
    named: Named,
    shadow: bool,
) -> Result<(), Fail> {
    let format = resolved.format;
    let bank: u32 = if shadow { 2 } else { 1 };
    vdu.screen.format = format;
    vdu.screen.named = named;
    vdu.screen.frame_rate = resolved.frame_rate;
    vdu.screen.own_bank = bank;
    vdu.screen.set_bank(true, bank);
    vdu.screen.set_bank(false, bank);
    os.set_byte_var(tairix_riscos_kernel::byte::var::VDU_BANK, 0);
    os.set_byte_var(tairix_riscos_kernel::byte::var::DISPLAY_BANK, 0);
    (vdu.text, vdu.graphics, vdu.colours, vdu.palette) = fresh(&format, vdu.text.flags);
    vdu.teletext = Teletext::new();
    vdu.cursors_removed = 0;
    os.set_byte_var(tairix_riscos_kernel::byte::var::PAGE_LINES, 0);
    crate::changed::reset(os)?;
    vdu.colours.publish(os, &format)?;
    let w = vdu.text.window;
    vdu.clear_cells(os, w.left, w.top, w.right, w.bottom)?;
    crate::changed::reset(os)
}

/// What a mode change starts afresh for `format`: the text, its `flags`
/// kept, the graphics, the colours and the palette.
fn fresh(format: &Format, flags: u8) -> (Text, Graphics, Colours, Palette) {
    (
        Text::for_screen(
            format.columns(),
            format.rows(),
            format.cell().space_y,
            flags,
        ),
        Graphics::for_screen(format.width(), format.height()),
        Colours::for_mode(format),
        Palette::for_mode(format),
    )
}

/// `OS_CheckModeValid`: R0 the mode, kept with C clear and Z set where it
/// can be shown; else C set, R0 -1 and R1 what is shown instead, or -2 where
/// nothing is; or R0 -2 where the screen's memory as it is cannot hold it.
pub(crate) fn check_mode_valid(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Some(spec) = read_spec(os, regs.r[0])? else {
        regs.r[0] = u32::MAX;
        regs.r[1] = 0xFFFF_FFFE;
        regs.set_flag(psr::C, true);
        return Ok(());
    };
    let monitor = with(os, |vdu, _| Ok(vdu.monitor))?;
    match resolve(os, &spec, monitor)? {
        Outcome::Valid(resolved) => {
            let size = with(os, |vdu, _| Ok(vdu.screen.size))?;
            let banks = if resolved.shadow { 2 } else { 1 };
            if resolved.format.screen_bytes() * banks > size {
                regs.r[0] = 0xFFFF_FFFE;
                regs.set_flag(psr::C, true);
                return Ok(());
            }
            regs.set_flag(psr::C, false);
            regs.set_flag(psr::Z, true);
        }
        Outcome::Substitute(substitute) => {
            regs.r[0] = u32::MAX;
            regs.r[1] = match substitute {
                Spec::Number(n) => n,
                Spec::Selector(selector) => {
                    let scratch = with(os, |vdu, _| Ok(vdu.screen.check_copy))?;
                    os.write_bytes(scratch, selector.bytes(&mut [0; SELECTOR_BYTES as usize]))?;
                    scratch
                }
            };
            regs.set_flag(psr::C, true);
        }
        Outcome::Unknown => {
            regs.r[0] = u32::MAX;
            regs.r[1] = 0xFFFF_FFFE;
            regs.set_flag(psr::C, true);
        }
    }
    Ok(())
}

/// `OS_ScreenMode`: by R0, 0 select R1's mode, 1 read the current, 2
/// enumerate the monitor's modes, 3 set the monitor type, 4 the screen cache's
/// controls, 5 and 6 clean it, 11 the display driver, 13 a mode string read
/// into a selector, 14 a selector written as a string, 15 select a string's
/// mode.
pub(crate) fn screen_mode(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        0 => {
            let spec = read_spec(os, regs.r[1])?.ok_or_else(|| os.error(BAD_MODE))?;
            select(os, &spec)
        }
        1 => {
            let named = with(os, |vdu, _| Ok(vdu.screen.named))?;
            regs.r[1] = match named {
                Named::Number(n) => n,
                Named::Selector(address) => address,
            };
            Ok(())
        }
        2 => enumerate(os, regs),
        3 => {
            let monitor = if regs.r[1] == u32::MAX {
                os.configured_display()?[1]
            } else {
                regs.r[1]
            };
            with(os, |vdu, _| {
                vdu.monitor = monitor;
                Ok(())
            })
        }
        4 => {
            regs.r[1] = 1;
            regs.r[2] = 1;
            Ok(())
        }
        5 | 6 => Ok(()),
        11 => {
            if regs.r[1] != u32::MAX && regs.r[1] != 0 {
                return Err(os.error(BAD_REASON));
            }
            regs.r[1] = 0;
            Ok(())
        }
        13 => {
            let text = os.read_text(regs.r[1], 256)?;
            let selector =
                crate::modestring::parse(&text).ok_or_else(|| os.error(BAD_MODE_STRING))?;
            let mut buffer = [0; SELECTOR_BYTES as usize];
            let words = selector.bytes(&mut buffer);
            if len32(words.len()) > regs.r[3] {
                return Err(os.buffer_overflow());
            }
            os.write_bytes(regs.r[2], words)
        }
        14 => {
            let Some(selector) = read_selector(os, regs.r[1])? else {
                return Err(os.error(BAD_MODE));
            };
            let Some(text) = crate::modestring::write(&selector) else {
                return Err(os.no_memory());
            };
            let needed = len32(text.len() + 1);
            if regs.r[3] < needed {
                regs.r[3] = 0u32.wrapping_sub(needed);
                return Ok(());
            }
            os.write_terminated(regs.r[2], text.as_bytes(), 0, regs.r[3] as usize)?;
            regs.r[3] = 0;
            Ok(())
        }
        15 => {
            let text = os.read_text(regs.r[1], 256)?;
            let selector =
                crate::modestring::parse(&text).ok_or_else(|| os.error(BAD_MODE_STRING))?;
            select(os, &Spec::Selector(selector))
        }
        _ => Err(os.error(BAD_REASON)),
    }
}

/// `OS_ScreenMode` 2: `Service_EnumerateScreenModes`, and where no module
/// answers, the monitor's own modes, each size and depth once.
fn enumerate(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let monitor = with(os, |vdu, _| Ok(vdu.monitor))?;
    let mut service = Regs::new([
        0,
        service::ENUMERATE_SCREEN_MODES,
        regs.r[2],
        monitor,
        0,
        0,
        regs.r[6],
        regs.r[7],
        0,
        0,
        0,
        0,
        0,
    ]);
    os.service(&mut service)?;
    if service.r[1] == 0 {
        regs.r[1] = 0;
        regs.r[2] = service.r[2];
        regs.r[6] = service.r[6];
        regs.r[7] = service.r[7];
        return Ok(());
    }
    let mut modes: Vec<(u32, u32, u32)> = Vec::new();
    if modes
        .try_reserve_exact(tairix_vdu::mode::MODES.len())
        .is_err()
    {
        return Err(os.no_memory());
    }
    for (n, m) in tairix_vdu::mode::MODES.iter().enumerate() {
        let format = Format::numbered(m);
        let entry = (format.shown_width(), format.height(), format.log2_bpp());
        if shown_on(u32::try_from(n).unwrap_or(u32::MAX), monitor)
            && format.graphic()
            && !modes.contains(&entry)
        {
            modes.push(entry);
        }
    }
    let (mut skip, mut block, mut room) = (regs.r[2], regs.r[6], regs.r[7]);
    let mut more = false;
    for &(width, height, depth) in &modes {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if block == 0 {
            room = room.wrapping_sub(28);
            continue;
        }
        if room < 28 {
            more = true;
            break;
        }
        for (i, word) in [28, 1, width, height, depth, frame_rate(monitor), 0]
            .iter()
            .enumerate()
        {
            os.write_word(block.wrapping_add(4 * len32(i)), *word)?;
        }
        block = block.wrapping_add(28);
        room -= 28;
        regs.r[2] = regs.r[2].wrapping_sub(1);
    }
    regs.r[1] = u32::from(!more);
    regs.r[6] = block;
    regs.r[7] = room;
    Ok(())
}

/// The current monitor type.
pub(crate) fn monitor(os: &mut Os<'_>) -> u32 {
    crate::state::read(os, |vdu, _| vdu.monitor).unwrap_or(3)
}

/// A newly configured monitor type made current.
pub(crate) fn set_monitor(os: &mut Os<'_>, monitor: u32) {
    let _ = crate::state::read(os, |vdu, _| vdu.monitor = monitor);
}

/// The drivers readied at reset: their state, the screen's area sized for
/// the configured screen and mode, and the configured mode selected.
pub(crate) fn start(os: &mut Os<'_>) -> Result<(), Fail> {
    let [mode, monitor, _] = os.configured_display()?;
    let mut info = Regs::default();
    os.perform(X | READ_SYS_INFO, &mut info)?;
    let configured_size = info.r[0];
    let spec = Spec::Number(mode & 0xFF);
    let resolved = match resolve(os, &spec, monitor)? {
        Outcome::Valid(resolved) => resolved,
        Outcome::Substitute(other) => match resolve(os, &other, monitor)? {
            Outcome::Valid(resolved) => resolved,
            _ => fallback(),
        },
        Outcome::Unknown => fallback(),
    };
    let handler = os
        .vdu_routine(crate::routine::SCREEN_AREA)
        .ok_or_else(|| os.no_memory())?;
    let needed = resolved.format.screen_bytes();
    let (base, size) = screen::make_area(os, configured_size.max(needed), handler)?;
    let selector_copy = os.rma_claim(SELECTOR_BYTES)?;
    let check_copy = os.rma_claim(SELECTOR_BYTES)?;
    let format = resolved.format;
    let (text, graphics, colours, palette) = fresh(&format, 0);
    let vdu = Vdu {
        screen: Screen {
            format,
            named: Named::Number(mode & 0x7F),
            frame_rate: resolved.frame_rate,
            base,
            size,
            vdu_start: 0,
            display_start: 0,
            own_bank: 1,
            tv: [0, 0],
            selector_copy,
            check_copy,
            lent: false,
        },
        text,
        graphics,
        colours,
        palette,
        font: crate::font::Font::new(),
        stream: Stream::new(),
        pointer: Pointer::new(),
        frames: Frames::new(os.now()),
        teletext: Teletext::new(),
        monitor,
        shadow: false,
        cursors_removed: 0,
    };
    os.hold_extension(vdu)?;
    let named = name(os, &resolved.spec)?;
    with(os, |vdu, os| apply(vdu, os, &resolved, named, false))?;
    bound_mouse(os, &resolved.format);
    Ok(())
}

/// The mode a configuration naming no mode the monitor can show starts in:
/// RISC OS 5's own default.
fn fallback() -> Resolved {
    let format = tairix_vdu::mode::numbered(DEFAULT_MODE).map_or_else(
        || Format::numbered(&tairix_vdu::mode::MODES[0]),
        Format::numbered,
    );
    Resolved {
        format,
        spec: Spec::Number(DEFAULT_MODE),
        frame_rate: VGA_RATE,
        shadow: false,
    }
}

#[cfg(test)]
mod tests;
