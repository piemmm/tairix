//! Deterministic fuzz harness for devices and pipes as filing systems:
//! DeviceFS's SWIs on hostile device lists, validation strings, handles and
//! characters, for a driver that answers every entry with nothing; and every
//! device and pipe opened, read, written, asked about, saved, loaded,
//! deleted and listed by hostile names, special fields and handles,
//! `*PipeCopy` among them, `printer:` routed into a pipe and into itself.
//!
//! The invariants:
//!
//! * nothing panics, and every refusal is a value; a call that waits for
//!   what nothing will give is stopped by its host, and ends the run;
//! * the output streams `OS_Byte` 3 selects are as they were after every
//!   call, whatever `vdu:` wrote;
//! * the drivers' code, which no call is given to write, is as it was after
//!   every call;
//! * every file closed and the three modules re-initialised, the RMA holds
//!   no more than it did before the run.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::{application_slot, SLOT_BYTES};
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Ending, Fail, Host, Kernel, Os, Pulse, Regs, Unwind};
use tairix_riscos_kernel::swi::device_fs::{
    CALL_DEVICE, DEREGISTER, DEREGISTER_OBJECTS, RECEIVED_CHARACTER, REGISTER, REGISTER_OBJECTS,
    THRESHOLD, TRANSMIT_CHARACTER,
};
use tairix_riscos_kernel::swi::os::{
    ARGS, BGET, BPUT, BYTE, CLI, FILE, FIND, GBPB, MODULE, READ_DYNAMIC_AREA, SET_VAR_VAL,
};
use tairix_riscos_kernel::swi::X;

/// Runs a plain `cargo test` makes of the sweep.
const SMOKE_RUNS: u64 = 24;
/// Calls in one run.
const STEPS: usize = 80;

/// Where the run's scratch block keeps a device list, a validation string,
/// a name, and the bytes moved; and the driver's code, which returns.
const LIST_AT: u32 = 0;
const VALIDATION_AT: u32 = 0x200;
const NAME_AT: u32 = 0x400;
const DATA_AT: u32 = 0x800;
const DATA_BYTES: u32 = 0x400;
const CODE_AT: u32 = 0xC00;
const SCRATCH: u32 = 0x1000;
/// `MOV PC, R14`.
const RETURN: u32 = 0xE1A0_F00E;

/// The names hostile opens and calls are made on.
const NAMES: [&[u8]; 22] = [
    b"devices:$.Prb",
    b"devices:$.prb",
    b"devices#baud9600;data7:$.Prb",
    b"devices#buffer3:$.Two",
    b"devices#buffer:$.Prb",
    b"devices:$",
    b"devices:$.None",
    b"null:",
    b"null:x",
    b"vdu:",
    b"rawvdu:",
    b"kbd:",
    b"rawkbd:",
    b"printer:",
    b"printer#1:",
    b"printer#2:",
    b"printer#bad:",
    b"printer#999:",
    b"pipe:$.P",
    b"pipe:$.D.E",
    b"pipe:$.D",
    b"pipe:$",
];

/// The validation strings a driver registers with.
const VALIDATIONS: [&[u8]; 7] = [
    b"",
    b"baud,b/Ndata/N",
    b"x/N/N",
    b"/",
    b"a,b,c/S",
    b"baud/N,data/N",
    b"\x01\x02/N",
];

/// The host's words a call may spend before it is stopped: work without end
/// — a pipe copied into itself — stopped as its host stops it. The kernel
/// asks for a word only each slice of work, so two are a generous call's.
const WORDS: u32 = 2;

/// A host still at the session's start, with no input, which stops a wait
/// and stops work once its words are spent.
struct Budgeted {
    words: Rc<Cell<u32>>,
}

impl Host for Budgeted {
    fn monotonic(&mut self) -> u64 {
        0
    }

    fn real_time(&mut self) -> u64 {
        0
    }

