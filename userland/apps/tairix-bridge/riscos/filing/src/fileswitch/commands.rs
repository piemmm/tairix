//! FileSwitch's `*` commands and configuration keywords
//! (`plans/RISCOS-EMULATOR.md` §6), each the call it stands for: the names
//! a command is given are read where they stand in its tail, as FileSwitch
//! reads any name, and `*Copy`, `*Wipe` and `*Count` take their options from
//! their variables, or their defaults where those are unset, then from the
//! command.

use alloc::vec::Vec;

use tairix_riscos_kernel::cmos::{FILESWITCH_OPTIONS, FILE_LANG};
use tairix_riscos_kernel::error::{Error, BAD_COMMAND, BAD_NUMBER};
use tairix_riscos_kernel::module::{Command, CommandKind, Help};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::read_unsigned::{BYTE_SIZED, DECIMAL, TERMINATED};
use tairix_riscos_kernel::swi::os::{
    self as swi, file as file_reason, find, fs_control as reason, gbpb,
};
use tairix_riscos_kernel::swi::X;

use super::tree::flag;
use super::{error, with, COPY_OPTIONS, COUNT_OPTIONS, WIPE_OPTIONS};

/// FileSwitch's option bit for a name too long for its filing system: cut
/// short where set, refused where clear.
const TRUNCATE_BIT: u8 = 1 << 0;
/// The column a status line's value starts at.
const VALUE_COLUMN: usize = 11;

/// "Bad option": an option letter the command does not take.
const BAD_OPTION: Error = Error {
    number: 0x601,
    token: "BadOption",
};

/// A command of FileSwitch's.
const fn command(
    name: &'static str,
    (min, max): (u8, u8),
    help: &'static str,
    syntax: &'static str,
    run: tairix_riscos_kernel::module::Code,
) -> Command {
    Command {
        name,
        kind: CommandKind::Command,
        min,
        max,
        translate: 0,
        help: Help::Text(help),
        syntax: Some(syntax),
        run: Some(run),
    }
}

/// A configuration keyword of FileSwitch's.
const fn configure(
    name: &'static str,
    help: &'static str,
    run: tairix_riscos_kernel::module::Code,
) -> Command {
    Command {
        name,
        kind: CommandKind::Configure,
        min: 0,
        max: 1,
        translate: 0,
        help: Help::Text(help),
        syntax: None,
        run: Some(run),
    }
}

/// FileSwitch's commands, in its command table's order.
pub(crate) static COMMANDS: [Command; 28] = [
    command("Access", (1, 2), "*Access sets the attributes of the objects that match: L locks, W and R give the owner write and read access, and those after a / the public's.", "Syntax: *Access <object spec> [<attributes>]", access),
    command("Back", (0, 0), "*Back swaps the current and previous directories.", "Syntax: *Back", back),
    command("Cat", (0, 1), "*Cat lists the objects in a directory, the current one if none is given.", "Syntax: *Cat [<directory>]", cat),
    command("CDir", (1, 2), "*CDir makes a directory, with room for the entries given where the filing system uses that.", "Syntax: *CDir <directory> [<size in entries>]", cdir),
    configure("FileSystem", "*Configure FileSystem sets the filing system selected when the machine starts.", configure_file_system),
    configure("Truncate", "*Configure Truncate sets whether a name too long for its filing system is cut short rather than refused.", configure_truncate),
    command("Copy", (2, 255), "*Copy copies the objects that match to the destination given, as Copy$Options and the options given ask.", "Syntax: *Copy <source spec> <destination spec> [[~]<options>]", copy),
    command("Count", (1, 255), "*Count adds up the sizes of the files that match, as Count$Options and the options given ask.", "Syntax: *Count <object spec> [[~]<options>]", count),
    command("Dir", (0, 1), "*Dir makes a directory current, the user root directory if none is given.", "Syntax: *Dir [<directory>]", dir),
    command("EnumDir", (2, 3), "*EnumDir writes the names in a directory that match the pattern, all of them if none is given, to a file.", "Syntax: *EnumDir <directory> <output file> [<pattern>]", enum_dir),
    command("Ex", (0, 1), "*Ex lists the objects in a directory with their information.", "Syntax: *Ex [<directory>]", ex),
    command("FileInfo", (1, 1), "*FileInfo gives the objects that match their full information.", "Syntax: *FileInfo <object spec>", file_info),
    command("Info", (1, 1), "*Info gives the objects that match their information.", "Syntax: *Info <object spec>", info),
    command("LCat", (0, 1), "*LCat lists the objects in a directory of the library, the library itself if none is given.", "Syntax: *LCat [<directory>]", lcat),
    command("LEx", (0, 1), "*LEx lists the objects in a directory of the library with their information.", "Syntax: *LEx [<directory>]", lex),
    command("Lib", (0, 1), "*Lib sets the library directory, the user root directory if none is given.", "Syntax: *Lib [<directory>]", lib),
    command("NoDir", (0, 0), "*NoDir unsets the current directory.", "Syntax: *NoDir", no_dir),
    command("NoLib", (0, 0), "*NoLib unsets the library directory.", "Syntax: *NoLib", no_lib),
    command("NoURD", (0, 0), "*NoURD unsets the user root directory.", "Syntax: *NoURD", no_urd),
    command("Rename", (2, 2), "*Rename gives an object a new name on the same filing system.", "Syntax: *Rename <object> <new name>", rename),
    command("Run", (1, 255), "*Run runs a file found on Run$Path, as its type says, with the parameters given.", "Syntax: *Run <filename> [<parameters>]", run),
    command("SetType", (2, 2), "*SetType gives a file a file type, by its name or its number.", "Syntax: *SetType <filename> <file type>", set_type),
    command("Shut", (0, 0), "*Shut closes every open file on every filing system.", "Syntax: *Shut", shut),
    command("ShutDown", (0, 0), "*ShutDown closes every open file and tells every filing system to shut down.", "Syntax: *ShutDown", shut_down),
    command("Stamp", (1, 1), "*Stamp sets a file's date-stamp to now, keeping its type.", "Syntax: *Stamp <filename>", stamp),
    command("Up", (0, 1), "*Up makes the current directory's parent current, or the one the levels given above it.", "Syntax: *Up [<levels>]", up),
    command("URD", (0, 1), "*URD sets the user root directory, $ if none is given.", "Syntax: *URD [<directory>]", urd),
    command("Wipe", (1, 255), "*Wipe deletes the objects that match, as Wipe$Options and the options given ask.", "Syntax: *Wipe <object spec> [[~]<options>]", wipe),
];

