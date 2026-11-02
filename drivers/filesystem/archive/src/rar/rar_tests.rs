//! RAR archives read through the driver: archives the RAR encoder wrote
//! (rarfile's, ISC), streams synthesised for the filters and PPMd and
//! checked against libarchive, and RAR 5 streams a small encoder below
//! writes, across every symbol, filter and solid run.

extern crate std;

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{
    FilesystemRead, FilesystemWrite, NodeKind, NodeSecurity, WriteRefusal,
};
use tairix_abi::DriverError;

use super::bits::{Bits, Reader, Source};
use super::filter;
use super::huffman::Code;
use super::ppmd::{Coder, Model};
use super::window::Window;
use crate::test_support::{contents, names, open, try_open, walk, MemoryAttachment};
use tairix_fuzzseed::meter::{metered, refusing_past};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../tests/fixtures/rar/", $name))
    };
}

fn crc(bytes: &[u8]) -> u32 {
    tairix_crc32::checksum(bytes)
}

#[test]
fn rar_1_5_and_2_archives_open_past_their_embedded_comments() {
    for (archive, crcs) in [
        (
            &fixture!("rarfile/rar15-comment.rar")[..],
            [0xe27f_07a9, 0x3c43_06f7],
        ),
        (
            &fixture!("rarfile/rar202-comment-nopsw.rar")[..],
            [0x7a19_7dba, 0x785f_c3e3],
        ),
    ] {
        let mut fs = open(&MemoryAttachment::new(archive, false, b"old.rar"));
        let root = fs.root();
        assert_eq!(
            names(&mut fs, root),
            [b"FILE1.TXT".to_vec(), b"FILE2.TXT".to_vec()]
        );
        for (name, expected) in [&b"FILE1.TXT"[..], b"FILE2.TXT"].into_iter().zip(crcs) {
            let file = walk(&mut fs, &[name]);
            assert_eq!(crc(&contents(&mut fs, file)), expected);
        }
    }
}

#[test]
fn a_solid_member_is_read_through_its_run_in_any_order() {
    let mut fs = open(&MemoryAttachment::new(
        fixture!("rarfile/rar3-solid.rar"),
        false,
        b"solid.rar",
    ));
    let second = walk(&mut fs, &[b"stest2.txt"]);
    let later = contents(&mut fs, second);
    let first = walk(&mut fs, &[b"stest1.txt"]);
    let earlier = contents(&mut fs, first);
    assert_eq!(later.len(), 2048);
    assert_eq!(crc(&later), 0xc5b7_e6a2);
    assert_eq!(earlier, later);
}

#[test]
fn names_directories_and_links_are_served_as_recorded() {
    let mut fs = open(&MemoryAttachment::new(
        fixture!("rarfile/rar3-subdirs.rar"),
        false,
        b"dirs.rar",
    ));
    let sub = walk(&mut fs, &[b"sub"]);
    let mut listed = names(&mut fs, sub);
    listed.sort();
    let expected: Vec<Vec<u8>> = vec![
        b"dir1".to_vec(),
        b"dir2".to_vec(),
        b"empty".to_vec(),
        "with space".as_bytes().to_vec(),
        "üȵĩöḋè".as_bytes().to_vec(),
    ];
    let mut expected = expected;
    expected.sort();
    assert_eq!(listed, expected);
    let unicode = walk(&mut fs, &[b"sub", "üȵĩöḋè".as_bytes(), b"file.txt"]);
    assert_eq!(crc(&contents(&mut fs, unicode)), 0x2fec_89c1);

    let mut fs = open(&MemoryAttachment::new(
        fixture!("rarfile/rar3-symlink-unix.rar"),
        false,
        b"links.rar",
    ));
    let link = walk(&mut fs, &[b"data_link"]);
    assert_eq!(fs.node_info(link).unwrap().kind, NodeKind::Symlink);
    let mut target = [0u8; 64];
    let len = fs.read_link(link, &mut target).unwrap();
    assert_eq!(&target[..len], b"data.txt");
}

#[test]
fn rar_5_members_are_held_to_their_crc_or_blake2sp_and_copies_read_as_their_source() {
    for archive in [
        &fixture!("rarfile/rar5-crc.rar")[..],
        fixture!("rarfile/rar5-blake.rar"),
    ] {
        let mut fs = open(&MemoryAttachment::new(archive, false, b"five.rar"));
        for name in [&b"stest1.txt"[..], b"stest2.txt"] {
            let file = walk(&mut fs, &[name]);
            assert_eq!(crc(&contents(&mut fs, file)), 0xc5b7_e6a2);
        }
    }
    for archive in [
        &fixture!("rarfile/rar5-hlink.rar")[..],
        fixture!("rarfile/rar5-dups.rar"),
    ] {
        let mut fs = open(&MemoryAttachment::new(archive, false, b"copies.rar"));
        let source = walk(&mut fs, &[b"stest1.txt"]);
        let original = contents(&mut fs, source);
        for name in [&b"stest2.txt"[..], b"stest3.txt"] {
            let copy = walk(&mut fs, &[name]);
            assert_eq!(contents(&mut fs, copy), original);
        }
    }
}

#[test]
fn a_member_whose_path_runs_through_a_link_is_left_out() {
    let mut fs = open(&MemoryAttachment::new(
        fixture!("rarfile/rar5-evil-symlink-traversal.rar"),
        false,
        b"evil.rar",
    ));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"up".to_vec()]);
    let up = walk(&mut fs, &[b"up"]);
    assert_eq!(fs.node_info(up).unwrap().kind, NodeKind::Symlink);
    let mut target = [0u8; 8];
    let len = fs.read_link(up, &mut target).unwrap();
    assert_eq!(&target[..len], b"..");
}

