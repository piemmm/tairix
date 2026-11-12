//! The translator held to the interpreter (`plans/PCEMU.md` PC18.5): a
//! random machine, random code at its CS:IP, and the block there translated
//! and run through the code generator's interpreter and every backend's
//! model of its machine, with and without the extensions each lowers to,
//! each from the same state, each held to the CPU interpreter stepping to
//! the same cycle.
//!
//! The interpreter shows the IR right; a backend's model runs the machine
//! instructions it lowers to, so it shows the lowering right too, and what
//! the IR interpreter cannot see: a register read before it was set, a
//! callee-saved register or the stack left other than it was, the stack
//! misaligned at a call, a frame record missing.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_codegen::interp::{self, Helpers, Memory, Trap};
use tairix_codegen::ir::{Function, Inst, Type, Width};
use tairix_codegen::model::{self, Calls, Fault};
use tairix_codegen::Target;

use super::frame::{at, FRAME_BYTES};
use super::{decode_block, helper, page, translate, Code, Exit, Frame, PageMap};
use crate::cpu::{flag, Bus, Cpu, CpuModel, Family, Seg};
use crate::snapshot::{Out, State};

/// Where the frame, the page map, guest memory and the helper table lie in
/// the address space a block is run in, and where the table says each
/// helper is: `HELPERS + 16 * index`.
const FRAME_BASE: u64 = 0x1_0000_0000;
const PAGES_BASE: u64 = 0x2_0000_0000;
const RAM_BASE: u64 = 0x3_0000_0000;
const TABLE: u64 = 0x4_0000_0000;
const HELPERS: u64 = 0x6_0000_0000;

/// Steps the interpreter runs a block in at most: far past any block's IR.
const FUEL: u64 = 1_000_000;
/// Machine instructions a model may take for each IR step.
const MODEL_STEPS: u64 = 64;

/// The code segment every case's code is in.
const CODE: u16 = 0x1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ram,
    Rom,
    Device,
}

/// A megabyte with wait states per page, read-only and device pages, and
/// DRAM refresh taking the bus every `period` cycles.
#[derive(Clone, Debug)]
struct TestBus {
    ram: Vec<u8>,
    waits: Vec<u8>,
    kinds: Vec<Kind>,
    stolen: u64,
    refresh_at: u64,
    period: u64,
    cost: u64,
}

impl TestBus {
    fn kind(&self, addr: u32) -> Kind {
        self.kinds[(addr >> page::PAGE_BITS) as usize]
    }

    fn waits(&self, addr: u32) -> u64 {
        u64::from(self.waits[(addr >> page::PAGE_BITS) as usize])
    }
}

impl Bus for TestBus {
    fn read(&mut self, addr: u32, _at: u64) -> u8 {
        if self.kind(addr) == Kind::Device {
            return 0xA5;
        }
        self.stolen += self.waits(addr);
        self.ram[addr as usize]
    }
    fn write(&mut self, addr: u32, value: u8, _at: u64) {
        match self.kind(addr) {
            Kind::Device => {}
            kind => {
                self.stolen += self.waits(addr);
                if kind == Kind::Ram {
                    self.ram[addr as usize] = value;
                }
            }
        }
    }
    fn peek(&self, addr: u32) -> u8 {
        if self.kind(addr) == Kind::Device {
            0xA5
        } else {
            self.ram[addr as usize]
        }
    }
    fn input(&mut self, _port: u16, _at: u64) -> u8 {
        0xFF
    }
    fn output(&mut self, _port: u16, _value: u8, _at: u64) {}
    fn intr(&self) -> bool {
        false
    }
    fn acknowledge(&mut self) -> u8 {
        0
    }
    fn nmi(&self) -> bool {
        false
    }
    fn acknowledge_nmi(&mut self) {}
    fn host_code(&self, _addr: u32) -> bool {
        false
    }
    fn take_wait_states(&mut self, at: u64) -> u32 {
        if at >= self.refresh_at {
            let due = (at - self.refresh_at) / self.period + 1;
            self.stolen += due * self.cost;
            self.refresh_at += due * self.period;
        }
        u32::try_from(core::mem::take(&mut self.stolen)).unwrap_or(u32::MAX)
    }
    fn next_refresh(&self) -> u64 {
        self.refresh_at
    }
    fn steal(&mut self, cycles: u32) {
        self.stolen += u64::from(cycles);
    }
}

