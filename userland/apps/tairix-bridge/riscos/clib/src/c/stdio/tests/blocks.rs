//! The block transfers held to what the same bytes moved a character at a
//! time leave: the bytes moved, the answers, and the stream's blocks after.

use tairix_riscos_kernel::os::{Fail, Unwind};

use super::*;
use crate::c::testing::SLOT_END;

/// Where the guest's block lies, clear of the heap and the stack.
const BLOCK: u32 = SLOT_END - 0x8000;
/// More than two buffers' worth.
const LENGTH: usize = 10_000;

/// Letters, with a newline every 97 bytes.
fn text() -> Vec<u8> {
    (0..LENGTH)
        .map(|n| {
            if n % 97 == 96 {
                b'\n'
            } else {
                b'a' + u8::try_from(n % 26).expect("a letter")
            }
        })
        .collect()
}

/// `P54w` holding `content` opened for `mode`, twice over in two clients
/// laid alike: the disc each serves, the clients and their streams.
fn twice(content: &[u8], mode: &[u8]) -> [(Tree, Client, u32); 2] {
    [(); 2].map(|()| {
        let tree = holding(content);
        let mut client = Client::with_streams(&tree, &[]);
        let stream = opened(&mut client, "P54w", mode);
        (tree, client, stream)
    })
}

/// Up to `most` characters by `fgetc`, stopping at the end or past a
/// newline where `line`.
fn by_getc(client: &mut Client, stream: u32, most: usize, line: bool) -> Vec<u8> {
    let mut got = Vec::new();
    while got.len() < most {
        let c = client.answer(fgetc, &[stream]);
        if c == EOF {
            break;
        }
        got.push(c.to_le_bytes()[0]);
        if line && c == u32::from(b'\n') {
            break;
        }
    }
    got
}

#[test]
fn fread_takes_what_fgetc_would_and_leaves_the_stream_as_it_would() {
    let content = text();
    let [(_, mut block, at), (_, mut chars, other)] = twice(&content, b"r");
    for (count, push_back) in [(5000, true), (2100, false), (6000, false)] {
        let wanted = by_getc(&mut chars, other, count, false);
        let read = block.answer(fread, &[BLOCK, 1, u32::try_from(count).expect("small"), at]);
        assert_eq!(read, u32::try_from(wanted.len()).expect("small"), "{count}");
        assert_eq!(block.bytes(BLOCK, wanted.len()), wanted, "{count}");
        assert_eq!(blocks(&mut block, at), blocks(&mut chars, other), "{count}");
        if push_back {
            let q = u32::from(b'Q');
            assert_eq!(block.answer(ungetc, &[q, at]), q);
            assert_eq!(chars.answer(ungetc, &[q, other]), q);
        }
    }
}

#[test]
fn fgets_reads_the_line_fgetc_would_across_every_refill() {
    let content = text();
    let [(_, mut block, at), (_, mut chars, other)] = twice(&content, b"r");
    for room in [300u32, 50, 98, 97, 2] {
        loop {
            let wanted = by_getc(&mut chars, other, room as usize - 1, true);
            let answer = block.answer(fgets, &[BLOCK, room, at]);
            if wanted.is_empty() {
                assert_eq!(answer, 0, "ended");
                break;
            }
            assert_eq!(answer, BLOCK);
            assert_eq!(block.string(BLOCK), wanted, "{room}");
            assert_eq!(blocks(&mut block, at), blocks(&mut chars, other), "{room}");
            if room == 2 && block.answer(ftell, &[at]) > 300 {
                break;
            }
        }
        for client in [(&mut block, at), (&mut chars, other)] {
            assert_eq!(client.0.answer(fseek, &[client.1, 0, 0]), 0);
        }
    }
}

#[test]
fn fwrite_and_fputs_write_what_fputc_would() {
    let content = text();
    let string = &content[..LENGTH - 4000];
    for (n, entry) in [fwrite, fputs].into_iter().enumerate() {
        let [(tree, mut block, at), (other_tree, mut chars, other)] = twice(b"", b"w");
        let from = if n == 0 { &content[..] } else { string };
        let bytes = u32::try_from(from.len()).expect("small");
        block.put(BLOCK, &[from, b"\0"].concat());
        let answer = if n == 0 {
            block.answer(entry, &[BLOCK, 1, bytes, at])
        } else {
            block.answer(entry, &[BLOCK, at])
        };
        for &b in from {
            assert_eq!(chars.answer(fputc, &[u32::from(b), other]), u32::from(b));
        }
        assert_eq!(answer, if n == 0 { bytes } else { 0 });
        assert_eq!(blocks(&mut block, at), blocks(&mut chars, other), "{n}");
        assert_eq!(block.answer(fclose, &[at]), 0);
        assert_eq!(chars.answer(fclose, &[other]), 0);
        assert_eq!(disc_file(&tree, "P54w").expect("written"), from, "{n}");
        assert_eq!(disc_file(&other_tree, "P54w").expect("written"), from);
    }
}

#[test]
fn a_block_from_memory_that_ends_moves_every_byte_before_it_then_aborts() {
    let [(_, mut client, stream), _] = twice(b"", b"w");
    assert_eq!(
        client.answer(fputc, &[u32::from(b'>'), stream]),
        u32::from(b'>')
    );
    let base = blocks(&mut client, stream).0[4];
    let start = SLOT_END - 100;
    let laid: Vec<u8> = (1..=100u8).collect();
    client.put(start, &laid);
    let calls: [(Code, [u32; 4]); 2] = [
        (fwrite, [start, 1, 200, stream]),
        (fputs, [start, stream, 0, 0]),
    ];
    for (entry, arguments) in calls {
        let outcome = client.outcome(entry, &arguments);
        assert!(
            matches!(outcome, Err(Fail::Unwind(Unwind::Transfer))),
            "aborted: {outcome:?}"
        );
        assert_eq!(client.bytes(base + 1, 100), laid, "moved before the abort");
        client.put(base + 1, &[0; 100]);
    }
}
