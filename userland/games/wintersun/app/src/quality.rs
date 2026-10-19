//! The detail a frame is drawn at, and the order `auto` gives it up in.
//!
//! Three knobs decide how a frame looks and what it costs: the shadows, the
//! ground's texture, and the render scale. A [`Detail`] is one setting of all
//! three — what a preset or a player's own choice holds — and a [`Ladder`] is
//! the path `auto` walks through them.
//!
//! # What the ladder turns, and what it leaves alone
//!
//! Only the knobs that cost frame time, in a fixed and total order: shadow
//! softness, then render scale, one notch at a time, each rung fully shed
//! before the next is touched. Two machines at the same step are drawing the
//! same picture, and the step is the one number a diagnostic has to report.
//!
//! The light is not on it: it is taken per pixel from the surface's own
//! normal, which costs a table lookup rather than a buffer to be shaded at a
//! fraction of the resolution.
//!
//! The ground's texture is not on it. Its octaves are spent synthesising a
//! tile once, not drawing one, so shedding one frees no frame time — and,
//! because the octave count is the tile cache's generation token, costs a
//! re-synthesis of every tile held, which reads as one more slow frame.

use tairix_wintersun_art::material::Quality as MaterialQuality;
use tairix_wintersun_figure::actor::{readable, Shade};

use crate::camera::{focus_step, Zoom};
use crate::view::Viewport;

/// How shadows are drawn across the frame.
///
/// A figure's contact shadow and the ground's own shading are one knob:
/// hardening it hardens every shadow edge at once, and only past that does
/// the ground give up its light.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Shadows {
    /// Feathered contact shadows over lit ground.
    Soft,
    /// One hard ellipse under each figure, over lit ground.
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

    /// Whether the ground takes the sun's light at all.
    #[must_use]
    pub const fn lit(self) -> bool {
        !matches!(self, Self::Flat)
    }
}

/// The render target's size as a share of the window.
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
        shadows: Shadows::Soft,
        ground: MaterialQuality::FULL,
        resolution: Resolution::Full,
    };

    /// Every knob at its plainest.
    pub const PLAINEST: Self = Self {
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
}

/// The fractions a window too large for the software path is rendered at,
/// largest first: the first that brings it inside the cap is used.
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
    /// Harder contact shadows, then unlit ground.
    ShadowSoftness,
    /// A smaller render target, upscaled to the window.
    RenderScale,
}

/// The notches of each rung, in the order they are shed.
///
/// Read once by [`Ladder`] to place a step on a rung, so the order lives in
/// exactly one place and the rung boundaries cannot drift from the knobs'
/// own settings: each rung's notches are its knob's settings past the finest.
const NOTCHES: [(Rung, u8); 2] = [
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
            shadows: Shadows::ALL[self.shed_on(Rung::ShadowSoftness)],
            ground: MaterialQuality::FULL,
            resolution: Resolution::ALL[self.shed_on(Rung::RenderScale)],
        }
    }

    /// The deepest `auto` may shed in a `width` × `height` window at `zoom`:
    /// the last step whose frame still draws every figure at the camera's
    /// focus at a size the art harness holds readable.
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
            let legible = Viewport::new(width, height, scale)
                .is_ok_and(|view| readable(focus_step(zoom, &view)));
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
