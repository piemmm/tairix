//! The streams held to the states RISC OS 5.30's library leaves them in, as
//! the probes recorded them: each `FILE` block and its extra block after
//! each call, their addresses taken as this library lays them.

extern crate std;

use std::format;
use std::vec::Vec;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, disc_file, DISC};
use tairix_riscos_kernel::module::Code;
use tairix_riscos_kernel::os::Regs;
use tairix_riscos_kernel::swi::{os as kos, X};

use super::open::{fclose, fopen, freopen, remove, rename, tmpfile, tmpnam};
use super::printf::{printf, vfprintf, vsprintf};
use super::scan::scanf;
use super::stream::{file_at, word, EXTRA_BYTES};
use super::*;
use crate::c::errno::ERANGE;
use crate::c::statics::L_TMPNAM;
use crate::c::string::strerror;
use crate::c::testing::{Client, FREE, STATICS};

const NAME: u32 = FREE;
const OTHER_NAME: u32 = FREE + 0x40;
const MODE: u32 = FREE + 0x80;
const TEXT: u32 = FREE + 0x100;
const BUFFER: u32 = FREE + 0x200;
const USER_BUFFER: u32 = FREE + 0x300;
const WORDS: u32 = FREE + 0x380;
const SPOOL: &str = "Out";

mod blocks;
mod positioning;
mod redirection;

fn statics() -> Statics {
    Statics::of_base(STATICS)
}

/// `text` terminated at `at`: `at`.
fn put_text(client: &mut Client, at: u32, text: &[u8]) -> u32 {
    client.put(at, &[text, b"\0"].concat());
    at
}

/// The file `name` on the disc opened with `mode`: the stream.
fn opened(client: &mut Client, name: &str, mode: &[u8]) -> u32 {
    let name = put_text(client, NAME, format!("HostFS:$.{name}").as_bytes());
    let mode = put_text(client, MODE, mode);
    client.answer(fopen, &[name, mode])
}

/// A stream's `FILE` block and its extra block, as words.
fn blocks(client: &mut Client, stream: u32) -> ([u32; 10], [u32; 9]) {
    let mut os = client.session.kernel.os();
    let mut file = [0; 10];
    os.read_words(stream, &mut file).expect("the FILE block");
    let [.., extra_at] = file;
    let mut extra = [0; 9];
    os.read_words(extra_at, &mut extra)
        .expect("the extra block");
    (file, extra)
}

/// What a stream's `FILE` block holds past its pointer, counts and flags:
/// its buffer, its file's handle, the buffer's size and its extra block.
#[derive(Clone, Copy)]
struct Layout {
    buffer: u32,
    handle: u32,
    size: u32,
    extra: u32,
}

impl Layout {
    /// The block with the pointer, the counts in and out, and the flags given.
    fn at(self, ptr: u32, icnt: u32, ocnt: u32, flag: u32) -> [u32; 10] {
        let Self {
            buffer,
            handle,
            size,
            extra,
        } = self;
        [ptr, icnt, ocnt, flag, buffer, handle, 0, size, 0, extra]
    }
}

/// Stream `n`'s extra block, as the streams were laid.
fn extra_of(client: &mut Client, n: u32) -> u32 {
    Stream::load(&mut client.session.kernel.os(), file_at(statics(), n))
        .expect("the stream")
        .extra
}

fn errno(client: &mut Client) -> u32 {
    client.word(STATICS + crate::c::statics::at::ERRNO)
}

fn set_errno(client: &mut Client, value: u32) {
    client.put(STATICS + crate::c::statics::at::ERRNO, &value.to_le_bytes());
}

/// `entry` called with `arguments`, what it writes to the console caught on
/// the disc by `*Spool`: its answer and that text.
fn spooled(client: &mut Client, tree: &Tree, entry: Code, arguments: &[u32]) -> (u32, Vec<u8>) {
    command(
        &mut client.session.kernel.os(),
        &format!("Spool HostFS:$.{SPOOL}"),
    )
    .expect("spooling");
    let answer = client.answer(entry, arguments);
    command(&mut client.session.kernel.os(), "Spool").expect("spooling ended");
    (answer, disc_file(tree, SPOOL).unwrap_or_default())
}

