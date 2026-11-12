//! What a terminal window's screen is drawn with — its face, its colours, its
//! effects and their retained stages — and what a change to the profile or to
//! the desktop makes of it.
//!
//! The two reach it separately. A profile change names what it moved
//! ([`Invalidation::between`]) and [`Look::adopt`] re-derives that alone. A
//! desktop change can move only the face's fit and the scale
//! ([`Look::refresh`]): every colour is the profile's, so a light/dark switch
//! leaves a terminal window as it was.

use tairix_font::BitmapFont;
use tairix_geometry::Scale;
use tairix_theme::Theme;
use tairix_window::Desktop;

use tairix_crt::{Phase, Pipeline};

use crate::effects::Effects;
use crate::layout::{cell_font, fit_font_size};
use crate::profile::{Invalidation, Profile};
use crate::scheme::Painted;

/// Everything a terminal window holds about how its screen currently looks.
///
/// Derived from the [`Profile`] and the desktop, and re-derived whenever
/// either changes, so the renderer, the window geometry, and the effect
/// pipeline can never disagree about what is in force. `P` is the embedder's
/// handle on the workers the passes spread across; nothing here reads it.
#[derive(Debug)]
pub struct Look<P> {
    /// The face the screen is drawn in.
    pub font: BitmapFont,
    /// The colours the screen is painted with.
    pub painted: Painted,
    /// The effect strengths in force, held beside the colours they were
    /// resolved with so a frame cannot be drawn under one profile and
    /// post-processed under another.
    pub effects: Effects,
    /// The desktop scale the face and the effects are sized at.
    pub scale: Scale,
    /// The animation step the effects are drawn at.
    pub phase: Phase,
    /// The effect passes' stages, kept between frames.
    pub pipeline: Pipeline,
    /// The workers the passes spread across.
    pub pool: P,
    /// The text size the profile asks for, which a desktop change fits again
    /// without consulting the profile.
    font_size_px: u16,
}

impl<P> Look<P> {
    /// The look `profile` implies on `desktop` under `theme`, its passes
    /// spread across `pool`.
    #[must_use]
    pub fn resolve(profile: &Profile, theme: &Theme, desktop: &Desktop, pool: P) -> Self {
        let (font, scale) = fit(profile.font_size_px, theme, desktop);
        Self {
            font,
            painted: colours(profile),
            effects: profile.effects,
            scale,
            phase: Phase::default(),
            pipeline: Pipeline::new(),
            pool,
            font_size_px: profile.font_size_px,
        }
    }

    /// Fit the face to a changed `desktop` under `theme`, answering what that
    /// made stale: the cell geometry when the face or the scale moved, and
    /// nothing else.
    ///
    /// No colour is the desktop's, so none is re-derived, and the effect
    /// stages are kept unless the face they were drawn over has gone.
    #[must_use]
    pub fn refresh(&mut self, theme: &Theme, desktop: &Desktop) -> Invalidation {
        let now = fit(self.font_size_px, theme, desktop);
        let changed = Invalidation::refitted((self.font, self.scale), now);
        if changed.metrics() {
            (self.font, self.scale) = now;
            // A trail of the old face would ghost over the new one.
            self.pipeline.clear();
        }
        changed
    }

    /// Adopt the parts of `profile` that `changed` says are stale, on
    /// `desktop` under `theme`, and nothing else.
    ///
    /// A drag delivers one of these per motion sample, so re-deriving the
    /// whole look here would make each sample cost a screenful; and what a
    /// field did not name — the phosphor trail, the effect stages — must
    /// survive it, because dropping either is visible.
    pub fn adopt(
        &mut self,
        changed: Invalidation,
        profile: &Profile,
        theme: &Theme,
        desktop: &Desktop,
    ) {
        if changed.metrics() {
            self.font_size_px = profile.font_size_px;
            (self.font, self.scale) = fit(profile.font_size_px, theme, desktop);
        }
        if changed.painted() {
            self.painted = colours(profile);
        }
        self.effects = profile.effects;
        // Only a new face or new colours forget the trail, which would ghost
        // over them; an effect turned up or down leaves what was lit still
        // true.
        if changed.metrics() || changed.painted() {
            self.pipeline.clear();
        }
    }
}

/// The face a screen asking for `font_size_px` is drawn in on `desktop` under
/// `theme` — that size, stepped down until the conventional grid fits the
/// screen — and the scale it is drawn at.
fn fit(font_size_px: u16, theme: &Theme, desktop: &Desktop) -> (BitmapFont, Scale) {
    let screen = (desktop.screen_width_px(), desktop.screen_height_px());
    let scale = desktop.scale();
    let size = fit_font_size(font_size_px, screen, theme, scale);
    (cell_font(size, theme, scale), scale)
}

/// The colours `profile` paints with.
fn colours(profile: &Profile) -> Painted {
    Painted::resolve(
        profile.scheme,
        &profile.custom,
        profile.effects.background_alpha(),
    )
}

#[cfg(test)]
#[path = "look_tests.rs"]
mod tests;
