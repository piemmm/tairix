//! The debugger's side of the worker (`plans/PCEMU.md` §3.1): the probe put
//! on the board and taken off as the front-end asks, the machine held still
//! at a stop, and every look answered from the machine as it stands.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::{Link, Memory, Worker};
use crate::cpu::{linear, Bus as _, Seg};
use crate::debug::disasm::{listing, Reading};
use crate::debug::probe::Probe;
use crate::debug::Registers;
use crate::link::debug::{Debug, Listing, Look, Said, Seen};
use crate::link::Event;
use tairix_sandbox::session::ShareWrite;

/// The debugger's hold on the machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Debugging {
    /// Its probe is on the board.
    pub attached: bool,
    /// The probe stopped the processor, which waits to be sent on.
    pub stopped: bool,
    /// A stop was asked for and is not yet made: the machine runs to it
    /// even held.
    pub breaking: bool,
}

impl<L: Link + 'static, S: ShareWrite, M: Memory> Worker<L, S, M> {
    /// Does what the debugger asks, the order it asks in kept: any request
    /// out of turn ends the session.
    pub(super) fn debug(&mut self, debug: &Debug) {
        let Debugging {
            attached, stopped, ..
        } = self.run.debugging;
        match debug {
            Debug::Attach if !attached => {
                let probe = Box::new(Probe::new(self.run.config.cpu));
                self.run.machine.board_mut().attach_probe(probe);
                self.run.debugging.attached = true;
            }
            Debug::Detach if attached => {
                self.run.machine.board_mut().detach_probe();
                self.run.debugging = Debugging::default();
                if stopped {
                    self.rebase();
                }
                self.send(&Event::Debug(Said::Detached));
            }
            Debug::Add(breakpoint) if attached => {
                if !self.run.machine.board_mut().add_breakpoint(*breakpoint) {
                    self.violation("a breakpoint the probe cannot hold");
                }
            }
            Debug::Remove(breakpoint) if attached => {
                if !self.run.machine.board_mut().remove_breakpoint(*breakpoint) {
                    self.violation("a breakpoint removed that was not held");
                }
            }
            // A stop already made answers a break asked before it was seen.
            Debug::Break if attached => {
                if !stopped {
                    self.run.machine.board_mut().request_break();
                    self.run.debugging.breaking = true;
                }
            }
            Debug::Go(go) if stopped => {
                self.run.machine.go(*go);
                self.run.debugging.stopped = false;
                self.rebase();
            }
            Debug::Look(look) if attached => self.look(look),
            _ => self.violation("a debugger request out of turn"),
        }
    }

    /// Whether the machine moves on in emulated time: run, or held only to
    /// reach a stop asked for, and not stopped.
    pub(super) const fn advancing(&self) -> bool {
        let debugging = self.run.debugging;
        !debugging.stopped && (self.run.running || debugging.breaking)
    }

    /// The processor stopped: tells the front-end why, and how it stands.
    pub(super) fn stopped(&mut self) {
        let Some(reason) = self.run.machine.board_mut().take_stop() else {
            return;
        };
        self.run.debugging.stopped = true;
        self.run.debugging.breaking = false;
        let machine = &self.run.machine;
        let registers = Registers::of(machine.cpu(), machine.board().coprocessor());
        self.send(&Event::Debug(Said::Stopped { reason, registers }));
    }

    /// A probe for the machine built afresh, holding what `old` held, and
    /// no stop. The reset is said whether or not the processor was stopped:
    /// the front-end cannot know whether a stop was made before the reset
    /// reached the worker.
    pub(super) fn probe_again(&mut self, old: Option<Box<Probe>>) {
        let Some(old) = old else {
            return;
        };
        let mut probe = Box::new(Probe::new(self.run.config.cpu));
        probe.set(old.breakpoints());
        self.run.machine.board_mut().attach_probe(probe);
        self.run.debugging.breaking = false;
        self.run.debugging.stopped = false;
        self.send(&Event::Debug(Said::Reset));
    }

    /// Answers `look` from the machine as it stands.
    fn look(&mut self, look: &Look) {
        let machine = &self.run.machine;
        let (cpu, board) = (machine.cpu(), machine.board());
        let reading = Reading {
            model: cpu.model(),
            emulating: cpu.in_8080_mode(),
        };
        let (before, after) = (usize::from(look.lines.0), usize::from(look.lines.1));
        let lines = listing(reading, board, look.code, (before, after));
        let leading = lines.len().saturating_sub(after);
        let (before, after, encoded) = Listing::encode(&lines, leading);
        let read = |segment: u16, offset: u16, len: u16| -> Vec<u8> {
            (0..len)
                .map(|n| board.peek(linear(segment, offset.wrapping_add(n))))
                .collect()
        };
        let regs = cpu.regs();
        let memory = read(look.memory.0, look.memory.1, look.bytes);
        let stack = read(regs.sreg(Seg::Ss), regs.sp(), u16::from(look.stack) * 2);
        let seen = Seen {
            number: look.number,
            listing: Listing::new(before, after, &encoded),
            memory: &memory,
            stack: &stack,
        };
        self.send(&Event::Debug(Said::Seen(seen)));
    }
}
