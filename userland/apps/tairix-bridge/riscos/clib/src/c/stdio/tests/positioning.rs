//! Positioning and transfers as the g15–g25 probes recorded RISC OS 5.30's:
//! a positioning only recorded, bytes moved at the file's own pointer, a
//! flush in error keeping its buffer, and the unchecked fast path into the
//! space a positioning leaves.

use super::*;
use crate::c::errno::{EDOM, EOVERFLOW, OS_REFUSED};

/// `OS_Args` `reason` on the stream's file: R2 after.
fn file_args(client: &mut Client, stream: u32, reason: u32, value: u32) -> u32 {
    let handle = blocks(client, stream).0[5];
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[reason, handle, value]);
    client
        .session
        .kernel
        .os()
        .perform(X | kos::ARGS, &mut regs)
        .expect("OS_Args");
    regs.r[2]
}

/// The stream's flags with `set` set by hand, as a program can set them.
fn mark(client: &mut Client, stream: u32, set: u32) {
    let flags = client.word(stream + word::FLAG) | set;
    client.put(stream + word::FLAG, &flags.to_le_bytes());
}

fn written(client: &mut Client, stream: u32, text: &[u8]) {
    let text = put_text(client, TEXT, text);
    assert_eq!(client.answer(fputs, &[text, stream]), 0);
}

#[test]
fn a_positioning_is_recorded_and_the_next_transfer_makes_it() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G17", b"w+b");
    written(&mut client, stream, b"ABCDE");
    assert_eq!(client.answer(fseek, &[stream, 100, 0]), 0);
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    assert_eq!(
        (file[0], file[1], file[2], file[3]),
        (base + 5, 0, 0, 0x0041_0927),
        "nothing delivered, the move pending"
    );
    assert_eq!(
        (words[1], words[3], words[5]),
        (0, 100, base + 5),
        "the position asked for kept"
    );
    assert_eq!(file_args(&mut client, stream, kos::args::READ_EXTENT, 0), 0);
    assert_eq!(client.answer(ftell, &[stream]), 100);
    written(&mut client, stream, b"XY");
    let (file, words) = blocks(&mut client, stream);
    assert_eq!(
        (file[0], file[3], words[1]),
        (base + 2, 0x0045_2917, 100),
        "made: the buffer delivered, then moved"
    );
    assert_eq!(file_args(&mut client, stream, kos::args::READ_EXTENT, 0), 5);
    assert_eq!(client.answer(fclose, &[stream]), 0);
    let expected = [&b"ABCDE"[..], &[0; 95], b"XY"].concat();
    assert_eq!(
        disc_file(&tree, "G17").as_deref(),
        Some(&expected[..]),
        "the file's pointer sent past its end"
    );
}

#[test]
fn a_stream_closed_in_error_after_a_far_positioning_loses_what_it_held() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G16", b"w+b");
    written(&mut client, stream, b"ABCDE");
    assert_eq!(client.answer(fseek, &[stream, 0x7FFF_FFFF, 0]), 0);
    set_errno(&mut client, 0);
    assert_eq!(client.answer(fseek, &[stream, 0x10, 1]), 2, "past LONG_MAX");
    assert_eq!(errno(&mut client), EOVERFLOW);
    assert_eq!(client.answer(ferror, &[stream]), flag::ERROR);
    assert_eq!(
        client.answer(fseek, &[stream, 0, 0]),
        0,
        "back within the buffer, still in error"
    );
    assert_eq!(client.answer(fclose, &[stream]), EOF);
    assert_eq!(disc_file(&tree, "G16").as_deref(), Some(&b""[..]));
}