#[test]
fn encrypted_members_and_headers_are_refused_with_their_reason() {
    let mut fs = open(&MemoryAttachment::new(
        fixture!("rarfile/rar5-psw.rar"),
        false,
        b"secret.rar",
    ));
    let file = walk(&mut fs, &[b"stest1.txt"]);
    let mut buf = [0u8; 16];
    assert_eq!(
        fs.read_at(file, 0, &mut buf),
        Err(DriverError::PermissionDenied)
    );
    for archive in [
        &fixture!("rarfile/rar3-comment-hpsw.rar")[..],
        fixture!("rarfile/rar5-hpsw.rar"),
    ] {
        let refused = try_open(&MemoryAttachment::new(archive, false, b"hidden.rar")).err();
        assert_eq!(refused, Some(DriverError::PermissionDenied));
    }
}

#[test]
fn rar_3_filters_and_ppmd_decode_to_what_their_checks_hold() {
    let vectors: [&[u8]; 11] = [
        fixture!("synth/rgb_w30_p0.rar"),
        fixture!("synth/rgb_w3_p0.rar"),
        fixture!("synth/rgb_w12_p1.rar"),
        fixture!("synth/rgb_w33_p2.rar"),
        fixture!("synth/audio_ch1.rar"),
        fixture!("synth/audio_ch2.rar"),
        fixture!("synth/audio_ch5.rar"),
        fixture!("synth/delta_reuse.rar"),
        fixture!("synth/ppmd_o6.rar"),
        fixture!("synth/ppmd_o25.rar"),
        fixture!("synth/ppmd_max.rar"),
    ];
    for archive in vectors {
        let mut fs = open(&MemoryAttachment::new(archive, false, b"synth.rar"));
        let root = fs.root();
        let listed = names(&mut fs, root);
        assert_eq!(listed.len(), 1);
        let file = walk(&mut fs, &[&listed[0]]);
        // A read through to the end is held to the member's CRC.
        let bytes = contents(&mut fs, file);
        assert_eq!(bytes.len() as u64, fs.node_info(file).unwrap().size);
    }
}

#[test]
fn a_read_from_anywhere_gives_what_a_read_in_order_gives() {
    for archive in [
        &fixture!("synth/ppmd_o6.rar")[..],
        fixture!("rarfile/rar3-solid.rar"),
    ] {
        let mut fs = open(&MemoryAttachment::new(archive, false, b"any.rar"));
        let root = fs.root();
        let last = names(&mut fs, root).pop().unwrap();
        let file = walk(&mut fs, &[&last]);
        let forward = contents(&mut fs, file);
        let mut backward = vec![0u8; forward.len()];
        let mut end = backward.len();
        while end > 0 {
            let start = end.saturating_sub(1000);
            let mut at = start;
            while at < end {
                at += fs.read_at(file, at as u64, &mut backward[at..end]).unwrap();
            }
            end = start;
        }
        assert_eq!(backward, forward);
    }
}

#[test]
fn damaged_data_is_refused_not_served() {
    // The stored member's last byte, and a byte of the compressed one.
    let mut archive = fixture!("rarfile/rar5-blake.rar").to_vec();
    let stored_end = archive.len() - 8;
    archive[stored_end - 1] ^= 0x20;
    let mut fs = open(&MemoryAttachment::new(&archive, false, b"damaged.rar"));
    let stored = walk(&mut fs, &[b"stest2.txt"]);
    let mut buf = vec![0u8; 2048];
    let mut at = 0;
    let outcome = loop {
        match fs.read_at(stored, at, &mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => at += n as u64,
            Err(err) => break Err(err),
        }
    };
    assert_eq!(outcome, Err(DriverError::MediumError));

    // The first solid member's data begins past the signature, the main
    // header and its own 44-byte header.
    let mut archive = fixture!("rarfile/rar3-solid.rar").to_vec();
    archive[7 + 13 + 44 + 40] ^= 0x55;
    let mut fs = open(&MemoryAttachment::new(&archive, false, b"damaged.rar"));
    for name in [&b"stest1.txt"[..], b"stest2.txt"] {
        let file = walk(&mut fs, &[name]);
        let mut buf = vec![0u8; 4096];
        let mut at = 0;
        let outcome = loop {
            match fs.read_at(file, at, &mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => at += n as u64,
                Err(err) => break Err(err),
            }
        };
        assert_eq!(
            outcome,
            Err(DriverError::MediumError),
            "{name:?} is decoded from damaged data"
        );
    }
}

#[test]
fn a_rar_archive_refuses_every_change() {
    let mut fs = open(&MemoryAttachment::new(
        fixture!("rarfile/rar5-crc.rar"),
        true,
        b"five.rar",
    ));
    assert_eq!(fs.write_refusal(), Some(WriteRefusal::FormatReadOnly));
    let root = fs.root();
    let made = fs.create(
        root,
        b"new",
        NodeKind::RegularFile,
        &NodeSecurity::new(0o644, 0, 0),
    );
    assert_eq!(made.err(), Some(DriverError::ReadOnly));
    let file = walk(&mut fs, &[b"stest1.txt"]);
    assert_eq!(fs.write_at(file, 0, b"x"), Err(DriverError::ReadOnly));
}

