//! `XCOPY source [destination] [/A | /M] [/D:date] [/P] [/S [/E]] [/V] [/W]`:
//! files, and with `/S` the directories below, copied as DOS 4's `XCOPY`
//! copies them. A destination that names no directory and holds no
//! wildcard is asked about: a file, or a directory to make. `/A` and `/M`
//! take only the files whose archive bit is set, `/M` clearing it; `/D`
//! those dated on or after its date; `/P` each the user says yes to; `/E`
//! makes even the empty directories. Each file copied is listed, and the
//! count told.

mod files;

pub use files::XCopyFiles;

use super::{refuse, switch_parts, Refusal};
use crate::dos::message::{self, decimal_in};
use crate::dos::name::{directory_of, wild};
use crate::dos::program::walk::{self, begin_walk, Walk};
use crate::dos::program::{
    handler_entry, say, Flow, Frame, Id, Int, Key, Program, Returned, Routine, ENDED,
};
use crate::dos::shell::dates::Writing;
use crate::dos::shell::{answered, asciz_at, parse, set_asciz};
use crate::frames::{locals, take, Fault, Outcome};
use alloc::vec::Vec;
use tairix_abi::time::{CivilTime, DosTime};

/// Where `XCOPY`'s state lies in its memory.
pub mod at {
    use crate::dos::program::at::STATE;
    use crate::dos::program::walk::walker;

    /// The switches, the date `/D` gives, and the files copied.
    pub const SWITCHES: u16 = STATE;
    pub const SINCE: u16 = STATE + 0x02;
    pub const COPIED: u16 = STATE + 0x04;
    /// Whether any file matched, the code `XCOPY` ends with, whether VERIFY
    /// was on, whether the destination names a file, and the depth below the
    /// starting directory.
    pub const FOUND: u16 = STATE + 0x08;
    pub const CODE: u16 = STATE + 0x09;
    pub const VERIFY: u16 = STATE + 0x0A;
    pub const INTO_FILE: u16 = STATE + 0x0B;
    /// Whether the files of the directory being copied have their
    /// destination made.
    pub const MADE: u16 = STATE + 0x0C;
    /// The walk's block, and the walk.
    pub const STACK: u16 = STATE + 0x0E;
    pub const WALK: u16 = STATE + 0x10;
    /// The source's directory, canonical and `\`-ended; as the files are
    /// listed; and the names it takes.
    pub const BASE: u16 = WALK + walker::LEN;
    pub const PATH_LEN: u16 = 0x80;
    pub const SHOWN: u16 = BASE + PATH_LEN;
    pub const PATTERN: u16 = SHOWN + PATH_LEN;
    pub const NAME_LEN: u16 = 0x10;
    /// The destination's directory, canonical and `\`-ended, and the name
    /// or pattern it gives the files.
    pub const TARGET: u16 = PATTERN + NAME_LEN;
    pub const NAME: u16 = TARGET + PATH_LEN;
    /// The directory the walk is in, the file copied and the file it is
    /// copied to.
    pub const WALKED: u16 = NAME + NAME_LEN;
    pub const FROM: u16 = WALKED + PATH_LEN;
    pub const TO: u16 = FROM + PATH_LEN;
    /// The search's DTA, and the buffer the copies read through.
    pub const DTA: u16 = TO + PATH_LEN;
    pub const DTA_LEN: u16 = 0x30;
    pub const BUFFER: u16 = DTA + DTA_LEN;
    pub const BUFFER_LEN: u16 = 0x4000;
    /// Bytes of `XCOPY`'s state.
    pub const LEN: u16 = BUFFER + BUFFER_LEN - STATE;
}

/// `XCOPY`'s switches, as [`at::SWITCHES`] keeps them.
pub mod switch {
    pub const ARCHIVED: u16 = 0x01;
    pub const MOVE_ARCHIVE: u16 = 0x02;
    pub const SINCE: u16 = 0x04;
    pub const PROMPT: u16 = 0x08;
    pub const SUBDIRECTORIES: u16 = 0x10;
    pub const EMPTY_TOO: u16 = 0x20;
    pub const VERIFY: u16 = 0x40;
    pub const WAIT: u16 = 0x80;
}

