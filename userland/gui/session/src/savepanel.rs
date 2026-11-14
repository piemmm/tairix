//! The trusted picker's Save panel (`plans/SAVE-DIALOG.md` SD1): the
//! document's icon, its name with the stem selected, the folder it goes in and
//! the format it is written in, and the answers, with the folder browser shown
//! above it on request.
//!
//! Layout, paint, hit-testing and the name's format rules only: the picker
//! ([`crate::picker`]) owns what a press asks for and the open that concludes
//! the save.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::home::{HOME_DESKTOP_DIR, HOME_FILES_DIR, HOME_FILES_SUBDIRS};
use tairix_abi::window_ipc::{AppMenuItemId, AppMenuSymbol, SaveFormat, SaveFormats};
use tairix_abi::FS_NAME_MAX;
use tairix_browse::media::{media_for_filetype, media_for_name, MediaType};
use tairix_browse::Ending;
use tairix_colour::Rgba;
use tairix_controls::{
    ground_fill, paint_icon_slot, paint_surface_plate, plate_border, ArtworkTone, Button,
    ButtonContent, ChainModel, ChainRow, ChromeLayer, ComboBox, ControlRole, ControlState,
    MenuItem, TextField, ValidationState,
};
use tairix_font::{BitmapFont, Cut, ELLIPSIS};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::{IconArtwork, IconKind, IconRequest};
use tairix_theme::{TextRole, Theme};
use tairix_wm::{InputEvent, Surface};

use crate::picker::{CANCEL_LABEL, SAVE_LABEL};

/// The document icon's side, in logical pixels: large enough to be the thing
/// dragged, as a RISC OS save box's is.
const ICON_SIDE: u32 = 64;

/// The plate's margin around the icon, in logical pixels.
const TILE_PAD: u32 = 12;

const NAME_LABEL: &str = "Name";
const FOLDER_NAME_LABEL: &str = "Folder";
const WHERE_LABEL: &str = "Where";
const FORMAT_LABEL: &str = "Format";
const SHOW_FOLDERS: &str = "Show Folders";
const HIDE_FOLDERS: &str = "Hide Folders";
const NEW_FOLDER_LABEL: &str = "New Folder";
const CREATE_LABEL: &str = "Create";

/// The caption under the icon.
const DRAG_CAPTION: &str = "Drag to save";

/// What a press on the panel lands on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PanelHit {
    /// The document's icon, which a drag carries to a folder.
    Icon,
    /// The name field.
    Name,
    /// The folder the document goes in.
    Where,
    /// The format it is written in.
    Format,
    /// *Show Folders* or *Hide Folders*.
    Browse,
    /// *New Folder*, while the folders show.
    NewFolder,
    /// *Cancel*.
    Cancel,
    /// *Save*, or *Create* while a new folder is named.
    Save,
}

/// Which of the panel's controls the keyboard is on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PanelFocus {
    /// The name field.
    Name,
    /// *Where*.
    Where,
    /// *Format*.
    Format,
    /// *Show Folders* or *Hide Folders*.
    Browse,
    /// The folder listing, while it shows.
    Listing,
    /// *New Folder*, while the folders show.
    NewFolder,
    /// *Cancel*.
    Cancel,
    /// *Save*.
    Save,
}

/// The order `Tab` walks the panel's controls in; `Shift+Tab` walks it back.
const FOCUS_ORDER: [PanelFocus; 8] = [
    PanelFocus::Name,
    PanelFocus::Where,
    PanelFocus::Format,
    PanelFocus::Browse,
    PanelFocus::Listing,
    PanelFocus::NewFolder,
    PanelFocus::Cancel,
    PanelFocus::Save,
];

/// Where the panel's parts sit, in window-local pixels.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct PanelLayout {
    /// The whole panel.
    pub(crate) panel: Rect,
    /// The plate the document's icon stands on: what a drag picks up.
    pub(crate) tile: Rect,
    pub(crate) icon: Rect,
    pub(crate) caption: Point,
    /// The right edge the labels are set against.
    pub(crate) labels_right: i32,
    pub(crate) name: Rect,
    pub(crate) where_box: Rect,
    /// The format chooser, where the requester offers formats.
    pub(crate) format_box: Option<Rect>,
    /// The line under the controls that states progress or a refusal.
    pub(crate) status: Rect,
    /// The band the answers stand in.
    pub(crate) footer: Rect,
    pub(crate) browse: Rect,
    /// *New Folder*, while the folders show.
    pub(crate) new_folder: Option<Rect>,
    pub(crate) cancel: Rect,
    pub(crate) save: Rect,
}

