//! A document application's settings window: the shared settings frame over
//! the closed registry the application keeps its preferences in.
//!
//! The window edits a copy of the settings in force and asks the application
//! for every change it makes — live while an interaction is under way, and
//! once settled — so the application applies it to every window and writes it
//! off the loop. What the store then says comes back through
//! [`SettingsWindow::adopt`], and what it refused through
//! [`SettingsWindow::say`].

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use tairix_abi::window_ipc::CursorShape;
use tairix_appconf::Live;
use tairix_controls::{
    Button, ColumnAction, FieldGroup, Keystroke, SettingsAction, SettingsFrame, SettingsLayout,
    Tab, SETTINGS_SIZE,
};
use tairix_geometry::{Point, Region, Scale};
use tairix_icon::IconArtwork;
use tairix_input::InputEvent;
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::{AppRequest, AppView, Outcome};

/// An application's preferences as its settings window shows them: a closed
/// registry, grouped into categories of form rows.
pub trait Preferences: Live {
    /// What the window's title reads.
    const TITLE: &'static str;
    /// What a command row asks only the application for.
    type Ask;
    /// The categories, in the sidebar's order.
    fn categories() -> Vec<Tab>;
    /// The groups category `category` shows of these settings.
    fn groups(&self, category: usize) -> Vec<FieldGroup>;
    /// Carry what a row of category `category` asked for into these settings,
    /// answering what a command row asks of the application instead.
    fn act(&mut self, category: usize, action: &ColumnAction) -> Option<Self::Ask>;
    /// Whether moving from `was` to these settings changes what another row
    /// of `category` offers — a choice that enables a field, a format that
    /// holds fewer colours — so its groups are built again.
    fn reshapes(&self, was: &Self, category: usize) -> bool;
}

/// The settings an event changed, from what to what.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edit<P> {
    /// Before.
    pub was: P,
    /// After.
    pub now: P,
    /// Whether the interaction that made the change is over, so it is
    /// written.
    pub settled: bool,
}

/// What a settings window asks beyond an edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Then<A> {
    /// Put every setting back to what the layers beneath the user's own say.
    Restore,
    /// What a command row asks only the application for.
    Ask(A),
}

/// What one event of a settings window asks the application for: the edit
/// it made, carried out first, and what it asked beyond it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsRequest<P: Preferences> {
    /// The settings the event changed, if it changed any.
    pub edit: Option<Edit<P>>,
    /// What it asked beyond them.
    pub then: Option<Then<P::Ask>>,
}

/// The settings window, its text set in faces `F`, which it does not use: the
/// frame sets its own from the theme.
#[derive(Clone, Debug)]
pub struct SettingsWindow<P, F> {
    frame: SettingsFrame,
    layout: SettingsLayout,
    record: P,
    faces: PhantomData<F>,
}

impl<P: Preferences, F> SettingsWindow<P, F> {
    /// The window showing `record`, at `category`.
    #[must_use]
    pub fn new(record: P, category: usize) -> Self {
        let categories = P::categories();
        let category = category.min(categories.len().saturating_sub(1));
        let frame = SettingsFrame::new(
            categories,
            category,
            record.groups(category),
            vec![Button::labelled("Restore defaults")],
        );
        Self {
            frame,
            layout: SettingsLayout::default(),
            record,
            faces: PhantomData,
        }
    }

    /// The category on show.
    #[must_use]
    pub fn category(&self) -> usize {
        self.frame.shown()
    }

    /// The settings the rows show.
    #[must_use]
    pub const fn record(&self) -> &P {
        &self.record
    }

    /// The frame, for a test that reads what the window shows.
    #[must_use]
    pub const fn frame(&self) -> &SettingsFrame {
        &self.frame
    }

    /// Show `record`, the settings now in force, answering whether the window
    /// is to be laid out again. A control in use is left as the user has it:
    /// its own settle writes it.
    pub fn adopt(&mut self, record: &P, damage: &mut Region) -> bool {
        if *record == self.record || self.frame.holding() {
            return false;
        }
        self.record.clone_from(record);
        let groups = self.record.groups(self.frame.shown());
        self.frame.rebuild(groups, &self.layout, damage);
        true
    }