    fn pulse(&mut self) -> Pulse {
        match self.words.get() {
            0 => Pulse::Stop,
            left => {
                self.words.set(left - 1);
                Pulse::Continue
            }
        }
    }

    fn input(&mut self) -> Option<InputEvent> {
        None
    }

    fn relay(&mut self, _request: &[u8], _reply: &mut Vec<u8>) -> bool {
        false
    }

    fn font(&mut self, _request: &[u8], _reply: &mut Vec<u8>) -> bool {
        false
    }

    fn wait(&mut self, _: Option<u64>, _: Ending) -> Pulse {
        Pulse::Stop
    }
}

/// What a call came to.
enum Outcome {
    Done(Regs),
    Refused,
    /// A pointer it was given aborted, taken as the guest's exception.
    Aborted,
    /// A wait for what nothing will give, stopped by the host.
    Stopped,
}

fn attempt(os: &mut Os<'_>, swi: u32, args: &[u32]) -> Outcome {
    let mut regs = Regs::default();
    regs.r[..args.len()].copy_from_slice(args);
    match os.perform(X | swi, &mut regs) {
        Ok(()) => Outcome::Done(regs),
        Err(Fail::Error(block)) => {
            let number = os.read_word(block);
            assert!(number.is_ok(), "an error block that reads");
            Outcome::Refused
        }
        Err(Fail::Unwind(Unwind::Stopped)) => Outcome::Stopped,
        Err(Fail::Unwind(Unwind::Transfer)) => Outcome::Aborted,
        Err(Fail::Unwind(unwind)) => {
            let line = if swi == CLI {
                os.read_zstring(args[0], 255).unwrap_or_default()
            } else {
                Vec::new()
            };
            panic!(
                "SWI {swi:#X} {args:X?} {} unwound: {unwind:?}",
                String::from_utf8_lossy(&line)
            )
        }
    }
}

/// The output streams `OS_Byte` 3 selects.
fn streams(os: &mut Os<'_>) -> u32 {
    match attempt(os, BYTE, &[236, 0, 0xFF]) {
        Outcome::Done(regs) => regs.r[1],
        _ => panic!("OS_Byte 236 answered"),
    }
}

/// The RMA's bytes in use.
fn rma_used(os: &mut Os<'_>) -> u32 {
    let (Outcome::Done(heap), Outcome::Done(area)) = (
        attempt(os, MODULE, &[5]),
        attempt(os, READ_DYNAMIC_AREA, &[1]),
    ) else {
        panic!("the RMA described");
    };
    area.r[1] - heap.r[3]
}

/// `bytes` and a terminator at `at`.
fn put(os: &mut Os<'_>, at: u32, bytes: &[u8]) {
    os.write_terminated(at, bytes, 0, bytes.len() + 1)
        .expect("scratch");
}

/// A device list at `at` of up to three entries, each name offset from its
/// own entry as DeviceFS reads one, or now and then from anywhere.
fn device_list(os: &mut Os<'_>, rng: &mut Prng, at: u32) {
    let names: [&[u8]; 4] = [b"Prb", b"Two", b"prb", b"A.B"];
    let count = rng.below(4);
    let mut name_at = at + 4 * 7 * 3 + 4;
    for entry in 0..count {
        let entry_at = at + 4 * 7 * u32::try_from(entry).expect("few");
        let name = names[rng.below(names.len())];
        put(os, name_at, name);
        let offset = if rng.below(8) == 0 {
            rng.next_u32()
        } else {
            name_at - entry_at
        };
        let flags = u32::try_from(rng.below(4)).expect("small");
        let size = [0, 16, 32, 1024][rng.below(4)];
        let words = [
            offset,
            flags,
            rng.next_u32() & 0xF,
            size,
            rng.next_u32() & 0xF,
            size,
            0,
        ];
        os.write_words(entry_at, &words).expect("scratch");
        name_at += 8;
    }
    let end = at + 4 * 7 * u32::try_from(count).expect("few");
    os.write_word(end, 0).expect("scratch");
}

