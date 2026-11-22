//! The screen's calibration: a table mapping each device colour to the
//! standard colour it shows, three lists of `&BBGGRRDD` pairs, one a device
//! channel. A device level between two of its list's pairs takes the colour
//! interpolated between them in 16.16, and the three channels' colours are
//! summed before the sum is cut toward minus infinity, as RISC OS 5 works it.

use alloc::rc::Rc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::service::CALIBRATION_CHANGED;

use super::{
    make_stale, own, parameters, screen_calibration, write_to, State, BAD_CALIBRATION, MAP_COMMAND,
    MAP_SIZE_COMMAND, MAP_SYNTAX,
};

/// The pairs a list may hold: one a device level.
const MOST_PAIRS: u32 = 256;
/// The words before the pairs: each list's count.
const COUNTS: u32 = 3;
/// A device level's top.
const TOP_LEVEL: u8 = u8::MAX;
/// The default calibration as a table: each device channel its own
/// standard channel, unchanged.
const DEFAULT_TABLE: [u32; 9] = [2, 2, 2, 0, 0x0000_FFFF, 0, 0x00FF_00FF, 0, 0xFF00_00FF];
/// `WriteCalibrationToFile`'s flag to write even the default calibration.
const EVEN_DEFAULT: u32 = 1;

/// The screen's calibration as ColourTrans holds it, and the RMA block its
/// copy is kept in.
pub(super) struct Installed {
    pub(super) table: Rc<Calibration>,
    pub(super) block: u32,
}

/// A table read and found good: its words, and what each device level of
/// each channel contributes, in 16.16.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Calibration {
    words: Vec<u32>,
    levels: [[[i32; 3]; 256]; 3],
}

/// A table whose lists do not each run from level nought to the top.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Invalid;

impl Calibration {
    /// The table at `at`.
    ///
    /// # Errors
    ///
    /// The fault reading it gives.
    pub(crate) fn read(os: &mut Os<'_>, at: u32) -> Result<Result<Self, Invalid>, Fail> {
        let mut counts = [0; 3];
        for (count, i) in counts.iter_mut().zip(0..) {
            *count = os.read_word(at.wrapping_add(4 * i))?;
        }
        if counts.iter().any(|&n| !(2..=MOST_PAIRS).contains(&n)) {
            return Ok(Err(Invalid));
        }
        let length = COUNTS + counts.iter().sum::<u32>();
        let mut words = Vec::new();
        if words.try_reserve_exact(length as usize).is_err() {
            return Err(os.no_memory());
        }
        for i in 0..length {
            words.push(os.read_word(at.wrapping_add(4 * i))?);
        }
        Ok(Self::of_words(words))
    }

    /// The table `words` hold, counts first.
    pub(crate) fn of_words(words: Vec<u32>) -> Result<Self, Invalid> {
        let counts = *words.first_chunk::<3>().ok_or(Invalid)?;
        let mut levels = [[[0; 3]; 256]; 3];
        let mut next = COUNTS as usize;
        for (channel, count) in levels.iter_mut().zip(counts) {
            let pairs = words.get(next..next + count as usize).ok_or(Invalid)?;
            *channel = interpolated(pairs)?;
            next += count as usize;
        }
        if next != words.len() {
            return Err(Invalid);
        }
        Ok(Self { words, levels })
    }

    /// The table's words, counts first.
    pub(crate) fn words(&self) -> &[u32] {
        &self.words
    }

    /// The standard colour device colour `device` shows.
    pub(crate) fn convert(&self, device: Rgb) -> Rgb {
        let mut sum = [0; 3];
        for (channel, level) in self.levels.iter().zip(device.to_array()) {
            for (total, part) in sum.iter_mut().zip(channel[usize::from(level)]) {
                *total += part;
            }
        }
        let [r, g, b] = sum.map(|c| u8::try_from((c >> 16).clamp(0, 255)).unwrap_or(u8::MAX));
        Rgb::new(r, g, b)
    }
}

