//! Deterministic fuzz harness for the kernel's international surfaces:
//! message files and ResourceFS blocks read through MessageTrans, the
//! Territory Manager's conversions, collation and symbols, and key sequences
//! through the key handler in every keyboard's layout and alphabet — all
//! with hostile inputs, under a host that stops endless work.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * nothing is written past the room a call was given;
//! * the kernel still answers a SWI.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

#[path = "support/boot.rs"]
mod boot;
#[path = "support/still.rs"]
mod still;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Kernel, Regs};
use tairix_riscos_kernel::swi::chunk::{MESSAGE_TRANS, TERRITORY};
use tairix_riscos_kernel::swi::os::{
    BYTE, CALL_A_VECTOR, INSTALL_KEY_HANDLER, READ_MONOTONIC_TIME,
};
use tairix_riscos_kernel::swi::resource_fs::{DEREGISTER_FILES, REGISTER_FILES};
use tairix_riscos_kernel::swi::X;
use tairix_riscos_kernel::vectors;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 12;
/// Calls in one run.
const STEPS: usize = 60;
/// The instructions a run of the guest is given.
const BUDGET: u64 = 20_000;

/// Where a run keeps a message file, a ResourceFS block, a descriptor, the
/// strings it passes, and the room it gives a call to write in.
const FILE: u32 = APPLICATION + 0x100;
const FILE_MOST: usize = 0xE00;
const BLOCK: u32 = APPLICATION + 0x1000;
const BLOCK_MOST: usize = 0xF00;
const DESCRIPTOR: u32 = APPLICATION + 0x2000;
const STRINGS: u32 = APPLICATION + 0x2100;
const STRING_ROOM: u32 = 0x100;
const OUT: u32 = APPLICATION + 0x3000;
const OUT_ROOM: u32 = 0x800;
/// What fills the room past a call's, to show it was not written.
const CANARY: u8 = 0xA5;
const CANARY_LEN: u32 = 64;

/// Pieces message files are made of.
const TOKENS: [&[u8]; 10] = [
    b"Fuzz",
    b"F?zz",
    b"F*",
    b"Err/Er2",
    b"Menu",
    b"#comment",
    b"T0",
    b"T1:",
    b"",
    b"Dup",
];
const VALUES: [&[u8]; 9] = [
    b"Value %0 and %1",
    b"%%",
    b"%",
    b"\x1b\x1bx",
    b"|M",
    b"plain",
    b"%3%2%1%0",
    b"",
    b"\x01\x02",
];
const NAMES: [&[u8]; 6] = [
    b"Resources:$.Fuzz.Messages\0",
    b"Resources:$.Fuzz\0",
    b"Resources:$.Nope\0",
    b"\0",
    b"Resources:$.Resources.Kernel.Messages\0",
    b"Resources:$.Fuzz.Messages.Deeper\0",
];
/// Pieces of dates and formats.
const DATES: [&[u8]; 8] = [
    b"1 Jan 2024",
    b"31/12/69",
    b"29 Feb 2100",
    b"00:00:00",
    b"Dec",
    b"99999999999 9 9",
    b"",
    b"12:61:61 32/13/1900",
];
const FORMATS: [&[u8]; 10] = [
    b"%DY %MO %CE%YR %24:%MI:%SE",
    b"%%",
    b"%",
    b"%Z",
    b"%W3 %M3 %DN %ST %WK %AM%PM %12 %CS %0",
    b"%TZ",
    b"%WN %MN",
    b"%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR%YR",
    b"",
    b"%z%Z%zMI%",
];

/// A hostile register: small, an application-space address, or anything.
fn hostile(rng: &mut Prng) -> u32 {
    let in_application = APPLICATION + u32::try_from(rng.below(0x8000)).unwrap_or(0);
    let anything = rng.next_u32();
    *rng.pick(&[
        0,
        1,
        u32::MAX,
        0x8000_0000,
        in_application,
        in_application | 3,
        anything,
        0xFFFF_FFFC,
    ])
}

/// R0 onwards from `values`, the rest nought.
fn registers(values: &[u32]) -> [u32; 13] {
    let mut registers = [0; 13];
    registers[..values.len()].copy_from_slice(values);
    registers
}

/// Performs SWI `number` with `registers`, checking every refusal reads.
fn swi(kernel: &mut Kernel, number: u32, registers: [u32; 13]) -> Option<Regs> {
    let mut regs = Regs::new(registers);
    match kernel.os().perform(X | number, &mut regs) {
        Ok(()) => Some(regs),
        Err(Fail::Error(block)) => {
            let _ = kernel.os().read_word(block);
            None
        }
        Err(Fail::Unwind(_)) => None,
    }
}

