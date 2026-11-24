//! A session's desktop font: the face the TAIRiX desktop sets its interface
//! text in, which the guest's desktop font is (`plans/RISCOS-EMULATOR.md`
//! §8).

use tairix_abi::font_ipc::FamilyKey;

/// The TAIRiX desktop's interface text as a session's desktop font: the font
/// service's family it is set in, and its size in sixteenths of a point
/// where the host knows it, RISC OS's own desktop size standing otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopFont {
    /// The font service's family.
    pub family: FamilyKey,
    /// The size, in sixteenths of a point.
    pub size: Option<u16>,
}

impl DesktopFont {
    /// The shipped themes' interface family, at no size of its own: a
    /// session whose host has no desktop.
    pub const SHIPPED: Self = Self {
        family: tairix_theme::UI_FAMILY,
        size: None,
    };
}