/// `DeviceFS_Register` of a random device list, validation and limits, the
/// driver kept in `drivers` where it registered.
fn register(os: &mut Os<'_>, rng: &mut Prng, scratch: u32, drivers: &mut Vec<u32>) -> Outcome {
    device_list(os, rng, scratch + LIST_AT);
    put(
        os,
        scratch + VALIDATION_AT,
        VALIDATIONS[rng.below(VALIDATIONS.len())],
    );
    let validation = if rng.below(4) == 0 {
        0
    } else {
        scratch + VALIDATION_AT
    };
    let flags = u32::try_from(rng.below(8)).expect("small");
    let most = [0, 1, 2, u32::MAX][rng.below(4)];
    let outcome = attempt(
        os,
        REGISTER,
        &[
            flags,
            scratch + LIST_AT,
            scratch + CODE_AT,
            0,
            0,
            validation,
            most,
            most,
        ],
    );
    if let Outcome::Done(regs) = &outcome {
        drivers.push(regs.r[0]);
    }
    outcome
}

/// `OS_File` 255 on the name at `named`. A load writes the file's whole
/// length, so it loads into the application slot, where nothing of the run
/// lies and a file longer than the slot aborts at its end.
fn load(os: &mut Os<'_>, named: u32) -> Outcome {
    attempt(os, FILE, &[255, named, APPLICATION, 0])
}

/// One call of a random kind on the session, `scratch` its block, the kind
/// and name kept in `trail` for a failure to report.
fn act(
    os: &mut Os<'_>,
    rng: &mut Prng,
    scratch: u32,
    drivers: &mut Vec<u32>,
    handles: &mut Vec<u32>,
    trail: &mut Vec<String>,
) -> Outcome {
    let name = NAMES[rng.below(NAMES.len())];
    put(os, scratch + NAME_AT, name);
    let named = scratch + NAME_AT;
    let data = scratch + DATA_AT;
    let count = [0, 1, 7, 255, DATA_BYTES][rng.below(5)];
    let handle = handles
        .get(rng.below(handles.len().max(1)))
        .copied()
        .unwrap_or_else(|| rng.next_u32() & 0xFF);
    let driver = drivers
        .get(rng.below(drivers.len().max(1)))
        .copied()
        .unwrap_or_else(|| rng.next_u32());
    let kind = rng.below(20);
    trail.push(format!("{kind} {}", String::from_utf8_lossy(name)));
    match kind {
        0 => register(os, rng, scratch, drivers),
        1 => {
            drivers.retain(|&held| held != driver);
            attempt(os, DEREGISTER, &[driver])
        }
        2 => {
            device_list(os, rng, scratch + LIST_AT);
            attempt(os, REGISTER_OBJECTS, &[driver, scratch + LIST_AT])
        }
        3 => {
            put(os, named, [&b"Prb"[..], b"Two", b"None"][rng.below(3)]);
            attempt(os, DEREGISTER_OBJECTS, &[driver, named])
        }
        4 => {
            let target = [driver, named, 0][rng.below(3)];
            put(os, named, b"$.Prb");
            attempt(
                os,
                CALL_DEVICE,
                &[rng.next_u32() & 0xF, target, 1, 2, 3, 4, 5, 6],
            )
        }
        5 => attempt(os, THRESHOLD, &[rng.next_u32(), rng.next_u32() & 0x3F]),
        6 => attempt(
            os,
            RECEIVED_CHARACTER,
            &[rng.next_u32() & 0xFF, rng.next_u32()],
        ),
        7 => attempt(os, TRANSMIT_CHARACTER, &[0, rng.next_u32()]),
        8 => {
            let mode = [0x40, 0x4F, 0x80, 0x8F, 0xC0, 0xCF][rng.below(6)];
            let outcome = attempt(os, FIND, &[mode, named]);
            if let Outcome::Done(regs) = &outcome {
                if regs.r[0] != 0 {
                    handles.push(regs.r[0]);
                }
            }
            outcome
        }
        9 => {
            handles.retain(|&open| open != handle);
            attempt(os, FIND, &[0, handle])
        }
        10 => attempt(os, BGET, &[0, handle]),
        11 => attempt(os, BPUT, &[rng.next_u32() & 0xFF, handle]),
        12 => attempt(os, GBPB, &[[2, 4][rng.below(2)], handle, data, count]),
        13 => {
            let reason = [0, 1, 2, 3, 4, 5, 6, 7, 254][rng.below(9)];
            attempt(os, ARGS, &[reason, handle, count])
        }
        14 => attempt(os, FILE, &[[5, 6, 8, 17][rng.below(4)], named]),
        15 => attempt(os, FILE, &[10, named, 0xFFF, 0, data, data + count]),
        16 => attempt(
            os,
            FILE,
            &[[1, 2, 3, 4, 18][rng.below(5)], named, 0x1234, 0x5678, 0, 3],
        ),
        17 => load(os, named),
        18 => attempt(os, GBPB, &[10, named, data, 8, 0, DATA_BYTES, 0]),
        _ => {
            let from = NAMES[rng.below(NAMES.len())];
            let to = NAMES[rng.below(NAMES.len())];
            let line = [&b"PipeCopy "[..], from, b" ", to].concat();
            put(os, named, &line);
            attempt(os, CLI, &[named])
        }
    }
}