/// The room past `size` bytes of the output area filled with the canary.
fn guard(kernel: &mut Kernel) {
    let _ = kernel
        .os()
        .write_bytes(OUT, &[CANARY; (OUT_ROOM + CANARY_LEN) as usize]);
}

/// Nothing written past `size` bytes of the output area.
fn guarded(kernel: &mut Kernel, size: u32, what: &str) {
    let mut past = Vec::new();
    for at in OUT + size..OUT + size + CANARY_LEN {
        past.push(kernel.os().read_byte(at).unwrap_or(0));
    }
    assert!(
        past.iter().all(|&b| b == CANARY),
        "{what} wrote past its {size} bytes"
    );
}

/// A string written at `STRINGS` with `slot`: its address.
fn string(kernel: &mut Kernel, slot: u32, bytes: &[u8]) -> u32 {
    let at = STRINGS + STRING_ROOM * slot;
    let take = bytes.len().min(STRING_ROOM as usize - 1);
    let _ = kernel.os().write_bytes(at, &bytes[..take]);
    let _ = kernel
        .os()
        .write_byte(at + u32::try_from(take).unwrap_or(0), 0);
    at
}

/// A random piece of `pieces`, or random bytes now and then.
fn piece(rng: &mut Prng, pieces: &[&[u8]]) -> Vec<u8> {
    if rng.below(8) == 0 {
        let mut bytes = vec![0u8; rng.below(40)];
        rng.fill(&mut bytes);
        bytes
    } else {
        rng.pick(pieces).to_vec()
    }
}

/// A message file: lines of tokens and values, some odd, now and then bytes.
fn message_file(rng: &mut Prng) -> Vec<u8> {
    let mut text = Vec::new();
    for _ in 0..rng.below(24) {
        text.extend(piece(rng, &TOKENS));
        if rng.below(6) != 0 {
            text.push(b':');
        }
        text.extend(piece(rng, &VALUES));
        text.push(*rng.pick(&[b'\n', b'\n', b'\r', 0, b'\n']));
    }
    text.truncate(FILE_MOST);
    text
}

/// A ResourceFS block holding `files`, each a name and data, now and then
/// with a word made hostile.
fn resource_block(rng: &mut Prng, files: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut block = Vec::new();
    for (name, data) in files {
        let name_room = (name.len() + 1).next_multiple_of(4);
        let data_room = data.len().next_multiple_of(4);
        let entry = 20 + name_room + 4 + data_room;
        let length = u32::try_from(data.len()).unwrap_or(0);
        for word in [u32::try_from(entry).unwrap_or(0), 0xFFFF_FD00, 0, length, 3] {
            block.extend_from_slice(&word.to_le_bytes());
        }
        block.extend_from_slice(name);
        block.resize(block.len() + name_room - name.len(), 0);
        block.extend_from_slice(&(length + 4).to_le_bytes());
        block.extend_from_slice(data);
        block.resize(block.len() + data_room - data.len(), 0);
    }
    block.extend_from_slice(&0u32.to_le_bytes());
    if rng.below(3) == 0 && block.len() >= 4 {
        let word = rng.below(block.len() / 4) * 4;
        block[word..word + 4].copy_from_slice(&hostile(rng).to_le_bytes());
    }
    block.truncate(BLOCK_MOST);
    block
}

/// The kernel still answers.
fn still_answers(kernel: &mut Kernel, step: usize) {
    let mut regs = Regs::new([0; 13]);
    assert!(
        kernel
            .os()
            .perform(X | READ_MONOTONIC_TIME, &mut regs)
            .is_ok(),
        "OS_ReadMonotonicTime after step {step}"
    );
}

/// Loops `body` over runs until the budget, or the smoke sweep's count.
fn sweep(name: &str, mut body: impl FnMut(&mut Prng)) {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        body(&mut rng);
        runs += 1;
    }
}

