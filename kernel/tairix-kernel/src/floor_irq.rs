//! How a device the kernel drives itself — a bootstrap-floor disk — takes its
//! interrupt: bound to the kernel's process, configured and armed, then
//! waited on.

use tairix_abi::driver::timing::{CompletionWait, Delay};
use tairix_abi::driver::CompletionSignal;
use tairix_abi::{HwNode, HwResourceKind, IrqHandle};
use tairix_kernel_core::waitq::deadline_for;
use tairix_kernel_core::{park_until, wait_now_ns, FallbackPark, IrqParkWaiter};
use tairix_kernel_irq::{IrqController, IrqTable, LineWait, Trigger};
use tairix_kernel_sec::captable::ProcessId;
use tairix_log::Sink;

/// Longest silence a host-opened floor device's completion wait tolerates
/// before failing the operation closed, in nanoseconds: a controller signals
/// every operation it starts well inside it, so a wait that elapses with no
/// interrupt at all means the controller or its routing is dead.
const SILENCE_BUDGET_NS: u64 = 2_000_000_000;

/// The one wired interrupt line `node` states, and how it signals.
///
/// # Errors
///
/// The node states no wired line, or more than one, which is ambiguous; a
/// message-signalled line is no wire.
pub fn node_line(node: &HwNode) -> Result<(u32, Trigger), &'static str> {
    let mut wired = node
        .resources()
        .iter()
        .filter(|resource| resource.kind() == Some(HwResourceKind::Irq) && !resource.is_message());
    let line = wired
        .next()
        .ok_or("floor: the node states no interrupt line")?;
    if wired.next().is_some() || line.length() != 1 {
        return Err("floor: the node states more than one interrupt line");
    }
    let number = u32::try_from(line.base())
        .map_err(|_| "floor: the node's interrupt line is out of range")?;
    let trigger = if line.is_edge_triggered() {
        Trigger::Edge
    } else {
        Trigger::Level
    };
    Ok((number, trigger))
}

/// A host-opened floor driver's completion wait: parked on its device's line,
/// its timed waits parked until their deadlines and spun only where the
/// scheduler cannot park the caller.
pub struct FloorCompletion {
    waiter: IrqParkWaiter,
    spin: fn(u32),
}

impl FloorCompletion {
    /// Park on `waiter`'s line, and time a wait with the port's `spin` where
    /// no park is possible.
    #[must_use]
    pub fn new(waiter: IrqParkWaiter, spin: fn(u32)) -> Self {
        Self { waiter, spin }
    }
}

impl CompletionWait for FloorCompletion {
    fn await_irq(&self) -> CompletionSignal {
        self.waiter.await_completion(SILENCE_BUDGET_NS).into()
    }
}

impl Delay for FloorCompletion {
    fn delay_us(&self, us: u32) {
        let span_ns = u64::from(us) * 1_000;
        let parked =
            wait_now_ns().is_some_and(|now| park_until(deadline_for(now, span_ns)).is_ok());
        if !parked {
            (self.spin)(us);
        }
    }

    fn now_us(&self) -> u64 {
        wait_now_ns().map_or(0, |ns| ns / 1_000)
    }
}

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

    /// The waiter the line's owner parks on for its completions, recording a
    /// late wake through `audit`.
    #[must_use]
    pub fn waiter(&self, audit: &'static (dyn Sink + Sync)) -> IrqParkWaiter {
        IrqParkWaiter::new(
            self.host.table,
            self.handle,
            self.owner,
            self.host.controller,
            Some(self.host.park),
            audit,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::vec::Vec;

    use tairix_abi::driver::timing::Delay;
    use tairix_abi::{HwDeviceClass, HwNode, HwResource, IrqHandle};
    use tairix_kernel_irq::{IrqController, IrqTable, MaskError, Trigger};
    use tairix_kernel_sec::captable::ProcessId;

    use super::{node_line, FloorCompletion, LineHost};
    use crate::test_support::NullSink;

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
        let recorder: &'static Recorder = tairix_kernel_core::forever(Box::new(Recorder {
            calls: Mutex::new(Vec::new()),
            refuse_rearm,
        }));
        let host = LineHost {
            table: tairix_kernel_core::forever(Box::new(IrqTable::new(31))),
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

    fn node_with(resources: &[HwResource]) -> HwNode {
        let mut node = HwNode::new(3, 0, HwDeviceClass::Storage);
        for &resource in resources {
            node.push_resource(resource).expect("room");
        }
        node
    }

    /// Regression: the SD bring-up re-read the raw tree for the first node of
    /// its kind instead of taking its own node's line, so a second such host
    /// would have bound the first's.
    #[test]
    fn a_node_s_one_wired_line_is_taken_with_its_trigger() {
        let window = HwResource::mmio(0x1000_0000, 0x100);
        assert_eq!(
            node_line(&node_with(&[window, HwResource::irq_at(126, 0)])),
            Ok((126, Trigger::Level))
        );
        assert_eq!(
            node_line(&node_with(&[HwResource::edge_irq_at(40, 0)])),
            Ok((40, Trigger::Edge))
        );
        assert_eq!(
            node_line(&node_with(&[
                HwResource::message_irq(90, 0),
                HwResource::irq_at(41, 1)
            ])),
            Ok((41, Trigger::Level)),
            "a message-signalled line is no wire"
        );
    }

    #[test]
    fn a_node_stating_no_wired_line_or_several_is_refused() {
        for resources in [
            &[][..],
            &[HwResource::message_irq(90, 0)],
            &[HwResource::irq_at(41, 0), HwResource::irq_at(42, 1)],
            &[HwResource::irq(41, 2)],
            &[HwResource::irq_at(u64::from(u32::MAX) + 1, 0)],
        ] {
            assert!(node_line(&node_with(resources)).is_err(), "{resources:?}");
        }
    }

    std::thread_local! {
        static SPUN_US: Cell<u32> = const { Cell::new(0) };
    }

    fn record_spin(us: u32) {
        SPUN_US.with(|spun| spun.set(spun.get() + us));
    }

    /// Where no wait clock is installed, as on the host, a timed wait cannot
    /// park and is the port's spin instead.
    #[test]
    fn a_wait_that_cannot_park_is_spun() {
        let (host, _) = host(false);
        let armed = host.bind(LINE, Some(Trigger::Level), OWNER).unwrap();
        let audit: &'static NullSink = Box::leak(Box::new(NullSink));
        let wait = FloorCompletion::new(armed.waiter(audit), record_spin);
        wait.delay_us(250);
        assert_eq!(SPUN_US.with(Cell::get), 250);
    }
}
