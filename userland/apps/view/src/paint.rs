//! Painting the viewer, from state alone.
//!
//! Nothing here reads a file, asks a service, or decodes anything: every pixel
//! comes from what the engine already holds. A picture that has not arrived
//! yet is drawn as the reason it has not — never as a blank canvas, and never
//! as a fabricated image.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use tairix_controls::{fill_area, Checker, Fact, FactList};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Rect, Scale};
use tairix_icon::IconArtwork;
use tairix_player::Span;
use tairix_raster::{Color, Surface};
use tairix_sandbox::imagerender::ViewFormat;
use tairix_theme::Theme;
use tairix_util::utf8::shown;

use crate::movie::Transport;
use crate::view::View;
use crate::{Layout, Movie};

/// Paint the whole viewer into the caller's retained window `surface`.
///
/// `artwork` is the icon cache the toolbar resolves its glyphs through. A
/// viewer holds no filesystem capability, so it can read no shipped asset and
/// resolves through the refusing seam — which falls to the mandatory built-in
/// glyph tier, so every tool still draws.
///
/// The surface is the host's, held for the life of the window, so a caller
/// that has narrowed its clip to the rectangle a round reported redraws only
/// that band: every pixel outside it is the one already on screen.
pub fn render_into(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    font: BitmapFont,
    artwork: &mut dyn IconArtwork,
) {
    surface.fill(Color::from(theme.palette().surface));
    // The chrome strips stand off the body, so the canvas reads as the
    // content it is rather than as more window.
    let chrome = Color::from(theme.palette().surface_raised);
    fill_area(surface, layout.toolbar(), chrome);
    fill_area(surface, layout.transport(), chrome);
    fill_area(surface, layout.status(), chrome);
    canvas(surface, view, layout, theme, scale, font);
    view.toolbar_control()
        .render(surface, layout.tools(), scale, theme, artwork);
    view.zoom_control()
        .render(surface, layout.zoom_slider(), scale, theme);
    view.seek_control()
        .render(surface, layout.seek_bar(), scale, theme);
    clock(surface, view, layout, theme, font);
    let (vertical, horizontal) = view.bars();
    vertical.render(surface, layout.vertical_bar(), scale, theme);
    horizontal.render(surface, layout.horizontal_bar(), scale, theme);
    if !layout.info().is_empty() {
        FactList::new(facts(view)).with_separators(true).render(
            surface,
            layout.info(),
            scale,
            theme,
        );
    }
    status(surface, view, layout, theme, scale, font);
}

/// Paint the canvas: the checkerboard, then the picture over it, or the reason
/// there is no picture.
fn canvas(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    font: BitmapFont,
) {
    let bounds = layout.canvas();
    if bounds.is_empty() {
        return;
    }
    let (Some(picture), Some(natural)) = (view.picture(), view.natural()) else {
        reason(surface, view, bounds, theme, font);
        return;
    };
    let placement = view.viewport().placement(natural, bounds);
    checkerboard(surface, placement, theme, scale);
    // The picture goes over the checkerboard so its own transparency shows
    // what is behind it, which is the whole point of drawing one.
    surface.blit(placement.left(), placement.top(), picture);
}

/// Paint the transparency checkerboard under `bounds`.
fn checkerboard(surface: &mut Surface, bounds: Rect, theme: &Theme, scale: Scale) {
    let Some((x0, y0)) = bounds.surface_origin() else {
        return;
    };
    Checker::new(theme, scale).paint(surface, x0, y0, bounds.width, bounds.height);
}

/// Draw the reason the canvas is showing no picture, centred.
fn reason(surface: &mut Surface, view: &View, bounds: Rect, theme: &Theme, font: BitmapFont) {
    let text = view
        .refusal()
        .map_or_else(|| OPENING.to_string(), ToString::to_string);
    centre_text(
        surface,
        bounds,
        &text,
        font,
        Color::from(theme.palette().on_surface_muted),
    );
}

/// What the canvas and the status line say before anything has arrived.
const OPENING: &str = "Opening…";