#[test]
fn the_standard_streams_start_unset_up_with_their_extra_blocks_in_a_row() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let first = extra_of(&mut client, 0);
    for (n, flag) in [(0, 1), (1, 2), (2, 2)] {
        let extra = first + n * EXTRA_BYTES;
        assert_eq!(
            blocks(&mut client, file_at(statics(), n)),
            ([0, 0, 0, flag, 0, 0, 0, 0x1000, 0, extra], [0; 9]),
            "stream {n}"
        );
    }
}

#[test]
fn a_file_written_moves_through_genuine_s_states() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P54w", b"w");
    assert_eq!(stream, file_at(statics(), 3), "the first stream not open");
    let (file, words) = blocks(&mut client, stream);
    let handle = file[5];
    assert_ne!(handle, 0);
    let unset = Layout {
        buffer: 0,
        handle,
        size: 0x1000,
        extra,
    };
    assert_eq!((file, words), (unset.at(0, 0, 0, 2), [0; 9]), "opened");

    let a = u32::from(b'A');
    assert_eq!(client.answer(fputc, &[a, stream]), a);
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    let set = Layout {
        buffer: base,
        ..unset
    };
    assert_eq!(
        file,
        set.at(base + 1, 0, 0xFFF, 0x0041_2902),
        "the first character"
    );
    assert_eq!(words, [0, 0, 0, 0, 0, base + 1, 0x1000, 0, 0]);
    let text = put_text(&mut client, TEXT, b"BCD\n");
    assert_eq!(client.answer(fputs, &[text, stream]), 0);
    let blocks_now = blocks(&mut client, stream);
    let putc_end = set.at(base + 5, 0, 0xFFB, 0x0041_2902);
    assert_eq!(
        blocks_now.0, putc_end,
        "the putc path leaves the data's end"
    );
    assert_eq!(blocks_now.1, words);
    let text = put_text(&mut client, TEXT, b"xyz");
    assert_eq!(client.answer(fwrite, &[text, 1, 3, stream]), 3);
    assert_eq!(
        blocks(&mut client, stream).0,
        set.at(base + 8, 0, 0xFF8, 0x0041_2902)
    );
    assert_eq!(client.answer(ftell, &[stream]), 8);
    assert_eq!(client.answer(fflush, &[stream]), 0);
    let flushed = set.at(base, 0, 0xFFFF_F000, 0x0044_0902);
    let flushed_words = [0, 8, 0, 0, 0, base, 0x1000, 0, 0];
    assert_eq!(
        blocks(&mut client, stream),
        (flushed, flushed_words),
        "flushed"
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
    let closed = [0, 0, 0, 0, 0, 0, 0, 0, 0, extra];
    assert_eq!(blocks(&mut client, stream), (closed, [0; 9]), "closed");
    assert_eq!(disc_file(&tree, "P54w").as_deref(), Some(&b"ABCD\nxyz"[..]));
}

