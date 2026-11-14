//! Deterministic fuzz harness for the machine library's store blobs: the
//! record of every machine, and each machine's state.
//!
//! The store is the application's own, but what a volume gives back may be
//! damaged. The invariants:
//!
//! * decoding a record, loading a store or decoding a state never panics, for
//!   arbitrary bytes or a damaged real one;
//! * a record decodes only to whole machines — numbers non-zero, unique and
//!   below the next to be minted, names valid, no more than the application
//!   holds — and its next record decodes back to it;
//! * a load names as orphans only blobs of a machine number that the library's
//!   machines do not own, and none while the record is unreadable; and
//! * a state decodes to what was kept, its header read alone agreeing.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_machines::record::{part_blob, state_blob, MAX_NAME, RECORD_BLOBS};
use tairix_machines::state::{self, Bounds, LastScreen, Suspended};
use tairix_machines::{Found, Kept, Library};
use tairix_sandbox::wire::{Reader, WireError, Writer};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;

/// The longest label a probe keeps.
const LABEL_MAX: usize = 12;

/// A machine whose own fields are a variable-length label and the parts it
/// owns of two kinds, so a record exercises every shape an application's
/// fields take.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probe {
    id: u32,
    name: String,
    label: String,
    /// One bit for each disc it owns, then one for its tape.
    owns: u8,
}

/// A label no probe may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unlabelled;

impl Kept for Probe {
    type Invalid = Unlabelled;
    const MAGIC: u32 = u32::from_le_bytes(*b"PRBE");
    const VERSION: u16 = 3;
    const MAX_MACHINES: usize = 8;
    const MAX_RECORD: usize = 2048;
    const PARTS: &'static [(&'static str, usize)] = &[("disc", 2), ("tape", 1)];

    fn id(&self) -> u32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn put(&self, w: &mut Writer) {
        w.str(&self.label);
        w.u8(self.owns);
    }

    fn get(id: u32, name: String, r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        let label = String::from(r.str(LABEL_MAX)?);
        let owns = r.u8()?;
        Ok((owns < 8).then_some(Self {
            id,
            name,
            label,
            owns,
        }))
    }

    fn invalid(&self) -> Option<Unlabelled> {
        self.label.is_empty().then_some(Unlabelled)
    }

    fn holds(&self, word: &str, index: usize) -> bool {
        let bit = match (word, index) {
            ("disc", 0) => 1,
            ("disc", 1) => 2,
            ("tape", 0) => 4,
            _ => return false,
        };
        self.owns & bit != 0
    }
}

/// A name a machine may or may not be given: valid, empty, too long, or
/// holding a control character.
fn any_name(rng: &mut Prng) -> String {
    match rng.below(6) {
        0 => String::new(),
        1 => "x".repeat(MAX_NAME + 1),
        2 => String::from("bad\u{7}name"),
        _ => format!("Machine {}", rng.below(5)),
    }
}

/// A library of machines made, renamed and removed at random, as an
/// application's edits leave it.
fn edited_library(rng: &mut Prng) -> Library<Probe> {
    let mut library = Library::new();
    for _ in 0..rng.at_most(12) {
        if rng.below(4) == 0 {
            let held: Vec<u32> = library.machines().iter().map(Probe::id).collect();
            if let Some(&id) = held.get(rng.at_most(held.len())) {
                library.remove(id);
            }
            continue;
        }
        let Ok(id) = library.mint() else { continue };
        let label = if rng.below(8) == 0 {
            String::new()
        } else {
            "l".repeat(1 + rng.below(LABEL_MAX))
        };
        let probe = Probe {
            id,
            name: any_name(rng),
            label,
            owns: rng.next_u8() & 7,
        };
        let _ = library.add(probe);
    }
    library
}

/// Every machine `library` holds is one a record may hold.
fn check_whole(library: &Library<Probe>) {
    let machines = library.machines();
    assert!(machines.len() <= Probe::MAX_MACHINES);
    for (at, machine) in machines.iter().enumerate() {
        assert_ne!(machine.id, 0);
        assert!(machines[..at].iter().all(|held| held.id != machine.id));
        assert!(!machine.name.is_empty() && machine.name.len() <= MAX_NAME);
        assert!(!machine.name.chars().any(char::is_control));
        assert!(!machine.label.is_empty() && machine.owns < 8);
    }
}

/// A decoded record holds whole machines, and its next record reads back as
/// it.
fn check_record(bytes: &[u8]) {
    let Ok(library) = Library::<Probe>::decode(bytes) else {
        return;
    };
    check_whole(&library);
    let Ok(next) = library.record() else {
        return;
    };
    let mut expected = library;
    expected.written(next.generation);
    assert_eq!(
        Library::decode(&next.bytes),
        Ok(expected),
        "a record reads back"
    );
}

/// A blob name a store may list: one a machine owns, one spelled like it but
/// not, or noise.
fn any_blob(rng: &mut Prng) -> String {
    let id = 1 + rng.below(10);
    match rng.below(7) {
        0 => state_blob(u32::try_from(id).expect("small")),
        1 => part_blob("disc", u32::try_from(id).expect("small"), rng.below(3)),
        2 => part_blob("tape", u32::try_from(id).expect("small"), rng.below(2)),
        3 => format!("state-0{id}"),
        4 => String::from(*rng.pick(&RECORD_BLOBS)),
        5 => format!("disc-{id}"),
        _ => format!("rom.{id}"),
    }
}

