//! The kernel's file commands, which reach files only through the filing
//! system's SWIs: memory loaded and saved, files made and deleted, shown as
//! text or as bytes, built from typed lines, and taking the input or a copy
//! of the output.
//!
//! How `*Type` and `*List` show a byte and how `*Dump` lays out a line are
//! pure machines, fed a byte or a line at a time, so that what they print is
//! tested apart from the files they read.

use alloc::vec::Vec;

use crate::convert::Decimal;
use tairix_obey::gstrans::Flags;
use tairix_riscos_arm::psr;

use crate::byte::var;
use crate::cmos;
use crate::error::{Error, BAD_ADDRESS, SYNTAX};
use crate::input::ACKNOWLEDGE_ESCAPE;
use crate::os::{len32, Fail, Os, Regs};
use crate::swi::os::file::{CREATE, CREATE_TYPED, DELETE, LOAD, MAKE_ERROR, READ_WITH_TYPE, SAVE};
use crate::swi::os::find::{INPUT, OUTPUT, UPDATE};
use crate::swi::{os, X};

/// The `OS_Byte` variables holding the `*Exec` and `*Spool` files.
const EXEC_HANDLE: u32 = var::EXEC_HANDLE as u32;
const SPOOL_HANDLE: u32 = var::SPOOL_HANDLE as u32;

/// `DumpFormat`'s bits, the low five of its CMOS byte: bit 7 taken off a
/// byte first, top-bit bytes shown as they are, an unshowable byte as `<n>`,
/// and that number in hexadecimal — or without `<n>`, as a `.`.
const SEVEN_BIT: u8 = 1 << 3;
const RAW_TOP: u8 = 1 << 2;
const ANGLED: u8 = 1 << 1;
const HEXADECIMAL: u8 = 1 << 0;
/// The bits the listings read, and the one that expands tabs as
/// `-TabExpand` does.
const LISTING_BITS: u8 = 0x0F;
const TAB_SPACES: u8 = 1 << 4;

/// A file `*Load` has no way to load: its type has no `@LoadType`.
const NO_LOAD_TYPE: Error = Error {
    number: 0x409,
    token: "NoLoaderForType",
};
/// The file type `*Create` gives a file it is given no addresses for: data,
/// in the low twelve bits.
const DATA: u32 = 0xFFFF_F000 | crate::filing::filetype::DATA;
/// The most bytes a `*Dump` line shows, which only a window over a thousand
/// columns wide would outgrow.
const LINE_MOST: u32 = 256;
/// `*Type` and `*List`'s template.
const TEMPLATE: &[u8] = b"File/A,TabExpand/S\0";

/// "Outside file".
const OUTSIDE: Error = Error {
    number: 0xB7,
    token: "OffsetOutsideFile",
};
/// "Bad parameters for *Save".
const BAD_SAVE: Error = Error {
    number: SYNTAX,
    token: "SaveParameterCount",
};

/// How `*Type`, `*List` and `*Print` show a file's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Listing {
    /// `DumpFormat`'s bits for a filtered listing; none for `*Print`, which
    /// writes each byte as it is.
    filter: Option<u8>,
    numbered: bool,
    /// A tab is eight spaces.
    tabs: bool,
}

/// How far a listing has got.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Progress {
    lines: u32,
    /// The character last shown, a line's end included, or zero after the
    /// second half of a CR LF or LF CR pair.
    last: u8,
    within_line: bool,
}

impl Listing {
    /// What `byte` adds to the listing.
    pub(crate) fn feed(self, at: &mut Progress, byte: u8, out: &mut Vec<u8>) {
        if !at.within_line {
            at.within_line = true;
            if self.numbered {
                at.lines += 1;
                line_number(at.lines, out);
            }
        }
        let Some(format) = self.filter else {
            out.push(byte);
            return;
        };
        if byte == b'\r' || byte == b'\n' {
            if byte == at.last {
                newline(out);
                at.within_line = false;
            } else if at.last == b'\r' || at.last == b'\n' {
                // A pair's second half: what follows stays on the line that
                // its first half began, numbered already.
                at.last = 0;
            } else {
                at.last = byte;
                newline(out);
                at.within_line = false;
            }
            return;
        }
        if byte == b'\t' && self.tabs {
            out.extend_from_slice(b"        ");
            return;
        }
        at.last = byte;
        if byte == b'"' || byte == b'<' {
            out.push(byte);
        } else {
            shown(format, byte, out);
        }
    }