/// The codes `XCOPY` ends with, as DOS 5 documents them.
pub mod code {
    pub const NONE_FOUND: u8 = 1;
    pub const BROKEN: u8 = 2;
    pub const REFUSED: u8 = 4;
    pub const WRITE_FAILED: u8 = 5;
}

/// The handler `XCOPY` installs, by its stub's number.
pub mod handler {
    /// INT 23h while it copies, the work abandoned for the main routine to
    /// end it.
    pub const BREAK: u16 = 0;
}

/// `XCOPY`'s messages, as DOS 4's are worded.
pub mod text {
    pub use crate::dos::message::{
        BAD_DATE, CREATION, DISK_FULL, NO_DIRECTORY, ONTO_ITSELF, PATH_NOT_FOUND,
    };
    pub const FIELDS: &[u8] = b"Invalid number of parameters\r\n";
    pub const CYCLIC: &[u8] = b"Cannot perform a cyclic copy\r\n";
    pub const FROM_DEVICE: &[u8] = b"Cannot XCOPY from a reserved device\r\n";
    pub const TO_DEVICE: &[u8] = b"Cannot XCOPY to a reserved device\r\n";
    pub const READING: &[u8] = b"Reading source file(s)...\r\n";
    pub const KEY: &[u8] = b"Press any key to begin copying file(s)";
}

/// What `XCOPY`'s command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub source: &'a [u8],
    pub destination: Option<&'a [u8]>,
    pub switches: u16,
    pub since: Option<&'a [u8]>,
}

/// Why `XCOPY`'s command line is not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused<'a> {
    Fields,
    Parser(Refusal<'a>),
}

/// `XCOPY`'s command line read: a source, a destination if given, and the
/// switches.
///
/// # Errors
///
/// [`Refused::Fields`] for no source or more than two names, or the parser's
/// refusal of a switch.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refused<'_>> {
    let (mut names, mut switches, mut since) = (Vec::new(), 0, None);
    for word in parse::arguments(tail) {
        let Some(switch) = word.strip_prefix(b"/") else {
            names.push(word);
            continue;
        };
        let (name, value) = switch_parts(switch);
        switches |= match (name.to_ascii_uppercase().as_slice(), value) {
            (b"A", None) => switch::ARCHIVED,
            (b"M", None) => switch::MOVE_ARCHIVE,
            (b"P", None) => switch::PROMPT,
            (b"S", None) => switch::SUBDIRECTORIES,
            (b"E", None) => switch::EMPTY_TOO,
            (b"V", None) => switch::VERIFY,
            (b"W", None) => switch::WAIT,
            (b"D", Some(date)) if !date.is_empty() => {
                since = Some(date);
                switch::SINCE
            }
            _ => return Err(Refused::Parser(Refusal::Switch(word))),
        };
    }
    let (source, destination) = match names[..] {
        [source] => (source, None),
        [source, destination] => (source, Some(destination)),
        _ => return Err(Refused::Fields),
    };
    Ok(Request {
        source,
        destination,
        switches,
        since,
    })
}

/// How a source is listed: as typed, upper case, the name or pattern taken
/// off a file's and a directory's closed with `\`, as DOS 4's `XCOPY`
/// spells it.
#[must_use]
pub fn listed_base(source: &[u8], directory: bool) -> Vec<u8> {
    let upper = source.to_ascii_uppercase();
    if !directory {
        return directory_of(&upper).to_vec();
    }
    match upper.as_slice() {
        [_, b':'] | [_, b':', b'\\'] | [] => upper,
        _ if upper.ends_with(b"\\") => upper,
        _ => [&upper[..], b"\\"].concat(),
    }
}

/// Whether a destination names a directory by its spelling alone: one
/// ending in `\`, a drive, `.` or `..`.
fn spelt_as_directory(path: &[u8]) -> bool {
    let last = path
        .rsplit(|&b| b == b'\\' || b == b':')
        .next()
        .unwrap_or(path);
    path.ends_with(b"\\") || path.ends_with(b":") || last == b"." || last == b".."
}

/// A canonical path `\`-closed, as a directory's is kept.
fn closed(mut path: Vec<u8>) -> Vec<u8> {
    if !path.ends_with(b"\\") {
        path.push(b'\\');
    }
    path
}

/// The country's date format and separator, from its information.
/// The first instant of a civil date.
fn midnight(year: u16, month: u8, day: u8) -> CivilTime {
    CivilTime {
        year: i64::from(year),
        month: u32::from(month),
        day: u32::from(day),
        hour: 0,
        minute: 0,
        second: 0,
    }
}

