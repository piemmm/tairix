//! `MODE`'s command line, as DOS 4's parser reads it: a device or a display
//! mode first, then its settings by position or by keyword.

use super::{retry, text};
use crate::dos::shell::parse;
use crate::dos::utility::Refusal;
use alloc::vec::Vec;

/// A device `MODE` names: the console, printer `n` or serial port `n`,
/// from `0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Console,
    Printer(u8),
    Serial(u8),
}

/// A display mode `MODE` sets, numbered as a routine keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Display {
    Forty,
    Eighty,
    Bw40,
    Co40,
    Bw80,
    Co80,
    Mono,
}

impl Display {
    const ALL: [Self; 7] = [
        Self::Forty,
        Self::Eighty,
        Self::Bw40,
        Self::Co40,
        Self::Bw80,
        Self::Co80,
        Self::Mono,
    ];

    /// The mode numbered `n`.
    #[must_use]
    pub fn of(n: u8) -> Option<Self> {
        Self::ALL.get(usize::from(n)).copied()
    }

    /// Its name as `MODE` takes it.
    #[must_use]
    pub const fn name(self) -> &'static [u8] {
        match self {
            Self::Forty => b"40",
            Self::Eighty => b"80",
            Self::Bw40 => b"BW40",
            Self::Co40 => b"CO40",
            Self::Bw80 => b"BW80",
            Self::Co80 => b"CO80",
            Self::Mono => b"MONO",
        }
    }
}

/// What follows a display mode: the picture shifted, a test pattern shown
/// to set it by, or the screen's text rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adjust {
    Shift { right: bool, test: bool },
    Rows(u8),
}

/// A serial port's framing: its rate, parity letter, data bits, and stop
/// bits in tenths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framing {
    pub baud: u32,
    pub parity: u8,
    pub data: u8,
    pub stop: u8,
}

/// What `MODE`'s command line asks for. A retry of `None` keeps the one in
/// force, as a keyword form that names none does; a positional form that
/// names none turns it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// One device's status, or every one's.
    Status(Option<Device>),
    Display {
        display: Option<Display>,
        adjust: Option<Adjust>,
    },
    Console {
        columns: Option<u8>,
        lines: Option<u8>,
        typematic: Option<(u8, u8)>,
    },
    Printer {
        lpt: u8,
        columns: Option<u8>,
        lines: Option<u8>,
        retry: Option<u8>,
    },
    /// A printer's output sent to a serial port, or, with none, back to the
    /// printer.
    Reroute {
        lpt: u8,
        com: Option<u8>,
    },
    Serial {
        com: u8,
        framing: Framing,
        retry: Option<u8>,
    },
    CodePage,
}

/// Why a command line is not taken: one of `MODE`'s own messages, a
/// parameter it does not take, or one no machine here can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Said(&'static [u8]),
    Invalid(Vec<u8>),
    Unsupported(Vec<u8>),
}

impl Problem {
    /// The text it is said as.
    #[must_use]
    pub fn text(&self) -> Vec<u8> {
        match self {
            Self::Said(text) => text.to_vec(),
            Self::Invalid(word) => [&b"\r\n"[..], &Refusal::Parameter(word).text(b"")].concat(),
            Self::Unsupported(word) => text::unsupported(word),
        }
    }

    fn invalid(word: &[u8]) -> Self {
        Self::Invalid(parse::trim(word).to_vec())
    }
}

/// The device a command line's first word names, and what follows its
/// name unparted: DOS's `LPT180` and `LPT1132` name a printer and its
/// columns.
fn device_of(word: &[u8]) -> Option<(Device, &[u8])> {
    let upper = word.to_ascii_uppercase();
    match upper.as_slice() {
        b"CON" => Some((Device::Console, &[])),
        b"PRN" => Some((Device::Printer(0), &[])),
        [b'L', b'P', b'T', n @ b'1'..=b'3', ..] => Some((Device::Printer(n - b'1'), &word[4..])),
        [b'C', b'O', b'M', n @ b'1'..=b'4'] => Some((Device::Serial(n - b'1'), &[])),
        _ => None,
    }
}

fn display_of(word: &[u8]) -> Option<Display> {
    Display::ALL
        .into_iter()
        .find(|display| word.eq_ignore_ascii_case(display.name()))
}

