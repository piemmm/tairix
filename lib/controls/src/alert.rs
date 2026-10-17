//! [`Alert`]: the error, warning, notice or question the desktop puts to the
//! user in a window of its own (`plans/APPWIN.md` AW8).
//!
//! The control lays out and paints an alert window's client area — its
//! severity mark on the left, the headline and detail beside it, the buttons
//! along the foot at the right — and answers which button was chosen. It
//! measures the client extent its content needs: as wide as its text, up to
//! a readable measure, and exactly as tall as what it says, so nothing is
//! elided. Past the screen's share the whole text, headline included,
//! scrolls, and the buttons stay at the foot.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertSeverity, AlertSpec, ALERT_DETAIL_MAX, ALERT_HEADLINE_MAX,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_icon::IconKind;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::button::{Button, ButtonAction, ButtonContent};
use crate::paint::{
    grab_after, role_font, route_pointer, surface_rect, text_plate_height, to_i32, withheld,
    TextBlock,
};
use crate::scroll::{ScrollModel, ScrollOrientation, ScrollRange};
use crate::scrollbar::ScrollBar;
use crate::state::{ControlRole, RenderInvariant};

/// The narrowest an alert window is, frame included, in logical pixels.
pub const ALERT_MIN_WIDTH: u32 = 360;

/// The side of an alert's severity mark, in logical pixels.
pub const ALERT_MARK_SIDE: u32 = 48;

/// The narrowest an alert's button is, in logical pixels: a target as easy
/// to hit when it reads "OK" as when it reads "Replace".
pub const ALERT_BUTTON_MIN_WIDTH: u32 = 88;

/// The most of the screen an alert window takes on either axis, in
/// per-mille: room is left round it to see what it is about.
const SCREEN_SHARE_PERMILLE: u32 = 900;

/// The longest line an alert's text is set to, in ems of its body face:
/// about sixty characters, the length a line is read at without losing the
/// eye's place.
const MEASURE_EMS: u32 = 30;

/// How an alert was answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AlertAction {
    /// The button at `index`, counted from the left, was chosen.
    Chosen {
        /// The chosen button's index.
        index: usize,
    },
}

/// The alert a desktop window draws: what it says, the answers it offers,
/// and which of them holds the keyboard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Alert {
    severity: AlertSeverity,
    headline: String,
    detail: String,
    buttons: Vec<Button>,
    roles: Vec<AlertButtonRole>,
    focus: usize,
    /// How many of the text's lines, the headline's first, are scrolled
    /// past; zero unless it overflows.
    scroll: usize,
    pointer: RenderInvariant<Point>,
    hovered: RenderInvariant<Option<usize>>,
    armed: RenderInvariant<Option<usize>>,
}

/// Where each part of an alert sits within its client area.
struct Layout {
    mark: Rect,
    /// Where the text is drawn, from its first line in view.
    text: Rect,
    lines: TextLines,
    /// How far the text can scroll, in lines; zero when it all fits.
    scroll_room: usize,
    buttons: Vec<Rect>,
}

/// An alert's text as the lines it takes at one width: the headline's, then
/// the detail's, each in its own face, with a gap between them.
struct TextLines {
    headline: TextBlock,
    detail: TextBlock,
    headline_lines: usize,
    detail_lines: usize,
    gap: u32,
}

impl TextLines {
    fn of(alert: &Alert, width: u32, metrics: &Metrics, scale: Scale, theme: &Theme) -> Self {
        let headline = Alert::headline_block(width, scale, theme);
        let detail = Alert::detail_block(width, scale, theme);
        let headline_lines = headline.line_count(&alert.headline);
        let detail_lines = detail.line_count(&alert.detail);
        let gap = if headline_lines > 0 && detail_lines > 0 {
            metrics.gap
        } else {
            0
        };
        Self {
            headline,
            detail,
            headline_lines,
            detail_lines,
            gap,
        }
    }

    const fn total(&self) -> usize {
        self.headline_lines + self.detail_lines
    }

