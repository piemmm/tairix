//! The terminal's screen effects: one strength per effect, and the ordered
//! [`Pass`] list a frame goes through.
//!
//! [`Effects`] carries a permille `0..=1000` per effect, which a settings
//! slider sets directly; [`Effects::passes`] resolves those into the physical
//! parameters each pass draws with, and a zero-strength effect contributes no
//! pass, so a terminal with the effects off pays nothing. The passes are drawn
//! by [`crate::pipeline::Pipeline`], which keeps their stages between frames
//! and redraws only what changed. [`EffectKey`] is the one ordered list of
//! which effects exist, what each is called and where its strength lives, so
//! the settings sheet cannot map a slider onto the wrong field.
//!
//! Translucency and backdrop blur are not passes: translucency is the alpha
//! the background is filled at, blended by the compositor, and the blur is
//! the compositor's, the only process that can see behind the window.
//!
//! Animation is a clock, never a spin: every moving pass is a pure function of
//! a monotonically increasing [`Phase`], advanced on a one-shot deadline only
//! while something moves.

use tairix_abi::driver::display::RefreshRate;
use tairix_theme::motion::whole_frames_ns;

/// The full-scale strength of an effect, in permille — the value a slider at
/// its maximum sets.
pub const FULL: u16 = 1000;

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

/// How far a wobble may displace a row, in physical pixels at the reference
/// density, at full strength.
const MAX_WOBBLE_PX: u32 = 6;

/// The strongest per-pixel luminance jitter fuzz applies, as an 8-bit
/// amplitude at full strength.
const MAX_FUZZ_AMPLITUDE: u32 = 56;

/// How much of a lit pixel survives into the next step's afterglow at full
/// phosphor strength, in permille.
const MAX_PERSISTENCE: u32 = 820;

/// The deepest a scan line dims the row it darkens, in permille, at full
/// strength.
const MAX_SCANLINE_DEPTH: u32 = 620;

/// How far a glow reaches from the pixel emitting it, in physical pixels at
/// the reference density.
///
/// Halation is a fixed distance through the faceplate glass, so the reach is a
/// density-scaled constant rather than a second slider: the strength sets how
/// *bright* the halo is, not how wide.
const GLOW_REACH_PX: u32 = 5;

/// How much of the spread light is added back at full glow strength, in
/// permille.
///
/// Above unity, so at full strength a halo is brighter than the light it was
/// spread from — which is what makes the effect read as a tube's bloom rather
/// than a faint wash. Still capped, for the reason [`MIN_OPACITY`] is: a
/// slider must not be able to make text unreadable, and a light scheme's
/// background is bright enough to glow on its own, so at full strength the
/// whole field glows.
const MAX_GLOW_INTENSITY: u32 = 1200;

/// A monotonic animation step.
///
/// Not a wall-clock time: the effects only need *a* value that advances once
/// per animation step, and a step count rather than a timestamp keeps every
/// pass exactly reproducible in a test.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default, PartialOrd, Ord)]
pub struct Phase(pub u32);

impl Phase {
    /// The next step. Wrapping is deliberate and harmless: every pass reads
    /// the phase through a periodic function.
    #[must_use]
    pub const fn advance(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// The fastest an animated effect draws, in millihertz: twenty frames a
/// second.
///
/// Slow enough that an idle terminal with the effects on costs a twentieth of
/// a repaint's work per second rather than a core, and fast enough that a
/// travelling wobble and an analogue noise floor read as motion rather than as
/// a stutter.
pub const FASTEST_MHZ: u32 = 20_000;

/// How long the program waits between frames of an animated effect on a
/// display refreshing at `refresh`: whole display frames, no faster than
/// [`FASTEST_MHZ`].
#[must_use]
pub const fn frame_interval_ns(refresh: RefreshRate) -> u64 {
    whole_frames_ns(refresh, FASTEST_MHZ)
}

/// When the next animation frame is due at `now_ns` on a display refreshing
/// at `refresh`: kept from `armed` while an effect stays `animated`, so a wake
/// for anything else never pushes the frame back; armed a
/// [`frame_interval_ns`] out when an effect starts; and none at all while
/// nothing animates, so the park sleeps outright.
#[must_use]
pub const fn frame_due(
    animated: bool,
    armed: Option<u64>,
    now_ns: u64,
    refresh: RefreshRate,
) -> Option<u64> {
    match (animated, armed) {
        (false, _) => None,
        (true, None) => Some(now_ns.saturating_add(frame_interval_ns(refresh))),
        (true, due) => due,
    }
}

/// One pass, in the order a frame goes through them, carrying the resolved
/// physical parameters rather than the permille the user set.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Pass {
    /// Displace each row horizontally along a travelling sine of this
    /// amplitude in physical pixels.
    Wobble {
        /// Peak displacement, in physical pixels.
        amplitude_px: u32,
    },
    /// Add the decaying afterglow of recently-lit pixels back into the frame.
    Phosphor {
        /// How much of a lit pixel survives one step, in permille.
        persistence: u32,
    },
    /// Dim alternate physical rows.
    ScanLines {
        /// How deeply the dimmed rows are darkened, in permille.
        depth: u32,
    },
    /// Spread the light of brightly-driven pixels into their neighbourhood.
    Glow {
        /// How far the halo reaches, in physical pixels, falling off as a
        /// tent across that reach.
        reach_px: u32,
        /// How much of the spread light is added back, in permille.
        intensity: u32,
    },
    /// Jitter each pixel's luminance.
    Fuzz {
        /// Peak jitter, as an 8-bit amplitude.
        amplitude: u32,
    },
}

/// The most passes the pipeline can hold — one per [`Pass`] variant, since a
/// pass appears at most once.
pub const MAX_PASSES: usize = 5;

/// The strengths every screen effect is set to, each in permille.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Effects {
    /// How opaque the default background is: [`FULL`] is solid, less lets the
    /// desktop through. Never below [`MIN_OPACITY`].
    pub opacity: u16,
    /// How strongly the compositor blurs what is behind the window.
    pub blur: u16,
    /// How deeply alternate rows are dimmed.
    pub scanlines: u16,
    /// How brightly lit pixels glow into their neighbourhood.
    pub glow: u16,
    /// How much per-pixel luminance jitter is added.
    pub fuzz: u16,
    /// How long a lit pixel persists as an afterglow.
    pub phosphor: u16,
    /// How far rows are displaced by the travelling wobble.
    pub wobble: u16,
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
            scanlines: 0,
            glow: 0,
            fuzz: 0,
            phosphor: 0,
            wobble: 0,
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

