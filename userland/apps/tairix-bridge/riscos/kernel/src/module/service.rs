//! Service calls (`plans/RISCOS-EMULATOR.md` §4.2): a call offered round the
//! module list in its order, each module's incarnations in theirs, until a
//! handler claims it by zeroing R1. A guest module is offered every service
//! at its service entry; a built-in one only the services it lists.
//!
//! The walk keeps its place by the module it reached, so a handler that
//! kills, loads or reinitialises modules leaves it sound: one gone is not
//! called, and one linked past the place is reached in its turn.

use super::header::Field;
use super::{ModuleKey, ServiceCode, Shape};
use crate::os::{Entry, Fail, Os, Regs, Target, Unwind};

/// How a module is offered services.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Handler {
    /// A guest module's service entry, offered every service.
    Guest(Target),
    /// A built-in module's Rust, offered the services it lists.
    Builtin {
        code: ServiceCode,
        numbers: &'static [u32],
    },
}

impl Handler {
    fn takes(&self, number: u32) -> bool {
        match self {
            Self::Guest(_) => true,
            Self::Builtin { numbers, .. } => numbers.contains(&number),
        }
    }
}

/// Where a walk round the module list has got to: the module at `at` when
/// it was reached, and the next of its incarnations to offer the call to.
struct Place {
    at: usize,
    module: Option<ModuleKey>,
    incarnation: usize,
}

impl Os<'_> {
    /// How a module of `shape` is offered services; none for one with no
    /// service entry.
    pub(crate) fn service_handler(&self, shape: &Shape) -> Option<Handler> {
        if let Some(rom) = shape.rom {
            let services = self.world.modules.builtin(rom)?.services.as_ref()?;
            return Some(Handler::Builtin {
                code: services.handler,
                numbers: services.numbers,
            });
        }
        let entry = shape.header.get(Field::Service);
        (entry != 0).then(|| {
            Handler::Guest(Target {
                address: shape.code.wrapping_add(entry),
                bits: shape.bits,
            })
        })
    }

    /// Offers the service in R1 with `regs` round the modules until one
    /// claims it: R1 zero then, and R0 and R2 to R8 as the handlers left
    /// them.
    pub fn service(&mut self, regs: &mut Regs) -> Result<(), Unwind> {
        match regs.r[1] {
            crate::service::CLAIM_FIQ | crate::service::CLAIM_FIQ_IN_BACKGROUND => {
                self.claim_fiq(regs)
            }
            crate::service::RELEASE_FIQ => self.release_fiq(regs),
            _ => self.issue(regs).map(drop),
        }
    }

    /// `OS_ServiceCall`.
    pub(crate) fn service_call(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        self.service(regs)?;
        Ok(())
    }

    /// Offers `regs` round the module list; whether a handler claimed it.
    /// Each handler is offered the service's number, whatever the last left
    /// in R1 short of claiming it.
    fn issue(&mut self, regs: &mut Regs) -> Result<bool, Unwind> {
        let number = regs.r[1];
        let mut place = Place {
            at: 0,
            module: None,
            incarnation: 0,
        };
        while let Some((private, handler)) = self.next_offered(&mut place, number) {
            let mut call = *regs;
            call.r[1] = number;
            call.r[12] = private;
            match handler {
                Handler::Builtin { code, .. } => code(self, &mut call)?,
                Handler::Guest(target) => {
                    self.call(target, &mut call, Entry::Supervisor)?;
                }
            }
            regs.r[0] = call.r[0];
            regs.r[2..9].copy_from_slice(&call.r[2..9]);
            if call.r[1] == 0 {
                regs.r[1] = 0;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The next incarnation past `place` that takes service `number`: its
    /// private word's address and its module's handler. The place is found
    /// again by its module, which a handler may have moved or killed.
    fn next_offered(&self, place: &mut Place, number: u32) -> Option<(u32, Handler)> {
        let modules = &self.world.modules;
        let chain = modules.chain();
        loop {
            if let Some(key) = place
                .module
                .filter(|&key| chain.get(place.at) != Some(&key))
            {
                if let Some(at) = modules.place_of(key) {
                    place.at = at;
                } else {
                    place.module = None;
                    place.incarnation = 0;
                }
            }
            let key = if let Some(key) = place.module {
                key
            } else {
                let &key = chain.get(place.at)?;
                place.module = Some(key);
                key
            };
            if let Some(module) = modules.get(key) {
                if let Some(handler) = module.services.filter(|h| h.takes(number)) {
                    if let Some(incarnation) = module.incarnations.get(place.incarnation) {
                        place.incarnation += 1;
                        return Some((incarnation.private(), handler));
                    }
                }
            }
            place.at += 1;
            place.module = None;
            place.incarnation = 0;
        }
    }

    /// `Service_ClaimFIQ`: the kernel, owning FIQ from reset, gives it up
    /// itself; otherwise the owner is asked to.
    fn claim_fiq(&mut self, regs: &mut Regs) -> Result<(), Unwind> {
        if core::mem::take(&mut self.world.modules.kernel_has_fiq) {
            regs.r[1] = 0;
            return Ok(());
        }
        self.issue(regs).map(drop)
    }

    /// `Service_ReleaseFIQ`: offered round the modules; one no module claims
    /// comes back to the kernel, its vector the kernel's again.
    fn release_fiq(&mut self, regs: &mut Regs) -> Result<(), Unwind> {
        if self.issue(regs)? {
            return Ok(());
        }
        if !core::mem::replace(&mut self.world.modules.kernel_has_fiq, true) {
            let _ = crate::host::restore_vector(self.memory, crate::host::FIQ);
        }
        regs.r[1] = 0;
        Ok(())
    }
}
