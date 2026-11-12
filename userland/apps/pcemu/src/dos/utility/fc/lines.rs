//! `FC`'s line compare, as DOS 4's `FC` makes it. Each file's lines are read
//! into a buffer of `/LB` lines, tabs expanded, CRs dropped, a line cut at
//! 127 characters, and with `/W` blank lines left out. The buffers are
//! compared to the first difference; the line before it is kept, and the
//! search then looks for the first place `/nnnn` lines match again, each
//! file's lines up to it shown with the matching line under them.

use super::{at, file_at, have_read, name, read_into, switch, text, DIFFERENT, MOST_LINES};
use crate::dos::message::decimal_in;
use crate::dos::program::{say, Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

/// A buffered line: its number, its length, then its text.
const NUMBER: u16 = 0;
const LEN: u16 = 2;
const TEXT: u16 = 3;
const LONGEST_LEN: u16 = 127;
const LONGEST: usize = LONGEST_LEN as usize;
const ENTRY: u16 = TEXT + LONGEST_LEN;

/// What the line compare keeps at [`at::WORK`]: each file's block of
/// lines, its first, how many, the last one's number, and the line being
/// read; which file the fill has reached and what follows it; then the
/// difference being shown.
mod work {
    pub const SEGMENT: u16 = 0x00;
    pub const FIRST: u16 = 0x04;
    pub const COUNT: u16 = 0x08;
    pub const NUMBER: u16 = 0x0C;
    pub const PARTIAL: u16 = 0x10;
    pub const FILLING: u16 = 0x14;
    pub const STAGE: u16 = 0x15;
    pub const PART: u16 = 0x16;
    pub const AFTER: u16 = 0x17;
    pub const INDEX: u16 = 0x18;
    pub const SHOWN: u16 = 0x1A;
    pub const ADJUST: u16 = 0x1E;
    pub const LEN: u16 = 0x22;
}

const _: () = assert!(work::LEN <= at::WORK_LEN);
const _: () = assert!(MOST_LINES as u32 * ENTRY as u32 <= 0x1_0000);

/// What a fill leads to: the scan for the next difference, or the search
/// for where the files match again.
const SCAN: u8 = 0;
const RESYNC: u8 = 1;
/// A difference shown in parts: the resynchronisation's failure when the
/// buffers were full, each file's name and lines, then the closing line.
mod part {
    pub const FAILURE: u8 = 0;
    pub const FIRST_NAME: u8 = 1;
    pub const FIRST_LINES: u8 = 2;
    pub const SECOND_NAME: u8 = 3;
    pub const SECOND_LINES: u8 = 4;
    pub const CLOSING: u8 = 5;
}
/// What follows a difference shown: the lines before the match dropped, or
/// the end.
const ADJUST: u8 = 0;
const FAIL: u8 = 1;

fn work(field: u16, file: u16) -> u16 {
    at::WORK + field + file * 2
}

/// Whether `byte` is white space as C's `isspace` takes it.
const fn is_space(byte: u8) -> bool {
    matches!(byte, 0x09..=0x0D | b' ')
}

/// A line's text as C sees it: as far as a NUL.
fn visible(text: &[u8]) -> &[u8] {
    &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())]
}

/// How lines are compared: through the country's upper case with `/C`,
/// white space passed over with `/W`.
pub struct Compare {
    upper: Option<[u8; at::UPPER_LEN as usize]>,
    spaces: bool,
}

impl Compare {
    /// The compare `switches` ask for, `upper` the country's upper case
    /// for 80h–FFh.
    #[must_use]
    pub fn new(switches: u16, upper: [u8; at::UPPER_LEN as usize]) -> Self {
        Self {
            upper: (switches & switch::IGNORE_CASE != 0).then_some(upper),
            spaces: switches & switch::IGNORE_SPACE != 0,
        }
    }

    fn fold(&self, byte: u8) -> u8 {
        match (&self.upper, byte) {
            (None, _) => byte,
            (Some(_), 0..=0x7F) => byte.to_ascii_uppercase(),
            (Some(table), _) => table[usize::from(byte - 0x80)],
        }
    }

