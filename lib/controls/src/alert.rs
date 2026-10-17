//! [`Alert`]: the error, warning, notice or question the desktop puts to the
//! user in a window of its own (`plans/APPWIN.md` AW8).
//!
//! The control lays out and paints an alert window's client area — its
//! severity mark on the left, the headline and detail beside it, the buttons
//! along the foot at the right — and answers which button was chosen. It
//! measures the client extent that makes the whole window, frame included,
//! exactly 4:3 and large enough that nothing it says is cut. Where even the
//! largest such window the screen holds is too small, the detail scrolls.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertSeverity, AlertSpec, ALERT_DETAIL_MAX,
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

/// The most lines a headline wraps to. It is one sentence of at most
/// [`ALERT_HEADLINE_MAX`](tairix_abi::window_ipc::ALERT_HEADLINE_MAX) bytes.
const HEADLINE_LINES: usize = 6;

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
    /// How many of the detail's lines are scrolled past; zero unless it
    /// overflows.
    scroll: usize,
    pointer: RenderInvariant<Point>,
    hovered: RenderInvariant<Option<usize>>,
    armed: RenderInvariant<Option<usize>>,
}

/// Where each part of an alert sits within its client area.
struct Layout {
    mark: Rect,
    headline: Rect,
    /// The part of the detail on screen: a whole number of its lines.
    detail: Rect,
    /// How many lines the whole detail takes, and how many are on screen.
    detail_lines: usize,
    shown_lines: usize,
    buttons: Vec<Rect>,
}