impl PanelLayout {
    /// What a press at `at` lands on.
    pub(crate) fn hit(&self, at: Point) -> Option<PanelHit> {
        [
            (Some(self.tile), PanelHit::Icon),
            (Some(self.name), PanelHit::Name),
            (Some(self.where_box), PanelHit::Where),
            (self.format_box, PanelHit::Format),
            (Some(self.browse), PanelHit::Browse),
            (self.new_folder, PanelHit::NewFolder),
            (Some(self.cancel), PanelHit::Cancel),
            (Some(self.save), PanelHit::Save),
        ]
        .into_iter()
        .find_map(|(rect, hit)| rect.filter(|rect| rect.contains(at)).map(|_| hit))
    }
}

/// What the panel's status line says.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Said {
    /// Nothing.
    #[default]
    Nothing,
    /// Work under way.
    Progress(String),
    /// Why the document, or the folder, was not made.
    Refusal(String),
}

/// A save pick's own controls: the name to save as, where and in what format,
/// and the answers.
pub(crate) struct SavePanel {
    pub(crate) name: TextField,
    /// The formats the requester can write.
    formats: SaveFormats,
    /// The one of them the document is to be written in, while it offers any.
    chosen: Option<usize>,
    /// The name offered when the pick began, put back after the field named a
    /// folder the picker then went into.
    pub(crate) suggested: String,
    where_box: ComboBox,
    format_box: Option<ComboBox>,
    browse: Button,
    new_folder: Button,
    cancel: Button,
    save: Button,
    /// Whether the folder browser shows above the panel.
    expanded: bool,
    /// The control the keyboard is on.
    focus: PanelFocus,
    /// The document's name, kept aside while the field names a new folder.
    naming: Option<String>,
    said: Said,
    /// Whether the pointer rests on the icon's plate.
    tile_lit: bool,
}

impl SavePanel {
    /// A panel offering `suggested`, its stem selected, in the format of
    /// `formats` the document is in now.
    pub(crate) fn new(suggested: &str, formats: &SaveFormats) -> Self {
        let chosen = formats.current();
        let mut panel = Self {
            name: offered(suggested, formats, chosen),
            formats: *formats,
            chosen,
            suggested: String::from(suggested),
            where_box: ComboBox::new(Vec::new()),
            format_box: None,
            browse: Button::labelled(SHOW_FOLDERS),
            new_folder: Button::labelled(NEW_FOLDER_LABEL),
            cancel: Button::labelled(CANCEL_LABEL),
            save: primary(SAVE_LABEL),
            expanded: false,
            focus: PanelFocus::Name,
            naming: None,
            said: Said::Nothing,
            tile_lit: false,
        };
        panel.restate_format();
        panel
    }

    /// State `said` on the status line, a refusal marking the name refused.
    pub(crate) fn say(&mut self, said: Said) {
        self.said = said;
        self.restate_validation();
    }

    /// Mark the name refused while the status line states a refusal: a field
    /// rebuilt for a new name keeps reading as what was refused.
    fn restate_validation(&mut self) {
        let validation = if matches!(self.said, Said::Refusal(_)) {
            ValidationState::Invalid
        } else {
            ValidationState::Valid
        };
        let state = self.name.state().with_validation(validation);
        self.name.set_state(state);
    }

    /// Track the pointer `event` over the panel at `layout` for the hover its
    /// controls show, reporting what it repainted into `damage`. Presses are
    /// the picker's to answer.
    pub(crate) fn on_pointer(
        &mut self,
        event: &InputEvent,
        layout: &PanelLayout,
        damage: &mut Region,
    ) {
        let lit = match event {
            InputEvent::PointerMoved { to } => layout.tile.contains(*to),
            InputEvent::PointerCancelled => false,
            _ => self.tile_lit,
        };
        if lit != self.tile_lit {
            self.tile_lit = lit;
            damage.add(layout.tile);
        }
        let buttons = [
            (&mut self.browse, Some(layout.browse)),
            (&mut self.new_folder, layout.new_folder),
            (&mut self.cancel, Some(layout.cancel)),
            (&mut self.save, Some(layout.save)),
        ];
        for (button, bounds) in buttons {
            if let Some(bounds) = bounds {
                let _ = button.on_pointer(event, bounds, damage);
            }
        }
    }