/// A command tail's words: where each begins.
fn words(os: &mut Os<'_>, tail: u32) -> Result<Vec<u32>, Fail> {
    let mut starts = Vec::new();
    let mut at = tail;
    let mut within = false;
    for _ in 0..super::name::NAME_MOST * 4 {
        let byte = os.read_byte(at)?;
        if byte < b' ' {
            return Ok(starts);
        }
        if byte != b' ' && !within {
            if starts.try_reserve(1).is_err() {
                return Err(os.no_memory());
            }
            starts.push(at);
        }
        within = byte != b' ';
        at = at.wrapping_add(1);
    }
    Ok(starts)
}

/// The bytes from `at` to the tail's end.
fn rest(os: &mut Os<'_>, at: u32) -> Result<Vec<u8>, Fail> {
    Ok(os.read_until(at, super::name::NAME_MOST, |b| b < b' ')?.0)
}

/// An empty string below the supervisor stack for a name a command was not
/// given, handed to `work`.
fn with_empty<T>(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Os<'_>, u32) -> Result<T, Fail>,
) -> Result<T, Fail> {
    os.with_text(b"", 0, work)
}

/// `OS_FSControl` `reason` with R1 the first word of the tail, or an empty
/// name where there is none.
fn control_on_name(os: &mut Os<'_>, regs: &Regs, reason: u32) -> Result<(), Fail> {
    let first = words(os, regs.r[0])?.first().copied();
    let mut call = Regs::default();
    call.r[0] = reason;
    match first {
        Some(at) => {
            call.r[1] = at;
            control(os, &mut call)
        }
        None => with_empty(os, |os, at| {
            call.r[1] = at;
            control(os, &mut call)
        }),
    }
}

fn control(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.perform(X | swi::FS_CONTROL, regs)
}

fn bare(os: &mut Os<'_>, reason: u32) -> Result<(), Fail> {
    let mut call = Regs::default();
    call.r[0] = reason;
    control(os, &mut call)
}