    /// What the end of the file adds: a filtered listing ends a line it
    /// is part way through.
    pub(crate) fn end(self, at: Progress, out: &mut Vec<u8>) {
        if self.filter.is_some() && at.within_line {
            newline(out);
        }
    }
}

/// `byte` as RISC OS's GSTrans would read it back, in `format`.
fn shown(format: u8, byte: u8, out: &mut Vec<u8>) {
    let byte = if format & SEVEN_BIT != 0 {
        byte & 0x7F
    } else {
        byte
    };
    let printable = (0x20..=0x7E).contains(&byte);
    let raw_top = format & RAW_TOP != 0 && byte >= 0x80;
    if (printable && !matches!(byte, b'|' | b'"' | b'<')) || raw_top {
        out.push(byte);
    } else if format & ANGLED != 0 {
        out.push(b'<');
        if format & HEXADECIMAL != 0 {
            out.push(b'&');
            out.extend_from_slice(&hex2(byte));
        } else {
            out.extend_from_slice(Decimal::of(i32::from(byte)).as_bytes());
        }
        out.push(b'>');
    } else if format & HEXADECIMAL != 0 {
        out.push(if printable { byte } else { b'.' });
    } else {
        if byte >= 0x80 {
            out.extend_from_slice(b"|!");
        }
        match byte & 0x7F {
            0x7F => out.extend_from_slice(b"|?"),
            c @ (b'"' | b'|') => out.extend_from_slice(&[b'|', c]),
            c @ 0..=0x1F => out.extend_from_slice(&[b'|', c + b'@']),
            c => out.push(c),
        }
    }
}

fn newline(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\n\r");
}

/// A line number as `*List` and `*Build` write it: right-aligned in four
/// columns, then a space.
fn line_number(line: u32, out: &mut Vec<u8>) {
    let digits = Decimal::of(line.cast_signed());
    let digits = digits.as_bytes();
    out.resize(out.len() + 4usize.saturating_sub(digits.len()), b' ');
    out.extend_from_slice(digits);
    out.push(b' ');
}

/// `byte` in two hexadecimal digits.
fn hex2(byte: u8) -> [u8; 2] {
    use tairix_util::hex::upper_digit;
    [upper_digit(byte >> 4), upper_digit(byte)]
}

/// How `*Dump` lays out a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Dump {
    /// The bytes a line shows.
    per_line: u32,
    /// `DumpFormat`'s bits.
    format: u8,
}

impl Dump {
    /// The layout for a window `width` columns wide: as many bytes as fit,
    /// cut to the highest power of two in them and the next lower one if it
    /// is in them too — 1, 2, 3, 4, 6, 8, 12, 16, 24 and on.
    pub(crate) fn for_width(width: u32, format: u8) -> Self {
        let fits = width.saturating_sub(13) / 4;
        let per_line = if fits == 0 {
            1
        } else {
            let top = 1 << fits.ilog2();
            top | (fits & (top >> 1))
        };
        Self {
            per_line: per_line.min(LINE_MOST),
            format,
        }
    }

    /// The heading every sixteenth line: each column's address's low byte,
    /// and over the characters, `ascii` centred when there is room for it.
    pub(crate) fn heading(self, address: u32, title: &[u8], ascii: &[u8], out: &mut Vec<u8>) {
        newline(out);
        out.extend_from_slice(title);
        for column in 0..self.per_line {
            out.push(b' ');
            out.extend_from_slice(&hex2(address.wrapping_add(column).to_le_bytes()[0]));
        }
        if self.per_line >= 11 {
            out.extend_from_slice(b" : ");
            out.resize(out.len() + ((self.per_line - 10) >> 1) as usize + 1, b' ');
            out.extend_from_slice(ascii);
        }
        newline(out);
        newline(out);
    }