locals! {
    /// `XCOPY`'s main routine: the key that answered its question, or
    /// [`ENDED`].
    pub struct XCopy {
        key: u16,
    }
}

mod phase {
    pub const SAID: u8 = 1;
    pub const COUNTRY: u8 = 2;
    pub const SOURCE_CANONICAL: u8 = 3;
    pub const SOURCE_KNOWN: u8 = 4;
    pub const TARGET_CANONICAL: u8 = 5;
    pub const TARGET_KNOWN: u8 = 6;
    pub const ASKED: u8 = 7;
    pub const ANSWERED: u8 = 8;
    pub const MADE: u8 = 9;
    pub const GUARDED: u8 = 10;
    pub const VERIFY_READ: u8 = 11;
    pub const VERIFY_SET: u8 = 12;
    pub const WAITED: u8 = 13;
    pub const KEY: u8 = 14;
    pub const STARTED: u8 = 15;
    pub const SHRUNK: u8 = 16;
    pub const BLOCK: u8 = 17;
    pub const TOP_COPIED: u8 = 18;
    pub const WALKED: u8 = 19;
    pub const DIRECTORY_COPIED: u8 = 20;
    pub const RESTORED: u8 = 21;
    pub const COUNTED: u8 = 22;
    pub const ENDED: u8 = 23;
    pub const BROKEN: u8 = 24;
    pub const KEY_SAID: u8 = 25;
    pub const LINE_SAID: u8 = 26;
}

impl XCopy {
    fn requested(p: &mut Program<'_, '_>) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        let tail = p.tail();
        let request = request(&tail).ok()?;
        Some((
            request.source.to_vec(),
            request.destination.map(<[u8]>::to_vec),
        ))
    }

    /// Refused before anything is copied: the reason said, the code 4.
    fn refuse(p: &mut Program<'_, '_>, phase: &mut u8, said: &[u8]) -> Flow {
        p.set8(at::CODE, code::REFUSED);
        *phase = phase::SAID;
        Flow::Call(say(p, said))
    }

    fn canonical(p: &mut Program<'_, '_>, from: u16, into: u16) -> Frame {
        Frame::of(&Int {
            si: from,
            di: into,
            ..Int::dos(p.own, 0x6000)
        })
    }

    /// The question whether the destination names a file or a directory.
    /// The copy abandoned: VERIFY put back, and the count not told, as
    /// DOS's `XCOPY` ends at Ctrl+C.
    fn broken(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::BROKEN;
        if p.get16(at::SWITCHES) & switch::VERIFY == 0 {
            return Flow::Return(Ok(u32::from(code::BROKEN)));
        }
        let was = u16::from(p.get8(at::VERIFY));
        Flow::Call(Frame::of(&Int::dos(p.own, 0x2E00 | was)))
    }

    fn ask(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (_, destination) = Self::requested(p).unwrap_or_default();
        let destination = destination.unwrap_or_default().to_ascii_uppercase();
        let named = &destination[directory_of(&destination).len()..];
        *phase = phase::ASKED;
        let asked = [&b"Does "[..], named, b" specify a file name\r\nor directory name on the target\r\n(F = file, D = directory)?"].concat();
        Flow::Call(say(p, &asked))
    }

    /// The destination known: the copy guarded, then begun.
    fn guard(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (base, target) = (
            asciz_at(p, at::BASE, at::PATH_LEN),
            asciz_at(p, at::TARGET, at::PATH_LEN),
        );
        let tree = p.get16(at::SWITCHES) & (switch::SUBDIRECTORIES | switch::EMPTY_TOO) != 0;
        if tree
            && p.get8(at::INTO_FILE) == 0
            && target
                .to_ascii_uppercase()
                .starts_with(&base.to_ascii_uppercase())
        {
            return Self::refuse(p, phase, text::CYCLIC);
        }
        *phase = phase::GUARDED;
        Flow::Call(Frame::of(&Int {
            dx: handler_entry(handler::BREAK),
            ..Int::dos(p.own, 0x2523)
        }))
    }

    /// The copy begun, the reading told unless one file alone is copied.
    fn start(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let pattern = asciz_at(p, at::PATTERN, at::NAME_LEN);
        let single = !wild(&pattern) && p.get16(at::SWITCHES) & switch::SUBDIRECTORIES == 0;
        *phase = phase::STARTED;
        if single {
            return Self::begin_copy(p, phase);
        }
        Flow::Call(say(p, text::READING))
    }

    /// The starting directory's files copied, the walk's block taken first
    /// for `/S`.
    fn begin_copy(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        if p.get16(at::SWITCHES) & switch::SUBDIRECTORIES == 0 {
            return Self::copy_top(p, phase);
        }
        *phase = phase::SHRUNK;
        let shrink = p.shrink();
        Flow::Call(shrink)
    }

    /// The starting directory's files copied.
    fn copy_top(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let base = asciz_at(p, at::BASE, at::PATH_LEN);
        set_asciz(p, at::WALKED, &base, at::PATH_LEN);
        p.set8(at::MADE, 0);
        *phase = phase::TOP_COPIED;
        Flow::Call(Frame::of(&XCopyFiles::default()))
    }

    /// The copy over: VERIFY put back, the count told, and before it that
    /// nothing was found if nothing was.
    fn finish(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::RESTORED;
        if p.get16(at::SWITCHES) & switch::VERIFY == 0 {
            return Self::count(p, phase);
        }
        let was = u16::from(p.get8(at::VERIFY));
        Flow::Call(Frame::of(&Int::dos(p.own, 0x2E00 | was)))
    }

    fn count(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::COUNTED;
        if p.get32(at::COPIED) == 0 && p.get8(at::FOUND) == 0 {
            p.set8(at::CODE, code::NONE_FOUND);
            let pattern = asciz_at(p, at::PATTERN, at::NAME_LEN);
            let said = message::named(message::FILE_NOT_FOUND, &pattern);
            return Flow::Call(say(p, &said));
        }
        Self::told(p, phase)
    }

    fn told(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let copied = p.get32(at::COPIED);
        *phase = phase::ENDED;
        let said = [&decimal_in(copied, 9)[..], b" File(s) copied\r\n"].concat();
        Flow::Call(say(p, &said))
    }
}