impl Code for TestBus {
    fn byte(&self, addr: u32) -> Option<u8> {
        (self.kind(addr) != Kind::Device).then(|| self.ram[addr as usize])
    }
}

/// What a block runs over: its frame, the page map, the bus whose memory the
/// map's pages lie in, and every helper call it made.
struct World {
    frame: Frame,
    pages: PageMap,
    bus: TestBus,
    calls: Vec<(u32, Vec<u64>)>,
}

impl World {
    fn new(case: &Case, watched: &[u32]) -> Self {
        Self {
            frame: case.cpu.frame(&case.bus, case.limit),
            pages: page_map(&case.bus, watched),
            bus: case.bus.clone(),
            calls: Vec::new(),
        }
    }

    /// What helper `index` answers for `args`; none for a refresh not handed
    /// the frame.
    fn call(&mut self, index: u32, args: &[u64]) -> Option<u64> {
        self.calls.push((index, args.to_vec()));
        if index != helper::REFRESH {
            return helper::call(index, args);
        }
        if args.first() != Some(&FRAME_BASE) {
            return None;
        }
        let stolen = self.bus.take_wait_states(*args.get(1)?);
        let next = self.bus.next_refresh();
        self.frame.set_u64(at::REFRESH, next);
        Some(u64::from(stolen))
    }
}

fn window(addr: u64, base: u64, len: usize, width: Width) -> Option<usize> {
    let at = usize::try_from(addr.checked_sub(base)?).ok()?;
    (at.checked_add(width.bytes() as usize)? <= len).then_some(at)
}

struct Space<'a>(&'a RefCell<World>);

impl Memory for Space<'_> {
    fn load(&mut self, addr: u64, width: Width) -> Result<u64, Trap> {
        let world = self.0.borrow();
        let n = width.bytes() as usize;
        let entry = addr.wrapping_sub(TABLE);
        if width == Width::W64 && entry.is_multiple_of(8) && entry < u64::from(helper::COUNT) * 8 {
            return Ok(HELPERS + entry / 8 * 16);
        }
        if let Some(at) = addr
            .checked_sub(PAGES_BASE)
            .and_then(|at| usize::try_from(at).ok())
        {
            if let Some(value) = world.pages.load(at, n) {
                return Ok(value);
            }
        }
        let bytes: &[u8] = if let Some(at) = window(addr, FRAME_BASE, FRAME_BYTES, width) {
            &world.frame.bytes()[at..at + n]
        } else if let Some(at) = window(addr, RAM_BASE, world.bus.ram.len(), width) {
            &world.bus.ram[at..at + n]
        } else {
            return Err(Trap::Memory(addr));
        };
        let mut raw = [0u8; 8];
        raw[..n].copy_from_slice(bytes);
        Ok(u64::from_le_bytes(raw))
    }

    fn store(&mut self, addr: u64, width: Width, value: u64) -> Result<(), Trap> {
        let mut world = self.0.borrow_mut();
        let n = width.bytes() as usize;
        let bytes = &value.to_le_bytes()[..n];
        if let Some(at) = window(addr, FRAME_BASE, FRAME_BYTES, width) {
            world.frame.bytes_mut()[at..at + n].copy_from_slice(bytes);
        } else if let Some(at) = window(addr, RAM_BASE, world.bus.ram.len(), width) {
            world.bus.ram[at..at + n].copy_from_slice(bytes);
        } else {
            return Err(Trap::Memory(addr));
        }
        Ok(())
    }
}

/// The helpers as the IR interpreter calls them: by the table and an index.
struct Indexed<'a>(&'a RefCell<World>);

impl Helpers for Indexed<'_> {
    fn call(
        &mut self,
        table: u64,
        index: u32,
        args: &[u64],
        ret: Option<Type>,
    ) -> Result<Option<u64>, Trap> {
        if table != TABLE || ret != Some(Type::I64) {
            return Err(Trap::Helper(index));
        }
        let arity = helper::ARITY.get(index as usize).copied();
        if arity != Some(args.len()) {
            return Err(Trap::Helper(index));
        }
        self.0
            .borrow_mut()
            .call(index, args)
            .map(Some)
            .ok_or(Trap::Helper(index))
    }
}

/// The helpers as a model calls them: by the address the table gave, each
/// of the arguments it takes in a register the code set.
struct Addressed<'a>(&'a RefCell<World>);

