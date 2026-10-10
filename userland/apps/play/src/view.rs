//! The full-screen interface: what is heard, where in it, how loud, and the
//! list — painted from the engine's status alone, so a paint reads nothing.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use tairix_curses::{truncate_to_width, Event, Pos, Size, Window};
use tairix_vt::Attributes;

use crate::report::describe;
use tairix_player::{Control, List, Span, Status, Transport};

/// The keys the footer names.
const KEYS: &str = " Space pause  \u{2190}/\u{2192} seek  n/b next/back  +/- level  q quit";

/// Rows above the list: title, transport, file, format, level, and a rule.
const HEAD_ROWS: u16 = 6;

const REVERSE: Attributes = Attributes {
    reverse: true,
    ..Attributes::PLAIN
};

const BOLD: Attributes = Attributes {
    bold: true,
    ..Attributes::PLAIN
};

/// What a key asks of the interface.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Key {
    /// Something of the playback.
    Control(Control),
    /// Give the terminal back and stop until continued.
    Suspend,
}

/// What `event` asks, if anything.
#[must_use]
pub fn key(event: &Event) -> Option<Key> {
    Some(match event {
        Event::Char(' ' | 'p') => Key::Control(Control::TogglePause),
        Event::Right => Key::Control(Control::Forward),
        Event::Left => Key::Control(Control::Back),
        Event::Char('n' | '>') => Key::Control(Control::Next),
        Event::Char('b' | '<') => Key::Control(Control::Previous),
        Event::Char('+' | '=') | Event::Up => Key::Control(Control::Louder),
        Event::Char('-' | '_') | Event::Down => Key::Control(Control::Quieter),
        Event::Char('q' | 'Q') | Event::Ctrl('c') => Key::Control(Control::Stop),
        Event::Ctrl('z') => Key::Suspend,
        _ => return None,
    })
}

/// Paint the interface for `playing` at `status` into a window of `size`,
/// with `notice` — the last thing that went wrong — on its own line.
#[must_use]
pub fn draw(playing: &List, status: &Status, notice: Option<&str>, size: Size) -> Window {
    let mut window = Window::new(Pos::ORIGIN, size);
    let cols = usize::from(size.cols);
    let files = playing.paths();
    let heard = status.heard.and_then(|(entry, _)| playing.index(entry));
    let title = match heard {
        Some(index) => format!(
            " play  {} of {}  pass {}",
            index + 1,
            files.len(),
            status.pass + 1
        ),
        None => format!(" play  {} files", files.len()),
    };
    line(&mut window, 0, &title, REVERSE, cols);
    line(
        &mut window,
        1,
        &transport_line(status, cols),
        Attributes::PLAIN,
        cols,
    );
    if let (Some(index), Some((_, info))) = (heard, &status.heard) {
        let name = files.get(index).map_or("", String::as_str);
        line(&mut window, 2, &format!(" {name}"), BOLD, cols);
        line(
            &mut window,
            3,
            &format!(" {}", describe(info)),
            Attributes::PLAIN,
            cols,
        );
    }
    line(
        &mut window,
        4,
        &level_line(status, cols),
        Attributes::PLAIN,
        cols,
    );
    let rule: String = core::iter::repeat_n('-', cols).collect();
    line(&mut window, 5, &rule, Attributes::PLAIN, cols);
    let footer = size.rows.saturating_sub(1);
    let notice_row = notice.map(|_| footer.saturating_sub(1));
    let list_end = notice_row.unwrap_or(footer);
    list(&mut window, files, heard, HEAD_ROWS..list_end, cols);
    if let (Some(row), Some(notice)) = (notice_row, notice) {
        line(&mut window, row, &format!(" {notice}"), BOLD, cols);
    }
    if footer >= HEAD_ROWS {
        line(&mut window, footer, KEYS, REVERSE, cols);
    }
    window
}