impl Layout {
    /// How far the detail can scroll, in lines.
    const fn scroll_room(&self) -> usize {
        self.detail_lines.saturating_sub(self.shown_lines)
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
    /// across and down, on a screen `screen` pixels across and down: the
    /// smallest that makes the whole window exactly 4:3 and seats everything
    /// the alert says, never wider or taller than its share of the screen.
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
        let widest = share(screen.0).min(four_thirds(share(screen.1))).max(1);
        let narrowest = scale
            .scale_length(ALERT_MIN_WIDTH)
            .max(
                self.row_width(&metrics, scale, theme)
                    .saturating_add(metrics.pad.saturating_mul(2))
                    .saturating_add(frame.0),
            )
            .min(widest);
        let fits = |width: u32| {
            let (client_w, client_h) = client_of(width, frame);
            self.needed_height(client_w, &metrics, scale, theme) <= client_h
        };
        let (mut low, mut high) = (narrowest, widest);
        if !fits(high) {
            return client_of(high, frame);
        }
        while low < high {
            let mid = low + (high - low) / 2;
            if fits(mid) {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        client_of(low, frame)
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
        if let Some((hx, hy, hw, _)) = surface_rect(layout.headline) {
            TextBlock::prose(
                role_font(theme, scale, TextRole::ItemTitle),
                hw,
                HEADLINE_LINES,
                Color::from(palette.on_surface),
            )
            .paint(surface, &self.headline, (hx, hy));
        }
        if let Some((dx, dy, dw, _)) = surface_rect(layout.detail) {
            Self::detail_block(dw, scale, theme).paint_window(
                surface,
                &self.detail,
                (dx, dy),
                self.scroll,
                layout.shown_lines,
            );
            if layout.scroll_room() > 0 {
                let bar = Self::scrollbar_rect(&layout, scale, theme);
                self.scrollbar(&layout).render(surface, bar, scale, theme);
            }
        }
        for (button, rect) in self.buttons.iter().zip(&layout.buttons) {
            button.render(surface, *rect, scale, theme);
        }
    }

    /// Route a pointer event, with `bounds` the client area. A completed
    /// click on a button chooses it, and a wheel turn scrolls a detail that
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
    /// focus along the row; the paging keys scroll a detail that overflows.
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
                let page = i64::try_from(layout.shown_lines.max(1)).unwrap_or(1);
                let all = i64::try_from(layout.detail_lines).unwrap_or(i64::MAX);
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
        let text_w = metrics.text_width(width);
        metrics
            .pad
            .saturating_mul(3)
            .saturating_add(
                self.text_height(text_w, metrics, scale, theme)
                    .max(metrics.mark),
            )
            .saturating_add(self.rows_height(width, metrics, scale, theme))
    }

    /// The headline and the whole detail, with the gap between them.
    fn text_height(&self, text_w: u32, metrics: &Metrics, scale: Scale, theme: &Theme) -> u32 {
        let detail = Self::detail_block(text_w, scale, theme).height(&self.detail);
        let gap = if detail == 0 { 0 } else { metrics.gap };
        self.headline_height(text_w, scale, theme)
            .saturating_add(gap)
            .saturating_add(detail)
    }

    fn headline_height(&self, text_w: u32, scale: Scale, theme: &Theme) -> u32 {
        TextBlock::prose(
            role_font(theme, scale, TextRole::ItemTitle),
            text_w,
            HEADLINE_LINES,
            Color::from(theme.palette().on_surface),
        )
        .height(&self.headline)
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
        let headline_h = self.headline_height(text_w, scale, theme);
        let block = Self::detail_block(text_w, scale, theme);
        let line = block.font.line_height().max(1);
        let detail_lines = block.line_count(&self.detail);
        let gap = if detail_lines == 0 { 0 } else { metrics.gap };
        let full_h = line.saturating_mul(u32::try_from(detail_lines).unwrap_or(u32::MAX));
        let text_h = headline_h.saturating_add(gap).saturating_add(full_h);
        let block_h = text_h.max(metrics.mark);
        // A block that fits is centred in the room above the buttons; one that
        // does not starts at the top and its detail scrolls a line at a time.
        let (block_top, shown_lines) = if block_h <= content_h {
            (
                content_top + to_i32((content_h - block_h) / 2),
                detail_lines,
            )
        } else {
            let room = content_h.saturating_sub(headline_h).saturating_sub(gap);
            (
                content_top,
                usize::try_from(room / line).unwrap_or(0).min(detail_lines),
            )
        };
        // Text shorter than the mark is centred on it, so a one-line alert
        // reads level with its mark rather than hanging from its top.
        let text_top = block_top + to_i32(metrics.mark.saturating_sub(text_h) / 2);
        let text_left = bounds.left() + pad + to_i32(metrics.mark) + pad;
        let shown_h = line.saturating_mul(u32::try_from(shown_lines).unwrap_or(u32::MAX));
        Layout {
            mark: Rect::new(bounds.left() + pad, block_top, metrics.mark, metrics.mark),
            headline: Rect::new(text_left, text_top, text_w, headline_h),
            detail: Rect::new(
                text_left,
                text_top + to_i32(headline_h) + to_i32(gap),
                text_w,
                shown_h,
            ),
            detail_lines,
            shown_lines,
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

    /// The bar beside a detail that overflows, as a reading of where it is.
    fn scrollbar(&self, layout: &Layout) -> ScrollBar {
        let as_u64 = |lines: usize| u64::try_from(lines).unwrap_or(u64::MAX);
        let range = ScrollRange::new(
            as_u64(layout.detail_lines),
            as_u64(layout.shown_lines),
            as_u64(self.scroll),
        );
        ScrollBar::new(ScrollOrientation::Vertical, ScrollModel::new(range, 1, 1))
    }

    /// Where the bar sits: just past the detail's right edge, inside the pad.
    fn scrollbar_rect(layout: &Layout, scale: Scale, theme: &Theme) -> Rect {
        let breadth = scale.scale_length(theme.metrics().scrollbar_breadth).max(1);
        Rect::new(
            layout.detail.right() + to_i32(Metrics::of(scale, theme).gap / 2),
            layout.detail.top(),
            breadth,
            layout.detail.height,
        )
    }

    /// Scroll the detail by `by` lines, held within what it holds, damaging
    /// the detail and its bar `bar`.
    fn scroll_by(&mut self, by: i64, layout: &Layout, bar: Rect, damage: &mut Region) {
        let most = i64::try_from(layout.scroll_room()).unwrap_or(i64::MAX);
        let held = i64::try_from(self.scroll).unwrap_or(i64::MAX);
        let next = usize::try_from(held.saturating_add(by).clamp(0, most)).unwrap_or(0);
        if next != self.scroll {
            self.scroll = next;
            damage.add(Rect::new(
                layout.detail.left(),
                layout.detail.top(),
                layout.detail.width,
                layout.detail.height,
            ));
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

/// Four thirds of `height`: the width of the 4:3 box that tall.
fn four_thirds(height: u32) -> u32 {
    u32::try_from(u64::from(height) * 4 / 3).unwrap_or(u32::MAX)
}

/// The client extent of a 4:3 window `width` pixels wide whose frame adds
/// `frame` across and down.
fn client_of(width: u32, frame: (u32, u32)) -> (u32, u32) {
    let height = u32::try_from(u64::from(width) * 3 / 4).unwrap_or(u32::MAX);
    (
        width.saturating_sub(frame.0).max(1),
        height.saturating_sub(frame.1).max(1),
    )
}

fn to_u32(value: i32) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

#[cfg(test)]
#[path = "alert_tests.rs"]
mod tests;
