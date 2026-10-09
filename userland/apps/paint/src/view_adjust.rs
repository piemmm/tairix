//! The open adjustment: opening it in the Adjustment pane, what the picture
//! shows of it and the histogram it reads — each worked out on a worker one
//! job at a time and landed only on the state it was asked of — applying it,
//! and the input the pane takes.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_colour::Rgb;
use tairix_controls::Keystroke;
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, PointerButton};
use tairix_theme::Theme;
use tairix_window::docapp::Relayout;

use super::input::Keyboard;
use super::{Action, Compute, Computed, Lands, Outcome, Own, Request, Settles, Then, View};
use crate::adjust::{AdjustOutcome, AdjustPane};
use crate::canvas::{Canvas, Kind, Tile};
use crate::filter::Filter;
use crate::histogram::Histogram;
use crate::layout::{Faces, Layout};
use crate::pane::PaneKind;
use crate::shape::Point as Fx;

/// The state a worker's answer for the adjustment is written over: the entry
/// showing, the layer painted on, the document's generation and the
/// selection's.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Basis {
    entry: usize,
    layer: usize,
    generation: u64,
    selection: u64,
}

/// What the adjustment's one job out is working out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Looking {
    /// The picture as these settings leave it.
    Preview(Filter),
    /// The histogram.
    Histogram,
}

/// The adjustment's job out.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct LookJob {
    job: u64,
    looking: Looking,
    basis: Basis,
}

/// A colour picture as the settings leave it, and what it was worked from.
#[derive(Debug)]
pub(crate) struct Shown {
    canvas: Canvas,
    tiles: Vec<(usize, Arc<Tile>)>,
    filter: Filter,
    basis: Basis,
}

/// What the picture shows of the open adjustment, and the histogram it reads.
#[derive(Debug, Default)]
pub(crate) struct Looks {
    job: Option<LookJob>,
    /// Whether the settings or the picture moved while the job was out, so it
    /// is asked again once it lands.
    stale: bool,
    shown: Option<Shown>,
    /// A palette picture's kind with its palette adjusted.
    kind: Option<(Kind, Filter, Basis)>,
    histogram: Option<(Histogram, Basis)>,
}

impl View {
    /// The state an adjustment's answer is written over.
    pub(super) fn basis(&self) -> Basis {
        Basis {
            entry: self.document.current(),
            layer: self.active_layer(),
            generation: self.document.generation(),
            selection: self.selection_epoch,
        }
    }

    /// The Adjustment pane, for the painter.
    #[must_use]
    pub(crate) const fn adjustment_pane(&self) -> &AdjustPane {
        &self.adjustment
    }

    /// The open adjustment's settings, if one is open.
    #[must_use]
    pub fn adjusting(&self) -> Option<Filter> {
        self.adjustment.filter()
    }

    /// The histogram the pane draws: the last worked out, which may lag the
    /// picture by the one being worked out now.
    #[must_use]
    pub(crate) fn histogram(&self) -> Option<&Histogram> {
        self.looks
            .histogram
            .as_ref()
            .map(|(histogram, _)| histogram)
    }

    /// The picture as the open adjustment leaves it, while it shows one worked
    /// from the picture as it stands.
    #[must_use]
    pub(crate) fn preview_canvas(&self) -> Option<&Canvas> {
        let basis = self.basis();
        self.looks
            .shown
            .as_ref()
            .filter(|shown| shown.basis == basis && self.shows_adjustment())
            .map(|shown| &shown.canvas)
    }

    /// A palette picture's kind as the open adjustment leaves it.
    #[must_use]
    pub(crate) fn preview_kind(&self) -> Option<&Kind> {
        let basis = self.basis();
        self.looks
            .kind
            .as_ref()
            .filter(|(_, _, at)| *at == basis && self.shows_adjustment())
            .map(|(kind, _, _)| kind)
    }

    /// Whether the picture shows the open adjustment: one is open, Preview
    /// is on, and it changes something.
    fn shows_adjustment(&self) -> bool {
        self.adjustment.previewing()
            && self
                .adjustment
                .filter()
                .is_some_and(|filter| !filter.is_identity())
    }

