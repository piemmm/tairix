//! The Font Manager's settings and the calls that read and set them: the
//! scale between millipoints and OS units, the `FontMax` values and the cache
//! the guest sees, each as RISC OS 5.30 keeps it.

use tairix_drawing::riscos::units::MILLIPOINTS_PER_OS_UNIT;
use tairix_riscos_kernel::cmos;
use tairix_riscos_kernel::memory::{Create, FONT_CACHE as CACHE_AREA};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as kos, X};
use tairix_vdu::mode::var::{X_EIG, Y_EIG};

use crate::grid::{Grid, MOST_EIG};
use crate::module::with;

/// The scale both ways at start: millipoints an OS unit.
const SCALE: i32 = MILLIPOINTS_PER_OS_UNIT.cast_signed();
/// `Font_CacheAddr`'s version: RISC OS 5.30's Font Manager's, in
/// hundredths.
const VERSION: u32 = tairix_riscos_kernel::module::inventory::FONT_MANAGER_ROM.version as u32;

/// The `FontMax` values in CMOS: `FontMax` in 64 KiB units, then `FontMax1`
/// to `FontMax5` in points, the first three stored exclusive-ored with these so
/// that a cleared CMOS holds their defaults.
pub(crate) const FONT_MAX_CMOS: u8 = 200;
pub(crate) const FONT_MAX_UNIT: u32 = 64 * 1024;
pub(crate) const FONT_MAX_STORED: [u8; 6] = [0, 16, 12, 24, 0, 0];

/// The font cache's dynamic area's title, and the most it may grow to: the
/// largest `FontMax` CMOS can configure.
const CACHE_TITLE: &[u8] = b"Font cache";
const CACHE_MOST: u32 = 255 * FONT_MAX_UNIT;

/// What the Font Manager is set to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// Millipoints an OS unit, across and up.
    pub(crate) scale: (i32, i32),
    /// `FontMax` in bytes, then `FontMax1` to `FontMax5` in points.
    pub(crate) font_max: [u32; 6],
}

impl Settings {
    /// The settings at start, `FontMax` as CMOS configures it.
    pub(crate) fn configured(os: &mut Os<'_>) -> Result<Self, Fail> {
        let mut font_max = [0; 6];
        for ((value, stored), location) in font_max
            .iter_mut()
            .zip(FONT_MAX_STORED)
            .zip(FONT_MAX_CMOS..)
        {
            *value = u32::from(os.read_cmos(location)? ^ stored);
        }
        font_max[0] *= FONT_MAX_UNIT;
        Ok(Self {
            scale: (SCALE, SCALE),
            font_max,
        })
    }
}

/// Makes the font cache's dynamic area, as large as `*Configure FontSize`
/// says. The cache itself is the session's, under its memory policy; the
/// area is what the guest sees of it.
pub(crate) fn make_cache_area(os: &mut Os<'_>) -> Result<(), Fail> {
    let units = os.read_cmos(cmos::FONT_SIZE)?;
    let caller = os.layout().kernel_bits();
    os.create_kernel_area(&Create {
        number: Some(CACHE_AREA),
        size: u32::from(units) * cmos::FONT_SIZE_UNIT,
        base: None,
        flags: 0,
        max: Some(CACHE_MOST),
        handler: None,
        workspace: None,
        title: Some(CACHE_TITLE),
        pool: 0,
        caller,
    })?;
    Ok(())
}

/// Removes the font cache's dynamic area, as the module is finalised.
pub(crate) fn remove_cache_area(os: &mut Os<'_>) -> Result<(), Fail> {
    os.remove_kernel_area(CACHE_AREA)
}

/// The font cache's size and what of it the fonts' data and glyphs use, at
/// most all of it.
pub(crate) fn cache_size(os: &mut Os<'_>) -> Result<(u32, u32), Fail> {
    let (_, size) = os.area_extent(CACHE_AREA)?;
    let held = with(os, |state| state.store.bytes())?;
    Ok((size, u32::try_from(held).unwrap_or(u32::MAX).min(size)))
}

/// `Font_CacheAddr`.
pub(crate) fn cache_addr(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (size, used) = cache_size(os)?;
    regs.r[0] = VERSION;
    regs.r[1] = 0;
    regs.r[2] = size;
    regs.r[3] = used;
    Ok(())
}

