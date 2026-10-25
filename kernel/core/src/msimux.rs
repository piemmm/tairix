//! The lines a chained message-interrupt multiplexer's vectors are delivered
//! on, and the `msi_alloc` facility over them (`plans/WIRING.md` W18).
//!
//! A port discovers the multiplexer ([`MsiMultiplexer`]); [`MsiMuxLines`]
//! lays its vectors out on lines from a first line the port chooses, brings
//! it up on the first allocation, hands out the lowest free vector, and fans
//! a fire of its parent line out to the vectors pending. None of it depends on
//! the part, so a port reuses it unchanged.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::AtomicU64;

use tairix_abi::{Errno, MsiAllocation};
use tairix_arch_api::{IrqControlError, MsiMultiplexer, MsiMuxError};
use tairix_kernel_irq::{IrqController, IrqTable, MaskError};
use tairix_sync::{claims, SpinLock};

use crate::devres::MsiAllocFacility;

/// Why [`MsiMuxLines::new`] laid out no lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MsiMuxLinesError {
    /// The multiplexer reports no bank.
    Undiscovered,
    /// Its lines would run past the line space.
    LinesOverflow,
    /// Its claim map could not be allocated.
    OutOfMemory,
}

/// How far the multiplexer's bring-up has got.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BringUp {
    /// Not yet.
    Down,
    /// Enabled, its parent line routed.
    Up,
    /// Refused for good.
    Failed,
}

/// A multiplexer's vectors laid out on the lines `first_line..`.
pub struct MsiMuxLines {
    mux: &'static dyn MsiMultiplexer,
    lines: Range<u32>,
    parent_line: u32,
    enable_parent: fn(u32) -> bool,
    bring_up: SpinLock<BringUp>,
    /// Bit `v` set: vector `v` was handed out. Never cleared once a device
    /// may have been told it, since nothing proves the device stopped
    /// raising it.
    claimed: Box<[AtomicU64]>,
}

impl MsiMuxLines {
    /// Lay `mux`'s vectors out from `first_line`. `enable_parent` routes and
    /// unmasks the parent line at bring-up, answering whether it could.
    ///
    /// # Errors
    ///
    /// See [`MsiMuxLinesError`].
    pub fn new(
        mux: &'static dyn MsiMultiplexer,
        first_line: u32,
        enable_parent: fn(u32) -> bool,
    ) -> Result<Self, MsiMuxLinesError> {
        let shape = mux.shape().ok_or(MsiMuxLinesError::Undiscovered)?;
        let vectors = shape.vectors.get();
        let end = first_line
            .checked_add(vectors)
            .ok_or(MsiMuxLinesError::LinesOverflow)?;
        let words = vectors.div_ceil(64) as usize;
        let mut claimed = Vec::new();
        claimed
            .try_reserve_exact(words)
            .map_err(|_| MsiMuxLinesError::OutOfMemory)?;
        claimed.resize_with(words, || AtomicU64::new(0));
        Ok(Self {
            mux,
            lines: first_line..end,
            parent_line: shape.parent_line,
            enable_parent,
            bring_up: SpinLock::new(BringUp::Down),
            claimed: claimed.into_boxed_slice(),
        })
    }

    /// The lines the vectors are delivered on, vector `v` on the `v`th.
    #[must_use]
    pub fn lines(&self) -> Range<u32> {
        self.lines.clone()
    }

    /// The line the multiplexer raises for its whole bank.
    #[must_use]
    pub fn parent_line(&self) -> u32 {
        self.parent_line
    }

    /// The vector `line` delivers, or [`None`] for a line outside the window.
    #[must_use]
    pub fn vector_of(&self, line: u32) -> Option<u32> {
        line.checked_sub(self.lines.start)
            .filter(|_| self.lines.contains(&line))
    }

    /// Mask `line`'s vector; [`None`] for a line outside the window.
    #[must_use]
    pub fn mask(&self, line: u32) -> Option<Result<(), MaskError>> {
        self.vector_of(line)
            .map(|vector| self.mux.mask(vector).map_err(mask_error))
    }

    /// Unmask `line`'s vector for its next message; [`None`] for a line
    /// outside the window.
    #[must_use]
    pub fn rearm(&self, line: u32) -> Option<Result<(), MaskError>> {
        self.vector_of(line)
            .map(|vector| self.mux.unmask(vector).map_err(mask_error))
    }

    /// Fan a fire of the parent line out to each pending vector's line,
    /// through `through`; `false`, touching nothing, for any other `intid`.
    ///
    /// Each vector's fire masks it before the multiplexer acknowledges it,
    /// so a vector nobody waits on stays masked rather than raising the
    /// parent again.
    #[must_use]
    pub fn service(&self, intid: u32, table: &IrqTable, through: &dyn IrqController) -> bool {
        if intid != self.parent_line {
            return false;
        }
        self.mux.drain(&mut |vector| {
            // A vector past the bank is the port's fault and raises nothing.
            if let Some(line) = self.line_of(vector) {
                // A stray is masked by its fire; the table reports it.
                let _ = table.fire(line, through);
            }
        });
        true
    }