    /// A line showing `bytes`, a line's worth or the file's last, read at
    /// `address`.
    pub(crate) fn line(self, address: u32, bytes: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&crate::env::hex8(address));
        out.extend_from_slice(b" :");
        for column in 0..self.per_line as usize {
            match bytes.get(column) {
                Some(&byte) => {
                    out.push(b' ');
                    out.extend_from_slice(&hex2(byte));
                }
                None => out.extend_from_slice(b"   "),
            }
        }
        out.extend_from_slice(b" : ");
        for &byte in bytes {
            let byte = if self.format & SEVEN_BIT != 0 {
                byte & 0x7F
            } else {
                byte
            };
            let raw = (0x20..=0x7E).contains(&byte) || (self.format & RAW_TOP != 0 && byte >= 0x80);
            out.push(if raw { byte } else { b'.' });
        }
        newline(out);
    }
}

impl Os<'_> {
    /// Where a command's tail goes past its first word: at a space, a
    /// control character or `DEL`.
    fn past_word(&mut self, tail: u32) -> Result<u32, Fail> {
        let mut at = tail;
        loop {
            let byte = self.read_byte(at)?;
            if byte <= b' ' || byte == 0x7F {
                return Ok(at);
            }
            at = at.wrapping_add(1);
        }
    }

    /// The hexadecimal number at `at`, as `OS_ReadUnsigned` reads one with
    /// no check of what ends it, and where it ends.
    fn hex_at(&mut self, at: u32) -> Result<(u32, u32), Fail> {
        let mut read = Regs::default();
        read.r[..2].copy_from_slice(&[16, at]);
        self.perform(X | os::READ_UNSIGNED, &mut read)?;
        Ok((read.r[2], read.r[1]))
    }

    /// The address after the filename at `tail`, where it ends, and whether
    /// anything but a space or the line's end comes straight after it.
    fn address_after_name(&mut self, tail: u32) -> Result<(u32, u32, bool), Fail> {
        let name_end = self.past_word(tail)?;
        let (_, at) = self.skip_spaces(name_end)?;
        let (address, end) = self.hex_at(at)?;
        Ok((address, end, self.read_byte(end)? > b' '))
    }

    /// Up to two more hexadecimal numbers at `at`, each standing in for its
    /// default, and whether anything follows them.
    fn optional_pair(&mut self, at: u32, defaults: [u32; 2]) -> Result<([u32; 2], bool), Fail> {
        let mut values = defaults;
        let mut at = at;
        for value in &mut values {
            let (byte, start) = self.skip_spaces(at)?;
            if byte < b' ' {
                return Ok((values, false));
            }
            (*value, at) = self.hex_at(start)?;
        }
        let (byte, _) = self.skip_spaces(at)?;
        Ok((values, byte >= b' '))
    }

    /// `OS_File` `reason` with `regs`, R0 the reason.
    /// A file of type `kind`, named at `tail`, loaded by its type's
    /// `@LoadType_XXX` alias run with the name.
    fn load_by_type(&mut self, kind: u32, tail: u32) -> Result<(), Fail> {
        let mut alias = *b"@LoadType_000";
        alias[10..].copy_from_slice(&crate::filing::filetype::digits(kind));
        let mut variable = *b"Alias$@LoadType_000";
        variable[6..].copy_from_slice(&alias);
        if self.variable_text(&variable)?.is_none() {
            return Err(self.error_with(NO_LOAD_TYPE, &[&alias[10..]]));
        }
        let (name, _) = self.read_until(tail, crate::cli::LONGEST, |b| b < b' ')?;
        let mut line = Vec::new();
        if line.try_reserve(alias.len() + 2 + name.len()).is_err() {
            return Err(self.no_memory());
        }
        line.extend_from_slice(&alias);
        line.push(b' ');
        line.extend_from_slice(&name);
        line.push(b'\r');
        let at = self.scratch_text(&line)?;
        self.command_line(at)
    }

    fn file_reason(&mut self, reason: u32, mut regs: Regs) -> Result<Regs, Fail> {
        regs.r[0] = reason;
        self.perform(X | os::FILE, &mut regs)?;
        Ok(regs)
    }

    /// `handle`'s next byte, none at its end.
    fn next_byte(&mut self, handle: u32) -> Result<Option<u8>, Fail> {
        let mut get = Regs::default();
        get.r[1] = handle;
        self.perform(X | os::BGET, &mut get)?;
        Ok((!get.flag(psr::C)).then(|| get.byte(0)))
    }

    fn put_byte(&mut self, handle: u32, byte: u8) -> Result<(), Fail> {
        let mut put = Regs::default();
        put.r[..2].copy_from_slice(&[u32::from(byte), handle]);
        self.perform(X | os::BPUT, &mut put)
    }

    /// `work` done on the open file `handle`, which is then closed: `work`'s
    /// error first, else the close's.
    fn closing(
        &mut self,
        handle: u32,
        work: impl FnOnce(&mut Self) -> Result<(), Fail>,
    ) -> Result<(), Fail> {
        let result = work(self);
        let closed = self.close_handle(handle);
        result.and(closed)
    }

    /// The `DumpFormat` CMOS configures.
    fn dump_format(&mut self) -> Result<u8, Fail> {
        self.read_cmos(cmos::PRINT_SOUND)
    }

    /// `*Type`, `*List` and `*Print`: the file the tail names shown as
    /// `listing` says, `-TabExpand` read where more than the name is given.
    fn list_file(&mut self, regs: &Regs, mut listing: Listing) -> Result<(), Fail> {
        let (buffer, held) = self.cli_buffer();
        let opened = (|| {
            let name = if regs.r[1] == 1 || listing.filter.is_none() {
                regs.r[0]
            } else {
                self.read_args_into(TEMPLATE, regs.r[0], buffer, super::BUFFER)?;
                listing.tabs |= self.read_word(buffer.wrapping_add(4))? != 0;
                self.read_word(buffer)?
            };
            self.open_file(INPUT, name)
        })();
        self.world.cli.release(held);
        let handle = opened?;
        self.closing(handle, |os| {
            let mut progress = Progress::default();
            let mut out = Vec::new();
            loop {
                os.escape_point()?;
                let Some(byte) = os.next_byte(handle)? else {
                    break;
                };
                out.clear();
                listing.feed(&mut progress, byte, &mut out);
                os.write_text(&out)?;
            }
            out.clear();
            listing.end(progress, &mut out);
            os.write_text(&out)
        })
    }

    /// A listing filtered by the configured `DumpFormat`, its tabs
    /// expanded if that says so.
    fn filtered(&mut self, numbered: bool) -> Result<Listing, Fail> {
        let format = self.dump_format()?;
        Ok(Listing {
            filter: Some(format & LISTING_BITS),
            numbered,
            tabs: format & TAB_SPACES != 0,
        })
    }

    /// `*Build` and `*Append`: each line typed, numbered, written to the end
    /// of the file opened with `reason`, its leading spaces as they are and
    /// the rest GSTrans'd, until Escape ends the last line unterminated.
    fn build_file(&mut self, name: u32, reason: u32) -> Result<(), Fail> {
        let handle = self.open_file(reason, name)?;
        self.closing(handle, |os| {
            os.pointer_to_end(handle)?;
            let (buffer, held) = os.cli_buffer();
            let result = os.build_lines(handle, buffer);
            os.world.cli.release(held);
            result
        })
    }

    fn build_lines(&mut self, handle: u32, buffer: u32) -> Result<(), Fail> {
        let mut prompt = Vec::new();
        for line in 1.. {
            prompt.clear();
            line_number(line, &mut prompt);
            self.write_text(&prompt)?;
            let mut read = Regs::default();
            read.r[..5].copy_from_slice(&[buffer, 255, u32::from(b' '), 0xFF, line]);
            self.perform(X | os::READ_LINE32, &mut read)?;
            self.write_byte(buffer.wrapping_add(read.r[1]), b'\r')?;
            let escaped = read.flag(psr::C);
            if escaped {
                let mut acknowledge = Regs::default();
                acknowledge.r[0] = ACKNOWLEDGE_ESCAPE;
                self.perform(X | os::BYTE, &mut acknowledge)?;
            }
            let (_, text) = self.skip_spaces(buffer)?;
            for _ in buffer..text {
                self.put_byte(handle, b' ')?;
            }
            self.gs_each(text, Flags::NO_QUOTES.bits(), |os, byte| {
                os.put_byte(handle, byte)
            })?;
            if escaped {
                return self.write_text(b"\n\r");
            }
            self.put_byte(handle, b'\r')?;
        }
        Ok(())
    }

    /// `*Exec`, `*Spool` and `*SpoolOn`: the file `OS_Byte` `reason` holds
    /// closed, then the one the tail names, if any, opened with `open` and
    /// held in its place.
    fn hold_file(&mut self, regs: &Regs, reason: u32, open: u32) -> Result<(), Fail> {
        let mut byte = Regs::default();
        byte.r[0] = reason;
        self.perform(X | os::BYTE, &mut byte)?;
        self.close_handle(byte.r[1])?;
        if regs.r[1] == 0 {
            return Ok(());
        }
        let handle = self.open_file(open, regs.r[0])?;
        let moved = if open == UPDATE {
            self.pointer_to_end(handle)
        } else {
            Ok(())
        };
        if let Err(fail) = moved {
            // Closed, so a failure leaves no file open that nothing holds.
            let _ = self.close_handle(handle);
            return Err(fail);
        }
        let mut hold = Regs::default();
        hold.r[..2].copy_from_slice(&[reason, handle]);
        self.perform(X | os::BYTE, &mut hold)
    }

    /// `*Dump`'s lines for the open file `handle`, `extent` bytes long, from
    /// `offset`, the first shown at `address`.
    fn dump_lines(
        &mut self,
        handle: u32,
        extent: u32,
        offset: u32,
        address: u32,
        format: u8,
    ) -> Result<(), Fail> {
        if offset > extent {
            return Err(self.error(OUTSIDE));
        }
        self.set_pointer(handle, offset)?;
        let layout = Dump::for_width(self.window_width()?, format);
        let mut address = address;
        let mut out = Vec::new();
        let mut bytes = Vec::new();
        let (mut title, mut ascii) = (Vec::new(), Vec::new());
        for line in 0u32.. {
            self.escape_point()?;
            bytes.clear();
            while len32(bytes.len()) < layout.per_line {
                match self.next_byte(handle)? {
                    Some(byte) => bytes.push(byte),
                    None => break,
                }
            }
            if bytes.is_empty() {
                break;
            }
            out.clear();
            if line % 16 == 0 {
                if title.is_empty() {
                    title = self.message_with(b"DumpAddressHeading", &[])?;
                    ascii = self.message_with(b"DumpTextHeading", &[])?;
                }
                layout.heading(address, &title, &ascii, &mut out);
            }
            layout.line(address, &bytes, &mut out);
            self.write_text(&out)?;
            if len32(bytes.len()) < layout.per_line {
                break;
            }
            address = address.wrapping_add(layout.per_line);
        }
        Ok(())
    }
}