#[test]
fn a_file_read_back_moves_through_genuine_s_states() {
    let tree = holding(b"ABCD\nxyz");
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P54w", b"r");
    assert_eq!(stream, file_at(statics(), 3), "the first stream not open");
    let handle = blocks(&mut client, stream).0[5];
    let unset = Layout {
        buffer: 0,
        handle,
        size: 0x1000,
        extra,
    };
    assert_eq!(blocks(&mut client, stream), (unset.at(0, 0, 0, 1), [0; 9]));
    assert_eq!(client.answer(fgetc, &[stream]), u32::from(b'A'));
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    let set = Layout {
        buffer: base,
        ..unset
    };
    assert_eq!(
        file,
        set.at(base + 1, 7, 0, 0x0042_1901),
        "the buffer filled"
    );
    assert_eq!(words, [0, 0, 0, 0, 0, base + 8, 0x1000, 0, 0]);
    let q = u32::from(b'Q');
    assert_eq!(client.answer(ungetc, &[q, stream]), q);
    let pushed = [0x5100, 0, 0, 0, 0, base + 8, 0x1000, 7, 0];
    let pushed_back = set.at(base + 1, 0, 0, 0x004A_1901);
    assert_eq!(
        blocks(&mut client, stream),
        (pushed_back, pushed),
        "pushed back"
    );
    assert_eq!(client.answer(fgetc, &[stream]), q);
    assert_eq!(client.answer(fseek, &[stream, 2, 0]), 0);
    let moved = set.at(base + 2, (-6i32).cast_unsigned(), 0, 0x0042_0901);
    assert_eq!(
        blocks(&mut client, stream),
        (moved, pushed),
        "moved within the buffer"
    );
    assert_eq!(client.answer(ftell, &[stream]), 2);
    assert_eq!(client.answer(fgets, &[BUFFER, 10, stream]), BUFFER);
    assert_eq!(client.string(BUFFER), b"CD\n");
    assert_eq!(
        blocks(&mut client, stream).0,
        set.at(base + 5, 3, 0, 0x0042_1901)
    );
    for &c in b"xyz" {
        assert_eq!(client.answer(fgetc, &[stream]), u32::from(c));
    }
    assert_eq!(client.answer(fgetc, &[stream]), EOF);
    let at_end = set.at(base, 0, 0, 0x0042_0941);
    let end_words = [0x5100, 8, 0, 0, 0, base, 0x1000, 7, 0];
    assert_eq!(
        blocks(&mut client, stream),
        (at_end, end_words),
        "at the end"
    );
    assert_eq!(
        (
            client.answer(feof, &[stream]),
            client.answer(ferror, &[stream])
        ),
        (0x40, 0)
    );
    assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    assert_eq!(blocks(&mut client, stream).0[3], 0x0042_0901);
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

/// A disc holding `P54w` as `content`.
fn holding(content: &[u8]) -> Tree {
    Tree::new().dir(DISC).file(&format!("{DISC}/P54w"), content)
}

#[test]
fn an_append_stream_keeps_the_extent_and_a_read_stream_refuses_writes() {
    let tree = holding(b"ABCD\nxyz");
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P54w", b"a");
    let handle = blocks(&mut client, stream).0[5];
    let unset = Layout {
        buffer: 0,
        handle,
        size: 0x1000,
        extra,
    };
    let extent_kept = [0, 0, 0, 8, 0, 0, 0, 0, 0];
    assert_eq!(
        blocks(&mut client, stream),
        (unset.at(0, 0, 0, 0x8022), extent_kept),
        "its extent kept"
    );
    assert_eq!(
        client.answer(fputc, &[u32::from(b'Z'), stream]),
        u32::from(b'Z')
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);

    let stream = opened(&mut client, "P54w", b"rb");
    let handle = blocks(&mut client, stream).0[5];
    assert_eq!(client.answer(fread, &[BUFFER, 1, 20, stream]), 9);
    assert_eq!(client.bytes(BUFFER, 9), b"ABCD\nxyzZ");
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    let set = Layout {
        buffer: base,
        handle,
        size: 0x1000,
        extra,
    };
    let read_words = [0, 9, 0, 0, 0, base, 0x1000, 0, 0];
    assert_eq!((file, words), (set.at(base, 0, 0, 0x0042_0945), read_words));
    set_errno(&mut client, 0);
    let b = u32::from(b'B');
    assert_eq!(
        client.answer(fputc, &[b, stream]),
        EOF,
        "a stream open only to read"
    );
    assert_eq!(
        blocks(&mut client, stream),
        (set.at(base, 0, 0, 0x0042_09C5), words)
    );
    assert_eq!(errno(&mut client), 0);
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn an_open_that_fails_or_names_no_mode_answers_nought() {
    let tree = holding(b"ABCD\nxyz");
    let mut client = Client::with_streams(&tree, &[]);
    assert_eq!(opened(&mut client, "P54nothere", b"r"), 0);
    assert_eq!(errno(&mut client), u32::MAX, "a failed open's errno");
    set_errno(&mut client, 0);
    assert_eq!(opened(&mut client, "P54w", b"q"), 0);
    assert_eq!(errno(&mut client), 0, "no mode, errno untouched");
}

#[test]
fn a_mode_longer_than_its_letters_opens_by_them_the_rest_passed_over() {
    let tree = holding(b"ABCD\nxyz");
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "P54w", b"w+bx,ccs=UTF-8");
    assert_ne!(stream, 0);
    let flags = client.word(stream + word::FLAG);
    assert_eq!(
        flags & (flag::READ | flag::WRITE | flag::BINARY),
        flag::READ | flag::WRITE | flag::BINARY
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn counts_and_sizes_no_int_holds_are_answered_not_overflowed() {
    let tree = holding(b"ABCD\nxyz");
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "P54w", b"r");
    assert_eq!(
        client.answer(fgets, &[BUFFER, i32::MIN.cast_unsigned(), stream]),
        0,
        "fgets of no room"
    );
    for top in [0xFFFF_FFF4, 0xFFFF_FFF8, u32::MAX] {
        client.call(feof, &[top]);
        client.call(ferror, &[top]);
    }
    let written = opened(&mut client, "P54x", b"w");
    assert_eq!(
        client.answer(setvbuf, &[written, USER_BUFFER, flag::FULLY, 0x8000_0000]),
        0
    );
    assert_eq!(
        client.answer(fputc, &[u32::from(b'x'), written]),
        u32::from(b'x')
    );
    assert_eq!(client.answer(fflush, &[written]), 0);
    assert_eq!(client.answer(fclose, &[written]), 0);
    assert_eq!(disc_file(&tree, "P54x").as_deref(), Some(&b"x"[..]));
}

#[test]
fn an_unbuffered_stream_works_through_its_extra_block() {
    let tree = holding(b"ABCD\nxyz");
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P54w", b"r+");
    let handle = blocks(&mut client, stream).0[5];
    let unset = Layout {
        buffer: 0,
        handle,
        size: 0x1000,
        extra,
    };
    assert_eq!(blocks(&mut client, stream), (unset.at(0, 0, 0, 3), [0; 9]));
    assert_eq!(client.answer(setvbuf, &[stream, 0, flag::UNBUFFERED, 0]), 0);
    let one = Layout {
        buffer: extra,
        handle,
        size: 1,
        extra,
    };
    let its_buffer = one.at(extra, 0, 0, 0x403);
    assert_eq!(
        blocks(&mut client, stream),
        (its_buffer, [0; 9]),
        "the extra block its buffer"
    );
    let c = u32::from(b'C');
    assert_eq!(client.answer(fputc, &[c, stream]), c);
    let written = [0x43, 1, 0, 0, 0, extra, 1, 0, 0];
    assert_eq!(
        blocks(&mut client, stream),
        (one.at(extra, 0, 0, 0x0045_2403), written)
    );
    assert_eq!(client.answer(fseek, &[stream, 0, 0]), 0);
    assert_eq!(client.answer(fgetc, &[stream]), c);
    let read_back = (
        one.at(extra + 1, 0, 0, 0x0042_1403),
        [0x43, 0, 0, 0, 0, extra + 1, 1, 0, 0],
    );
    assert_eq!(
        blocks(&mut client, stream),
        read_back,
        "read back through the one-byte buffer"
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn a_line_buffered_stream_delivers_each_line_from_the_buffer_given() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P54w", b"w+");
    let handle = blocks(&mut client, stream).0[5];
    assert_eq!(
        client.answer(setvbuf, &[stream, USER_BUFFER, flag::LINE, 16]),
        0
    );
    let user = Layout {
        buffer: USER_BUFFER,
        handle,
        size: 16,
        extra,
    };
    assert_eq!(
        blocks(&mut client, stream),
        (user.at(USER_BUFFER, 0, 0, 0x203), [0; 9])
    );
    let text = put_text(&mut client, TEXT, b"ab\ncd");
    assert_eq!(client.answer(fputs, &[text, stream]), 0);
    let pending = user.at(USER_BUFFER + 2, 0, (-2i32).cast_unsigned(), 0x0045_2203);
    let delivered = [0, 3, 0, 0, 0, USER_BUFFER, 16, 0, 0];
    let line = "the line delivered, the rest pending";
    assert_eq!(blocks(&mut client, stream), (pending, delivered), "{line}");
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "P54w").as_deref(), Some(&b"ab\ncd"[..]));
}

#[test]
fn remove_answers_one_for_nothing_and_fclose_eof_for_no_stream() {
    let tree = holding(b"ab\ncd");
    let mut client = Client::with_streams(&tree, &[]);
    set_errno(&mut client, 0);
    let name = put_text(&mut client, NAME, b"HostFS:$.P54w");
    assert_eq!(client.answer(remove, &[name]), 0);
    assert_eq!(client.answer(remove, &[name]), 1, "nothing to remove");
    assert_eq!(errno(&mut client), 0);
    assert_eq!(client.answer(fclose, &[0]), EOF, "no stream");
}

#[test]
fn a_temporary_name_is_the_time_and_a_count_and_a_temporary_file_needs_its_directory() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let mut names = Vec::new();
    for (low, high) in [
        (0, 0),
        (0x1234_5678u32, 0x9A),
        (0xFFFF_FFFF, 0x55),
        (100, 0),
    ] {
        client.put(WORDS, &[&[5], &low.to_le_bytes()[..], &[high]].concat());
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[15, WORDS]);
        client
            .session
            .kernel
            .os()
            .perform(X | kos::WORD, &mut regs)
            .expect("the clock set");
        for into in [0, BUFFER] {
            let at = client.answer(tmpnam, &[into]);
            let at_expected = if into == 0 {
                STATICS + crate::c::statics::at::TMPNAM_TEXT
            } else {
                into
            };
            assert_eq!(at, at_expected);
            names.push(client.string(at));
        }
    }
    let expected: Vec<Vec<u8>> = [
        "ffffff00", "ffffff01", "ffffff02", "ffffff03", "7e774204", "7e774205", "ffffff06",
        "ffffff07",
    ]
    .iter()
    .map(|digits| format!("$.tmp.x{digits}").into_bytes())
    .collect();
    assert_eq!(names, expected);
    assert!(names.iter().all(|name| name.len() < L_TMPNAM as usize));
    assert_eq!(client.answer(tmpfile, &[]), 0, "no $.tmp");
}