    fn line_of(&self, vector: u32) -> Option<u32> {
        self.lines
            .start
            .checked_add(vector)
            .filter(|line| self.lines.contains(line))
    }

    /// Enable the multiplexer and route its parent line, once; a refusal is
    /// for good.
    fn bring_up(&self) -> Result<(), Errno> {
        let mut state = self.bring_up.lock();
        if *state == BringUp::Down {
            *state = match self.mux.enable() {
                Ok(()) if (self.enable_parent)(self.parent_line) => BringUp::Up,
                Ok(()) | Err(MsiMuxError::NotSupported | MsiMuxError::OutOfRange) => {
                    BringUp::Failed
                }
            };
        }
        if *state == BringUp::Up {
            Ok(())
        } else {
            Err(Errno::NotImplemented)
        }
    }
}

impl MsiAllocFacility for MsiMuxLines {
    /// The lowest free vector, after bringing the multiplexer up on the
    /// first allocation. Every vector is open once it is up, so a message
    /// raised before its driver waits is masked by its fire and acknowledged.
    fn allocate(&self) -> Result<MsiAllocation, Errno> {
        self.bring_up()?;
        let bank = self.lines.len();
        let index = claims::claim_lowest(&self.claimed, bank).ok_or(Errno::OutOfRange)?;
        let allocation = u32::try_from(index)
            .map_err(|_| MsiMuxError::OutOfRange)
            .and_then(|vector| {
                let message = self.mux.message(vector)?;
                let line = self.line_of(vector).ok_or(MsiMuxError::OutOfRange)?;
                Ok(MsiAllocation::new(message.address, message.data, line))
            });
        allocation.map_err(|error| {
            // No device was told the vector, so it is free again.
            claims::release(&self.claimed, index);
            match error {
                MsiMuxError::NotSupported => Errno::NotImplemented,
                MsiMuxError::OutOfRange => Errno::OutOfRange,
            }
        })
    }
}

fn mask_error(error: IrqControlError) -> MaskError {
    match error {
        IrqControlError::OutOfRange => MaskError::OutOfRange,
    }
}

#[cfg(test)]
mod tests {
    use super::{MsiMuxLines, MsiMuxLinesError};
    use crate::devres::MsiAllocFacility;
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::num::NonZeroU32;
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Mutex;
    use tairix_abi::{Errno, MsiAllocation, MsiMessage};
    use tairix_arch_api::{
        IrqControlError, IrqController, MsiMultiplexer, MsiMuxError, MsiMuxShape,
    };
    use tairix_kernel_irq::{IrqTable, MaskError, WaitStep};
    use tairix_kernel_sec::ProcessId;

    const PARENT: u32 = 148;
    const FIRST: u32 = 1020;

