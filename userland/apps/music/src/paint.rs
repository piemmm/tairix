//! The renderers: everything the window shows, drawn from the player's state
//! alone, so a paint reads nothing.
//!
//! [`paint`] draws only the parts that meet the rectangle being repainted, so
//! a change scoped to one part — the meters on every period, a row's
//! selection — costs that part and nothing more.

use alloc::format;
use alloc::string::String;
use alloc::vec;

use tairix_controls::{HeaderColumn, TableCell, TableHeader, TableRow};
use tairix_font::{BitmapFont, ELLIPSIS};
use tairix_geometry::{Rect, Scale};
use tairix_icon::{builtin_icon, IconKind};
use tairix_player::{describe, EntryId, Span, Transport};
use tairix_raster::{Color, Surface};
use tairix_theme::{TextRole, Theme};

use crate::layout::Layout;
use crate::view::{Player, Row};

/// The mark the row being heard carries in place of its number.
const HEARD_MARK: &str = "\u{25B6}";

/// Paint every part of `player`'s window that `clip` meets into `surface`,
/// with `art` the heard track's decoded cover, when it has arrived.
pub fn paint(
    surface: &mut Surface,
    player: &Player,
    layout: &Layout,
    art: Option<&Surface>,
    scale: Scale,
    theme: &Theme,
    clip: Rect,
) {
    fill(
        surface,
        clip.intersection(&layout.window()),
        theme.palette().surface.into(),
    );
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    paint_heard(surface, player, layout, art, font, theme, clip);
    paint_transport(surface, player, layout, scale, theme, clip);
    paint_list(surface, player, layout, font, scale, theme, clip);
}

/// What is heard: its cover, its names, its format and where it is.
fn paint_heard(
    surface: &mut Surface,
    player: &Player,
    layout: &Layout,
    art: Option<&Surface>,
    font: BitmapFont,
    theme: &Theme,
    clip: Rect,
) {
    let palette = theme.palette();
    let meets = |part: Rect| !part.intersection(&clip).is_empty();
    let heard = player
        .heard()
        .and_then(|entry| player.playlist().get(entry));

    if meets(layout.art()) {
        paint_art(
            surface,
            layout.art(),
            art.filter(|_| heard.is_some()),
            theme,
        );
    }
    let (title, subtitle) = match heard {
        Some(row) => (String::from(row.title()), subtitle(row)),
        None => (String::from("Nothing playing"), String::new()),
    };
    if meets(layout.title()) {
        text(
            surface,
            font,
            layout.title(),
            &title,
            palette.on_surface.into(),
        );
    }
    if meets(layout.subtitle()) {
        text(
            surface,
            font,
            layout.subtitle(),
            &subtitle,
            palette.on_surface_muted.into(),
        );
    }
    if meets(layout.format()) {
        if let Some((_, info)) = player.status().heard {
            text(
                surface,
                font,
                layout.format(),
                &describe(&info),
                palette.on_surface_muted.into(),
            );
        }
    }
    if let Some((here, length)) = player.shown_position() {
        if meets(layout.elapsed()) {
            text(
                surface,
                font,
                layout.elapsed(),
                &here.clock(),
                palette.on_surface.into(),
            );
        }
        if meets(layout.total()) {
            text(
                surface,
                font,
                layout.total(),
                &length.clock(),
                palette.on_surface.into(),
            );
        }
    }
}

/// The seek slider, the meters, the buttons and the volume.
fn paint_transport(
    surface: &mut Surface,
    player: &Player,
    layout: &Layout,
    scale: Scale,
    theme: &Theme,
    clip: Rect,
) {
    let palette = theme.palette();
    let meets = |part: Rect| !part.intersection(&clip).is_empty();
    if meets(layout.seek()) {
        player
            .seek_slider()
            .render(surface, layout.seek(), scale, theme);
    }
    if meets(layout.meters()) {
        paint_meters(surface, layout.meters(), player, theme);
    }
    for (bounds, button) in [
        (layout.previous(), player.previous_button()),
        (layout.play(), player.play_button()),
        (layout.next(), player.next_button()),
        (layout.shuffle(), player.shuffle_button()),
        (layout.repeat(), player.repeat_button()),
    ] {
        if meets(bounds) {
            button.render(surface, bounds, scale, theme, None);
        }
    }
    if meets(layout.volume_icon()) {
        glyph(
            surface,
            layout.volume_icon(),
            IconKind::Volume,
            palette.on_surface_muted.into(),
        );
    }
    if meets(layout.volume()) {
        player
            .volume_slider()
            .render(surface, layout.volume(), scale, theme);
    }
}

/// The playlist, its scroll bar, and the status line.
fn paint_list(
    surface: &mut Surface,
    player: &Player,
    layout: &Layout,
    font: BitmapFont,
    scale: Scale,
    theme: &Theme,
    clip: Rect,
) {
    let palette = theme.palette();
    let meets = |part: Rect| !part.intersection(&clip).is_empty();
    if meets(layout.header()) {
        header().render(surface, layout.header(), scale, theme, layout.columns());
    }
    if meets(layout.rows()) {
        paint_rows(surface, player, layout, scale, theme, clip);
    }
    if meets(layout.scrollbar()) {
        player
            .scroll_bar()
            .render(surface, layout.scrollbar(), scale, theme);
    }
    if meets(layout.status()) {
        text(
            surface,
            font,
            layout.status(),
            &status_line(player),
            palette.on_surface_muted.into(),
        );
    }
}