    /// Whether `a` and `b` are the same line.
    #[must_use]
    pub fn same(&self, a: &[u8], b: &[u8]) -> bool {
        let kept = |byte: &&u8| !(self.spaces && is_space(**byte));
        let a = visible(a).iter().filter(kept).map(|&b| self.fold(b));
        let b = visible(b).iter().filter(kept).map(|&b| self.fold(b));
        a.eq(b)
    }
}

/// Where the buffers line up again, as DOS 4's `FC` searches: the place in
/// one file each line of a growing stretch of the other is tried against,
/// a match being as many lines as `sync` asks or both buffers still hold.
/// The lines of each before it, or none when nothing matches.
#[must_use]
pub fn resync(
    one: &[Vec<u8>],
    two: &[Vec<u8>],
    sync: u16,
    compare: &Compare,
) -> Option<(usize, usize)> {
    let (l1, l2, sync) = (one.len(), two.len(), usize::from(sync));
    let matches = |s1: usize, s2: usize| {
        let ct = l1.saturating_sub(s1).min(l2.saturating_sub(s2)).min(sync);
        ct > 0 && (0..ct).all(|k| compare.same(&one[s1 + k], &two[s2 + k]))
    };
    let (mut xc, mut yc, mut xp, mut yp) = (1, 1, 1, 1);
    let (mut x_done, mut y_done) = (false, false);
    loop {
        if matches(xc, yp) {
            return Some((xc, yp));
        }
        if matches(xp, yc) {
            return Some((xp, yc));
        }
        xp += 1;
        if xp > xc {
            xp = 1;
            xc += 1;
            if xc >= l1 {
                xc = l1;
                x_done = true;
            }
        }
        yp += 1;
        if yp > yc {
            yp = 1;
            yc += 1;
            if yc >= l2 {
                yc = l2;
                y_done = true;
            }
        }
        if x_done && y_done {
            return None;
        }
    }
}

/// The lines both buffers begin with that match, but for the last of them:
/// it is kept to show above the first difference.
#[must_use]
pub fn matching(one: &[Vec<u8>], two: &[Vec<u8>], compare: &Compare) -> usize {
    let both = one.len().min(two.len());
    match (0..both).position(|n| !compare.same(&one[n], &two[n])) {
        Some(first) => first.saturating_sub(1),
        None => both,
    }
}

/// Buffered line `index` of file `file`: its block and offset.
fn slot(p: &mut Program<'_, '_>, file: u16, index: u16) -> (u16, u16) {
    let lines = u32::from(p.get16(at::LINES).max(1));
    let first = u32::from(p.get16(work(work::FIRST, file)));
    let slot = (first + u32::from(index)) % lines;
    (
        p.get16(work(work::SEGMENT, file)),
        u16::try_from(slot).unwrap_or(0) * ENTRY,
    )
}

/// Buffered line `index` of file `file`: its number and text.
fn line(p: &mut Program<'_, '_>, file: u16, index: u16) -> (u16, Vec<u8>) {
    let (segment, entry) = slot(p, file, index);
    let len = usize::from(p.g.read8(segment, entry + LEN)).min(LONGEST);
    let mut text = alloc::vec![0u8; len];
    p.g.read_bytes(segment, entry + TEXT, &mut text);
    (p.g.read16(segment, entry + NUMBER), text)
}

/// Every line file `file`'s buffer holds.
fn buffered(p: &mut Program<'_, '_>, file: u16) -> Vec<Vec<u8>> {
    let count = p.get16(work(work::COUNT, file));
    (0..count).map(|index| line(p, file, index).1).collect()
}

/// File `file`'s first `n` buffered lines dropped.
fn drop_lines(p: &mut Program<'_, '_>, file: u16, n: u16) {
    let lines = p.get16(at::LINES).max(1);
    let (first, count) = (
        p.get16(work(work::FIRST, file)),
        p.get16(work(work::COUNT, file)),
    );
    let n = n.min(count);
    let first = u16::try_from((u32::from(first) + u32::from(n)) % u32::from(lines)).unwrap_or(0);
    p.set16(work(work::FIRST, file), first);
    p.set16(work(work::COUNT, file), count - n);
}