impl Calls for Addressed<'_> {
    fn call(&mut self, target: u64, args: [Option<u64>; 6]) -> Result<u64, Fault> {
        let offset = target.wrapping_sub(HELPERS);
        let index = u32::try_from(offset / 16)
            .ok()
            .filter(|&index| offset.is_multiple_of(16) && index < helper::COUNT)
            .ok_or(Fault::Call(target))?;
        let passed = args[..helper::ARITY[index as usize]]
            .iter()
            .enumerate()
            .map(|(n, arg)| arg.ok_or(Fault::Argument(n)))
            .collect::<Result<Vec<u64>, Fault>>()?;
        self.0
            .borrow_mut()
            .call(index, &passed)
            .ok_or(Fault::Call(target))
    }
}

/// The block's page map over `bus`: plain pages at their place in memory, a
/// device page nowhere, and stores barred from ROM and from `watched`.
fn page_map(bus: &TestBus, watched: &[u32]) -> PageMap {
    let mut map = PageMap::default();
    for (page, &kind) in bus.kinds.iter().enumerate() {
        let base = RAM_BASE + page as u64 * u64::from(page::PAGE_BYTES);
        match kind {
            Kind::Device => map.set(page, 0, 0, false),
            Kind::Rom => map.set(page, base, bus.waits[page], false),
            Kind::Ram => {
                let watch = u32::try_from(page).is_ok_and(|page| watched.contains(&page));
                map.set(page, base, bus.waits[page], !watch);
            }
        }
    }
    map
}

fn snapshot(cpu: &Cpu) -> Vec<u8> {
    let mut out = Out::refusable();
    cpu.save(&mut out);
    out.finish().unwrap_or_default()
}

/// What the runs reached, which a test holds to its floors.
#[derive(Debug, Default)]
pub struct Seen {
    /// Blocks, by the exit each took.
    pub exits: [usize; 3],
    /// Blocks that ran at least one instruction.
    pub ran: usize,
    /// Blocks within which refresh took the bus.
    pub refreshed: usize,
    /// Blocks run on a backend's model.
    pub modelled: usize,
    /// Instructions translated, by kind.
    pub kinds: BTreeMap<String, usize>,
}

/// A source of random words.
struct Draw<'a>(&'a mut dyn FnMut() -> u64);

