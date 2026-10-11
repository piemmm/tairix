//! The resolution the user asked the graphical desktop to be drawn at.
//!
//! The document spells it `native` or `<width>x<height>`, and only a standard
//! size ([`STANDARD_SIZES`]) is a value: every size an output offers below its
//! own is one, so no other spelling could ever be drawn.

use alloc::format;
use alloc::string::{String, ToString};

use tairix_geometry::{desktop_modes, STANDARD_SIZES};

use crate::input::parse_decimal;

/// The size the desktop is drawn at.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Resolution {
    /// The output's own size.
    #[default]
    Native,
    /// A standard size, resampled onto any output that offers it.
    Size {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
}

impl Resolution {
    const NATIVE: &'static str = "native";

    /// Decode `native`, or a standard size spelled `<width>x<height>`.
    #[must_use]
    pub fn from_value(value: &str) -> Option<Self> {
        if value == Self::NATIVE {
            return Some(Self::Native);
        }
        let (width, height) = value.split_once('x')?;
        let size = (parse_decimal(width)?, parse_decimal(height)?);
        STANDARD_SIZES.contains(&size).then_some(Self::Size {
            width: size.0,
            height: size.1,
        })
    }

    /// The canonical value spelling.
    #[must_use]
    pub fn render_value(self) -> String {
        match self {
            Self::Native => Self::NATIVE.to_string(),
            Self::Size { width, height } => format!("{width}x{height}"),
        }
    }

    /// The size to draw on an output of `output` pixels: this one where the
    /// output offers it, and the output's own otherwise, so a choice made on
    /// another monitor never draws a desktop this one cannot show.
    #[must_use]
    pub fn on(self, output: (u32, u32)) -> (u32, u32) {
        match self {
            Self::Size { width, height }
                if desktop_modes(output).any(|size| size == (width, height)) =>
            {
                (width, height)
            }
            _ => output,
        }
    }

    /// The choice that draws `size` on an output of `output` pixels.
    #[must_use]
    pub const fn of(size: (u32, u32), output: (u32, u32)) -> Self {
        if size.0 == output.0 && size.1 == output.1 {
            Self::Native
        } else {
            Self::Size {
                width: size.0,
                height: size.1,
            }
        }
    }
}

#[cfg(test)]
#[path = "resolution_tests.rs"]
mod tests;
