//! The size a machine window opens at. The picture in the window, the
//! window's points as the frame's dots, the library's thumbnail and the
//! screenshot are `tairix_crt::screen`'s.

use tairix_crt::screen::ASPECT;

/// A VGA screen's dots and lines, which a machine window opens to show at one
/// pixel a dot, and which a screenshot is at least as wide as.
pub const VGA_SCREEN: (u32, u32) = (640, 480);

const _: () = assert!(VGA_SCREEN.0 * ASPECT.1 == VGA_SCREEN.1 * ASPECT.0);

/// The client a machine window opens at over a bar `bar` pixels tall: a VGA
/// screen at one pixel a dot, the picture `fit` places, above the bar.
#[must_use]
pub const fn opening_extent(bar: u32) -> (u32, u32) {
    (VGA_SCREEN.0, VGA_SCREEN.1.saturating_add(bar))
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
