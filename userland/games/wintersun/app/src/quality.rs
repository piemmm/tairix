//! The detail a frame is drawn at, and the order `auto` gives it up in.
//!
//! Four knobs decide how a frame looks and what it costs: the light buffer's
//! resolution, the shadows, the ground's texture, and the render scale. A
//! [`Detail`] is one setting of all four — what a preset or a player's own
//! choice holds — and a [`Ladder`] is the path `auto` walks through them.
//!
//! # What the ladder turns, and what it leaves alone
//!
//! Only the knobs that cost frame time, in a fixed and total order: light
//! resolution, then shadow softness, then render scale, one notch at a time,
//! each rung fully shed before the next is touched. Two machines at the same
//! step are drawing the same picture, and the step is the one number a
//! diagnostic has to report.
//!
//! The ground's texture is not on it. Its octaves are spent synthesising a
//! tile once, not drawing one, so shedding one frees no frame time — and,
//! because the octave count is the tile cache's generation token, costs a
//! re-synthesis of every tile held, which reads as one more slow frame.

use tairix_wintersun_art::material::Quality as MaterialQuality;
use tairix_wintersun_figure::actor::{readable, Shade};

use crate::camera::Zoom;
use crate::view::Viewport;

/// How finely the light buffer is shaded.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Lighting {
    /// At half the render target's resolution, and upsampled: the authored
    /// quality, since the buffer is low-frequency by nature and the upsample
    /// is invisible.
    Fine,
    /// At a quarter.
    Medium,
    /// At an eighth.
    Coarse,
}

impl Lighting {
    /// Every setting, finest first.
    pub const ALL: [Self; 3] = [Self::Fine, Self::Medium, Self::Coarse];

    /// Log2 of the light buffer's divisor.
    #[must_use]
    pub const fn shift(self) -> u32 {
        match self {
            Self::Fine => 1,
            Self::Medium => 2,
            Self::Coarse => 3,
        }
    }
}

/// How shadows are drawn across the frame.
///
/// A figure's contact shadow and the ground's relief penumbra are one knob:
/// hardening it hardens every shadow edge at once, and only past that does
/// the relief go, since the wider stencil is both the penumbra and the dearer
/// pass.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Shadows {
    /// Feathered contact shadows, and the relief measured across two cells.
    Soft,
    /// One hard ellipse under each figure, and the relief across one cell.
    Hard,
    /// Hard contact shadows, and the ground drawn in its materials' own
    /// colours. A contact shadow never goes: it is what says where a figure
    /// stands and whether it has left the ground.
    Flat,
}

impl Shadows {
    /// Every setting, softest first.
    pub const ALL: [Self; 3] = [Self::Soft, Self::Hard, Self::Flat];

    /// How figures' contact shadows are drawn.
    #[must_use]
    pub const fn shade(self) -> Shade {
        match self {
            Self::Soft => Shade::Soft,
            Self::Hard | Self::Flat => Shade::Hard,
        }
    }

    /// How the ground's relief shading is measured.
    #[must_use]
    pub const fn relief(self) -> Relief {
        match self {
            Self::Soft => Relief::Wide,
            Self::Hard => Relief::Narrow,
            Self::Flat => Relief::Flat,
        }
    }
}

/// How the ground's relief shading is measured.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Relief {
    /// Across two cells: the penumbra, and the dearer stencil.
    Wide,
    /// Across one cell.
    Narrow,
    /// Not at all: the ground drawn in its materials' own colours.
    Flat,
}

/// The render target's size as a share of the window.
///
/// Whole steps only: a fraction that split a world sub-unit would have the
/// view cover a different piece of the world, a zoom rather than a
/// degradation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Resolution {
    /// The window's own resolution.
    Full,
    /// Four fifths of it on each axis.
    FourFifths,
    /// Two thirds.
    TwoThirds,
    /// Half.
    Half,
}

impl Resolution {
    /// Every setting, finest first.
    pub const ALL: [Self; 4] = [Self::Full, Self::FourFifths, Self::TwoThirds, Self::Half];

    /// The fraction of the window the render target is drawn at.
    ///
    /// Numerators that are powers of two no larger than four, so that with
    /// the window's own cap every zoom's step stays a whole number of
    /// sub-units a render pixel.
    #[must_use]
    pub const fn scale(self) -> RenderScale {
        let (numerator, denominator) = match self {
            Self::Full => (1, 1),
            Self::FourFifths => (4, 5),
            Self::TwoThirds => (2, 3),
            Self::Half => (1, 2),
        };
        RenderScale {
            numerator,
            denominator,
        }
    }
}