/// The line being read in file `file` ended as `text`: buffered with its
/// number, unless `/W` leaves a blank one out.
fn complete(p: &mut Program<'_, '_>, file: u16, text: &[u8]) {
    p.set16(work(work::PARTIAL, file), 0);
    let blank = visible(text).iter().all(|&b| is_space(b));
    if p.get16(at::SWITCHES) & switch::IGNORE_SPACE != 0 && blank {
        return;
    }
    let number = p.get16(work(work::NUMBER, file)).wrapping_add(1);
    p.set16(work(work::NUMBER, file), number);
    let count = p.get16(work(work::COUNT, file));
    let (segment, entry) = slot(p, file, count);
    p.g.write16(segment, entry + NUMBER, number);
    p.g.write8(segment, entry + LEN, u8::try_from(text.len()).unwrap_or(0));
    p.g.write_bytes(segment, entry + TEXT, text);
    p.set16(work(work::COUNT, file), count + 1);
}

/// File `file`'s buffer filled from what its reading holds: the call
/// reading on when it needs more, none once the buffer is full or the file
/// has ended.
fn fill(p: &mut Program<'_, '_>, file: u16) -> Option<Frame> {
    let lines = p.get16(at::LINES);
    let tabs = p.get16(at::SWITCHES) & switch::KEEP_TABS == 0;
    let base = file_at(file);
    loop {
        let count = p.get16(work(work::COUNT, file));
        if count >= lines {
            return None;
        }
        let (start, end, done) = (
            p.get16(base + at::AT),
            p.get16(base + at::END),
            p.get8(base + at::DONE) != 0,
        );
        let mut partial = usize::from(p.get16(work(work::PARTIAL, file))).min(LONGEST);
        let (segment, entry) = slot(p, file, count);
        let mut text = [0u8; LONGEST];
        p.g.read_bytes(segment, entry + TEXT, &mut text[..partial]);
        if start >= end {
            if !done {
                return Some(read_into(p, file));
            }
            if partial == 0 {
                return None;
            }
            complete(p, file, &text[..partial]);
            continue;
        }
        let mut bytes = alloc::vec![0u8; usize::from(end - start)];
        p.g.read_bytes(p.own, base + at::BUFFER + start, &mut bytes);
        let mut taken = 0u16;
        let mut ended = false;
        for &byte in &bytes {
            taken += 1;
            match byte {
                b'\n' => ended = true,
                b'\r' => {}
                b'\t' if tabs => {
                    let spaces = (8 - partial % 8).min(LONGEST - partial);
                    text[partial..partial + spaces].fill(b' ');
                    partial += spaces;
                }
                _ => {
                    text[partial] = byte;
                    partial += 1;
                }
            }
            ended |= partial == LONGEST;
            if ended {
                break;
            }
        }
        p.set16(base + at::AT, start + taken);
        if ended {
            complete(p, file, &text[..partial]);
        } else {
            p.g.write_bytes(segment, entry + TEXT, &text[..partial]);
            p.set16(
                work(work::PARTIAL, file),
                u16::try_from(partial).unwrap_or(0),
            );
        }
    }
}

/// Memory for `lines` buffered lines, in paragraphs.
fn paragraphs(lines: u16) -> u16 {
    u16::try_from((u32::from(lines) * u32::from(ENTRY)).div_ceil(16)).unwrap_or(u16::MAX)
}

locals! {
    /// The line compare: each file's block of lines taken, then the fill,
    /// the scan, the search and the differences shown in turn.
    pub struct FcLines {
        code: u8,
    }
}

mod phase {
    pub const FIRST_BLOCK: u8 = 1;
    pub const SECOND_BLOCK: u8 = 2;
    pub const READ: u8 = 3;
    pub const SAID: u8 = 4;
    pub const ENDED: u8 = 5;
}

