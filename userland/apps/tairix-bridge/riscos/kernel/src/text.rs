//! The kernel's text SWIs (`plans/RISCOS-EMULATOR.md` §4.5): `OS_CRC`,
//! `OS_HeapSort`, `OS_PrettyPrint` with the kernel's dictionary,
//! `OS_Confirm`, and `OS_ReadLine` through `ReadLineV`.
//!
//! A line read that waits for a key keeps what it has read, so the `SWI`
//! performed again when the key comes carries on where it left off.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use crate::dictionary::ESCAPE;
use crate::input::buttons::SELECT;
use crate::input::Continuation;
use crate::memory::Bits;
use crate::os::{len32, Entry, Fail, Os, Regs, Target, Unwind};
use crate::vectors::{self, Carry};

/// `OS_ReadLine`'s flags in R0: echo R4's character, echo only what is
/// stored.
const READ_LINE_FLAGS: u32 = 0xC000_0000;

/// How deep dictionary entries may nest: a bound on a hostile dictionary
/// that names itself.
const NESTING: usize = 16;

/// The CRC `OS_CRC` computes: CRC-16 with the reflected polynomial &A001,
/// from `crc`, over `bytes`. All 32 bits of `crc` are carried, as genuine
/// RISC OS 5 carries its register, so a start past 16 bits shifts down into
/// the result, where `lib/crc16`'s 16-bit CRC would drop it.
pub(crate) fn crc16(crc: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(crc, |mut crc, &byte| {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
        crc
    })
}

/// `OS_HeapSort32`'s flags: the temporary slot given is used whatever the
/// block size; the pointers are made from the blocks first; the blocks are
/// put in their pointers' order after.
const OWN_TEMPORARY: u32 = 1 << 29;
const BUILD_POINTERS: u32 = 1 << 30;
const REORDER_BLOCKS: u32 = 1 << 31;

/// `Wimp_BaseOfSprites`.
const BASE_OF_SPRITES: u32 = 0x400EA;
/// The Wimp's sprite `OS_Confirm` shows as the pointer.
const CONFIRM_POINTER: &[u8] = b"ptr_confirm\0";
/// `OS_Byte` 106's pointer shape number, none if zero.
const POINTER_SHAPE: u32 = 15;

/// `OS_Confirm` under way: the pointer state to restore, nonzero only
/// while the mouse answers too, and until when buttons held from before it
/// began are ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Confirm {
    pointer: u32,
    held_until: Option<u64>,
}

