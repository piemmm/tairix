//! The reference harness's part for `lib/openbasic`'s BASIC V: each program in
//! [`PROGRAMS`] typed into genuine RISC OS 5's BASIC V, saved and listed, and
//! what BASIC made of it written to [`ANSWERS`], which `lib/openbasic`'s replay
//! holds the crate to.
//!
//! A program opens `program ENTRY LISTO...`: the `LISTO` its lines are typed
//! under, then each it is listed under. `line "TEXT"` is a numbered line
//! typed, and `poke "STATEMENT"` a statement typed once the lines are, the
//! program then saved and listed as it left it; both are printable ASCII but
//! for `\xHH`. Ahead of them goes [`CALIBRATION`], a line for each message
//! BASIC gives as a line is typed and one where `LISTO 8`'s listing stops:
//! every later message is told apart by matching it against those, so none
//! of their text leaves the machine.
//!
//! The answers echo each program and line. After a line, `= stored B` and
//! `warns`, its warnings or `none`; or `= refused long`. After each `LISTO`,
//! `= B`, the listing, every line of it ending LF CR, then `stopped N` where
//! it stopped at a line reference in line `N`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use tairix_openbasic::program;

use super::{find, quote};

/// The programs, from the workspace's root.
pub(super) const PROGRAMS: &str = "lib/openbasic/tests/riscos5/programs.txt";
/// The answers the harness records.
pub(super) const ANSWERS: &str = "lib/openbasic/tests/riscos5/answers.txt";
/// The HostFS name BASIC's input is staged under.
pub(super) const TYPED: &str = "Typed";
/// The `!Boot` command that types it; BASIC quits at its end.
pub(super) const RUN: &str = "BASIC { < HostFS:$.Typed }";

/// The most of a line BASIC's line editor takes: it rings for any more.
const MOST_TYPED: usize = 238;

/// What BASIC says as a line is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Message {
    Quote,
    Brackets,
    LineNumber,
    Long,
}

impl Message {
    const fn name(self) -> &'static str {
        match self {
            Self::Quote => "quote",
            Self::Brackets => "brackets",
            Self::LineNumber => "line",
            Self::Long => "long",
        }
    }
}

/// A line for each message, which gives it and nothing else: an open string,
/// a bracket closed unopened, a line number past the last, references that
/// tokenise to more than a line holds; then [`STOPPING`], which gives none.
const CALIBRATION: [(&str, Option<Message>); 5] = [
    ("1PRINT\"", Some(Message::Quote)),
    ("2PRINT)", Some(Message::Brackets)),
    ("3GOTO 65280", Some(Message::LineNumber)),
    (
        "4GOTO1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
        Some(Message::Long),
    ),
    ("5GOTO 5", None),
];

/// The calibration line `LISTO 8`'s listing stops at, by its number.
const STOPPING: u16 = 5;

/// The `LISTO` that lists no line numbers, and stops at a reference.
const NO_NUMBERS: u8 = 8;

/// How BASIC ends an error's report: the line it was met in.
const AT_LINE: &[u8] = b" at line ";

/// A program to type.
pub(super) struct Program {
    entry: u8,
    listos: Vec<u8>,
    lines: Vec<Vec<u8>>,
    pokes: Vec<Vec<u8>>,
}