    /// The control the keyboard is on.
    pub(crate) const fn focus(&self) -> PanelFocus {
        self.focus
    }

    /// Put the keyboard on `focus`, or on the name where that cannot take it
    /// now.
    pub(crate) fn set_focus(&mut self, focus: PanelFocus) {
        self.focus = if self.focusable(focus) {
            focus
        } else {
            PanelFocus::Name
        };
        self.name.set_focused(self.focus == PanelFocus::Name);
        self.where_box.set_focused(self.focus == PanelFocus::Where);
        if let Some(format) = self.format_box.as_mut() {
            format.set_focused(self.focus == PanelFocus::Format);
        }
        self.browse.set_focused(self.focus == PanelFocus::Browse);
        self.new_folder
            .set_focused(self.focus == PanelFocus::NewFolder);
        self.cancel.set_focused(self.focus == PanelFocus::Cancel);
        self.save.set_focused(self.focus == PanelFocus::Save);
    }

    /// Move the keyboard to the next control that can take it, or with `back`
    /// the one before, wrapping at the ends.
    pub(crate) fn move_focus(&mut self, back: bool) {
        let len = FOCUS_ORDER.len();
        let at = FOCUS_ORDER
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        let next = (1..len)
            .map(|step| {
                if back {
                    (at + len - step) % len
                } else {
                    (at + step) % len
                }
            })
            .map(|index| FOCUS_ORDER[index])
            .find(|focus| self.focusable(*focus));
        if let Some(next) = next {
            self.set_focus(next);
        }
    }

    /// Whether the keyboard can be on `focus`: the format only when there is
    /// a choice of one and no folder is being named, the listing and *New
    /// Folder* only while the folders show.
    fn focusable(&self, focus: PanelFocus) -> bool {
        match focus {
            PanelFocus::Format => self.formats.len() > 1 && self.naming.is_none(),
            PanelFocus::Listing | PanelFocus::NewFolder => self.expanded,
            _ => true,
        }
    }

    /// Whether the field names a new folder rather than the document.
    pub(crate) const fn naming(&self) -> bool {
        self.naming.is_some()
    }

    /// Name a new folder in the field, offering `suggested` selected; the
    /// document's name is kept to come back to, and *Save* reads *Create*.
    pub(crate) fn begin_naming(&mut self, suggested: &str) {
        if self.naming.is_none() {
            self.naming = Some(String::from(self.name.text()));
        }
        let mut field = TextField::new()
            .with_text(suggested)
            .with_max_len(FS_NAME_MAX)
            .with_selection(0..suggested.len());
        field.set_focused(true);
        self.name = field;
        self.restate_validation();
        self.save = primary(CREATE_LABEL);
        self.set_focus(PanelFocus::Name);
    }

    /// Stop naming a folder: the field holds the document's name again, its
    /// stem selected.
    pub(crate) fn end_naming(&mut self) {
        let Some(document) = self.naming.take() else {
            return;
        };
        self.name = offered(&document, &self.formats, self.chosen);
        self.restate_validation();
        self.save = primary(SAVE_LABEL);
        self.restate_format();
        self.set_focus(PanelFocus::Name);
    }

    /// Offer `name` in the field as a pick begins, its stem selected so
    /// typing replaces it and keeps its ending.
    pub(crate) fn offer(&mut self, name: &str) {
        self.name = offered(name, &self.formats, self.chosen);
        self.restate_validation();
        self.restate_format();
        self.set_focus(PanelFocus::Name);
    }

    /// The formats the requester can write.
    pub(crate) const fn formats(&self) -> &SaveFormats {
        &self.formats
    }

    /// Whether the folder browser shows.
    pub(crate) const fn expanded(&self) -> bool {
        self.expanded
    }