    /// Open `filter` in the Adjustment pane, or apply at once one with
    /// nothing to set; the same adjustment, already open, is shown where it
    /// is.
    pub(super) fn adjust(
        &mut self,
        filter: Filter,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if self
            .adjustment
            .filter()
            .is_some_and(|open| open.same_kind(&filter))
        {
            self.panes.show(PaneKind::Adjustment);
            return Outcome::relaid();
        }
        if !self.editable(layout, damage) {
            return Outcome::none();
        }
        if self.kind().palette().is_some() && filter.neighbourly() {
            let refusal = alloc::format!(
                "{} needs a colour picture: convert it to millions of colours first",
                filter.label()
            );
            self.state(&refusal, layout, damage);
            return Outcome::none();
        }
        if !filter.has_settings() {
            return self.apply_once(filter, layout, damage);
        }
        self.adjustment = AdjustPane::setting(filter);
        self.looks = Looks::default();
        self.retitle_adjustment();
        self.panes.show(PaneKind::Adjustment);
        let mut outcome = self.refresh_adjustment(layout, damage);
        outcome.relayout = Relayout::Whole;
        outcome
    }

    /// Close the open adjustment without applying it: the pane goes back to
    /// its list and the picture shows itself. The pane's height changes, so
    /// the window is laid out again.
    pub(super) fn close_adjustment(&mut self, layout: &Layout, damage: &mut Region) {
        if self.adjustment.filter().is_none() {
            return;
        }
        self.adjustment = AdjustPane::choosing();
        self.looks = Looks::default();
        self.retitle_adjustment();
        damage.add(layout.canvas());
    }

    /// Name the open adjustment on its pane's band.
    fn retitle_adjustment(&mut self) {
        let title = self.adjustment.title();
        self.headers[PaneKind::Adjustment.index()].set_title(title);
    }

    /// Withhold the pane while a worker has the picture or there is no
    /// picture to adjust.
    pub(super) fn sync_adjustment(&mut self) {
        let withheld = self.pending.is_some() || self.document.picture().is_none();
        self.adjustment.set_withheld(withheld);
    }

    /// Ask a worker for what the open adjustment needs next: the picture as
    /// its settings leave it, then the histogram it reads — or, with
    /// `histogram_first`, the other way about. A palette picture's preview is
    /// worked out here. Nothing is asked while a worker has the picture or
    /// the adjustment's own job is out; it is asked once that lands.
    pub(super) fn refresh_adjustment(&mut self, layout: &Layout, damage: &mut Region) -> Outcome {
        self.refresh_looks(false, layout, damage)
    }

    fn refresh_looks(
        &mut self,
        histogram_first: bool,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        self.sync_adjustment();
        let Some(filter) = self.adjustment.filter() else {
            return Outcome::none();
        };
        let basis = self.basis();
        let fresh_histogram = self
            .looks
            .histogram
            .as_ref()
            .is_some_and(|(_, at)| *at == basis);
        if self.adjustment.set_histogram_ready(fresh_histogram) {
            damage.add(layout.adjustment());
        }
        if self.pending.is_some() || self.looks.job.is_some() {
            self.looks.stale = true;
            return Outcome::none();
        }
        self.looks.stale = false;
        let wants_preview = self.shows_adjustment();
        if let Some(palette) = self.kind().palette() {
            let kind = self.kind().clone();
            let mapped = wants_preview
                .then(|| filter.mapped_palette(palette))
                .flatten()
                .and_then(|palette| match kind {
                    Kind::Indexed { depth, masked, .. } => Some(Kind::Indexed {
                        depth,
                        palette,
                        masked,
                    }),
                    Kind::Rgba => None,
                });
            self.looks.kind = mapped.map(|kind| (kind, filter, basis));
            damage.add(layout.canvas());
        } else if !wants_preview {
            // Kept, so turning Preview back on to compare shows it at once.
            damage.add(layout.canvas());
        } else if self
            .looks
            .shown
            .as_ref()
            .is_none_or(|shown| (shown.filter, shown.basis) != (filter, basis))
            && !(histogram_first && self.wants_histogram(basis))
        {
            return self.look(Looking::Preview(filter), basis, layout, damage);
        }
        if self.wants_histogram(basis) {
            return self.look(Looking::Histogram, basis, layout, damage);
        }
        if histogram_first {
            return self.refresh_looks(false, layout, damage);
        }
        Outcome::none()
    }

    /// Whether the pane reads a histogram, and the one held is not of the
    /// picture as it stands.
    fn wants_histogram(&self, basis: Basis) -> bool {
        self.adjustment.reads_histogram()
            && self
                .looks
                .histogram
                .as_ref()
                .is_none_or(|(_, at)| *at != basis)
    }

