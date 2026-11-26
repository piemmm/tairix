//! A stream as RISC OS 5.30's library keeps one: its `FILE` block among the
//! client's statics and the extra block beside it in the heap, whose pointer
//! and counts the `getc` and `putc` macros compiled into programs work on
//! too. Bytes move at the file's own pointer: the buffer stands for the
//! file's bytes from `bufpos`, a positioning is only recorded until the next
//! transfer makes it, and the pointer is moved back to `bufpos` only where a
//! fill or a positioning left it elsewhere. A file's four kilobytes are
//! taken at its first transfer, while the console's output is unbuffered
//! and its input read a line at a time.

use tairix_riscos_arm::mmu::to_page_end;
use tairix_riscos_arm::psr;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};
use tairix_riscos_kernel::swi::{os as kos, X};

use crate::c::alloc;
use crate::c::errno::{self, OS_REFUSED};
use crate::c::statics::{at, STREAMS, STREAM_BYTES};
use crate::kernel::os::note;
use crate::kernel::statics::Statics;

/// The `FILE` block's words read where a stream is not loaded whole: its
/// flags and its file's handle.
pub(crate) mod word {
    pub(crate) const FLAG: u32 = 12;
    pub(crate) const HANDLE: u32 = 20;
}

/// An extra block's bytes: nine words. Its first byte is the one-byte buffer
/// of an unbuffered stream and its second the character `ungetc` pushed
/// back; then come the file position of the buffer's first byte, nought,
/// the position a positioning asked for, nought, where the buffer's data
/// ends, the buffer's size once it is readied, the input count `ungetc` set
/// aside, and the name a temporary file is removed by.
pub(crate) const EXTRA_BYTES: u32 = 36;
const EXTRA_WORDS: usize = 9;
const _: () = assert!(4 * EXTRA_WORDS == EXTRA_BYTES as usize);

/// A stream's flags: the PRM's, then the library's own.
pub(crate) mod flag {
    pub(crate) const READ: u32 = 0x1;
    pub(crate) const WRITE: u32 = 0x2;
    pub(crate) const BINARY: u32 = 0x4;
    /// The file's pointer is to be moved to `bufpos` before the next
    /// transfer.
    pub(crate) const MOVE_POINTER: u32 = 0x10;
    /// The next transfer starts at the sought position, not the buffer's.
    pub(crate) const REPOSITION: u32 = 0x20;
    pub(crate) const EOF: u32 = 0x40;
    pub(crate) const ERROR: u32 = 0x80;
    pub(crate) const FULLY: u32 = 0x100;
    pub(crate) const LINE: u32 = 0x200;
    pub(crate) const UNBUFFERED: u32 = 0x400;
    pub(crate) const BUFFERING: u32 = FULLY | LINE | UNBUFFERED;
    /// The buffer is the library's, taken from the heap.
    pub(crate) const SYSTEM_BUFFER: u32 = 0x800;
    /// The buffer holds input not yet read.
    pub(crate) const READ_VALID: u32 = 0x1000;
    /// Written through `ptr` since `data_end` last caught up with it.
    pub(crate) const DIRTY: u32 = 0x2000;
    /// Opened to append: its first transfer goes to the file's end.
    pub(crate) const APPENDING: u32 = 0x8000;
    /// The buffer holds output, delivered whenever the buffer is.
    pub(crate) const WRITING: u32 = 0x10000;
    /// The buffer was filled from the file, whose pointer stands past it.
    pub(crate) const READING: u32 = 0x20000;
    /// Something has reached the file.
    pub(crate) const WRITTEN: u32 = 0x40000;
    pub(crate) const UNGOT: u32 = 0x80000;
    /// The file's handle is another stream's too: `stderr` sent where
    /// `stdout` goes.
    pub(crate) const SHARED: u32 = 0x10_0000;
    /// A transfer has been asked of the stream.
    pub(crate) const SET_UP: u32 = 0x40_0000;
    /// Opened by one of the large-file calls.
    pub(crate) const LARGE: u32 = 0x80_0000;
}

/// `BUFSIZ`.
pub(crate) const BUFSIZ: u32 = 0x1000;
/// `EOF`, as a word.
pub(crate) const EOF: u32 = u32::MAX;

/// A stream's state, read from its blocks and written back after a change.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stream {
    pub(crate) at: u32,
    pub(crate) ptr: u32,
    pub(crate) icnt: i32,
    pub(crate) ocnt: i32,
    pub(crate) flag: u32,
    pub(crate) base: u32,
    pub(crate) handle: u32,
    pub(crate) pos: u32,
    pub(crate) bufsiz: u32,
    pub(crate) signature: u32,
    pub(crate) extra: u32,
    pub(crate) bytes: [u8; 4],
    pub(crate) bufpos: u32,
    pub(crate) sought: u32,
    pub(crate) data_end: u32,
    pub(crate) bufsize: u32,
    pub(crate) saved_icnt: i32,
    pub(crate) remove: u32,
}