/// What the information panel states.
///
/// Drawn from the container's own declaration, and from the *selected*
/// entry's geometry where a page container's pages differ in size.
pub(crate) fn facts(view: &View) -> Vec<Fact> {
    if let Some(movie) = view.movie() {
        return movie_facts(movie, view.viewport().zoom());
    }
    let Some(document) = view.document() else {
        return alloc::vec![Fact::new("Document", "none open")];
    };
    let natural = document.natural();
    let mut facts = alloc::vec![
        Fact::new("Name", shown(&document.name).into_owned()),
        Fact::new("Format", format_name(document.format())),
        Fact::new("Pixels", format!("{} x {}", natural.0, natural.1)),
        Fact::new("Size", bytes(document.bytes)),
    ];
    if document.info.count > 1 {
        facts.push(Fact::new(
            entry_word(document.info.animated),
            format!("{} of {}", document.index() + 1, document.info.count),
        ));
    }
    if document.info.animated {
        facts.push(Fact::new(
            "Repeats",
            document
                .info
                .loop_count
                .map_or_else(|| "for ever".to_string(), |count| format!("{count}")),
        ));
    }
    if let Some(page) = document.page.filter(|page| page.delay_ns > 0) {
        facts.push(Fact::new(
            "Delay",
            format!("{} ms", page.delay_ns / 1_000_000),
        ));
    }
    facts.push(Fact::new("Zoom", percent(view.viewport().zoom())));
    facts
}

/// Paint the status line: what is open, and at what magnification.
fn status(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    font: BitmapFont,
) {
    let bounds = layout.status();
    if bounds.is_empty() {
        return;
    }
    let gap = scale.scale_length(theme.metrics().control_gap).max(1);
    let ink = Color::from(theme.palette().on_surface_muted);
    let baseline = font.centred_top(bounds.top(), bounds.height);
    let zoom = percent(view.viewport().zoom());
    let zoom_width = font.text_width(&zoom);
    let room = bounds.width.saturating_sub(gap * 3 + zoom_width);
    font.draw_text(
        surface,
        bounds.left().saturating_add(to_i32(gap)),
        baseline,
        font.truncate_to_width(&summary(view), room),
        ink,
    );
    font.draw_text(
        surface,
        bounds
            .left()
            .saturating_add(to_i32(bounds.width.saturating_sub(gap + zoom_width))),
        baseline,
        &zoom,
        ink,
    );
}

/// The status line's one-line summary of what is open.
pub(crate) fn summary(view: &View) -> String {
    if let Some(movie) = view.movie() {
        return movie_summary(movie);
    }
    let Some(document) = view.document() else {
        return view
            .refusal()
            .map_or_else(|| OPENING.to_string(), ToString::to_string);
    };
    let natural = document.natural();
    let mut line = format!(
        "{} — {} {} x {}",
        shown(&document.name),
        format_name(document.format()),
        natural.0,
        natural.1
    );
    if document.info.count > 1 {
        let _ = write!(
            line,
            " — {} {} of {}",
            entry_word(document.info.animated).to_ascii_lowercase(),
            document.index() + 1,
            document.info.count
        );
    }
    let _ = write!(line, " — {}", bytes(document.bytes));
    line
}

/// The name the movie format is shown under.
const MOVIE_FORMAT: &str = "WebM";

/// What the information panel states about a movie.
fn movie_facts(movie: &Movie, zoom: u32) -> Vec<Fact> {
    let info = movie.info;
    let length = info.duration_ns.map_or_else(
        || "not stated".to_string(),
        |length| Span::from_nanos(length).clock(),
    );
    let sound = info.sound.map_or_else(
        || "none".to_string(),
        |channels| channel_words(channels.channels()),
    );
    let seeks = if info.indexed {
        "by the file's index"
    } else {
        "by reading the file"
    };
    let mut facts = alloc::vec![
        Fact::new("Name", shown(&movie.name).into_owned()),
        Fact::new("Format", MOVIE_FORMAT),
        Fact::new("Pixels", format!("{} x {}", info.width, info.height)),
        Fact::new("Length", length),
        Fact::new("Sound", sound),
        Fact::new("Seeks", seeks),
        Fact::new("Size", bytes(movie.bytes)),
    ];
    match movie.replaced {
        0 => {}
        1 => facts.push(Fact::new("Decoder", "replaced once")),
        count => facts.push(Fact::new("Decoder", format!("replaced {count} times"))),
    }
    facts.push(Fact::new("Zoom", percent(zoom)));
    facts
}

/// A sound's channels as a listener names them.
fn channel_words(count: u8) -> String {
    match count {
        1 => "mono".to_string(),
        2 => "stereo".to_string(),
        count => format!("{count} channels"),
    }
}