#[test]
fn bytes_go_at_the_file_s_pointer_wherever_it_was_moved() {
    let tree = Tree::new().dir(DISC).file(
        &format!("{DISC}/G19r"),
        &[[b'A'; 1], [0; 1]].concat().repeat(2600),
    );
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G19", b"w+b");
    written(&mut client, stream, b"ABCDE");
    assert_eq!(client.answer(fflush, &[stream]), 0);
    file_args(&mut client, stream, kos::args::WRITE_POINTER, 1);
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        u32::from(b'X')
    );
    assert_eq!(client.answer(fflush, &[stream]), 0);
    assert_eq!(
        file_args(&mut client, stream, kos::args::READ_POINTER, 0),
        2
    );
    assert_eq!(
        blocks(&mut client, stream).1[1],
        6,
        "the stream's own count of where it is"
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "G19").as_deref(), Some(&b"AXCDE"[..]));

    let stream = opened(&mut client, "G19r", b"rb");
    assert_eq!(client.answer(fgetc, &[stream]), u32::from(b'A'));
    file_args(&mut client, stream, kos::args::WRITE_POINTER, 1);
    assert_eq!(client.answer(fread, &[BUFFER, 1, 4095, stream]), 4095);
    assert_eq!(
        client.answer(fgetc, &[stream]),
        0,
        "refilled from the pointer, the zero after the first byte"
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn a_flush_in_error_answers_eof_and_keeps_its_buffer_for_a_later_delivery() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G18", b"w+b");
    written(&mut client, stream, b"ABCDE");
    mark(&mut client, stream, flag::ERROR);
    assert_eq!(client.answer(fflush, &[stream]), EOF);
    let (file, words) = blocks(&mut client, stream);
    let base = file[4];
    assert_eq!(
        (file[0], file[1], file[2], file[3]),
        (base + 5, 0, (5 - 4096i32).cast_unsigned(), 0x0041_0987)
    );
    assert_eq!(
        (words[1], words[5]),
        (0, base + 1),
        "the data's end left as it stood"
    );
    assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "G18").as_deref(), Some(&b"ABCDE"[..]));

    let stream = opened(&mut client, "G18", b"w+b");
    written(&mut client, stream, b"ABCDE");
    assert_eq!(client.answer(fflush, &[stream]), 0);
    mark(&mut client, stream, flag::ERROR);
    assert_eq!(
        client.answer(fflush, &[stream]),
        EOF,
        "in error with nothing to deliver"
    );
    assert_eq!(client.answer(fclose, &[stream]), EOF);
    assert_eq!(disc_file(&tree, "G18").as_deref(), Some(&b"ABCDE"[..]));

    let stream = opened(&mut client, "G18", b"rb");
    assert_eq!(client.answer(fgetc, &[stream]), u32::from(b'A'));
    mark(&mut client, stream, flag::ERROR);
    assert_eq!(
        client.answer(fflush, &[stream]),
        0,
        "a stream not written to"
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn output_goes_unchecked_into_the_space_a_positioning_left() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G25", b"w+b");
    written(&mut client, stream, b"ABCDE");
    assert_eq!(client.answer(fseek, &[stream, 2, 0]), 0);
    mark(&mut client, stream, flag::ERROR);
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        u32::from(b'X'),
        "in error, yet written"
    );
    let file = blocks(&mut client, stream).0;
    assert_eq!(
        (file[0] - file[4], file[2], file[3]),
        (3, 4093, 0x0041_2987)
    );
    assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "G25").as_deref(), Some(&b"ABXDE"[..]));

    let stream = opened(&mut client, "G25", b"w+b");
    written(&mut client, stream, b"ABCDE");
    mark(&mut client, stream, flag::ERROR);
    client.put(stream + 8, &0u32.to_le_bytes());
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        EOF,
        "its count nought: checked, so refused"
    );
    client.put(stream + 8, &u32::MAX.to_le_bytes());
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        u32::from(b'X'),
        "one place left by hand"
    );
    assert_eq!(blocks(&mut client, stream).0[2], 0);
    assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "G25").as_deref(), Some(&b"ABCDEX"[..]));
}

#[test]
fn a_read_past_held_output_delivers_it_and_reads_on_after_it() {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/G22"), b"ABCDEFGHIJ");
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G22", b"r+b");
    assert_eq!(
        client.answer(setvbuf, &[stream, USER_BUFFER, flag::FULLY, 4]),
        0
    );
    for c in *b"XY" {
        assert_eq!(client.answer(fputc, &[u32::from(c), stream]), u32::from(c));
    }
    assert_eq!(client.answer(fseek, &[stream, 2, 0]), 0);
    assert_eq!(client.answer(fgetc, &[stream]), u32::from(b'C'));
    let (file, words) = blocks(&mut client, stream);
    assert_eq!(
        (file[0], file[1], file[3]),
        (USER_BUFFER + 3, 1, 0x0043_1107),
        "the buffer kept, read on into its room"
    );
    assert_eq!((words[1], words[5]), (0, USER_BUFFER + 4));
    assert_eq!(
        file_args(&mut client, stream, kos::args::READ_POINTER, 0),
        4
    );
    assert_eq!(client.answer(ftell, &[stream]), 3);
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "G22").as_deref(), Some(&b"XYCDEFGHIJ"[..]));
}

#[test]
fn a_flush_with_the_pointer_inside_its_data_leaves_a_positioning() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G22s", b"w+b");
    written(&mut client, stream, b"ABCDE");
    assert_eq!(client.answer(fseek, &[stream, 2, 0]), 0);
    assert_eq!(client.answer(fflush, &[stream]), 0);
    let (file, words) = blocks(&mut client, stream);
    assert_eq!(
        (file[0] - file[4], file[1], file[2], file[3]),
        (0, 0, 0, 0x0044_0927)
    );
    assert_eq!(
        (words[1], words[3]),
        (5, 2),
        "the buffer past what it delivered, the position kept to make"
    );
    assert_eq!(client.answer(ftell, &[stream]), 2);
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        u32::from(b'X')
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(disc_file(&tree, "G22s").as_deref(), Some(&b"ABXDE"[..]));
}

