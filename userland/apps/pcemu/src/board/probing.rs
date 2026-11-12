//! The debugger's probe on the board ([`crate::debug::probe`]): the memory
//! cycles any master makes on a page it watches, every port cycle, and the
//! processor's instruction boundaries and interrupts.

use alloc::boxed::Box;

use super::memory::PAGE_SIZE;
use super::{Board, Planar};
use crate::cpu::Cpu;
use crate::debug::probe::{Breakpoint, Cycle, Go, Master, Probe, Reason};

impl Board {
    /// Puts `probe` on the board.
    pub fn attach_probe(&mut self, probe: Box<Probe>) {
        self.planar.probe = Some(probe);
        self.planar.mark_probed();
    }

    /// Takes the probe off the board.
    pub fn detach_probe(&mut self) -> Option<Box<Probe>> {
        let probe = self.planar.probe.take();
        self.planar.mark_probed();
        probe
    }

    /// The probe on the board, if one is.
    #[must_use]
    pub fn probe(&self) -> Option<&Probe> {
        self.planar.probe.as_deref()
    }

    /// Has the probe hold `breakpoint` too: whether it could
    /// ([`Probe::add`]).
    pub fn add_breakpoint(&mut self, breakpoint: Breakpoint) -> bool {
        let added = self
            .planar
            .probe
            .as_deref_mut()
            .is_some_and(|probe| probe.add(breakpoint));
        self.planar.mark_probed();
        added
    }

    /// Has the probe hold `breakpoint` no more: whether it held it.
    pub fn remove_breakpoint(&mut self, breakpoint: Breakpoint) -> bool {
        let removed = self
            .planar
            .probe
            .as_deref_mut()
            .is_some_and(|probe| probe.remove(breakpoint));
        self.planar.mark_probed();
        removed
    }

    /// Has the probe stop the running processor at its next boundary.
    pub fn request_break(&mut self) {
        if let Some(probe) = self.planar.probe.as_deref_mut() {
            probe.request_break();
        }
    }

    /// Sends `cpu` on from the stop it is at, as `go` says.
    pub fn go(&mut self, go: Go, cpu: &Cpu) {
        if let Some(mut probe) = self.planar.probe.take() {
            probe.go(go, cpu, self);
            self.planar.probe = Some(probe);
        }
    }

    /// Why the processor stopped, once, after it has.
    pub fn take_stop(&mut self) -> Option<Reason> {
        self.planar.probe.as_deref_mut().and_then(Probe::take_stop)
    }

    /// The processor halted, or waits for TEST: whether the probe stops it
    /// there.
    pub fn halt_stops(&mut self) -> bool {
        self.planar.probe.as_deref_mut().is_some_and(Probe::halted)
    }

    /// The processor is about to begin an instruction: whether the probe
    /// stops it there.
    pub(super) fn probe_begins(&mut self, cpu: &Cpu) -> bool {
        let Some(mut probe) = self.planar.probe.take() else {
            return false;
        };
        let stops = probe.begins(cpu, self);
        self.planar.probe = Some(probe);
        stops
    }
}

impl Planar {
    /// Marks every page a memory watch covers as one the board sees each
    /// access to.
    fn mark_probed(&mut self) {
        let probe = self.probe.as_deref();
        self.memory.set_probed(|page| {
            let first = u32::try_from(page).unwrap_or(u32::MAX) * PAGE_SIZE;
            probe.is_some_and(|probe| probe.watches(first, first + (PAGE_SIZE - 1)))
        });
    }

    /// `master` made memory cycle `cycle` carrying `value` at `addr`.
    pub(super) fn probe_memory(&mut self, addr: u32, value: u8, cycle: Cycle, master: Master) {
        let probed = self.memory.probed(addr);
        if let Some(probe) = self.probe.as_deref_mut().filter(|_| probed) {
            probe.memory(addr, value, cycle, master);
        }
    }

    /// `master` made port cycle `cycle` carrying `value` at `port`.
    pub(super) fn probe_port(&mut self, port: u16, value: u8, cycle: Cycle, master: Master) {
        if let Some(probe) = self.probe.as_deref_mut() {
            probe.port(port, value, cycle, master);
        }
    }

    /// The processor's word cycle at `port` of its control block: a cycle a
    /// byte.
    pub(super) fn probe_port_word(&mut self, port: u16, value: u16, cycle: Cycle) {
        let [low, high] = value.to_le_bytes();
        self.probe_port(port, low, cycle, Master::Cpu);
        self.probe_port(port.wrapping_add(1), high, cycle, Master::Cpu);
    }
}

#[cfg(test)]
#[path = "probing_tests.rs"]
mod tests;