/// The status line's summary of an open movie, and what became of it.
fn movie_summary(movie: &Movie) -> String {
    let mut line = format!(
        "{} — {MOVIE_FORMAT} {} x {} — {}",
        shown(&movie.name),
        movie.info.width,
        movie.info.height,
        bytes(movie.bytes)
    );
    if let Transport::Stopped(stop) = movie.status.transport {
        let _ = write!(line, " — {stop}");
    }
    if let Some(failure) = movie.silenced {
        let _ = write!(line, " — no sound: {failure}");
    }
    if movie.replaced > 0 {
        line.push_str(" — its decoder failed and was replaced");
    }
    line
}

/// Paint the movie's clock at the transport band's trailing edge.
fn clock(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme, font: BitmapFont) {
    let bounds = layout.clock();
    let Some(text) = clock_text(view).filter(|_| !bounds.is_empty()) else {
        return;
    };
    let fitted = font.truncate_to_width(&text, bounds.width);
    let indent = bounds.width.saturating_sub(font.text_width(fitted));
    font.draw_text(
        surface,
        bounds.left().saturating_add(to_i32(indent)),
        font.centred_top(bounds.top(), bounds.height),
        fitted,
        Color::from(theme.palette().on_surface_muted),
    );
}

/// The movie's clock: where it is, or where a drag of the seek bar points,
/// and how long it is where the file says.
pub(crate) fn clock_text(view: &View) -> Option<String> {
    let movie = view.movie()?;
    let at = view.scrub_ns().unwrap_or(movie.status.position_ns);
    let mut text = Span::from_nanos(at).clock();
    if let Some(length) = movie.info.duration_ns {
        let _ = write!(text, " / {}", Span::from_nanos(length).clock());
    }
    Some(text)
}

/// What a container's entries are called: a played animation has frames, a
/// document has pages.
const fn entry_word(animated: bool) -> &'static str {
    if animated {
        "Frame"
    } else {
        "Page"
    }
}

/// The name a format is shown under.
///
/// Exhaustive on purpose: [`ViewFormat`] is the view protocol's own
/// vocabulary, so a format added to it must be given a name here rather than
/// falling through to a placeholder.
const fn format_name(format: ViewFormat) -> &'static str {
    match format {
        ViewFormat::Png => "PNG",
        ViewFormat::Jpeg => "JPEG",
        ViewFormat::Gif => "GIF",
        ViewFormat::Bmp => "BMP",
        ViewFormat::Ico => "Icon",
        ViewFormat::Sprite => "Sprite",
        ViewFormat::Tiff => "TIFF",
        ViewFormat::Webp => "WEBP",
        ViewFormat::Svg => "SVG",
        ViewFormat::OpenRaster => "OpenRaster",
    }
}

/// A byte count in the units a user reads, to one decimal place above a
/// kibibyte.
pub(crate) fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    const STEP: u64 = 1_024;
    if count < STEP {
        return format!("{count} B");
    }
    let mut whole = count;
    let mut remainder = 0;
    let mut unit = 0;
    while whole >= STEP && unit + 1 < UNITS.len() {
        remainder = whole % STEP;
        whole /= STEP;
        unit += 1;
    }
    // The tenth comes from the remainder of the *last* division, so it is
    // taken from the bytes themselves rather than from an already-rounded
    // quotient — and, being under one step, it cannot overflow the scaling
    // however large the file is.
    format!("{whole}.{} {}", remainder * 10 / STEP, UNITS[unit])
}

/// A zoom in parts per thousand, as the percentage a user reads.
pub(crate) fn percent(per_mille: u32) -> String {
    if per_mille.is_multiple_of(10) {
        return format!("{}%", per_mille / 10);
    }
    format!("{}.{}%", per_mille / 10, per_mille % 10)
}

/// Draw `text` centred in `rect`, truncated to fit.
fn centre_text(surface: &mut Surface, rect: Rect, text: &str, font: BitmapFont, ink: Color) {
    let fitted = font.truncate_to_width(text, rect.width);
    let width = font.text_width(fitted);
    let x = rect
        .left()
        .saturating_add(to_i32(rect.width.saturating_sub(width) / 2));
    font.draw_text(
        surface,
        x,
        font.centred_top(rect.top(), rect.height),
        fitted,
        ink,
    );
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