/// A load of any listing names as orphans only blobs the machines it holds
/// do not own, and every state of a machine it does not hold.
fn check_load(rng: &mut Prng, copies: [Option<&[u8]>; 2]) {
    let listing: Vec<(String, u64)> = (0..rng.at_most(16))
        .map(|_| (any_blob(rng), rng.next_u64()))
        .collect();
    let loaded = Library::<Probe>::load(&listing, copies);
    check_whole(&loaded.library);
    if loaded.found == Found::Unreadable {
        assert!(loaded.orphans.is_empty(), "nothing is swept on a guess");
    }
    for orphan in &loaded.orphans {
        assert!(listing.iter().any(|(name, _)| name == orphan));
        for machine in loaded.library.machines() {
            assert_ne!(*orphan, state_blob(machine.id));
            for (word, count) in Probe::PARTS {
                for index in 0..*count {
                    let owned = machine.holds(word, index);
                    assert!(!owned || *orphan != part_blob(word, machine.id, index));
                }
            }
        }
    }
    if loaded.found != Found::Unreadable {
        for (name, _) in &listing {
            let stranded = (1..=10)
                .filter(|&id| loaded.library.machine(id).is_none())
                .any(|id| *name == state_blob(id));
            assert!(
                !stranded || loaded.orphans.contains(name),
                "{name} is swept"
            );
        }
    }
}

const BOUNDS: Bounds = Bounds {
    largest: (24, 16),
    most_kept: 512,
};

/// A state as a machine leaves it: a last screen within the bounds, and a
/// snapshot while it is suspended.
fn kept_state(rng: &mut Prng) -> (LastScreen, Option<Suspended>) {
    let (width, height) = (1 + rng.at_most(23), 1 + rng.at_most(15));
    let mut pixels = vec![0; width * height * 4];
    if rng.below(2) == 0 {
        rng.fill(&mut pixels);
    }
    let screen = LastScreen {
        width: u32::try_from(width).expect("small"),
        height: u32::try_from(height).expect("small"),
        pixels,
    };
    let suspended = (rng.below(2) == 0).then(|| {
        let len = 1 + rng.at_most(BOUNDS.most_kept - 1);
        Suspended {
            kept: rng.noise(len),
            at: (rng.below(2) == 0).then(|| i64::from_le_bytes(rng.next_u64().to_le_bytes())),
        }
    });
    (screen, suspended)
}

/// A state that decodes holds a screen and snapshot within the bounds, and
/// its header read alone agrees with it.
fn check_state(bytes: &[u8]) {
    let head = state::head(bytes, &BOUNDS);
    let Ok((screen, suspended)) = state::decode(bytes, &BOUNDS) else {
        return;
    };
    let head = head.expect("a state that decodes has a header that does");
    assert_eq!(head.size(), bytes.len());
    assert_eq!(head.suspended(), suspended.is_some());
    assert_eq!(
        state::screen(&head, &bytes[head.screen_span()], &BOUNDS).as_ref(),
        Ok(&screen)
    );
    let (most_w, most_h) = BOUNDS.largest;
    assert!(screen.width <= most_w && screen.height <= most_h);
    let dots = usize::try_from(screen.width * screen.height).expect("small");
    assert_eq!(screen.pixels.len(), dots * 4);
    assert!(suspended.is_none_or(|suspended| suspended.kept.len() <= BOUNDS.most_kept));
}

#[test]
fn library_records_and_states_never_panic_and_round_trip() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "library_records_and_states_never_panic_and_round_trip",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let library = edited_library(&mut rng);
        check_whole(&library);
        let record = library.record().expect("a library this small records");
        let copy = record.bytes;
        assert_eq!(
            Library::decode(&copy).map(|read| read.machines().to_vec()),
            Ok(library.machines().to_vec()),
            "a record reads back"
        );
        let damaged = rng.mutated(&copy);
        let len = rng.at_most(96);
        let noise = rng.noise(len);
        for bytes in [&copy, &damaged, &noise] {
            check_record(bytes);
        }
        let other = edited_library(&mut rng).record().expect("records").bytes;
        let copies = match rng.below(4) {
            0 => [Some(copy.as_slice()), Some(other.as_slice())],
            1 => [Some(damaged.as_slice()), None],
            2 => [None, Some(noise.as_slice())],
            _ => [None, None],
        };
        check_load(&mut rng, copies);

        let (screen, suspended) = kept_state(&mut rng);
        let kept =
            state::encode(&screen, suspended.as_ref(), &BOUNDS).expect("a state within bounds");
        assert_eq!(
            state::decode(&kept, &BOUNDS),
            Ok((screen, suspended)),
            "a state reads back"
        );
        check_state(&kept);
        check_state(&rng.mutated(&kept));
        let len = rng.at_most(state::HEAD + 64);
        check_state(&rng.noise(len));

        iteration += 1;
        match deadline {
            None if iteration >= SMOKE_ITERATIONS => break,
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            _ => {}
        }
    }
}