/// A length within a buffer as a count.
const fn count(n: u32) -> i32 {
    n.cast_signed()
}

impl Stream {
    /// The stream whose `FILE` block is at `at`.
    pub(crate) fn load(os: &mut Os<'_>, at: u32) -> Result<Self, Fail> {
        let mut file = [0; 10];
        os.as_caller().read_words(at, &mut file)?;
        let [ptr, icnt, ocnt, flag, base, handle, pos, bufsiz, signature, extra] = file;
        let mut words = [0; EXTRA_WORDS];
        if extra != 0 {
            os.as_caller().read_words(extra, &mut words)?;
        }
        Ok(Self {
            at,
            ptr,
            icnt: icnt.cast_signed(),
            ocnt: ocnt.cast_signed(),
            flag,
            base,
            handle,
            pos,
            bufsiz,
            signature,
            extra,
            bytes: words[0].to_le_bytes(),
            bufpos: words[1],
            sought: words[3],
            data_end: words[5],
            bufsize: words[6],
            saved_icnt: words[7].cast_signed(),
            remove: words[8],
        })
    }

    /// The stream written back to its blocks.
    pub(crate) fn store(&self, os: &mut Os<'_>) -> Result<(), Fail> {
        let file = [
            self.ptr,
            self.icnt.cast_unsigned(),
            self.ocnt.cast_unsigned(),
            self.flag,
            self.base,
            self.handle,
            self.pos,
            self.bufsiz,
            self.signature,
            self.extra,
        ];
        os.as_caller().write_words(self.at, &file)?;
        if self.extra != 0 {
            let words: [u32; EXTRA_WORDS] = [
                u32::from_le_bytes(self.bytes),
                self.bufpos,
                0,
                self.sought,
                0,
                self.data_end,
                self.bufsize,
                self.saved_icnt.cast_unsigned(),
                self.remove,
            ];
            os.as_caller().write_words(self.extra, &words)?;
        }
        Ok(())
    }

    /// Whether the stream is the console's: a standard stream not reopened
    /// onto a file.
    pub(crate) const fn console(&self) -> bool {
        self.handle == 0
    }

    /// The bytes of the buffer `data_end` says stand for the file.
    const fn data(&self) -> u32 {
        self.data_end.wrapping_sub(self.base)
    }

    /// The buffer's offset of `ptr`.
    const fn offset(&self) -> u32 {
        self.ptr.wrapping_sub(self.base)
    }

    /// Where the buffer's data ends: `ptr` where the `putc` macro has
    /// written past `data_end`.
    const fn data_end_now(&self) -> u32 {
        if self.ptr.wrapping_sub(self.data_end).cast_signed() > 0 {
            self.ptr
        } else {
            self.data_end
        }
    }

    /// The buffer's output: up to `ptr` for a line-buffered stream, to the
    /// data's end for any other.
    const fn output(&self) -> u32 {
        let end = if self.flag & flag::LINE != 0 {
            self.ptr
        } else {
            self.data_end_now()
        };
        end.wrapping_sub(self.base)
    }

    /// The counts a positioning within the buffer leaves: each of the
    /// stream's directions sent to the library on its next character.
    fn counts_to_library(&mut self) {
        self.icnt = if self.flag & flag::READ != 0 {
            count(self.offset()).wrapping_sub(count(self.data()))
        } else {
            0
        };
        self.ocnt = if self.flag & flag::WRITE != 0 {
            count(self.offset()).wrapping_sub(count(self.bufsiz))
        } else {
            0
        };
    }

    /// The buffer readied at the stream's first transfer: a console output
    /// stream unbuffered unless told otherwise, an unbuffered stream's
    /// buffer its extra block's first byte, any other's taken from the heap
    /// where it was given none, and an append stream with no positioning
    /// pending sent to the file's end as it then stands. False where it
    /// could not be, or the stream has no extra block to keep its state in.
    fn ready(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
        if self.bufsize != 0 {
            return Ok(true);
        }
        if self.extra == 0 {
            return Ok(false);
        }
        if self.console() && self.flag & flag::WRITE != 0 && self.flag & flag::BUFFERING == 0 {
            self.flag |= flag::UNBUFFERED;
        }
        if self.flag & flag::UNBUFFERED != 0 {
            self.base = self.extra;
            self.bufsiz = 1;
        } else if self.bufsiz == 0 {
            return Ok(false);
        } else if self.base == 0 {
            let buffer = alloc::allocated(os, statics, self.bufsiz)?;
            if buffer == 0 {
                return Ok(false);
            }
            self.base = buffer;
            self.flag |= flag::SYSTEM_BUFFER;
        }
        if self.flag & flag::BUFFERING == 0 {
            self.flag |= flag::FULLY;
        }
        if self.flag & (flag::APPENDING | flag::REPOSITION) == flag::APPENDING && !self.console() {
            let Some(extent) = self.extent_now(os, statics)? else {
                return Ok(false);
            };
            self.sought = extent;
            self.flag |= flag::REPOSITION;
        }
        self.ptr = self.base;
        self.bufsize = self.bufsiz;
        Ok(true)
    }