/// A session booted for a run, with an application slot, and the words its
/// host allows a call.
fn booted() -> (Kernel, Rc<Cell<u32>>) {
    let words = Rc::new(Cell::new(u32::MAX));
    let mut kernel = common::kernel_on(Box::new(Budgeted {
        words: Rc::clone(&words),
    }));
    application_slot(&mut kernel, SLOT_BYTES);
    (kernel, words)
}

/// The run's scratch block with its drivers' code written, `printer:`
/// routed into a pipe and into itself, and opens on names no device answers
/// refused.
fn prepare(os: &mut Os<'_>) -> u32 {
    let scratch = os.rma_claim(SCRATCH).expect("scratch");
    os.write_word(scratch + CODE_AT, RETURN).expect("scratch");
    for (variable, value) in [
        (&b"PrinterType$1"[..], &b"pipe:$.Printed"[..]),
        (b"PrinterType$2", b"printer#2:"),
    ] {
        put(os, scratch + NAME_AT, variable);
        put(os, scratch + VALIDATION_AT, value);
        let length = u32::try_from(value.len()).expect("short");
        let set = attempt(
            os,
            SET_VAR_VAL,
            &[scratch + NAME_AT, scratch + VALIDATION_AT, length, 0, 0],
        );
        assert!(matches!(set, Outcome::Done(_)), "PrinterType set");
    }
    for (mode, name) in [
        (0x4F, &b"null#x:"[..]),
        (0x4F, b"printer:"),
        (0x4F, b"NoSuchFS:x"),
    ] {
        put(os, scratch + NAME_AT, name);
        let refused = attempt(os, FIND, &[mode, scratch + NAME_AT]);
        assert!(
            matches!(refused, Outcome::Refused),
            "{}",
            String::from_utf8_lossy(name)
        );
    }
    scratch
}

/// Whether the drivers' code in `scratch` is as it was written.
fn code_whole(os: &mut Os<'_>, scratch: u32) -> bool {
    os.read_word(scratch + CODE_AT).expect("scratch") == RETURN
}

