//! What a Paint window shows: a picture's editor, or a sprite file's sprites
//! (`plans/PAINT.md` PT32), each laid out, fed and painted by its own engine.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, SaveFormats};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;
use tairix_window::docapp::{
    DialogWindow, DocumentView, Outcome as HostOutcome, SaveTarget, ToolGone, ToolMove,
    ToolOpening, ToolWindow,
};
use tairix_window::document::{Access, SavedDocument};

use crate::area::AreaView;
use crate::document::Snapshot;
use crate::layout::Faces;
use crate::view::{Computed, MenuKind, Outcome, Own, View};

/// The engine a window shows.
#[derive(Debug)]
pub enum Engine {
    /// A picture: one, a TIFF's pages, or one sprite of a sprite file.
    Picture(Box<View>),
    /// A sprite file's sprites.
    Area(Box<AreaView>),
}

impl Engine {
    /// The picture's editor, where the window shows one.
    #[must_use]
    pub fn picture(&self) -> Option<&View> {
        match self {
            Self::Picture(view) => Some(view),
            Self::Area(_) => None,
        }
    }

    /// The picture's editor, where the window shows one.
    pub fn picture_mut(&mut self) -> Option<&mut View> {
        match self {
            Self::Picture(view) => Some(view),
            Self::Area(_) => None,
        }
    }

    /// The sprite file's engine, where the window shows one.
    #[must_use]
    pub fn area(&self) -> Option<&AreaView> {
        match self {
            Self::Area(view) => Some(view),
            Self::Picture(_) => None,
        }
    }

    /// The sprite file's engine, where the window shows one.
    pub fn area_mut(&mut self) -> Option<&mut AreaView> {
        match self {
            Self::Area(view) => Some(view),
            Self::Picture(_) => None,
        }
    }

    /// What a worker answered job `job` with, landed on what asked for it.
    pub fn computed(&mut self, job: u64, computed: Computed, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => view.computed(job, computed, damage),
            Self::Area(view) => view.computed(job, computed, damage),
        }
    }

    /// Release the history, and a picture's preview, to memory pressure
    /// `band`, reporting what that repaints.
    pub fn adopt_pressure(&mut self, band: tairix_reclaim::PressureBand, damage: &mut Region) {
        match self {
            Self::Picture(view) => view.adopt_pressure(band, damage),
            Self::Area(view) => view.adopt_pressure(band),
        }
    }
}

impl SavedDocument for Engine {
    type Snapshot = Snapshot;

    fn access(&self) -> Access {
        match self {
            Self::Picture(view) => view.access(),
            Self::Area(view) => view.access(),
        }
    }

    fn is_modified(&self) -> bool {
        match self {
            Self::Picture(view) => view.is_modified(),
            Self::Area(view) => view.is_modified(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Picture(view) => view.is_empty(),
            Self::Area(view) => view.is_empty(),
        }
    }

    fn snapshot(&mut self) -> Option<(u64, Arc<Snapshot>)> {
        match self {
            Self::Picture(view) => view.snapshot(),
            Self::Area(view) => view.snapshot(),
        }
    }

    fn saved(&mut self, generation: u64) {
        match self {
            Self::Picture(view) => view.saved(generation),
            Self::Area(view) => view.saved(generation),
        }
    }

    fn rename(&mut self, name: String) {
        match self {
            Self::Picture(view) => SavedDocument::rename(&mut **view, name),
            Self::Area(view) => SavedDocument::rename(&mut **view, name),
        }
    }

    fn set_access(&mut self, access: Access) {
        match self {
            Self::Picture(view) => view.set_access(access),
            Self::Area(view) => view.set_access(access),
        }
    }

    fn say(&mut self, message: String) {
        match self {
            Self::Picture(view) => SavedDocument::say(&mut **view, message),
            Self::Area(view) => SavedDocument::say(&mut **view, message),
        }
    }
}

impl DocumentView for Engine {
    type Faces = Faces;
    type MenuKind = MenuKind;
    type Own = Own;

    fn name(&self) -> &str {
        match self {
            Self::Picture(view) => DocumentView::name(&**view),
            Self::Area(view) => view.name(),
        }
    }

    fn write_title(&self, title: &mut String) {
        match self {
            Self::Picture(view) => DocumentView::write_title(&**view, title),
            Self::Area(view) => view.write_title(title),
        }
    }

    fn lay_out(
        &mut self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
        damage: &mut Region,
    ) {
        match self {
            Self::Picture(view) => view.lay_out(width, height, theme, scale, faces, damage),
            Self::Area(view) => view.lay_out(width, height, theme, scale, damage),
        }
    }

    fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        match self {
            Self::Picture(view) => DocumentView::min_size(&**view, theme, scale, faces),
            Self::Area(_) => AreaView::min_size(theme, scale),
        }
    }

    fn open_size(&self, theme: &Theme, _faces: Faces) -> Option<(u32, u32)> {
        match self {
            Self::Area(view) => Some(AreaView::open_size(view.document().entries().len(), theme)),
            Self::Picture(_) => None,
        }
    }

    fn message_area(&self) -> Rect {
        match self {
            Self::Picture(view) => view.message_area(),
            Self::Area(view) => view.layout().status(),
        }
    }