impl Routine for XCopy {
    const ID: Id = Id::XCopy;

    #[allow(
        clippy::too_many_lines,
        reason = "one arm a phase, in the order the copy runs"
    )]
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        if answered(child) {
            p.set8(at::CODE, code::REFUSED);
            return Self::finish(p, phase);
        }
        if child == Err(Fault::Break) {
            return Self::broken(p, phase);
        }
        match *phase {
            0 => {
                p.set8(at::CODE, 0);
                p.set32(at::COPIED, 0);
                p.set8(at::FOUND, 0);
                p.set8(at::INTO_FILE, 0);
                let tail = p.tail();
                let request = match request(&tail) {
                    Ok(request) => request,
                    Err(Refused::Fields) => return Self::refuse(p, phase, text::FIELDS),
                    Err(Refused::Parser(refusal)) => {
                        p.set8(at::CODE, code::REFUSED);
                        *phase = phase::SAID;
                        return Flow::Call(refuse(p, b"", refusal));
                    }
                };
                p.set16(at::SWITCHES, request.switches);
                *phase = phase::COUNTRY;
                if request.since.is_none() {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int {
                    dx: p.scratch(0),
                    ..Int::dos(p.own, 0x3800)
                }))
            }
            phase::COUNTRY => {
                let tail = p.tail();
                let Ok(request) = request(&tail) else {
                    return Flow::Return(Ok(u32::from(code::REFUSED)));
                };
                if let Some(since) = request.since {
                    let info = p.scratch(0);
                    let writing = Writing::read(p, info);
                    let stamp = crate::dos::shell::internal::parse_date(
                        since,
                        writing.order,
                        writing.date_separator,
                    )
                    .filter(|&(_, month, day)| (1..=12).contains(&month) && (1..=31).contains(&day))
                    .and_then(|(year, month, day)| DosTime::of_civil(&midnight(year, month, day)));
                    let Some(stamp) = stamp else {
                        return Self::refuse(p, phase, text::BAD_DATE);
                    };
                    p.set16(at::SINCE, stamp.date);
                }
                set_asciz(p, at::FROM, request.source, at::PATH_LEN);
                *phase = phase::SOURCE_CANONICAL;
                Flow::Call(Self::canonical(p, at::FROM, at::BASE))
            }
            phase::SOURCE_CANONICAL => {
                if child.is_err() {
                    return Self::refuse(p, phase, text::PATH_NOT_FOUND);
                }
                if asciz_at(p, at::BASE, at::PATH_LEN).contains(&b'/') {
                    return Self::refuse(p, phase, text::FROM_DEVICE);
                }
                *phase = phase::SOURCE_KNOWN;
                Flow::Call(Frame::of(&Int {
                    dx: at::FROM,
                    ..Int::dos(p.own, 0x4300)
                }))
            }
            // A source naming a directory copies all its files; else its
            // last name is the files' name or pattern.
            phase::SOURCE_KNOWN => {
                let (source, destination) = Self::requested(p).unwrap_or_default();
                let named_directory = spelt_as_directory(&source)
                    || (child.is_ok() && p.result(Returned::Cx) & 0x10 != 0);
                let canonical = asciz_at(p, at::BASE, at::PATH_LEN);
                let (base, pattern) = if named_directory {
                    (closed(canonical), b"*.*".to_vec())
                } else {
                    let directory = directory_of(&canonical).to_vec();
                    (directory.clone(), canonical[directory.len()..].to_vec())
                };
                set_asciz(p, at::BASE, &base, at::PATH_LEN);
                set_asciz(p, at::PATTERN, &pattern, at::NAME_LEN);
                let listed = listed_base(&source, named_directory);
                set_asciz(p, at::SHOWN, &listed, at::PATH_LEN);
                set_asciz(
                    p,
                    at::FROM,
                    destination.as_deref().unwrap_or(b"."),
                    at::PATH_LEN,
                );
                *phase = phase::TARGET_CANONICAL;
                Flow::Call(Self::canonical(p, at::FROM, at::TARGET))
            }
            phase::TARGET_CANONICAL => {
                if child.is_err() {
                    return Self::refuse(p, phase, text::PATH_NOT_FOUND);
                }
                if asciz_at(p, at::TARGET, at::PATH_LEN).contains(&b'/') {
                    return Self::refuse(p, phase, text::TO_DEVICE);
                }
                let (_, destination) = Self::requested(p).unwrap_or_default();
                if destination.as_deref().is_none_or(spelt_as_directory) {
                    let target = closed(asciz_at(p, at::TARGET, at::PATH_LEN));
                    set_asciz(p, at::TARGET, &target, at::PATH_LEN);
                    return Self::guard(p, phase);
                }
                *phase = phase::TARGET_KNOWN;
                Flow::Call(Frame::of(&Int {
                    dx: at::FROM,
                    ..Int::dos(p.own, 0x4300)
                }))
            }
            // A destination that is no directory: a pattern names the
            // copies; else the user says what it names.
            phase::TARGET_KNOWN => {
                let canonical = asciz_at(p, at::TARGET, at::PATH_LEN);
                if child.is_ok() && p.result(Returned::Cx) & 0x10 != 0 {
                    set_asciz(p, at::TARGET, &closed(canonical), at::PATH_LEN);
                    return Self::guard(p, phase);
                }
                let directory = directory_of(&canonical).to_vec();
                let named = &canonical[directory.len()..];
                if wild(named) {
                    set_asciz(p, at::NAME, named, at::NAME_LEN);
                    set_asciz(p, at::TARGET, &directory, at::PATH_LEN);
                    return Self::guard(p, phase);
                }
                Self::ask(p, phase)
            }
            phase::ASKED => {
                take!(child);
                *phase = phase::ANSWERED;
                Flow::Call(Frame::of(&Key::of(0x0C01)))
            }
            phase::ANSWERED => {
                self.key = u16::try_from(take!(child)).unwrap_or(u16::MAX);
                *phase = phase::LINE_SAID;
                Flow::Call(say(p, b"\r\n"))
            }
            // A file names the copy; a directory is made; an input with no
            // answer left ends the copy as Ctrl+C would; anything else is
            // asked again.
            phase::LINE_SAID => {
                take!(child);
                if u32::from(self.key) >= ENDED {
                    return Self::broken(p, phase);
                }
                let canonical = asciz_at(p, at::TARGET, at::PATH_LEN);
                match u8::try_from(self.key).unwrap_or(0).to_ascii_uppercase() {
                    b'F' => {
                        let directory = directory_of(&canonical).to_vec();
                        set_asciz(p, at::NAME, &canonical[directory.len()..], at::NAME_LEN);
                        set_asciz(p, at::TARGET, &directory, at::PATH_LEN);
                        p.set8(at::INTO_FILE, 1);
                        Self::guard(p, phase)
                    }
                    b'D' => {
                        set_asciz(p, at::FROM, &canonical, at::PATH_LEN);
                        set_asciz(p, at::TARGET, &closed(canonical), at::PATH_LEN);
                        *phase = phase::MADE;
                        Flow::Call(Frame::of(&Int {
                            dx: at::FROM,
                            ..Int::dos(p.own, 0x3900)
                        }))
                    }
                    _ => Self::ask(p, phase),
                }
            }
            phase::MADE => {
                if child.is_err() {
                    return Self::refuse(p, phase, text::NO_DIRECTORY);
                }
                Self::guard(p, phase)
            }
            phase::GUARDED => {
                *phase = phase::VERIFY_READ;
                if p.get16(at::SWITCHES) & switch::VERIFY == 0 {
                    return self.step(p, phase, Ok(0));
                }
                Flow::Call(Frame::of(&Int::dos(p.own, 0x5400)))
            }
            phase::VERIFY_READ => {
                *phase = phase::VERIFY_SET;
                if p.get16(at::SWITCHES) & switch::VERIFY == 0 {
                    return self.step(p, phase, Ok(0));
                }
                p.set8(at::VERIFY, take!(child).to_le_bytes()[0].min(1));
                Flow::Call(Frame::of(&Int::dos(p.own, 0x2E01)))
            }
            phase::VERIFY_SET => {
                if p.get16(at::SWITCHES) & switch::WAIT == 0 {
                    return Self::start(p, phase);
                }
                *phase = phase::WAITED;
                Flow::Call(say(p, text::KEY))
            }
            phase::WAITED => {
                take!(child);
                *phase = phase::KEY;
                Flow::Call(Frame::of(&Int::dos(p.own, 0x0C08)))
            }
            phase::KEY => {
                take!(child);
                *phase = phase::KEY_SAID;
                Flow::Call(say(p, b"\r\n"))
            }
            phase::KEY_SAID => {
                take!(child);
                Self::start(p, phase)
            }
            phase::STARTED => {
                take!(child);
                Self::begin_copy(p, phase)
            }
            phase::SHRUNK => {
                take!(child);
                *phase = phase::BLOCK;
                Flow::Call(Frame::of(&Int {
                    bx: walk::PARAGRAPHS,
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::BLOCK => {
                let segment = take!(child);
                p.set16(at::STACK, u16::try_from(segment).unwrap_or(0));
                Self::copy_top(p, phase)
            }
            // Hidden and system directories are passed by, as their files
            // are.
            phase::TOP_COPIED | phase::DIRECTORY_COPIED => {
                let stopped = take!(child);
                if stopped != 0 || p.get16(at::SWITCHES) & switch::SUBDIRECTORIES == 0 {
                    return Self::finish(p, phase);
                }
                if *phase == phase::TOP_COPIED {
                    let (stack, base) = (p.get16(at::STACK), asciz_at(p, at::BASE, at::PATH_LEN));
                    set_asciz(p, at::WALKED, &base, at::PATH_LEN);
                    begin_walk(p, at::WALK, stack, at::WALKED, at::PATH_LEN, 0x06);
                }
                *phase = phase::WALKED;
                Flow::Call(Frame::of(&Walk::of(at::WALK)))
            }
            phase::WALKED => match take!(child) {
                walk::step::ENTERED => {
                    p.set8(at::MADE, 0);
                    *phase = phase::DIRECTORY_COPIED;
                    Flow::Call(Frame::of(&XCopyFiles::default()))
                }
                walk::step::LEAVING => Flow::Call(Frame::of(&Walk::of(at::WALK))),
                _ => Self::finish(p, phase),
            },
            phase::RESTORED => Self::count(p, phase),
            phase::COUNTED => Self::told(p, phase),
            phase::BROKEN => Flow::Return(Ok(u32::from(code::BROKEN))),
            _ => Flow::Return(Ok(u32::from(p.get8(at::CODE)))),
        }
    }
}

#[cfg(test)]
#[path = "xcopy_tests.rs"]
mod tests;
