//! How a device the kernel drives itself — a bootstrap-floor disk — takes its
//! interrupt: bound to the kernel's process, configured and armed, then
//! waited on.

use tairix_abi::IrqHandle;
use tairix_kernel_core::{FallbackPark, IrqParkWaiter};
use tairix_kernel_irq::{IrqController, IrqTable, Trigger};
use tairix_kernel_sec::captable::ProcessId;

/// Where a port's floor devices take their interrupts.
#[derive(Clone, Copy)]
pub struct LineHost {
    /// The table device interrupts fire into.
    pub table: &'static IrqTable,
    /// The controller lines are configured and re-armed through.
    pub controller: &'static (dyn IrqController + Sync),
    /// The park a wait takes where the scheduler cannot park it.
    pub park: FallbackPark,
}

impl LineHost {
    /// Bind `line` to `owner` and arm it, first making it signal by `trigger`
    /// where it is wired; a message-signalled line has none. A line that
    /// cannot be armed is unbound again.
    ///
    /// # Errors
    ///
    /// The step that failed.
    pub fn bind(
        self,
        line: u32,
        trigger: Option<Trigger>,
        owner: ProcessId,
    ) -> Result<ArmedLine, &'static str> {
        let handle = self
            .table
            .bind_exclusive(line, owner)
            .map_err(|_| "floor: bind the device line")?
            .handle;
        // The trigger is configured while the line is still masked.
        let armed = trigger
            .map_or(Ok(()), |trigger| self.controller.set_trigger(line, trigger))
            .and_then(|()| self.controller.rearm(line));
        if armed.is_err() {
            self.table.release_binding(handle, owner, self.controller);
            return Err("floor: arm the device line");
        }
        Ok(ArmedLine {
            host: self,
            handle,
            owner,
        })
    }
}

/// A device interrupt bound to its owner and armed for its first completion.
pub struct ArmedLine {
    host: LineHost,
    handle: IrqHandle,
    owner: ProcessId,
}

impl ArmedLine {
    /// The table the line fires into.
    #[must_use]
    pub fn table(&self) -> &'static IrqTable {
        self.host.table
    }

    /// The owner's binding of the line.
    #[must_use]
    pub fn handle(&self) -> IrqHandle {
        self.handle
    }

    /// The waiter the line's owner parks on for its completions.
    #[must_use]
    pub fn waiter(&self) -> IrqParkWaiter {
        IrqParkWaiter::new(
            self.host.table,
            self.handle,
            self.owner,
            self.host.controller,
            Some(self.host.park),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;
    use std::sync::Mutex;
    use std::vec::Vec;

    use tairix_abi::IrqHandle;
    use tairix_kernel_irq::{IrqController, IrqTable, MaskError, Trigger};
    use tairix_kernel_sec::captable::ProcessId;

    use super::LineHost;

    const OWNER: ProcessId = ProcessId(7);
    const LINE: u32 = 5;

    /// A controller recording what it was asked, refusing a re-arm when told.
    struct Recorder {
        calls: Mutex<Vec<&'static str>>,
        refuse_rearm: bool,
    }

    impl IrqController for Recorder {
        fn mask(&self, _line: u32) -> Result<(), MaskError> {
            Ok(())
        }

        fn rearm(&self, _line: u32) -> Result<(), MaskError> {
            self.calls.lock().unwrap().push("rearm");
            if self.refuse_rearm {
                Err(MaskError::Unsupported)
            } else {
                Ok(())
            }
        }

        fn set_trigger(&self, _line: u32, trigger: Trigger) -> Result<(), MaskError> {
            self.calls.lock().unwrap().push(match trigger {
                Trigger::Level => "level",
                Trigger::Edge => "edge",
            });
            Ok(())
        }
    }

    fn park(_table: &IrqTable, _handle: IrqHandle) {}

    fn host(refuse_rearm: bool) -> (LineHost, &'static Recorder) {
        let recorder: &'static Recorder = Box::leak(Box::new(Recorder {
            calls: Mutex::new(Vec::new()),
            refuse_rearm,
        }));
        let host = LineHost {
            table: Box::leak(Box::new(IrqTable::new(31))),
            controller: recorder,
            park,
        };
        (host, recorder)
    }

    #[test]
    fn a_wired_line_is_configured_before_it_is_armed() {
        let (host, recorder) = host(false);
        let armed = host.bind(LINE, Some(Trigger::Level), OWNER).unwrap();
        assert_eq!(*recorder.calls.lock().unwrap(), ["level", "rearm"]);
        assert_eq!(
            host.table.lookup(armed.handle()).map(|entry| entry.owner),
            Some(OWNER)
        );
    }

    #[test]
    fn a_message_signalled_line_is_armed_with_no_trigger() {
        let (host, recorder) = host(false);
        host.bind(LINE, None, OWNER).unwrap();
        assert_eq!(*recorder.calls.lock().unwrap(), ["rearm"]);
    }

    #[test]
    fn a_line_that_cannot_be_armed_is_unbound_again() {
        let (host, _) = host(true);
        assert!(host.bind(LINE, Some(Trigger::Level), OWNER).is_err());
        assert!(host.table.bind_exclusive(LINE, OWNER).is_ok());
    }
}