impl FcLines {
    fn compare(p: &mut Program<'_, '_>) -> Compare {
        let mut upper = [0u8; at::UPPER_LEN as usize];
        p.g.read_bytes(p.own, at::UPPER, &mut upper);
        Compare::new(p.get16(at::SWITCHES), upper)
    }

    fn end(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, code: u32, said: &[u8]) -> Flow {
        self.code = u8::try_from(code).unwrap_or(1);
        *phase = phase::ENDED;
        Flow::Call(say(p, said))
    }

    /// Both buffers filled, file by file, then what follows the fill.
    fn fill_on(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        for file in u16::from(p.get8(at::WORK + work::FILLING))..2 {
            p.set8(at::WORK + work::FILLING, u8::try_from(file).unwrap_or(0));
            if let Some(read) = fill(p, file) {
                *phase = phase::READ;
                return Flow::Call(read);
            }
        }
        p.set8(at::WORK + work::FILLING, 0);
        if p.get8(at::WORK + work::STAGE) == RESYNC {
            return self.search(p, phase);
        }
        self.scan(p, phase)
    }

    /// The matching lines both buffers begin with dropped, but the last.
    fn scan(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (one, two) = (buffered(p, 0), buffered(p, 1));
        if one.is_empty() && two.is_empty() {
            if p.get8(at::SAME) == 0 {
                return Flow::Return(Ok(DIFFERENT));
            }
            return self.end(p, phase, 0, text::SAME);
        }
        let compare = Self::compare(p);
        let matched = u16::try_from(matching(&one, &two, &compare)).unwrap_or(0);
        drop_lines(p, 0, matched);
        drop_lines(p, 1, matched);
        let both_empty = p.get16(work(work::COUNT, 0)) == 0 && p.get16(work(work::COUNT, 1)) == 0;
        p.set8(
            at::WORK + work::STAGE,
            if both_empty { SCAN } else { RESYNC },
        );
        self.fill_on(p, phase)
    }

    /// Where the files match again found, and the lines up to it shown;
    /// with no match, every line buffered shown and the compare ended.
    fn search(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let (one, two) = (buffered(p, 0), buffered(p, 1));
        let compare = Self::compare(p);
        let sync = p.get16(at::SYNC);
        let to_u16 = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        p.set8(at::SAME, 0);
        let (shown, after, first) = if let Some((a, b)) = resync(&one, &two, sync, &compare) {
            p.set16(work(work::ADJUST, 0), to_u16(a));
            p.set16(work(work::ADJUST, 1), to_u16(b));
            ((to_u16(a + 1), to_u16(b + 1)), ADJUST, part::FIRST_NAME)
        } else {
            let lines = usize::from(p.get16(at::LINES));
            let full = one.len() >= lines || two.len() >= lines;
            (
                (to_u16(one.len()), to_u16(two.len())),
                FAIL,
                if full {
                    part::FAILURE
                } else {
                    part::FIRST_NAME
                },
            )
        };
        p.set16(work(work::SHOWN, 0), shown.0);
        p.set16(work(work::SHOWN, 1), shown.1);
        p.set8(at::WORK + work::AFTER, after);
        p.set8(at::WORK + work::PART, first);
        self.show(p, phase)
    }

    /// The next line of file `file`'s part of a difference: every line
    /// shown, or with `/A` and more than three the first, `...` and the
    /// last.
    fn next_line(p: &mut Program<'_, '_>, file: u16) -> Option<Vec<u8>> {
        let (shown, index) = (
            p.get16(work(work::SHOWN, file)),
            p.get16(at::WORK + work::INDEX),
        );
        let switches = p.get16(at::SWITCHES);
        let abbreviated = switches & switch::ABBREVIATE != 0 && shown > 3;
        let line_index = match (abbreviated, index) {
            (false, n) if n < shown => n,
            (true, 0) => 0,
            (true, 1) => {
                p.set16(at::WORK + work::INDEX, 2);
                return Some(b"...\r\n".to_vec());
            }
            (true, 2) => shown - 1,
            _ => return None,
        };
        p.set16(at::WORK + work::INDEX, index + 1);
        let (number, text) = line(p, file, line_index);
        let mut out = Vec::with_capacity(text.len() + 9);
        if switches & switch::NUMBERS != 0 {
            out.extend(decimal_in(u32::from(number), 5));
            out.extend_from_slice(b":  ");
        }
        out.extend_from_slice(visible(&text));
        out.extend_from_slice(b"\r\n");
        Some(out)
    }

