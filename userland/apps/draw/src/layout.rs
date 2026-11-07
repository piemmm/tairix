//! Where everything a Draw window shows goes.
//!
//! The tool box down the left; the tool's own bar across the top; rulers
//! along the canvas's top and left edges and scrollbars along its right and
//! bottom; the inspector down the right with its tabs above its panel; the
//! status band along the bottom. Every length is a logical one scaled to
//! device pixels once, here.

use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_theme::{TextRole, Theme};

/// The size a new window opens at, in logical pixels.
pub const WINDOW_SIZE: (u32, u32) = (1080, 720);

const TOOL_BOX: u32 = 44;
const BAR: u32 = 34;
const RULER: u32 = 18;
const INSPECTOR: u32 = 256;
const TABS: u32 = 28;
const STATUS: u32 = 22;
const LEAST_CANVAS: u32 = 160;

/// The faces a window's text is set in.
#[derive(Copy, Clone, Debug)]
pub struct Faces {
    /// The status band and the rulers' figures.
    pub status: BitmapFont,
    /// Labels and buttons.
    pub label: BitmapFont,
    /// Section headings.
    pub heading: BitmapFont,
}

impl Faces {
    /// The faces `theme` sets these roles in at `scale`.
    #[must_use]
    pub fn of(theme: &Theme, scale: Scale) -> Self {
        let face = |role| BitmapFont::for_role(theme.fonts(), role, scale);
        Self {
            status: face(TextRole::Caption),
            label: face(TextRole::Body),
            heading: face(TextRole::SectionHeader),
        }
    }
}

/// How broad a scrollbar is in `theme` at `scale`, in device pixels.
fn scrollbar(scale: Scale, theme: &Theme) -> u32 {
    scale.scale_length(theme.metrics().scrollbar_breadth).max(1)
}

/// A window's parts, in device pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The whole window.
    pub window: Rect,
    /// The tool box.
    pub tools: Rect,
    /// The tool's own bar.
    pub bar: Rect,
    /// The ruler along the canvas's top.
    pub top_ruler: Rect,
    /// The ruler down the canvas's left.
    pub left_ruler: Rect,
    /// Where the rulers meet.
    pub corner: Rect,
    /// The canvas's scrollbar across.
    pub horizontal_bar: Rect,
    /// The canvas's scrollbar down.
    pub vertical_bar: Rect,
    /// Where the scrollbars meet.
    pub bar_corner: Rect,
    /// The canvas.
    pub canvas: Rect,
    /// The inspector's tabs.
    pub tabs: Rect,
    /// The inspector's panel.
    pub panel: Rect,
    /// The status band.
    pub status: Rect,
    /// One logical pixel, in device pixels, as a ratio.
    pub ratio: u32,
}

/// A length in device pixels as an offset between coordinates.
pub(crate) fn px(length: u32) -> i32 {
    i32::try_from(length).unwrap_or(i32::MAX)
}

impl Layout {
    /// The layout of a window `width` by `height` device pixels, drawn in
    /// `theme` at `scale`.
    #[must_use]
    pub fn of(width: u32, height: u32, scale: Scale, theme: &Theme) -> Self {
        let length = |logical: u32| scale.scale_length(logical);
        let breadth = scrollbar(scale, theme);
        let mut rest = Rect::new(0, 0, width, height);
        let status = rest.take_bottom(length(STATUS).min(rest.height));
        let bar = rest.take_top(length(BAR).min(rest.height));
        let tools = rest.take_left(length(TOOL_BOX).min(rest.width));
        let inspector_width =
            length(INSPECTOR).min(rest.width.saturating_sub(length(LEAST_CANVAS)));
        let mut inspector = rest.take_right(inspector_width);
        let tabs = inspector.take_top(length(TABS).min(inspector.height));
        let panel = inspector;
        let mut ruled = rest;
        let mut vertical_bar = ruled.take_right(breadth.min(ruled.width));
        let horizontal_bar = ruled.take_bottom(breadth.min(ruled.height));
        let bar_corner = vertical_bar.take_bottom(horizontal_bar.height);
        let mut top_ruler = ruled.take_top(length(RULER).min(ruled.height));
        let left_ruler = ruled.take_left(length(RULER).min(ruled.width));
        let corner = top_ruler.take_left(left_ruler.width);
        Self {
            window: Rect::new(0, 0, width, height),
            tools,
            bar,
            top_ruler,
            left_ruler,
            corner,
            horizontal_bar,
            vertical_bar,
            bar_corner,
            canvas: ruled,
            tabs,
            panel,
            status,
            ratio: length(100),
        }
    }

    /// The smallest window a layout fits, in device pixels.
    #[must_use]
    pub fn least(scale: Scale, theme: &Theme) -> (u32, u32) {
        let length = |logical: u32| scale.scale_length(logical);
        let breadth = scrollbar(scale, theme);
        (
            length(TOOL_BOX + RULER + LEAST_CANVAS + INSPECTOR) + breadth,
            length(BAR + RULER + LEAST_CANVAS + STATUS) + breadth,
        )
    }

    /// `logical` pixels in device pixels.
    #[must_use]
    pub const fn length(&self, logical: u32) -> u32 {
        logical * self.ratio / 100
    }
}

#[cfg(test)]
mod tests {
    use tairix_geometry::Scale;
    use tairix_theme::Theme;

    use super::Layout;

    #[test]
    fn the_parts_tile_the_window_without_overlapping() {
        let scale = Scale::from_percent(150).expect("a scale");
        let theme = Theme::light();
        let layout = Layout::of(1600, 1000, scale, &theme);
        let parts = [
            layout.tools,
            layout.bar,
            layout.top_ruler,
            layout.left_ruler,
            layout.corner,
            layout.horizontal_bar,
            layout.vertical_bar,
            layout.bar_corner,
            layout.canvas,
            layout.tabs,
            layout.panel,
            layout.status,
        ];
        let area: u64 = parts
            .iter()
            .map(|part| u64::from(part.width) * u64::from(part.height))
            .sum();
        assert_eq!(area, 1600 * 1000);
        for (index, a) in parts.iter().enumerate() {
            for b in &parts[index + 1..] {
                assert!(a.intersection(b).is_empty(), "{a:?} meets {b:?}");
            }
        }
        assert!(layout.canvas.width > layout.panel.width);
        let (width, height) = Layout::least(scale, &theme);
        let small = Layout::of(width, height, scale, &theme);
        assert!(small.canvas.width > 0 && small.canvas.height > 0);
    }
}