/// One setting of every knob a frame is drawn with.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Detail {
    /// How finely the light is shaded.
    pub lighting: Lighting,
    /// How shadows are drawn.
    pub shadows: Shadows,
    /// How many octaves of detail the ground's materials are synthesised
    /// with.
    pub ground: MaterialQuality,
    /// The render target's share of the window.
    pub resolution: Resolution,
}

impl Detail {
    /// Every knob at its finest.
    pub const FINEST: Self = Self {
        lighting: Lighting::Fine,
        shadows: Shadows::Soft,
        ground: MaterialQuality::FULL,
        resolution: Resolution::Full,
    };

    /// Every knob at its plainest.
    pub const PLAINEST: Self = Self {
        lighting: Lighting::Coarse,
        shadows: Shadows::Flat,
        ground: MaterialQuality::new(0),
        resolution: Resolution::Half,
    };
}

/// The size of the render target as a fraction of the window.
///
/// Held as a fraction rather than a percentage so the scaling arithmetic
/// is exact and a round trip through it cannot drift.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RenderScale {
    numerator: u32,
    denominator: u32,
}

impl RenderScale {
    /// Render at the window's own resolution.
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// The numerator of the fraction.
    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    /// The denominator of the fraction.
    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }

    /// Whether the render target is the window's own size, so the present
    /// needs no resample at all.
    #[must_use]
    pub const fn is_native(self) -> bool {
        self.numerator == self.denominator
    }

    /// `length` scaled down, never to zero.
    ///
    /// A render target of no pixels is not a degradation, it is a blank
    /// window, so the floor is one pixel.
    #[must_use]
    pub fn apply(self, length: u32) -> u32 {
        let scaled = u64::from(length) * u64::from(self.numerator) / u64::from(self.denominator);
        u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
    }

    /// This fraction of `other`: one scale taken after another.
    #[must_use]
    pub fn of(self, other: Self) -> Self {
        Self {
            numerator: self.numerator.saturating_mul(other.numerator),
            denominator: self.denominator.saturating_mul(other.denominator),
        }
    }

    /// `step` world sub-units a window pixel, as the render target's pixels
    /// see it — each of which covers more of the world by exactly this
    /// fraction's inverse — or `None` where that is not a whole number of
    /// sub-units.
    ///
    /// The terrain pass steps a span by adding the step, so a render target
    /// is only drawn at a fraction that keeps it whole; anything else would
    /// cover a different piece of the world at a coarser resolution, which
    /// is a zoom rather than a degradation.
    #[must_use]
    pub fn step(self, step: i32) -> Option<i32> {
        let widened = i64::from(step).checked_mul(i64::from(self.denominator))?;
        let numerator = i64::from(self.numerator);
        if numerator == 0 || widened % numerator != 0 {
            return None;
        }
        i32::try_from(widened / numerator).ok()
    }
}

/// The fractions a window too large for the software path is rendered at,
/// largest first: the first that brings it inside the cap is used.
///
/// Numerators of one or two, and [`Resolution`]'s of at most four, so the
/// two together keep every zoom's step whole.
pub(crate) const CAPS: [RenderScale; 5] = [
    RenderScale::ONE,
    RenderScale {
        numerator: 2,
        denominator: 3,
    },
    RenderScale {
        numerator: 1,
        denominator: 2,
    },
    RenderScale {
        numerator: 1,
        denominator: 3,
    },
    RenderScale {
        numerator: 1,
        denominator: 4,
    },
];

/// Which knob the ladder last turned.
///
/// Reported for diagnosis: the step says how far the renderer has fallen
/// back, and this says what a viewer is actually seeing less of.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Rung {
    /// Nothing has been shed.
    Full,
    /// A coarser light buffer.
    LightResolution,
    /// Harder contact shadows, then flat relief shading on the ground.
    ShadowSoftness,
    /// A smaller render target, upscaled to the window.
    RenderScale,
}

/// The notches of each rung, in the order they are shed.
///
/// Read once by [`Ladder`] to place a step on a rung, so the order lives in
/// exactly one place and the rung boundaries cannot drift from the knobs'
/// own settings: each rung's notches are its knob's settings past the finest.
const NOTCHES: [(Rung, u8); 3] = [
    (Rung::LightResolution, notches(Lighting::ALL.len())),
    (Rung::ShadowSoftness, notches(Shadows::ALL.len())),
    (Rung::RenderScale, notches(Resolution::ALL.len())),
];

