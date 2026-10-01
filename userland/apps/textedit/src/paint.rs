//! Painting an editor window, from state alone.
//!
//! A paint reads the rows in view and nothing else, draws the colours the
//! lexer has already answered, and allocates one run buffer however large the
//! document is.

use alloc::string::String;
use core::fmt::Write as _;
use core::ops::ControlFlow;

use tairix_controls::{blend_area, fill_area, withheld};
use tairix_font::BitmapFont;
use tairix_geometry::{to_i32, Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_syntax::{Severity, Span};
use tairix_theme::{Palette, Rgba, SyntaxRole, Theme};

use crate::document::Document;
use crate::editor::Mode;
use crate::hex::{self, HexCaret, HexLayout, Nibble, Pane, BYTES_PER_ROW};
use crate::highlight::Stopped;
use crate::layout::{Faces, Layout};
use crate::text::{self, Glyph, Row};
use crate::view::View;

/// How far the caret's row is lifted from the page, in 255ths.
const CURRENT_ROW_ALPHA: u8 = 18;

/// How much a modal question darkens the window behind it, in 255ths.
const VEIL_ALPHA: u8 = 110;

/// Paint the whole window into `surface`; `focused` says whether it holds
/// the keyboard, which is when the caret is drawn.
pub fn render_into(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    scale: Scale,
    faces: Faces,
    focused: bool,
) {
    let palette = theme.palette();
    surface.fill(Color::from(palette.surface));
    let chrome = Color::from(palette.surface_raised);
    if view.find_open() && !withheld(surface, layout.find()) {
        fill_area(surface, layout.find(), chrome);
        let (find, replace, buttons) = view.find_controls();
        find.render(surface, layout.find_field(), scale, theme);
        replace.render(surface, layout.replace_field(), scale, theme);
        for (button, bounds) in buttons.iter().zip(layout.find_buttons()) {
            button.render(surface, *bounds, scale, theme);
        }
    }
    if !withheld(surface, layout.grid()) {
        fill_area(surface, layout.grid(), Color::from(palette.document));
        match view.editor().mode() {
            Mode::Text => text_grid(surface, view, layout, theme, faces.grid, focused, scale),
            Mode::Hex => hex_grid(surface, view, layout, theme, faces.grid, focused),
        }
    }
    let (vertical, horizontal) = view.scrollbars();
    if !withheld(surface, layout.vertical_bar()) {
        vertical.render(surface, layout.vertical_bar(), scale, theme);
    }
    if !withheld(surface, layout.horizontal_bar()) {
        horizontal.render(surface, layout.horizontal_bar(), scale, theme);
    }
    fill_area(surface, layout.corner(), Color::from(palette.scroll_track));
    if !withheld(surface, layout.status()) {
        status(surface, view, layout, theme, faces.status);
    }
    if let Some((dialog, field)) = view.modal() {
        let window = layout.window();
        blend_area(surface, window, palette.drop_shadow.with_alpha(VEIL_ALPHA));
        let bounds = View::modal_rect(dialog, window, field.is_some(), scale, theme);
        dialog.render(surface, bounds, scale, theme);
        if let (Some(field), Some(content)) = (field, dialog.content_rect(bounds, scale, theme)) {
            field.render(surface, content, scale, theme);
        }
    }
}

/// The colour a unit is drawn in: its token's role, else the span over it.
fn unit_role(glyph: Glyph, spans: &[Span], at: &mut usize, offset: u32) -> SyntaxRole {
    match glyph {
        Glyph::Control(_) => SyntaxRole::Control,
        Glyph::Invalid(_) => SyntaxRole::Invalid,
        Glyph::Hidden(_) => SyntaxRole::Invisible,
        Glyph::Char(_) | Glyph::Tab => {
            while spans.get(*at).is_some_and(|span| span.end <= offset) {
                *at += 1;
            }
            spans
                .get(*at)
                .filter(|span| span.start <= offset)
                .map_or(SyntaxRole::Plain, |span| span.role)
        }
    }
}

/// The text grid's fixed facts for one paint.
struct Grid<'a> {
    theme: &'a Theme,
    font: BitmapFont,
    area: Rect,
    gutter: Rect,
    cell: (u32, u32),
    left: usize,
    columns: usize,
    caret_width: u32,
}

