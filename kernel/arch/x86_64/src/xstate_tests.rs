//! Host tests for the extended-state configuration and the park/resume state
//! machine.

extern crate std;

use std::fmt::Write as _;
use std::format;
use std::string::String;
use std::vec::Vec;

use super::*;

#[test]
fn xcr0_enables_avx_when_supported_and_avx512_only_whole() {
    assert_eq!(xcr0_for(X87 | SSE), X87 | SSE);
    assert_eq!(xcr0_for(X87 | SSE | AVX), X87 | SSE | AVX);
    // MPX (bits 3-4), PKRU (bit 9) and AMX (bits 17-18) are never enabled.
    let everything = 0x6_02FF;
    assert_eq!(xcr0_for(everything), X87 | SSE | AVX | AVX512);
    // Two thirds of AVX-512 is not AVX-512, and it needs AVX beneath it.
    assert_eq!(xcr0_for(X87 | SSE | AVX | (0b011 << 5)), X87 | SSE | AVX);
    assert_eq!(xcr0_for(X87 | SSE | AVX512), X87 | SSE);
}

#[test]
fn the_flavour_is_the_best_save_the_cpu_offers() {
    assert_eq!(flavour_for(0, 1), Flavour::Fxsave);
    assert_eq!(flavour_for(LEAF1_ECX_XSAVE, 0), Flavour::Xsave);
    assert_eq!(
        flavour_for(LEAF1_ECX_XSAVE, LEAF_D1_EAX_XSAVEOPT),
        Flavour::Xsaveopt
    );
}

#[test]
fn the_park_mask_leaves_the_framed_sse_state_out() {
    let config = Config::new(Flavour::Xsaveopt, X87 | SSE | AVX, 832);
    assert_eq!(config.park_mask(), X87 | AVX);
}

#[test]
fn a_mask_splits_into_the_edx_eax_pair() {
    assert_eq!(halves(0x0000_0002_0000_00E7), (0xE7, 2));
    assert_eq!(halves(X87 | SSE | AVX), (7, 0));
}

#[test]
fn an_area_is_the_header_and_the_image_rounded_to_its_alignment() {
    assert_eq!(
        Config::new(Flavour::Fxsave, X87 | SSE, 0).area_bytes(),
        64 + 512
    );
    // XSAVE with AVX: 512 legacy + 64 header + 256 upper halves.
    assert_eq!(
        Config::new(Flavour::Xsave, X87 | SSE | AVX, 832).area_bytes(),
        64 + 832
    );
    assert_eq!(
        Config::new(Flavour::Xsave, X87 | SSE | AVX, 833).area_bytes(),
        64 + 896
    );
    // The AVX-512 standard-format image.
    assert_eq!(
        Config::new(Flavour::Xsaveopt, X87 | SSE | AVX | AVX512, 2688).area_bytes(),
        64 + 2688
    );
}

#[test]
fn a_config_survives_its_packed_word_and_is_never_the_unset_zero() {
    for config in [
        Config::new(Flavour::Fxsave, X87 | SSE, 0),
        Config::new(Flavour::Xsave, X87 | SSE | AVX, 832),
        Config::new(Flavour::Xsaveopt, X87 | SSE | AVX | AVX512, 2688),
    ] {
        assert_ne!(config.packed(), 0);
        assert_eq!(Config::unpacked(config.packed()), Some(config));
    }
    assert_eq!(Config::unpacked(0), None);
}

#[test]
fn the_initial_image_is_the_architectural_initial_state() {
    let image = &INIT_IMAGE;
    assert_eq!(
        core::ptr::addr_of!(*image) as usize % 64,
        0,
        "XRSTOR needs 64-byte alignment"
    );
    assert_eq!(core::mem::size_of::<InitImage>(), 576);
    assert_eq!(
        u16::from_le_bytes([image.legacy[0], image.legacy[1]]),
        0x037F
    );
    let mxcsr = u32::from_le_bytes(image.legacy[24..28].try_into().unwrap());
    assert_eq!(mxcsr, crate::fpu::MXCSR_DEFAULT);
    let other = image
        .legacy
        .iter()
        .enumerate()
        .filter(|&(i, _)| !(0..2).contains(&i) && !(24..28).contains(&i));
    assert!(other.map(|(_, b)| *b).all(|b| b == 0));
    // `XSTATE_BV` zero initialises every component, and a non-zero byte in
    // 8..24 of the header would make XRSTOR fault.
    assert!(image.header.iter().all(|&b| b == 0));
}

// --- The state machine -----------------------------------------------------

/// A policy for [`resume`], so the model below can be shown to catch a wrong
/// one.
type ResumeFn = fn(&mut AreaHeader, u64, u64, u64, u64);

/// Whose extended state a CPU's registers hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Regs {
    /// Task `id` as of its `version`-th change.
    Task { id: usize, version: u64 },
    /// Nothing any live task may see.
    Garbage,
}