#[test]
fn one_volume_of_several_is_refused() {
    // RAR 5: the main header's volume flag, past its type, its flags and
    // the length of its extra area, its CRC recomputed.
    let mut five = fixture!("rarfile/rar5-crc.rar").to_vec();
    let size = usize::from(five[12]);
    assert_eq!(
        (five[13], five[14] & 1),
        (1, 1),
        "a main header with an extra area"
    );
    five[16] |= 1;
    let sum = crc(&five[12..13 + size]);
    five[8..12].copy_from_slice(&sum.to_le_bytes());
    assert_eq!(
        try_open(&MemoryAttachment::new(&five, false, b"v.rar")).err(),
        Some(DriverError::Unsupported)
    );
    // RAR 1.5 to 4: the main header's volume flag, its CRC recomputed.
    let mut four = fixture!("rarfile/rar3-solid.rar").to_vec();
    four[10] |= 1;
    let sum = u16::try_from(crc(&four[9..20]) & 0xFFFF).unwrap();
    four[7..9].copy_from_slice(&sum.to_le_bytes());
    assert_eq!(
        try_open(&MemoryAttachment::new(&four, false, b"v.rar")).err(),
        Some(DriverError::Unsupported)
    );
}

#[test]
fn a_code_with_too_many_short_lengths_is_refused_and_one_with_too_few_is_accepted() {
    let mut code = Code::empty();
    assert_eq!(code.build(&[1, 1, 1]), Err(DriverError::MediumError));
    assert_eq!(code.build(&[16]), Err(DriverError::MediumError));
    // Two of three one-bit... an incomplete code: one symbol of two bits.
    assert!(code.build(&[0, 2, 0]).is_ok());
    assert_eq!(code.resolve(0b00 << 13), Some((1, 2)));
    assert_eq!(code.resolve(0b11 << 13), None);
    // Canonical order: shorter codes first, then by symbol.
    code.build(&[3, 1, 3, 2]).unwrap();
    assert_eq!(code.resolve(0), Some((1, 1)));
    assert_eq!(code.resolve(0b10 << 13), Some((3, 2)));
    assert_eq!(code.resolve(0b110 << 12), Some((0, 3)));
    assert_eq!(code.resolve(0b111 << 12), Some((2, 3)));
    // A long code is resolved past the lookup table.
    let mut lengths = [15u8; 300];
    lengths[0] = 1;
    code.build(&lengths).unwrap();
    assert_eq!(code.resolve(1 << 14), Some((1, 15)));
}

#[test]
fn a_match_overlapping_itself_repeats_and_one_reaching_too_far_is_refused() {
    let mut win = Window::new(0x1_0000, 100).unwrap();
    for &byte in b"ab" {
        win.literal(byte);
    }
    win.copy(2, 7).unwrap();
    let mut out = [0u8; 9];
    win.read(0, &mut out).unwrap();
    assert_eq!(&out, b"ababababa");
    assert_eq!(
        win.copy(10, 1),
        Err(DriverError::MediumError),
        "before the stream"
    );
    assert_eq!(win.copy(0, 1), Err(DriverError::MediumError));
    let mut short = Window::new(4, 1 << 20).unwrap();
    for _ in 0..8 {
        short.literal(1);
    }
    assert_eq!(
        short.copy(5, 1),
        Err(DriverError::MediumError),
        "past the dictionary"
    );
}

#[test]
fn a_window_grows_with_what_is_decoded_not_with_what_a_header_claims() {
    // A four-gibibyte dictionary over a terabyte's claimed output costs only
    // what is decoded into it.
    let mut win = Window::new(1 << 32, 1 << 40).unwrap();
    let mut written = Vec::new();
    for k in 0..200_000u32 {
        win.has_room(win.pos()).unwrap();
        let byte = u8::try_from(k.wrapping_mul(2_654_435_761) >> 24).unwrap();
        win.literal(byte);
        written.push(byte);
    }
    win.has_room(win.pos()).unwrap();
    win.copy(199_000, 50).unwrap();
    written.extend_from_within(1000..1050);
    let mut out = vec![0u8; written.len()];
    win.read(0, &mut out).unwrap();
    assert_eq!(out, written);
}

/// x86 calls made relative, as a RAR encoder makes them before its E8
/// filter: the inverse of what the filter restores.
fn e8_forward(buf: &mut [u8], file_pos: u64, wrap: u64) {
    let mut i = 0;
    while i + 5 <= buf.len() {
        if buf[i] != 0xE8 {
            i += 1;
            continue;
        }
        let cur = u32::try_from((file_pos + i as u64 + 1) % wrap).unwrap();
        let plain = u32::from_le_bytes(buf[i + 1..i + 5].try_into().unwrap());
        let stored = if plain.wrapping_add(cur) < 0x0100_0000 {
            plain.wrapping_add(cur)
        } else if plain.wrapping_sub(0x0100_0000) & 0x8000_0000 != 0
            && plain.wrapping_sub(0x0100_0000).wrapping_add(cur) & 0x8000_0000 == 0
        {
            plain.wrapping_sub(0x0100_0000)
        } else {
            plain
        };
        buf[i + 1..i + 5].copy_from_slice(&stored.to_le_bytes());
        i += 5;
    }
}

#[test]
fn the_x86_and_delta_filters_undo_what_an_encoder_does() {
    let mut plain = Vec::new();
    for k in 0u32..400 {
        plain.push(0xE8);
        plain.extend_from_slice(&(k.wrapping_mul(0x9E37_79B9) >> (k % 9)).to_le_bytes());
        plain.extend_from_slice(&[0x90, u8::try_from(k % 251).unwrap()]);
    }
    for (pos, wrap) in [(0u64, 1u64 << 32), (0x00FF_FFF0, 1 << 24), (12345, 1 << 32)] {
        let mut coded = plain.clone();
        e8_forward(&mut coded, pos, wrap);
        assert_ne!(coded, plain);
        filter::e8(&mut coded, pos, false, wrap);
        assert_eq!(coded, plain);
    }
    let samples: Vec<u8> = (0u32..999)
        .map(|k| u8::try_from((k * 7 + k / 3) % 256).unwrap())
        .collect();
    for channels in [1usize, 3, 4] {
        let mut coded = Vec::new();
        for channel in 0..channels {
            let mut prev = 0u8;
            for &byte in samples.iter().skip(channel).step_by(channels) {
                coded.push(prev.wrapping_sub(byte));
                prev = byte;
            }
        }
        let mut out = vec![0u8; samples.len()];
        filter::delta(&coded, &mut out, channels);
        assert_eq!(out, samples);
    }
}