/// `Font_ReadScaleFactor`.
pub(crate) fn read_scale_factor(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (x, y) = with(os, |state| state.settings.scale)?;
    regs.r[1] = x.cast_unsigned();
    regs.r[2] = y.cast_unsigned();
    Ok(())
}

/// `Font_SetScaleFactor`, which takes any scale.
pub(crate) fn set_scale_factor(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let scale = (regs.r[1].cast_signed(), regs.r[2].cast_signed());
    with(os, |state| state.settings.scale = scale)
}

/// `Font_ConverttoOS`: millipoints to OS units, down to a whole pixel of
/// the current mode. A scale of nought, on which RISC OS hangs, gives
/// nought.
pub(crate) fn convert_to_os(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (sx, sy) = with(os, |state| state.settings.scale)?;
    let (ex, ey) = (eigen_x(os)?, eigen_y(os)?);
    regs.r[1] = to_os(regs.r[1].cast_signed(), sx, ex).cast_unsigned();
    regs.r[2] = to_os(regs.r[2].cast_signed(), sy, ey).cast_unsigned();
    Ok(())
}

/// `Font_Converttopoints`: OS units to millipoints, wrapping as RISC OS's
/// multiplication does.
pub(crate) fn convert_to_points(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (sx, sy) = with(os, |state| state.settings.scale)?;
    regs.r[1] = regs.r[1].cast_signed().wrapping_mul(sx).cast_unsigned();
    regs.r[2] = regs.r[2].cast_signed().wrapping_mul(sy).cast_unsigned();
    Ok(())
}

/// The screen's grid at the scale set and the current mode's eigen
/// factors; none where the scale gives it no pixels.
pub(crate) fn grid(os: &mut Os<'_>) -> Result<Option<Grid>, Fail> {
    let scale = with(os, |state| state.settings.scale)?;
    Ok(Grid::of(scale, (eigen_x(os)?, eigen_y(os)?)))
}

/// The current mode's eigen factors, across and up.
pub(crate) fn eigen_x(os: &mut Os<'_>) -> Result<u32, Fail> {
    eigen(os, X_EIG)
}

pub(crate) fn eigen_y(os: &mut Os<'_>) -> Result<u32, Fail> {
    eigen(os, Y_EIG)
}

/// The current mode's eigen factor `variable`.
fn eigen(os: &mut Os<'_>, variable: u32) -> Result<u32, Fail> {
    let mut read = Regs::default();
    read.r[..2].copy_from_slice(&[kos::CURRENT_MODE, variable]);
    os.perform(X | kos::READ_MODE_VARIABLE, &mut read)?;
    Ok(read.r[2].min(MOST_EIG))
}

/// `millipoints` in OS units at `scale`, floored to a pixel of `eigen`.
pub(crate) fn to_os(millipoints: i32, scale: i32, eigen: u32) -> i32 {
    let pixel = 1i64 << eigen;
    let divisor = i64::from(scale) * pixel;
    if divisor == 0 {
        return 0;
    }
    let dividend = i64::from(millipoints);
    let (quotient, remainder) = (dividend / divisor, dividend % divisor);
    let pixels = if remainder != 0 && (remainder < 0) != (divisor < 0) {
        quotient - 1
    } else {
        quotient
    };
    i32::try_from(pixels * pixel).unwrap_or(0)
}

/// `Font_ReadFontMax`.
pub(crate) fn read_font_max(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let values = with(os, |state| state.settings.font_max)?;
    regs.r[..6].copy_from_slice(&values);
    Ok(())
}

/// `Font_SetFontMax`, which takes any values and leaves CMOS be.
pub(crate) fn set_font_max(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut values = [0; 6];
    values.copy_from_slice(&regs.r[..6]);
    with(os, |state| state.settings.font_max = values)
}

/// `Font_UnCacheFile`: what is cached of the file is let go, to be read
/// again as it now is when next wanted: every font's data, since a file
/// may be any font's, and the catalogue.
pub(crate) fn uncache_file(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    with(os, |state| {
        state.catalogue = None;
        state.store.forget_all();
    })
}

#[cfg(test)]
mod tests;
