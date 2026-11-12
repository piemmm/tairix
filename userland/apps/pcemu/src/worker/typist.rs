//! Text typed on the machine's keyboard: a character at a time, each begun
//! only once the guest has read every key before it, so nothing typed is lost
//! to a full buffer or to a program that empties the buffer before it reads.

use alloc::collections::TryReserveError;
use alloc::collections::VecDeque;

use crate::bios::{self, Keystrokes};
use crate::link::TYPE_WINDOW;
use crate::machine::Machine;

/// What waits to be typed, and the character under way.
#[derive(Debug)]
pub struct Typist {
    waiting: VecDeque<u8>,
    under_way: Keystrokes,
    /// The next of the character's key events to press.
    next: usize,
    /// Characters begun or left untyped since last reported.
    taken: u32,
}

/// The front-end gave more to type than [`TYPE_WINDOW`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overfull;

impl Typist {
    /// A typist with nothing to type, its whole window held.
    ///
    /// # Errors
    ///
    /// The heap's refusal of the window.
    pub fn new() -> Result<Self, TryReserveError> {
        let mut waiting = VecDeque::new();
        waiting.try_reserve_exact(TYPE_WINDOW)?;
        Ok(Self {
            waiting,
            under_way: Keystrokes::default(),
            next: 0,
            taken: 0,
        })
    }

    /// Keep `codes` to type after what already waits.
    ///
    /// # Errors
    ///
    /// [`Overfull`] past [`TYPE_WINDOW`], keeping none of them.
    pub fn take(&mut self, codes: &[u8]) -> Result<(), Overfull> {
        if self.waiting.len() + codes.len() > TYPE_WINDOW {
            return Err(Overfull);
        }
        self.waiting.extend(codes);
        Ok(())
    }

    /// Leave what waits untyped; the character under way is finished.
    pub fn stop(&mut self) {
        let left = u32::try_from(self.waiting.len()).unwrap_or(u32::MAX);
        self.taken = self.taken.saturating_add(left);
        self.waiting.clear();
    }

    /// Leave everything untyped, the character under way too: the keyboard
    /// it was being typed on is gone.
    pub fn clear(&mut self) {
        self.stop();
        self.under_way = Keystrokes::default();
        self.next = 0;
    }

    /// The characters begun or left untyped since last asked, if any were.
    pub fn take_taken(&mut self) -> Option<u32> {
        (self.taken > 0).then(|| core::mem::take(&mut self.taken))
    }

    /// Press what the keyboard has room for of the character under way,
    /// then, where `may_begin`, begin the next once the guest has read every
    /// key before it.
    pub fn feed(&mut self, machine: &mut Machine, may_begin: bool) {
        loop {
            if let Some(&(key, down)) = self.under_way.as_slice().get(self.next) {
                let board = machine.board_mut();
                if !board.key_fits(key, down) {
                    return;
                }
                board.key(key, down);
                self.next += 1;
                continue;
            }
            if !may_begin || !read_up(machine) {
                return;
            }
            let Some(code) = self.waiting.pop_front() else {
                return;
            };
            self.taken = self.taken.saturating_add(1);
            self.under_way = bios::keystrokes(code, bios::caps_lock(machine));
            self.next = 0;
        }
    }
}

/// Whether the guest has read every key typed so far: none held, none on its
/// way from the keyboard, none waiting in the BIOS's buffer.
fn read_up(machine: &Machine) -> bool {
    let board = machine.board();
    !board.keys_held()
        && board.keyboard_idle()
        && bios::keys_waiting(machine).is_none_or(|waiting| waiting == 0)
}

#[cfg(test)]
#[path = "typist_tests.rs"]
mod tests;
