//! The recorded screen corpus (`plans/RISCOS-EMULATOR.md` §17): BASIC
//! programs genuine RISC OS 5 ran for `cargo xtask riscos --reference`, and
//! what each left — the readings it took and the screen memory it drew —
//! which the replay holds the drivers to.
//!
//! A case opens `case NAME`; its numbered lines follow, from 10 to 8999.
//! [`program`] frames them: it defines every character from 32 to 255 as
//! `lib/vdu`'s first-party font draws it, so no RISC OS glyph is drawn, then
//! runs the case, then records. The frame gives a case `PROCmode(n)` and
//! `PROCscreen(selector)`, which select a mode, define the font again and
//! turn the cursor off, so no screen holds the cursor's flashing;
//! `PROCr("NAME",value)`, which records a reading in hexadecimal;
//! `FNm(n)`, `FNv(n)` and `FNs(n)`, a mode variable, a VDU variable, and an
//! address VDU variable less the screen area's base; `FNe(e,f)`, the error
//! number a `SYS ... TO e;f` gave, or -1; and `PROCa("NAME",a)`, which
//! records the words of sprite area `a` from its start to its free space,
//! eight a line, as offsets never being addresses, which the two machines
//! give apart. At the end, or at an
//! error the case did not trap, whose number alone is recorded, the cursor is
//! turned off, so its flashing is in no screen, and the screen memory the VDU
//! drivers write is saved — unless the mode is Teletext, whose glyphs would
//! be RISC OS's, or a character's definition no longer matches the font's,
//! which refuses the recording.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use tairix_riscos_kernel::corpus::{answers_head, hex_bytes, listings};
use tairix_riscos_kernel::os::len32;

/// The case's lines: the first and last numbers a case may use.
const FIRST_LINE: u32 = 10;
const LAST_LINE: u32 = 8999;
/// Where the font's definitions are, as `DATA`.
const FONT_LINE: u32 = 9900;
/// The glyphs each `DATA` line defines.
const GLYPHS_PER_LINE: usize = 4;

/// A case of the corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    /// Its name.
    pub name: String,
    /// Its numbered lines, a newline ending each.
    pub text: String,
}

/// The cases `corpus` holds.
///
/// # Errors
///
/// The line of the corpus that is neither a case's name nor one of its
/// lines, and why.
pub fn cases(corpus: &str) -> Result<Vec<Case>, String> {
    Ok(
        listings::<()>(corpus, "case", FIRST_LINE..=LAST_LINE, None)?
            .into_iter()
            .map(|listing| Case {
                name: listing.name,
                text: listing.text,
            })
            .collect(),
    )
}

/// The HostFS name case `index`'s program is staged under.
#[must_use]
pub fn program_name(index: usize) -> String {
    format!("Scr{index}")
}

/// The command that runs the program staged as `name`.
#[must_use]
pub fn run_command(name: &str) -> String {
    format!("BASIC -quit HostFS:$.{name}")
}

/// The HostFS name case `index` records its readings under.
#[must_use]
pub fn record_name(index: usize) -> String {
    format!("ScrRec{index}")
}

/// The HostFS name case `index` saves its screen under.
#[must_use]
pub fn dump_name(index: usize) -> String {
    format!("ScrDump{index}")
}