/// `field` as a decimal number, if it is one.
fn number(field: &[u8]) -> Option<u32> {
    if field.len() > 6 {
        return None;
    }
    crate::dos::shell::parse::number(field)
}

/// A rate, given whole, by its first two digits, or 19200 as `19.2` or
/// `19.2K`.
#[must_use]
pub fn baud_of(field: &[u8]) -> Option<u32> {
    const RATES: [u32; 9] = [110, 150, 300, 600, 1200, 2400, 4800, 9600, 19200];
    if field.eq_ignore_ascii_case(b"19.2") || field.eq_ignore_ascii_case(b"19.2K") {
        return Some(19200);
    }
    let n = number(field)?;
    RATES
        .into_iter()
        .find(|&rate| rate == n || (field.len() == 2 && rate / 10u32.pow(rate.ilog10() - 1) == n))
}

/// A retry letter as typed, `N` and `NONE` the same.
fn retry_of(field: &[u8]) -> Option<u8> {
    Some(match field.to_ascii_uppercase().as_slice() {
        b"N" | b"NONE" => retry::NONE,
        b"E" => retry::ERROR,
        b"B" => retry::BUSY,
        b"R" => retry::READY,
        b"P" => retry::ALWAYS,
        _ => return None,
    })
}

fn parity_of(field: &[u8]) -> Option<u8> {
    Some(match field.to_ascii_uppercase().as_slice() {
        b"N" | b"NONE" => b'n',
        b"O" | b"ODD" => b'o',
        b"E" | b"EVEN" => b'e',
        b"M" | b"MARK" => b'm',
        b"S" | b"SPACE" => b's',
        _ => return None,
    })
}

/// A `KEY=value` word: its key in upper case, its value, and the word as
/// typed, spaces about its `=` taken out.
struct Keyword {
    key: Vec<u8>,
    value: Vec<u8>,
    word: Vec<u8>,
}

/// The `KEY=value` words of `rest`.
fn keywords(rest: &[u8]) -> Vec<Keyword> {
    let mut words: Vec<Vec<u8>> = Vec::new();
    for word in rest
        .split(|&b| matches!(b, b' ' | b'\t'))
        .filter(|word| !word.is_empty())
    {
        match words.last_mut() {
            Some(last) if last.ends_with(b"=") || word.starts_with(b"=") => {
                last.extend_from_slice(word);
            }
            _ => words.push(word.to_vec()),
        }
    }
    words
        .into_iter()
        .map(|word| {
            let at = word.iter().position(|&b| b == b'=').unwrap_or(word.len());
            Keyword {
                key: word[..at].to_ascii_uppercase(),
                value: word.get(at + 1..).unwrap_or_default().to_vec(),
                word,
            }
        })
        .collect()
}

/// The fields of a positional tail, `,` between them.
fn fields(rest: &[u8]) -> Vec<&[u8]> {
    let rest = parse::trim(rest);
    if rest.is_empty() {
        return Vec::new();
    }
    rest.split(|&b| b == b',').map(parse::trim).collect()
}

/// Whether the settings are `/STATUS`, or `/STA` for short.
fn status(rest: &[u8]) -> bool {
    let rest = parse::trim(rest).to_ascii_uppercase();
    rest == b"/STATUS" || rest == b"/STA"
}

/// `MODE`'s command line read, or what is wrong with it.
///
/// # Errors
///
/// The [`Problem`] `MODE` says of a line it does not take.
pub fn request(tail: &[u8]) -> Result<Request, Problem> {
    let text = parse::trim(tail);
    if text.is_empty() {
        return Ok(Request::Status(None));
    }
    let end = text
        .iter()
        .position(|b| matches!(b, b' ' | b'\t' | b',' | b':' | b'=' | b'/'))
        .unwrap_or(text.len());
    let (word, rest) = text.split_at(end);
    let rest = rest.strip_prefix(b":").unwrap_or(rest);
    if word.is_empty() {
        return if status(rest) {
            Ok(Request::Status(None))
        } else {
            display(None, rest)
        };
    }
    if let Some(display_mode) = display_of(word) {
        return display(Some(display_mode), rest);
    }
    let Some((device, glued)) = device_of(word) else {
        return Err(Problem::invalid(word));
    };
    if status(rest) && glued.is_empty() {
        return Ok(Request::Status(Some(device)));
    }
    let upper = rest.to_ascii_uppercase();
    if parse::arguments(&upper)
        .iter()
        .any(|w| *w == b"CP" || *w == b"CODEPAGE")
    {
        return Ok(Request::CodePage);
    }
    match device {
        Device::Printer(lpt) if !glued.is_empty() => {
            if !matches!(glued, b"80" | b"132") || !parse::trim(rest).is_empty() {
                return Err(Problem::invalid(word));
            }
            printer(lpt, glued)
        }
        Device::Console => console(rest),
        Device::Printer(lpt) => printer(lpt, rest),
        Device::Serial(com) => serial(com, rest),
    }
}

