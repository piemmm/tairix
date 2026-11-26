//! The standard streams as `_main`'s redirections leave them on RISC OS 5.30,
//! as the g24 and p43 probes recorded: `stderr` sent where `stdout` goes
//! shares its handle, each line-buffered in a small buffer of its own.

use super::*;
use crate::c::args::{Redirection, Standard};
use crate::c::stdio::open::redirect;

fn to_file(stream: Standard, name: &str, append: bool) -> Redirection {
    Redirection::File {
        stream,
        name: format!("HostFS:$.{name}").into_bytes(),
        append,
    }
}

fn redirected(client: &mut Client, redirections: &[Redirection]) {
    for redirection in redirections {
        assert!(
            redirect(&mut client.session.kernel.os(), statics(), redirection).expect("redirected")
        );
    }
}

fn status_of_handle(client: &mut Client, handle: u32) -> u32 {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[kos::args::STATUS, handle]);
    client
        .session
        .kernel
        .os()
        .perform(X | kos::ARGS, &mut regs)
        .expect("OS_Args 254");
    regs.r[0]
}

#[test]
fn stderr_sent_where_stdout_goes_shares_its_file_line_buffered() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let (stdout, stderr) = (file_at(statics(), 1), file_at(statics(), 2));
    redirected(
        &mut client,
        &[to_file(Standard::OutputAndError, "V1", false)],
    );
    let (out, err) = (blocks(&mut client, stdout).0, blocks(&mut client, stderr).0);
    assert_eq!(
        (out[3], out[5], out[7], out[0] == out[4]),
        (0x0010_0A02, err[5], 0x100, true),
        "shared, a line each"
    );
    assert_eq!(
        (err[3], err[7], err[0] == err[4]),
        (0x0010_0A02, 0x100, true)
    );
    assert_ne!(out[4], err[4], "each its own buffer");
    for (stream, text) in [
        (stdout, &b"a"[..]),
        (stderr, b"X\n"),
        (stdout, b"b\n"),
        (stderr, b"Y"),
    ] {
        let text = put_text(&mut client, TEXT, text);
        assert_eq!(client.answer(fputs, &[text, stream]), 0);
    }
    let (out, out_words) = blocks(&mut client, stdout);
    assert_eq!(
        (out[3], out_words[1]),
        (0x0055_2A02, 5),
        "its place read back from the shared file"
    );
    assert_eq!(client.answer(fclose, &[stdout]), 0);
    let handle = blocks(&mut client, stderr).0[5];
    assert_eq!(
        blocks(&mut client, stderr).0[3] & flag::SHARED,
        0,
        "the file left to stderr alone"
    );
    assert_ne!(
        status_of_handle(&mut client, handle) & kos::args::status::UNALLOCATED,
        kos::args::status::UNALLOCATED
    );
    assert_eq!(client.answer(fclose, &[stderr]), 0);
    assert_eq!(
        status_of_handle(&mut client, handle) & kos::args::status::UNALLOCATED,
        kos::args::status::UNALLOCATED
    );
    assert_eq!(
        disc_file(&tree, "V1").as_deref(),
        Some(&b"X\nab\nY"[..]),
        "each line where it was delivered"
    );
}

#[test]
fn stderr_appended_after_stdout_follows_it_to_the_end() {
    let tree = Tree::new().dir(DISC).file(&format!("{DISC}/V4"), b"12345");
    let mut client = Client::with_streams(&tree, &[]);
    let (stdout, stderr) = (file_at(statics(), 1), file_at(statics(), 2));
    redirected(
        &mut client,
        &[
            to_file(Standard::Output, "V4", true),
            Redirection::ErrorToOutput,
        ],
    );
    assert_eq!(blocks(&mut client, stdout).0[3], 0x0010_8A22);
    assert_eq!(
        blocks(&mut client, stderr).0[3],
        0x0010_8A02,
        "stdout's flags but its positioning"
    );
    for (stream, text) in [(stdout, &b"a\n"[..]), (stderr, b"b\n")] {
        let text = put_text(&mut client, TEXT, text);
        assert_eq!(client.answer(fputs, &[text, stream]), 0);
    }
    assert_eq!(
        blocks(&mut client, stderr).1[..4],
        [0, 9, 0, 7],
        "sent to the end its first transfer found"
    );
    assert_eq!(client.answer(fclose, &[stdout]), 0);
    assert_eq!(client.answer(fclose, &[stderr]), 0);
    assert_eq!(disc_file(&tree, "V4").as_deref(), Some(&b"12345a\nb\n"[..]));
}

#[test]
fn stderr_alone_redirected_is_fully_buffered_and_a_later_stdout_redirection_unshares_it() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let (stdout, stderr) = (file_at(statics(), 1), file_at(statics(), 2));
    redirected(&mut client, &[to_file(Standard::Error, "V3", false)]);
    let text = put_text(&mut client, TEXT, b"ab");
    assert_eq!(client.answer(fputs, &[text, stderr]), 0);
    assert_eq!(
        blocks(&mut client, stderr).0[3],
        0x0041_2902,
        "fully buffered"
    );
    assert_eq!(client.answer(fclose, &[stderr]), 0);

    let mut client = Client::with_streams(&tree, &[]);
    redirected(&mut client, &[Redirection::ErrorToOutput]);
    assert_eq!(
        (
            blocks(&mut client, stdout).0[3],
            blocks(&mut client, stderr).0[3]
        ),
        (0x0010_0002, 0x0010_0002)
    );
    assert_eq!(
        blocks(&mut client, stderr).0[7],
        0,
        "no buffer for the console"
    );
    redirected(&mut client, &[to_file(Standard::Output, "V8", false)]);
    assert_eq!(
        (
            blocks(&mut client, stdout).0[3],
            blocks(&mut client, stderr).0[3]
        ),
        (2, 2),
        "stderr the console's alone"
    );
}

#[test]
fn setvbuf_gives_back_a_buffer_the_library_gave() {
    let tree = Tree::new().dir(DISC);
    let mut client = Client::with_streams(&tree, &[]);
    let (stdout, stderr) = (file_at(statics(), 1), file_at(statics(), 2));
    redirected(
        &mut client,
        &[to_file(Standard::OutputAndError, "V6", false)],
    );
    let given: Vec<u32> = [stdout, stderr]
        .iter()
        .map(|&stream| blocks(&mut client, stream).0[4])
        .collect();
    assert_eq!(
        client.answer(setvbuf, &[stdout, USER_BUFFER, flag::FULLY, 64]),
        0
    );
    assert_eq!(client.answer(setvbuf, &[stderr, 0, flag::FULLY, 64]), 0);
    let (out, err) = (blocks(&mut client, stdout).0, blocks(&mut client, stderr).0);
    assert_eq!((out[3], out[4], out[7]), (0x0010_0102, USER_BUFFER, 64));
    assert_eq!((err[3], err[4], err[7]), (0x0010_0102, 0, 64));
    let again = client.answer(crate::c::alloc::malloc, &[0x100]);
    assert!(
        given.contains(&again),
        "a buffer given back is taken again: {again:#X} of {given:X?}"
    );
}