fn transport_line(status: &Status, cols: usize) -> String {
    let word = match status.transport {
        Transport::Starting => "Starting",
        Transport::Playing => "Playing",
        Transport::Paused => "Paused",
        Transport::Held => "Held: the seat is elsewhere",
        Transport::Ending => "Ending",
        Transport::Stopped => "Stopped",
    };
    let Some((_, info)) = &status.heard else {
        return format!(" {word}");
    };
    let hz = info.rate.hz();
    let here = Span::of_frames(status.position, hz).clock();
    let Some(frames) = info.frames else {
        return format!(" {word}  {here}");
    };
    let text = format!(
        " {word}  {here} / {}  ",
        Span::of_frames(frames, hz).clock()
    );
    let room = cols.saturating_sub(text.len() + 2);
    if room < 4 {
        return text;
    }
    format!("{text}[{}]", bar(status.position, frames, room, '=', '-'))
}

fn level_line(status: &Status, cols: usize) -> String {
    let millibel = status.gain.millibel();
    let mut text = format!(
        " Level {}{}.{:02} dB",
        if millibel < 0 { "-" } else { "" },
        millibel.unsigned_abs() / 100,
        millibel.unsigned_abs() % 100
    );
    if status.underruns > 0 {
        let _ = write!(text, "   underruns {}", status.underruns);
    }
    let (levels, channels) = status.peaks;
    let meters = channels.min(levels.len());
    // Each meter is a space, its label and two brackets around its bar.
    let room = cols.saturating_sub(text.len() + 2);
    if meters == 0 || room < meters * 8 {
        return text;
    }
    let each = room / meters - 4;
    text.push_str("  ");
    for (channel, level) in levels[..meters].iter().enumerate() {
        let label = match (meters, channel) {
            (1, _) => 'M',
            (2, 0) => 'L',
            (2, _) => 'R',
            (_, n) => u32::try_from((n + 1) % 10)
                .ok()
                .and_then(|digit| char::from_digit(digit, 10))
                .unwrap_or('?'),
        };
        let _ = write!(
            text,
            " {label}[{}]",
            bar(u64::from(*level), 255, each, '#', ' ')
        );
    }
    text
}

/// A bar `width` cells wide, filled to `value` of `whole`.
fn bar(value: u64, whole: u64, width: usize, filled: char, empty: char) -> String {
    let full = if whole == 0 {
        0
    } else {
        usize::try_from(u128::from(value.min(whole)) * width as u128 / u128::from(whole))
            .unwrap_or(width)
    };
    core::iter::repeat_n(filled, full)
        .chain(core::iter::repeat_n(empty, width - full))
        .collect()
}

/// The list in `rows`, scrolled so the file heard is in sight.
fn list(
    window: &mut Window,
    files: &[String],
    heard: Option<usize>,
    rows: core::ops::Range<u16>,
    cols: usize,
) {
    let shown = usize::from(rows.end.saturating_sub(rows.start));
    if shown == 0 {
        return;
    }
    let first = heard
        .map_or(0, |index| index.saturating_sub(shown / 2))
        .min(files.len().saturating_sub(shown));
    for (row, (index, name)) in rows.zip(files.iter().enumerate().skip(first)) {
        let marker = if Some(index) == heard { '>' } else { ' ' };
        let attributes = if Some(index) == heard {
            BOLD
        } else {
            Attributes::PLAIN
        };
        line(
            window,
            row,
            &format!(" {marker} {:>3}  {name}", index + 1),
            attributes,
            cols,
        );
    }
}

/// Write `text` across `row`, cut to the window's width.
fn line(window: &mut Window, row: u16, text: &str, attributes: Attributes, cols: usize) {
    if row >= window.size().rows {
        return;
    }
    window.set_attributes(attributes);
    let _ = window.move_add_str(Pos::new(row, 0), truncate_to_width(text, cols));
    window.set_attributes(Attributes::PLAIN);
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
