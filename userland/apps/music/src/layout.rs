//! The player's window geometry: the one function every painter, every
//! hit-test, and every test agrees on.
//!
//! ```text
//! +--------------------------------------------------------------+
//! | +------+  Title                                         L R  |
//! | | art  |  Artist — Album                                | |  |
//! | |      |  FLAC, 16-bit, 44100 Hz, stereo                | |  |
//! | +------+  0:42 [========o-------------------] 3:20     | |  |
//! | [|<] [>] [>|]  [shuffle] [repeat]          [vol] --o----     |
//! +--------------------------------------------------------------+
//! |  #  Title                       Artist               Time  ^ |
//! |  1  ...                                                    | |
//! +--------------------------------------------------------------+
//! | status                                                       |
//! +--------------------------------------------------------------+
//! ```
//!
//! Every extent is derived from the active theme's metrics at the desktop UI
//! scale and from the text face's own line height, never a pixel constant a
//! denser theme or a larger scale would leave wrong. The now-playing band,
//! the transport and the status line are claimed first, so however small the
//! window becomes the transport stays reachable and only the playlist gives
//! up room. Every region is total: one with no room yields an empty
//! rectangle, which every painter and hit-test treats as absent.

use core::ops::Range;

use tairix_font::BitmapFont;
use tairix_geometry::{Rect, Scale};
use tairix_theme::Theme;

/// The widest a volume slider is drawn, in logical pixels.
const VOLUME_WIDTH: u32 = 120;

/// The narrowest seek slider worth drawing, in logical pixels: below this a
/// position cannot be aimed at.
const MIN_SEEK_WIDTH: u32 = 48;

/// How many lines of text stand beside the album art: title, artist and
/// album, format.
const ART_LINES: u32 = 3;

/// The share of the free columns the title takes, in fifths; the artist has
/// the rest.
const TITLE_SHARE: u32 = 3;

/// The playlist's columns: number, title, artist, time.
pub const COLUMNS: usize = 4;

/// The widest clock the transport or a row shows.
const CLOCK_TEXT: &str = "88:88:88";

/// The widest track number a row shows.
const NUMBER_TEXT: &str = "8888";

/// The extents every band is claimed in, resolved once from the theme's
/// metrics at the desktop scale and the text face's own line height.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Metrics {
    gap: u32,
    line: u32,
    row: u32,
    bar: u32,
    inset: u32,
}

impl Metrics {
    fn resolve(theme: &Theme, scale: Scale, font: BitmapFont) -> Self {
        let metrics = theme.metrics();
        let gap = scale.scale_length(metrics.control_gap).max(1);
        let line = font.line_height().max(1);
        Self {
            gap,
            line,
            row: scale.scale_length(metrics.control_height).max(line),
            bar: scale.scale_length(metrics.scrollbar_breadth).max(1),
            inset: gap.saturating_mul(2),
        }
    }

    /// The album art's side: the text lines beside it and the seek row.
    fn art(&self) -> u32 {
        self.line
            .saturating_mul(ART_LINES)
            .saturating_add(self.row)
            .saturating_add(self.gap.saturating_mul(ART_LINES))
    }
}

/// The player's resolved window geometry.
///
/// Built by [`Layout::for_window`] and read unchanged by the painter and by
/// every hit-test, so what the listener sees and what a click lands on can
/// never disagree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    window: Rect,
    art: Rect,
    title: Rect,
    subtitle: Rect,
    format: Rect,
    elapsed: Rect,
    seek: Rect,
    total: Rect,
    meters: Rect,
    previous: Rect,
    play: Rect,
    next: Rect,
    shuffle: Rect,
    repeat: Rect,
    volume_icon: Rect,
    volume: Rect,
    header: Rect,
    rows: Rect,
    scrollbar: Rect,
    status: Rect,
    columns: [u32; COLUMNS],
    row_pitch: u32,
}

