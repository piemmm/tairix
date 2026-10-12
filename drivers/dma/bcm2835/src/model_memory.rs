//! The simulated memory both engines' register models fetch control blocks
//! from, and the timeline every model records its steps on.
//!
//! A chain is carved here and freed when its table drops unless withheld, so
//! a block fetched from memory already freed is found missing, as on metal.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::DriverError;

use crate::chain::{BlockStore, BlockTable, BLOCK_BYTES};

/// Where the first chain is carved.
const TABLE_BASE: u64 = 0xC020_0000;
const TABLE_STEP: u64 = 0x1000;

/// One step the device or the endpoint took, in the order it happened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trace {
    /// A register write.
    Write {
        channel: usize,
        register: usize,
        value: u32,
    },
    /// The endpoint dropped its mapping of the buffer at this bus address.
    Released { bus: u64 },
    /// A chain carved at this bus address was freed.
    Freed { bus: u64 },
}

pub type Timeline = Rc<RefCell<Vec<Trace>>>;

/// The memory chains are carved from.
pub struct Memory {
    tables: BTreeMap<u64, Vec<u8>>,
    next: u64,
    carved: usize,
    refuse: bool,
    timeline: Timeline,
}

impl Memory {
    /// An empty memory recording frees on `timeline`.
    pub fn new(timeline: &Timeline) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            tables: BTreeMap::new(),
            next: TABLE_BASE,
            carved: 0,
            refuse: false,
            timeline: Rc::clone(timeline),
        }))
    }

    /// The eight words of the block at bus address `address`, or [`None`]
    /// where no live table holds it.
    pub fn block(&self, address: u64) -> Option<[u32; 8]> {
        let (&base, bytes) = self.tables.range(..=address).next_back()?;
        let offset = usize::try_from(address - base).ok()?;
        let bytes = bytes.get(offset..offset + BLOCK_BYTES)?;
        let mut words = [0u32; 8];
        for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*chunk);
        }
        Some(words)
    }

    /// Refuse every further carve.
    pub fn refuse(&mut self) {
        self.refuse = true;
    }

    pub fn carved(&self) -> usize {
        self.carved
    }

    pub fn live_tables(&self) -> usize {
        self.tables.len()
    }
}

/// Carves chains from a model's memory.
pub struct ModelStore {
    pub memory: Rc<RefCell<Memory>>,
}

impl BlockStore for ModelStore {
    type Table = ModelTable;

    fn carve(&self, bytes: usize) -> Result<ModelTable, DriverError> {
        let mut memory = self.memory.borrow_mut();
        if memory.refuse {
            return Err(DriverError::OutOfMemory);
        }
        let base = memory.next;
        memory.next += TABLE_STEP;
        memory.carved += 1;
        memory.tables.insert(base, std::vec![0u8; bytes]);
        Ok(ModelTable {
            base,
            memory: Rc::clone(&self.memory),
            withheld: false,
        })
    }
}

/// One carve; freed when dropped unless withheld, after which a fetch from it
/// finds nothing.
pub struct ModelTable {
    base: u64,
    memory: Rc<RefCell<Memory>>,
    withheld: bool,
}

impl BlockTable for ModelTable {
    fn bus_address(&self) -> u64 {
        self.base
    }

    fn store(&mut self, offset: usize, block: &[u32; 8]) -> Result<(), DriverError> {
        let mut memory = self.memory.borrow_mut();
        let table = memory
            .tables
            .get_mut(&self.base)
            .ok_or(DriverError::OutOfRange)?;
        let bytes = table
            .get_mut(offset..offset + BLOCK_BYTES)
            .ok_or(DriverError::OutOfRange)?;
        for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(block) {
            *chunk = word.to_le_bytes();
        }
        Ok(())
    }

    fn withhold(&mut self) {
        self.withheld = true;
    }
}

impl Drop for ModelTable {
    fn drop(&mut self) {
        if self.withheld {
            return;
        }
        let mut memory = self.memory.borrow_mut();
        memory.tables.remove(&self.base);
        memory
            .timeline
            .borrow_mut()
            .push(Trace::Freed { bus: self.base });
    }
}