    /// Show the folder browser, or hide it.
    pub(crate) fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        self.browse = Button::labelled(if expanded { HIDE_FOLDERS } else { SHOW_FOLDERS });
        self.set_focus(self.focus);
    }

    /// Show `folder` as the place the document goes.
    pub(crate) fn set_where(&mut self, folder: &str) {
        if self.where_box.selected_text() != Some(folder) {
            self.where_box = ComboBox::new(alloc::vec![String::from(folder)]).with_selected(0);
            self.where_box.set_focused(self.focus == PanelFocus::Where);
        }
    }

    /// The format the document is to be written in, as an index of those
    /// offered, or `None` where none is offered.
    pub(crate) const fn format(&self) -> Option<usize> {
        self.chosen
    }

    /// Write the document in the format at `index`, its name keeping its stem
    /// and taking the format's extension, if it is named by one.
    pub(crate) fn set_format(&mut self, index: usize) -> bool {
        let Some(format) = self.formats.get(index) else {
            return false;
        };
        let name = self.name.text();
        let stem = &name[..stem_len(name, &self.formats, self.chosen)];
        let renamed = format!("{stem}{}", format.extensions().next().unwrap_or(""));
        self.chosen = Some(index);
        self.name.set_text(&renamed);
        self.restate_format();
        true
    }

    /// Show the format the document is written in, after the name was
    /// edited: a name typed into another format offered takes that format.
    pub(crate) fn restate_format(&mut self) {
        if let Some(typed) = self.formats.of(self.name.text()) {
            self.chosen = Some(typed);
        }
        let Some(format) = self.chosen.and_then(|index| self.formats.get(index)) else {
            self.format_box = None;
            return;
        };
        let label = format_label(format);
        let shown = self.format_box.as_ref().and_then(ComboBox::selected_text);
        if shown != Some(label.as_str()) {
            let mut combo = ComboBox::new(alloc::vec![label]).with_selected(0);
            combo.set_focused(self.focus == PanelFocus::Format);
            // One format offered is one to state, not to choose.
            if self.naming.is_some() || self.formats.len() == 1 {
                combo.set_state(ControlState::disabled());
            }
            self.format_box = Some(combo);
        }
    }

    /// The picture the document is drawn with: its format's own, or its
    /// name's where no format is offered.
    pub(crate) fn icon(&self) -> IconKind {
        self.chosen
            .and_then(|index| self.formats.get(index))
            .and_then(format_media)
            .or_else(|| media_for_name(self.name.text()))
            .map_or(IconKind::File, MediaType::icon)
    }

    /// The menu *Format* opens: every format offered by its type's name, the
    /// one chosen marked.
    pub(crate) fn format_menu(&self) -> ChainModel {
        let mut model = ChainModel::new(FORMAT_LABEL);
        for (index, format) in self.formats.iter().enumerate() {
            let Some(id) = AppMenuItemId::for_index(index) else {
                continue;
            };
            model.push(ChainRow::item(
                id,
                AppMenuSymbol::File,
                crate::menu::marked(
                    MenuItem::new(format_label(format)),
                    self.chosen == Some(index),
                ),
            ));
        }
        model
    }

    /// Where the panel's parts sit, the panel's top edge at `top` in a window
    /// `width` pixels wide: the icon's plate and the controls beside it, the
    /// status line under them, and the answers in the band below.
    pub(crate) fn layout(&self, top: i32, width: u32, scale: Scale, theme: &Theme) -> PanelLayout {
        let metrics = theme.metrics();
        let inset = scale.scale_length(metrics.control_inset);
        let gap = scale.scale_length(metrics.control_gap);
        let margin = inset.saturating_mul(2);
        let row = TextField::height(scale, theme);
        let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        let side = scale.scale_length(ICON_SIDE);
        let tile_side = side.saturating_add(scale.scale_length(TILE_PAD).saturating_mul(2));
        let labels = [NAME_LABEL, FOLDER_NAME_LABEL, WHERE_LABEL, FORMAT_LABEL]
            .into_iter()
            .map(|label| body.text_width(label))
            .max()
            .unwrap_or(0);
        let at = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let first = top.saturating_add(at(margin));
        let tile = Rect::new(at(margin), first, tile_side, tile_side);
        let centring = at(tile_side.saturating_sub(side) / 2);
        let icon = Rect::new(
            tile.left().saturating_add(centring),
            tile.top().saturating_add(centring),
            side,
            side,
        );
        let caption_at = Point::new(tile.left(), tile.bottom().saturating_add(at(gap)));
        let labels_right = margin
            .saturating_mul(2)
            .saturating_add(tile_side)
            .saturating_add(labels);
        let field_x = labels_right.saturating_add(gap);
        let field_w = width.saturating_sub(field_x).saturating_sub(margin);
        let line = |y: i32| Rect::new(at(field_x), y, field_w, row);
        let name = line(first);
        let where_box = line(name.bottom().saturating_add(at(gap)));
        let format_box = self
            .format_box
            .is_some()
            .then(|| line(where_box.bottom().saturating_add(at(gap))));
        let controls_bottom = format_box.map_or(where_box.bottom(), |rect| rect.bottom());
        let status = Rect::new(
            at(field_x),
            controls_bottom.saturating_add(at(gap)),
            field_w,
            body.line_height(),
        );
        let caption_bottom = caption_at.y.saturating_add(at(caption.line_height()));
        let footer_top = status
            .bottom()
            .max(caption_bottom)
            .saturating_add(at(inset));
        let footer = Rect::new(
            0,
            footer_top,
            width,
            row.saturating_add(inset.saturating_mul(2)),
        );
        let buttons = footer_top.saturating_add(at(inset));
        let save_w = self.save.measured_width(scale, theme);
        let cancel_w = self.cancel.measured_width(scale, theme);
        let browse_w = self.browse.measured_width(scale, theme);
        let save_x = width.saturating_sub(margin).saturating_sub(save_w);
        let cancel_x = save_x.saturating_sub(gap).saturating_sub(cancel_w);
        let new_folder = self.expanded.then(|| {
            Rect::new(
                at(margin.saturating_add(browse_w).saturating_add(gap)),
                buttons,
                self.new_folder.measured_width(scale, theme),
                row,
            )
        });
        PanelLayout {
            panel: Rect::new(
                0,
                top,
                width,
                u32::try_from(footer.bottom().saturating_sub(top)).unwrap_or(0),
            ),
            tile,
            icon,
            caption: caption_at,
            labels_right: at(labels_right),
            name,
            where_box,
            format_box,
            status,
            footer,
            browse: Rect::new(at(margin), buttons, browse_w, row),
            new_folder,
            cancel: Rect::new(at(cancel_x), buttons, cancel_w, row),
            save: Rect::new(at(save_x), buttons, save_w, row),
        }
    }

    /// Draw the panel at `layout`, the document's picture from `artwork`.
    pub(crate) fn render(
        &self,
        surface: &mut Surface,
        layout: &PanelLayout,
        scale: Scale,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
    ) {
        let palette = theme.palette();
        fill(
            surface,
            layout.panel,
            ground_fill(theme, palette.surface, ChromeLayer::Ground),
        );
        if self.expanded {
            rule(surface, layout.panel, scale, palette.border);
        }
        self.draw_tile(surface, layout, scale, theme, artwork);
        let caption = BitmapFont::for_role(theme.fonts(), TextRole::Caption, scale);
        let centred = layout
            .tile
            .width
            .saturating_sub(caption.text_width(DRAG_CAPTION))
            / 2;
        caption.draw_text(
            surface,
            layout
                .caption
                .x
                .saturating_add(i32::try_from(centred).unwrap_or(0)),
            layout.caption.y,
            DRAG_CAPTION,
            palette.on_surface_muted.into(),
        );
        let body = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
        let label = |surface: &mut Surface, row: Rect, text: &str| {
            let x = layout
                .labels_right
                .saturating_sub(i32::try_from(body.text_width(text)).unwrap_or(0));
            let drop = row.height.saturating_sub(body.line_height()) / 2;
            let y = row.top().saturating_add(i32::try_from(drop).unwrap_or(0));
            body.draw_text(surface, x, y, text, palette.on_surface_muted.into());
        };
        let named = if self.naming.is_some() {
            FOLDER_NAME_LABEL
        } else {
            NAME_LABEL
        };
        label(surface, layout.name, named);
        label(surface, layout.where_box, WHERE_LABEL);
        self.name.render(surface, layout.name, scale, theme);
        self.where_box
            .render(surface, layout.where_box, scale, theme);
        if let (Some(field), Some(rect)) = (self.format_box.as_ref(), layout.format_box) {
            label(surface, rect, FORMAT_LABEL);
            field.render(surface, rect, scale, theme);
        }
        self.draw_status(surface, layout.status, body, theme);
        fill(
            surface,
            layout.footer,
            ground_fill(theme, palette.surface_raised, ChromeLayer::Inlay),
        );
        rule(surface, layout.footer, scale, palette.border);
        self.browse.render(surface, layout.browse, scale, theme);
        if let Some(rect) = layout.new_folder {
            self.new_folder.render(surface, rect, scale, theme);
        }
        self.cancel.render(surface, layout.cancel, scale, theme);
        self.save.render(surface, layout.save, scale, theme);
    }

    /// Draw the plate the document's picture stands on, lit while the
    /// pointer rests on it, and the picture: its own artwork where `artwork`
    /// has it, its kind's glyph in the accent otherwise.
    fn draw_tile(
        &self,
        surface: &mut Surface,
        layout: &PanelLayout,
        scale: Scale,
        theme: &Theme,
        artwork: &mut dyn IconArtwork,
    ) {
        let palette = theme.palette();
        if let Some((x, y)) = layout.tile.surface_origin() {
            let side = layout.tile.width;
            let radius = scale
                .scale_length(theme.metrics().control_corner_radius)
                .min(side / 2);
            let ground = if self.tile_lit {
                palette.surface_hover
            } else {
                palette.surface_raised
            };
            let _ = paint_surface_plate(
                surface,
                (x, y, side, side),
                (radius, plate_border(theme, scale)),
                theme,
                (ground, ChromeLayer::Plate),
            );
        }
        let Some((x, y)) = layout.icon.surface_origin() else {
            return;
        };
        let kind = self.icon();
        let side = layout.icon.width;
        paint_icon_slot(
            surface,
            (x, y, side),
            kind,
            palette.accent.into(),
            artwork.artwork(IconRequest::kind(kind), side),
            ArtworkTone::FULL,
        );
    }

    /// Draw what the status line says in `bounds`: progress quietly, a
    /// refusal in the danger colour, cut to the line with a mark.
    fn draw_status(&self, surface: &mut Surface, bounds: Rect, font: BitmapFont, theme: &Theme) {
        let palette = theme.palette();
        let (text, colour) = match &self.said {
            Said::Nothing => return,
            Said::Progress(text) => (text.as_str(), palette.on_surface_muted),
            Said::Refusal(text) => (text.as_str(), palette.danger),
        };
        let Some(line) = font.wrap_with_cut(text, bounds.width, 1, Cut::End).next() else {
            return;
        };
        let pen = font.draw_text(
            surface,
            bounds.left(),
            bounds.top(),
            line.text,
            colour.into(),
        );
        if line.elided {
            font.draw_text(surface, pen, bounds.top(), ELLIPSIS, colour.into());
        }
    }
}