fn back(os: &mut Os<'_>, _: &mut Regs) -> Result<(), Fail> {
    bare(os, reason::BACK)
}
fn cat(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::CAT)
}
fn ex(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::EX)
}
fn lcat(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::LIBRARY_CAT)
}
fn lex(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::LIBRARY_EX)
}
fn info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::INFO)
}
fn file_info(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::FILE_INFO)
}
fn dir(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::SET_CURRENT)
}
fn lib(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::SET_LIBRARY)
}
fn urd(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    control_on_name(os, regs, reason::SET_USER_ROOT)
}
fn no_dir(os: &mut Os<'_>, _: &mut Regs) -> Result<(), Fail> {
    bare(os, reason::UNSET_CURRENT)
}
fn no_urd(os: &mut Os<'_>, _: &mut Regs) -> Result<(), Fail> {
    bare(os, reason::UNSET_USER_ROOT)
}
fn no_lib(os: &mut Os<'_>, _: &mut Regs) -> Result<(), Fail> {
    bare(os, reason::UNSET_LIBRARY)
}
fn shut(os: &mut Os<'_>, _: &mut Regs) -> Result<(), Fail> {
    bare(os, reason::CLOSE_ALL)
}
fn shut_down(os: &mut Os<'_>, _: &mut Regs) -> Result<(), Fail> {
    bare(os, reason::SHUT_DOWN)
}

/// `*Access`: `OS_FSControl` 24, an object's attributes set from the second
/// word, or cleared where there is none.
fn access(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let mut call = Regs::default();
    call.r[0] = reason::ACCESS;
    call.r[1] = words[0];
    match words.get(1) {
        Some(&attributes) => {
            call.r[2] = attributes;
            control(os, &mut call)
        }
        None => with_empty(os, |os, at| {
            call.r[2] = at;
            control(os, &mut call)
        }),
    }
}

/// `*Rename`: `OS_FSControl` 25.
/// `*Run`: `OS_FSControl` 4 on the tail.
fn run(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut call = Regs::default();
    call.r[..2].copy_from_slice(&[reason::RUN, regs.r[0]]);
    control(os, &mut call)
}

fn rename(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let mut call = Regs::default();
    call.r[..3].copy_from_slice(&[reason::RENAME, words[0], words[1]]);
    control(os, &mut call)
}

/// `OS_File` `reason`, R1 to R5 as given.
fn file(os: &mut Os<'_>, reason: u32, regs: &[u32; 5]) -> Result<Regs, Fail> {
    let mut call = Regs::default();
    call.r[0] = reason;
    call.r[1..6].copy_from_slice(regs);
    os.perform(X | swi::FILE, &mut call)?;
    Ok(call)
}

/// `*CDir`: `OS_File` 8, with the entries given, in decimal.
fn cdir(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let entries = match words.get(1) {
        Some(&at) => os.number_at(at, DECIMAL, 0)?.0,
        None => 0,
    };
    file(
        os,
        file_reason::CREATE_DIRECTORY,
        &[words[0], 0, 0, entries, 0],
    )
    .map(drop)
}

/// `*SetType`: `OS_File` 18, the type read by `OS_FSControl` 31.
fn set_type(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let mut kind = Regs::default();
    kind.r[0] = reason::TYPE_NUMBER;
    kind.r[1] = words[1];
    control(os, &mut kind)?;
    file(os, file_reason::SET_TYPE, &[words[0], kind.r[2], 0, 0, 0]).map(drop)
}

/// `*Stamp`: `OS_File` 9.
fn stamp(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    file(os, file_reason::STAMP, &[regs.r[0], 0, 0, 0, 0]).map(drop)
}

/// `*Up`: `OS_FSControl` 0 to the parent the levels given above, one where
/// none are.
fn up(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let levels = match words(os, regs.r[0])?.first() {
        Some(&at) => os.number_at(at, DECIMAL, 0)?.0,
        None => 1,
    };
    let count = levels.clamp(1, len32(super::name::NAME_MOST / 2)) as usize;
    let mut path = Vec::new();
    if path.try_reserve_exact(count * 2).is_err() {
        return Err(os.no_memory());
    }
    for level in 0..count {
        if level > 0 {
            path.push(b'.');
        }
        path.push(b'^');
    }
    os.with_text(&path, 0, |os, at| {
        let mut call = Regs::default();
        call.r[1] = at;
        control(os, &mut call)
    })
}

/// The letters an operation's options take, each with the flag it sets;
/// `A`'s flag is set by its absence.
type Letters = &'static [(u8, u32)];
const COPY_LETTERS: Letters = &[
    (b'A', flag::NO_ATTRIBUTES),
    (b'C', flag::CONFIRM),
    (b'D', flag::DELETE),
    (b'F', flag::FORCE),
    (b'L', flag::LOOK),
    (b'N', flag::NEWER),
    (b'P', flag::PROMPT),
    (b'Q', flag::QUICK),
    (b'R', flag::RECURSE),
    (b'S', flag::STAMP),
    (b'T', flag::STRUCTURE),
    (b'V', flag::VERBOSE),
];
const WIPE_LETTERS: Letters = &[
    (b'C', flag::CONFIRM),
    (b'F', flag::FORCE),
    (b'R', flag::RECURSE),
    (b'V', flag::VERBOSE),
];
const COUNT_LETTERS: Letters = &[
    (b'C', flag::CONFIRM),
    (b'R', flag::RECURSE),
    (b'V', flag::VERBOSE),
];
/// The letter whose flag means its absence.
const INVERTED: u8 = b'A';