#[test]
fn the_console_s_output_is_unbuffered_each_newline_a_line_feed_and_return() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let (stdout, stderr) = (file_at(statics(), 1), file_at(statics(), 2));
    let (out_extra, err_extra) = (extra_of(&mut client, 1), extra_of(&mut client, 2));
    let unbuffered = |extra: u32, flag: u32| [extra, 0, 0, flag, extra, 0, 0, 1, 0, extra];
    let text = put_text(&mut client, TEXT, b"hi");
    assert_eq!(
        spooled(&mut client, &tree, printf, &[text]),
        (2, b"hi".to_vec())
    );
    assert_eq!(
        blocks(&mut client, stdout).0,
        unbuffered(out_extra, 0x0045_2402)
    );
    assert_eq!(
        spooled(&mut client, &tree, putchar, &[10]),
        (10, b"\n\r".to_vec())
    );
    let text = put_text(&mut client, TEXT, b"err");
    assert_eq!(
        spooled(&mut client, &tree, fputs, &[text, stderr]),
        (0, b"err".to_vec())
    );
    assert_eq!(
        blocks(&mut client, stderr).0,
        unbuffered(err_extra, 0x0045_2402)
    );
    let text = put_text(&mut client, TEXT, b"a\nb");
    assert_eq!(
        spooled(&mut client, &tree, printf, &[text]),
        (3, b"a\n\rb".to_vec())
    );
    assert_eq!(client.answer(fflush, &[stdout]), 0);
    assert_eq!(
        blocks(&mut client, stdout).0,
        unbuffered(out_extra, 0x0044_0402),
        "flushed"
    );
    let text = put_text(&mut client, TEXT, b"p");
    assert_eq!(
        spooled(&mut client, &tree, puts, &[text]),
        (10, b"p\n\r".to_vec())
    );
    assert_eq!(
        blocks(&mut client, stdout).0,
        unbuffered(out_extra, 0x0045_2402)
    );

    set_errno(&mut client, ERANGE);
    let reason = client.answer(strerror, &[ERANGE]);
    let reason = client.string(reason);
    let text = put_text(&mut client, TEXT, b"msg");
    let written = spooled(&mut client, &tree, perror, &[text]).1;
    assert_eq!(written, [&b"msg: "[..], &reason, b"\n\r"].concat());
    assert_eq!(
        client.answer(setvbuf, &[stdout, 0, flag::UNBUFFERED, 0]),
        1,
        "too late"
    );
    let text = put_text(&mut client, TEXT, b"x");
    assert_eq!(
        spooled(&mut client, &tree, printf, &[text]),
        (1, b"x".to_vec())
    );
}