/// `*Load`: the file at the address given; with none, an untyped file at
/// its own load address, and a typed one as its type's `Alias$@LoadType_XXX`
/// loads it.
pub(crate) fn load(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut file = Regs::default();
    file.r[1] = regs.r[0];
    if regs.r[1] == 1 {
        let mut info = Regs::default();
        info.r[1] = regs.r[0];
        let info = os.file_reason(READ_WITH_TYPE, info)?;
        if info.r[0] != 0 && info.r[6] != u32::MAX {
            return os.load_by_type(info.r[6], regs.r[0]);
        }
        file.r[3] = 0xFF;
    } else {
        let (address, _, junk) = os.address_after_name(regs.r[0])?;
        if junk {
            return Err(os.error(BAD_ADDRESS));
        }
        file.r[2] = address;
    }
    os.file_reason(LOAD, file).map(drop)
}

/// `*Save`: memory from a start to an end, or for `+` a length, with
/// execution and load addresses that are the start unless given.
pub(crate) fn save(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (start, after, _) = os.address_after_name(regs.r[0])?;
    let (byte, at) = os.skip_spaces(after)?;
    let (end, after) = if byte == b'+' {
        let (_, at) = os.skip_spaces(at.wrapping_add(1))?;
        let (length, end) = os.hex_at(at)?;
        (start.wrapping_add(length), end)
    } else {
        os.hex_at(at)?
    };
    let [exec, load] = match os.optional_pair(after, [start, start]) {
        Ok((values, false)) => values,
        Ok((_, true)) | Err(Fail::Error(_)) => return Err(os.error(BAD_SAVE)),
        Err(unwind) => return Err(unwind),
    };
    let mut file = Regs::default();
    file.r[1..6].copy_from_slice(&[regs.r[0], load, exec, start, end]);
    os.file_reason(SAVE, file).map(drop)
}

