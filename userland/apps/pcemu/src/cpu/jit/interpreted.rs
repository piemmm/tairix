//! An executor that runs each block's IR on the code generator's reference
//! interpreter, with no code installed: the oracle the generated code is
//! held to, so a whole machine can be run through its blocks on any host and
//! held to the CPU interpreter.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_codegen::interp::{self, Helpers, Memory, Trap};
use tairix_codegen::ir::{Function, Type, Width};

use super::cache::Request;
use super::dispatch::{Env, Execute};
use super::frame::{Frame, FRAME_BYTES};
use super::page::PageMap;
use super::{helper, translate_request, Exit};

/// Where the frame, the page map and the helper table seem to lie to the
/// interpreted code; the memory the page map maps lies where it does.
const FRAME_BASE: u64 = 0x1_0000;
const PAGES_BASE: u64 = 0x10_0000;
const TABLE: u64 = 0x20_0000;
/// Steps a block runs in at most: far past any 32 instructions' IR.
const FUEL: u64 = 1_000_000;

/// The blocks compiled, by handle; none once released.
type Compiled = Rc<RefCell<Vec<Option<Function>>>>;

/// Runs blocks on the IR interpreter.
#[derive(Debug, Default)]
pub struct Interpreted {
    blocks: Compiled,
    /// The blocks adopted, by handle: the bytes each was said to take.
    adopted: BTreeMap<u64, u64>,
    runs: Rc<Cell<u64>>,
}

/// What compiles requests for an [`Interpreted`]: its compiler.
#[derive(Debug)]
pub struct Compiler {
    blocks: Compiled,
    runs: Rc<Cell<u64>>,
}

impl Interpreted {
    /// An executor holding no blocks, and the compiler that fills it.
    #[must_use]
    pub fn new() -> (Self, Compiler) {
        let blocks = Compiled::default();
        let runs = Rc::new(Cell::new(0));
        (
            Self {
                blocks: Rc::clone(&blocks),
                adopted: BTreeMap::new(),
                runs: Rc::clone(&runs),
            },
            Compiler { blocks, runs },
        )
    }
}

impl Compiler {
    /// Compiles `request` from its bytes alone, as the compiler sandbox
    /// does: the handle of its code, the address its executor adopts it by,
    /// or none where it does not translate.
    #[must_use]
    pub fn compile(&self, request: &Request) -> Option<u64> {
        let block = translate_request(request)?;
        let mut blocks = self.blocks.borrow_mut();
        blocks.push(Some(block.func));
        u64::try_from(blocks.len() - 1).ok()
    }

    /// Blocks its executor has run.
    #[must_use]
    pub fn runs(&self) -> u64 {
        self.runs.get()
    }
}

impl Execute for Interpreted {
    fn run(
        &mut self,
        code: u64,
        frame: &mut Frame,
        pages: &PageMap,
        env: &mut Env<'_>,
    ) -> Option<Exit> {
        if !self.adopted.contains_key(&code) {
            return None;
        }
        let blocks = self.blocks.borrow();
        let func = blocks.get(usize::try_from(code).ok()?)?.as_ref()?;
        self.runs.set(self.runs.get() + 1);
        let world = RefCell::new(World { frame, pages, env });
        let answer = interp::run(
            func,
            &[FRAME_BASE, PAGES_BASE, TABLE],
            &mut Space(&world),
            &mut Calls(&world),
            FUEL,
        );
        Exit::of(answer.ok()??)
    }

    fn adopt(&mut self, address: u64, len: u64) -> Option<u64> {
        let compiled = usize::try_from(address)
            .is_ok_and(|at| self.blocks.borrow().get(at).is_some_and(Option::is_some));
        if !compiled || self.adopted.contains_key(&address) {
            return None;
        }
        self.adopted.insert(address, len);
        Some(address)
    }

    fn release(&mut self, code: u64) {
        self.adopted.remove(&code);
        let mut blocks = self.blocks.borrow_mut();
        if let Some(slot) = usize::try_from(code).ok().and_then(|at| blocks.get_mut(at)) {
            *slot = None;
        }
    }

    fn held(&self) -> u64 {
        self.adopted.values().sum()
    }
}

struct World<'a, 'e> {
    frame: &'a mut Frame,
    pages: &'a PageMap,
    env: &'a mut Env<'e>,
}

/// Where `len` bytes at `base` hold the `width` bytes at `addr`: their
/// offset.
fn within(addr: u64, base: u64, len: usize, width: Width) -> Option<usize> {
    let at = usize::try_from(addr.checked_sub(base)?).ok()?;
    (at.checked_add(width.bytes() as usize)? <= len).then_some(at)
}

struct Space<'w, 'a, 'e>(&'w RefCell<World<'a, 'e>>);

impl Memory for Space<'_, '_, '_> {
    fn load(&mut self, addr: u64, width: Width) -> Result<u64, Trap> {
        let world = self.0.borrow();
        let n = width.bytes() as usize;
        if let Some(at) = addr
            .checked_sub(PAGES_BASE)
            .and_then(|at| usize::try_from(at).ok())
        {
            if let Some(value) = world.pages.load(at, n) {
                return Ok(value);
            }
        }
        let bytes = if let Some(at) = within(addr, FRAME_BASE, FRAME_BYTES, width) {
            &world.frame.bytes()[at..at + n]
        } else if let Some(at) = within(addr, world.env.base, world.env.store.len(), width) {
            &world.env.store[at..at + n]
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
        if let Some(at) = within(addr, FRAME_BASE, FRAME_BYTES, width) {
            world.frame.bytes_mut()[at..at + n].copy_from_slice(bytes);
        } else if let Some(at) = within(addr, world.env.base, world.env.store.len(), width) {
            world.env.store[at..at + n].copy_from_slice(bytes);
        } else {
            return Err(Trap::Memory(addr));
        }
        Ok(())
    }
}

struct Calls<'w, 'a, 'e>(&'w RefCell<World<'a, 'e>>);

impl Helpers for Calls<'_, '_, '_> {
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
        if index == helper::REFRESH {
            if args.first() != Some(&FRAME_BASE) {
                return Err(Trap::Helper(index));
            }
            let at = args.get(1).copied().ok_or(Trap::Helper(index))?;
            let mut world = self.0.borrow_mut();
            let World { frame, env, .. } = &mut *world;
            return Ok(Some(env.refresh(frame, at)));
        }
        helper::call(index, args)
            .map(Some)
            .ok_or(Trap::Helper(index))
    }
}