/// The programs `text` holds, the calibration first.
pub(super) fn programs(text: &str) -> Result<Vec<Program>, String> {
    let mut programs = vec![Program {
        entry: 0,
        listos: vec![0, NO_NUMBERS],
        lines: CALIBRATION
            .iter()
            .map(|(line, _)| line.as_bytes().to_vec())
            .collect(),
        pokes: Vec::new(),
    }];
    for (index, line) in text.lines().enumerate() {
        let refused = |reason: &str| format!("riscos: {PROGRAMS}:{}: {reason}", index + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(numbers) = line.strip_prefix("program ") {
            let numbers: Vec<u8> = numbers
                .split(' ')
                .map(|n| {
                    n.parse()
                        .map_err(|_| refused("a LISTO is a number below 256"))
                })
                .collect::<Result<_, _>>()?;
            let [entry, listos @ ..] = numbers.as_slice() else {
                return Err(refused("a program names its LISTOs"));
            };
            programs.push(Program {
                entry: *entry,
                listos: listos.to_vec(),
                lines: Vec::new(),
                pokes: Vec::new(),
            });
            continue;
        }
        let (kind, rest) = line
            .split_once(' ')
            .ok_or_else(|| refused("a kind and a text"))?;
        let [text] = super::quoted(rest)
            .try_into()
            .map_err(|_| refused("one quoted text"))?;
        if text.len() > MOST_TYPED || text.iter().any(|&b| b < b' ') {
            return Err(refused("a text the line editor takes whole"));
        }
        let within = programs.len() > 1;
        let Some(program) = programs.last_mut().filter(|_| within) else {
            return Err(refused("a text within a program"));
        };
        match kind {
            "line" if text.first().is_some_and(u8::is_ascii_digit) => program.lines.push(text),
            "poke" => program.pokes.push(text),
            _ => return Err(refused("a numbered line or a poke")),
        }
    }
    Ok(programs)
}

/// Each HostFS file a run leaves: what was typed, and each program's entry,
/// image and listings.
pub(super) fn left(programs: &[Program]) -> Vec<String> {
    let mut names = vec![TYPED.to_string()];
    for (n, program) in programs.iter().enumerate() {
        names.push(format!("Entry{n}"));
        names.push(format!("Tok{n}"));
        names.extend(
            program
                .listos
                .iter()
                .map(|listo| format!("List{n}x{listo}")),
        );
    }
    names
}

/// What BASIC is to read: each program typed with its output spooled, saved
/// and listed, then `QUIT`.
pub(super) fn typed(programs: &[Program]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |line: &[u8]| {
        out.extend_from_slice(line);
        out.push(b'\n');
    };
    for (n, program) in programs.iter().enumerate() {
        put(b"NEW");
        put(format!("LISTO {}", program.entry).as_bytes());
        put(format!("*Spool HostFS:$.Entry{n}").as_bytes());
        for line in &program.lines {
            put(line);
        }
        put(b"*Spool");
        for poke in &program.pokes {
            put(poke);
        }
        put(format!("SAVE \"HostFS:$.Tok{n}\"").as_bytes());
        for listo in &program.listos {
            put(format!("LISTO {listo}").as_bytes());
            put(format!("*Spool HostFS:$.List{n}x{listo}").as_bytes());
            put(b"LIST");
            put(b"*Spool");
        }
    }
    put(b"QUIT");
    out
}

/// The output BASIC spooled to the HostFS file `name`, a line an entry, each
/// without the LF CR that ended it.
fn spooled(hostfs: &Path, name: &str) -> Result<Vec<Vec<u8>>, String> {
    let path = find(hostfs, name).ok_or_else(|| format!("riscos: BASIC left no {name}"))?;
    let bytes = std::fs::read(&path).map_err(|e| format!("riscos: {}: {e}", path.display()))?;
    let body = bytes
        .strip_suffix(b"\n\r")
        .ok_or_else(|| format!("riscos: {name} does not end a line"))?;
    Ok(body
        .split(|&b| b == b'\n')
        .enumerate()
        .map(|(at, line)| {
            if at == 0 {
                line
            } else {
                line.strip_prefix(b"\r").unwrap_or(line)
            }
            .to_vec()
        })
        .collect())
}

/// Each typed line's messages: in `entry`, the spooled typing, what came
/// between its echo and the next, the closing `*Spool`'s.
fn messages(entry: &[Vec<u8>], lines: &[Vec<u8>], name: &str) -> Result<Vec<Vec<Vec<u8>>>, String> {
    let mut said = Vec::with_capacity(lines.len());
    let mut at = 0;
    for line in lines.iter().map(Vec::as_slice).chain([&b"*Spool"[..]]) {
        match entry.get(at) {
            Some(echo) if echo.first() == Some(&b'>') && echo[1..] == *line => at += 1,
            echo => {
                return Err(format!(
                    "riscos: {name} echoed {} where {} was typed",
                    echo.map_or_else(|| "nothing".into(), |echo| quote(echo)),
                    quote(line)
                ))
            }
        }
        let start = at;
        while entry
            .get(at)
            .is_some_and(|line| line.first() != Some(&b'>'))
        {
            at += 1;
        }
        said.push(entry[start..at].to_vec());
    }
    if said.pop().is_some_and(|closing| !closing.is_empty()) || at != entry.len() {
        return Err(format!("riscos: {name} goes on past its closing *Spool"));
    }
    Ok(said)
}

/// The lines BASIC printed for one message.
type Said = Vec<Vec<u8>>;

/// The messages `said` after one line, each told by the calibration's: a run
/// of its lines, an error's report opening with an empty one.
fn told(said: &[Vec<u8>], known: &[(Message, Said)], name: &str) -> Result<Vec<Message>, String> {
    let mut messages = Vec::new();
    let mut rest = said;
    while !rest.is_empty() {
        let (message, lines) = known
            .iter()
            .find(|(_, lines)| rest.starts_with(lines))
            .ok_or_else(|| format!("riscos: {name} said what no calibration line did"))?;
        messages.push(*message);
        rest = &rest[lines.len()..];
    }
    Ok(messages)
}

/// An error's report split at its line: what it said, and the line.
fn report_at(report: &[u8]) -> Option<(&[u8], u16)> {
    let at = report.windows(AT_LINE.len()).rposition(|w| w == AT_LINE)?;
    let line = std::str::from_utf8(&report[at + AT_LINE.len()..])
        .ok()?
        .parse()
        .ok()?;
    Some((&report[..at], line))
}

/// The number a typed line starts with.
fn number(line: &[u8]) -> Option<u16> {
    let digits = line.iter().take_while(|b| b.is_ascii_digit()).count();
    std::str::from_utf8(&line[..digits]).ok()?.parse().ok()
}

/// The answers file: the recording's provenance, then what BASIC made of each
/// program, from the files it left in `hostfs`.
pub(super) fn answers(
    provenance: &[String],
    programs: &[Program],
    hostfs: &Path,
) -> Result<String, String> {
    let mut out = String::new();
    for line in provenance {
        let _ = writeln!(out, "# {line}");
    }
    out.push_str("# Never edit by hand.\n");
    let mut known: Vec<(Message, Said)> = Vec::new();
    let mut stop: Option<Vec<u8>> = None;
    for (n, program) in programs.iter().enumerate() {
        let entry = format!("Entry{n}");
        let said = messages(&spooled(hostfs, &entry)?, &program.lines, &entry)?;
        if n == 0 {
            for ((line, message), said) in CALIBRATION.iter().zip(&said) {
                match message {
                    Some(message) if !said.is_empty() => known.push((*message, said.clone())),
                    None if said.is_empty() => {}
                    _ => {
                        return Err(format!(
                            "riscos: the calibration line {line:?} said {} lines",
                            said.len()
                        ))
                    }
                }
            }
        }
        let image = find(hostfs, &format!("Tok{n}"))
            .ok_or_else(|| format!("riscos: BASIC saved no Tok{n}"))?;
        let image =
            std::fs::read(&image).map_err(|e| format!("riscos: {}: {e}", image.display()))?;
        let stored: BTreeMap<u16, &[u8]> = program::lines(&image)
            .map(|line| line.map(|line| (line.number, line.text)))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("riscos: Tok{n}: {e:?}"))?;
        let listos: Vec<String> = program.listos.iter().map(u8::to_string).collect();
        let _ = writeln!(out, "program {} {}", program.entry, listos.join(" "));
        for (line, said) in program.lines.iter().zip(&said) {
            let _ = writeln!(out, "line {}", quote(line));
            let warned = told(said, &known, &entry)?;
            if warned.contains(&Message::Long) {
                out.push_str("= refused long\n");
                continue;
            }
            let text = number(line)
                .and_then(|number| stored.get(&number))
                .ok_or_else(|| format!("riscos: Tok{n} holds no line {}", quote(line)))?;
            let warned: Vec<&str> = warned.iter().map(|message| message.name()).collect();
            let warned = if warned.is_empty() {
                "none".into()
            } else {
                warned.join(" ")
            };
            let _ = writeln!(out, "= stored {} warns {warned}", quote(text));
        }
        for &listo in &program.listos {
            let name = format!("List{n}x{listo}");
            let listed = spooled(hostfs, &name)?;
            let [first, listing @ .., last] = listed.as_slice() else {
                return Err(format!("riscos: {name} holds no listing"));
            };
            let mut body = listing;
            if first.as_slice() != b">LIST" || last.as_slice() != b">*Spool" {
                return Err(format!("riscos: {name} is not one LIST"));
            }
            let mut stopped = None;
            if n == 0 && listo == NO_NUMBERS {
                let [listing @ .., report] = body else {
                    return Err(format!("riscos: {name} did not stop"));
                };
                let (text, at) = report_at(report)
                    .filter(|&(_, at)| at == STOPPING)
                    .ok_or_else(|| format!("riscos: {name} stopped at no calibrated line"))?;
                stop = Some(text.to_vec());
                (body, stopped) = (listing, Some(at));
            } else if let [listing @ .., report] = body {
                if let Some((text, at)) = report_at(report) {
                    if stop.as_deref() == Some(text) {
                        (body, stopped) = (listing, Some(at));
                    }
                }
            }
            let mut listing = Vec::new();
            for line in body {
                listing.extend_from_slice(line);
                listing.extend_from_slice(b"\n\r");
            }
            let _ = write!(out, "listo {listo}\n= {}", quote(&listing));
            match stopped {
                Some(at) => {
                    let _ = writeln!(out, " stopped {at}");
                }
                None => out.push('\n'),
            }
        }
    }
    Ok(out)
}