/// The whole program case `index` is run as: the font defined, the case,
/// then its record.
#[must_use]
pub fn program(case: &Case, index: usize) -> String {
    let record = record_name(index);
    let dump = dump_name(index);
    let mut text = String::new();
    let _ = writeln!(text, "1 R%=OPENOUT \"HostFS:$.{record}\"");
    let _ = writeln!(
        text,
        "2 ON ERROR ON ERROR OFF:BPUT#R%,\"error \"+STR$~ERR:PROCdump:CLOSE#R%:END"
    );
    let _ = writeln!(text, "3 PROCfont");
    text.push_str(&case.text);
    let _ = writeln!(text, "9000 PROCdump:CLOSE#R%:END");
    let _ = writeln!(
        text,
        "9010 DEFPROCr(n$,v%):BPUT#R%,n$+\" \"+STR$~v%:ENDPROC"
    );
    let _ = writeln!(
        text,
        "9020 DEFFNm(v%):LOCAL x%:SYS \"OS_ReadModeVariable\",-1,v% TO ,,x%:=x%"
    );
    let _ = writeln!(
        text,
        "9030 DEFFNv(v%):LOCAL b%:DIM b% LOCAL 8:b%!0=v%:b%!4=-1:SYS \"OS_ReadVduVariables\",b%,b%:=b%!0"
    );
    let _ = writeln!(
        text,
        "9040 DEFPROCfont:LOCAL c%,i%,d%:RESTORE {FONT_LINE}:FOR c%=32 TO 255:VDU 23,c%:FOR i%=0 TO 7:READ d%:VDU d%:NEXT:NEXT:ENDPROC"
    );
    let _ = writeln!(
        text,
        "9050 DEFFNfont:LOCAL c%,i%,d%,b%,k%:DIM b% LOCAL 12:k%=TRUE:RESTORE {FONT_LINE}:FOR c%=32 TO 255:?b%=c%:SYS \"OS_Word\",10,b%:FOR i%=0 TO 7:READ d%:IF b%?(i%+1)<>d% THEN k%=FALSE"
    );
    let _ = writeln!(text, "9060 NEXT:NEXT:=k%");
    let _ = writeln!(
        text,
        "9070 DEFPROCdump:LOCAL b%:VDU 23,1,0;0;0;0;:IF FNm(0) AND 2 THEN BPUT#R%,\"teletext\":ENDPROC"
    );
    let _ = writeln!(
        text,
        "9080 IF NOT FNfont THEN BPUT#R%,\"font lost\":ENDPROC"
    );
    let _ = writeln!(
        text,
        "9090 BPUT#R%,\"screen \"+STR$(FNm(11)+1)+\" \"+STR$(FNm(12)+1)+\" \"+STR$FNm(9)+\" \"+STR$FNm(6)"
    );
    let _ = writeln!(
        text,
        "9100 b%=FNv(148):SYS \"OS_File\",10,\"HostFS:$.{dump}\",&FFD,,b%,b%+FNm(6)*(FNm(12)+1):ENDPROC"
    );
    let _ = writeln!(
        text,
        "9110 DEFPROCmode(m%):MODE m%:PROCfont:VDU 23,1,0;0;0;0;:ENDPROC"
    );
    let _ = writeln!(
        text,
        "9120 DEFPROCscreen(s%):SYS \"OS_ScreenMode\",0,s%:PROCfont:VDU 23,1,0;0;0;0;:ENDPROC"
    );
    let _ = writeln!(
        text,
        "9130 DEFFNs(v%):LOCAL b%:SYS \"OS_ReadDynamicArea\",2 TO b%:=FNv(v%)-b%"
    );
    let _ = writeln!(text, "9140 DEFFNe(e%,f%):IF f% AND 1 THEN =!e% ELSE =-1");
    let _ = writeln!(
        text,
        "9150 DEFPROCa(n$,a%):LOCAL i%,j%,S$:FOR i%=0 TO a%!12-4 STEP 32:S$=n$+\" \"+STR$~i%+\":\""
    );
    let _ = writeln!(
        text,
        "9160 FOR j%=i% TO i%+28 STEP 4:IF j%<a%!12 THEN S$=S$+\" \"+STR$~(a%!j%)"
    );
    let _ = writeln!(text, "9170 NEXT:BPUT#R%,S$:NEXT:ENDPROC");
    for (row, chunk) in tairix_vdu::font::RISC_OS
        .chunks(GLYPHS_PER_LINE)
        .enumerate()
    {
        let _ = write!(text, "{} DATA ", FONT_LINE + 10 * len32(row));
        let mut first = true;
        for byte in chunk.iter().flatten() {
            if !first {
                text.push(',');
            }
            first = false;
            let _ = write!(text, "{byte}");
        }
        text.push('\n');
    }
    text
}

/// The screen a case saved: its size, its depth and its lines' length,
/// then its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// The bits a pixel takes, as a power of two.
    pub log2_bpp: u32,
    /// The bytes a line of it takes.
    pub line_length: u32,
    /// Its memory, the top line first.
    pub bytes: Vec<u8>,
}

/// What a case recorded.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Recorded {
    /// Each reading or note, in the order taken: its name and value, or a
    /// note alone.
    pub lines: Vec<String>,
    /// The screen it saved, if it saved one.
    pub screen: Option<Screen>,
}