    /// The ordered passes a frame goes through, at `scale_percent` of the
    /// reference density (so a wobble is the same *apparent* size on a dense
    /// screen as on a sparse one).
    ///
    /// Returns the filled prefix of the array; a zero-strength effect
    /// contributes no pass.
    #[must_use]
    pub fn passes(self, scale_percent: u32) -> ([Pass; MAX_PASSES], usize) {
        let mut passes = [Pass::ScanLines { depth: 0 }; MAX_PASSES];
        let mut len = 0;
        let mut push = |pass: Pass| {
            if let Some(slot) = passes.get_mut(len) {
                *slot = pass;
                len += 1;
            }
        };
        // Geometry first, so what a later pass jitters or dims is the frame
        // as it will actually be seen.
        if self.wobble > 0 {
            let reach = MAX_WOBBLE_PX.saturating_mul(scale_percent.max(1)) / 100;
            let amplitude_px = scaled(self.wobble, reach.max(1)).max(1);
            push(Pass::Wobble { amplitude_px });
        }
        if self.phosphor > 0 {
            push(Pass::Phosphor {
                persistence: scaled(self.phosphor, MAX_PERSISTENCE),
            });
        }
        if self.scanlines > 0 {
            push(Pass::ScanLines {
                depth: scaled(self.scanlines, MAX_SCANLINE_DEPTH),
            });
        }
        // The glow spreads the light the passes above decided on, so it runs
        // after them and its halo reaches into the scan lines' dark rows, as a
        // tube's does. Fuzz stays last so the noise floor reads as noise
        // rather than being smeared into the halo.
        if self.glow > 0 {
            let reach = GLOW_REACH_PX.saturating_mul(scale_percent.max(1)) / 100;
            push(Pass::Glow {
                reach_px: reach.max(1),
                intensity: scaled(self.glow, MAX_GLOW_INTENSITY),
            });
        }
        if self.fuzz > 0 {
            push(Pass::Fuzz {
                amplitude: scaled(self.fuzz, MAX_FUZZ_AMPLITUDE),
            });
        }
        (passes, len)
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
    /// [`Effects::scanlines`].
    ScanLines,
    /// [`Effects::glow`].
    Glow,
    /// [`Effects::fuzz`].
    Fuzz,
    /// [`Effects::phosphor`].
    Phosphor,
    /// [`Effects::wobble`].
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
            Self::ScanLines => effects.scanlines,
            Self::Glow => effects.glow,
            Self::Fuzz => effects.fuzz,
            Self::Phosphor => effects.phosphor,
            Self::Wobble => effects.wobble,
        }
    }

    /// Set this effect's strength in `effects`.
    pub fn set(self, effects: &mut Effects, permille: u16) {
        let field = match self {
            Self::Opacity => &mut effects.opacity,
            Self::Blur => &mut effects.blur,
            Self::ScanLines => &mut effects.scanlines,
            Self::Glow => &mut effects.glow,
            Self::Fuzz => &mut effects.fuzz,
            Self::Phosphor => &mut effects.phosphor,
            Self::Wobble => &mut effects.wobble,
        };
        *field = permille;
    }
}

/// Scale a permille `strength` onto `full`, rounding to nearest.
fn scaled(strength: u16, full: u32) -> u32 {
    let strength = u32::from(strength.min(FULL));
    let full_permille = u32::from(FULL);
    (strength.saturating_mul(full) + full_permille / 2) / full_permille
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod tests;