    /// Ask a worker to work out `looking` from the picture as it stands.
    fn look(
        &mut self,
        looking: Looking,
        basis: Basis,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        // Copied only once a worker is to be asked: a slider dragged while one
        // works would otherwise copy the picture's tile table each step.
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state("There is not enough memory to preview that", layout, damage);
            return Outcome::none();
        };
        let job = self.next_job;
        self.next_job += 1;
        let clip = self.selection.clone();
        let work = match looking {
            Looking::Preview(filter) => Compute::Filter {
                canvas,
                filter,
                clip,
            },
            Looking::Histogram => Compute::Histogram { canvas, clip },
        };
        self.looks.job = Some(LookJob {
            job,
            looking,
            basis,
        });
        Outcome::asking(Request::Own(Own::Compute { job, work }))
    }

    /// Whether job `job` is the adjustment's.
    pub(super) fn looking(&self, job: u64) -> bool {
        self.looks.job.is_some_and(|out| out.job == job)
    }

    /// The adjustment's job answered: land it on the state it was asked of,
    /// or drop it where that has moved, and ask for what is needed next.
    pub(super) fn looked(
        &mut self,
        answer: Computed,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let Some(out) = self.looks.job.take() else {
            return Outcome::none();
        };
        let current = self.basis() == out.basis && self.adjustment.filter().is_some();
        let landed = match (out.looking, answer) {
            (Looking::Preview(filter), Computed::Filtered(Ok(tiles))) => {
                if current {
                    self.show_preview(filter, tiles, out.basis, layout, damage);
                }
                true
            }
            (Looking::Histogram, Computed::Histogram(Ok(histogram))) => {
                if current {
                    self.looks.histogram = Some((histogram, out.basis));
                    damage.add(layout.adjustment());
                }
                true
            }
            _ => false,
        };
        // A job refused or failed is asked again by the next input, never
        // straight away: the queue that refused it would refuse it again.
        if !landed && current {
            self.looks.stale = true;
            self.state("There is not enough memory to preview that", layout, damage);
            return Outcome::none();
        }
        let histogram_first = matches!(out.looking, Looking::Preview(_));
        self.refresh_looks(histogram_first, layout, damage)
    }

    /// Show `tiles`, worked out for `filter` from the picture as it stood at
    /// `basis`, over the picture.
    fn show_preview(
        &mut self,
        filter: Filter,
        tiles: Vec<(usize, Arc<Tile>)>,
        basis: Basis,
        layout: &Layout,
        damage: &mut Region,
    ) {
        let shown = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone());
        let Some(Ok(mut canvas)) = shown else {
            self.state("There is not enough memory to preview that", layout, damage);
            return;
        };
        for (index, tile) in &tiles {
            canvas.replace_tile(*index, Arc::clone(tile));
        }
        self.looks.shown = Some(Shown {
            canvas,
            tiles,
            filter,
            basis,
        });
        damage.add(layout.canvas());
    }

    /// When the picture moved under an open adjustment, ask again for what it
    /// shows and reads, with `outcome` carried out first.
    pub(super) fn follow_picture(
        &mut self,
        before: Basis,
        outcome: Outcome,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        if self.adjustment.filter().is_none() || (self.basis() == before && !self.looks.stale) {
            return outcome;
        }
        if outcome.request.is_some() {
            self.looks.stale = true;
            return outcome;
        }
        let mut refreshed = self.refresh_adjustment(layout, damage);
        refreshed.relayout = stronger(refreshed.relayout, outcome.relayout);
        refreshed
    }

    /// Apply the open adjustment, then carry out `then`: now where nothing
    /// is open or it applies at once — nothing to apply, a palette's, or a
    /// colour picture's from the preview it shows exactly — and otherwise
    /// once a worker has applied it.
    pub(super) fn apply_adjustment(
        &mut self,
        then: Then,
        layout: &Layout,
        damage: &mut Region,
    ) -> Applying {
        let Some(filter) = self.adjustment.filter() else {
            return Applying::Now(then);
        };
        if !self.idle(layout, damage) {
            return Applying::Waiting(Outcome::none());
        }
        let applied = if filter.is_identity() {
            true
        } else if let Some(palette) = self.kind().palette() {
            if let Some(mapped) = filter.mapped_palette(palette) {
                self.change_palette(mapped, layout, damage);
            }
            true
        } else {
            self.adopt_preview(filter)
        };
        if applied {
            self.close_adjustment(layout, damage);
            return Applying::Now(then);
        }
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state(
                "There is not enough memory to adjust the picture",
                layout,
                damage,
            );
            return Applying::Waiting(Outcome::none());
        };
        let work = Compute::Filter {
            canvas,
            filter,
            clip: self.selection.clone(),
        };
        Applying::Waiting(self.begin_work(
            work,
            Lands::Tiles(Settles::Adjusted(then)),
            "adjust the picture",
            layout,
            damage,
        ))
    }

    /// Land the preview shown as the change, where it shows exactly `filter`
    /// over the picture as it stands.
    fn adopt_preview(&mut self, filter: Filter) -> bool {
        let basis = self.basis();
        let Some(shown) = self.looks.shown.take() else {
            return false;
        };
        if (shown.filter, shown.basis) != (filter, basis) {
            self.looks.shown = Some(shown);
            return false;
        }
        // Tiles that no longer fit, or no room to keep them, leave it to a
        // worker.
        matches!(
            self.document.adopt_tiles(basis.layer, shown.tiles),
            Ok(true)
        )
    }

    /// Apply an adjustment with nothing to set, at once.
    fn apply_once(&mut self, filter: Filter, layout: &Layout, damage: &mut Region) -> Outcome {
        damage.add(layout.canvas());
        if let Some(palette) = self.kind().palette() {
            if let Some(mapped) = filter.mapped_palette(palette) {
                self.change_palette(mapped, layout, damage);
            }
            return Outcome::none();
        }
        let Some(Ok(canvas)) = self
            .document
            .picture()
            .map(|picture| picture.canvas().try_clone())
        else {
            self.state(
                "There is not enough memory to filter the picture",
                layout,
                damage,
            );
            return Outcome::none();
        };
        let work = Compute::Filter {
            canvas,
            filter,
            clip: self.selection.clone(),
        };
        self.begin_work(
            work,
            Lands::Tiles(Settles::Nothing),
            "filter the picture",
            layout,
            damage,
        )
    }

    /// Whether `action` leaves the open adjustment open: the view, the inks,
    /// the panes, choosing a tool, undo and redo, the selection and the
    /// document's name — and choosing the adjustment that is open. Anything
    /// else that changes or reads the picture applies it first.
    pub(super) fn leaves_adjustment(&self, action: Action) -> bool {
        match action {
            Action::ZoomIn
            | Action::ZoomOut
            | Action::Fit
            | Action::Actual
            | Action::PixelGrid
            | Action::Grid
            | Action::Zoom(_)
            | Action::EditPrimary
            | Action::EditSecondary
            | Action::SwapColours
            | Action::Pane(_)
            | Action::ResetPanes
            | Action::Tool(_)
            | Action::Undo
            | Action::Redo
            | Action::SelectAll
            | Action::Deselect
            | Action::Rename => true,
            Action::PutDown | Action::Delete => self.draft.is_some(),
            Action::Adjust(index) => Filter::ALL
                .get(index)
                .zip(self.adjustment.filter())
                .is_some_and(|(chosen, open)| chosen.same_kind(&open)),
            _ => false,
        }
    }

    /// The pointer on the Adjustment pane: `None` where the event is none of
    /// its.
    pub(super) fn adjustment_pointer(
        &mut self,
        event: &InputEvent,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let bounds = layout.adjustment_settings();
        if bounds.is_empty() && !self.adjustment.holding() {
            return None;
        }
        let faces = Faces::of(theme, scale);
        let answer = self.adjustment.on_pointer(
            event,
            (bounds, layout.pane_window(PaneKind::Adjustment)),
            (faces, scale, theme),
            damage,
        );
        let pressed = matches!(
            event,
            InputEvent::PointerPressed {
                button: PointerButton::Primary
            }
        );
        match answer {
            AdjustOutcome::Ignored => {
                (pressed && layout.adjustment().contains(self.pointer)).then(Outcome::none)
            }
            answer => Some(self.adjusted(answer, layout, damage)),
        }
    }

    /// A key while the Adjustment pane has the keyboard: `None` where it is
    /// none of the pane's.
    pub(super) fn adjustment_key(
        &mut self,
        stroke: Keystroke,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<Outcome> {
        let faces = Faces::of(theme, scale);
        let answer = self.adjustment.on_key(
            stroke,
            (
                layout.adjustment_settings(),
                layout.pane_window(PaneKind::Adjustment),
            ),
            (faces, scale, theme),
            damage,
        );
        match answer {
            AdjustOutcome::Ignored => None,
            AdjustOutcome::Left { forward } => {
                self.walk_keyboard(Keyboard::Adjustment, forward, layout, scale, theme, damage);
                Some(Outcome::none())
            }
            answer => Some(self.adjusted(answer, layout, damage)),
        }
    }

    /// Settle the pane's typing and take the keyboard from it, as a press
    /// elsewhere or a menu does; a value it settles is shown once what the
    /// event came to is carried out.
    pub(super) fn release_adjustment(
        &mut self,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let faces = Faces::of(theme, scale);
        let answer =
            self.adjustment
                .blur(layout.adjustment_settings(), (faces, scale, theme), damage);
        if matches!(answer, AdjustOutcome::Changed { .. }) {
            self.looks.stale = true;
        }
    }

    /// Give the pane the keyboard at its first stop, or its last.
    pub(super) fn enter_adjustment(
        &mut self,
        forward: bool,
        bounds: tairix_geometry::Rect,
        damage: &mut Region,
    ) -> bool {
        self.adjustment.enter_focus(forward, bounds, damage)
    }

    /// Put an eyedropper that is up down, answering whether one was.
    pub(super) fn put_down_pick(&mut self, layout: &Layout, damage: &mut Region) -> bool {
        if !self.adjustment.put_down_pick() {
            return false;
        }
        damage.add(layout.adjustment());
        true
    }

    /// What the pane's answer comes to for the window.
    fn adjusted(&mut self, answer: AdjustOutcome, layout: &Layout, damage: &mut Region) -> Outcome {
        match answer {
            AdjustOutcome::Ignored | AdjustOutcome::Taken | AdjustOutcome::Left { .. } => {
                Outcome::none()
            }
            AdjustOutcome::Changed { .. } | AdjustOutcome::Previewed => {
                self.end_gesture(layout, damage);
                self.refresh_adjustment(layout, damage)
            }
            AdjustOutcome::Picking => {
                damage.add(layout.canvas());
                Outcome::none()
            }
            AdjustOutcome::Auto => {
                let Some((histogram, at)) = &self.looks.histogram else {
                    return Outcome::none();
                };
                if *at != self.basis() {
                    return Outcome::none();
                }
                let histogram = histogram.clone();
                if self.adjustment.auto(&histogram) {
                    damage.add(layout.adjustment());
                    return self.refresh_adjustment(layout, damage);
                }
                Outcome::none()
            }
            AdjustOutcome::Apply => match self.apply_adjustment(Then::Rest, layout, damage) {
                Applying::Now(_) => Outcome::relaid(),
                Applying::Waiting(outcome) => outcome,
            },
            AdjustOutcome::Open(filter) => self.act_on_filter(filter, layout, damage),
        }
    }

    /// The list chose `filter`: open it as the menu would.
    fn act_on_filter(&mut self, filter: Filter, layout: &Layout, damage: &mut Region) -> Outcome {
        match Filter::ALL
            .iter()
            .position(|entry| entry.same_kind(&filter))
        {
            Some(index) => self.act(Action::Adjust(index), layout, damage),
            None => Outcome::none(),
        }
    }

    /// A press on the picture while an eyedropper is up: take the colour of
    /// the layer painted on there, as it is beneath the preview.
    pub(super) fn pick_for_adjustment(
        &mut self,
        at: Fx,
        layout: &Layout,
        damage: &mut Region,
    ) -> Outcome {
        let (x, y) = at.pixel();
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return Outcome::none();
        };
        let Some(colour) = self.document.picture().and_then(|picture| {
            picture
                .canvas()
                .sample(x, y)
                .map(|sample| picture.kind().colour(sample))
        }) else {
            return Outcome::none();
        };
        let [red, green, blue, _] = colour;
        damage.add(layout.adjustment());
        if self.adjustment.picked(Rgb::new(red, green, blue)) {
            return self.refresh_adjustment(layout, damage);
        }
        Outcome::none()
    }
}

/// What applying an open adjustment came to.
#[allow(
    clippy::large_enum_variant,
    reason = "handed straight back to the caller and never stored; boxing would allocate on every apply"
)]
pub(super) enum Applying {
    /// Applied now, or nothing was open: what follows is carried out now.
    Now(Then),
    /// A worker applies it and what follows waits for it to land, or it
    /// could not be applied; this is what the window does meanwhile.
    Waiting(Outcome),
}

/// The more of two relayouts.
pub(super) fn stronger(one: Relayout, other: Relayout) -> Relayout {
    let weight = |relayout: Relayout| match relayout {
        Relayout::None => 0,
        Relayout::Reported => 1,
        Relayout::Whole => 2,
    };
    if weight(other) > weight(one) {
        other
    } else {
        one
    }
}