    /// Say `message` beneath the settings, or nothing.
    pub fn say(&mut self, message: Option<String>, damage: &mut Region) {
        let _ = self.frame.say(message, &self.layout, damage);
    }

    /// Draw the window into `surface` as far as its clip admits.
    pub fn render(
        &self,
        surface: &mut Surface,
        (theme, scale): (&Theme, Scale),
        artwork: &mut dyn IconArtwork,
    ) {
        self.frame
            .render(surface, &self.layout, (scale, theme), artwork);
    }

    /// What the frame's `actions` ask: each row's carried into the record, a
    /// category shown, a command or a close asked for.
    fn answered(
        &mut self,
        actions: impl IntoIterator<Item = SettingsAction>,
        damage: &mut Region,
    ) -> Outcome<AppRequest<SettingsRequest<P>>> {
        let was = self.record.clone();
        let mut settled = false;
        let mut then = None;
        let mut relaid = false;
        for action in actions {
            match action {
                SettingsAction::Shown { category } => {
                    let groups = self.record.groups(category);
                    self.frame.show(category, groups, &self.layout, damage);
                    relaid = true;
                }
                SettingsAction::Field(acted) => {
                    settled |= acted.action.action.settles();
                    let before = self.record.clone();
                    let category = self.frame.shown();
                    if let Some(ask) = self.record.act(category, &acted) {
                        then = Some(Then::Ask(ask));
                    }
                    tairix_appconf::Registry::normalise(&mut self.record);
                    if self.record.reshapes(&before, category) {
                        let groups = self.record.groups(category);
                        self.frame.rebuild(groups, &self.layout, damage);
                        relaid = true;
                    }
                }
                SettingsAction::Command { .. } => then = Some(Then::Restore),
                SettingsAction::Close => return Outcome::asking(AppRequest::Close),
            }
        }
        let edit = (self.record != was || settled).then(|| Edit {
            was,
            now: self.record.clone(),
            settled,
        });
        let mut outcome = if edit.is_none() && then.is_none() {
            Outcome::none()
        } else {
            Outcome::asking(AppRequest::Own(SettingsRequest { edit, then }))
        };
        if relaid {
            outcome.relayout = super::Relayout::Whole;
        }
        outcome
    }
}

impl<P: Preferences, F: Copy> AppView for SettingsWindow<P, F> {
    type Faces = F;
    type Own = SettingsRequest<P>;

    fn title(&self) -> &str {
        P::TITLE
    }

    fn size(&self) -> (u32, u32) {
        SETTINGS_SIZE
    }

    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        _: F,
        damage: &mut Region,
    ) {
        self.layout = self.frame.layout(width, height, scale, theme);
        self.frame.settle(&self.layout, damage);
    }

    fn min_size(&self, theme: &Theme, scale: Scale, _: F) -> (u32, u32) {
        self.frame.min_size(scale, theme)
    }

    fn cursor(&self, _: Point) -> CursorShape {
        CursorShape::Arrow
    }

    /// The keyboard stays where it was, for the window's return.
    fn focus_changed(&mut self, _: bool, _: &mut Region) {}

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome<AppRequest<SettingsRequest<P>>> {
        let layout = &self.layout;
        let actions = match *input {
            InputEvent::KeyPressed { key, modifiers } => {
                let stroke = Keystroke {
                    key,
                    modifiers,
                    at_ns: now_ns,
                };
                self.frame.on_key(stroke, layout, scale, theme, damage)
            }
            InputEvent::KeyReleased { .. } => return Outcome::none(),
            _ => self.frame.on_pointer(input, layout, scale, theme, damage),
        };
        // Most events — a hover, a motion — ask nothing, and cost no copy of
        // the record.
        if actions.is_empty() {
            return Outcome::none();
        }
        self.answered(actions, damage)
    }

    fn deadline_ns(&mut self, now_ns: u64) -> Option<u64> {
        self.frame.deadline_ns(now_ns)
    }

    fn advance(&mut self, now_ns: u64, scale: Scale, theme: &Theme, damage: &mut Region) {
        self.frame
            .advance(now_ns, &self.layout, scale, theme, damage);
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