    /// What line `index` adds to a view whose first line is `first`: its own
    /// height, and the gap before the detail's first line when headline
    /// lines stand above it.
    fn height_of(&self, index: usize, first: usize) -> u32 {
        if index < self.headline_lines {
            return self.headline.font.line_height();
        }
        let gap = if index == self.headline_lines && first < self.headline_lines {
            self.gap
        } else {
            0
        };
        self.detail.font.line_height().saturating_add(gap)
    }

    /// The height of the whole text.
    fn height(&self) -> u32 {
        (0..self.total()).fold(0, |sum, index| sum.saturating_add(self.height_of(index, 0)))
    }

    /// How many of the headline's and the detail's lines fit `room` pixels
    /// with line `first` at the top.
    fn fitting(&self, first: usize, room: u32) -> (usize, usize) {
        let mut used = 0u32;
        let mut shown = (0, 0);
        for index in first..self.total() {
            used = used.saturating_add(self.height_of(index, first));
            if used > room {
                break;
            }
            if index < self.headline_lines {
                shown.0 += 1;
            } else {
                shown.1 += 1;
            }
        }
        shown
    }

    /// The lowest first line that still shows the text through its end in
    /// `room` pixels: how far it scrolls.
    fn scroll_room(&self, room: u32) -> usize {
        let mut used = 0u32;
        let mut first = self.total();
        while let Some(index) = first.checked_sub(1) {
            used = used.saturating_add(self.height_of(index, index));
            if index + 1 == self.headline_lines && self.detail_lines > 0 {
                used = used.saturating_add(self.gap);
            }
            if used > room {
                break;
            }
            first = index;
        }
        first
    }
}

/// The lengths an alert is laid out with at one scale.
struct Metrics {
    pad: u32,
    gap: u32,
    mark: u32,
    button_h: u32,
    button_min: u32,
}

impl Metrics {
    fn of(scale: Scale, theme: &Theme) -> Self {
        let inset = theme.metrics().control_inset;
        Self {
            pad: scale.scale_length(inset.saturating_mul(2)).max(1),
            gap: scale.scale_length(theme.metrics().control_gap).max(1),
            mark: scale.scale_length(ALERT_MARK_SIDE).max(1),
            button_h: text_plate_height(theme, scale, TextRole::Body),
            button_min: scale.scale_length(ALERT_BUTTON_MIN_WIDTH),
        }
    }

    /// The width the text column has in a client `width` pixels wide.
    fn text_width(&self, width: u32) -> u32 {
        width
            .saturating_sub(self.pad.saturating_mul(3))
            .saturating_sub(self.mark)
    }

    /// The client width whose text column is `text_w` pixels wide.
    fn client_for_text(&self, text_w: u32) -> u32 {
        text_w
            .saturating_add(self.pad.saturating_mul(3))
            .saturating_add(self.mark)
    }
}

impl Alert {
    /// The alert `spec` describes, its keyboard on the default button, else
    /// the cancel button, else the first that destroys nothing: a stray
    /// `Enter` never destroys.
    #[must_use]
    pub fn from_spec(spec: &AlertSpec) -> Self {
        let roles: Vec<AlertButtonRole> = spec.buttons().map(AlertButton::role).collect();
        let buttons = spec
            .buttons()
            .map(|button| {
                let role = match button.role() {
                    AlertButtonRole::Default => ControlRole::Recommended,
                    AlertButtonRole::Destructive => ControlRole::Destructive,
                    AlertButtonRole::Cancel | AlertButtonRole::Plain => ControlRole::Neutral,
                };
                Button::new(ButtonContent::Label(String::from(button.label())), role)
            })
            .collect();
        let first = |want: fn(AlertButtonRole) -> bool| roles.iter().position(|role| want(*role));
        let focus = first(|role| role == AlertButtonRole::Default)
            .or_else(|| first(|role| role == AlertButtonRole::Cancel))
            .or_else(|| first(|role| role != AlertButtonRole::Destructive))
            .unwrap_or(0);
        let mut alert = Self {
            severity: spec.severity(),
            headline: String::from(spec.headline()),
            detail: String::from(spec.detail()),
            buttons,
            roles,
            focus,
            scroll: 0,
            pointer: RenderInvariant::new(Point::ORIGIN),
            hovered: RenderInvariant::new(None),
            armed: RenderInvariant::new(None),
        };
        alert.show_focus();
        alert
    }