/// Each device level's colour in 16.16 from a list's `pairs`: ascending
/// levels from nought to the top, each between the pairs either side of it.
fn interpolated(pairs: &[u32]) -> Result<[[i32; 3]; 256], Invalid> {
    let level = |word: u32| word.to_le_bytes()[0];
    let colour = |word: u32| {
        let [_, r, g, b] = word.to_le_bytes();
        [r, g, b].map(|c| i32::from(c) << 16)
    };
    let (Some(&first), Some(&last)) = (pairs.first(), pairs.last()) else {
        return Err(Invalid);
    };
    if level(first) != 0 || level(last) != TOP_LEVEL {
        return Err(Invalid);
    }
    let mut out = [[0; 3]; 256];
    out[0] = colour(first);
    for pair in pairs.windows(2) {
        let (low, high) = (level(pair[0]), level(pair[1]));
        if high <= low {
            return Err(Invalid);
        }
        let (from, to) = (colour(pair[0]), colour(pair[1]));
        let span = i32::from(high - low);
        for v in low..=high {
            let t = (i32::from(v - low) << 16) / span;
            out[usize::from(v)] = core::array::from_fn(|c| from[c] + ((to[c] - from[c]) >> 16) * t);
        }
    }
    Ok(out)
}

/// `ColourTrans_SetCalibration`: R0 the table, nought for the default. A
/// table refused leaves the default calibration, which is announced as a
/// change, as RISC OS 5 announces it.
pub(super) fn set(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] == 0 {
        return install(os, None);
    }
    match Calibration::read(os, regs.r[0])? {
        Ok(table) => set_table(os, table),
        Err(Invalid) => refused(os),
    }
}

/// A table refused: the default calibration, announced, and the refusal.
fn refused(os: &mut Os<'_>) -> Result<(), Fail> {
    install(os, None)?;
    announce(os)?;
    Err(os.own_error(own, BAD_CALIBRATION, &[]))
}

/// `table` made the screen's calibration and announced.
fn set_table(os: &mut Os<'_>, table: Calibration) -> Result<(), Fail> {
    let installed = install(os, Some(table));
    announce(os)?;
    installed
}

/// `table` held as the screen's calibration, the default where none; the
/// default where its RMA copy finds no room.
fn install(os: &mut Os<'_>, table: Option<Calibration>) -> Result<(), Fail> {
    if let Some(old) = os
        .extension::<State>()
        .and_then(|state| state.calibration.take())
    {
        os.rma_free(old.block)?;
    }
    make_stale(os);
    let Some(table) = table else {
        return Ok(());
    };
    let words = table.words();
    let block = os.rma_claim(len32(4 * words.len()))?;
    for (i, &word) in (0u32..).zip(words) {
        os.write_word(block + 4 * i, word)?;
    }
    if let Some(state) = os.extension::<State>() {
        state.calibration = Some(Installed {
            table: Rc::new(table),
            block,
        });
    }
    Ok(())
}