impl Layout {
    /// Resolve the geometry of a `width`×`height` client area for the active
    /// theme and UI scale, with `font` the face the text is set in.
    #[must_use]
    pub fn for_window(
        width: u32,
        height: u32,
        theme: &Theme,
        scale: Scale,
        font: BitmapFont,
    ) -> Self {
        let m = Metrics::resolve(theme, scale, font);
        let window = Rect::new(0, 0, width, height);
        let band_h = m
            .art()
            .saturating_add(m.inset.saturating_mul(2))
            .min(height);
        let transport_h = m
            .row
            .saturating_add(m.inset)
            .min(height.saturating_sub(band_h));
        let status_h = m
            .line
            .saturating_add(m.gap)
            .min(height.saturating_sub(band_h).saturating_sub(transport_h));
        let status_top = height.saturating_sub(status_h);
        let band = now_playing(&m, window, font, scale);
        let row = transport(
            &m,
            window,
            band_h.saturating_add(m.gap.min(transport_h)),
            scale,
        );
        let list = playlist(
            &m,
            window,
            band_h.saturating_add(transport_h),
            status_top,
            font,
        );
        Self {
            window,
            art: band.art,
            title: band.title,
            subtitle: band.subtitle,
            format: band.format,
            elapsed: band.elapsed,
            seek: band.seek,
            total: band.total,
            meters: band.meters,
            previous: row.previous,
            play: row.play,
            next: row.next,
            shuffle: row.shuffle,
            repeat: row.repeat,
            volume_icon: row.volume_icon,
            volume: row.volume,
            header: list.header,
            rows: list.rows,
            scrollbar: list.scrollbar,
            status: clip(Rect::new(0, at(status_top), width, status_h), window),
            columns: list.columns,
            row_pitch: m.row,
        }
    }

    /// The whole client area.
    #[must_use]
    pub const fn window(&self) -> Rect {
        self.window
    }

    /// The album art's square.
    #[must_use]
    pub const fn art(&self) -> Rect {
        self.art
    }

    /// The title line.
    #[must_use]
    pub const fn title(&self) -> Rect {
        self.title
    }

    /// The artist and album line.
    #[must_use]
    pub const fn subtitle(&self) -> Rect {
        self.subtitle
    }

    /// The format line.
    #[must_use]
    pub const fn format(&self) -> Rect {
        self.format
    }

    /// The clock of the place being heard.
    #[must_use]
    pub const fn elapsed(&self) -> Rect {
        self.elapsed
    }

    /// The seek slider.
    #[must_use]
    pub const fn seek(&self) -> Rect {
        self.seek
    }

    /// The clock of the track's length.
    #[must_use]
    pub const fn total(&self) -> Rect {
        self.total
    }

    /// The peak meters, one bar a channel side by side.
    #[must_use]
    pub const fn meters(&self) -> Rect {
        self.meters
    }

    /// The previous-track button.
    #[must_use]
    pub const fn previous(&self) -> Rect {
        self.previous
    }

    /// The play and pause button.
    #[must_use]
    pub const fn play(&self) -> Rect {
        self.play
    }

    /// The next-track button.
    #[must_use]
    pub const fn next(&self) -> Rect {
        self.next
    }

    /// The shuffle toggle.
    #[must_use]
    pub const fn shuffle(&self) -> Rect {
        self.shuffle
    }

    /// The repeat control.
    #[must_use]
    pub const fn repeat(&self) -> Rect {
        self.repeat
    }

    /// The speaker beside the volume slider.
    #[must_use]
    pub const fn volume_icon(&self) -> Rect {
        self.volume_icon
    }

    /// The volume slider.
    #[must_use]
    pub const fn volume(&self) -> Rect {
        self.volume
    }

    /// The playlist's column titles.
    #[must_use]
    pub const fn header(&self) -> Rect {
        self.header
    }

    /// The playlist's rows.
    #[must_use]
    pub const fn rows(&self) -> Rect {
        self.rows
    }

    /// The playlist's scroll bar.
    #[must_use]
    pub const fn scrollbar(&self) -> Rect {
        self.scrollbar
    }