/// A CPU in the model.
struct Cpu {
    owner: u64,
    regs: Regs,
    /// The area XRSTOR last loaded from, for XSAVEOPT's modified
    /// optimisation; `Some(0)` is the initial image.
    tracked: Option<u64>,
    /// Whether ring 3 has changed the registers since that XRSTOR.
    modified: bool,
    running: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    User(usize),
    Kernel(usize),
    Parked,
}

/// A task incarnation in the model.
struct Task {
    id: usize,
    area: u64,
    header: AreaHeader,
    image: Option<Regs>,
    version: u64,
    mode: Mode,
}

/// A deterministic generator, so a failing sequence is reproducible.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n as u64).unwrap_or(0)
    }
}

/// The area addresses tasks draw from: fewer than the tasks created, so
/// addresses are reused.
const AREAS: [u64; 3] = [0x1000, 0x2000, 0x3000];

/// The owner slot address the model hands `resume` for `cpu`.
const fn slot_of(cpu: usize) -> u64 {
    0x100 + cpu as u64
}

/// Run `steps` random legal events on `cpus` CPUs, asserting at each return
/// to ring 3 that the registers hold exactly the returning task's state, and
/// at each XSAVEOPT skip that the skipped image is current.
fn simulate(resume_policy: ResumeFn, seed: u64, cpus: usize, steps: usize) -> Result<(), String> {
    let mut rng = XorShift(seed | 1);
    let cpu: Vec<Cpu> = (0..cpus)
        .map(|_| Cpu {
            owner: 0,
            regs: Regs::Garbage,
            tracked: None,
            modified: false,
            running: None,
        })
        .collect();
    let mut world = World {
        cpu,
        tasks: Vec::new(),
        next_id: 0,
        log: String::new(),
    };
    for _ in 0..steps {
        apply(&mut world, &mut rng, resume_policy, cpus)?;
    }
    Ok(())
}

/// The mutable state one [`simulate`] run threads through [`apply`].
struct World {
    cpu: Vec<Cpu>,
    tasks: Vec<Task>,
    next_id: usize,
    log: String,
}

/// Apply one random legal event to `world`, returning an error string when an
/// invariant the model checks is broken.
///
/// One `match` over the seven event kinds: the arms share the world's mutable
/// state, so splitting them into separate functions would only thread six
/// borrows through each — the dispatch reads more clearly whole.
#[allow(clippy::too_many_lines)]
fn apply(
    world: &mut World,
    rng: &mut XorShift,
    resume_policy: ResumeFn,
    cpus: usize,
) -> Result<(), String> {
    let World {
        cpu,
        tasks,
        next_id,
        log,
    } = world;
    let idle: Vec<usize> = (0..cpus).filter(|&c| cpu[c].running.is_none()).collect();
    let free: Vec<u64> = AREAS
        .iter()
        .copied()
        .filter(|a| tasks.iter().all(|t| t.area != *a))
        .collect();
    match rng.below(7) {
        0 if !idle.is_empty() && !free.is_empty() => {
            let c = idle[rng.below(idle.len())];
            let area = free[rng.below(free.len())];
            let _ = write!(log, "first entry of t{} at {area:#x} on c{c}; ", *next_id);
            cpu[c].owner = 0;
            cpu[c].regs = Regs::Task {
                id: *next_id,
                version: 0,
            };
            cpu[c].tracked = Some(0);
            cpu[c].modified = false;
            cpu[c].running = Some(*next_id);
            tasks.push(Task {
                id: *next_id,
                area,
                header: AreaHeader::default(),
                image: None,
                version: 0,
                mode: Mode::User(c),
            });
            *next_id += 1;
        }
        1 if !tasks.is_empty() => {
            let t = rng.below(tasks.len());
            let task = &mut tasks[t];
            match task.mode {
                Mode::User(c) => {
                    task.version += 1;
                    cpu[c].regs = Regs::Task {
                        id: task.id,
                        version: task.version,
                    };
                    cpu[c].modified = true;
                }
                Mode::Kernel(c) => {
                    let _ = write!(log, "return of t{} on c{c}; ", task.id);
                    if task.header.load_pending != 0 {
                        let owner = task.header.owner_slot;
                        if owner != slot_of(c) {
                            return Err(format!("{log}: a load for another CPU's slot"));
                        }
                        cpu[c].regs = task.image.ok_or(format!("{log}: load of no image"))?;
                        cpu[c].owner = task.area;
                        cpu[c].tracked = Some(task.area);
                        cpu[c].modified = false;
                        task.header.load_pending = 0;
                    }
                    let own = Regs::Task {
                        id: task.id,
                        version: task.version,
                    };
                    if cpu[c].regs != own {
                        return Err(format!("{log}: returned holding {:?}", cpu[c].regs));
                    }
                    task.mode = Mode::User(c);
                }
                Mode::Parked => {}
            }
        }
        2 if !tasks.is_empty() => {
            let t = rng.below(tasks.len());
            if let Mode::User(c) = tasks[t].mode {
                tasks[t].mode = Mode::Kernel(c);
            }
        }
        3 if !tasks.is_empty() => {
            let t = rng.below(tasks.len());
            let task = &mut tasks[t];
            if let Mode::Kernel(c) = task.mode {
                let _ = write!(log, "park of t{} on c{c}; ", task.id);
                if park(&mut task.header, c as u64) {
                    let skip = cpu[c].tracked == Some(task.area) && !cpu[c].modified;
                    if skip {
                        if task.image != Some(cpu[c].regs) {
                            return Err(format!("{log}: XSAVEOPT skipped a stale image"));
                        }
                    } else {
                        task.image = Some(cpu[c].regs);
                    }
                    cpu[c].owner = task.area;
                }
                cpu[c].running = None;
                task.mode = Mode::Parked;
            }
        }
        4 if !tasks.is_empty() && !idle.is_empty() => {
            let t = rng.below(tasks.len());
            let c = idle[rng.below(idle.len())];
            let task = &mut tasks[t];
            if task.mode == Mode::Parked {
                let _ = write!(log, "resume of t{} on c{c}; ", task.id);
                resume_policy(
                    &mut task.header,
                    task.area,
                    c as u64,
                    cpu[c].owner,
                    slot_of(c),
                );
                cpu[c].running = Some(task.id);
                task.mode = Mode::Kernel(c);
            }
        }
        5 if !tasks.is_empty() => {
            // Death on any path, a park included or not.
            let t = rng.below(tasks.len());
            let task = tasks.swap_remove(t);
            let _ = write!(log, "death of t{}; ", task.id);
            if let Mode::User(c) | Mode::Kernel(c) = task.mode {
                cpu[c].running = None;
            }
        }
        _ => {}
    }
    Ok(())
}