    /// The difference shown a part at a time; then what follows it.
    fn show(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        loop {
            let part = p.get8(at::WORK + work::PART);
            let said = match part {
                part::FAILURE => text::RESYNC.to_vec(),
                part::FIRST_NAME | part::SECOND_NAME => {
                    p.set16(at::WORK + work::INDEX, 0);
                    let file = u16::from(part == part::SECOND_NAME);
                    [&b"***** "[..], &name(p, file), b"\r\n"].concat()
                }
                part::FIRST_LINES | part::SECOND_LINES => {
                    if let Some(line) = Self::next_line(p, u16::from(part == part::SECOND_LINES)) {
                        *phase = phase::SAID;
                        return Flow::Call(say(p, &line));
                    }
                    p.set8(at::WORK + work::PART, part + 1);
                    continue;
                }
                part::CLOSING => b"*****\r\n\r\n".to_vec(),
                _ => {
                    if p.get8(at::WORK + work::AFTER) == FAIL {
                        return Flow::Return(Ok(DIFFERENT));
                    }
                    let (a, b) = (
                        p.get16(work(work::ADJUST, 0)),
                        p.get16(work(work::ADJUST, 1)),
                    );
                    drop_lines(p, 0, a);
                    drop_lines(p, 1, b);
                    p.set8(at::WORK + work::STAGE, SCAN);
                    return self.fill_on(p, phase);
                }
            };
            p.set8(at::WORK + work::PART, part + 1);
            *phase = phase::SAID;
            return Flow::Call(say(p, &said));
        }
    }
}

impl Routine for FcLines {
    const ID: Id = Id::FcLines;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            0 => {
                let lines = p.get16(at::LINES);
                if lines > MOST_LINES {
                    return self.end(p, phase, DIFFERENT, text::NO_MEMORY);
                }
                *phase = phase::FIRST_BLOCK;
                Flow::Call(Frame::of(&Int {
                    bx: paragraphs(lines),
                    ..Int::dos(p.own, 0x4800)
                }))
            }
            phase::FIRST_BLOCK | phase::SECOND_BLOCK => {
                let Ok(segment) = child else {
                    return self.end(p, phase, DIFFERENT, text::NO_MEMORY);
                };
                let file = u16::from(*phase == phase::SECOND_BLOCK);
                p.set16(
                    work(work::SEGMENT, file),
                    u16::try_from(segment).unwrap_or(0),
                );
                if file == 0 {
                    *phase = phase::SECOND_BLOCK;
                    let lines = p.get16(at::LINES);
                    return Flow::Call(Frame::of(&Int {
                        bx: paragraphs(lines),
                        ..Int::dos(p.own, 0x4800)
                    }));
                }
                for field in [work::FIRST, work::COUNT, work::NUMBER, work::PARTIAL] {
                    p.set16(work(field, 0), 0);
                    p.set16(work(field, 1), 0);
                }
                p.set8(at::WORK + work::FILLING, 0);
                p.set8(at::WORK + work::STAGE, SCAN);
                self.fill_on(p, phase)
            }
            phase::READ => {
                let read = take!(child);
                let file = u16::from(p.get8(at::WORK + work::FILLING));
                have_read(p, file, read);
                self.fill_on(p, phase)
            }
            phase::SAID => {
                take!(child);
                self.show(p, phase)
            }
            _ => {
                take!(child);
                Flow::Return(Ok(u32::from(self.code)))
            }
        }
    }
}