#[test]
fn the_console_s_input_is_read_a_line_at_a_time_and_scanf_gives_back_what_it_stopped_at() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, b"AB\rline two\rx\r12 34\r");
    let stdin = file_at(statics(), 0);
    let extra = extra_of(&mut client, 0);
    assert_eq!(client.answer(getchar, &[]), u32::from(b'A'));
    let base = blocks(&mut client, stdin).0[4];
    assert_eq!(
        blocks(&mut client, stdin).0,
        [base + 1, 2, 0, 0x0042_1901, base, 0, 0, 0x1000, 0, extra],
        "a line and its newline"
    );
    assert_eq!(client.answer(getchar, &[]), u32::from(b'B'));
    assert_eq!(client.answer(getchar, &[]), u32::from(b'\n'));
    assert_eq!(
        blocks(&mut client, stdin).0,
        [base + 3, 0, 0, 0x0042_1901, base, 0, 0, 0x1000, 0, extra]
    );
    assert_eq!(client.answer(fgets, &[BUFFER, 20, stdin]), BUFFER);
    assert_eq!(client.string(BUFFER), b"line two\n");
    assert_eq!(
        blocks(&mut client, stdin).0,
        [base + 9, 0, 0, 0x0042_1901, base, 0, 0, 0x1000, 0, extra],
        "the next line from the start"
    );
    assert_eq!(client.answer(gets, &[BUFFER]), BUFFER);
    assert_eq!(client.string(BUFFER), b"x");
    let format = put_text(&mut client, TEXT, b"%d");
    assert_eq!(client.answer(scanf, &[format, WORDS]), 1);
    assert_eq!(client.word(WORDS), 12);
    assert_eq!(client.answer(getchar, &[]), u32::from(b' '));
}