/// The x column `column` is drawn at in a grid whose left edge is `edge`,
/// scrolled `left` columns of `cell_w` pixels: left of the edge for a column
/// scrolled past, so what straddles the edge is drawn where it is and the
/// clip cuts it.
fn column_x(edge: i32, left: usize, cell_w: u32, column: usize) -> i32 {
    let span = |columns: usize| {
        to_i32(
            u32::try_from(columns)
                .unwrap_or(u32::MAX)
                .saturating_mul(cell_w),
        )
    };
    if column >= left {
        edge.saturating_add(span(column - left))
    } else {
        edge.saturating_sub(span(left - column))
    }
}

/// The clip of the row of `area` from `top`, `height` tall.
fn row_clip(area: Rect, top: i32, height: u32) -> (u32, u32, u32, u32) {
    (
        u32::try_from(area.left()).unwrap_or(0),
        u32::try_from(top.max(0)).unwrap_or(0),
        area.width,
        height,
    )
}

impl Grid<'_> {
    /// The x a grid column is drawn at.
    fn x_of(&self, column: usize) -> i32 {
        column_x(self.area.left(), self.left, self.cell.0, column)
    }

    /// Whether a unit `width` columns wide at `column` shows at all.
    const fn visible(&self, column: usize, width: usize) -> bool {
        column + width > self.left && column < self.left + self.columns
    }

    /// Draw the pending run of characters and start a new one.
    fn flush(
        &self,
        surface: &mut Surface,
        run: &mut String,
        pen: &mut Option<(usize, SyntaxRole)>,
        y: i32,
    ) {
        if let Some((column, role)) = pen.take() {
            if !run.is_empty() {
                let ink = Color::from(self.theme.palette().syntax(role));
                self.font.draw_text(surface, self.x_of(column), y, run, ink);
            }
        }
        run.clear();
    }
}

fn text_grid(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    font: BitmapFont,
    focused: bool,
    scale: Scale,
) {
    let palette = theme.palette();
    let editor = view.editor();
    let document = editor.document();
    let (_, _, left) = view.scroll();
    let painter = Grid {
        theme,
        font,
        area: layout.grid(),
        gutter: layout.gutter(),
        cell: layout.cell(),
        left,
        columns: layout.columns(),
        caret_width: scale.scale_length(2).max(1),
    };
    fill_area(surface, painter.gutter, Color::from(palette.surface));
    let caret_row = text::row_of(document, editor.selection().head);
    let rows = layout.rows().max(1);
    let problems = problems_by_line(view, rows);
    let mut run = String::new();
    for (index, row) in view.visible_rows(rows).enumerate() {
        let rect = layout.row_rect(index);
        if withheld(surface, rect) {
            continue;
        }
        let problem = row
            .line
            .checked_sub(view.scroll().0.line)
            .and_then(|at| problems.get(at).copied().flatten());
        let current = row == caret_row;
        if current {
            let band = Rect::new(
                painter.area.left(),
                rect.top(),
                painter.area.width,
                painter.cell.1,
            );
            blend_area(surface, band, palette.accent.with_alpha(CURRENT_ROW_ALPHA));
        }
        gutter_cell(
            surface,
            &painter,
            rect.top(),
            row,
            current,
            problem,
            &mut run,
        );
        text_row(
            surface,
            &painter,
            view,
            row,
            rect.top(),
            focused && current,
            &mut run,
        );
        if let Some(severity) = problem {
            let y = rect.bottom().saturating_sub(1);
            fill_area(
                surface,
                Rect::new(painter.area.left(), y, painter.area.width, 1),
                Color::from(severity_colour(theme, severity)),
            );
        }
    }
}