// The two sweeps below are pure safe-logic oracles, not UB probes — the port's
// `unsafe` is the `target_os = "none"` asm the host miri run never compiles —
// and each interprets hundreds of thousands of allocating steps, far past
// miri's budget. They run in full under an ordinary `cargo test`; miri still
// covers the layout, init-image and state-machine unit tests above it, which
// are what touch static alignment and offsets.

#[cfg_attr(
    miri,
    ignore = "pure-logic model, no UB for miri; full run under cargo test"
)]
#[test]
fn every_return_to_ring_3_holds_the_tasks_own_state() {
    for seed in 1..=4_000 {
        for cpus in [1, 2, 3] {
            if let Err(e) = simulate(resume, seed, cpus, 80) {
                panic!("seed {seed}, {cpus} CPUs: {e}");
            }
        }
    }
}

/// The model must be able to fail: trusting the owner slot alone lets a task
/// that ran elsewhere since trust registers that went stale here.
#[cfg_attr(
    miri,
    ignore = "pure-logic model, no UB for miri; full run under cargo test"
)]
#[test]
fn a_resume_that_ignores_where_the_area_last_lived_is_caught() {
    fn owner_only(header: &mut AreaHeader, area: u64, cpu: u64, owner: u64, slot: u64) {
        if owner == area {
            return;
        }
        header.load_pending = 1;
        header.last_cpu = cpu + 1;
        header.owner_slot = slot;
    }
    let caught = (1..=4_000).any(|seed| simulate(owner_only, seed, 2, 80).is_err());
    assert!(caught);
}

#[test]
fn a_task_back_on_its_own_cpu_loads_nothing() {
    let mut header = AreaHeader::default();
    let area = 0x4000;
    assert!(park(&mut header, 1));
    // The park recorded the area as CPU 1's owner.
    resume(&mut header, area, 1, area, slot_of(1));
    assert_eq!(header.load_pending, 0);
}

#[test]
fn a_task_parked_again_before_it_returned_skips_the_save_and_still_loads() {
    let mut header = AreaHeader::default();
    let area = 0x4000;
    assert!(park(&mut header, 0));
    // Another task's state reached CPU 0 in between.
    resume(&mut header, area, 0, 0x5000, slot_of(0));
    assert_eq!(header.load_pending, 1);
    assert!(
        !park(&mut header, 0),
        "the registers are not the task's to save"
    );
    resume(&mut header, area, 0, 0x5000, slot_of(0));
    assert_eq!(header.load_pending, 1);
    assert_eq!(header.owner_slot, slot_of(0));
}

#[test]
fn a_migrated_task_loads_where_it_once_owned_the_registers() {
    let mut header = AreaHeader::default();
    let area = 0x4000;
    assert!(park(&mut header, 0));
    resume(&mut header, area, 1, 0, slot_of(1));
    header.load_pending = 0;
    assert!(park(&mut header, 1));
    // CPU 0 still names the area, from before the migration.
    resume(&mut header, area, 0, area, slot_of(0));
    assert_eq!(header.load_pending, 1);
    assert_eq!(header.last_cpu, 1);
}

/// A zeroed header names no CPU, so a reused address that a CPU's stale
/// owner slot still names cannot pass for resident on CPU 0.
#[test]
fn a_zeroed_header_is_resident_nowhere() {
    let mut header = AreaHeader::default();
    let area = 0x4000;
    resume(&mut header, area, 0, area, slot_of(0));
    assert_eq!(header.load_pending, 1);
}
