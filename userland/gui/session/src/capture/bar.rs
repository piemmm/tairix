//! The capture bar: the floating chrome a capture is chosen from, at the
//! bottom centre of the work area (`plans/CAPTURE.md`).
//!
//! Close comes first, then the six captures as icon buttons in two groups —
//! stills, then recordings — the chosen one marked. Options opens the seat's
//! menu chain above the bar, and the primary button takes the capture. Each
//! part's tip comes through the seat's one tooltip.

use alloc::string::String;

use tairix_abi::input::{KeyInput, KeyValue, NamedKeyCode};
use tairix_controls::damage;
use tairix_controls::{
    paint_surface_plate, plate_border, plate_content_inset, Button, ButtonContent, ChromeLayer,
    ControlRole, IconButton, SelectionState,
};
use tairix_icon::{IconArtwork, IconKind, IconRequest};
use tairix_theme::Theme;
use tairix_wm::{
    Color, Compositor, Corners, InputEvent, Point, Rect, Region, Scale, Surface, WindowId,
};

use super::plate::{at, bottom_centre, PLATE_MARGIN};
use super::CaptureMode;
use crate::shell::{tip_region, DesktopShell};

/// A part of the bar.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Part {
    /// The close button.
    Close,
    /// The button for the mode at this place of [`CaptureMode::ALL`].
    Mode(usize),
    /// The Options button.
    Options,
    /// The button that takes the capture.
    Primary,
}

/// What a press on the bar asks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum BarAction {
    /// This mode was chosen.
    Mode(CaptureMode),
    /// The Options menu, opening above this screen rectangle.
    Options(Rect),
    /// Take the capture.
    Commit,
    /// Close the bar.
    Close,
}

const MODE_ICONS: [IconKind; 6] = [
    IconKind::CaptureScreen,
    IconKind::CaptureWindow,
    IconKind::CaptureArea,
    IconKind::RecordScreen,
    IconKind::RecordWindow,
    IconKind::RecordArea,
];

const MODE_TIPS: [&str; 6] = [
    "Capture Entire Screen",
    "Capture Selected Window",
    "Capture Selected Portion",
    "Record Entire Screen",
    "Record Selected Window",
    "Record Selected Portion",
];

const CAPTURE_LABEL: &str = "Capture";
const RECORD_LABEL: &str = "Record";

/// Where each part sits, window-local.
struct BarLayout {
    size: (u32, u32),
    close: Rect,
    modes: [Rect; 6],
    rules: [Rect; 2],
    options: Rect,
    primary: Rect,
}

impl BarLayout {
    fn of(options: &Button, scale: Scale, theme: &Theme) -> Self {
        let inset = plate_content_inset(theme, scale);
        let gap = scale.scale_length(theme.metrics().control_gap);
        let height = Button::height(scale, theme);
        let rule = plate_border(theme, scale).max(1);
        let primary = [CAPTURE_LABEL, RECORD_LABEL]
            .into_iter()
            .map(|label| primary_button(label).measured_width(scale, theme))
            .max()
            .unwrap_or(0);
        let top = at(inset);
        let mut x = inset;
        let mut next = |width: u32| {
            let rect = Rect::new(at(x), top, width, height);
            x = x.saturating_add(width).saturating_add(gap);
            rect
        };
        let close = next(height);
        let first_rule = next(rule);
        let stills = [next(height), next(height), next(height)];
        let second_rule = next(rule);
        let records = [next(height), next(height), next(height)];
        let options = next(options.measured_width(scale, theme));
        let primary = next(primary);
        let width = x.saturating_sub(gap).saturating_add(inset);
        let quarter = at(height / 4);
        let rules = [first_rule, second_rule].map(|rule| {
            Rect::new(
                rule.left(),
                rule.top().saturating_add(quarter),
                rule.width,
                height / 2,
            )
        });
        let [screen, window, area] = stills;
        let [record_screen, record_window, record_area] = records;
        Self {
            size: (width, height.saturating_add(inset.saturating_mul(2))),
            close,
            modes: [
                screen,
                window,
                area,
                record_screen,
                record_window,
                record_area,
            ],
            rules,
            options,
            primary,
        }
    }

    fn part_at(&self, point: Point) -> Option<Part> {
        if self.close.contains(point) {
            return Some(Part::Close);
        }
        if let Some(index) = self.modes.iter().position(|rect| rect.contains(point)) {
            return Some(Part::Mode(index));
        }
        if self.options.contains(point) {
            return Some(Part::Options);
        }
        self.primary.contains(point).then_some(Part::Primary)
    }

    fn rect(&self, part: Part) -> Option<Rect> {
        match part {
            Part::Close => Some(self.close),
            Part::Mode(index) => self.modes.get(index).copied(),
            Part::Options => Some(self.options),
            Part::Primary => Some(self.primary),
        }
    }
}

/// The bar's controls.
struct Parts {
    close: IconButton,
    modes: [IconButton; 6],
    options: Button,
    primary: Button,
}