/// `Service_CalibrationChanged`.
fn announce(os: &mut Os<'_>) -> Result<(), Fail> {
    let mut service = Regs::new([0, CALIBRATION_CHANGED, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    os.service(&mut service).map_err(Fail::Unwind)
}

/// `ColourTrans_ReadCalibration`: the table copied to R0 unless it is
/// nought, R1 its size; nought for the default.
pub(super) fn read_back(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let table = screen_calibration(os);
    let words = table.as_ref().map_or(&[][..], |table| table.words());
    if regs.r[0] != 0 {
        for (i, &word) in (0u32..).zip(words) {
            os.write_word(regs.r[0].wrapping_add(4 * i), word)?;
        }
    }
    regs.r[1] = len32(4 * words.len());
    Ok(())
}

/// The table R3 names: nought the screen's, else one at R3, refused where it
/// is not good.
fn table_at(os: &mut Os<'_>, r3: u32) -> Result<Option<Rc<Calibration>>, Fail> {
    if r3 == 0 {
        return Ok(screen_calibration(os));
    }
    match Calibration::read(os, r3)? {
        Ok(table) => Ok(Some(Rc::new(table))),
        Err(Invalid) => Err(os.own_error(own, BAD_CALIBRATION, &[])),
    }
}

/// `ColourTrans_ConvertDeviceColour`: R1's device colour as the standard
/// colour R2 it shows by R3's table; unchanged by the default.
pub(super) fn convert_colour(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[2] = match table_at(os, regs.r[3])? {
        Some(table) => table
            .convert(Rgb::from_riscos_word(regs.r[1]))
            .riscos_word(),
        None => regs.r[1],
    };
    Ok(())
}

/// `ColourTrans_ConvertDevicePalette`: R0 device colours at R1 written as
/// standard colours at R2 by R3's table, every register kept.
pub(super) fn convert_palette(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let table = table_at(os, regs.r[3])?;
    for i in 0..regs.r[0] {
        let offset = i.wrapping_mul(4);
        let device = os.read_word(regs.r[1].wrapping_add(offset))?;
        let standard = table.as_ref().map_or(device, |table| {
            table.convert(Rgb::from_riscos_word(device)).riscos_word()
        });
        os.write_word(regs.r[2].wrapping_add(offset), standard)?;
    }
    Ok(())
}

/// `ColourTrans_WriteCalibrationToFile`: the commands that set the
/// calibration written to R1's file, the default's only where R0 asks. R0
/// is left as the last character written, as RISC OS 5 leaves it.
pub(super) fn write_to_file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if let Some(last) = write_commands(os, regs.r[1], regs.r[0] & EVEN_DEFAULT != 0)? {
        regs.r[0] = u32::from(last);
    }
    Ok(())
}

/// The calibration's commands written to the file `handle`: none for the
/// default unless `even_default`. The last character written, if any.
pub(super) fn write_commands(
    os: &mut Os<'_>,
    handle: u32,
    even_default: bool,
) -> Result<Option<u8>, Fail> {
    let table = screen_calibration(os);
    let words = match table.as_ref() {
        Some(table) => table.words(),
        None if even_default => &DEFAULT_TABLE[..],
        None => return Ok(None),
    };
    let Some(text) = commands(words) else {
        return Err(os.no_memory());
    };
    write_to(os, handle, &text)?;
    Ok(text.last().copied())
}

/// The commands that set the table `words` hold: its sizes blue first and
/// its pairs last first, the order the commands read them in; none where
/// there is no room for them.
fn commands(words: &[u32]) -> Option<Vec<u8>> {
    let (counts, pairs) = words.split_at(COUNTS as usize);
    let mut text = Vec::new();
    text.try_reserve(MAP_SIZE_COMMAND.len() + MAP_COMMAND.len() + 11 * words.len() + 2)
        .ok()?;
    text.extend_from_slice(MAP_SIZE_COMMAND.as_bytes());
    for &count in counts.iter().rev() {
        number(&mut text, count);
    }
    text.push(b'\n');
    text.extend_from_slice(MAP_COMMAND.as_bytes());
    for &pair in pairs.iter().rev() {
        number(&mut text, pair);
    }
    text.push(b'\n');
    Some(text)
}

/// ` &value` in hexadecimal appended to `text`.
fn number(text: &mut Vec<u8>, value: u32) {
    text.extend_from_slice(b" &");
    text.extend_from_slice(tairix_riscos_kernel::hex(value).as_bytes());
}

/// `*ColourTransMapSize <blue> <green> <red>`: the pairs each list of the
/// next `*ColourTransMap` gives.
pub(super) fn map_size_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut sizes = [0; 3];
    parameters(os, regs.r[0], &mut sizes)?;
    sizes.reverse();
    if let Some(state) = os.extension::<State>() {
        state.map_size = sizes;
    }
    Ok(())
}

/// `*ColourTransMap <pair> ...`: the calibration from its pairs, the last
/// pair first, as many as `*ColourTransMapSize` set, made the screen's as
/// `ColourTrans_SetCalibration` makes a table its.
pub(super) fn map_command(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let counts = os
        .extension::<State>()
        .map_or([0; 3], |state| state.map_size);
    let pairs: u64 = counts.iter().map(|&n| u64::from(n)).sum();
    if u64::from(regs.r[1]) != pairs {
        return Err(os.syntax_failure(MAP_SYNTAX));
    }
    let length = COUNTS as usize + regs.r[1] as usize;
    let mut words = Vec::new();
    if words.try_reserve_exact(length).is_err() {
        return Err(os.no_memory());
    }
    words.extend_from_slice(&counts);
    words.resize(length, 0);
    parameters(os, regs.r[0], &mut words[COUNTS as usize..])?;
    words[COUNTS as usize..].reverse();
    match Calibration::of_words(words) {
        Ok(table) => set_table(os, table),
        Err(Invalid) => refused(os),
    }
}

#[cfg(test)]
mod tests;