    /// The status line.
    #[must_use]
    pub const fn status(&self) -> Rect {
        self.status
    }

    /// The playlist's column widths: number, title, artist, time.
    #[must_use]
    pub const fn columns(&self) -> &[u32; COLUMNS] {
        &self.columns
    }

    /// One playlist row's height.
    #[must_use]
    pub const fn row_pitch(&self) -> u32 {
        self.row_pitch
    }

    /// The rectangle of the playlist's row `index`, the list scrolled down by
    /// `scroll` pixels; empty when it is out of sight.
    #[must_use]
    pub fn row(&self, index: usize, scroll: u32) -> Rect {
        let pitch = i64::from(self.row_pitch);
        let top = i64::from(self.rows.top())
            .saturating_add(
                i64::try_from(index)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(pitch),
            )
            .saturating_sub(i64::from(scroll));
        let Ok(top) = i32::try_from(top) else {
            return Rect::EMPTY;
        };
        Rect::new(self.rows.left(), top, self.rows.width, self.row_pitch).intersection(&self.rows)
    }

    /// The playlist row a point at height `y` falls on, the list scrolled down
    /// by `scroll` pixels.
    #[must_use]
    pub fn row_at(&self, y: i32, scroll: u32) -> Option<usize> {
        if y < self.rows.top() || y >= self.rows.bottom() || self.row_pitch == 0 {
            return None;
        }
        let into = u64::try_from(y - self.rows.top()).ok()? + u64::from(scroll);
        usize::try_from(into / u64::from(self.row_pitch)).ok()
    }

    /// The playlist rows any part of which is in sight, the list scrolled
    /// down by `scroll` pixels.
    #[must_use]
    pub fn visible_rows(&self, scroll: u32) -> Range<usize> {
        if self.row_pitch == 0 || self.rows.is_empty() {
            return 0..0;
        }
        let pitch = u64::from(self.row_pitch);
        let first = u64::from(scroll) / pitch;
        let end = (u64::from(scroll) + u64::from(self.rows.height)).div_ceil(pitch);
        let index = |row: u64| usize::try_from(row).unwrap_or(usize::MAX);
        index(first)..index(end)
    }
}

/// The now-playing band's parts.
struct Band {
    art: Rect,
    title: Rect,
    subtitle: Rect,
    format: Rect,
    elapsed: Rect,
    seek: Rect,
    total: Rect,
    meters: Rect,
}

/// The album art, the three lines beside it, the seek row under them and the
/// meters at the far edge.
fn now_playing(m: &Metrics, window: Rect, font: BitmapFont, scale: Scale) -> Band {
    let art_side = m.art();
    let art = clip(
        Rect::new(at(m.inset), at(m.inset), art_side, art_side),
        window,
    );
    let meter_breadth = (m.bar / 2).max(3);
    let meters_w = meter_breadth.saturating_mul(2).saturating_add(m.gap);
    let meters_x = window
        .width
        .saturating_sub(m.inset)
        .saturating_sub(meters_w);
    let meters = clip(
        Rect::new(at(meters_x), at(m.inset), meters_w, art_side),
        window,
    );
    let text_x = art.right().max(0).unsigned_abs().saturating_add(m.inset);
    let text_w = meters_x.saturating_sub(m.inset).saturating_sub(text_x);
    let line_at = |index: u32| {
        let top = m
            .inset
            .saturating_add(index.saturating_mul(m.line.saturating_add(m.gap)));
        clip(Rect::new(at(text_x), at(top), text_w, m.line), window)
    };
    let seek_top = m
        .inset
        .saturating_add(ART_LINES.saturating_mul(m.line.saturating_add(m.gap)));
    let clock_w = font.text_width(CLOCK_TEXT);
    let seek_w = text_w
        .saturating_sub(clock_w.saturating_mul(2))
        .saturating_sub(m.gap.saturating_mul(2));
    let (elapsed, seek, total) = if seek_w >= scale.scale_length(MIN_SEEK_WIDTH) {
        let seek_x = text_x.saturating_add(clock_w).saturating_add(m.gap);
        let total_x = seek_x.saturating_add(seek_w).saturating_add(m.gap);
        (
            clip(Rect::new(at(text_x), at(seek_top), clock_w, m.row), window),
            clip(Rect::new(at(seek_x), at(seek_top), seek_w, m.row), window),
            clip(Rect::new(at(total_x), at(seek_top), clock_w, m.row), window),
        )
    } else {
        (Rect::EMPTY, Rect::EMPTY, Rect::EMPTY)
    };
    Band {
        art,
        title: line_at(0),
        subtitle: line_at(1),
        format: line_at(2),
        elapsed,
        seek,
        total,
        meters,
    }
}