impl Parts {
    fn render(
        &self,
        surface: &mut Surface,
        layout: &BarLayout,
        (scale, theme): (Scale, &Theme),
        artwork: &mut dyn IconArtwork,
    ) {
        let (width, height) = layout.size;
        let radius = scale.scale_length(theme.metrics().popup_corner_radius);
        let ground = (theme.palette().surface_raised, ChromeLayer::Plate);
        let shape = (radius, plate_border(theme, scale));
        let _ = paint_surface_plate(surface, (0, 0, width, height), shape, theme, ground);
        let icons = core::iter::once((&self.close, layout.close))
            .chain(self.modes.iter().zip(layout.modes));
        for (button, rect) in icons {
            let side = button.icon_side(rect, scale, theme);
            let picture = artwork.artwork(IconRequest::kind(button.icon()), side);
            button.render(surface, rect, scale, theme, picture);
        }
        let rule = Color::from(theme.palette().border);
        for line in layout.rules {
            if let Some((x, y)) = line.surface_origin() {
                surface.fill_rect(x, y, line.width, line.height, rule);
            }
        }
        self.options.render(surface, layout.options, scale, theme);
        self.primary.render(surface, layout.primary, scale, theme);
    }
}

/// The bar and its window.
pub(super) struct CaptureBar {
    window: WindowId,
    origin: Point,
    layout: BarLayout,
    parts: Parts,
    mode: usize,
    /// The part whose tip is declared.
    tipped: Option<Part>,
    owed: Region,
}