    /// What kind of thing the alert says.
    #[must_use]
    pub const fn severity(&self) -> AlertSeverity {
        self.severity
    }

    /// The headline.
    #[must_use]
    pub fn headline(&self) -> &str {
        &self.headline
    }

    /// The detail; empty when the headline says it all.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Say `detail` instead, keeping the focus and where the pointer is: a
    /// detail that counts down is one alert throughout.
    pub fn set_detail(&mut self, detail: &str) {
        self.detail.clear();
        self.detail.push_str(detail);
        self.scroll = 0;
    }

    /// The button holding the keyboard.
    #[must_use]
    pub const fn focused(&self) -> usize {
        self.focus
    }

    /// The kind the severity mark is drawn as.
    #[must_use]
    pub const fn mark_kind(&self) -> IconKind {
        match self.severity {
            AlertSeverity::Error => IconKind::AlertError,
            AlertSeverity::Warning => IconKind::AlertWarning,
            AlertSeverity::Information => IconKind::AlertInformation,
            AlertSeverity::Question => IconKind::AlertQuestion,
        }
    }

    /// The severity mark rasterised at `scale`, for the owner to keep and
    /// hand back to every [`render`](Self::render): it changes only with the
    /// scale, so it is drawn once, never per paint.
    #[must_use]
    pub fn mark(&self, scale: Scale) -> Option<Surface> {
        let kind = self.mark_kind();
        tairix_icon::badge(
            kind,
            kind.badge()?,
            kind.plate_shape(),
            scale.scale_length(ALERT_MARK_SIDE).max(1),
        )
    }

    /// The client extent of an alert window whose frame adds `frame` pixels
    /// across and down, on a screen `screen` pixels across and down: as wide
    /// as its text needs up to a readable measure, never narrower than
    /// [`ALERT_MIN_WIDTH`] or its button row, and exactly as tall as its
    /// content, each within its share of the screen.
    #[must_use]
    pub fn client_extent(
        &self,
        frame: (u32, u32),
        screen: (u32, u32),
        scale: Scale,
        theme: &Theme,
    ) -> (u32, u32) {
        let metrics = Metrics::of(scale, theme);
        let share = |side: u32| {
            u32::try_from(u64::from(side) * u64::from(SCREEN_SHARE_PERMILLE) / 1000).unwrap_or(side)
        };
        let text_w = self.text_width(Self::measure(scale, theme), scale, theme);
        let width = metrics
            .client_for_text(text_w)
            .saturating_add(frame.0)
            .max(scale.scale_length(ALERT_MIN_WIDTH))
            .max(
                self.row_width(&metrics, scale, theme)
                    .saturating_add(metrics.pad.saturating_mul(2))
                    .saturating_add(frame.0),
            )
            .min(share(screen.0));
        let client_w = width.saturating_sub(frame.0).max(1);
        let tallest = share(screen.1).saturating_sub(frame.1);
        let client_h = self
            .needed_height(client_w, &metrics, scale, theme)
            .min(tallest)
            .max(1);
        (client_w, client_h)
    }

    /// The readable measure: the longest line the text is set to.
    fn measure(scale: Scale, theme: &Theme) -> u32 {
        role_font(theme, scale, TextRole::Body)
            .pixel_height()
            .saturating_mul(MEASURE_EMS)
    }

    /// The width the text column needs when no line is longer than
    /// `measure`: its widest line.
    fn text_width(&self, measure: u32, scale: Scale, theme: &Theme) -> u32 {
        Self::headline_block(measure, scale, theme)
            .measured_width(&self.headline)
            .max(Self::detail_block(measure, scale, theme).measured_width(&self.detail))
    }