/// Fill `bounds` with `colour`.
fn fill(surface: &mut Surface, bounds: Rect, colour: Rgba) {
    if let Some((x, y)) = bounds.surface_origin() {
        surface.fill_rect(x, y, bounds.width, bounds.height, colour.into());
    }
}

/// Draw the hairline that parts `bounds` from what is above it.
fn rule(surface: &mut Surface, bounds: Rect, scale: Scale, colour: Rgba) {
    if let Some((x, y)) = bounds.surface_origin() {
        surface.fill_rect(
            x,
            y,
            bounds.width,
            scale.scale_length(1).max(1),
            colour.into(),
        );
    }
}

/// One place *Where* offers: what it is called, and where it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Place {
    pub(crate) label: String,
    pub(crate) components: Vec<String>,
}

impl Place {
    fn at(components: Vec<String>) -> Self {
        let label = components
            .last()
            .map_or_else(|| String::from("/"), String::clone);
        Self { label, components }
    }
}

/// The places *Where* offers a picker that starts among the user's own files
/// at `start`, showing `shown`, the last save having gone to `last`: the
/// folder shown, the user's own places, and the last save's folder, each once.
pub(crate) fn places(
    start: &[String],
    shown: Option<&[String]>,
    last: Option<&[String]>,
) -> Vec<Place> {
    let mut found: Vec<Vec<String>> = Vec::new();
    let mut offer = |components: Vec<String>| {
        if !found.contains(&components) {
            found.push(components);
        }
    };
    if let Some(shown) = shown {
        offer(shown.to_vec());
    }
    if start.last().is_some_and(|leaf| leaf == HOME_FILES_DIR) {
        offer(start.to_vec());
        for sub in HOME_FILES_SUBDIRS {
            let mut place = start.to_vec();
            place.push(String::from(sub));
            offer(place);
        }
        let mut desktop = start[..start.len() - 1].to_vec();
        desktop.push(String::from(HOME_DESKTOP_DIR));
        offer(desktop);
    } else {
        offer(start.to_vec());
    }
    if let Some(last) = last {
        offer(last.to_vec());
    }
    found.into_iter().map(Place::at).collect()
}