    /// The file's pointer moved to `bufpos`: false, the stream in error,
    /// where that was refused.
    fn set_pointer(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
        let mut args = registers(&[kos::args::WRITE_POINTER, self.handle, self.bufpos]);
        if !transferred(os, statics, kos::ARGS, &mut args)? {
            self.flag |= flag::ERROR;
            return Ok(false);
        }
        self.flag &= !(flag::MOVE_POINTER | flag::READING);
        Ok(true)
    }

    /// `count` bytes at `from` written at the file's pointer, moved to
    /// `bufpos` first where it stands elsewhere: false, the stream in error,
    /// where they were refused.
    fn write_out(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        from: u32,
        count: u32,
    ) -> Result<bool, Fail> {
        if count == 0 {
            return Ok(true);
        }
        if self.console() {
            let text = os.as_caller().read_vec(from, count as usize)?;
            return self.console_out(os, statics, &text);
        }
        if self.flag & (flag::MOVE_POINTER | flag::READING) != 0
            && !self.set_pointer(os, statics)?
        {
            return Ok(false);
        }
        let mut transfer = registers(&[kos::gbpb::WRITE, self.handle, from, count]);
        if !transferred(os, statics, kos::GBPB, &mut transfer)? {
            self.flag |= flag::ERROR;
            return Ok(false);
        }
        Ok(true)
    }

    /// `text` written to the console: false, the stream in error, where it
    /// was refused.
    fn console_out(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        text: &[u8],
    ) -> Result<bool, Fail> {
        if to_console(os, statics, text)? {
            return Ok(true);
        }
        errno::set(os, statics, OS_REFUSED)?;
        self.flag |= flag::ERROR;
        Ok(false)
    }

    /// The buffer's output delivered as a full buffer, a line or a flush
    /// delivers it, the stream marked as having reached its file: false
    /// where it was refused.
    fn deliver(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
        self.flag |= flag::WRITTEN;
        self.write_out(os, statics, self.base, self.output())
    }

    /// At most `room` bytes read at the file's pointer into `into` — the
    /// console's a line, ended with a newline — how many; none, the stream
    /// in error, where they could not be.
    fn device_read(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        into: u32,
        room: u32,
    ) -> Result<Option<u32>, Fail> {
        if self.console() {
            flush_lines(os, statics, self.at)?;
            let mut line = registers(&[into, room.saturating_sub(1), 0, 255, 0]);
            if !transferred(os, statics, kos::READ_LINE32, &mut line)? {
                self.flag |= flag::ERROR;
                return Ok(None);
            }
            if line.flags & psr::C != 0 {
                return Ok(Some(0));
            }
            let length = line.r[1].min(room.saturating_sub(1));
            os.as_caller()
                .write_byte(into.wrapping_add(length), b'\n')?;
            return Ok(Some(length + 1));
        }
        if self.flag & flag::MOVE_POINTER != 0 && !self.set_pointer(os, statics)? {
            return Ok(None);
        }
        let mut transfer = registers(&[kos::gbpb::READ, self.handle, into, room]);
        if !transferred(os, statics, kos::GBPB, &mut transfer)? {
            self.flag |= flag::ERROR;
            return Ok(None);
        }
        Ok(Some(room.saturating_sub(transfer.r[3])))
    }

