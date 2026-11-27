//! A host that stops work at its first word, its clock moving on as it is
//! asked or as the harness moves it, and a kernel booted with it.

use std::cell::Cell;
use std::rc::Rc;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::os::{Ending, Host, Kernel, Pulse};
use tairix_riscos_kernel::relay::{Relayed, Side};

struct Still {
    clock: Rc<Cell<u64>>,
}

impl Host for Still {
    fn monotonic(&mut self) -> u64 {
        self.clock.get()
    }

    fn real_time(&mut self) -> u64 {
        self.clock.get()
    }

    fn pulse(&mut self) -> Pulse {
        Pulse::Stop
    }

    fn input(&mut self) -> Option<InputEvent> {
        None
    }

    fn relay(&mut self, _: Side, _request: &[u8], _reply: &mut Vec<u8>, _parking: bool) -> Relayed {
        Relayed::Failed
    }

    fn wait(&mut self, until: Option<u64>, _: Ending) -> Pulse {
        if let Some(until) = until {
            self.clock.set(self.clock.get().max(until));
        }
        Pulse::Stop
    }
}

/// A kernel ([`crate::boot::boot_with`]) on a still host, and its clock.
pub fn boot(rng: &mut Prng) -> (Kernel, Rc<Cell<u64>>) {
    let clock = Rc::new(Cell::new(0));
    let kernel = crate::boot::boot_with(
        rng,
        Box::new(Still {
            clock: Rc::clone(&clock),
        }),
    );
    (kernel, clock)
}