/// One MessageTrans call with hostile tokens, parameters and descriptors,
/// held to the room it was given.
fn message_call(kernel: &mut Kernel, rng: &mut Prng) {
    let token = piece(rng, &TOKENS);
    let token = string(kernel, 0, &token);
    let parameters = [0, 1, 2, 3].map(|slot| {
        if rng.below(2) == 0 {
            0
        } else {
            let value = piece(rng, &VALUES);
            string(kernel, 1 + slot, &value)
        }
    });
    // A descriptor is the call's to write, so none is drawn in the room
    // the guard watches.
    let anywhere =
        Some(hostile(rng)).filter(|at| !(OUT..OUT + OUT_ROOM + CANARY_LEN + 16).contains(at));
    let descriptor = *rng.pick(&[DESCRIPTOR, DESCRIPTOR, 0, anywhere.unwrap_or(DESCRIPTOR)]);
    let size = u32::try_from(rng.below(OUT_ROOM as usize)).unwrap_or(0);
    let buffer = if rng.below(6) == 0 { 0 } else { OUT };
    guard(kernel);
    let (reason, regs) = match rng.below(9) {
        0 => {
            let name = *rng.pick(&NAMES);
            let name = string(kernel, 5, name);
            let into = if rng.below(3) == 0 { OUT } else { 0 };
            (1, registers(&[DESCRIPTOR, name, into]))
        }
        1 => {
            let name = *rng.pick(&NAMES);
            let name = string(kernel, 5, name);
            (0, registers(&[0, name]))
        }
        2 => (
            2,
            registers(&[
                descriptor,
                token,
                buffer,
                size,
                parameters[0],
                parameters[1],
                parameters[2],
                parameters[3],
            ]),
        ),
        3 => (
            7,
            registers(&[
                descriptor,
                token,
                buffer,
                size,
                parameters[0],
                parameters[1],
                parameters[2],
                parameters[3],
            ]),
        ),
        4 => {
            let mut error = rng.next_u32().to_le_bytes().to_vec();
            error.extend(piece(rng, &TOKENS));
            let error = string(kernel, 6, &error);
            (
                6,
                registers(&[
                    error,
                    descriptor,
                    buffer,
                    size,
                    parameters[0],
                    parameters[1],
                    parameters[2],
                    parameters[3],
                ]),
            )
        }
        5 => (
            5,
            registers(&[
                descriptor,
                token,
                buffer,
                size,
                u32::try_from(rng.below(30)).unwrap_or(0),
            ]),
        ),
        6 => (3, registers(&[descriptor, token, OUT, size])),
        7 => (4, registers(&[descriptor])),
        _ => (9, [0; 13]),
    };
    let _ = swi(kernel, MESSAGE_TRANS + reason, regs);
    // An opened file may be put where the room is, whatever its length.
    if reason != 1 && reason != 0 && buffer != 0 {
        guarded(
            kernel,
            size,
            &format!("MessageTrans {reason} with {regs:X?}"),
        );
    }
}

#[test]
fn any_message_file_through_any_call_answers_as_a_value() {
    sweep(
        "any_message_file_through_any_call_answers_as_a_value",
        |rng| {
            let (mut kernel, _) = still::boot(rng);
            let file = message_file(rng);
            let other = message_file(rng);
            let block = resource_block(rng, &[(b"Fuzz.Messages", &file), (b"Fuzz.Other", &other)]);
            let _ = kernel.os().write_bytes(BLOCK, &block);
            let registered = swi(&mut kernel, REGISTER_FILES, registers(&[BLOCK])).is_some();
            let _ = kernel.os().write_bytes(FILE, &file);
            for step in 0..STEPS {
                message_call(&mut kernel, rng);
                still_answers(&mut kernel, step);
            }
            let _ = swi(&mut kernel, MESSAGE_TRANS + 4, registers(&[DESCRIPTOR]));
            if registered {
                let _ = swi(&mut kernel, DEREGISTER_FILES, registers(&[BLOCK]));
            }
            still_answers(&mut kernel, STEPS);
        },
    );
}

/// One Territory Manager call with hostile strings, times and formats.
fn territory_call(kernel: &mut Kernel, rng: &mut Prng) {
    let anything = hostile(rng);
    let territory = *rng.pick(&[u32::MAX, u32::MAX, 1, 0, anything]);
    let mut time = [0u8; 8];
    rng.fill(&mut time);
    let time = string(kernel, 0, &time[..5]);
    let date = piece(rng, &DATES);
    let date = string(kernel, 1, &date);
    let format = piece(rng, &FORMATS);
    let format = string(kernel, 2, &format);
    let other = piece(rng, &DATES);
    let other = string(kernel, 3, &other);
    let size = u32::try_from(rng.below(OUT_ROOM as usize)).unwrap_or(0);
    guard(kernel);
    let (offset, regs, written) = match rng.below(14) {
        0 => (
            11,
            registers(&[territory, time, OUT, size, format]),
            Some(size),
        ),
        1 => (12, registers(&[territory, time, OUT, size]), Some(size)),
        2 => (
            16,
            registers(&[
                territory,
                u32::try_from(rng.below(5)).unwrap_or(0),
                date,
                OUT,
            ]),
            Some(36),
        ),
        3 => (15, registers(&[territory, time, OUT]), Some(36)),
        4 => {
            let ordinals = STRINGS + STRING_ROOM * 4;
            for word in 0..9 {
                let _ = kernel.os().write_word(ordinals + 4 * word, hostile(rng));
            }
            (17, registers(&[territory, OUT, ordinals]), Some(5))
        }
        5 => (
            29,
            registers(&[territory, date, other, rng.next_u32() & 7]),
            None,
        ),
        6 => (
            33,
            registers(&[
                territory,
                if rng.below(4) == 0 { 0 } else { OUT },
                date,
                size,
            ]),
            Some(size),
        ),
        7 => (
            30,
            registers(&[territory, u32::try_from(rng.below(24)).unwrap_or(0)]),
            None,
        ),
        8 => (31, registers(&[territory, time, OUT]), Some(48)),
        9 => (32, registers(&[0, date]), None),
        10 => (10, registers(&[territory, rng.next_u32()]), None),
        11 => (
            55,
            registers(&[u32::try_from(rng.below(7)).unwrap_or(0), hostile(rng)]),
            None,
        ),
        12 => (
            u32::try_from(22 + rng.below(7)).unwrap_or(22),
            registers(&[territory, hostile(rng)]),
            None,
        ),
        _ => (35, registers(&[territory, hostile(rng), OUT, size]), None),
    };
    let _ = swi(kernel, TERRITORY + offset, regs);
    if let Some(written) = written {
        guarded(
            kernel,
            written,
            &format!("Territory {offset} with {regs:X?}"),
        );
    }
}