/// What a run of case `index` left: `record` its record file's text and
/// `dump` the screen it saved, if any.
///
/// # Errors
///
/// A record that is not lines of text, a screen named but not saved or
/// saved short, or a font found redefined.
pub fn recorded(record: &[u8], dump: Option<&[u8]>) -> Result<Recorded, String> {
    let text = core::str::from_utf8(record).map_err(|_| String::from("a record of text"))?;
    let mut out = Recorded::default();
    for line in text.lines() {
        if line == "font lost" {
            return Err("a character's definition no longer matches the font".into());
        }
        if let Some(size) = line.strip_prefix("screen ") {
            let numbers: Vec<u32> = size.split(' ').filter_map(|n| n.parse().ok()).collect();
            let [width, height, log2_bpp, line_length] = numbers[..] else {
                return Err(format!("a screen's size, not {line:?}"));
            };
            let bytes = dump.ok_or_else(|| String::from("a screen saved"))?;
            let length = usize::try_from(u64::from(line_length) * u64::from(height))
                .map_err(|_| String::from("a screen of reasonable size"))?;
            if bytes.len() != length {
                return Err(format!("a screen of {length} bytes, not {}", bytes.len()));
            }
            out.screen = Some(Screen {
                width,
                height,
                log2_bpp,
                line_length,
                bytes: bytes.to_vec(),
            });
            continue;
        }
        out.lines.push(line.to_string());
    }
    Ok(out)
}

/// The answers file: the recording's provenance, then what each case
/// recorded, each screen line not wholly zero as its number and its bytes —
/// a run of lines alike as the first and last numbers, a run of three or
/// more pixels alike as `NxHH`.
#[must_use]
pub fn answers(provenance: &[String], cases: &[Case], recorded: &[Recorded]) -> String {
    let mut out = answers_head(provenance);
    for (case, recorded) in cases.iter().zip(recorded) {
        let _ = writeln!(out, "case {}", case.name);
        for line in &recorded.lines {
            let _ = writeln!(out, "r {line}");
        }
        if let Some(screen) = &recorded.screen {
            let _ = writeln!(
                out,
                "screen {} {} {} {}",
                screen.width, screen.height, screen.log2_bpp, screen.line_length
            );
            let stride = screen.line_length as usize;
            let unit = unit(screen.log2_bpp);
            let rows: Vec<&[u8]> = screen.bytes.chunks(stride.max(1)).collect();
            let mut first = 0;
            while first < rows.len() {
                let last = first
                    + rows[first..]
                        .iter()
                        .take_while(|&&r| r == rows[first])
                        .count()
                    - 1;
                if rows[first].iter().any(|&b| b != 0) {
                    let span = if last > first {
                        format!("{first}-{last}")
                    } else {
                        format!("{first}")
                    };
                    let _ = writeln!(out, "row {span} {}", encode_row(rows[first], unit));
                }
                first = last + 1;
            }
        }
    }
    out
}

/// The bytes a pixel of a screen `log2_bpp` deep takes, at least one: the
/// unit its lines are written in.
#[must_use]
pub const fn unit(log2_bpp: u32) -> usize {
    1 << log2_bpp.saturating_sub(3)
}

/// How a replayed case's recording differs from genuine RISC OS's, if it
/// does: the first reading that differs, else the screen's shape or its
/// first line that differs.
#[must_use]
pub fn difference(ours: &Recorded, genuine: &Recorded) -> Option<String> {
    if ours.lines != genuine.lines {
        let first = ours
            .lines
            .iter()
            .zip(&genuine.lines)
            .position(|(a, b)| a != b)
            .unwrap_or(ours.lines.len().min(genuine.lines.len()));
        return Some(format!(
            "reading {first}: {:?} where genuine RISC OS read {:?}",
            ours.lines.get(first),
            genuine.lines.get(first)
        ));
    }
    match (&ours.screen, &genuine.screen) {
        (Some(a), Some(b)) if a == b => None,
        (Some(a), Some(b)) => {
            let shape = |s: &Screen| (s.width, s.height, s.log2_bpp, s.line_length);
            if shape(a) != shape(b) {
                return Some(format!(
                    "a screen of {:?} where genuine RISC OS had {:?}",
                    shape(a),
                    shape(b)
                ));
            }
            let stride = (b.line_length as usize).max(1);
            let mut differing = a
                .bytes
                .chunks(stride)
                .zip(b.bytes.chunks(stride))
                .enumerate()
                .filter(|(_, (x, y))| x != y);
            let (first, (x, y)) = differing.next()?;
            let unit = unit(b.log2_bpp);
            Some(format!(
                "{} lines differ, the first {first}:\n  ours    {}\n  genuine {}",
                1 + differing.count(),
                encode_row(x, unit),
                encode_row(y, unit)
            ))
        }
        (a, b) => Some(format!(
            "a screen {} where genuine RISC OS {}",
            if a.is_some() { "saved" } else { "not saved" },
            if b.is_some() {
                "saved one"
            } else {
                "saved none"
            }
        )),
    }
}