/// Bits written most significant first, as RAR reads them.
#[derive(Default)]
struct BitSink {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitSink {
    fn put(&mut self, value: u64, n: u32) {
        for k in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if value >> k & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

/// One thing a RAR 5 stream built here codes.
#[derive(Clone, Copy)]
enum Op {
    Literal(u8),
    /// A match `dist` back, `len` long.
    Match(u64, u32),
    /// A match at the `k`th last distance, `len` long.
    Repeat(usize, u32),
    /// The last match again.
    Last,
    /// A filter of `kind` over `len` bytes starting `ahead` past the window.
    Filter {
        ahead: u64,
        len: u64,
        kind: u8,
        channels: u8,
    },
}

/// A RAR 5 member's data: blocks of `ops`, each block carrying its tables,
/// every code a fixed width so symbol `k` codes as `k`.
fn rar5_data(blocks: &[&[Op]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, ops) in blocks.iter().enumerate() {
        let mut body = BitSink::default();
        // The precode: twenty symbols of five bits, then the four tables.
        for _ in 0..20 {
            body.put(5, 4);
        }
        let widths: [(usize, u64); 4] = [(306, 9), (64, 6), (16, 4), (44, 6)];
        for &(count, width) in &widths {
            for _ in 0..count {
                body.put(width, 5);
            }
        }
        for &op in *ops {
            match op {
                Op::Literal(byte) => body.put(u64::from(byte), 9),
                Op::Last => body.put(257, 9),
                Op::Repeat(k, len) => {
                    body.put(258 + k as u64, 9);
                    put_length(&mut body, len, None);
                }
                Op::Match(dist, len) => {
                    let base = len
                        - u32::from(dist > 0x100)
                        - u32::from(dist > 0x2000)
                        - u32::from(dist > 0x4_0000);
                    put_length(&mut body, base, Some(262));
                    put_distance(&mut body, dist);
                }
                Op::Filter {
                    ahead,
                    len,
                    kind,
                    channels,
                } => {
                    body.put(256, 9);
                    for value in [ahead, len] {
                        body.put(3, 2);
                        for k in 0..4 {
                            body.put(value >> (8 * k) & 0xFF, 8);
                        }
                    }
                    body.put(u64::from(kind), 3);
                    if kind == 0 {
                        body.put(u64::from(channels) - 1, 5);
                    }
                }
            }
        }
        let used = body.bits % 8;
        let size = body.bytes.len();
        let last = index == blocks.len() - 1;
        let flags = 0x80
            | if last { 0x40 } else { 0 }
            | (2 << 3)
            | u8::try_from(if used == 0 { 7 } else { used - 1 }).unwrap();
        let size_bytes = u32::try_from(size).unwrap().to_le_bytes();
        let check = 0x5A ^ flags ^ size_bytes[0] ^ size_bytes[1] ^ size_bytes[2];
        out.extend_from_slice(&[flags, check, size_bytes[0], size_bytes[1], size_bytes[2]]);
        out.extend_from_slice(&body.bytes);
    }
    out
}

/// A length as RAR 5 codes it: a slot (on `base` of the main table, else
/// the length table) and its extra bits.
fn put_length(body: &mut BitSink, len: u32, main: Option<u64>) {
    let (slot, extra, bits) = if len < 10 {
        (u64::from(len - 2), 0, 0)
    } else {
        (8..44)
            .find_map(|slot: u32| {
                let bits = slot / 4 - 1;
                let base = ((4 | (slot & 3)) << bits) + 2;
                (len >= base && len - base < 1 << bits).then_some((
                    u64::from(slot),
                    u64::from(len - base),
                    bits,
                ))
            })
            .unwrap()
    };
    match main {
        Some(base) => body.put(base + slot, 9),
        None => body.put(slot, 6),
    }
    body.put(extra, bits);
}

fn put_distance(body: &mut BitSink, dist: u64) {
    let d = dist - 1;
    if d < 4 {
        body.put(d, 6);
        return;
    }
    let bits = d.ilog2() - 1;
    let slot = u64::from(2 * (bits + 1)) + (d >> bits & 1);
    let extra = d - ((2 | (d >> bits & 1)) << bits);
    body.put(slot, 6);
    if bits >= 4 {
        body.put(extra >> 4, bits - 4);
        body.put(extra & 15, 4);
    } else {
        body.put(extra, bits);
    }
}

/// What `ops` decode to, run on `history`: matches copied, filters applied
/// as their blocks complete — a model of the format, not of the decoder.
fn rar5_plain(history: &mut Vec<u8>, ops: &[Op]) -> Vec<u8> {
    let start = history.len();
    let mut old = [0u64; 4];
    let mut last_len = 0;
    let mut filters = Vec::new();
    for &op in ops {
        let copy = |history: &mut Vec<u8>, dist: u64, len: u32| {
            for _ in 0..len {
                let byte = history[history.len() - usize::try_from(dist).unwrap()];
                history.push(byte);
            }
        };
        match op {
            Op::Literal(byte) => history.push(byte),
            Op::Match(dist, len) => {
                old.copy_within(0..3, 1);
                old[0] = dist;
                last_len = len;
                copy(history, dist, len);
            }
            Op::Repeat(k, len) => {
                let dist = old[k];
                old.copy_within(0..k, 1);
                old[0] = dist;
                last_len = len;
                copy(history, dist, len);
            }
            Op::Last => copy(history, old[0], last_len),
            Op::Filter {
                ahead,
                len,
                kind,
                channels,
            } => filters.push((history.len() as u64 + ahead, len, kind, channels)),
        }
    }
    let mut out = history[start..].to_vec();
    for (at, len, kind, channels) in filters {
        let from = usize::try_from(at).unwrap() - start;
        let to = from + usize::try_from(len).unwrap();
        let block = out[from..to].to_vec();
        let file_pos = at - start as u64;
        match kind {
            0 => filter::delta(&block, &mut out[from..to], usize::from(channels)),
            1 => filter::e8(&mut out[from..to], file_pos, false, 1 << 24),
            2 => filter::e8(&mut out[from..to], file_pos, true, 1 << 24),
            _ => filter::arm(&mut out[from..to], file_pos),
        }
    }
    out
}

/// A RAR 5 member to be written: its name, whether it continues the last,
/// its data's blocks or stored bytes, and the length it declares where that
/// is not what its data decodes to.
type Rar5Member<'a> = (
    &'a [u8],
    bool,
    Result<&'a [&'a [Op]], &'a [u8]>,
    Option<u64>,
);

/// A RAR 5 archive of `members`, and what each holds; a member named with a
/// `\` is written as a Windows archiver writes one.
fn rar5_archive(members: &[Rar5Member<'_>]) -> (Vec<u8>, Vec<Vec<u8>>) {
    fn vint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = u8::try_from(value & 0x7F).unwrap();
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    fn block(out: &mut Vec<u8>, fields: &[u8]) {
        let mut sized = Vec::new();
        vint(&mut sized, fields.len() as u64);
        sized.extend_from_slice(fields);
        out.extend_from_slice(&crc(&sized).to_le_bytes());
        out.extend_from_slice(&sized);
    }
    let mut out = b"Rar!\x1a\x07\x01\x00".to_vec();
    block(&mut out, &[1, 0, 0]);
    let mut history = Vec::new();
    let mut plains = Vec::new();
    for &(name, solid, data, declared) in members {
        let (packed, plain, method) = match data {
            Ok(blocks) => {
                let ops: Vec<Op> = blocks.iter().flat_map(|ops| ops.iter().copied()).collect();
                if !solid {
                    history.clear();
                }
                // A member declaring another length is a hostile one, whose
                // bytes are never compared.
                let plain = match declared {
                    Some(_) => Vec::new(),
                    None => rar5_plain(&mut history, &ops),
                };
                (rar5_data(blocks), plain, 1u64)
            }
            Err(stored) => (stored.to_vec(), stored.to_vec(), 0),
        };
        let mut fields = Vec::new();
        vint(&mut fields, 2);
        vint(&mut fields, 0x02);
        vint(&mut fields, packed.len() as u64);
        vint(&mut fields, 0x04);
        vint(&mut fields, declared.unwrap_or(plain.len() as u64));
        vint(&mut fields, 0o644);
        fields.extend_from_slice(&crc(&plain).to_le_bytes());
        // Algorithm 0, a 4 MiB dictionary.
        vint(
            &mut fields,
            (5 << 10) | (method << 7) | if solid { 0x40 } else { 0 },
        );
        vint(&mut fields, u64::from(!name.contains(&b'\\')));
        vint(&mut fields, name.len() as u64);
        fields.extend_from_slice(name);
        block(&mut out, &fields);
        out.extend_from_slice(&packed);
        plains.push(plain);
    }
    block(&mut out, &[5, 0, 0]);
    (out, plains)
}

#[test]
fn rar_5_streams_decode_every_symbol_filter_and_solid_run() {
    let mut text = Vec::new();
    for k in 0..300u32 {
        text.extend_from_slice(b"the quick brown fox ");
        text.push(u8::try_from(k % 26).unwrap() + b'a');
    }
    let literals: Vec<Op> = text.iter().map(|&b| Op::Literal(b)).collect();
    let mut first: Vec<Op> = literals[..1000].to_vec();
    // Distances across every slot range, low bits through their own table.
    for dist in [1u64, 2, 4, 5, 17, 100, 257, 999] {
        first.push(Op::Match(dist, 12));
    }
    first.extend([
        Op::Repeat(0, 9),
        Op::Repeat(3, 40),
        Op::Last,
        Op::Match(3, 300),
    ]);
    let second: Vec<Op> = {
        let mut ops = vec![Op::Filter {
            ahead: 0,
            len: 600,
            kind: 0,
            channels: 3,
        }];
        ops.extend_from_slice(&literals[1000..1600]);
        ops.push(Op::Filter {
            ahead: 0,
            len: 64,
            kind: 1,
            channels: 0,
        });
        for k in 0..12u8 {
            ops.extend([
                Op::Literal(0xE8),
                Op::Literal(k),
                Op::Literal(0),
                Op::Literal(0),
                Op::Literal(0),
                Op::Literal(1),
            ]);
        }
        ops.extend([Op::Literal(0x90); 2]);
        ops.push(Op::Filter {
            ahead: 0,
            len: 40,
            kind: 3,
            channels: 0,
        });
        for k in 0..10u8 {
            ops.extend([
                Op::Literal(k),
                Op::Literal(1),
                Op::Literal(0),
                Op::Literal(0xEB),
            ]);
        }
        // Back into the first member, across the solid boundary.
        ops.push(Op::Match(1500, 50));
        ops
    };
    let third: Vec<Op> = literals[1600..2000]
        .iter()
        .copied()
        .chain([Op::Match(2000, 70)])
        .collect();
    let (archive, plains) = rar5_archive(&[
        (b"first", false, Ok(&[&first[..800], &first[800..]]), None),
        (b"stored", false, Err(b"stored between solid members"), None),
        (b"second", true, Ok(&[&second]), None),
        (b"third", true, Ok(&[&third]), None),
    ]);
    let mut fs = open(&MemoryAttachment::new(&archive, false, b"made.rar"));
    for (name, plain) in [&b"third"[..], b"first", b"second", b"stored"]
        .into_iter()
        .zip([&plains[3], &plains[0], &plains[2], &plains[1]])
    {
        let file = walk(&mut fs, &[name]);
        assert_eq!(
            &contents(&mut fs, file),
            plain,
            "{}",
            core::str::from_utf8(name).unwrap()
        );
    }
}

/// The first byte of `name` in `archive`, or its refusal.
fn first_byte(archive: &[u8], name: &[u8]) -> Result<usize, DriverError> {
    let mut fs = open(&MemoryAttachment::new(archive, false, b"hostile.rar"));
    let file = walk(&mut fs, &[name]);
    fs.read_at(file, 0, &mut [0u8; 16])
}

#[test]
fn a_solid_member_declaring_more_than_any_stream_holds_is_refused_not_overflowed() {
    let one = [Op::Literal(b'a')];
    let (archive, _) = rar5_archive(&[
        (b"first", false, Ok(&[&one]), None),
        (b"second", true, Ok(&[&one]), Some(u64::MAX)),
    ]);
    assert_eq!(
        first_byte(&archive, b"second"),
        Err(DriverError::MediumError)
    );
    assert_eq!(first_byte(&archive, b"first"), Ok(1));
}

#[test]
fn a_member_whose_filter_lies_far_past_its_end_is_refused_not_spun_on() {
    let mut first: Vec<Op> = (0..100u8).map(Op::Literal).collect();
    first.push(Op::Filter {
        ahead: 0x1_0000,
        len: 4,
        kind: 3,
        channels: 0,
    });
    // The longest match carries decoding past the stretch the filter lies beyond.
    first.push(Op::Match(1, 4097));
    let second = [Op::Literal(b'b')];
    let (archive, _) = rar5_archive(&[
        (b"first", false, Ok(&[&first]), Some(100)),
        (b"second", true, Ok(&[&second]), None),
    ]);
    assert_eq!(
        first_byte(&archive, b"second"),
        Err(DriverError::MediumError)
    );
}

/// Its packed data's length in bits is what a decoder bounds its reads by,
/// so an entry of more than a `u64` of bits is refused as the reader is made.
#[test]
fn a_member_whose_bits_no_count_holds_is_refused() {
    let entry = super::Entry {
        at: 0,
        packed: u64::MAX / 8 + 1,
        len: 20,
        method: super::Method::V29,
        dict: 0x40_0000,
        solid: false,
        check: super::Check::None,
        refused: None,
    };
    let reader = super::Reader::new(vec![entry]).unwrap();
    assert_eq!(reader.entries[0].refused, Some(DriverError::MediumError));
}

/// Packed bytes held in memory, as a decoder reads its member's data.
struct Held<'a>(&'a [u8]);

impl Source for Held<'_> {
    fn chunk(&mut self) -> &[u8] {
        self.0
    }