/// `*Create`: a file of the length given, typed as data, or with the
/// execution and load addresses given.
pub(crate) fn create(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (length, after) = if regs.r[1] == 1 {
        (0, 0)
    } else {
        let (length, after, _) = os.address_after_name(regs.r[0])?;
        (length, after)
    };
    let mut file = Regs::default();
    file.r[1] = regs.r[0];
    file.r[5] = length;
    if regs.r[1] < 3 {
        file.r[2] = DATA;
        return os.file_reason(CREATE_TYPED, file).map(drop);
    }
    let ([exec, load], _) = os.optional_pair(after, [0, 0])?;
    file.r[2..4].copy_from_slice(&[load, exec]);
    os.file_reason(CREATE, file).map(drop)
}

/// `*Delete`: the object deleted, the filing system's own error if there
/// was none.
pub(crate) fn delete(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut file = Regs::default();
    file.r[1] = regs.r[0];
    if os.file_reason(DELETE, file)?.r[0] == 0 {
        let mut missing = Regs::default();
        missing.r[1] = regs.r[0];
        os.file_reason(MAKE_ERROR, missing)?;
    }
    Ok(())
}

/// `*Remove`: the object deleted, nothing said if there was none.
pub(crate) fn remove(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut file = Regs::default();
    file.r[1] = regs.r[0];
    os.file_reason(DELETE, file).map(drop)
}