/// The notches a knob of `settings` settings has past its finest.
const fn notches(settings: usize) -> u8 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a knob has a handful of settings"
    )]
    {
        (settings - 1) as u8
    }
}

/// How far `auto` has fallen back.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Ladder {
    step: u8,
}

impl Default for Ladder {
    fn default() -> Self {
        Self::FULL
    }
}

impl Ladder {
    /// Nothing shed.
    pub const FULL: Self = Self { step: 0 };

    /// Every notch of every rung shed.
    pub const MAX_STEP: u8 = {
        let mut total = 0;
        let mut i = 0;
        while i < NOTCHES.len() {
            total += NOTCHES[i].1;
            i += 1;
        }
        total
    };

    /// Steps on the ladder, full included.
    pub const STEPS: usize = Self::MAX_STEP as usize + 1;

    /// The ladder at `step`, clamped to the bottom.
    #[must_use]
    pub const fn new(step: u8) -> Self {
        Self {
            step: if step > Self::MAX_STEP {
                Self::MAX_STEP
            } else {
                step
            },
        }
    }

    /// How many notches have been shed.
    #[must_use]
    pub const fn step(self) -> u8 {
        self.step
    }

    /// One notch further back, or `None` when there is nothing left to
    /// shed.
    ///
    /// `None` is not a failure: it is the renderer having spent its whole
    /// ladder, which the caller reports rather than reaching for the frame
    /// rate.
    #[must_use]
    pub const fn shed(self) -> Option<Self> {
        if self.step < Self::MAX_STEP {
            Some(Self {
                step: self.step + 1,
            })
        } else {
            None
        }
    }

    /// One notch back toward full, or `None` at full.
    #[must_use]
    pub const fn restore(self) -> Option<Self> {
        if self.step > 0 {
            Some(Self {
                step: self.step - 1,
            })
        } else {
            None
        }
    }

    /// Which rung the last shed notch came off.
    #[must_use]
    pub fn rung(self) -> Rung {
        if self.step == 0 {
            return Rung::Full;
        }
        let mut remaining = self.step;
        for (rung, notches) in NOTCHES {
            if remaining <= notches {
                return rung;
            }
            remaining -= notches;
        }
        Rung::RenderScale
    }

    /// How many notches of `rung` have been shed.
    fn shed_on(self, want: Rung) -> usize {
        let mut remaining = self.step;
        for (rung, notches) in NOTCHES {
            let taken = remaining.min(notches);
            if rung == want {
                return usize::from(taken);
            }
            remaining -= taken;
        }
        0
    }

    /// The detail this step draws at: the ground always at its finest, and
    /// every other knob as far down as the step has shed it.
    #[must_use]
    pub fn detail(self) -> Detail {
        // A rung's notches are its knob's settings past the finest, so every
        // count indexes that knob's own list.
        Detail {
            lighting: Lighting::ALL[self.shed_on(Rung::LightResolution)],
            shadows: Shadows::ALL[self.shed_on(Rung::ShadowSoftness)],
            ground: MaterialQuality::FULL,
            resolution: Resolution::ALL[self.shed_on(Rung::RenderScale)],
        }
    }

    /// The deepest `auto` may shed in a `width` × `height` window at `zoom`:
    /// the last step whose frame still draws every figure at a size the art
    /// harness holds readable.
    ///
    /// Every rung but the render scale leaves a figure as legible as it was —
    /// its contact shadow stops at hard and never goes — so the floor falls
    /// in the render-scale rung, at the coarsest fraction that still draws
    /// the smallest figure a record describes at the harness's floor. Where
    /// the window's own resolution already draws it smaller, the zoom the
    /// player chose has made that call, and the floor is the last step before
    /// the render scale moves at all.
    #[must_use]
    pub fn floor(width: u32, height: u32, zoom: Zoom) -> Self {
        let mut floor = Self::FULL;
        let mut ladder = Self::FULL;
        while let Some(next) = ladder.shed() {
            let scale = next.detail().resolution.scale();
            let legible =
                Viewport::new(width, height, scale).is_ok_and(|view| readable(view.step(zoom)));
            if !scale.is_native() && !legible {
                break;
            }
            floor = next;
            ladder = next;
        }
        floor
    }
}

#[cfg(test)]
#[path = "quality_tests.rs"]
mod tests;
