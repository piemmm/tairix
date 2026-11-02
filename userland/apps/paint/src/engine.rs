//! What a Paint window shows: a picture's editor, or a sprite file's sprites
//! (`plans/PAINT.md` PT32), each laid out, fed and painted by its own engine.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, SaveEndings};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;
use tairix_window::docapp::{
    DialogWindow, DocumentView, Outcome as HostOutcome, ToolGone, ToolMove, ToolOpening, ToolWindow,
};
use tairix_window::document::{Access, SavedDocument};

use crate::area::{AreaLayout, AreaView};
use crate::document::Snapshot;
use crate::layout::{Faces, Layout};
use crate::view::{Computed, MenuKind, Outcome, Own, View};

/// The engine a window shows.
#[derive(Debug)]
pub enum Engine {
    /// A picture: one, a TIFF's pages, or one sprite of a sprite file.
    Picture(Box<View>),
    /// A sprite file's sprites.
    Area(Box<AreaView>),
}

/// Where a window's engine draws.
#[derive(Clone, Debug)]
pub enum EngineLayout {
    /// A picture's.
    Picture(Box<Layout>),
    /// A sprite file's.
    Area(AreaLayout),
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
    pub fn computed(
        &mut self,
        job: u64,
        computed: Computed,
        layout: &EngineLayout,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                view.computed(job, computed, layout, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.computed(job, computed, layout, damage)
            }
            _ => HostOutcome::none(),
        }
    }

    /// Release the history to memory pressure `band`.
    pub fn adopt_pressure(&mut self, band: tairix_reclaim::PressureBand) {
        match self {
            Self::Picture(view) => view.adopt_pressure(band),
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
    type Layout = EngineLayout;
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

    fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        faces: Faces,
    ) -> EngineLayout {
        match self {
            Self::Picture(view) => EngineLayout::Picture(Box::new(DocumentView::layout(
                &**view, width, height, theme, scale, faces,
            ))),
            Self::Area(view) => EngineLayout::Area(view.layout(width, height, theme, scale, faces)),
        }
    }

    fn min_size(&self, theme: &Theme, scale: Scale, faces: Faces) -> (u32, u32) {
        match self {
            Self::Picture(view) => DocumentView::min_size(&**view, theme, scale, faces),
            Self::Area(_) => AreaView::min_size(theme, scale, faces),
        }
    }

    fn open_size(&self, theme: &Theme, faces: Faces) -> Option<(u32, u32)> {
        match self {
            Self::Area(view) => Some(AreaView::open_size(
                view.document().entries().len(),
                theme,
                faces,
            )),
            Self::Picture(_) => None,
        }
    }

    fn settle(&mut self, layout: &EngineLayout, damage: &mut Region) {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::settle(&mut **view, layout, damage);
            }
            (Self::Area(view), EngineLayout::Area(layout)) => view.settle(layout, damage),
            _ => {}
        }
    }

    fn message_area(layout: &EngineLayout) -> Rect {
        match layout {
            EngineLayout::Picture(layout) => <View as DocumentView>::message_area(layout),
            EngineLayout::Area(layout) => layout.status(),
        }
    }

    fn asking_to_close(&self) -> bool {
        match self {
            Self::Picture(view) => view.asking_to_close(),
            Self::Area(_) => false,
        }
    }

    fn cursor(&self, layout: &EngineLayout, at: Point) -> CursorShape {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::cursor(&**view, layout, at)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => view.cursor_shape(layout, at),
            _ => CursorShape::Arrow,
        }
    }

    fn tool_tip(&self, layout: &EngineLayout, scale: Scale, theme: &Theme) -> Option<(Rect, &str)> {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::tool_tip(&**view, layout, scale, theme)
            }
            _ => None,
        }
    }

    fn menu(&self, kind: MenuKind) -> AppMenu {
        match self {
            Self::Picture(view) => DocumentView::menu(&**view, kind),
            Self::Area(view) => view.menu(kind),
        }
    }

    fn close_requested(&mut self, layout: &EngineLayout, damage: &mut Region) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::close_requested(&mut **view, layout, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => view.close_requested(layout, damage),
            _ => HostOutcome::none(),
        }
    }

    fn save_then_close(&mut self, layout: &EngineLayout, damage: &mut Region) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::save_then_close(&mut **view, layout, damage)
            }
            _ => HostOutcome::asking(tairix_window::docapp::Request::SaveThenClose),
        }
    }

    fn chosen(
        &mut self,
        item: AppMenuItemId,
        layout: &EngineLayout,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::chosen(&mut **view, item, layout, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => view.chosen_row(item, layout, damage),
            _ => HostOutcome::none(),
        }
    }

    fn entered(
        &mut self,
        item: AppMenuItemId,
        text: &str,
        layout: &EngineLayout,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::entered(&mut **view, item, text, layout, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.entered(item, text, layout, damage)
            }
            _ => HostOutcome::none(),
        }
    }

    fn focus_changed(&mut self, focused: bool, layout: &EngineLayout, damage: &mut Region) {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::focus_changed(&mut **view, focused, layout, damage);
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.focus_changed(focused, layout, damage);
            }
            _ => {}
        }
    }

    fn input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &EngineLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::input(&mut **view, input, now_ns, layout, scale, theme, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.input(input, now_ns, layout, scale, theme, damage)
            }
            _ => HostOutcome::none(),
        }
    }

    fn refuse_save(&self, name: &str) -> Option<String> {
        match self {
            Self::Picture(view) => DocumentView::refuse_save(&**view, name),
            Self::Area(view) => view.refuse_save(name),
        }
    }

    fn ask_how(
        &mut self,
        then_close: bool,
        layout: &EngineLayout,
        damage: &mut Region,
    ) -> Option<Outcome> {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::ask_how(&mut **view, then_close, layout, damage)
            }
            _ => None,
        }
    }

    fn offered_extension(&self) -> &'static str {
        match self {
            Self::Picture(view) => DocumentView::offered_extension(&**view),
            Self::Area(view) => view.offered_extension(),
        }
    }

    fn save_endings(&self) -> Result<SaveEndings, String> {
        match self {
            Self::Picture(view) => DocumentView::save_endings(&**view),
            Self::Area(view) => view.save_endings(),
        }
    }

    fn tool_window(&self, layout: &EngineLayout, index: usize) -> Option<ToolWindow<'_>> {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::tool_window(&**view, layout, index)
            }
            _ => None,
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
        layout: &EngineLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::tool_moved(&mut **view, moved, layout, scale, theme, damage)
            }
            _ => HostOutcome::none(),
        }
    }

    fn tool_gone(
        &mut self,
        id: u32,
        why: ToolGone,
        layout: &EngineLayout,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::tool_gone(&mut **view, id, why, layout, damage)
            }
            _ => HostOutcome::none(),
        }
    }

    fn dialog_window(
        &self,
        layout: &EngineLayout,
        scale: Scale,
        theme: &Theme,
    ) -> Option<DialogWindow<'_>> {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::dialog_window(&**view, layout, scale, theme)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.dialog_window(layout, scale, theme)
            }
            _ => None,
        }
    }

    fn dialog_input(
        &mut self,
        input: &InputEvent,
        now_ns: u64,
        layout: &EngineLayout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::dialog_input(&mut **view, input, now_ns, layout, scale, theme, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.dialog_input(input, now_ns, layout, (scale, theme), damage)
            }
            _ => HostOutcome::none(),
        }
    }

    fn dialog_focus_changed(&mut self, focused: bool, layout: &EngineLayout, damage: &mut Region) {
        if let (Self::Picture(view), EngineLayout::Picture(layout)) = (self, layout) {
            DocumentView::dialog_focus_changed(&mut **view, focused, layout, damage);
        }
    }

    fn dialog_refused(&mut self, why: &str, layout: &EngineLayout, damage: &mut Region) -> Outcome {
        match (self, layout) {
            (Self::Picture(view), EngineLayout::Picture(layout)) => {
                DocumentView::dialog_refused(&mut **view, why, layout, damage)
            }
            (Self::Area(view), EngineLayout::Area(layout)) => {
                view.dialog_refused(why, layout, damage)
            }
            _ => HostOutcome::none(),
        }
    }
}