/// `flags` with the options `text` gives applied: a letter sets its option,
/// one after a `~` clears it, spaces between them ignored.
fn apply_options(text: &[u8], letters: Letters, flags: u32) -> Option<u32> {
    let mut flags = flags;
    let mut negated = false;
    for &byte in text {
        match byte {
            b' ' => {}
            b'~' => negated = true,
            letter => {
                let upper = letter.to_ascii_uppercase();
                let &(_, flag) = letters.iter().find(|(l, _)| *l == upper)?;
                let on = !negated ^ (upper == INVERTED);
                flags = if on { flags | flag } else { flags & !flag };
                negated = false;
            }
        }
    }
    Some(flags)
}

/// The flags an operation's options give: its variable's, or its default's
/// where that is unset, then those given from `given` on.
fn options(
    os: &mut Os<'_>,
    variable_name: &[u8],
    default: &[u8],
    letters: Letters,
    given: Option<u32>,
) -> Result<u32, Fail> {
    let base = os
        .variable_text(variable_name)?
        .unwrap_or_else(|| default.to_vec());
    let Some(flags) = apply_options(&base, letters, 0) else {
        return Err(error::fail(os, BAD_OPTION, &[]));
    };
    let Some(at) = given else {
        return Ok(flags);
    };
    let text = rest(os, at)?;
    match apply_options(&text, letters, flags) {
        Some(flags) => Ok(flags),
        None => Err(error::fail(os, BAD_OPTION, &[])),
    }
}

/// `*Copy`: `OS_FSControl` 26 as the options ask.
fn copy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let flags = options(
        os,
        b"Copy$Options",
        COPY_OPTIONS,
        COPY_LETTERS,
        words.get(2).copied(),
    )?;
    let mut call = Regs::default();
    call.r[..4].copy_from_slice(&[reason::COPY, words[0], words[1], flags]);
    control(os, &mut call)
}

/// `*Wipe`: `OS_FSControl` 27 as the options ask.
fn wipe(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let flags = options(
        os,
        b"Wipe$Options",
        WIPE_OPTIONS,
        WIPE_LETTERS,
        words.get(1).copied(),
    )?;
    let mut call = Regs::default();
    call.r[..4].copy_from_slice(&[reason::WIPE, words[0], 0, flags]);
    control(os, &mut call)
}

/// `*Count`: `OS_FSControl` 28 as the options ask, and what it totalled.
fn count(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let flags = options(
        os,
        b"Count$Options",
        COUNT_OPTIONS,
        COUNT_LETTERS,
        words.get(1).copied(),
    )?;
    let mut call = Regs::default();
    call.r[..4].copy_from_slice(&[reason::COUNT, words[0], 0, flags]);
    control(os, &mut call)?;
    let (bytes, files) = (call.r[2], call.r[3]);
    let mut line = Vec::new();
    super::listing::decimal(&mut line, files);
    line.extend_from_slice(if files == 1 { b" file, " } else { b" files, " });
    super::listing::decimal(&mut line, bytes);
    line.extend_from_slice(b" bytes");
    os.write_line(&line)
}

/// The most names one `OS_GBPB` 9 of `*EnumDir` reads, into its room, as
/// RISC OS 5 reads them.
const ENUM_NAMES: u32 = 24;
const ENUM_ROOM: u32 = 144;

/// `*EnumDir`: the file made by `OS_Find`, then each name in the directory
/// that matches written to it a line at a time, read by `OS_GBPB` 9.
fn enum_dir(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let words = words(os, regs.r[0])?;
    let mut create = Regs::default();
    create.r[0] = find::OUTPUT | find::MUST_EXIST | find::NO_PATH;
    create.r[1] = words[1];
    os.perform(X | swi::FIND, &mut create)?;
    let handle = create.r[0];
    let listed = list_into(os, handle, words[0], words.get(2).copied());
    let mut close = Regs::default();
    close.r[1] = handle;
    let closed = os.perform(X | swi::FIND, &mut close);
    listed.and(closed)
}