    fn consume(&mut self, n: usize) {
        self.0 = &self.0[n..];
    }
}

/// The one PPMd member of a synthesized RAR 3 archive: its model's size and
/// order, as its first block declares them.
struct PpmdMember<'a> {
    data: &'a [u8],
    len: u32,
    crc: u32,
    size: u32,
    order: u32,
}

impl<'a> PpmdMember<'a> {
    fn of(archive: &'a [u8]) -> Self {
        let head = 7 + 13;
        let field =
            |at: usize| u32::from_le_bytes(archive[head + at..head + at + 4].try_into().unwrap());
        let header = usize::from(u16::from_le_bytes([archive[head + 5], archive[head + 6]]));
        let packed = usize::try_from(field(7)).unwrap();
        let data = &archive[head + header..head + header + packed];
        // The block's flags — PPMd, a new model, its order — then its size.
        let (flags, max_mb) = (data[0] & 0x7F, data[1]);
        Self {
            data,
            len: field(11),
            crc: field(16),
            size: (u32::from(max_mb) + 1) << 20,
            order: u32::from(flags & 0x1F) + 1,
        }
    }

    /// Decode the member on `model`, answering what it decoded to.
    fn decode(&self, model: &mut Model) -> Vec<u8> {
        let mut bits = Bits::default();
        let mut source = Held(&self.data[2..]);
        let mut br = Reader::new(&mut bits, &mut source);
        let mut coder = Coder::start(&mut br).unwrap();
        (0..self.len)
            .map(|_| model.decode(&mut coder, &mut br).unwrap())
            .collect()
    }
}