/// The worst problem the parser reported on each line in view, the first
/// line in view first.
fn problems_by_line(view: &View, rows: usize) -> alloc::vec::Vec<Option<Severity>> {
    let first = view.scroll().0.line;
    let last = view.visible_rows(rows).last().map_or(first, |row| row.line);
    let mut problems = alloc::vec![None; last - first + 1];
    for diagnostic in view.editor().diagnostics().0 {
        let Some(slot) = diagnostic
            .line
            .and_then(|line| (line as usize).checked_sub(1 + first))
            .and_then(|at| problems.get_mut(at))
        else {
            continue;
        };
        if *slot != Some(Severity::Error) {
            *slot = Some(diagnostic.severity);
        }
    }
    problems
}

fn severity_colour(theme: &Theme, severity: Severity) -> Rgba {
    match severity {
        Severity::Error => theme.palette().danger,
        Severity::Warning => theme.palette().warning,
    }
}

/// One row's text: its units in their colours, the selection behind them,
/// and the caret when `caret` says it is on this row.
fn text_row(
    surface: &mut Surface,
    painter: &Grid<'_>,
    view: &View,
    row: Row,
    top: i32,
    caret: bool,
    run: &mut String,
) {
    let palette = painter.theme.palette();
    let editor = view.editor();
    let document = editor.document();
    let selected = editor.selection().range();
    let head = editor.selection().head;
    let line_start = document.line_start(row.line);
    let bounds = text::row_bounds(document, row);
    let spans = editor.highlight().spans(row.line).unwrap_or(&[]);
    let (cell_w, cell_h) = painter.cell;
    let y = painter.font.centred_top(top, cell_h);
    let mut span_at = 0usize;
    let mut end_column = 0usize;
    let mut caret_column = None;
    let mut pen: Option<(usize, SyntaxRole)> = None;
    let (x, clip_top, width, height) = row_clip(painter.area, top, cell_h);
    surface.with_clip(x, clip_top, width, height, |surface| {
        text::for_each_unit(document, bounds, editor.tab_width(), |unit| {
            end_column = unit.column + unit.width;
            if unit.offset == head {
                caret_column = Some(unit.column);
            }
            if unit.column >= painter.left + painter.columns {
                return ControlFlow::Break(());
            }
            if !painter.visible(unit.column, unit.width) {
                return ControlFlow::Continue(());
            }
            if selected.contains(&unit.offset) {
                let wide = u32::try_from(unit.width).unwrap_or(0) * cell_w;
                blend_area(
                    surface,
                    Rect::new(painter.x_of(unit.column), top, wide, cell_h),
                    palette.selection_fill,
                );
            }
            let offset = u32::try_from(unit.offset - line_start).unwrap_or(u32::MAX);
            let role = unit_role(unit.glyph, spans, &mut span_at, offset);
            match unit.glyph {
                Glyph::Char(ch) => {
                    if pen.is_some_and(|(_, ink)| ink != role) {
                        painter.flush(surface, run, &mut pen, y);
                    }
                    pen.get_or_insert((unit.column, role));
                    run.push(ch);
                }
                Glyph::Tab => painter.flush(surface, run, &mut pen, y),
                token => {
                    painter.flush(surface, run, &mut pen, y);
                    let mut buf = [0u8; 12];
                    if let Some(label) = token.token(&mut buf) {
                        let ink = Color::from(palette.syntax(role));
                        painter
                            .font
                            .draw_text(surface, painter.x_of(unit.column), y, label, ink);
                    }
                }
            }
            ControlFlow::Continue(())
        });
        painter.flush(surface, run, &mut pen, y);
        // A selection running on past the row's end holds its line break:
        // it shows as one cell more.
        if selected.contains(&bounds.end)
            && bounds.end < bounds.next
            && painter.visible(end_column, 1)
        {
            blend_area(
                surface,
                Rect::new(painter.x_of(end_column), top, cell_w, cell_h),
                palette.selection_fill,
            );
        }
    });
    if caret && selected.is_empty() {
        let column = caret_column.unwrap_or(end_column);
        if painter.visible(column, 1) {
            let x = painter.x_of(column);
            let (width, y, height) = if editor.overwrite() {
                let y = top.saturating_add(to_i32(cell_h.saturating_sub(painter.caret_width)));
                (cell_w, y, painter.caret_width)
            } else {
                (painter.caret_width, top, cell_h)
            };
            fill_area(
                surface,
                Rect::new(x, y, width, height).intersection(&painter.area),
                Color::from(palette.accent),
            );
        }
    }
}