/// The menu *Where* opens: every one of `places` by its name, the one `shown`
/// marked.
pub(crate) fn where_menu(places: &[Place], shown: Option<&[String]>) -> ChainModel {
    let mut model = ChainModel::new(WHERE_LABEL);
    for (index, place) in places.iter().enumerate() {
        let Some(id) = AppMenuItemId::for_index(index) else {
            continue;
        };
        let chosen = shown == Some(place.components.as_slice());
        model.push(ChainRow::item(
            id,
            AppMenuSymbol::OpenFolder,
            crate::menu::marked(MenuItem::new(place.label.as_str()), chosen),
        ));
    }
    model
}

/// The name field offering `name` for the format of `formats` at `chosen`,
/// focused, its stem selected: an ending another type names is left off, so
/// a document saved in another format is not offered under its old type's
/// name.
fn offered(name: &str, formats: &SaveFormats, chosen: Option<usize>) -> TextField {
    let format = chosen.and_then(|index| formats.get(index));
    let foreign =
        format.is_some_and(|format| !format.types(name) && named_by_another(name, format));
    let name = if foreign { Ending::of(name).stem } else { name };
    let mut field = TextField::new()
        .with_text(name)
        .with_max_len(FS_NAME_MAX)
        .with_selection(0..stem_len(name, formats, chosen));
    field.set_focused(true);
    field
}