fn display(display: Option<Display>, rest: &[u8]) -> Result<Request, Problem> {
    let rest = parse::trim(rest);
    if rest.is_empty() {
        return if display.is_some() {
            Ok(Request::Display {
                display,
                adjust: None,
            })
        } else {
            Err(Problem::Said(text::FIELDS))
        };
    }
    let Some(rest) = rest.strip_prefix(b",") else {
        return Err(Problem::invalid(rest));
    };
    let fields = fields(rest);
    if fields.len() > 2 {
        return Err(Problem::Said(text::FIELDS));
    }
    let first = fields.first().copied().unwrap_or_default();
    let adjust = match first.to_ascii_uppercase().as_slice() {
        b"" if fields.len() <= 1 && display.is_some() => None,
        b"" if fields.len() <= 1 => return Err(Problem::Said(text::FIELDS)),
        b"R" | b"L" => {
            let test = match fields.get(1) {
                None => false,
                Some(f) if f.eq_ignore_ascii_case(b"T") => true,
                Some(f) => return Err(Problem::invalid(f)),
            };
            Some(Adjust::Shift {
                right: first.eq_ignore_ascii_case(b"R"),
                test,
            })
        }
        _ => match number(first)
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| matches!(n, 25 | 43 | 50))
        {
            Some(rows) if fields.len() == 1 => Some(Adjust::Rows(rows)),
            _ => return Err(Problem::invalid(first)),
        },
    };
    Ok(Request::Display { display, adjust })
}

fn console(rest: &[u8]) -> Result<Request, Problem> {
    let (mut columns, mut lines, mut rate, mut delay) = (None, None, None, None);
    let words = keywords(rest);
    if words.is_empty() {
        return Ok(Request::Status(Some(Device::Console)));
    }
    for Keyword { key, value, word } in &words {
        let n = number(value).and_then(|n| u8::try_from(n).ok());
        let refused = || Problem::invalid(word);
        match key.as_slice() {
            b"COLS" | b"COLUMNS" => {
                columns = Some(n.filter(|&c| c == 40 || c == 80).ok_or_else(refused)?);
            }
            b"LINES" => {
                lines = Some(
                    n.filter(|&l| matches!(l, 25 | 43 | 50))
                        .ok_or_else(refused)?,
                );
            }
            b"RATE" => rate = Some(n.filter(|&r| (1..=32).contains(&r)).ok_or_else(refused)?),
            b"DELAY" | b"DEL" => {
                delay = Some(n.filter(|&d| (1..=4).contains(&d)).ok_or_else(refused)?);
            }
            _ => return Err(refused()),
        }
    }
    let typematic = match (rate, delay) {
        (Some(rate), Some(delay)) => Some((rate, delay)),
        (None, None) => None,
        _ => return Err(Problem::Said(text::RATE_DELAY)),
    };
    Ok(Request::Console {
        columns,
        lines,
        typematic,
    })
}