#[test]
fn positioning_an_update_stream_keeps_its_buffer_and_pushback_is_one_character() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P56u", b"w+");
    let handle = blocks(&mut client, stream).0[5];
    let text = put_text(&mut client, TEXT, b"hello");
    assert_eq!(client.answer(fputs, &[text, stream]), 0);
    assert_eq!(
        client.answer(fgetc, &[stream]),
        EOF,
        "a read straight after a write"
    );
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    let set = Layout {
        buffer: base,
        handle,
        size: 0x1000,
        extra,
    };
    let written = [0, 0, 0, 0, 0, base + 1, 0x1000, 0, 0];
    assert_eq!(
        (file, words),
        (set.at(base + 5, 0, 0, 0x0041_2983), written)
    );
    assert_eq!(client.answer(fseek, &[stream, 0, 0]), 0);
    let kept = set.at(base, (-5i32).cast_unsigned(), 0xFFFF_F000, 0x0041_0983);
    let undelivered = [0, 0, 0, 0, 0, base + 5, 0x1000, 0, 0];
    let nothing = "the buffer kept, nothing delivered";
    assert_eq!(
        blocks(&mut client, stream),
        (kept, undelivered),
        "{nothing}"
    );
    assert_eq!(client.answer(fgetc, &[stream]), EOF, "a stream in error");
    let x = u32::from(b'X');
    assert_eq!(
        client.answer(fputc, &[x, stream]),
        EOF,
        "in error, its counts nought"
    );
    assert_eq!(
        blocks(&mut client, stream).0,
        set.at(base, 0, 0, 0x0041_0983)
    );
    let back_two = (-2i32).cast_unsigned();
    assert_eq!(client.answer(fseek, &[stream, back_two, 2]), 0);
    assert_eq!(client.answer(ftell, &[stream]), 3);
    let at_three = set.at(base + 3, back_two, 0xFFFF_F003, 0x0041_0983);
    assert_eq!(blocks(&mut client, stream).0, at_three);
    assert_eq!(
        client.answer(fputc, &[u32::from(b'Y'), stream]),
        u32::from(b'Y')
    );
    assert_eq!(client.answer(fseek, &[stream, 1, 1]), 0);
    assert_eq!(client.answer(ftell, &[stream]), 5);
    assert_eq!(client.answer(fgetpos, &[stream, WORDS]), 0);
    assert_eq!(client.word(WORDS), 5);
    assert_eq!(client.answer(rewind, &[stream]), stream + word::FLAG);
    assert_eq!(client.answer(ftell, &[stream]), 0);
    let cleared = set.at(base, (-5i32).cast_unsigned(), 0xFFFF_F000, 0x0041_0903);
    assert_eq!(
        blocks(&mut client, stream).0,
        cleared,
        "both indicators cleared"
    );
    assert_eq!(client.answer(fsetpos, &[stream, WORDS]), 0);
    assert_eq!(client.answer(ftell, &[stream]), 5);
    assert_eq!(
        client.answer(ungetc, &[u32::from(b'A'), stream]),
        u32::from(b'A')
    );
    assert_eq!(client.answer(ungetc, &[u32::from(b'B'), stream]), EOF);
    assert_eq!(client.answer(ungetc, &[EOF, stream]), EOF);
    assert_eq!(client.answer(fgetc, &[stream]), u32::from(b'A'));
    assert_eq!(client.answer(fgetc, &[stream]), EOF);
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "P56u").as_deref(), Some(&b"helYo"[..]));

    let (from, to) = (
        put_text(&mut client, NAME, b"HostFS:$.P56u"),
        put_text(&mut client, OTHER_NAME, b"HostFS:$.P56v"),
    );
    set_errno(&mut client, 0);
    assert_eq!(client.answer(rename, &[from, to]), 0);
    assert_eq!(client.answer(rename, &[from, to]), 1, "nothing to rename");
    assert_eq!(errno(&mut client), 0);
    let stream = opened(&mut client, "P56v", b"w");
    let handle = blocks(&mut client, stream).0[5];
    assert_eq!(client.answer(setbuf, &[stream, 0]), 0);
    let one = Layout {
        buffer: extra,
        handle,
        size: 1,
        extra,
    };
    assert_eq!(
        blocks(&mut client, stream),
        (one.at(extra, 0, 0, 0x402), [0; 9])
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn a_va_list_is_advanced_in_place_and_stdout_reopened_onto_a_file_is_fully_buffered() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let extra = extra_of(&mut client, 3);
    let stream = opened(&mut client, "P56v", b"w");
    let handle = blocks(&mut client, stream).0[5];
    let (list, arguments, zz) = (WORDS, WORDS + 4, WORDS + 12);
    client.put(zz, b"zz\0");
    client.put(arguments, &[42u32.to_le_bytes(), zz.to_le_bytes()].concat());
    client.put(list, &arguments.to_le_bytes());
    let format = put_text(&mut client, TEXT, b"%d-%s");
    assert_eq!(client.answer(vsprintf, &[BUFFER, format, list]), 5);
    assert_eq!(client.string(BUFFER), b"42-zz");
    assert_eq!(
        client.word(list),
        arguments + 8,
        "the list past the two words taken"
    );
    client.put(list, &arguments.to_le_bytes());
    assert_eq!(client.answer(vfprintf, &[stream, format, list]), 5);
    let empty = put_text(&mut client, OTHER_NAME, b"");
    assert_eq!(client.answer(printf, &[empty]), 0);
    assert_eq!(client.answer(fflush, &[0]), 0);
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    assert_eq!(
        (file, words),
        (
            [
                base,
                0,
                0xFFFF_F000,
                0x0044_0902,
                base,
                handle,
                0,
                0x1000,
                0,
                extra
            ],
            [0, 5, 0, 0, 0, base, 0x1000, 0, 0]
        ),
        "every stream flushed"
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(client.answer(fflush, &[0]), 0);
    assert_eq!(
        blocks(&mut client, stream),
        ([0, 0, 0, 0, 0, 0, 0, 0, 0, extra], [0; 9])
    );
    assert_eq!(client.answer(fclose, &[stream]), EOF, "closed already");
    assert_eq!(disc_file(&tree, "P56v").as_deref(), Some(&b"42-zz"[..]));

    let stdout = file_at(statics(), 1);
    let out_extra = extra_of(&mut client, 1);
    let name = put_text(&mut client, NAME, b"HostFS:$.P56u");
    let mode = put_text(&mut client, MODE, b"w");
    assert_eq!(client.answer(freopen, &[name, mode, stdout]), stdout);
    let handle = blocks(&mut client, stdout).0[5];
    assert_ne!(handle, 0);
    assert_eq!(
        blocks(&mut client, stdout),
        ([0, 0, 0, 2, 0, handle, 0, 0x1000, 0, out_extra], [0; 9])
    );
    let text = put_text(&mut client, TEXT, b"into file\n");
    assert_eq!(client.answer(printf, &[text]), 10);
    let (file, words) = blocks(&mut client, stdout);
    let base = file[4];
    assert_eq!(
        (file, words),
        (
            [
                base + 10,
                0,
                0xFF6,
                0x0041_2902,
                base,
                handle,
                0,
                0x1000,
                0,
                out_extra
            ],
            [0, 0, 0, 0, 0, base + 1, 0x1000, 0, 0]
        )
    );
    assert_eq!(client.answer(fflush, &[stdout]), 0);
    assert_eq!(client.answer(fclose, &[stdout]), 0);
    assert_eq!(
        disc_file(&tree, "P56u").as_deref(),
        Some(&b"into file\n"[..])
    );
}