/// The playlist's column titles.
fn header() -> TableHeader {
    TableHeader::new(vec![
        HeaderColumn::fixed("#"),
        HeaderColumn::new("Title"),
        HeaderColumn::new("Artist"),
        HeaderColumn::fixed("Time"),
    ])
}

fn paint_rows(
    surface: &mut Surface,
    player: &Player,
    layout: &Layout,
    scale: Scale,
    theme: &Theme,
    clip: Rect,
) {
    let scroll = player.scroll();
    for (index, entry) in player.rows_in_sight(layout) {
        let bounds = layout.row(index, scroll);
        if bounds.intersection(&clip).is_empty() {
            continue;
        }
        let Some(row) = player.playlist().get(entry) else {
            continue;
        };
        let mut table_row = table_row(index, entry, row, player.heard());
        table_row.set_selected(player.selected() == Some(entry));
        table_row.render(surface, bounds, scale, theme, layout.columns(), None);
    }
}

/// The table row `row` is drawn as at `index`.
fn table_row(index: usize, entry: EntryId, row: &Row, heard: Option<EntryId>) -> TableRow {
    let number = if heard == Some(entry) {
        String::from(HEARD_MARK)
    } else {
        format!("{}", index + 1)
    };
    let length = row
        .known()
        .and_then(crate::view::Known::length)
        .map_or_else(String::new, Span::clock);
    TableRow::new(vec![
        TableCell::numeric(number),
        TableCell::new(row.title()),
        TableCell::new(row.artist()),
        TableCell::numeric(length),
    ])
}

/// The artist and album line, or the file's own name where the tags give
/// neither.
fn subtitle(row: &Row) -> String {
    let Some(known) = row.known() else {
        return row.name.clone();
    };
    match (known.artist.as_deref(), known.album.as_deref()) {
        (Some(artist), Some(album)) => format!("{artist} \u{2014} {album}"),
        (Some(only), None) | (None, Some(only)) => String::from(only),
        (None, None) => row.name.clone(),
    }
}

/// What the status line says: a notice, a held stream, or the playlist's
/// length.
fn status_line(player: &Player) -> String {
    if let Some(notice) = player.notice() {
        return String::from(notice);
    }
    if player.status().transport == Transport::Held {
        return String::from("Held: another session holds the speakers");
    }
    let playlist = player.playlist();
    if playlist.is_empty() {
        return String::from("Open files or a folder to play (Ctrl+O, Ctrl+Shift+O)");
    }
    let count = playlist.len();
    let noun = if count == 1 { "track" } else { "tracks" };
    format!("{count} {noun}, {}", player.listed_length().clock())
}

/// The cover, or the sound glyph on a plate where there is none yet.
fn paint_art(surface: &mut Surface, bounds: Rect, art: Option<&Surface>, theme: &Theme) {
    if let Some(picture) = art {
        surface.blit(bounds.left(), bounds.top(), picture);
        return;
    }
    let palette = theme.palette();
    fill(surface, bounds, palette.surface_raised.into());
    glyph(
        surface,
        bounds,
        IconKind::Audio,
        palette.on_surface_muted.into(),
    );
}

/// One bar a channel, filled from the bottom to the loudest sample of the
/// span being heard.
fn paint_meters(surface: &mut Surface, bounds: Rect, player: &Player, theme: &Theme) {
    let palette = theme.palette();
    let (levels, channels) = player.status().peaks;
    let breadth = bounds.width / 3;
    for bar in 0..2u32 {
        let x = bounds.left() + i32::try_from(bar * (bounds.width - breadth)).unwrap_or(0);
        let track = Rect::new(x, bounds.top(), breadth, bounds.height);
        fill(surface, track, palette.scroll_track.into());
        let channel = usize::try_from(bar)
            .unwrap_or(0)
            .min(channels.saturating_sub(1));
        let level = if channels == 0 { 0 } else { levels[channel] };
        let height = bounds.height * u32::from(level) / u32::from(u8::MAX);
        let top = bounds.bottom() - i32::try_from(height).unwrap_or(0);
        fill(
            surface,
            Rect::new(x, top, breadth, height),
            palette.accent.into(),
        );
    }
}

/// `text` in `bounds`, centred down it and cut with an ellipsis where it is
/// too wide.
fn text(surface: &mut Surface, font: BitmapFont, bounds: Rect, text: &str, color: Color) {
    if bounds.is_empty() || text.is_empty() {
        return;
    }
    let (shown, elided) = font.elide_to_width(text, bounds.width);
    let drop = bounds.height.saturating_sub(font.line_height()) / 2;
    let y = bounds.top() + i32::try_from(drop).unwrap_or(0);
    let pen = font.draw_text(surface, bounds.left(), y, shown, color);
    if elided {
        font.draw_text(surface, pen, y, ELLIPSIS, color);
    }
}

/// The built-in glyph for `kind`, centred in `bounds`.
fn glyph(surface: &mut Surface, bounds: Rect, kind: IconKind, color: Color) {
    let side = bounds.width.min(bounds.height) * 2 / 3;
    let Some(picture) = builtin_icon(kind, color).rasterise(side) else {
        return;
    };
    let inset = |extent: u32| i32::try_from(extent.saturating_sub(side) / 2).unwrap_or(0);
    surface.blit(
        bounds.left() + inset(bounds.width),
        bounds.top() + inset(bounds.height),
        &picture,
    );
}

/// Fill `rect` with `color`, where it lies on the surface.
fn fill(surface: &mut Surface, rect: Rect, color: Color) {
    let (Ok(x), Ok(y)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
        return;
    };
    surface.fill_rect(x, y, rect.width, rect.height, color);
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