impl Draw<'_> {
    fn below(&mut self, n: usize) -> usize {
        usize::try_from((self.0)() % u64::try_from(n.max(1)).unwrap_or(u64::MAX)).unwrap_or(0)
    }

    fn word(&mut self, below: u32) -> u16 {
        u16::try_from(self.below(below as usize)).unwrap_or(0)
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// Opcodes the translator takes, drawn more often than chance.
const COMMON: [u8; 72] = [
    0x01, 0x03, 0x05, 0x27, 0x29, 0x2F, 0x31, 0x37, 0x39, 0x3B, 0x3F, 0x40, 0x48, 0x50, 0x58, 0x60,
    0x61, 0x68, 0x69, 0x6A, 0x6B, 0x74, 0x75, 0x80, 0x81, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F, 0x90, 0x98, 0x99, 0x9A, 0x9C, 0x9E, 0x9F, 0xA4, 0xA6, 0xAA,
    0xAC, 0xAE, 0xB0, 0xB8, 0xC0, 0xC1, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC9, 0xCB, 0xD0, 0xD1, 0xD2,
    0xD3, 0xD4, 0xD5, 0xE2, 0xE3, 0xE8, 0xFE, 0xFF,
];

/// The megabyte every case's memory starts as, its code then written over
/// it.
#[must_use]
pub fn pattern() -> Vec<u8> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..1 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

/// A machine to run a block in.
#[derive(Clone, Debug)]
pub struct Case {
    cpu: Cpu,
    bus: TestBus,
    limit: u64,
}

impl Case {
    /// A random machine drawn from `next`: its memory `pattern` with random
    /// code at CS:IP, its pages of every kind and speed, its registers and
    /// bus interface unit anywhere a step boundary allows.
    #[must_use]
    pub fn random(next: &mut dyn FnMut() -> u64, pattern: &[u8]) -> Self {
        let mut draw = Draw(next);
        let model = draw.pick(&CpuModel::ALL);
        let (kinds, waits) = pages(&mut draw);
        let ip = draw.word(0x400);
        let mut ram = pattern.to_vec();
        for i in 0..96u16 {
            let byte = if draw.below(4) == 0 {
                draw.pick(&COMMON)
            } else {
                u8::try_from(draw.below(256)).unwrap_or(0)
            };
            ram[crate::cpu::linear(CODE, ip.wrapping_add(i)) as usize] = byte;
        }
        let mut kinds = kinds;
        // The code, and a kibibyte either side of it, is memory.
        let code = crate::cpu::linear(CODE, ip) as usize;
        let near = code.saturating_sub(1024) >> page::PAGE_BITS
            ..((code + 3072) >> page::PAGE_BITS).min(page::PAGES);
        for kind in &mut kinds[near] {
            if *kind == Kind::Device {
                *kind = Kind::Ram;
            }
        }
        let cpu = processor(&mut draw, model, ip);
        let bus = TestBus {
            ram,
            waits,
            kinds,
            stolen: 0,
            refresh_at: cpu.cycles() + draw.below(120) as u64,
            period: 60 + draw.below(20) as u64,
            cost: 4 + draw.below(4) as u64,
        };
        let span = if draw.below(3) == 0 { 60 } else { 4000 };
        let limit = cpu.cycles() + 1 + draw.below(span) as u64;
        Self { cpu, bus, limit }
    }
}

/// Every page's kind and wait states: mostly memory, some ROM and devices,
/// all fast or of mixed speeds.
fn pages(draw: &mut Draw<'_>) -> (Vec<Kind>, Vec<u8>) {
    let mut kinds = vec![Kind::Ram; page::PAGES];
    let mut waits = vec![0u8; page::PAGES];
    let slow = draw.below(2) == 0;
    for (kind, wait) in kinds.iter_mut().zip(&mut waits) {
        *kind = match draw.below(40) {
            0 | 1 => Kind::Rom,
            2 => Kind::Device,
            _ => Kind::Ram,
        };
        if slow {
            *wait = u8::try_from(draw.below(3)).unwrap_or(0);
        }
    }
    (kinds, waits)
}

/// A `model` processor at `CODE:ip`, its registers, flags and bus interface
/// unit drawn from `draw`.
fn processor(draw: &mut Draw<'_>, model: CpuModel, ip: u16) -> Cpu {
    let mut cpu = Cpu::new(model);
    cpu.idle_until(draw.below(5000) as u64);
    let segments = [0x0000u16, 0x2000, 0x4000, 0x7F00, 0xF000];
    let regs = cpu.regs_mut();
    for r in &mut regs.gpr {
        *r = if draw.below(3) == 0 {
            draw.word(0x200)
        } else {
            draw.word(0x1_0000)
        };
    }
    regs.set_sreg(Seg::Cs, CODE);
    for seg in [Seg::Ds, Seg::Es, Seg::Ss] {
        regs.set_sreg(seg, draw.pick(&segments));
    }
    regs.ip = ip;
    let flags = draw.word(0x1_0000) & !flag::TF;
    let high = if model.family() == Family::Nec {
        0x8000
    } else {
        0xF000
    };
    regs.flags = (flags & flag::WRITABLE) | flag::RESERVED_ONE | high;
    cpu.biu.fill = u8::try_from(draw.below(usize::from(model.queue_len()) + 1)).unwrap_or(0);
    cpu.biu.credit = u32::try_from(draw.below(4)).unwrap_or(0);
    cpu.ind = draw.word(0x1_0000);
    cpu.opr = draw.word(0x1_0000);
    cpu
}

/// What a block run left: its answer, frame and world.
struct Ran {
    answer: Option<u64>,
    world: World,
}

/// Runs `func` from `case` on the IR interpreter.
fn interpreted(case: &Case, func: &Function, watched: &[u32]) -> Ran {
    let world = RefCell::new(World::new(case, watched));
    let answer = interp::run(
        func,
        &[FRAME_BASE, PAGES_BASE, TABLE],
        &mut Space(&world),
        &mut Indexed(&world),
        FUEL,
    );
    let answer = answer.unwrap_or_else(|trap| panic!("{trap:?}\n{func:#?}"));
    Ran {
        answer,
        world: world.into_inner(),
    }
}

/// Runs `func` from `case` on `target`'s model.
fn modelled(target: Target, case: &Case, func: &Function, watched: &[u32]) -> Ran {
    let world = RefCell::new(World::new(case, watched));
    let answer = model::run(
        target,
        func,
        &[FRAME_BASE, PAGES_BASE, TABLE],
        &mut Space(&world),
        &mut Addressed(&world),
        FUEL * MODEL_STEPS,
    );
    let answer = answer.unwrap_or_else(|fault| panic!("{target:?}: {fault:?}\n{func:#?}"));
    Ran {
        answer,
        world: world.into_inner(),
    }
}

/// Runs the block at `case`'s CS:IP through the IR interpreter and every
/// backend's model, and the CPU interpreter from the same state to the same
/// cycle, and holds every part of the machine to each other: the exit the
/// block took, or none where no block begins there.
///
/// # Panics
///
/// Where any two disagree, with the case's code and state.
#[must_use]
pub fn agree(case: &Case, seen: &mut Seen) -> Option<Exit> {
    let (cpu, bus) = (&case.cpu, &case.bus);
    let (cs, ip) = (cpu.regs().sreg(Seg::Cs), cpu.regs().ip);
    let block = translate(cpu.model(), cs, ip, bus)?;
    assert!(cpu.can_enter_block(bus));
    for (_, inst) in block.func.blocks().iter().flat_map(|b| &b.insts) {
        if let Inst::Call { index, args, .. } = inst {
            assert_eq!(
                helper::ARITY.get(*index as usize),
                Some(&args.len()),
                "helper {index} passed {} arguments",
                args.len()
            );
        }
    }
    let context = || {
        alloc::format!(
            "{:?} at {cs:04X}:{ip:04X}, {} instructions\n{:02X?}",
            cpu.model(),
            block.instructions,
            code_bytes(bus, cs, ip)
        )
    };
    let ir = interpreted(case, &block.func, &block.pages);
    for target in Target::ALL {
        let native = modelled(target, case, &block.func, &block.pages);
        let what = |part| alloc::format!("{target:?} {part}: {}", context());
        assert_eq!(native.answer, ir.answer, "{}", what("answer"));
        assert!(native.world.frame == ir.world.frame, "{}", what("frame"));
        assert!(
            native.world.bus.ram == ir.world.bus.ram,
            "{}",
            what("memory")
        );
        assert_eq!(
            native.world.calls,
            ir.world.calls,
            "{}",
            what("helper calls")
        );
        assert_eq!(
            (native.world.bus.stolen, native.world.bus.refresh_at),
            (ir.world.bus.stolen, ir.world.bus.refresh_at),
            "{}",
            what("refresh")
        );
        seen.modelled += 1;
    }
    let exit = ir.answer.and_then(Exit::of).expect("an exit");
    let Ran { world, .. } = ir;
    let (frame, mut jit_bus) = (world.frame, world.bus);
    let mut jit = cpu.clone();
    jit.adopt(&mut jit_bus, &frame);
    let mut reference = cpu.clone();
    let mut reference_bus = bus.clone();
    while reference.cycles() < jit.cycles() {
        reference.step(&mut reference_bus, case.limit);
    }
    assert_eq!(reference.cycles(), jit.cycles(), "cycles: {}", context());
    assert_eq!(reference.regs(), jit.regs(), "registers: {}", context());
    assert!(
        snapshot(&reference) == snapshot(&jit),
        "state: {}\n{reference:?}\n{jit:?}",
        context()
    );
    if let Some(at) = reference_bus
        .ram
        .iter()
        .zip(&jit_bus.ram)
        .position(|(a, b)| a != b)
    {
        panic!("memory differs at {at:05X}: {}", context());
    }
    assert_eq!(
        (reference_bus.stolen, reference_bus.refresh_at),
        (jit_bus.stolen, jit_bus.refresh_at),
        "pending waits and refresh: {}",
        context()
    );
    for unit in decode_block(cpu.model(), cs, ip, bus) {
        let name = alloc::format!("{:?}", unit.insn.op);
        let name = name.split(['(', ' ', '{']).next().unwrap_or("").to_string();
        *seen.kinds.entry(name).or_insert(0) += 1;
    }
    seen.exits[exit as usize] += 1;
    seen.ran += usize::from(jit.cycles() > cpu.cycles());
    seen.refreshed += usize::from(jit_bus.refresh_at != bus.refresh_at);
    Some(exit)
}

fn code_bytes(bus: &TestBus, cs: u16, ip: u16) -> Vec<u8> {
    (0..24)
        .map(|i| bus.ram[crate::cpu::linear(cs, ip.wrapping_add(i)) as usize])
        .collect()
}