/// The gutter beside one row: its line number on a line's first row, a
/// continuation mark on the rest, and a marker when its line has a problem.
fn gutter_cell(
    surface: &mut Surface,
    painter: &Grid<'_>,
    top: i32,
    row: Row,
    current: bool,
    problem: Option<Severity>,
    run: &mut String,
) {
    let gutter = painter.gutter;
    if gutter.is_empty() {
        return;
    }
    let palette = painter.theme.palette();
    let (cell_w, cell_h) = painter.cell;
    run.clear();
    if row.part == 0 {
        let _ = write!(run, "{}", row.line + 1);
    } else {
        run.push('\u{21aa}');
    }
    let wide = u32::try_from(run.chars().count()).unwrap_or(0) * cell_w;
    let right = gutter.right().saturating_sub(to_i32(cell_w * 2));
    let ink = Color::from(if current {
        palette.on_surface
    } else {
        palette.on_surface_muted
    });
    painter.font.draw_text(
        surface,
        right.saturating_sub(to_i32(wide)),
        painter.font.centred_top(top, cell_h),
        run,
        ink,
    );
    run.clear();
    if let (Some(severity), 0) = (problem, row.part) {
        let side = (cell_w / 2).max(2);
        let x = gutter.right().saturating_sub(to_i32(cell_w + side / 2));
        let y = top.saturating_add(to_i32(cell_h.saturating_sub(side) / 2));
        if let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) {
            surface.fill_round_rect(
                x,
                y,
                side,
                side,
                side / 2,
                Color::from(severity_colour(painter.theme, severity)),
            );
        }
    }
}

