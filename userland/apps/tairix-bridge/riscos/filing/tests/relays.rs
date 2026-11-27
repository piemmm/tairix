//! Relays that park: a program's filing SWIs over HostFS come to the same —
//! every register, every byte read, every file left — whether each relay
//! that may park is left pending and its SWI performed again, or every relay
//! is answered at once.

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, disc_file, session, Setup, DISC};
use tairix_riscos_kernel::os::{Ended, Outcome};
use tairix_riscos_kernel::swi::os::{EXIT, FILE, FIND, GBPB};
use tairix_riscos_kernel::swi::X;

/// Where the program is loaded and what it keeps.
const BASE: u32 = 0x8000;
const NAMES: u32 = BASE + 0x1000;
const LISTED: u32 = BASE + 0x2000;
const READ_INTO: u32 = BASE + 0x2400;
const LOADED: u32 = BASE + 0x2800;
const RESULTS: u32 = BASE + 0x3000;
/// The bytes of the program's image: its code, then its names.
const IMAGE_BYTES: usize = 0x1200;
/// The registers kept after each SWI: R0 to R7 and the PSR.
const KEPT: u32 = 9;

/// The names the program uses, each at its offset from [`NAMES`].
const FILE_A: (u32, &[u8]) = (0, b"HostFS::Disc.$.a\0");
const MISSING: (u32, &[u8]) = (0x40, b"HostFS::Disc.$.missing\0");
const ROOT: (u32, &[u8]) = (0x80, b"HostFS::Disc.$\0");
const FILE_B: (u32, &[u8]) = (0xC0, b"HostFS::Disc.$.b\0");
const FILE_C: (u32, &[u8]) = (0x100, b"HostFS::Disc.$.c\0");

/// ARM code run from [`BASE`].
struct Program(Vec<u32>);

impl Program {
    /// `value` in `rd`: a `MOV`, then an `ORR` for each further byte.
    fn set(&mut self, rd: u32, value: u32) {
        let bytes = value.to_le_bytes();
        self.0.push(0xE3A0_0000 | rd << 12 | u32::from(bytes[0]));
        for (k, &byte) in (1u32..).zip(&bytes[1..]) {
            if byte != 0 {
                let rotate = 16 - 4 * k;
                self.0
                    .push(0xE380_0000 | rd << 16 | rd << 12 | rotate << 8 | u32::from(byte));
            }
        }
    }

    /// R0 onwards set to `registers`.
    fn registers(&mut self, registers: &[u32]) {
        for (rd, &value) in (0..).zip(registers) {
            self.set(rd, value);
        }
    }

    /// `SWI number`, then R0 to R7 and the PSR kept at R12, which moves on.
    fn swi(&mut self, number: u32) {
        self.0.push(0xEF00_0000 | number);
        self.0.push(0xE10F_8000); // MRS R8, CPSR
        self.0.push(0xE8AC_01FF); // STMIA R12!, {R0-R8}
    }

    /// `MOV rd, rm`.
    fn copy(&mut self, rd: u32, rm: u32) {
        self.0.push(0xE1A0_0000 | rd << 12 | rm);
    }