#[test]
fn hostile_device_and_pipe_calls_are_answered_and_give_back_the_rma() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_device_and_pipe_calls_are_answered_and_give_back_the_rma",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let (mut kernel, words) = booted();
        let mut os = kernel.os();
        let scratch = prepare(&mut os);
        let mut trail = Vec::new();
        assert!(settle(&mut os, scratch, &trail), "settled at the start");
        let held = rma_used(&mut os);
        let selected = streams(&mut os);
        let mut drivers = Vec::new();
        let mut handles = Vec::new();
        let mut stopped = false;
        for _ in 0..STEPS {
            words.set(WORDS);
            let outcome = act(
                &mut os,
                &mut rng,
                scratch,
                &mut drivers,
                &mut handles,
                &mut trail,
            );
            words.set(u32::MAX);
            if let Some(last) = trail.last_mut() {
                last.push_str(match outcome {
                    Outcome::Done(_) => "",
                    Outcome::Refused => " refused",
                    Outcome::Aborted => " aborted",
                    Outcome::Stopped => " stopped",
                });
            }
            assert!(
                code_whole(&mut os, scratch),
                "the drivers' code as it was after {trail:?}"
            );
            stopped = matches!(outcome, Outcome::Stopped);
            if stopped {
                break;
            }
            assert_eq!(
                streams(&mut os),
                selected,
                "OS_Byte 3 as it was after {trail:?}"
            );
        }
        if !stopped && settle(&mut os, scratch, &trail) {
            let used = rma_used(&mut os);
            assert!(
                used <= held,
                "the RMA given back: {used} of {held} after {trail:?}"
            );
        }
        runs += 1;
    }
}

/// Every file closed and the three modules re-initialised, as a run ends,
/// and as it begins so its count of the RMA is taken as its end's will be,
/// DeviceFS's start-up callback pending in both: whether that was done, not
/// stopped by a close that waits.
fn settle(os: &mut Os<'_>, scratch: u32, trail: &[String]) -> bool {
    for command in [
        &b"Shut"[..],
        b"RMReInit DeviceFS",
        b"RMReInit SystemDevices",
        b"RMReInit PipeFS",
    ] {
        put(os, scratch + NAME_AT, command);
        match attempt(os, CLI, &[scratch + NAME_AT]) {
            Outcome::Done(_) => {}
            // A stream on a device whose driver never takes the byte it was
            // written waits for it as it closes.
            Outcome::Stopped => return false,
            Outcome::Refused | Outcome::Aborted => panic!(
                "{} refused after {trail:?}",
                String::from_utf8_lossy(command)
            ),
        }
    }
    true
}

/// A pipe longer than a run's data block, loaded whole, writes over nothing
/// of the run, so DeviceFS still finds its drivers' code as it is
/// re-initialised.
#[test]
fn a_pipe_loaded_whole_writes_over_nothing_of_the_run() {
    let (mut kernel, _) = booted();
    let mut os = kernel.os();
    let scratch = prepare(&mut os);
    assert!(settle(&mut os, scratch, &[]), "settled at the start");
    let held = rma_used(&mut os);
    let list = scratch + LIST_AT;
    let name_offset = 4 * 8;
    os.write_words(list, &[name_offset, 0, 0, 0, 0, 0, 0, 0])
        .expect("scratch");
    put(&mut os, list + name_offset, b"Prb");
    let registered = attempt(
        &mut os,
        REGISTER,
        &[0, list, scratch + CODE_AT, 0, 0, 0, 1, 1],
    );
    assert!(matches!(registered, Outcome::Done(_)), "registered");
    let (named, data) = (scratch + NAME_AT, scratch + DATA_AT);
    put(&mut os, named, b"pipe:$.P");
    let Outcome::Done(opened) = attempt(&mut os, FIND, &[0x80, named]) else {
        panic!("the pipe opened");
    };
    let handle = opened.r[0];
    for _ in 0..2 {
        let written = attempt(&mut os, GBPB, &[2, handle, data, DATA_BYTES]);
        assert!(matches!(written, Outcome::Done(_)), "written");
    }
    let closed = attempt(&mut os, FIND, &[0, handle]);
    assert!(matches!(closed, Outcome::Done(_)), "closed");
    assert!(matches!(load(&mut os, named), Outcome::Done(_)), "loaded");
    assert!(code_whole(&mut os, scratch), "the drivers' code as it was");
    assert!(settle(&mut os, scratch, &[]), "settled");
    assert!(rma_used(&mut os) <= held, "the RMA given back");
}