#[test]
fn positionings_no_file_has_are_refused_as_genuine_refuses_them() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G17r", b"w+b");
    written(&mut client, stream, b"ABCDE");
    for (offset, whence, errno_after) in [
        ((-100i32).cast_unsigned(), 0, 0),
        ((-7i32).cast_unsigned(), 1, 0),
        ((-100i32).cast_unsigned(), 2, 0),
        (0, 3, 0),
        (0x7FFF_FFFE, 2, EOVERFLOW),
    ] {
        set_errno(&mut client, 0);
        assert_eq!(
            client.answer(fseek, &[stream, offset, whence]),
            2,
            "{offset:#X} from {whence}"
        );
        assert_eq!(errno(&mut client), errno_after);
        let file = blocks(&mut client, stream).0;
        assert_eq!(
            (file[1], file[2], file[3] & flag::ERROR),
            (0, 0, flag::ERROR),
            "refused: in error, counts nought"
        );
        assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    }
    assert_eq!(
        client.answer(fseek, &[stream, (-2i32).cast_unsigned(), 2]),
        0,
        "the buffer's data counted at the end"
    );
    assert_eq!(client.answer(ftell, &[stream]), 3);
    for position in [0x8000_0001, (-100i32).cast_unsigned()] {
        client.put(WORDS, &position.to_le_bytes());
        set_errno(&mut client, 0);
        assert_eq!(client.answer(fsetpos, &[stream, WORDS]), 2);
        assert_eq!(
            errno(&mut client),
            EDOM,
            "{position:#X}: no position a long holds"
        );
        assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    }
    assert_eq!(client.answer(fseeko64, &[stream, 0x8000_0000, 0, 0]), 0);
    set_errno(&mut client, 0);
    assert_eq!(client.answer(ftell, &[stream]), u32::MAX);
    assert_eq!(errno(&mut client), EOVERFLOW);
    set_errno(&mut client, 0);
    assert_eq!(
        client.answer(fgetpos, &[stream, WORDS]),
        0,
        "answers nought"
    );
    assert_eq!(
        (client.word(WORDS), errno(&mut client)),
        (u32::MAX, EOVERFLOW)
    );
    assert_eq!(client.call(ftello64, &[stream]).r[..2], [0x8000_0000, 0]);
    set_errno(&mut client, 0);
    assert_eq!(
        client.answer(fseeko64, &[stream, 0, 1, 0]),
        2,
        "past the 32 bits"
    );
    assert_eq!(errno(&mut client), EOVERFLOW);
    client.put(WORDS, &[0u32.to_le_bytes(), 1u32.to_le_bytes()].concat());
    set_errno(&mut client, 0);
    assert_eq!(client.answer(fsetpos64, &[stream, WORDS]), 2);
    assert_eq!(errno(&mut client), EDOM);
    assert_eq!(client.answer(clearerr, &[stream]), stream + word::FLAG);
    assert_eq!(client.answer(fseek, &[stream, 0, 0]), 0);
    assert_eq!(client.answer(fclose, &[stream]), 0);
}

#[test]
fn an_append_stream_goes_to_the_end_only_at_its_first_transfer() {
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/G19a"), b"ABCDE")
        .file(&format!("{DISC}/G19b"), b"ABCDE");
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G19a", b"ab");
    assert_eq!(client.answer(fseek, &[stream, 1, 0]), 0);
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        u32::from(b'X')
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
    assert_eq!(
        disc_file(&tree, "G19a").as_deref(),
        Some(&b"AXCDE"[..]),
        "a positioning honoured"
    );

    let stream = opened(&mut client, "G19b", b"ab");
    assert_eq!(
        client.answer(fputc, &[u32::from(b'X'), stream]),
        u32::from(b'X')
    );
    assert_eq!(client.answer(fflush, &[stream]), 0);
    file_args(&mut client, stream, kos::args::WRITE_EXTENT, 20);
    assert_eq!(
        client.answer(fputc, &[u32::from(b'Y'), stream]),
        u32::from(b'Y')
    );
    assert_eq!(client.answer(fclose, &[stream]), 0);
    let expected = [&b"ABCDEXY"[..], &[0; 13]].concat();
    assert_eq!(
        disc_file(&tree, "G19b").as_deref(),
        Some(&expected[..]),
        "the end not followed after that"
    );
}

#[test]
fn a_transfer_the_file_refuses_sets_errno_minus_one() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let stream = opened(&mut client, "G23", b"w+b");
    written(&mut client, stream, b"ABCDE");
    let handle = blocks(&mut client, stream).0[5];
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[kos::find::CLOSE, handle]);
    client
        .session
        .kernel
        .os()
        .perform(X | kos::FIND, &mut regs)
        .expect("closed behind its back");
    set_errno(&mut client, 0);
    assert_eq!(client.answer(fflush, &[stream]), EOF);
    assert_eq!(errno(&mut client), OS_REFUSED);
    let file = blocks(&mut client, stream).0;
    assert_eq!(
        file[3], 0x0045_0987,
        "in error, marked as having reached its file"
    );
    set_errno(&mut client, 0);
    assert_eq!(
        client.answer(fseek, &[stream, 0, 2]),
        1,
        "its end could not be read"
    );
    assert_eq!(errno(&mut client), OS_REFUSED);
}