impl CaptureBar {
    /// Show the bar with `mode` chosen; `None` where the heap refuses it.
    pub(super) fn open(
        mode: CaptureMode,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<Self> {
        let mode = CaptureMode::ALL
            .iter()
            .position(|each| *each == mode)
            .unwrap_or(0);
        let mut parts = Parts {
            close: IconButton::new(IconKind::Close, ControlRole::Neutral),
            modes: MODE_ICONS.map(|icon| IconButton::new(icon, ControlRole::Neutral)),
            options: Button::labelled("Options"),
            primary: primary_button(primary_label(mode)),
        };
        if let Some(chosen) = parts.modes.get_mut(mode) {
            mark(chosen, true);
        }
        let scale = compositor.scale();
        let area = shell.work_area(compositor);
        let (theme, mut artwork) = shell.chrome_theme_and_artwork();
        let layout = BarLayout::of(&parts.options, scale, theme);
        let origin = bottom_centre(area, layout.size, scale.scale_length(PLATE_MARGIN));
        let mut pixels = Surface::new(layout.size.0, layout.size.1)?;
        parts.render(&mut pixels, &layout, (scale, theme), &mut artwork);
        let (radius, blur) = (
            scale.scale_length(theme.metrics().popup_corner_radius),
            theme.backdrop_blur(),
        );
        let window = compositor.add_window(origin, pixels);
        compositor.set_corners(window, Corners::painted(radius));
        compositor.set_backdrop_blur(window, blur);
        compositor.set_casts_shadow(window, true);
        compositor.raise(window);
        Some(Self {
            window,
            origin,
            layout,
            parts,
            mode,
            tipped: None,
            owed: Region::new(),
        })
    }

    /// The bar's window.
    pub(super) const fn window(&self) -> WindowId {
        self.window
    }

    /// Take `event`: what a completed press asks for.
    pub(super) fn pointer(
        &mut self,
        event: &InputEvent,
        now_ns: u64,
        shell: &mut DesktopShell,
        compositor: &Compositor,
    ) -> Option<BarAction> {
        let local = match *event {
            InputEvent::PointerMoved { to } => {
                self.declare_tip(to, shell, compositor);
                shell.tooltip_pointer(to, now_ns, compositor);
                InputEvent::PointerMoved {
                    to: Point::new(
                        to.x.saturating_sub(self.origin.x),
                        to.y.saturating_sub(self.origin.y),
                    ),
                }
            }
            InputEvent::PointerPressed { .. } => {
                shell.dismiss_tooltip();
                *event
            }
            other => other,
        };
        let (layout, owed) = (&self.layout, &mut self.owed);
        let mut fired = None;
        if self
            .parts
            .close
            .on_pointer(&local, layout.close, owed)
            .is_some()
        {
            fired = Some(Part::Close);
        }
        for (index, (button, rect)) in self.parts.modes.iter_mut().zip(layout.modes).enumerate() {
            if button.on_pointer(&local, rect, owed).is_some() {
                fired = Some(Part::Mode(index));
            }
        }
        if self
            .parts
            .options
            .on_pointer(&local, layout.options, owed)
            .is_some()
        {
            fired = Some(Part::Options);
        }
        if self
            .parts
            .primary
            .on_pointer(&local, layout.primary, owed)
            .is_some()
        {
            fired = Some(Part::Primary);
        }
        fired.and_then(|part| self.fire(part))
    }

    /// Take `key`: Enter takes the capture, and Left and Right choose the
    /// mode beside the one chosen, as along a segmented control.
    pub(super) fn key(&mut self, key: &KeyInput) -> Option<BarAction> {
        let KeyInput::Pressed {
            key: Some(value), ..
        } = key
        else {
            return None;
        };
        let beside = match value {
            KeyValue::Named(NamedKeyCode::Enter) => return Some(BarAction::Commit),
            KeyValue::Named(NamedKeyCode::Left) => self.mode.checked_sub(1)?,
            KeyValue::Named(NamedKeyCode::Right) => self.mode.checked_add(1)?,
            _ => return None,
        };
        self.fire(Part::Mode(beside))
    }

    /// Repaint what the events since the last settle changed; what a repaint
    /// the heap refuses leaves owed is painted at the next settle.
    pub(super) fn settle(&mut self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        if self.owed.is_empty() {
            return;
        }
        let scale = compositor.scale();
        let (theme, mut artwork) = shell.chrome_theme_and_artwork();
        let Self {
            window,
            layout,
            parts,
            owed,
            ..
        } = self;
        let painted = compositor.repaint_window(*window, layout.size, owed, |surface, rects| {
            damage::paint_parts(surface, rects, |surface| {
                parts.render(surface, layout, (scale, theme), &mut artwork);
            });
        });
        if painted {
            owed.clear();
        }
    }

    /// Take the bar and any tip it shows down.
    pub(super) fn close(self, shell: &mut DesktopShell, compositor: &mut Compositor) {
        let _ = shell.forget_tooltip(self.window);
        let _ = shell.present_tooltip(compositor);
        compositor.remove(self.window);
    }

    /// Where `part`'s centre is on screen.
    #[cfg(test)]
    pub(super) fn part_centre(&self, part: Part, compositor: &Compositor) -> Option<Point> {
        compositor.window(self.window)?;
        let centre = self.layout.rect(part)?.center();
        Some(Point::new(
            centre.x.saturating_add(self.origin.x),
            centre.y.saturating_add(self.origin.y),
        ))
    }

    fn fire(&mut self, part: Part) -> Option<BarAction> {
        match part {
            Part::Close => Some(BarAction::Close),
            Part::Mode(index) => {
                let mode = *CaptureMode::ALL.get(index)?;
                self.choose(index);
                Some(BarAction::Mode(mode))
            }
            Part::Options => {
                let rect = self.layout.options;
                Some(BarAction::Options(Rect::new(
                    rect.left().saturating_add(self.origin.x),
                    rect.top().saturating_add(self.origin.y),
                    rect.width,
                    rect.height,
                )))
            }
            Part::Primary => Some(BarAction::Commit),
        }
    }

    /// Mark the mode at `index` chosen, and name the primary button for it.
    fn choose(&mut self, index: usize) {
        if index == self.mode {
            return;
        }
        for (place, marked) in [(self.mode, false), (index, true)] {
            if let (Some(button), Some(rect)) = (
                self.parts.modes.get_mut(place),
                self.layout.modes.get(place),
            ) {
                mark(button, marked);
                self.owed.add(*rect);
            }
        }
        let relabelled = primary_label(index) != primary_label(self.mode);
        self.mode = index;
        if relabelled {
            self.parts.primary = primary_button(primary_label(index));
            self.owed.add(self.layout.primary);
        }
    }

    /// Declare the tip of the part under `to`, or withdraw the bar's tip
    /// where no part is, when that changed.
    fn declare_tip(&mut self, to: Point, shell: &mut DesktopShell, compositor: &Compositor) {
        let local = Point::new(
            to.x.saturating_sub(self.origin.x),
            to.y.saturating_sub(self.origin.y),
        );
        let part = self.layout.part_at(local);
        if part == self.tipped {
            return;
        }
        self.tipped = part;
        let declared = part.and_then(|part| {
            let tip = part_tip(part)?;
            Some((tip_region(self.layout.rect(part)?)?, tip))
        });
        match declared {
            Some((region, tip)) => {
                let _ = shell.declare_tooltip(self.window, region, tip, compositor);
            }
            None => {
                let _ = shell.forget_tooltip(self.window);
            }
        }
    }
}

/// What `part` explains on dwell; the primary button's label says it all.
fn part_tip(part: Part) -> Option<&'static str> {
    match part {
        Part::Close => Some("Close"),
        Part::Mode(index) => MODE_TIPS.get(index).copied(),
        Part::Options => Some("Where captures are saved, the timer, and what they show"),
        Part::Primary => None,
    }
}

/// The primary button's label for the mode at `index`.
fn primary_label(index: usize) -> &'static str {
    match CaptureMode::ALL.get(index) {
        Some(mode) if mode.recording => RECORD_LABEL,
        _ => CAPTURE_LABEL,
    }
}

fn primary_button(label: &str) -> Button {
    Button::new(
        ButtonContent::Label(String::from(label)),
        ControlRole::Primary,
    )
}

/// Mark `button` as the chosen mode, or not.
fn mark(button: &mut IconButton, chosen: bool) {
    let selection = if chosen {
        SelectionState::Selected
    } else {
        SelectionState::Unselected
    };
    button.set_state(button.state().with_selection(selection));
}