    /// Paint the alert's client area into `surface` at `bounds`, with `mark`
    /// the severity mark [`mark`](Self::mark) drew.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        mark: Option<&Surface>,
    ) {
        if withheld(surface, bounds) {
            return;
        }
        let Some((x, y, w, h)) = surface_rect(bounds) else {
            return;
        };
        let palette = theme.palette();
        surface.fill_rect(x, y, w, h, Color::from(palette.surface));
        let layout = self.layout(bounds, scale, theme);
        if let Some(mark) = mark {
            surface.blit(layout.mark.left(), layout.mark.top(), mark);
        }
        if let Some((x, y, _, h)) = surface_rect(layout.text) {
            let lines = &layout.lines;
            let first = self.scroll.min(layout.scroll_room);
            let (headline, detail) = lines.fitting(first, h);
            lines
                .headline
                .paint_window(surface, &self.headline, (x, y), first, headline);
            let below = u32::try_from(headline)
                .unwrap_or(u32::MAX)
                .saturating_mul(lines.headline.font.line_height());
            let gap = if headline > 0 { lines.gap } else { 0 };
            lines.detail.paint_window(
                surface,
                &self.detail,
                (x, y.saturating_add(below).saturating_add(gap)),
                first.saturating_sub(lines.headline_lines),
                detail,
            );
            if layout.scroll_room > 0 {
                let bar = Self::scrollbar_rect(&layout, scale, theme);
                self.scrollbar(&layout, headline + detail)
                    .render(surface, bar, scale, theme);
            }
        }
        for (button, rect) in self.buttons.iter().zip(&layout.buttons) {
            button.render(surface, *rect, scale, theme);
        }
    }

    /// Route a pointer event, with `bounds` the client area. A completed
    /// click on a button chooses it, and a wheel turn scrolls text that
    /// overflows. What changed is reported into `damage`.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<AlertAction> {
        if let InputEvent::PointerMoved { to } = event {
            *self.pointer = *to;
        }
        let layout = self.layout(bounds, scale, theme);
        if let InputEvent::PointerScrolled { dy, .. } = *event {
            // A detent is three lines, as a document scrolls; a part of one
            // that moves no whole line moves nothing.
            let lines =
                i64::from(dy) * 3 / i64::from(tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT);
            let bar = Self::scrollbar_rect(&layout, scale, theme);
            self.scroll_by(lines, &layout, bar, damage);
            return None;
        }
        let over = layout
            .buttons
            .iter()
            .position(|rect| rect.contains(*self.pointer));
        let route = route_pointer(&mut self.hovered, *self.armed, over);
        *self.armed = grab_after(*self.armed, event, over);
        let mut chosen = None;
        for index in route.into_iter().flatten() {
            let (Some(button), Some(rect)) =
                (self.buttons.get_mut(index), layout.buttons.get(index))
            else {
                continue;
            };
            if button.on_pointer(event, *rect, damage) == Some(ButtonAction::Activated)
                && chosen.is_none()
            {
                chosen = Some(AlertAction::Chosen { index });
            }
        }
        chosen
    }

    /// Feed a key press. `Enter` and `Space` press the focused button, and
    /// `Escape` the cancel one — or the only button there is, which the spec
    /// guarantees destroys nothing; `Tab`, `Shift+Tab` and the arrows move the
    /// focus along the row; the paging keys scroll text that overflows.
    pub fn on_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) -> Option<AlertAction> {
        match key {
            Key::Named(NamedKey::Enter) | Key::Char(' ') => {
                Some(AlertAction::Chosen { index: self.focus })
            }
            Key::Named(NamedKey::Escape) => self
                .roles
                .iter()
                .position(|role| *role == AlertButtonRole::Cancel)
                .or((self.buttons.len() == 1).then_some(0))
                .map(|index| AlertAction::Chosen { index }),
            Key::Named(NamedKey::Tab) if modifiers.shift => {
                self.step_focus(false, bounds, scale, theme, damage);
                None
            }
            Key::Named(NamedKey::Tab | NamedKey::Right) => {
                self.step_focus(true, bounds, scale, theme, damage);
                None
            }
            Key::Named(NamedKey::Left) => {
                self.step_focus(false, bounds, scale, theme, damage);
                None
            }
            Key::Named(named) => {
                let layout = self.layout(bounds, scale, theme);
                let (headline, detail) = layout
                    .lines
                    .fitting(self.scroll.min(layout.scroll_room), layout.text.height);
                let page = i64::try_from((headline + detail).max(1)).unwrap_or(1);
                let all = i64::try_from(layout.lines.total()).unwrap_or(i64::MAX);
                let by = match named {
                    NamedKey::Up => -1,
                    NamedKey::Down => 1,
                    NamedKey::PageUp => -page,
                    NamedKey::PageDown => page,
                    NamedKey::Home => -all,
                    NamedKey::End => all,
                    _ => return None,
                };
                let bar = Self::scrollbar_rect(&layout, scale, theme);
                self.scroll_by(by, &layout, bar, damage);
                None
            }
            Key::Char(_) => None,
        }
    }

    /// The buttons' rectangles within `bounds`, in spec order: what a host
    /// routing a press through its own hit test resolves against.
    #[must_use]
    pub fn button_rects(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Vec<Rect> {
        self.button_rects_with(bounds, &Metrics::of(scale, theme), scale, theme)
    }

    /// The width the button row needs: each button at its natural width or
    /// the minimum, whichever is wider, with the gaps between them.
    fn row_width(&self, metrics: &Metrics, scale: Scale, theme: &Theme) -> u32 {
        let gaps = metrics
            .gap
            .saturating_mul(u32::try_from(self.buttons.len().saturating_sub(1)).unwrap_or(0));
        self.buttons
            .iter()
            .map(|button| button.measured_width(scale, theme).max(metrics.button_min))
            .fold(gaps, u32::saturating_add)
    }

    /// The client height a client `width` pixels wide needs to seat the
    /// whole alert without scrolling.
    fn needed_height(&self, width: u32, metrics: &Metrics, scale: Scale, theme: &Theme) -> u32 {
        let text = TextLines::of(self, metrics.text_width(width), metrics, scale, theme);
        metrics
            .pad
            .saturating_mul(3)
            .saturating_add(text.height().max(metrics.mark))
            .saturating_add(self.rows_height(width, metrics, scale, theme))
    }

    /// The block the whole headline is laid out in: every line it takes.
    fn headline_block(width: u32, scale: Scale, theme: &Theme) -> TextBlock {
        TextBlock::prose(
            role_font(theme, scale, TextRole::ItemTitle),
            width,
            ALERT_HEADLINE_MAX,
            Color::from(theme.palette().on_surface),
        )
    }

    /// The block the whole detail is laid out in: every line it has, since
    /// what does not fit scrolls rather than being cut.
    fn detail_block(width: u32, scale: Scale, theme: &Theme) -> TextBlock {
        TextBlock::prose(
            role_font(theme, scale, TextRole::Body),
            width,
            ALERT_DETAIL_MAX,
            Color::from(theme.palette().on_surface_muted),
        )
    }

    /// Where every part sits in `bounds`. The one definition the paint, the
    /// hit test and the scroll all read.
    fn layout(&self, bounds: Rect, scale: Scale, theme: &Theme) -> Layout {
        let metrics = Metrics::of(scale, theme);
        let pad = to_i32(metrics.pad);
        let buttons = self.button_rects_with(bounds, &metrics, scale, theme);
        let button_top = buttons
            .iter()
            .map(Rect::top)
            .min()
            .unwrap_or(bounds.bottom() - pad);
        let content_top = bounds.top() + pad;
        let content_h = to_u32(button_top - pad - content_top);
        let text_w = metrics.text_width(bounds.width);
        let lines = TextLines::of(self, text_w, &metrics, scale, theme);
        let text_h = lines.height();
        let block_h = text_h.max(metrics.mark);
        let text_left = bounds.left() + pad + to_i32(metrics.mark) + pad;
        // A block that fits is centred in the room above the buttons, and text
        // shorter than the mark is centred on it, so a one-line alert reads
        // level with its mark; one that does not fit fills the room and
        // scrolls a line at a time.
        let (mark_top, text, scroll_room) = if block_h <= content_h {
            let block_top = content_top + to_i32((content_h - block_h) / 2);
            let text_top = block_top + to_i32(metrics.mark.saturating_sub(text_h) / 2);
            (block_top, Rect::new(text_left, text_top, text_w, text_h), 0)
        } else {
            (
                content_top,
                Rect::new(text_left, content_top, text_w, content_h),
                lines.scroll_room(content_h),
            )
        };
        Layout {
            mark: Rect::new(bounds.left() + pad, mark_top, metrics.mark, metrics.mark),
            text,
            lines,
            scroll_room,
            buttons,
        }
    }

    /// The buttons laid right to left from the client's far corner, a row at
    /// a time: one that would cross the near edge starts a row above, so a
    /// row wider than the window wraps rather than leaving the client.
    fn button_rects_with(
        &self,
        bounds: Rect,
        metrics: &Metrics,
        scale: Scale,
        theme: &Theme,
    ) -> Vec<Rect> {
        let (pad, gap, height) = (to_i32(metrics.pad), to_i32(metrics.gap), metrics.button_h);
        let (near, far) = (bounds.left() + pad, bounds.right() - pad);
        let room = to_u32(far - near);
        let mut top = bounds.bottom() - pad - to_i32(height);
        let mut right = far;
        let mut rects = Vec::with_capacity(self.buttons.len());
        for button in self.buttons.iter().rev() {
            let width = button
                .measured_width(scale, theme)
                .max(metrics.button_min)
                .min(room);
            if right - to_i32(width) < near && right != far {
                top -= to_i32(height) + gap;
                right = far;
            }
            let rect = Rect::new(right - to_i32(width), top, width, height);
            right = rect.left() - gap;
            rects.push(rect);
        }
        rects.reverse();
        rects
    }

    /// The height the button rows take in a client `width` wide.
    fn rows_height(&self, width: u32, metrics: &Metrics, scale: Scale, theme: &Theme) -> u32 {
        let rects = self.button_rects_with(Rect::new(0, 0, width, 0), metrics, scale, theme);
        let highest = rects.iter().map(Rect::top).min().unwrap_or(0);
        to_u32(-highest - to_i32(metrics.pad))
    }

    /// The bar beside text that overflows, `shown` of its lines in view, as
    /// a reading of where it is.
    fn scrollbar(&self, layout: &Layout, shown: usize) -> ScrollBar {
        let as_u64 = |lines: usize| u64::try_from(lines).unwrap_or(u64::MAX);
        let range = ScrollRange::new(
            as_u64(layout.scroll_room.saturating_add(shown)),
            as_u64(shown),
            as_u64(self.scroll.min(layout.scroll_room)),
        );
        ScrollBar::new(ScrollOrientation::Vertical, ScrollModel::new(range, 1, 1))
    }

    /// Where the bar sits: just past the text's right edge, inside the pad.
    fn scrollbar_rect(layout: &Layout, scale: Scale, theme: &Theme) -> Rect {
        let breadth = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        Rect::new(
            layout.text.right() + to_i32(Metrics::of(scale, theme).gap / 2),
            layout.text.top(),
            breadth,
            layout.text.height,
        )
    }

    /// Scroll the text by `by` lines, held within what it holds, damaging
    /// the text and its bar `bar`.
    fn scroll_by(&mut self, by: i64, layout: &Layout, bar: Rect, damage: &mut Region) {
        let most = i64::try_from(layout.scroll_room).unwrap_or(i64::MAX);
        let held = i64::try_from(self.scroll).unwrap_or(i64::MAX);
        let next = usize::try_from(held.saturating_add(by).clamp(0, most)).unwrap_or(0);
        if next != self.scroll {
            self.scroll = next;
            damage.add(layout.text);
            damage.add(bar);
        }
    }

    /// Move the focus one button along the row, wrapping at its ends.
    fn step_focus(
        &mut self,
        forward: bool,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        damage: &mut Region,
    ) {
        let count = self.buttons.len();
        if count < 2 {
            return;
        }
        let rects = self.button_rects_with(bounds, &Metrics::of(scale, theme), scale, theme);
        if let Some(rect) = rects.get(self.focus) {
            damage.add(*rect);
        }
        self.focus = if forward {
            (self.focus + 1) % count
        } else {
            (self.focus + count - 1) % count
        };
        if let Some(rect) = rects.get(self.focus) {
            damage.add(*rect);
        }
        self.show_focus();
    }

    /// Ring the focused button and only it.
    fn show_focus(&mut self) {
        for (index, button) in self.buttons.iter_mut().enumerate() {
            button.set_focused(index == self.focus);
        }
    }
}

fn to_u32(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

#[cfg(test)]
#[path = "alert_tests.rs"]
mod tests;