/// The names of the directory at `directory` matching `pattern` written to
/// the file open on `handle`, one to a line.
fn list_into(
    os: &mut Os<'_>,
    handle: u32,
    directory: u32,
    pattern: Option<u32>,
) -> Result<(), Fail> {
    os.below_supervisor_stack(ENUM_ROOM, |os, buffer| {
        let mut offset = 0;
        for _ in 0..super::directory::READS_MOST {
            let mut call = Regs::default();
            call.r[..7].copy_from_slice(&[
                gbpb::READ_NAMES,
                directory,
                buffer,
                ENUM_NAMES,
                offset,
                ENUM_ROOM,
                pattern.unwrap_or(0),
            ]);
            os.perform(X | swi::GBPB, &mut call)?;
            let mut at = buffer;
            for _ in 0..call.r[3] {
                let (name, ended) = os.read_until(at, super::name::NAME_MOST, |b| b == 0)?;
                if !ended {
                    break;
                }
                for byte in name.iter().copied().chain(core::iter::once(b'\n')) {
                    let mut put = Regs::default();
                    put.r[0] = u32::from(byte);
                    put.r[1] = handle;
                    os.perform(X | swi::BPUT, &mut put)?;
                }
                at += len32(name.len()) + 1;
            }
            if call.r[4] == u32::MAX || call.r[4] == offset {
                return Ok(());
            }
            offset = call.r[4];
        }
        Ok(())
    })
}

/// A configuration keyword's request: its syntax, its status, or a value.
enum Asked {
    Syntax,
    Status,
    Set(u32),
}

const fn asked(r0: u32) -> Asked {
    match r0 {
        0 => Asked::Syntax,
        1 => Asked::Status,
        text => Asked::Set(text),
    }
}

/// A status line: the keyword, then its value from the value column, then
/// `end`.
fn status_line(os: &mut Os<'_>, keyword: &[u8], value: &[u8], end: &[u8]) -> Result<(), Fail> {
    const SPACES: [u8; VALUE_COLUMN] = [b' '; VALUE_COLUMN];
    os.write_text(keyword)?;
    os.write_text(&SPACES[keyword.len().min(VALUE_COLUMN - 1)..])?;
    os.write_text(value)?;
    os.write_text(end)
}

/// `*Configure FileSystem`: the filing system selected at start. A value
/// that starts with a letter names it, whole and ignoring case; any other is
/// its number, which ends at a space or the line's end and is at most 255.
fn configure_file_system(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match asked(regs.r[0]) {
        Asked::Syntax => os.write_line(b"FileSystem <fs name>|<fs number>"),
        Asked::Status => {
            let number = os.read_cmos(FILE_LANG)?;
            let name = super::control::name_of_number(os, u32::from(number))?;
            let value = name.unwrap_or_else(|| {
                let mut digits = Vec::new();
                super::listing::decimal(&mut digits, u32::from(number));
                digits
            });
            status_line(os, b"FileSystem", &value, b"\n\r")
        }
        Asked::Set(at) => {
            let (first, start) = os.skip_spaces(at)?;
            let number = if first.is_ascii_alphabetic() {
                let name = rest(os, start)?;
                let named = with(os, |state| {
                    state
                        .systems
                        .iter()
                        .find(|s| s.named(&name))
                        .map(|s| s.number)
                })?;
                named.ok_or_else(|| error::no_prefix(os, &name))?
            } else {
                let (number, _) = os.number_at(start, TERMINATED | BYTE_SIZED | DECIMAL, 0)?;
                u8::try_from(number).map_err(|_| os.error(BAD_NUMBER))?
            };
            os.write_cmos(FILE_LANG, number)
        }
    }
}

/// `*Configure Truncate`: whether a name too long for its filing system is
/// cut short, `On`, or refused, `Off`, as the value's first word says, case
/// ignored; anything else is the unknown command RISC OS 5 answers. Its
/// status line ends as RISC OS 5's does, its return first.
fn configure_truncate(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let options = os.read_cmos(FILESWITCH_OPTIONS)?;
    match asked(regs.r[0]) {
        Asked::Syntax => os.write_line(b"Truncate On|Off"),
        Asked::Status => {
            let value: &[u8] = if options & TRUNCATE_BIT != 0 {
                b"On"
            } else {
                b"Off"
            };
            status_line(os, b"Truncate", value, b"\r\n")
        }
        Asked::Set(at) => {
            let (_, start) = os.skip_spaces(at)?;
            let (word, _) = os.read_until(start, b"Off".len() + 1, |b| b <= b' ')?;
            let on = if word.eq_ignore_ascii_case(b"On") {
                true
            } else if word.eq_ignore_ascii_case(b"Off") {
                false
            } else {
                return Err(os.error(BAD_COMMAND));
            };
            let changed = if on {
                options | TRUNCATE_BIT
            } else {
                options & !TRUNCATE_BIT
            };
            os.write_cmos(FILESWITCH_OPTIONS, changed)
        }
    }
}

#[cfg(test)]
mod tests;