/// `*Close`: every file the filing system has open closed.
pub(crate) fn close(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut find = Regs::default();
    os.perform(X | os::FIND, &mut find)
}

/// `*Dump`: the file's bytes from the offset given, each line's address
/// counted from the file's load address if it has one rather than a type,
/// or from the address given.
pub(crate) fn dump(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = os.open_file(INPUT, regs.r[0])?;
    os.closing(handle, |os| {
        let mut info = Regs::default();
        info.r[1] = regs.r[0];
        let info = os.file_reason(READ_WITH_TYPE, info)?;
        // An object that opened but is not there now is a stream: its end
        // is wherever reading stops.
        let (extent, unstamped) = if info.r[0] == 0 {
            (u32::MAX, false)
        } else {
            (info.r[4], info.r[6] == u32::MAX)
        };
        let format = os.dump_format()? & (SEVEN_BIT | RAW_TOP);
        let base = if unstamped { info.r[2] } else { 0 };
        let [offset, base] = if regs.r[1] == 1 {
            [0, base]
        } else {
            let name_end = os.past_word(regs.r[0])?;
            os.optional_pair(name_end, [0, base])?.0
        };
        os.dump_lines(handle, extent, offset, base.wrapping_add(offset), format)
    })
}

/// `*Type`.
pub(crate) fn type_file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let listing = os.filtered(false)?;
    os.list_file(regs, listing)
}

/// `*List`.
pub(crate) fn list(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let listing = os.filtered(true)?;
    os.list_file(regs, listing)
}

/// `*Print`.
pub(crate) fn print(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let raw = Listing {
        filter: None,
        numbered: false,
        tabs: false,
    };
    os.list_file(regs, raw)
}

/// `*Build`.
pub(crate) fn build(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.build_file(regs.r[0], OUTPUT)
}

/// `*Append`.
pub(crate) fn append(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.build_file(regs.r[0], UPDATE)
}

/// `*Exec`.
pub(crate) fn exec(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.hold_file(regs, EXEC_HANDLE, INPUT)
}

/// `*Spool`.
pub(crate) fn spool(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.hold_file(regs, SPOOL_HANDLE, OUTPUT)
}

/// `*SpoolOn`.
pub(crate) fn spool_on(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.hold_file(regs, SPOOL_HANDLE, UPDATE)
}

#[cfg(test)]
mod tests;