/// The transport row's parts.
struct Transport {
    previous: Rect,
    play: Rect,
    next: Rect,
    shuffle: Rect,
    repeat: Rect,
    volume_icon: Rect,
    volume: Rect,
}

/// The three transport buttons, the two mode toggles a gap apart, and the
/// volume at the far edge, along the row at `top`.
fn transport(m: &Metrics, window: Rect, top: u32, scale: Scale) -> Transport {
    // The mode toggles stand a further inset apart from the transport.
    let square = |slot: u32, apart: u32| {
        let x = m
            .inset
            .saturating_add(slot.saturating_mul(m.row.saturating_add(m.gap)))
            .saturating_add(apart);
        clip(Rect::new(at(x), at(top), m.row, m.row), window)
    };
    let repeat = square(4, m.inset);
    let room = window
        .width
        .saturating_sub(repeat.right().max(0).unsigned_abs())
        .saturating_sub(
            m.row
                .saturating_add(m.gap.saturating_mul(2))
                .saturating_add(m.inset),
        );
    let volume_w = scale.scale_length(VOLUME_WIDTH).min(room);
    let volume_x = window
        .width
        .saturating_sub(m.inset)
        .saturating_sub(volume_w);
    let volume = clip(Rect::new(at(volume_x), at(top), volume_w, m.row), window);
    let volume_icon = if volume.is_empty() {
        Rect::EMPTY
    } else {
        let x = volume_x.saturating_sub(m.gap).saturating_sub(m.row);
        clip(Rect::new(at(x), at(top), m.row, m.row), window)
    };
    Transport {
        previous: square(0, 0),
        play: square(1, 0),
        next: square(2, 0),
        shuffle: square(3, m.inset),
        repeat,
        volume_icon,
        volume,
    }
}

/// The playlist's parts.
struct List {
    header: Rect,
    rows: Rect,
    scrollbar: Rect,
    columns: [u32; COLUMNS],
}

/// The column titles, the rows and their scroll bar between `top` and
/// `bottom`, and the columns' widths.
fn playlist(m: &Metrics, window: Rect, top: u32, bottom: u32, font: BitmapFont) -> List {
    let list_h = bottom.saturating_sub(top);
    let header_h = m.row.min(list_h);
    let list_w = window.width.saturating_sub(m.bar);
    let rows_top = top.saturating_add(header_h);
    let rows_h = list_h.saturating_sub(header_h);
    let number_w = font.text_width(NUMBER_TEXT).saturating_add(m.gap);
    let time_w = font.text_width(CLOCK_TEXT).saturating_add(m.gap);
    let free = list_w
        .saturating_sub(number_w)
        .saturating_sub(time_w)
        .saturating_sub(m.inset.saturating_mul(2));
    let title_w = free.saturating_mul(TITLE_SHARE) / 5;
    List {
        header: clip(Rect::new(0, at(top), list_w, header_h), window),
        rows: clip(Rect::new(0, at(rows_top), list_w, rows_h), window),
        scrollbar: clip(Rect::new(at(list_w), at(rows_top), m.bar, rows_h), window),
        columns: [number_w, title_w, free.saturating_sub(title_w), time_w],
    }
}

/// `value` as a coordinate, saturating.
fn at(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// `rect` within `window`.
fn clip(rect: Rect, window: Rect) -> Rect {
    rect.intersection(&window)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