/// A screen line's bytes in `unit`s: each run of three or more alike as
/// `NxHH…`, the units between as hexadecimal, each part set apart by a
/// space.
#[must_use]
pub fn encode_row(bytes: &[u8], unit: usize) -> String {
    let mut out = String::new();
    let mut literal = false;
    let units: Vec<&[u8]> = bytes.chunks(unit.max(1)).collect();
    let mut at = 0;
    while at < units.len() {
        let run = units[at..].iter().take_while(|&&u| u == units[at]).count();
        let parted = !out.is_empty();
        if run >= 3 {
            if parted {
                out.push(' ');
            }
            let _ = write!(out, "{run}x");
            hex(&mut out, units[at]);
            literal = false;
        } else {
            if !literal && parted {
                out.push(' ');
            }
            for &each in &units[at..at + run] {
                hex(&mut out, each);
            }
            literal = true;
        }
        at += run;
    }
    out
}

fn hex(out: &mut String, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(out, "{byte:02X}");
    }
}

/// The bytes a screen line's text in `unit`s gives, at most `length` of
/// them.
///
/// # Errors
///
/// Text that is neither hexadecimal nor runs of whole units.
pub fn decode_row(text: &str, length: usize, unit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        let (count, digits) = match word.split_once('x') {
            Some((count, digits)) => {
                let count: usize = count.parse().map_err(|_| format!("a run, not {word:?}"))?;
                (count, digits)
            }
            None => (1, word),
        };
        let bytes = hex_bytes(digits).ok_or_else(|| format!("hexadecimal, not {word:?}"))?;
        let whole = if count == 1 {
            bytes.len() % unit.max(1) == 0
        } else {
            bytes.len() == unit.max(1)
        };
        let within = count
            .checked_mul(bytes.len())
            .and_then(|run| run.checked_add(out.len()))
            .is_some_and(|end| end <= length);
        if !whole || !within {
            return Err(format!("whole units within the line, not {word:?}"));
        }
        for _ in 0..count {
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

/// A case's answer, as the answers file gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The case's name.
    pub name: String,
    /// What it recorded, its screen's lines laid out whole.
    pub recorded: Recorded,
}

/// The answers `text` gives.
///
/// # Errors
///
/// A line that is no part of an answer, and why.
pub fn parse_answers(text: &str) -> Result<Vec<Answer>, String> {
    let mut answers: Vec<Answer> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let refused = |reason: &str| format!("line {}: {reason}", index + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix("case ") {
            answers.push(Answer {
                name: name.to_string(),
                recorded: Recorded::default(),
            });
            continue;
        }
        let Some(answer) = answers.last_mut() else {
            return Err(refused("a line within a case"));
        };
        if let Some(reading) = line.strip_prefix("r ") {
            answer.recorded.lines.push(reading.to_string());
        } else if let Some(size) = line.strip_prefix("screen ") {
            let numbers: Vec<u32> = size.split(' ').filter_map(|n| n.parse().ok()).collect();
            let [width, height, log2_bpp, line_length] = numbers[..] else {
                return Err(refused("a screen's size"));
            };
            let length = (line_length as usize)
                .checked_mul(height as usize)
                .filter(|&n| n <= 64 << 20)
                .ok_or_else(|| refused("a screen of reasonable size"))?;
            answer.recorded.screen = Some(Screen {
                width,
                height,
                log2_bpp,
                line_length,
                bytes: alloc::vec![0; length],
            });
        } else if let Some(rest) = line.strip_prefix("row ") {
            let Some(screen) = answer.recorded.screen.as_mut() else {
                return Err(refused("a screen's line after its size"));
            };
            let (rows, data) = rest
                .split_once(' ')
                .ok_or_else(|| refused("a line's number"))?;
            let (first, last) = match rows.split_once('-') {
                Some((first, last)) => (first.parse(), last.parse()),
                None => (rows.parse(), rows.parse()),
            };
            let (Ok(first), Ok(last)) = (first, last) else {
                return Err(refused("a line's number, or the first and last of a run"));
            };
            let stride = screen.line_length as usize;
            let bytes = decode_row(data, stride, unit(screen.log2_bpp)).map_err(|e| refused(&e))?;
            let within = |row: usize| {
                row.checked_mul(stride)
                    .filter(|&s| s + bytes.len() <= screen.bytes.len())
            };
            if first > last || within(last).is_none() {
                return Err(refused("lines within the screen"));
            }
            for row in first..=last {
                let start = row * stride;
                screen.bytes[start..start + bytes.len()].copy_from_slice(&bytes);
            }
        } else {
            return Err(refused("a reading, a screen or a line of one"));
        }
    }
    Ok(answers)
}

#[cfg(test)]
mod tests;