#[test]
fn a_ppmd_model_laid_out_on_a_used_heap_decodes_as_on_a_fresh_one() {
    let member = PpmdMember::of(fixture!("synth/ppmd_o6.rar"));
    let span = usize::try_from(member.size).unwrap() + 64;
    for heap in [Vec::new(), vec![0xA5u8; span]] {
        let mut model = Model::new(member.size, member.order, heap).unwrap();
        assert_eq!(crc(&member.decode(&mut model)), member.crc);
    }
}

/// The largest model a stream may declare has its heap asked for zeroed,
/// never as bytes the decoder then fills: under a kernel that backs a page
/// on first touch, a stream holds only what its symbols build.
#[test]
fn a_ppmd_model_declaring_the_largest_heap_asks_for_it_zeroed() {
    let member = PpmdMember::of(fixture!("synth/ppmd_max.rar"));
    assert_eq!(member.size, 256 << 20, "the format's largest model");
    let (model, metering) = metered(|| Model::new(member.size, member.order, Vec::new()));
    let mut model = model.unwrap();
    assert!(metering.zeroed > 256 << 20);
    assert_eq!(
        metering.requested, metering.zeroed,
        "nothing asked for to be filled"
    );
    assert_eq!(crc(&member.decode(&mut model)), member.crc);
}