    /// The file's extent; none, the stream in error, where it could not be
    /// read.
    fn extent_now(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<Option<u32>, Fail> {
        let mut args = registers(&[kos::args::READ_EXTENT, self.handle]);
        if !transferred(os, statics, kos::ARGS, &mut args)? {
            self.flag |= flag::ERROR;
            return Ok(None);
        }
        Ok(Some(args.r[2]))
    }

    /// The position past `held` bytes delivered from `bufpos`: counted on,
    /// or for a stream sharing its file, read back from it.
    fn position_after(&self, os: &mut Os<'_>, statics: Statics, held: u32) -> Result<u32, Fail> {
        if self.flag & flag::SHARED != 0 {
            // A console handle has no pointer, and the count comes back.
            let mut args = registers(&[kos::args::READ_POINTER, self.handle, held]);
            if performed(os, statics, kos::ARGS, &mut args)? {
                return Ok(args.r[2]);
            }
        }
        Ok(self.bufpos.wrapping_add(held))
    }

    /// The buffer emptied once the `held` bytes it stood for are delivered.
    fn rebase(&mut self, os: &mut Os<'_>, statics: Statics, held: u32) -> Result<(), Fail> {
        self.bufpos = self.position_after(os, statics, held)?;
        self.ptr = self.base;
        self.data_end = self.base;
        Ok(())
    }

    /// The buffer moved empty to the position a positioning asked for, the
    /// file's pointer to follow it.
    const fn move_to_sought(&mut self) {
        self.bufpos = self.sought;
        self.ptr = self.base;
        self.data_end = self.base;
        self.flag = (self.flag & !(flag::REPOSITION | flag::WRITING)) | flag::MOVE_POINTER;
    }

    /// A transfer refused: the stream in error, both counts sent to the
    /// library.
    pub(crate) const fn refused(&mut self) -> u32 {
        self.flag |= flag::ERROR;
        self.icnt = 0;
        self.ocnt = 0;
        EOF
    }

    /// `fflush`'s work: an output stream's buffer delivered and emptied, a
    /// positioning it leaves pending recorded; an input stream's data made
    /// the library's to read again; a pushed-back character forgotten.
    /// False where the file refused, or an output stream is in error, whose
    /// buffer is then kept.
    pub(crate) fn flush(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
        if self.flag & flag::UNGOT != 0 {
            self.flag &= !flag::UNGOT;
            self.icnt = self.saved_icnt;
        }
        if self.flag & flag::SET_UP == 0 {
            return Ok(true);
        }
        if self.flag & flag::WRITE != 0 {
            let held = self.output();
            if self.flag & (flag::WRITING | flag::UNBUFFERED | flag::ERROR) == flag::WRITING {
                self.deliver(os, statics)?;
            }
            if self.flag & flag::ERROR != 0 {
                self.flag &= !flag::DIRTY;
                self.icnt = 0;
                self.ocnt = count(self.offset()).wrapping_sub(count(self.bufsiz));
                return Ok(false);
            }
            if self.flag & flag::WRITING != 0 {
                let position = self.bufpos.wrapping_add(self.offset());
                self.rebase(os, statics, held)?;
                if self.flag & flag::REPOSITION != 0 {
                    self.move_to_sought();
                } else if position != self.bufpos {
                    self.sought = position;
                    self.flag |= flag::REPOSITION;
                }
                self.flag &= !(flag::WRITING | flag::DIRTY | flag::READING);
                self.icnt = 0;
                self.ocnt = if self.flag & (flag::REPOSITION | flag::FULLY) == flag::FULLY {
                    count(self.bufsiz).wrapping_neg()
                } else {
                    0
                };
                return Ok(true);
            }
        }
        self.flag &= !flag::READ_VALID;
        if self.flag & flag::REPOSITION == 0 {
            self.counts_to_library();
        }
        Ok(true)
    }

    /// `__flsbuf`'s work: `c` written, the buffer delivered as its kind of
    /// buffering asks; the character, or `EOF` with the stream in error.
    /// Into the space a positioning left in a full buffer the character
    /// goes unchecked, as RISC OS 5.30 lets it.
    pub(crate) fn put(&mut self, os: &mut Os<'_>, statics: Statics, c: u8) -> Result<u32, Fail> {
        let space = self.ocnt.wrapping_add(1).wrapping_neg();
        if self.flag & flag::BUFFERING == flag::FULLY && space > 0 {
            os.as_caller().write_byte(self.ptr, c)?;
            self.ptr = self.ptr.wrapping_add(1);
            self.ocnt = space - 1;
            self.flag |= flag::WRITING | flag::DIRTY;
            return Ok(u32::from(c));
        }
        self.flag |= flag::SET_UP;
        if self.flag & flag::WRITE == 0
            || self.flag & (flag::READ_VALID | flag::ERROR) != 0
            || !self.ready(os, statics)?
        {
            return Ok(self.refused());
        }
        if self.flag & flag::REPOSITION != 0 {
            if self.flag & flag::WRITING != 0 && !self.deliver(os, statics)? {
                return Ok(self.refused());
            }
            self.move_to_sought();
        }
        if self.flag & flag::UNBUFFERED != 0 {
            return self.put_unbuffered(os, statics, c);
        }
        if self.offset() >= self.bufsiz {
            let held = self.output();
            if !self.deliver(os, statics)? {
                return Ok(self.refused());
            }
            self.rebase(os, statics, held)?;
        }
        os.as_caller().write_byte(self.ptr, c)?;
        self.ptr = self.ptr.wrapping_add(1);
        if self.flag & flag::LINE != 0 {
            if c == b'\n' || self.offset() >= self.bufsiz {
                let held = self.output();
                if !self.deliver(os, statics)? {
                    return Ok(self.refused());
                }
                self.rebase(os, statics, held)?;
            }
            self.ocnt = count(self.offset()).wrapping_neg();
        } else {
            self.data_end = self.data_end_now();
            self.ocnt = count(self.bufsiz).wrapping_sub(count(self.offset()));
        }
        self.flag |= flag::WRITING | flag::DIRTY;
        Ok(u32::from(c))
    }

    /// `c` delivered at once through the one-byte buffer.
    fn put_unbuffered(&mut self, os: &mut Os<'_>, statics: Statics, c: u8) -> Result<u32, Fail> {
        self.bytes[0] = c;
        os.as_caller().write_byte(self.base, c)?;
        let delivered = if self.console() {
            self.console_out(os, statics, &[c])?
        } else {
            self.write_out(os, statics, self.base, 1)?
        };
        if !delivered {
            return Ok(self.refused());
        }
        if !self.console() || self.flag & flag::SHARED != 0 {
            self.bufpos = self.position_after(os, statics, 1)?;
        }
        self.ptr = self.base;
        self.data_end = self.base;
        self.ocnt = 0;
        self.flag |= flag::WRITTEN | flag::WRITING | flag::DIRTY;
        Ok(u32::from(c))
    }

    /// `__filbuf`'s work: the next character, a pushed-back one first, the
    /// buffer refilled where it is spent — output it holds delivered first,
    /// and kept where room is left after it; `EOF` at the end, or with the
    /// stream in error.
    pub(crate) fn get(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<u32, Fail> {
        if self.flag & flag::UNGOT != 0 {
            self.flag &= !flag::UNGOT;
            self.icnt = self.saved_icnt;
            return Ok(u32::from(self.bytes[1]));
        }
        self.flag |= flag::SET_UP;
        if self.flag & flag::READ == 0
            || self.flag & (flag::DIRTY | flag::ERROR) != 0
            || !self.ready(os, statics)?
        {
            return Ok(self.refused());
        }
        if self.flag & flag::REPOSITION != 0 {
            if self.flag & flag::WRITING != 0
                && !self.write_out(os, statics, self.base, self.output())?
            {
                return Ok(self.refused());
            }
            self.move_to_sought();
            self.flag &= !flag::WRITTEN;
            if !self.set_pointer(os, statics)? {
                return Ok(self.refused());
            }
        }
        self.flag |= flag::READ_VALID;
        self.data_end = self.data_end_now();
        if self.ptr.wrapping_sub(self.data_end).cast_signed() < 0 {
            return self.next_held(os);
        }
        let held = self.data();
        let writing = self.flag & flag::WRITING != 0;
        if writing {
            if !self.write_out(os, statics, self.base, held)? {
                return Ok(self.refused());
            }
            self.flag |= flag::READING;
        }
        let kept = writing && held < self.bufsiz;
        let (into, room) = if kept {
            (self.data_end, self.bufsiz - held)
        } else {
            self.bufpos = self.bufpos.wrapping_add(held);
            self.flag &= !flag::WRITING;
            self.ocnt = 0;
            (self.base, self.bufsiz)
        };
        let Some(read) = self.device_read(os, statics, into, room)? else {
            return Ok(self.refused());
        };
        if !kept {
            self.ptr = self.base;
            self.data_end = self.base;
        }
        if read == 0 {
            self.flag = (self.flag | flag::EOF) & !flag::READ_VALID;
            self.icnt = 0;
            return Ok(EOF);
        }
        self.flag |= flag::READING;
        self.data_end = into.wrapping_add(read);
        self.next_held(os)
    }

    /// The next character the buffer holds.
    fn next_held(&mut self, os: &mut Os<'_>) -> Result<u32, Fail> {
        let c = os.as_caller().read_byte(self.ptr)?;
        if self.flag & flag::UNBUFFERED != 0 {
            // The one-byte buffer is the extra block's, which `store` writes.
            self.bytes[0] = c;
        }
        self.ptr = self.ptr.wrapping_add(1);
        self.icnt = count(self.data_end.wrapping_sub(self.ptr));
        Ok(u32::from(c))
    }

    /// A character read as the `getc` macro reads one: from the buffer
    /// while its count lasts, else through `__filbuf`'s work.
    pub(crate) fn getc(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<u32, Fail> {
        self.icnt = self.icnt.wrapping_sub(1);
        if self.icnt >= 0 {
            let c = os.as_caller().read_byte(self.ptr)?;
            self.ptr = self.ptr.wrapping_add(1);
            return Ok(u32::from(c));
        }
        self.get(os, statics)
    }

    /// `c` written as the `putc` macro writes it.
    pub(crate) fn putc(&mut self, os: &mut Os<'_>, statics: Statics, c: u8) -> Result<u32, Fail> {
        self.ocnt = self.ocnt.wrapping_sub(1);
        if self.ocnt >= 0 {
            os.as_caller().write_byte(self.ptr, c)?;
            self.ptr = self.ptr.wrapping_add(1);
            return Ok(u32::from(c));
        }
        self.put(os, statics, c)
    }

    /// The bytes the `getc` macro takes from the buffer before it calls
    /// `__filbuf`, at most `most`, and none past the page they start in: so a
    /// block read aborts where a byte-at-a-time one would, every byte before
    /// moved.
    fn held_run(&self, most: u32) -> u32 {
        u32::try_from(self.icnt)
            .unwrap_or(0)
            .min(most)
            .min(to_page_end(self.ptr))
    }

    /// The room the `putc` macro writes into before it calls `__flsbuf`, at
    /// most `most`.
    fn room_run(&self, most: u32) -> u32 {
        u32::try_from(self.ocnt).unwrap_or(0).min(most)
    }

    /// `n` of the bytes [`Self::held_run`] gave taken.
    const fn take_held(&mut self, n: u32) {
        self.ptr = self.ptr.wrapping_add(n);
        self.icnt -= count(n);
    }

    /// `n` of the room [`Self::room_run`] gave filled.
    const fn take_room(&mut self, n: u32) {
        self.ptr = self.ptr.wrapping_add(n);
        self.ocnt -= count(n);
    }

    /// Up to `length` bytes read to `into` as `getc` reads them, the
    /// buffer's run copied at once: how many before the input ended.
    pub(crate) fn read_into(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        into: u32,
        length: u32,
    ) -> Result<u32, Fail> {
        let mut read = 0;
        while read < length {
            let run = self.held_run(length - read);
            if run > 0 {
                os.as_caller()
                    .copy_bytes(self.ptr, into.wrapping_add(read), run)?;
                self.take_held(run);
                read += run;
                continue;
            }
            let c = self.getc(os, statics)?;
            if c == EOF {
                break;
            }
            os.as_caller()
                .write_byte(into.wrapping_add(read), c.to_le_bytes()[0])?;
            read += 1;
        }
        Ok(read)
    }

    /// A line of at most `most` bytes read to `into` as `getc` reads it, the
    /// newline kept where `keep_newline`: how many, a kept newline counted,
    /// and whether the input ended before any.
    pub(crate) fn read_line(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        into: u32,
        most: u32,
        keep_newline: bool,
    ) -> Result<(u32, bool), Fail> {
        let mut length = 0;
        while length < most {
            let run = self.held_run(most - length);
            if run > 0 {
                let newline = os.as_caller().position(self.ptr, run, |b| b == b'\n')?;
                let taken = newline.map_or(run, |at| at + 1);
                let kept = taken - u32::from(newline.is_some() && !keep_newline);
                os.as_caller()
                    .copy_bytes(self.ptr, into.wrapping_add(length), kept)?;
                self.take_held(taken);
                length += kept;
                if newline.is_some() {
                    return Ok((length, false));
                }
                continue;
            }
            let c = self.getc(os, statics)?;
            if c == EOF {
                return Ok((length, length == 0));
            }
            if c == u32::from(b'\n') && !keep_newline {
                return Ok((length, false));
            }
            os.as_caller()
                .write_byte(into.wrapping_add(length), c.to_le_bytes()[0])?;
            length += 1;
            if c == u32::from(b'\n') {
                break;
            }
        }
        Ok((length, false))
    }

    /// `bytes` written as `putc` writes them, the buffer's room filled at
    /// once: false at the first refused.
    pub(crate) fn write_all(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        bytes: &[u8],
    ) -> Result<bool, Fail> {
        let mut rest = bytes;
        while let Some((&first, after)) = rest.split_first() {
            let run = self.room_run(len32(rest.len()));
            if run > 0 {
                let (now, later) = rest.split_at(run as usize);
                os.as_caller().write_bytes(self.ptr, now)?;
                self.take_room(run);
                rest = later;
                continue;
            }
            if self.putc(os, statics, first)? == EOF {
                return Ok(false);
            }
            rest = after;
        }
        Ok(true)
    }

    /// Up to `length` bytes of the guest's from `from` written as `putc`
    /// writes them, the buffer's room filled at once: how many before a
    /// refusal.
    pub(crate) fn write_from(
        &mut self,
        os: &mut Os<'_>,
        statics: Statics,
        from: u32,
        length: u32,
    ) -> Result<u32, Fail> {
        let mut written = 0;
        while written < length {
            let at = from.wrapping_add(written);
            let run = self.room_run(length - written).min(to_page_end(at));
            if run > 0 {
                os.as_caller().copy_bytes(at, self.ptr, run)?;
                self.take_room(run);
                written += run;
                continue;
            }
            let c = os.as_caller().read_byte(at)?;
            if self.putc(os, statics, c)? == EOF {
                break;
            }
            written += 1;
        }
        Ok(written)
    }

    /// `ungetc`'s work: `c` pushed back, one character at most.
    pub(crate) fn unget(&mut self, c: u32) -> u32 {
        if c == EOF || self.flag & flag::UNGOT != 0 {
            return EOF;
        }
        let byte = c.to_le_bytes()[0];
        self.bytes[1] = byte;
        self.saved_icnt = self.icnt;
        self.icnt = 0;
        self.flag = (self.flag | flag::UNGOT) & !flag::EOF;
        u32::from(byte)
    }

    /// The position the next transfer is at — the one a positioning asked
    /// for, while it is pending — a pushed-back character counted off.
    pub(crate) fn tell(&self) -> u32 {
        let pushed = u32::from(self.flag & flag::UNGOT != 0);
        let at = if self.flag & flag::REPOSITION != 0 {
            self.sought
        } else {
            self.bufpos.wrapping_add(self.offset())
        };
        at.wrapping_sub(pushed)
    }

    /// `fseek`'s work, recorded and nothing delivered: `ptr` moved to
    /// `target` where the buffer holds it with room past it, else the move
    /// left for the next transfer to make.
    pub(crate) fn seek(&mut self, target: u32) {
        if self.flag & flag::DIRTY != 0 {
            self.data_end = self.data_end_now();
        }
        self.flag &= !(flag::UNGOT | flag::EOF | flag::READ_VALID | flag::DIRTY);
        let into = target.wrapping_sub(self.bufpos);
        if self.flag & flag::SET_UP != 0
            && target >= self.bufpos
            && into <= self.data()
            && into < self.bufsiz
        {
            self.ptr = self.base.wrapping_add(into);
            self.flag &= !flag::REPOSITION;
            self.counts_to_library();
        } else {
            self.sought = target;
            self.flag |= flag::REPOSITION;
            self.icnt = 0;
            self.ocnt = 0;
        }
    }

    /// The file's end as the stream sees it, nothing delivered: its extent,
    /// or past that the buffer's data; none where the extent could not be
    /// read.
    pub(crate) fn end(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<Option<u32>, Fail> {
        let Some(extent) = self.extent_now(os, statics)? else {
            return Ok(None);
        };
        let buffered = self
            .bufpos
            .wrapping_add(self.data_end_now().wrapping_sub(self.base));
        Ok(Some(extent.max(buffered)))
    }

    /// `fclose`'s work: flushed, its file closed — a shared file left to the
    /// stream sharing it — its temporary file removed, its buffer given
    /// back, and its blocks cleared but for the extra block's pointer.
    /// False where flushing or closing failed.
    pub(crate) fn close(&mut self, os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
        let mut closed = self.flush(os, statics)?;
        if self.flag & flag::SHARED != 0 {
            unshare(os, statics, self.at, self.handle)?;
        } else if !self.console() {
            closed &= transferred(
                os,
                statics,
                kos::FIND,
                &mut registers(&[kos::find::CLOSE, self.handle]),
            )?;
        }
        if self.remove != 0 {
            performed(
                os,
                statics,
                kos::FILE,
                &mut registers(&[kos::file::DELETE, self.remove]),
            )?;
            alloc::freed(os, statics, self.remove)?;
        }
        if self.flag & flag::SYSTEM_BUFFER != 0 {
            alloc::freed(os, statics, self.base)?;
        }
        *self = Self {
            at: self.at,
            extra: self.extra,
            ..Self::default()
        };
        Ok(closed)
    }
}

/// `text` written to the console, each newline as `OS_NewLine` writes one:
/// false, the OS's error noted, where it was refused.
pub(crate) fn to_console(os: &mut Os<'_>, statics: Statics, text: &[u8]) -> Result<bool, Fail> {
    for run in text.split_inclusive(|&b| b == b'\n') {
        let (line, newline) = match run.split_last() {
            Some((b'\n', line)) => (line, true),
            _ => (run, false),
        };
        for &b in line {
            if !performed(os, statics, kos::WRITE_C, &mut registers(&[u32::from(b)]))? {
                return Ok(false);
            }
        }
        if newline && !performed(os, statics, kos::NEW_LINE, &mut Regs::default())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Registers holding `low` from R0 on.
pub(crate) fn registers(low: &[u32]) -> Regs {
    let mut regs = Regs::default();
    regs.r[..low.len()].copy_from_slice(low);
    regs
}

/// `swi` performed with its X bit: false, its error noted for
/// `_kernel_last_oserror`, where it failed.
pub(crate) fn performed(
    os: &mut Os<'_>,
    statics: Statics,
    swi: u32,
    regs: &mut Regs,
) -> Result<bool, Fail> {
    match os.perform(X | swi, regs) {
        Ok(()) => Ok(true),
        Err(Fail::Error(block)) => {
            note(os, statics, block)?;
            Ok(false)
        }
        Err(unwind) => Err(unwind),
    }
}

/// [`performed`] for a stream's own use of its file, which `errno` reports
/// the refusal of.
pub(crate) fn transferred(
    os: &mut Os<'_>,
    statics: Statics,
    swi: u32,
    regs: &mut Regs,
) -> Result<bool, Fail> {
    if performed(os, statics, swi, regs)? {
        return Ok(true);
    }
    errno::set(os, statics, OS_REFUSED)?;
    Ok(false)
}

/// The `FILE` block of stream `n`, `stdin`'s nought.
pub(crate) fn file_at(statics: Statics, n: u32) -> u32 {
    statics.field(at::STREAMS + STREAM_BYTES * n)
}

/// Whether `at` is one of the library's `FILE` blocks.
pub(crate) fn is_stream(statics: Statics, at: u32) -> bool {
    let into = at.wrapping_sub(file_at(statics, 0));
    into.is_multiple_of(STREAM_BYTES) && into / STREAM_BYTES < STREAMS
}

/// The standard streams' blocks.
pub(crate) fn stdin(statics: Statics) -> u32 {
    file_at(statics, 0)
}

pub(crate) fn stdout(statics: Statics) -> u32 {
    file_at(statics, 1)
}

pub(crate) fn stderr(statics: Statics) -> u32 {
    file_at(statics, 2)
}

/// The streams laid as `_clib_initialise` lays them: an extra block each,
/// cut from one block of the heap, and the standard streams' flags.
pub(crate) fn start(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let blocks = alloc::allocated(os, statics, EXTRA_BYTES * STREAMS)?;
    if blocks != 0 {
        os.as_caller().fill(blocks, EXTRA_BYTES * STREAMS, 0)?;
    }
    for n in 0..STREAMS {
        let at = file_at(statics, n);
        let (flag, bufsiz) = match n {
            0 => (flag::READ, BUFSIZ),
            1 | 2 => (flag::WRITE, BUFSIZ),
            _ => (0, 0),
        };
        let extra = if blocks == 0 {
            0
        } else {
            blocks + EXTRA_BYTES * n
        };
        let stream = Stream {
            at,
            flag,
            bufsiz,
            extra,
            ..Stream::default()
        };
        stream.store(os)?;
    }
    Ok(())
}

/// Every open stream closed, as the C language's finalisation closes them.
pub(crate) fn close_all(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    for n in 0..STREAMS {
        let mut stream = Stream::load(os, file_at(statics, n))?;
        if stream.flag & (flag::READ | flag::WRITE) != 0 {
            stream.close(os, statics)?;
            stream.store(os)?;
        }
    }
    Ok(())
}

/// The other open stream sharing the file `handle` with the one at `at`
/// told that it holds the file alone now.
fn unshare(os: &mut Os<'_>, statics: Statics, at: u32, handle: u32) -> Result<(), Fail> {
    for n in 0..STREAMS {
        let other = file_at(statics, n);
        let (flag_at, handle_at) = (
            other.wrapping_add(word::FLAG),
            other.wrapping_add(word::HANDLE),
        );
        let flags = os.as_caller().read_word(flag_at)?;
        if other != at
            && flags & flag::SHARED != 0
            && os.as_caller().read_word(handle_at)? == handle
        {
            os.as_caller().write_word(flag_at, flags & !flag::SHARED)?;
        }
    }
    Ok(())
}

/// Every line-buffered output stream but the one at `at` flushed, as
/// interactive input asks.
fn flush_lines(os: &mut Os<'_>, statics: Statics, at: u32) -> Result<(), Fail> {
    for n in 0..STREAMS {
        let other = file_at(statics, n);
        if other != at
            && os.as_caller().read_word(other.wrapping_add(word::FLAG))?
                & (flag::LINE | flag::WRITE)
                == flag::LINE | flag::WRITE
        {
            let mut stream = Stream::load(os, other)?;
            stream.flush(os, statics)?;
            stream.store(os)?;
        }
    }
    Ok(())
}

/// Every open output stream flushed, as `fflush(NULL)` and `system` flush
/// them: false where one refused.
pub(crate) fn flush_all(os: &mut Os<'_>, statics: Statics) -> Result<bool, Fail> {
    let mut flushed = true;
    for n in 0..STREAMS {
        let mut stream = Stream::load(os, file_at(statics, n))?;
        if stream.flag & flag::WRITE != 0 {
            flushed &= stream.flush(os, statics)?;
            stream.store(os)?;
        }
    }
    Ok(flushed)
}

#[cfg(test)]
mod tests;