/// Whether `name` ends as the registry names a type other than `format`'s.
fn named_by_another(name: &str, format: &SaveFormat) -> bool {
    media_for_name(name).is_some_and(|media| Some(media) != format_media(format))
}

/// The type `format` is: its file type's, else its first extension's.
pub(crate) fn format_media(format: &SaveFormat) -> Option<MediaType> {
    format.filetype().and_then(media_for_filetype).or_else(|| {
        let extension = format.extensions().next()?;
        media_for_name(&format!("x{extension}"))
    })
}

/// The panel's committing button, labelled `label`.
fn primary(label: &str) -> Button {
    Button::new(
        ButtonContent::Label(String::from(label)),
        ControlRole::Primary,
    )
}

/// How long the stem of `name` is: what comes before the ending a type is
/// named by — the chosen format's, or the registry's — where it has one, the
/// whole name where it has none.
fn stem_len(name: &str, formats: &SaveFormats, chosen: Option<usize>) -> usize {
    let typed = chosen
        .and_then(|index| formats.get(index))
        .is_some_and(|format| format.types(name));
    if typed || media_for_name(name).is_some() {
        Ending::of(name).stem.len()
    } else {
        name.len()
    }
}

/// What a format is called: its type's name in the registry, else the ending
/// it is named by, else the name RISC OS gives its file type, else that type's
/// number.
fn format_label(format: &SaveFormat) -> String {
    if let Some(media) = format_media(format) {
        return String::from(media.name());
    }
    if let Some(extension) = format.extensions().next() {
        return String::from(extension);
    }
    format.filetype().map_or_else(String::new, |filetype| {
        tairix_fsmeta::preset::acorn::filetype_name(filetype).map_or_else(
            || format!("File type &{:03X}", filetype.get()),
            String::from,
        )
    })
}

#[cfg(test)]
#[path = "savepanel_tests.rs"]
mod tests;