fn printer(lpt: u8, rest: &[u8]) -> Result<Request, Problem> {
    let trimmed = parse::trim(rest);
    if let Some(target) = trimmed.strip_prefix(b"=") {
        let target = parse::trim(target);
        let target = target.strip_suffix(b":").unwrap_or(target);
        return match device_of(target) {
            Some((Device::Serial(com), [])) => Ok(Request::Reroute {
                lpt,
                com: Some(com),
            }),
            _ => Err(Problem::Said(text::MUST_COM)),
        };
    }
    if trimmed.is_empty() {
        return Ok(Request::Reroute { lpt, com: None });
    }
    let (columns, lines, retry) = if trimmed.contains(&b'=') {
        let (mut columns, mut lines, mut retry) = (None, None, None);
        for Keyword { key, value, word } in keywords(trimmed) {
            let slot = match key.as_slice() {
                b"COLS" | b"COLUMNS" => &mut columns,
                b"LINES" => &mut lines,
                b"RETRY" => &mut retry,
                _ => return Err(Problem::invalid(&word)),
            };
            *slot = Some((value, word));
        }
        (columns, lines, retry)
    } else {
        let fields = fields(trimmed);
        if fields.len() > 3 {
            return Err(Problem::Said(text::FIELDS));
        }
        let field = |n: usize| {
            fields
                .get(n)
                .filter(|f| !f.is_empty())
                .map(|f| (f.to_vec(), f.to_vec()))
        };
        let retry = field(2).or_else(|| Some((b"N".to_vec(), Vec::new())));
        (field(0), field(1), retry)
    };
    let pick = |given: Option<(Vec<u8>, Vec<u8>)>, valid: fn(u32) -> bool| {
        given
            .map(|(value, word)| {
                number(&value)
                    .filter(|&n| valid(n))
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| Problem::invalid(&word))
            })
            .transpose()
    };
    let columns = pick(columns, |c| c == 80 || c == 132)?;
    let lines = pick(lines, |l| l == 6 || l == 8)?;
    let retry = retry
        .map(|(value, word)| retry_of(&value).ok_or_else(|| Problem::invalid(&word)))
        .transpose()?;
    Ok(Request::Printer {
        lpt,
        columns,
        lines,
        retry,
    })
}

fn serial(com: u8, rest: &[u8]) -> Result<Request, Problem> {
    let trimmed = parse::trim(rest);
    if trimmed.is_empty() {
        return Ok(Request::Status(Some(Device::Serial(com))));
    }
    let keyed = trimmed.contains(&b'=');
    let mut values: [Option<(Vec<u8>, Vec<u8>)>; 5] = Default::default();
    if keyed {
        for Keyword { key, value, word } in keywords(trimmed) {
            let slot = match key.as_slice() {
                b"BAUD" => 0,
                b"PARITY" => 1,
                b"DATA" => 2,
                b"STOP" => 3,
                b"RETRY" => 4,
                _ => return Err(Problem::invalid(&word)),
            };
            values[slot] = Some((value, word));
        }
    } else {
        let fields = fields(trimmed);
        if fields.len() > 5 {
            return Err(Problem::Said(text::FIELDS));
        }
        for (slot, field) in values.iter_mut().zip(&fields) {
            *slot = (!field.is_empty()).then(|| (field.to_vec(), field.to_vec()));
        }
    }
    let [baud, parity, data, stop, retry] = values;
    let (baud, baud_word) = baud.ok_or(Problem::Said(text::NEED_BAUD))?;
    let baud = baud_of(&baud).ok_or(Problem::Said(text::BAD_BAUD))?;
    // No machine here has the PS/2 BIOS's extended initialisation, which
    // alone sets these.
    if baud == 19200 {
        return Err(Problem::Unsupported(baud_word));
    }
    let parity = match parity {
        None => b'e',
        Some((value, word)) => match parity_of(&value) {
            Some(b'm' | b's') => return Err(Problem::Unsupported(word)),
            Some(parity) => parity,
            None => return Err(Problem::invalid(&word)),
        },
    };
    let data = match data {
        None => 7,
        Some((value, word)) => match number(&value) {
            Some(5 | 6) => return Err(Problem::Unsupported(word)),
            Some(7) => 7,
            Some(8) => 8,
            _ => return Err(Problem::invalid(&word)),
        },
    };
    let stop = match stop {
        None if baud == 110 => 20,
        None => 10,
        Some((value, word)) => match value.as_slice() {
            b"1" => 10,
            b"2" => 20,
            b"1.5" => return Err(Problem::Unsupported(word)),
            _ => return Err(Problem::invalid(&word)),
        },
    };
    let retry = match retry {
        Some((value, word)) => Some(retry_of(&value).ok_or_else(|| Problem::invalid(&word))?),
        None if keyed => None,
        None => Some(retry::NONE),
    };
    Ok(Request::Serial {
        com,
        framing: Framing {
            baud,
            parity,
            data,
            stop,
        },
        retry,
    })
}