    /// The image: the code, then the names.
    fn image(&self) -> Vec<u8> {
        let mut image = vec![0; IMAGE_BYTES];
        for (at, word) in (0..).step_by(4).zip(&self.0) {
            image[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        for (offset, name) in [FILE_A, MISSING, ROOT, FILE_B, FILE_C] {
            let at = (NAMES - BASE + offset) as usize;
            image[at..at + name.len()].copy_from_slice(name);
        }
        image
    }
}

const fn name(of: (u32, &[u8])) -> u32 {
    NAMES + of.0
}

/// The program: information read, a directory listed, a file read through
/// an open handle, another loaded, a third saved and read back, and the
/// directory listed again.
fn program() -> (Program, u32) {
    let mut p = Program(Vec::new());
    p.set(12, RESULTS);
    let swis = [
        (FILE, vec![5, name(FILE_A)]),
        (FILE, vec![5, name(MISSING)]),
        (GBPB, vec![10, name(ROOT), LISTED, 8, 0, 0x200, 0]),
    ];
    let mut count = 0;
    for (number, registers) in swis {
        p.registers(&registers);
        p.swi(X | number);
        count += 1;
    }
    p.registers(&[0x40, name(FILE_A)]);
    p.swi(X | FIND);
    p.copy(1, 0);
    p.set(0, 4);
    p.set(2, READ_INTO);
    p.set(3, 0x40);
    p.swi(X | GBPB);
    p.set(0, 0);
    p.swi(X | FIND);
    count += 3;
    for (number, registers) in [
        (FILE, vec![255, name(FILE_B), LOADED, 0]),
        (
            FILE,
            vec![10, name(FILE_C), 0xFFD, 0, LISTED, LISTED + 0x30],
        ),
        (FILE, vec![5, name(FILE_C)]),
        (GBPB, vec![9, name(ROOT), LISTED + 0x100, 8, 0, 0x100, 0]),
    ] {
        p.registers(&registers);
        p.swi(X | number);
        count += 1;
    }
    p.registers(&[0, 0, 0]);
    p.0.push(0xEF00_0000 | EXIT);
    (p, count)
}

/// What a run left: each SWI's registers, what it read and loaded, the
/// directory as listed, and the file it saved.
#[derive(Debug, PartialEq, Eq)]
struct Left {
    results: Vec<u8>,
    listed: Vec<u8>,
    read: Vec<u8>,
    loaded: Vec<u8>,
    saved: Option<Vec<u8>>,
}

/// The program run over a fresh disc, its parkable relays left pending
/// where `park`: what it left, and how many relays it parked on.
fn run(park: bool) -> (Left, u32) {
    let (program, count) = program();
    let a: Vec<u8> = (0..100u8).collect();
    let b: Vec<u8> = (0..=250u8).cycle().take(300).collect();
    let tree = Tree::new()
        .dir(DISC)
        .file(&format!("{DISC}/a,fff"), &a)
        .file(&format!("{DISC}/b,ffd"), &b)
        .file(&format!("{DISC}/prog,ff8"), &program.image());
    let mut session = session(&Setup {
        vdu: None,
        modules: &tairix_riscos_filing::MODULES,
        bytes: 0x10_0000,
        keys: b"",
        disc: Some(&tree),
    });
    let started = command(&mut session.kernel.os(), "Run HostFS::Disc.$.prog");
    assert!(started.is_err(), "the program's start left the command");
    session.park_relays(park);
    assert_eq!(
        session.run(1 << 24),
        Outcome::Ended(Ended::Exit { code: 0 })
    );
    let mut os = session.kernel.os();
    let left = Left {
        results: os
            .read_vec(RESULTS, (count * KEPT * 4) as usize)
            .expect("its results"),
        listed: os.read_vec(LISTED, 0x400).expect("listed"),
        read: os.read_vec(READ_INTO, 0x40).expect("read"),
        loaded: os.read_vec(LOADED, 300).expect("loaded"),
        saved: disc_file(&tree, "c"),
    };
    (left, session.relays_parked())
}

/// Register `n` as SWI `swi` of the program left it.
fn register(left: &Left, swi: u32, n: u32) -> u32 {
    let at = ((swi * KEPT + n) * 4) as usize;
    u32::from_le_bytes(left.results[at..at + 4].try_into().expect("a word"))
}

#[test]
fn a_program_s_filing_comes_to_the_same_whether_its_relays_park_or_not() {
    const V: u32 = 1 << 28;
    let (at_once, none) = run(false);
    assert_eq!(none, 0);
    for swi in 0..10 {
        assert_eq!(register(&at_once, swi, 8) & V, 0, "SWI {swi} succeeded");
    }
    assert_eq!(
        (register(&at_once, 0, 0), register(&at_once, 0, 4)),
        (1, 100)
    );
    assert_eq!(register(&at_once, 1, 0), 0, "nothing of that name");
    assert_eq!(register(&at_once, 2, 3), 3, "three entries listed");
    assert_eq!(register(&at_once, 4, 3), 0, "all read");
    assert_eq!(at_once.read, (0..0x40u8).collect::<Vec<_>>());
    assert_eq!(
        at_once.loaded,
        (0..=250u8).cycle().take(300).collect::<Vec<_>>()
    );
    assert_eq!(at_once.saved.as_deref(), Some(&at_once.listed[..0x30]));
    assert_eq!(
        (register(&at_once, 8, 0), register(&at_once, 8, 4)),
        (1, 0x30)
    );
    assert_eq!(register(&at_once, 9, 3), 4, "the saved file listed too");
    let (parked, times) = run(true);
    // Every SWI but the close reads before it changes anything.
    assert!(times >= 9, "its SWIs parked {times} times");
    assert_eq!(parked, at_once);
}
