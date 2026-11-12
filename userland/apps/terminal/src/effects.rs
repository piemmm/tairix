//! The terminal's screen effects: how see-through and frosted the window is,
//! and the tube its picture is drawn through.
//!
//! [`Effects`] carries a permille `0..=1000` per effect, which a settings
//! slider sets directly. Translucency and backdrop blur are the window's own:
//! translucency is the alpha the background is filled at, blended by the
//! compositor, and the blur is the compositor's, the only process that can see
//! behind the window. The rest are the [`Tube`]'s, drawn by
//! `tairix_crt::Pipeline`. [`EffectKey`] is the one ordered list of which
//! effects exist, what each is called and where its strength lives, so the
//! settings sheet cannot map a slider onto the wrong field.

use tairix_crt::{scaled, Tube, FULL};

/// The largest backdrop-blur radius the terminal will ask the compositor for,
/// in logical pixels.
///
/// A fixed bound on how much per-frame work one window may demand of the
/// compositor, not a capacity that grows with the machine: a wider blur costs
/// the desktop, not this app.
pub const MAX_BLUR_RADIUS_PX: u16 = 24;

/// The least opaque a background may be made, in permille.
///
/// A window faded past this stops being a terminal and starts being a way to
/// make text unreadable, so the slider cannot reach it. Text itself never
/// fades — only the background does.
pub const MIN_OPACITY: u16 = 300;

/// The strengths every screen effect is set to, each in permille.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Effects {
    /// How opaque the default background is: [`FULL`] is solid, less lets the
    /// desktop through. Never below [`MIN_OPACITY`].
    pub opacity: u16,
    /// How strongly the compositor blurs what is behind the window.
    pub blur: u16,
    /// The tube the picture is drawn through.
    pub tube: Tube,
}

impl Default for Effects {
    /// A see-through, frosted window, and every effect that costs a frame off.
    ///
    /// Translucency is free: it is the alpha the background is filled at while
    /// the cells are painted, so the compositor's own blend does the work. The
    /// blur is what makes it read as glass rather than as a hole, and it is
    /// half strength because that is the depth at which text stays legible over
    /// a busy backdrop. The compositor stopped computing the frosts a stack of
    /// them buries (`plans/FIX-DESKTOP-SPEEDUP.md` D.13), which is what made a
    /// screenful of frosted terminals affordable.
    fn default() -> Self {
        Self {
            opacity: 800,
            blur: 500,
            tube: Tube::default(),
        }
    }
}

impl Effects {
    /// The alpha the default background is filled at, `0..=255`.
    #[must_use]
    pub fn background_alpha(self) -> u8 {
        let clamped = self.opacity.clamp(MIN_OPACITY, FULL);
        u8::try_from(scaled(clamped, 255)).unwrap_or(u8::MAX)
    }

    /// The backdrop-blur radius to ask the compositor for, in logical pixels.
    ///
    /// A blur behind an opaque window would be invisible work, so a fully
    /// opaque terminal asks for none however far the slider is pushed.
    #[must_use]
    pub fn blur_radius_px(self) -> u16 {
        if self.opacity >= FULL {
            return 0;
        }
        u16::try_from(scaled(self.blur, u32::from(MAX_BLUR_RADIUS_PX)))
            .unwrap_or(MAX_BLUR_RADIUS_PX)
    }
}

/// Which screen effects exist, what each is called, and where its strength
/// lives in [`Effects`].
///
/// The settings sheet builds a slider row per key and reads a row back
/// through this same list, so a reordering cannot redirect a slider onto
/// another effect's field.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EffectKey {
    /// [`Effects::opacity`].
    Opacity,
    /// [`Effects::blur`].
    Blur,
    /// [`Tube::scanlines`].
    ScanLines,
    /// [`Tube::glow`].
    Glow,
    /// [`Tube::fuzz`].
    Fuzz,
    /// [`Tube::phosphor`].
    Phosphor,
    /// [`Tube::wobble`].
    Wobble,
}

impl EffectKey {
    /// Every effect, in the order the settings sheet lists them.
    pub const ALL: [Self; 7] = [
        Self::Opacity,
        Self::Blur,
        Self::ScanLines,
        Self::Glow,
        Self::Fuzz,
        Self::Phosphor,
        Self::Wobble,
    ];

    /// How many effects there are.
    pub const COUNT: usize = Self::ALL.len();

    /// The human label the settings sheet shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Opacity => "Opacity",
            Self::Blur => "Backdrop blur",
            Self::ScanLines => "Scan lines",
            Self::Glow => "Glow",
            Self::Fuzz => "Fuzz",
            Self::Phosphor => "Phosphor",
            Self::Wobble => "Wobble",
        }
    }

    /// The closed range this effect's slider spans.
    ///
    /// Opacity starts at [`MIN_OPACITY`] rather than zero because a fully
    /// transparent screen is unreadable; mapping the slider onto that range
    /// keeps the whole travel live instead of leaving its first third
    /// snapping back.
    #[must_use]
    pub const fn bounds(self) -> (u16, u16) {
        match self {
            Self::Opacity => (MIN_OPACITY, FULL),
            Self::Blur
            | Self::ScanLines
            | Self::Glow
            | Self::Fuzz
            | Self::Phosphor
            | Self::Wobble => (0, FULL),
        }
    }

    /// This effect's strength in `effects`.
    #[must_use]
    pub const fn of(self, effects: Effects) -> u16 {
        match self {
            Self::Opacity => effects.opacity,
            Self::Blur => effects.blur,
            Self::ScanLines => effects.tube.scanlines,
            Self::Glow => effects.tube.glow,
            Self::Fuzz => effects.tube.fuzz,
            Self::Phosphor => effects.tube.phosphor,
            Self::Wobble => effects.tube.wobble,
        }
    }

    /// Set this effect's strength in `effects`.
    pub fn set(self, effects: &mut Effects, permille: u16) {
        let field = match self {
            Self::Opacity => &mut effects.opacity,
            Self::Blur => &mut effects.blur,
            Self::ScanLines => &mut effects.tube.scanlines,
            Self::Glow => &mut effects.tube.glow,
            Self::Fuzz => &mut effects.tube.fuzz,
            Self::Phosphor => &mut effects.tube.phosphor,
            Self::Wobble => &mut effects.tube.wobble,
        };
        *field = permille;
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
