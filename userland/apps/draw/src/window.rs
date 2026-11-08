//! The contract the document host drives a Draw window through.

use alloc::string::String;
use alloc::sync::Arc;

use tairix_abi::window_ipc::{AppMenu, AppMenuItemId, CursorShape, SaveFormats};
use tairix_bezier::Point;
use tairix_browse::media::{save_format, MediaType};
use tairix_collections::try_arc;
use tairix_drawing::unkept::Unkept;
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::InputEvent;
use tairix_theme::Theme;
use tairix_util::mathf::round_i32;
use tairix_window::docapp::{DocumentView, Outcome, Request};
use tairix_window::document::{Access, SavedDocument};

use crate::copy::CopyFormat;
use crate::input::ViewOutcome;
use crate::layout::{Faces, Layout};
use crate::menu::chosen;
use crate::tools::Tool;
use crate::view::{MenuKind, Own, Snapshot, View};

/// Whether a file called `name` is one Draw saves its documents in.
fn native(name: &str) -> bool {
    CopyFormat::of(name, None) == Some(CopyFormat::Svg)
}

impl SavedDocument for View {
    type Snapshot = Snapshot;

    fn access(&self) -> Access {
        self.access
    }

    fn is_modified(&self) -> bool {
        self.document.is_modified() || self.live.is_some()
    }

    fn is_empty(&self) -> bool {
        let drawing = self.document.drawing();
        !self.is_modified()
            && self.document.depth() == 0
            && drawing.objects() == drawing.layers().len()
    }

    fn snapshot(&mut self) -> Option<(u64, Arc<Snapshot>)> {
        // A save the view asked for landed this already, and reported what
        // it repainted; one the host began on its own repaints the window.
        self.land_pending(&mut Region::new());
        self.finish_pen();
        let drawing = self.document.drawing().clone();
        let frozen = try_arc(Snapshot { drawing }).ok()?;
        Some((self.document.generation(), frozen))
    }

    fn saved(&mut self, generation: u64) {
        self.document.saved(generation);
    }

    fn rename(&mut self, name: String) {
        // A Save As writes everything the drawing holds into a file of its own.
        self.unkept = Unkept::NONE;
        self.name = name;
    }

    fn set_access(&mut self, access: Access) {
        self.access = access;
    }

    fn say(&mut self, message: String) {
        self.message = Some(message);
    }
}

impl DocumentView for View {
    type Layout = Layout;
    type Faces = Faces;
    type MenuKind = MenuKind;
    type Own = Own;

    fn name(&self) -> &str {
        &self.name
    }

    fn write_title(&self, title: &mut String) {
        title.push_str(&self.name);
        if self.access == Access::ReadOnly {
            title.push_str(" (read-only)");
        }
    }

    fn layout(
        &self,
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        _faces: Faces,
    ) -> Layout {
        Layout::of(width, height, scale, theme)
    }

    fn min_size(&self, theme: &Theme, scale: Scale, _faces: Faces) -> (u32, u32) {
        Layout::least(scale, theme)
    }

    fn settle(&mut self, layout: &Layout, damage: &mut Region) {
        let ratio = f64::from(layout.ratio) / 100.0;
        let centre = Point::new(
            f64::from(layout.canvas.width) / 2.0,
            f64::from(layout.canvas.height) / 2.0,
        );
        if self.placed {
            self.viewport.rescale(ratio, centre);
        } else {
            self.viewport = crate::viewport::Viewport::new(1.0, ratio);
            let page = self.document.drawing().page().bounds();
            self.fit(page, layout);
            self.placed = true;
        }
        self.laid = Some(*layout);
        damage.add(layout.window);
    }

    fn message_area(layout: &Layout) -> tairix_geometry::Rect {
        layout.status
    }

    fn cursor(&self, layout: &Layout, at: tairix_geometry::Point) -> CursorShape {
        if !layout.canvas.contains(at) {
            return CursorShape::Arrow;
        }
        if self.spaced || matches!(self.gesture, Some(crate::view::Gesture::Pan { .. })) {
            return CursorShape::Pointer;
        }
        match self.tool {
            Tool::Select | Tool::Nodes | Tool::Eyedropper => CursorShape::Arrow,
            Tool::Text => CursorShape::Text,
            Tool::Hand => CursorShape::Pointer,
            _ => CursorShape::Crosshair,
        }
    }

    fn tool_tip(&self, layout: &Layout, scale: Scale, theme: &Theme) -> Option<(Rect, &str)> {
        let at = tairix_geometry::Point::new(round_i32(self.pointer.x), round_i32(self.pointer.y));
        let index = self.tool_box.tool_at(layout.tools, scale, theme, at)?;
        let rect = self.tool_box.tool_rect(index, layout.tools, scale, theme)?;
        Some((rect, self.tips.get(index)?.as_str()))
    }

    fn menu(&self, kind: MenuKind) -> AppMenu {
        self.build_menu(kind)
    }

    fn close_requested(&mut self, _layout: &Layout, damage: &mut Region) -> ViewOutcome {
        self.closing(damage)
    }

    fn save_then_close(&mut self, _layout: &Layout, damage: &mut Region) -> ViewOutcome {
        self.land_pending(damage);
        self.finish_pen();
        Outcome::asking(Request::SaveThenClose)
    }

    fn chosen(&mut self, item: AppMenuItemId, layout: &Layout, damage: &mut Region) -> ViewOutcome {
        match chosen(item.get()) {
            Some(chosen) => self.choose(chosen, layout, damage),
            None => Outcome::none(),
        }
    }

    fn focus_changed(&mut self, _focused: bool, layout: &Layout, damage: &mut Region) {
        self.spaced = false;
        damage.add(layout.window);
    }

    fn input(
        &mut self,
        input: &InputEvent,
        _now_ns: u64,
        layout: &Layout,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> ViewOutcome {
        self.on_input(input, layout, scale, theme, damage)
    }

    fn refuse_save(&self, name: &str) -> Option<String> {
        if !native(name) {
            return Some(String::from(
                "Draw saves its drawings as SVG. Name the file with .svg, or save a copy to write PDF, PNG or a RISC OS Draw file.",
            ));
        }
        if self.access == Access::ReadOnly && name == self.name && !self.unkept.is_empty() {
            return Some(String::from(
                "The file holds what a drawing cannot keep, so saving over it would lose that. Save it under another name.",
            ));
        }
        None
    }

    fn offered_extension(&self) -> &'static str {
        "svg"
    }

    fn save_formats(&self) -> Result<SaveFormats, String> {
        let mut formats = SaveFormats::ANY;
        formats
            .push(save_format(MediaType::ImageSvg), true)
            .map_err(|_| String::from("no format can be offered"))?;
        Ok(formats)
    }

    fn copy_formats(&self) -> Result<SaveFormats, String> {
        let mut formats = SaveFormats::ANY;
        for (media, current) in [
            (MediaType::Pdf, true),
            (MediaType::PostScript, false),
            (MediaType::ImagePng, false),
            (MediaType::ImageJpeg, false),
            (MediaType::ImageTiff, false),
            (MediaType::ImageSvg, false),
            (MediaType::RiscOsDraw, false),
        ] {
            formats
                .push(save_format(media), current)
                .map_err(|_| String::from("no format can be offered"))?;
        }
        Ok(formats)
    }

    fn copy_extension(&self) -> &'static str {
        "pdf"
    }
}