#[test]
fn any_territory_call_with_any_strings_answers_as_a_value() {
    sweep(
        "any_territory_call_with_any_strings_answers_as_a_value",
        |rng| {
            let (mut kernel, _) = still::boot(rng);
            for step in 0..STEPS {
                territory_call(&mut kernel, rng);
                still_answers(&mut kernel, step);
            }
        },
    );
}

/// A low-level key: mostly a PC's, sometimes past them.
fn low_key(rng: &mut Prng) -> u32 {
    match rng.below(10) {
        0 => u32::try_from(0x70 + rng.below(8)).unwrap_or(0),
        1 => hostile(rng),
        2 => *rng.pick(&[0x4C, 0x58, 0x3B, 0x61, 0x5E, 0x60, 0x5D, 0x22, 0x6D]),
        _ => u32::try_from(rng.below(0x70)).unwrap_or(0),
    }
}

/// One step of typing: a key, a setting, the clock, or the buffer read.
fn key_step(kernel: &mut Kernel, rng: &mut Prng, clock: &std::cell::Cell<u64>) {
    match rng.below(16) {
        0..=7 => {
            let reason = 1 + u32::try_from(rng.below(2)).unwrap_or(0);
            let key = low_key(rng);
            let _ = swi(
                kernel,
                CALL_A_VECTOR,
                registers(&[reason, key, 0, 0, 0, 0, 0, 0, 0, vectors::KEY.0]),
            );
        }
        8 => {
            let keyboard = 0x80 | u32::try_from(rng.below(0x7F)).unwrap_or(0);
            let _ = swi(kernel, BYTE, registers(&[71, keyboard]));
        }
        9 => {
            let alphabet = u32::try_from(rng.below(0x7F)).unwrap_or(0);
            let _ = swi(kernel, BYTE, registers(&[71, alphabet]));
        }
        10 => {
            let _ = swi(
                kernel,
                BYTE,
                registers(&[202, rng.next_u32() & 0xFF, rng.next_u32() & 0xFF]),
            );
        }
        11 => {
            let which = *rng.pick(&[11, 12]);
            let _ = swi(
                kernel,
                BYTE,
                registers(&[which, u32::try_from(rng.below(40)).unwrap_or(0)]),
            );
        }
        12 => {
            clock.set(clock.get() + u64::try_from(rng.below(80)).unwrap_or(0));
            let _ = kernel.run(BUDGET);
        }
        13 => {
            for _ in 0..rng.below(20) {
                let _ = swi(kernel, BYTE, registers(&[145]));
            }
        }
        14 => {
            let anything = hostile(rng);
            let id = *rng.pick(&[1, 2, 3, 0xFF, anything]);
            let no_debounce = if rng.below(2) == 0 {
                u32::from_le_bytes(*b"NoKd")
            } else {
                0
            };
            let _ = swi(
                kernel,
                CALL_A_VECTOR,
                registers(&[0, id, no_debounce, 0, 0, 0, 0, 0, 0, vectors::KEY.0]),
            );
        }
        _ => {
            let read = u32::try_from(rng.below(2)).unwrap_or(0);
            let _ = swi(kernel, INSTALL_KEY_HANDLER, registers(&[read]));
        }
    }
}

#[test]
fn any_key_sequence_in_any_layout_answers_as_a_value() {
    sweep("any_key_sequence_in_any_layout_answers_as_a_value", |rng| {
        let (mut kernel, clock) = still::boot(rng);
        for step in 0..STEPS * 4 {
            key_step(&mut kernel, rng, &clock);
            still_answers(&mut kernel, step);
        }
    });
}
