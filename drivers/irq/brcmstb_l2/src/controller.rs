//! The lines of one L2 controller, served over `irqline-v1`.
//!
//! The controller latches each of its thirty-two inputs in a status register
//! and gates its output by a mask, so a line is unmasked only while its
//! consumer waits: it fires, the controller is masked there and acknowledged,
//! and the wait is answered. An edge arriving while masked stays latched and
//! fires the moment the next wait unmasks it.

use tairix_abi::{DriverError, Errno, ProcId, RegisterBlock};
use tairix_drvrt::irqline::{IrqLines, LineHardware, LineRecord};
use tairix_drvrt::{Recorder, Supplier, SupplierHost};

/// The lines one controller collects.
pub const LINES: usize = 32;

/// Where one layout of the controller keeps its registers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    status: usize,
    /// Write one to clear a latched edge; a level controller has none.
    clear: Option<usize>,
    mask_status: usize,
    mask_set: usize,
    mask_clear: usize,
}

impl Layout {
    /// The edge-latching controller (`brcm,l2-intc` and its kin).
    pub const EDGE: Self = Self {
        status: 0x00,
        clear: Some(0x08),
        mask_status: 0x0C,
        mask_set: 0x10,
        mask_clear: 0x14,
    };

    /// The level controller (`brcm,bcm7271-l2-intc`).
    pub const LEVEL: Self = Self {
        status: 0x00,
        clear: None,
        mask_status: 0x04,
        mask_set: 0x08,
        mask_clear: 0x0C,
    };

    /// Bytes of register window the layout needs.
    #[must_use]
    pub const fn window_len(&self) -> usize {
        let end = if self.mask_clear > self.mask_set {
            self.mask_clear
        } else {
            self.mask_set
        };
        end + 4
    }
}

/// The controller's registers, as the holders of its lines drive them.
struct Registers<'r> {
    regs: &'r dyn RegisterBlock,
    layout: Layout,
}

impl LineHardware for Registers<'_> {
    type Trigger = ();

    /// The binding's one cell: the line.
    fn select(&self, selector: &[u32]) -> Result<(u8, ()), Errno> {
        let [cell] = *selector else {
            return Err(Errno::OutOfRange);
        };
        u8::try_from(cell)
            .ok()
            .filter(|&line| usize::from(line) < LINES)
            .map(|line| (line, ()))
            .ok_or(Errno::OutOfRange)
    }

    fn arm(&mut self, line: u8, (): (), _posted: bool) -> Result<(), Errno> {
        self.regs
            .write32(self.layout.mask_clear, 1 << line)
            .map_err(DriverError::as_errno)
    }

    fn release(&mut self, line: u8, (): ()) {
        let _ = self.regs.write32(self.layout.mask_set, 1 << line);
    }
}

/// One controller's lines and the consumers waiting on them.
pub struct Controller<'r, S: SupplierHost, L: Recorder<LineRecord>> {
    registers: Registers<'r>,
    lines: IrqLines<(), LINES>,
    supplier: S,
    log: L,
}

impl<'r, S: SupplierHost, L: Recorder<LineRecord>> Controller<'r, S, L> {
    /// The controller behind `regs` in `layout`, serving `endpoint` and
    /// asking about its callers through `supplier`: every line masked and
    /// every latched edge cleared, so nothing a predecessor left armed can
    /// fire before its consumer waits.
    ///
    /// # Errors
    ///
    /// The register write that failed.
    pub fn new(
        regs: &'r dyn RegisterBlock,
        layout: Layout,
        supplier: S,
        log: L,
        endpoint: u64,
    ) -> Result<Self, DriverError> {
        regs.write32(layout.mask_set, u32::MAX)?;
        if let Some(clear) = layout.clear {
            regs.write32(clear, u32::MAX)?;
        }
        Ok(Self {
            registers: Registers { regs, layout },
            lines: IrqLines::new(endpoint),
            supplier,
            log,
        })
    }
}

impl<S: SupplierHost, L: Recorder<LineRecord>> Supplier for Controller<'_, S, L> {
    /// Answers at once unless the call is a wait, answered when its line
    /// fires.
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
        self.lines.serve(
            ticket,
            frame,
            &mut self.registers,
            &mut self.supplier,
            &mut self.log,
        );
    }

    /// The controller's own line, its only one: answer the wait on every
    /// unmasked line that fired, masking and acknowledging each first.
    fn woke(&mut self, _index: usize) -> Result<(), DriverError> {
        let Registers { regs, layout } = &self.registers;
        let fired = regs.read32(layout.status)? & !regs.read32(layout.mask_status)?;
        if fired == 0 {
            return Ok(());
        }
        regs.write32(layout.mask_set, fired)?;
        if let Some(clear) = layout.clear {
            regs.write32(clear, fired)?;
        }
        self.lines
            .fired(u64::from(fired), &mut self.supplier, &mut self.log);
        Ok(())
    }

    /// Release every line `peer` held, masking each.
    fn peer_exited(&mut self, peer: ProcId) {
        self.lines
            .peer_exited(peer, &mut self.registers, &mut self.supplier, &mut self.log);
    }
}
