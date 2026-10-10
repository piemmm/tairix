//! The block's control register as the tests model it: the sync bit echoes
//! what was written only once a bit clock has run, and a clear takes effect
//! with it. Control writes, and whatever the tests' suppliers are asked, go
//! to one log, so a test can read their order.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::vec::Vec;

use tairix_abi::driver::codec::CodecOp;
use tairix_abi::{DriverError, RegisterBlock};

use super::{CS, CS_SYNC, CS_TXCLR, WINDOW_LEN};

/// Something a test saw happen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// The control register was written.
    Control(u32),
    /// The bit clock was asked for this rate.
    ClockRun(u64),
    /// The bit clock was given up.
    ClockRelease,
    /// The codec was asked this.
    Codec(CodecOp),
    /// The DMA channel was started.
    DmaStart,
    /// The DMA channel was stopped.
    DmaStop,
}

/// The order things happened in.
pub type Log = Rc<RefCell<Vec<Event>>>;

/// The modelled block.
pub struct Block {
    writes: RefCell<Vec<(usize, u32)>>,
    /// The sync bit as it reads.
    sync: Cell<u32>,
    /// Reads left before the last written sync bit echoes.
    echo_in: Cell<Option<u32>>,
    written_sync: Cell<u32>,
    /// Whether a bit clock runs, whichever side drives it.
    pub clocked: Rc<Cell<bool>>,
    /// Clears that took effect.
    pub cleared: Cell<u32>,
    clear_pending: Cell<bool>,
    log: Option<Log>,
}

impl Block {
    /// A block whose sync bit reads `sync`, a bit clock running when
    /// `clocked`.
    pub fn new(clocked: bool, sync: u32) -> Self {
        Self {
            writes: RefCell::new(Vec::new()),
            sync: Cell::new(sync),
            echo_in: Cell::new(None),
            written_sync: Cell::new(sync),
            clocked: Rc::new(Cell::new(clocked)),
            cleared: Cell::new(0),
            clear_pending: Cell::new(false),
            log: None,
        }
    }

    /// A block writing its control register to `log`.
    pub fn logged(log: &Log, clocked: bool) -> Self {
        Self {
            log: Some(log.clone()),
            ..Self::new(clocked, 0)
        }
    }

    /// Every write, in order.
    pub fn writes(&self) -> Vec<(usize, u32)> {
        self.writes.borrow().clone()
    }

    /// Forget the writes so far.
    pub fn clear_writes(&self) {
        self.writes.borrow_mut().clear();
    }

    /// The value last written at `offset`.
    pub fn last(&self, offset: usize) -> Option<u32> {
        self.writes
            .borrow()
            .iter()
            .rev()
            .find(|(at, _)| *at == offset)
            .map(|&(_, value)| value)
    }
}

impl RegisterBlock for Block {
    fn read32(&self, offset: usize) -> Result<u32, DriverError> {
        assert_eq!(offset, CS, "only the control register is read");
        if self.clocked.get() {
            match self.echo_in.get() {
                Some(0) => {
                    self.sync.set(self.written_sync.get());
                    self.echo_in.set(None);
                    if self.clear_pending.replace(false) {
                        self.cleared.set(self.cleared.get() + 1);
                    }
                }
                Some(left) => self.echo_in.set(Some(left - 1)),
                None => {}
            }
        }
        Ok(self.sync.get())
    }

    fn write32(&self, offset: usize, value: u32) -> Result<(), DriverError> {
        self.writes.borrow_mut().push((offset, value));
        if offset == CS {
            if value & CS_TXCLR != 0 {
                self.clear_pending.set(true);
            }
            self.written_sync.set(value & CS_SYNC);
            self.echo_in.set(Some(3));
            if let Some(log) = &self.log {
                log.borrow_mut().push(Event::Control(value));
            }
        }
        Ok(())
    }

    fn block_len(&self) -> usize {
        WINDOW_LEN
    }
}