/// Two runs of `member`'s PPMd data, each decoded by a model of its own.
fn two_ppmd_runs(member: &PpmdMember<'_>) -> Vec<super::Entry> {
    let entry = super::Entry {
        at: 0,
        packed: member.data.len() as u64,
        len: u64::from(member.len),
        method: super::Method::V29,
        dict: 0x40_0000,
        solid: false,
        check: super::Check::Crc32(member.crc),
        refused: None,
    };
    vec![entry.clone(), entry]
}

/// Every byte of `entry`, read in order.
fn read_whole<A: tairix_fsserve::VolumeAccess>(
    reader: &mut super::Reader,
    regions: &crate::region::Regions<A>,
    entry: usize,
    len: u64,
) -> Result<Vec<u8>, DriverError> {
    let mut out = vec![0u8; usize::try_from(len).unwrap()];
    let mut at = 0;
    while at < out.len() {
        at += reader.read(regions, entry, at as u64, &mut out[at..])?;
    }
    Ok(out)
}

#[test]
fn a_run_short_of_memory_takes_the_memory_other_runs_hold() {
    let member = PpmdMember::of(fixture!("synth/ppmd_o6.rar"));
    let attachment = MemoryAttachment::new(member.data, false, b"two.rar");
    let regions = crate::region::Regions::new(attachment.access(), member.data.len() as u64);
    let len = u64::from(member.len);
    let entries = two_ppmd_runs(&member);

    let mut reader = super::Reader::new(entries.clone()).unwrap();
    let (first, held) = metered(|| read_whole(&mut reader, &regions, 0, len));
    assert_eq!(crc(&first.unwrap()), member.crc);
    // Room for one run's decoder and not two: the second is decoded only by
    // taking the first's memory.
    let room = held.retained / 2;
    let second = refusing_past(room, || read_whole(&mut reader, &regions, 1, len));
    assert_eq!(crc(&second.unwrap()), member.crc);

    // With no other run's memory to take, the refusal is the answer, and the
    // next read starts afresh.
    let mut alone = super::Reader::new(entries).unwrap();
    let mut buf = [0u8; 64];
    let refused = refusing_past(room, || alone.read(&regions, 1, 0, &mut buf));
    assert_eq!(refused, Err(DriverError::OutOfMemory));
    assert_eq!(
        crc(&read_whole(&mut alone, &regions, 1, len).unwrap()),
        member.crc
    );
}

#[test]
fn a_stored_member_whose_lengths_disagree_is_refused() {
    let attachment = MemoryAttachment::new(&[0x41u8; 64], false, b"short.rar");
    let regions = crate::region::Regions::new(attachment.access(), 64);
    let entry = super::Entry {
        at: 0,
        packed: 10,
        len: 20,
        method: super::Method::Stored,
        dict: 0,
        solid: false,
        check: super::Check::None,
        refused: None,
    };
    let mut reader = super::Reader::new(vec![entry]).unwrap();
    let mut buf = [0u8; 20];
    assert_eq!(
        reader.read(&regions, 0, 0, &mut buf),
        Err(DriverError::MediumError)
    );
}

#[test]
fn a_first_read_decodes_what_it_asks_for_not_a_quarter_of_the_window() {
    let mut ops = vec![Op::Literal(b'x')];
    ops.extend(core::iter::repeat_n(Op::Match(1, 4097), 256));
    let (archive, plains) = rar5_archive(&[(b"long", false, Ok(&[&ops]), None)]);
    let attachment = MemoryAttachment::new(&archive, false, b"long.rar");
    let regions = crate::region::Regions::new(attachment.access(), archive.len() as u64);
    let mut reader = super::read(&regions).unwrap().rar.unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(reader.read(&regions, 0, 0, &mut buf), Ok(16));
    assert_eq!(&buf[..], &plains[0][..16]);
    // A symbol past what was asked for at most.
    assert!(reader.cursors[0].window.pos() <= 16 + 4097);
}

#[test]
fn a_dictionary_reaches_no_further_than_its_codings_distances() {
    use super::Method;
    assert_eq!(Method::V50 { v1: true }.reach(1 << 49), 1 << 40);
    assert_eq!(Method::V50 { v1: false }.reach(1 << 40), 1 << 32);
    assert_eq!(Method::V50 { v1: false }.reach(1 << 22), 1 << 22);
    assert_eq!(Method::V29.reach(1 << 30), 0x40_0000);
}

#[test]
fn a_read_elsewhere_in_a_stored_member_never_takes_another_readers_check() {
    let data: Vec<u8> = (0..20_000u32)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    let (mut archive, _) = rar5_archive(&[(b"stored", false, Err(&data), None)]);
    // One byte damaged, past what the reader in order has read first.
    let at = archive.windows(8).position(|w| w == &data[..8]).unwrap() + 1000;
    archive[at] ^= 0xFF;
    let attachment = MemoryAttachment::new(&archive, false, b"stored.rar");
    let regions = crate::region::Regions::new(attachment.access(), archive.len() as u64);
    let mut reader = super::read(&regions).unwrap().rar.unwrap();
    let mut buf = [0u8; 100];
    assert_eq!(reader.read(&regions, 0, 0, &mut buf), Ok(100));
    assert_eq!(reader.read(&regions, 0, 15_000, &mut buf), Ok(100));
    let mut rest = vec![0u8; data.len() - 100];
    let mut done = 0;
    let refused = loop {
        match reader.read(&regions, 0, 100 + done as u64, &mut rest[done..]) {
            Ok(got) => done += got,
            Err(err) => break Some(err),
        }
        if done == rest.len() {
            break None;
        }
    };
    assert_eq!(refused, Some(DriverError::MediumError));
}