fn hex_grid(
    surface: &mut Surface,
    view: &View,
    layout: &Layout,
    theme: &Theme,
    font: BitmapFont,
    focused: bool,
) {
    let palette = theme.palette();
    let editor = view.editor();
    let document = editor.document();
    let len = document.len();
    let hex_layout = HexLayout::for_len(len);
    let (_, hex_top, left) = view.scroll();
    let (cell_w, cell_h) = layout.cell();
    let grid = layout.grid();
    let selected = editor.selection().range();
    let caret = editor.hex_caret();
    let columns = layout.columns();
    let x_of = |column: usize| column_x(grid.left(), left, cell_w, column);
    let shown = |column: usize, cells: usize| column + cells > left && column < left + columns;
    let muted = Color::from(palette.on_surface_muted);
    let mut digits = [0u8; 16];
    for index in 0..layout.rows() {
        let row = hex_top + index;
        let base = row * BYTES_PER_ROW;
        if base > len {
            break;
        }
        let rect = layout.row_rect(index);
        if withheld(surface, rect) {
            continue;
        }
        let top = rect.top();
        let y = font.centred_top(top, cell_h);
        let (bytes, count) = row_bytes(document, base);
        let (x, clip_top, width, height) = row_clip(grid, top, cell_h);
        surface.with_clip(x, clip_top, width, height, |surface| {
            let offset_text = hex_layout.offset_text(base, &mut digits);
            if shown(0, offset_text.len()) {
                font.draw_text(surface, x_of(0), y, offset_text, muted);
            }
            for (at, &byte) in bytes[..count].iter().enumerate() {
                let offset = base + at;
                let (hex_column, ascii_column) =
                    (hex_layout.hex_column(at), hex_layout.ascii_column(at));
                if selected.contains(&offset) {
                    for (column, cells, wide) in
                        [(hex_column, 2, 2 * cell_w), (ascii_column, 1, cell_w)]
                    {
                        if shown(column, cells) {
                            blend_area(
                                surface,
                                Rect::new(x_of(column), top, wide, cell_h),
                                palette.selection_fill,
                            );
                        }
                    }
                }
                let ink = byte_ink(byte, palette);
                let pair = hex::hex_pair(byte);
                if let (true, Ok(text)) = (shown(hex_column, 2), core::str::from_utf8(&pair)) {
                    font.draw_text(surface, x_of(hex_column), y, text, ink);
                }
                let mut ch = [0u8; 4];
                if shown(ascii_column, 1) {
                    font.draw_text(
                        surface,
                        x_of(ascii_column),
                        y,
                        tairix_util::hexdump::ascii_of(byte).encode_utf8(&mut ch),
                        ink,
                    );
                }
            }
            for column in [
                hex_layout.ascii_column(0) - 1,
                hex_layout.ascii_column(BYTES_PER_ROW),
            ] {
                if shown(column, 1) {
                    font.draw_text(surface, x_of(column), y, "|", muted);
                }
            }
            if focused && caret.offset / BYTES_PER_ROW == row {
                hex_caret(
                    surface,
                    caret,
                    hex_layout,
                    &x_of,
                    Rect::new(0, top, cell_w, cell_h),
                    palette,
                );
            }
        });
    }
}