impl Os<'_> {
    /// `OS_CRC`: R0's CRC carried over the bytes from R1 to R2, R3 apart.
    pub(crate) fn crc(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (mut at, end, step) = (regs.r[1], regs.r[2], regs.r[3].cast_signed());
        if step == 0 {
            return Err(self.error(crate::error::BAD_PARAMETERS));
        }
        let mut crc = regs.r[0];
        while at != end {
            let byte = self.read_byte(at)?;
            crc = crc16(crc, &[byte]);
            at = at.wrapping_add_signed(step);
            self.yield_point()?;
        }
        regs.r[0] = crc;
        Ok(())
    }

    /// `OS_HeapSort32`.
    pub(crate) fn heap_sort32(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let r = regs.r;
        self.heap_sort(r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7])
    }

    /// `OS_HeapSort`: its flags in R1's top bits, building the pointers
    /// reordering the blocks too.
    pub(crate) fn heap_sort_legacy(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let r = regs.r;
        let mut flags = r[1] & (OWN_TEMPORARY | REORDER_BLOCKS);
        if r[1] & BUILD_POINTERS != 0 {
            flags |= BUILD_POINTERS | REORDER_BLOCKS;
        }
        self.heap_sort(
            r[0],
            r[1] & 0x1FFF_FFFF,
            r[2],
            r[3],
            r[4],
            r[5],
            r[6],
            flags,
        )
    }

    /// Sorts the `n` words at `array` ascending by `compare` — a built-in
    /// order or guest code — then, if asked, the blocks they point at.
    #[expect(
        clippy::too_many_arguments,
        reason = "one per register OS_HeapSort32 takes"
    )]
    fn heap_sort(
        &mut self,
        n: u32,
        array: u32,
        compare: u32,
        workspace: u32,
        blocks: u32,
        size: u32,
        temporary: u32,
        flags: u32,
    ) -> Result<(), Fail> {
        if n < 2 {
            return Ok(());
        }
        if flags & BUILD_POINTERS != 0 {
            for i in 0..n {
                self.write_word(
                    array.wrapping_add(4 * i),
                    blocks.wrapping_add(i.wrapping_mul(size)),
                )?;
            }
        }
        // Knuth's algorithm H, the heap sort the PRM cites, over R(1) to
        // R(n), its l and r here lower and upper: equal keys end in the order
        // the algorithm leaves them.
        let item = |k: u32| array.wrapping_add((k - 1).wrapping_mul(4));
        let (mut lower, mut upper) = (n / 2 + 1, n);
        loop {
            let record = if lower > 1 {
                lower -= 1;
                self.read_word(item(lower))?
            } else {
                let record = self.read_word(item(upper))?;
                let first = self.read_word(item(1))?;
                self.write_word(item(upper), first)?;
                upper -= 1;
                if upper == 1 {
                    self.write_word(item(1), record)?;
                    break;
                }
                record
            };
            let mut j = lower;
            let hole = loop {
                let i = j;
                j = match j.checked_mul(2) {
                    Some(j) if j <= upper => j,
                    _ => break i,
                };
                if j < upper {
                    let (left, right) = (self.read_word(item(j))?, self.read_word(item(j + 1))?);
                    if self.less(left, right, compare, workspace)? {
                        j += 1;
                    }
                }
                let child = self.read_word(item(j))?;
                if !self.less(record, child, compare, workspace)? {
                    break i;
                }
                self.write_word(item(i), child)?;
                self.yield_point()?;
            };
            self.write_word(item(hole), record)?;
        }
        if flags & REORDER_BLOCKS != 0 {
            self.reorder_blocks(n, array, blocks, size, temporary, flags)?;
        }
        Ok(())
    }

    /// Whether `x` sorts before `y` by the built-in order or guest routine
    /// `compare`.
    fn less(&mut self, x: u32, y: u32, compare: u32, workspace: u32) -> Result<bool, Fail> {
        Ok(match compare {
            0 => x < y,
            1 => x.cast_signed() < y.cast_signed(),
            2 => self.read_word(x)? < self.read_word(y)?,
            3 => self.read_word(x)?.cast_signed() < self.read_word(y)?.cast_signed(),
            4 | 5 => self.string_less(x, y, compare == 4)?,
            routine => {
                let mut regs = Regs::new([x, y, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, workspace]);
                let target = Target {
                    address: routine,
                    bits: self.caller_bits(),
                };
                self.call(target, &mut regs, Entry::SupervisorIrq)?;
                regs.flag(psr::N) != regs.flag(psr::V)
            }
        })
    }

    /// Whether the control-terminated string at `x` sorts before the one at
    /// `y`, byte by byte with the terminators compared too, case folded
    /// when `fold`.
    fn string_less(&mut self, x: u32, y: u32, fold: bool) -> Result<bool, Fail> {
        let key = |c: u8| if fold { latin1_lower(c) } else { c };
        let (mut x, mut y) = (x, y);
        loop {
            let a = key(self.read_byte(x)?);
            let b = key(self.read_byte(y)?);
            if a != b {
                return Ok(a < b);
            }
            if a < b' ' {
                return Ok(false);
            }
            (x, y) = (x.wrapping_add(1), y.wrapping_add(1));
            self.yield_point()?;
        }
    }

    /// Moves the blocks into the order their pointers were sorted into, a
    /// cycle at a time through the temporary slot, a pointer zeroed as its
    /// block moves to mark it done; then each pointer points at its block
    /// where it now is. Pointers that are no permutation of the blocks stop
    /// the moves.
    fn reorder_blocks(
        &mut self,
        n: u32,
        array: u32,
        blocks: u32,
        size: u32,
        temporary: u32,
        flags: u32,
    ) -> Result<(), Fail> {
        let scratch = crate::memory::layout::SCRATCH;
        let temporary = if flags & OWN_TEMPORARY != 0 || size > scratch.end - scratch.start {
            temporary
        } else {
            scratch.start
        };
        let pointer = |k: u32| array.wrapping_add(k.wrapping_mul(4));
        let block = |k: u32| blocks.wrapping_add(k.wrapping_mul(size));
        let mut moved = 0;
        let mut first = 0;
        loop {
            let mut item = first;
            let mut current = block(first);
            self.copy_bytes(current, temporary, size)?;
            loop {
                let next = self.read_word(pointer(item))?;
                self.write_word(pointer(item), 0)?;
                moved += 1;
                let offset = next.wrapping_sub(blocks);
                let Some(index) = offset
                    .checked_div(size)
                    .filter(|&k| k < n && offset % size == 0 && moved <= n)
                else {
                    return Ok(());
                };
                if index == first {
                    self.copy_bytes(temporary, current, size)?;
                    break;
                }
                self.copy_bytes(next, current, size)?;
                current = next;
                item = index;
            }
            loop {
                first += 1;
                if first >= n {
                    for k in 0..n {
                        self.write_word(pointer(k), block(k))?;
                        self.yield_point()?;
                    }
                    return Ok(());
                }
                if self.read_word(pointer(first))? != 0 {
                    break;
                }
            }
        }
    }

    /// `OS_PrettyPrint`: R0's text formatted to the window's width, R1's
    /// dictionary (the kernel's if zero) expanding its escapes, R2 the
    /// text an escape and zero names.
    pub(crate) fn pretty_print(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        // The byte after an escape is the escape's, even where it is zero.
        let mut escaped = false;
        let text = self.read_ended(regs.r[0], 1 << 16, |b| {
            let end = b == 0 && !escaped;
            escaped = !escaped && b == ESCAPE;
            end
        })?;
        let special = if regs.r[2] == 0 {
            Vec::new()
        } else {
            self.read_zstring(regs.r[2], 1 << 12)?
        };
        self.pretty_with(&text, regs.r[1], &special)
    }

    /// Writes `text` as `OS_PrettyPrint` does, `dictionary` expanding its
    /// escapes and `special` its escape and zero.
    pub(crate) fn pretty_with(
        &mut self,
        text: &[u8],
        dictionary: u32,
        special: &[u8],
    ) -> Result<(), Fail> {
        let mut expanded = Vec::new();
        self.expand_tokens(text, dictionary, special, 0, &mut expanded)?;
        self.pretty(&expanded)
    }

    /// `text` with each escape and token expanded from `dictionary`.
    fn expand_tokens(
        &mut self,
        text: &[u8],
        dictionary: u32,
        special: &[u8],
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), Fail> {
        let mut bytes = text.iter();
        while let Some(&b) = bytes.next() {
            if b != ESCAPE {
                if out.try_reserve(1).is_err() {
                    return Err(self.no_memory());
                }
                out.push(b);
                continue;
            }
            let Some(&n) = bytes.next() else { break };
            if depth >= NESTING {
                continue;
            }
            if n == 0 {
                self.expand_tokens(special, dictionary, special, depth + 1, out)?;
                continue;
            }
            let entry = self.dictionary_entry(dictionary, n)?;
            self.expand_tokens(&entry, dictionary, special, depth + 1, out)?;
        }
        Ok(())
    }

    /// Entry `n` of the dictionary at `dictionary`, the kernel's for zero;
    /// none past its end. An entry's length byte counts itself and its
    /// terminator, and its text is read by that length, since an escape and
    /// zero may be in it.
    fn dictionary_entry(&mut self, dictionary: u32, n: u8) -> Result<Vec<u8>, Fail> {
        let mut at = if dictionary == 0 {
            self.world.dictionaries.kernel
        } else {
            dictionary
        };
        for index in 1..=n {
            let length = self.read_byte(at)?;
            if length == 0 {
                return Ok(Vec::new());
            }
            if index == n {
                let size = usize::from(length.saturating_sub(2));
                return self.read_vec(at.wrapping_add(1), size);
            }
            at = at.wrapping_add(u32::from(length));
        }
        Ok(Vec::new())
    }

    /// Writes `text` as `OS_PrettyPrint` lays out a string it need expand
    /// nothing of.
    pub(crate) fn pretty(&mut self, text: &[u8]) -> Result<(), Fail> {
        let width = self.window_width()?;
        let column = self.output_column()?;
        self.lay_out(text, width, column)
    }

    /// Lays `text` out in lines of `width` columns from column `column`.
    fn lay_out(&mut self, text: &[u8], width: u32, mut column: u32) -> Result<(), Fail> {
        let width = width.max(1);
        let mut words = text.split_inclusive(|&b| b <= b' ' && b != 31).peekable();
        let mut first = true;
        while let Some(chunk) = words.next() {
            let (word, separator) = match chunk.last() {
                Some(&b) if b <= b' ' && b != 31 => (&chunk[..chunk.len() - 1], Some(b)),
                _ => (chunk, None),
            };
            if !word.is_empty() {
                let length = len32(word.len());
                if !first && column + 1 + length > width {
                    self.write_text(b"\n\r")?;
                    column = 0;
                } else if !first && column > 0 {
                    self.write_c(b' ')?;
                    column += 1;
                }
                for &c in word {
                    self.write_c(if c == 31 { b' ' } else { c })?;
                }
                column += length;
                first = false;
            }
            match separator {
                Some(b'\r') => {
                    self.write_text(b"\n\r")?;
                    column = 0;
                    first = true;
                }
                Some(b'\t') => {
                    let next = (column / 8 + 1) * 8;
                    let following = words.peek().map_or(0, |w| len32(w.len()));
                    if next + following > width {
                        self.write_text(b"\n\r")?;
                        column = 0;
                    } else {
                        while column < next {
                            self.write_c(b' ')?;
                            column += 1;
                        }
                    }
                    first = true;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The text window's width, from the VDU drivers; 80 without them.
    pub(crate) fn window_width(&mut self) -> Result<u32, Unwind> {
        let block = self.world.scratch;
        let (Some(()), Some(())) = (
            self.poke_word(block, tairix_vdu::mode::vdu_var::WINDOW_WIDTH),
            self.poke_word(block + 4, u32::MAX),
        ) else {
            return Ok(80);
        };
        let mut regs = Regs::new([block, block + 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let read = self.perform_quietly(
            crate::swi::X | crate::swi::os::READ_VDU_VARIABLES,
            &mut regs,
        )?;
        Ok(read
            .then(|| self.peek_word(block + 8))
            .flatten()
            .map_or(80, |last| last.wrapping_add(1)))
    }

    /// The output cursor's column (`OS_Byte` 165), zero without VDU drivers.
    fn output_column(&mut self) -> Result<u32, Unwind> {
        let mut regs = Regs::new([165, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let read = self.perform_quietly(crate::swi::X | crate::swi::os::BYTE, &mut regs)?;
        Ok(if read { regs.r[1] } else { 0 })
    }

    /// `OS_Confirm`: a key read until one comes, lowered; Z set if it was
    /// the yes letter, C set on escape. Where the Wimp's confirm pointer can
    /// be shown, the mouse answers too: Select yes, the other buttons no.
    pub(crate) fn confirm(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let letters = self.message_with(b"ConfirmKeys", &[]).unwrap_or_default();
        let yes = letters.first().copied().unwrap_or(b'y');
        let no = letters.get(1).copied().unwrap_or(b'n');
        let mut state = match self.world.continuation.take() {
            Some(Continuation::Confirm(state)) => state,
            _ => self.begin_confirm()?,
        };
        let (c, escape) = loop {
            if let Some(until) = state.held_until {
                if self.mouse_buttons()? != 0 && self.now() < until {
                    self.wait_for_input(Some(until), Some(Continuation::Confirm(state)))?;
                    continue;
                }
                state.held_until = None;
            }
            let mut inkey = Regs::new([129, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.perform(crate::swi::X | crate::swi::os::BYTE, &mut inkey)?;
            if inkey.r[2] != 0xFF {
                break (inkey.byte(1), inkey.r[2] == 0x1B);
            }
            if state.pointer != 0 {
                let buttons = self.mouse_buttons()?;
                if buttons != 0 {
                    break (if buttons & SELECT != 0 { yes } else { no }, false);
                }
            }
            self.wait_for_input(None, Some(Continuation::Confirm(state)))?;
        };
        if state.pointer != 0 {
            let mut restore = Regs::new([106, state.pointer, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.perform_quietly(crate::swi::X | crate::swi::os::BYTE, &mut restore)?;
        }
        let c = latin1_lower(c);
        regs.r[0] = u32::from(c);
        regs.set_flag(psr::C, escape);
        regs.set_flag(psr::Z, c == yes);
        Ok(())
    }

    /// `OS_Confirm`'s start: the pointer read and, if it is on and the
    /// Wimp's confirm pointer is found, that shown, the mouse buffer flushed
    /// and buttons already held ignored for an auto-repeat delay.
    fn begin_confirm(&mut self) -> Result<Confirm, Fail> {
        let unused = Confirm {
            pointer: 0,
            held_until: None,
        };
        let mut read = Regs::new([106, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform(crate::swi::X | crate::swi::os::BYTE, &mut read)?;
        let pointer = read.r[1];
        if pointer & POINTER_SHAPE == 0 {
            return Ok(unused);
        }
        let mut bases = Regs::default();
        if !self.perform_quietly(crate::swi::X | BASE_OF_SPRITES, &mut bases)? {
            return Ok(unused);
        }
        let name = self.scratch_text(CONFIRM_POINTER)?;
        let mut found = None;
        for area in [bases.r[1], bases.r[0]] {
            let mut info = Regs::new([0x128, area, name, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            if self.perform_quietly(crate::swi::X | crate::swi::os::SPRITE_OP, &mut info)? {
                found = Some(area);
                break;
            }
        }
        let Some(area) = found else {
            return Ok(unused);
        };
        let scale = name + crate::os::len32(CONFIRM_POINTER.len()).next_multiple_of(4);
        self.write_words(scale, &[1; 4])?;
        let mut shape = Regs::new([0x124, area, name, 0x23, 0, 0, scale, 0, 0, 0, 0, 0, 0]);
        self.perform_quietly(crate::swi::X | crate::swi::os::SPRITE_OP, &mut shape)?;
        let mut flush = Regs::new([21, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform_quietly(crate::swi::X | crate::swi::os::BYTE, &mut flush)?;
        let mut delay = Regs::new([0xC4, 0, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        self.perform_quietly(crate::swi::X | crate::swi::os::BYTE, &mut delay)?;
        Ok(Confirm {
            pointer,
            held_until: Some(self.now() + u64::from(delay.r[1])),
        })
    }

    /// The mouse buttons `OS_Mouse` reads.
    fn mouse_buttons(&mut self) -> Result<u32, Fail> {
        let mut mouse = Regs::default();
        self.perform(crate::swi::X | crate::swi::os::MOUSE, &mut mouse)?;
        Ok(mouse.r[2])
    }

    /// `OS_ReadLine`: `OS_ReadLine32` into R0's address less its top two
    /// bits, which come back in R0. From a 26-bit caller those bits are the
    /// flags and R4 the echo character, as the PRM has them; RISC OS 5 ignores
    /// both, so a 32-bit caller's are ignored too. R4 comes back as given.
    pub(crate) fn read_line(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let mut line = *regs;
        line.r[0] = regs.r[0] & !READ_LINE_FLAGS;
        line.r[4] = match self.caller_bits() {
            Bits::Bit26 => (regs.r[0] & READ_LINE_FLAGS) | (regs.r[4] & 0xFF),
            Bits::Bit32 => 0,
        };
        let read = self.vectored(
            vectors::READ_LINE,
            &mut line,
            Entry::SupervisorIrq,
            Carry::All,
        );
        regs.r[0] = line.r[0];
        regs.r[1..4].copy_from_slice(&line.r[1..4]);
        regs.r[5..10].copy_from_slice(&line.r[5..10]);
        regs.flags = line.flags;
        read
    }

    /// `OS_ReadLine32`, through `ReadLineV`.
    pub(crate) fn read_line32(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.vectored(vectors::READ_LINE, regs, Entry::SupervisorIrq, Carry::All)
    }

    /// `ReadLineV`'s default: a line into R0's buffer of at most R1
    /// characters, those between R2 and R3 stored; R1 its length, C set if
    /// escape ended it.
    pub(crate) fn default_read_line(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (buffer, most, low, high, flags) =
            (regs.r[0], regs.r[1], regs.r[2], regs.r[3], regs.r[4]);
        let mut length = match self.world.continuation.take() {
            Some(Continuation::Line {
                buffer: same,
                length,
            }) if same == buffer => length,
            _ => {
                self.set_byte_var(crate::byte::var::PAGE_LINES, 0);
                0
            }
        };
        // Characters come through `OS_ReadC`; with `RdchV` unclaimed that is
        // the input itself, read here so a wait keeps the line.
        let through_rdchv = self.vector_claimed(vectors::RDCH);
        loop {
            self.yield_point()?;
            let c = if through_rdchv {
                let mut read = Regs::default();
                self.perform(crate::swi::X | crate::swi::os::READ_C, &mut read)?;
                if read.flag(psr::C) {
                    regs.r[1] = length;
                    regs.set_flag(psr::C, true);
                    return Ok(());
                }
                read.byte(0)
            } else {
                match self.next_char()? {
                    Some(c) => c,
                    None if self.escape_set() => {
                        regs.r[1] = length;
                        regs.set_flag(psr::C, true);
                        return Ok(());
                    }
                    None => {
                        self.wait_for_input(None, Some(Continuation::Line { buffer, length }))?;
                        continue;
                    }
                }
            };
            let queued = self.world.vdu.is_some_and(|vdu| (vdu.queued)(self));
            if queued {
                self.write_c(c)?;
                continue;
            }
            match c {
                127 | 8 => {
                    if length > 0 {
                        length -= 1;
                        self.write_c(127)?;
                    }
                }
                21 => {
                    while length > 0 {
                        length -= 1;
                        self.write_c(127)?;
                    }
                }
                b'\r' | b'\n' => {
                    self.write_byte(buffer.wrapping_add(length), b'\r')?;
                    self.write_text(b"\n\r")?;
                    regs.r[1] = length;
                    regs.set_flag(psr::C, false);
                    return Ok(());
                }
                _ if length >= most => self.write_c(7)?,
                _ if (low..=high).contains(&u32::from(c)) => {
                    self.write_byte(buffer.wrapping_add(length), c)?;
                    length += 1;
                    let echo = if flags & (1 << 30) != 0 {
                        flags.to_le_bytes()[0]
                    } else {
                        c
                    };
                    self.write_c(echo)?;
                }
                _ => {
                    if flags & (1 << 31) == 0 {
                        let echo = if flags & (1 << 30) != 0 {
                            flags.to_le_bytes()[0]
                        } else {
                            c
                        };
                        self.write_c(echo)?;
                    }
                }
            }
        }
    }
}

/// `c` in lower case, Latin-1's letters included.
const fn latin1_lower(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' | 0xC0..=0xD6 | 0xD8..=0xDE => c + 0x20,
        _ => c,
    }
}

#[cfg(test)]
mod tests;