/// The three members of a solid run, the second carrying on from the first
/// and the third from the second.
fn three_in_a_run() -> [Vec<Op>; 3] {
    [
        b"abcdefgh".iter().map(|&b| Op::Literal(b)).collect(),
        alloc::vec![Op::Match(8, 8), Op::Literal(b'!')],
        alloc::vec![Op::Match(9, 9)],
    ]
}

#[test]
fn a_member_served_under_no_name_keeps_its_place_in_its_solid_run() {
    let [first, second, third] = three_in_a_run();
    let (archive, plains) = rar5_archive(&[
        (b"first", false, Ok(&[&first]), None),
        (b"bad\\name", true, Ok(&[&second]), None),
        (b"third", true, Ok(&[&third]), None),
    ]);
    let mut fs = open(&MemoryAttachment::new(&archive, false, b"solid.rar"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"first".to_vec(), b"third".to_vec()]);
    let third = walk(&mut fs, &[b"third"]);
    assert_eq!(contents(&mut fs, third), plains[2]);
}

/// A RAR 5 `vint` at the head of `bytes`, and its length.
fn read_vint(bytes: &[u8]) -> (u64, usize) {
    let mut value = 0;
    for (at, &byte) in bytes.iter().enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * at);
        if byte & 0x80 == 0 {
            return (value, at + 1);
        }
    }
    std::panic!("an unended vint")
}

/// `archive` with the header holding `name` changed by `patch`, its CRC made
/// to hold again: a header damaged yet whole.
fn patched_header(archive: &[u8], name: &[u8], patch: impl FnOnce(&mut [u8], usize)) -> Vec<u8> {
    let mut out = archive.to_vec();
    let mut pos = 8;
    loop {
        let (size, size_len) = read_vint(&out[pos + 4..]);
        let size = usize::try_from(size).expect("a header's size");
        let header = pos + 4 + size_len..pos + 4 + size_len + size;
        if let Some(at) = out[header.clone()]
            .windows(name.len())
            .position(|w| w == name)
        {
            patch(&mut out[header.clone()], at);
            let crc = crc(&out[pos + 4..header.end]);
            out[pos..pos + 4].copy_from_slice(&crc.to_le_bytes());
            return out;
        }
        // The type, the flags, then the data's size where they say it has
        // data.
        let fields = &out[header.clone()];
        let (_, kind_len) = read_vint(fields);
        let (flags, flags_len) = read_vint(&fields[kind_len..]);
        let mut at = kind_len + flags_len;
        if flags & 1 != 0 {
            at += read_vint(&fields[at..]).1;
        }
        let data = if flags & 2 != 0 {
            read_vint(&fields[at..]).0
        } else {
            0
        };
        pos = header.end + usize::try_from(data).expect("a member's size");
    }
}

#[test]
fn a_member_whose_header_cannot_be_read_refuses_the_members_carrying_on_from_it() {
    let [first, second, third] = three_in_a_run();
    let (archive, _) = rar5_archive(&[
        (b"first", false, Ok(&[&first]), None),
        (b"second", true, Ok(&[&second]), None),
        (b"third", true, Ok(&[&third]), None),
    ]);
    // Its name's length now runs past its header.
    let archive = patched_header(&archive, b"second", |header, name| header[name - 1] = 0x7F);
    let attachment = MemoryAttachment::new(&archive, false, b"solid.rar");
    let regions = crate::region::Regions::new(attachment.access(), archive.len() as u64);
    let reader = super::read(&regions).unwrap().rar.unwrap();
    // Its bytes keep their place, so the third is known to carry on from
    // what cannot be decoded, never from the first.
    assert_eq!(reader.entries.len(), 3);
    assert_eq!(reader.entries[2].refused, Some(DriverError::MediumError));
    let mut fs = open(&attachment);
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"first".to_vec(), b"third".to_vec()]);
    let third = walk(&mut fs, &[b"third"]);
    assert_eq!(
        fs.read_at(third, 0, &mut [0u8; 16]),
        Err(DriverError::MediumError)
    );
    let first = walk(&mut fs, &[b"first"]);
    assert_eq!(contents(&mut fs, first), b"abcdefgh");
}

#[test]
fn a_rar_4_member_named_as_no_volume_serves_keeps_its_place_in_its_solid_run() {
    let mut archive = fixture!("rarfile/rar3-solid.rar").to_vec();
    let name = archive
        .windows(10)
        .position(|w| w == b"stest1.txt")
        .unwrap();
    // Its header: CRC-16, type, flags, size, then the fields before its name.
    let header = name - 32;
    let flags = u16::from_le_bytes([archive[header + 3], archive[header + 4]]) | 0x200;
    archive[header + 3..header + 5].copy_from_slice(&flags.to_le_bytes());
    // Flagged Unicode, with no code-page spelling before it, and no UTF-8.
    archive[name] = 0xFF;
    let size = usize::from(u16::from_le_bytes([
        archive[header + 5],
        archive[header + 6],
    ]));
    let sum = u16::try_from(crc(&archive[header + 2..header + size]) & 0xFFFF).unwrap();
    archive[header..header + 2].copy_from_slice(&sum.to_le_bytes());
    let mut fs = open(&MemoryAttachment::new(&archive, false, b"solid.rar"));
    let root = fs.root();
    assert_eq!(names(&mut fs, root), [b"stest2.txt".to_vec()]);
    let second = walk(&mut fs, &[b"stest2.txt"]);
    assert_eq!(crc(&contents(&mut fs, second)), 0xc5b7_e6a2);
}