/// The bytes of the hex row starting at `base`, and how many there are.
fn row_bytes(document: &Document, base: usize) -> ([u8; BYTES_PER_ROW], usize) {
    let mut bytes = [0u8; BYTES_PER_ROW];
    let mut count = 0;
    document.walk(base, |slice| {
        let take = slice.len().min(BYTES_PER_ROW - count);
        bytes[count..count + take].copy_from_slice(&slice[..take]);
        count += take;
        if count == BYTES_PER_ROW {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    (bytes, count)
}

/// What a byte is drawn in: text in the body colour, a NUL muted, and the rest
/// in the token colours the text view draws them in.
fn byte_ink(byte: u8, palette: &Palette) -> Color {
    Color::from(match byte {
        0 => palette.on_surface_muted,
        0x20..=0x7e | b'\t' | b'\n' | b'\r' => palette.on_surface,
        0x80..=0xff => palette.syntax(SyntaxRole::Invalid),
        _ => palette.syntax(SyntaxRole::Control),
    })
}

/// Frame the caret in the pane that has it and its twin in the other, on the
/// row `cell` spans; `x_of` places a column.
fn hex_caret(
    surface: &mut Surface,
    caret: HexCaret,
    hex_layout: HexLayout,
    x_of: &dyn Fn(usize) -> i32,
    cell: Rect,
    palette: &Palette,
) {
    let at = caret.offset % BYTES_PER_ROW;
    let hex_cell = hex_layout.hex_column(at) + usize::from(caret.nibble == Nibble::Low);
    let (active, other) = match caret.pane {
        Pane::Hex => (hex_cell, hex_layout.ascii_column(at)),
        Pane::Ascii => (hex_layout.ascii_column(at), hex_layout.hex_column(at)),
    };
    let at_column = |column: usize| Rect::new(x_of(column), cell.top(), cell.width, cell.height);
    frame(surface, at_column(active), Color::from(palette.accent), 2);
    frame(
        surface,
        at_column(other),
        Color::from(palette.on_surface_muted),
        1,
    );
}

/// Outline `rect` `weight` pixels thick.
fn frame(surface: &mut Surface, rect: Rect, color: Color, weight: u32) {
    let weight = weight.min(rect.width).min(rect.height);
    fill_area(
        surface,
        Rect::new(rect.left(), rect.top(), rect.width, weight),
        color,
    );
    fill_area(
        surface,
        Rect::new(
            rect.left(),
            rect.bottom().saturating_sub(to_i32(weight)),
            rect.width,
            weight,
        ),
        color,
    );
    fill_area(
        surface,
        Rect::new(rect.left(), rect.top(), weight, rect.height),
        color,
    );
    fill_area(
        surface,
        Rect::new(
            rect.right().saturating_sub(to_i32(weight)),
            rect.top(),
            weight,
            rect.height,
        ),
        color,
    );
}

/// The status band: where the caret is, what was last said or what the
/// parser reports, and the fields that open the view's settings.
fn status(surface: &mut Surface, view: &View, layout: &Layout, theme: &Theme, font: BitmapFont) {
    let palette = theme.palette();
    fill_area(
        surface,
        layout.status(),
        Color::from(palette.surface_raised),
    );
    let editor = view.editor();
    let document = editor.document();
    let selection = editor.selection();
    let mut text = String::new();
    match editor.mode() {
        Mode::Text => {
            let row = text::row_of(document, selection.head);
            let bounds = text::row_bounds(document, row);
            let column = text::column_of(document, bounds, selection.head, editor.tab_width());
            let _ = write!(text, "Ln {}, Col {}", row.line + 1, column + 1);
        }
        Mode::Hex => {
            let _ = write!(text, "Offset 0x{:X} ({})", selection.head, selection.head);
        }
    }
    if !selection.is_empty() {
        let _ = write!(text, ", {} selected", selection.range().len());
    }
    let muted = Color::from(palette.on_surface_muted);
    let ink = Color::from(palette.on_surface);
    let status = layout.status();
    let y = font.centred_top(status.top(), status.height);
    let draw = |surface: &mut Surface, rect: Rect, words: &str, colour: Color| {
        let fitted = font.truncate_to_width(words, rect.width);
        font.draw_text(surface, rect.left(), y, fitted, colour);
    };
    draw(surface, layout.position(), &text, ink);
    text.clear();
    let (diagnostics, current) = editor.diagnostics();
    let colour = if let Some(message) = view.message() {
        text.push_str(message);
        ink
    } else if let Some(stopped) = editor.highlight().stopped() {
        text.push_str(match stopped {
            Stopped::Failed => "Colouring stopped: the lexer failed on this document",
            Stopped::OutOfMemory => "Colouring stopped: there is not enough memory",
        });
        Color::from(palette.warning)
    } else if editor.format().is_store() && current {
        match diagnostics {
            [] => text.push_str("No problems"),
            [only] => {
                let _ = match only.line {
                    Some(line) => write!(text, "Line {line}: {}", only.message),
                    None => write!(text, "{}", only.message),
                };
            }
            many => {
                let _ = write!(text, "{} problems \u{2014} F8 goes to the next", many.len());
            }
        }
        let error = diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        match (diagnostics.is_empty(), error) {
            (true, _) => Color::from(palette.success),
            (false, true) => Color::from(palette.danger),
            (false, false) => Color::from(palette.warning),
        }
    } else {
        muted
    };
    draw(surface, layout.message(), &text, colour);
    let fields = layout.status_fields();
    let indent = match editor.indent() {
        crate::detect::Indent::Tab => String::from("Tab"),
        crate::detect::Indent::Spaces(width) => alloc::format!("Spaces: {width}"),
    };
    let values: [&str; 5] = [
        if editor.overwrite() { "OVR" } else { "INS" },
        &indent,
        editor.line_ending().label(),
        match editor.mode() {
            Mode::Text => "Text",
            Mode::Hex => "Hex",
        },
        editor.format().label(),
    ];
    for (rect, value) in fields.iter().zip(values) {
        draw(surface, *rect, value, ink);
    }
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