    fn asking_to_close(&self) -> bool {
        match self {
            Self::Picture(view) => view.asking_to_close(),
            Self::Area(_) => false,
        }
    }

    fn cursor(&self, at: Point) -> CursorShape {
        match self {
            Self::Picture(view) => DocumentView::cursor(&**view, at),
            Self::Area(view) => view.cursor_shape(at),
        }
    }

    fn tool_tip(&self, scale: Scale, theme: &Theme) -> Option<(Rect, &str)> {
        match self {
            Self::Picture(view) => DocumentView::tool_tip(&**view, scale, theme),
            Self::Area(_) => None,
        }
    }

    fn menu(&self, kind: MenuKind) -> AppMenu {
        match self {
            Self::Picture(view) => DocumentView::menu(&**view, kind),
            Self::Area(view) => view.menu(kind),
        }
    }

    fn close_requested(&mut self, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::close_requested(&mut **view, damage),
            Self::Area(view) => view.close_requested(damage),
        }
    }

    fn save_then_close(&mut self, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::save_then_close(&mut **view, damage),
            Self::Area(_) => HostOutcome::asking(tairix_window::docapp::Request::SaveThenClose),
        }
    }

    fn chosen(&mut self, item: AppMenuItemId, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::chosen(&mut **view, item, damage),
            Self::Area(view) => view.chosen_row(item, damage),
        }
    }

    fn entered(&mut self, item: AppMenuItemId, text: &str, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::entered(&mut **view, item, text, damage),
            Self::Area(view) => view.entered(item, text, damage),
        }
    }

    fn focus_changed(&mut self, focused: bool, damage: &mut Region) {
        match self {
            Self::Picture(view) => DocumentView::focus_changed(&mut **view, focused, damage),
            Self::Area(view) => view.focus_changed(focused, damage),
        }
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match self {
            Self::Picture(view) => {
                DocumentView::input(&mut **view, input, now_ns, scale, theme, damage)
            }
            Self::Area(view) => view.input(input, now_ns, scale, theme, damage),
        }
    }

    fn refuse_save(&self, name: &str, target: SaveTarget) -> Option<String> {
        match self {
            Self::Picture(view) => DocumentView::refuse_save(&**view, name, target),
            Self::Area(view) => view.refuse_save(name, target),
        }
    }

    fn ask_how(&mut self, then_close: bool, damage: &mut Region) -> Option<Outcome> {
        match self {
            Self::Picture(view) => DocumentView::ask_how(&mut **view, then_close, damage),
            Self::Area(_) => None,
        }
    }

    fn offered_extension(&self) -> &'static str {
        match self {
            Self::Picture(view) => DocumentView::offered_extension(&**view),
            Self::Area(view) => view.offered_extension(),
        }
    }

    fn save_formats(&self) -> Result<SaveFormats, String> {
        match self {
            Self::Picture(view) => DocumentView::save_formats(&**view),
            Self::Area(view) => view.save_formats(),
        }
    }

    fn tool_window(&self, index: usize) -> Option<ToolWindow<'_>> {
        match self {
            Self::Picture(view) => DocumentView::tool_window(&**view, index),
            Self::Area(_) => None,
        }
    }

    fn tool_opening(&mut self, id: u32) -> ToolOpening {
        match self {
            Self::Picture(view) => DocumentView::tool_opening(&mut **view, id),
            Self::Area(_) => ToolOpening::default(),
        }
    }

    fn tool_moved(
        &mut self,
        moved: ToolMove,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match self {
            Self::Picture(view) => {
                DocumentView::tool_moved(&mut **view, moved, scale, theme, damage)
            }
            Self::Area(_) => HostOutcome::none(),
        }
    }

    fn tool_resized(
        &mut self,
        id: u32,
        size: (u32, u32),
        scale: Scale,
        damage: &mut Region,
    ) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::tool_resized(&mut **view, id, size, scale, damage),
            Self::Area(_) => HostOutcome::none(),
        }
    }

    fn tool_gone(&mut self, id: u32, why: ToolGone, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::tool_gone(&mut **view, id, why, damage),
            Self::Area(_) => HostOutcome::none(),
        }
    }

    fn dialog_window(&self, scale: Scale, theme: &Theme) -> Option<DialogWindow<'_>> {
        match self {
            Self::Picture(view) => DocumentView::dialog_window(&**view, scale, theme),
            Self::Area(view) => view.dialog_window(scale, theme),
        }
    }

    fn dialog_input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match self {
            Self::Picture(view) => {
                DocumentView::dialog_input(&mut **view, input, now_ns, scale, theme, damage)
            }
            Self::Area(view) => view.dialog_input(input, now_ns, (scale, theme), damage),
        }
    }

    fn dialog_focus_changed(&mut self, focused: bool, damage: &mut Region) {
        if let Self::Picture(view) = self {
            DocumentView::dialog_focus_changed(&mut **view, focused, damage);
        }
    }

    fn dialog_refused(&mut self, why: &str, damage: &mut Region) -> Outcome {
        match self {
            Self::Picture(view) => DocumentView::dialog_refused(&mut **view, why, damage),
            Self::Area(view) => view.dialog_refused(why, damage),
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