    /// What the multiplexer was asked to do, in order.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    enum Seen {
        Enable,
        Mask(u32),
        Unmask(u32),
        Acknowledge(u32),
    }

    /// A bank of up to 64 vectors that logs every request.
    struct Bank {
        vectors: u32,
        pending: AtomicU64,
        seen: Mutex<Vec<Seen>>,
    }

    impl Bank {
        fn leaked(vectors: u32) -> &'static Bank {
            crate::forever(Box::new(Bank {
                vectors,
                pending: AtomicU64::new(0),
                seen: Mutex::new(Vec::new()),
            }))
        }

        fn note(&self, seen: Seen) {
            self.seen.lock().unwrap().push(seen);
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn raise(&self, vector: u32) {
            self.pending.fetch_or(1 << vector, Ordering::SeqCst);
        }

        fn admit(&self, vector: u32) -> Result<(), IrqControlError> {
            if vector < self.vectors {
                Ok(())
            } else {
                Err(IrqControlError::OutOfRange)
            }
        }
    }

    impl IrqController for Bank {
        fn mask(&self, vector: u32) -> Result<(), IrqControlError> {
            self.admit(vector)?;
            self.note(Seen::Mask(vector));
            Ok(())
        }
        fn unmask(&self, vector: u32) -> Result<(), IrqControlError> {
            self.admit(vector)?;
            self.note(Seen::Unmask(vector));
            Ok(())
        }
    }

    impl MsiMultiplexer for Bank {
        fn shape(&self) -> Option<MsiMuxShape> {
            NonZeroU32::new(self.vectors).map(|vectors| MsiMuxShape {
                vectors,
                parent_line: PARENT,
            })
        }
        fn enable(&self) -> Result<(), MsiMuxError> {
            self.note(Seen::Enable);
            Ok(())
        }
        fn message(&self, vector: u32) -> Result<MsiMessage, MsiMuxError> {
            self.admit(vector).map_err(|_| MsiMuxError::OutOfRange)?;
            Ok(MsiMessage {
                address: 0xFFFF_FFFC,
                data: 0x6540 | vector,
            })
        }
        fn drain(&self, fire: &mut dyn FnMut(u32)) {
            let pending = self.pending.load(Ordering::SeqCst);
            for vector in (0..self.vectors).filter(|vector| pending & (1 << vector) != 0) {
                fire(vector);
                self.pending.fetch_and(!(1 << vector), Ordering::SeqCst);
                self.note(Seen::Acknowledge(vector));
            }
        }
    }

    fn route(parent: u32) -> bool {
        parent == PARENT
    }

    fn refuse(_: u32) -> bool {
        false
    }

    fn laid_out(bank: &'static Bank) -> MsiMuxLines {
        MsiMuxLines::new(bank, FIRST, route).unwrap()
    }

    /// Masks a multiplexer line through it, as a port's composite does.
    struct Through<'a>(&'a MsiMuxLines);

    impl tairix_kernel_irq::IrqController for Through<'_> {
        fn mask(&self, line: u32) -> Result<(), MaskError> {
            self.0.mask(line).unwrap_or(Err(MaskError::OutOfRange))
        }
    }

    #[test]
    fn allocates_lowest_free_vector_on_its_line_with_its_message() {
        let lines = laid_out(Bank::leaked(4));
        assert_eq!(
            lines.allocate(),
            Ok(MsiAllocation::new(0xFFFF_FFFC, 0x6540, FIRST))
        );
        assert_eq!(
            lines.allocate(),
            Ok(MsiAllocation::new(0xFFFF_FFFC, 0x6541, FIRST + 1))
        );
        assert_eq!(lines.lines(), FIRST..FIRST + 4);
        assert_eq!(lines.parent_line(), PARENT);
    }

    #[test]
    fn exhausted_space_is_out_of_range() {
        let lines = laid_out(Bank::leaked(2));
        assert!(lines.allocate().is_ok());
        assert!(lines.allocate().is_ok());
        assert_eq!(lines.allocate(), Err(Errno::OutOfRange));
    }

    static ROUTED: AtomicU32 = AtomicU32::new(0);

    fn count_route(parent: u32) -> bool {
        ROUTED.fetch_add(1, Ordering::SeqCst);
        route(parent)
    }

    #[test]
    fn brings_up_once_only_on_first_allocation() {
        let bank = Bank::leaked(4);
        let lines = MsiMuxLines::new(bank, FIRST, count_route).unwrap();
        assert!(
            bank.seen().is_empty(),
            "nothing is brought up before an allocation"
        );
        assert_eq!(ROUTED.load(Ordering::SeqCst), 0);
        assert!(lines.allocate().is_ok());
        assert!(lines.allocate().is_ok());
        assert_eq!(bank.seen(), [Seen::Enable]);
        assert_eq!(ROUTED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unroutable_parent_allocates_nothing() {
        let bank = Bank::leaked(4);
        let lines = MsiMuxLines::new(bank, FIRST, refuse).unwrap();
        assert_eq!(lines.allocate(), Err(Errno::NotImplemented));
        assert_eq!(lines.allocate(), Err(Errno::NotImplemented));
        assert_eq!(bank.seen(), [Seen::Enable], "a refusal is for good");
        assert!(lines
            .claimed
            .iter()
            .all(|word| word.load(Ordering::SeqCst) == 0));
    }

    #[test]
    fn undiscovered_controller_offers_no_lines() {
        assert!(matches!(
            MsiMuxLines::new(Bank::leaked(0), FIRST, route),
            Err(MsiMuxLinesError::Undiscovered)
        ));
        assert!(matches!(
            MsiMuxLines::new(Bank::leaked(4), u32::MAX - 3, route),
            Err(MsiMuxLinesError::LinesOverflow)
        ));
        assert!(MsiMuxLines::new(Bank::leaked(4), u32::MAX - 4, route).is_ok());
    }

    #[test]
    fn mask_and_rearm_answer_only_for_its_window() {
        let bank = Bank::leaked(4);
        let lines = laid_out(bank);
        assert_eq!(lines.mask(FIRST - 1), None);
        assert_eq!(lines.mask(FIRST + 4), None);
        assert_eq!(lines.rearm(FIRST + 4), None);
        assert_eq!(lines.mask(FIRST), Some(Ok(())));
        assert_eq!(lines.rearm(FIRST + 3), Some(Ok(())));
        assert_eq!(bank.seen(), [Seen::Mask(0), Seen::Unmask(3)]);
        assert_eq!(lines.vector_of(FIRST + 2), Some(2));
        assert_eq!(lines.vector_of(FIRST + 4), None);
    }

    #[test]
    fn service_fires_pending_vectors_only_for_its_parent() {
        let bank = Bank::leaked(4);
        let lines = laid_out(bank);
        let table = IrqTable::new(FIRST + 3);
        let bound = table.bind(FIRST + 1, ProcessId(1)).unwrap();
        bank.raise(1);
        bank.raise(3);
        let through = Through(&lines);
        assert!(!lines.service(PARENT + 1, &table, &through));
        assert!(bank.seen().is_empty(), "another line touches nothing");
        assert!(lines.service(PARENT, &table, &through));
        assert_eq!(
            bank.seen(),
            [
                Seen::Mask(1),
                Seen::Acknowledge(1),
                Seen::Mask(3),
                Seen::Acknowledge(3)
            ]
        );
        assert_eq!(
            table.try_wait_step(bound.handle, ProcessId(1), 0, u64::MAX),
            WaitStep::Ready
        );
    }
}
