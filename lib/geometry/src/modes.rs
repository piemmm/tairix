//! The resolutions a graphical desktop may take on an output.
//!
//! The output keeps its own mode; a desktop smaller than it is resampled onto
//! it, as a fixed-pixel panel shows any mode but its native one. So a desktop
//! size is offered only where it fills the output at the output's own shape,
//! and one definition serves the session that applies a size and every
//! surface that offers one.

/// The smallest desktop offered: below it the desktop's own chrome does not
/// fit.
pub const DESKTOP_MIN: (u32, u32) = (640, 360);

/// The standard resolutions offered below an output's own, largest area
/// first: the VESA and CEA sizes of every shape a panel ships in.
pub const STANDARD_SIZES: &[(u32, u32)] = &[
    (3840, 2400),
    (3840, 2160),
    (3000, 2000),
    (3200, 1800),
    (3440, 1440),
    (2560, 1600),
    (2560, 1440),
    (2048, 1536),
    (2160, 1440),
    (2560, 1080),
    (1920, 1280),
    (1920, 1200),
    (1920, 1080),
    (1600, 1200),
    (1680, 1050),
    (1400, 1050),
    (1600, 900),
    (1440, 960),
    (1280, 1024),
    (1440, 900),
    (1280, 960),
    (1366, 768),
    (1280, 800),
    (1152, 864),
    (1280, 720),
    (1024, 768),
    (1080, 720),
    (1024, 640),
    (960, 640),
    (1024, 576),
    (960, 540),
    (800, 600),
    (854, 480),
    (640, 480),
    (640, 400),
    (640, 360),
];

/// The desktop sizes an output of `native` pixels offers: its native size
/// first, then each standard size smaller on both axes, no smaller than
/// [`DESKTOP_MIN`], whose shape is within 1% of the output's. An output with
/// no pixels offers nothing.
pub fn desktop_modes(native: (u32, u32)) -> impl Iterator<Item = (u32, u32)> {
    let (width, height) = native;
    let offered = width > 0 && height > 0;
    offered.then_some(native).into_iter().chain(
        STANDARD_SIZES
            .iter()
            .copied()
            .filter(move |&size| offered && fills(native, size)),
    )
}

/// Whether a desktop of `size` fills an output of `native` once resampled.
fn fills((width, height): (u32, u32), (w, h): (u32, u32)) -> bool {
    let (min_w, min_h) = DESKTOP_MIN;
    if w >= width || h >= height || w < min_w || h < min_h {
        return false;
    }
    // |w/h - W/H| <= (W/H) / 100, cross-multiplied so nothing divides.
    let cross = u64::from(w) * u64::from(height);
    let native_cross = u64::from(h) * u64::from(width);
    cross.abs_diff(native_cross) * 100 <= native_cross
}

#[cfg(test)]
#[path = "modes_tests.rs"]
mod tests;
