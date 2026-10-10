//! The `clock-v1` endpoint, over the clock manager's generators.
//!
//! A clock is held by every process instance that runs it and runs at the
//! rate the first of them set: another may join it at that rate, never retune
//! it, so one consumer's stream is never retimed under it by another. The
//! last holder's release, or its end, stops the clock. A request counts only
//! once the kernel attests the caller holds the clock link it quotes.

use alloc::vec::Vec;

use tairix_abi::driver::clock::{
    encode_describe_reply, encode_error_reply, encode_release_reply, encode_run_reply, ClockOp,
    ClockRequest, ClockState, CLOCK_MAX_REPLY,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId, RegisterBlock};
use tairix_drvrt::SupplierHost;

use crate::cprman::{Cprman, Generator, Halted, Setting, PCM, PWM};

/// The binding's id of the PWM blocks' clock.
pub const CLOCK_PWM: u32 = 30;

/// The binding's id of the PCM block's clock.
pub const CLOCK_PCM: u32 = 31;

/// Every clock the endpoint serves, by its binding id. The rest are the
/// firmware's: it retunes the cores', the memory's and the buses' as it
/// pleases, and the board's own wiring may depend on the general-purpose
/// ones.
const SERVED: [(u32, Generator); 2] = [(CLOCK_PWM, PWM), (CLOCK_PCM, PCM)];

/// A decision the endpoint records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A clock was set running for its first holder, or retuned by its only
    /// one.
    Ran {
        /// The clock's binding id.
        clock: u32,
        /// The rate it runs at.
        hz: u64,
        /// Its holder.
        holder: ProcId,
    },
    /// A process joined a clock another holds, at the rate it runs at.
    Joined {
        /// The clock's binding id.
        clock: u32,
        /// The new holder.
        holder: ProcId,
    },
    /// A clock's last holder gave it up, or ended, and it was stopped.
    Stopped {
        /// The clock's binding id.
        clock: u32,
    },
    /// A holder ended still holding a clock.
    Abandoned {
        /// The clock's binding id.
        clock: u32,
        /// The holder that ended.
        holder: ProcId,
    },
    /// A generator did not finish its period, and was reset mid-period.
    Killed {
        /// The clock's binding id.
        clock: u32,
    },
    /// A generator kept running through its reset; it is left as it is.
    Stuck {
        /// The clock's binding id.
        clock: u32,
    },
    /// A request was refused.
    Refused {
        /// The operation, when the frame named one.
        op: Option<ClockOp>,
        /// The caller, when the kernel could say.
        caller: Option<ProcId>,
        /// Why.
        reason: Errno,
    },
}

/// Where the endpoint's decisions go.
pub trait Recorder {
    /// Record a decision.
    fn record(&mut self, record: Record);
}

/// One holder of a clock: the instance, and the link it holds it through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Holder {
    instance: ProcId,
    link: LinkRequest,
}

struct Clock {
    id: u32,
    generator: Generator,
    holders: Vec<Holder>,
    /// What the endpoint set the generator running at, while it is held.
    setting: Option<Setting>,
}

enum Answer {
    Described(ClockState),
    Running(u64),
    Released,
}

/// The reply a refusal from the hardware carries.
fn refusal(err: DriverError) -> Errno {
    match err {
        DriverError::Unsupported => Errno::NotSupported,
        DriverError::OutOfRange => Errno::OutOfRange,
        _ => Errno::DeviceFault,
    }
}

/// A clock manager's endpoint.
pub struct Controller<'r, R: RegisterBlock + ?Sized, S: SupplierHost, L: Recorder> {
    cprman: Cprman<'r, R>,
    supplier: S,
    log: L,
    endpoint: u64,
    clocks: [Clock; SERVED.len()],
}

impl<'r, R: RegisterBlock + ?Sized, S: SupplierHost, L: Recorder> Controller<'r, R, S, L> {
    /// The endpoint `endpoint` for `cprman`, asking about its callers through
    /// `supplier` and recording to `log`. It touches no clock until asked.
    pub fn new(cprman: Cprman<'r, R>, supplier: S, log: L, endpoint: u64) -> Self {
        Self {
            cprman,
            supplier,
            log,
            endpoint,
            clocks: SERVED.map(|(id, generator)| Clock {
                id,
                generator,
                holders: Vec::new(),
                setting: None,
            }),
        }
    }

    /// Serve the call `ticket` carrying `frame`, answering it.
    pub fn serve(&mut self, ticket: u64, frame: &[u8]) {
        let caller = match self.supplier.caller(ticket) {
            Ok(caller) => caller,
            Err(reason) => return self.refuse(ticket, None, None, reason),
        };
        let request = match ClockRequest::decode(frame) {
            Ok(request) => request,
            Err(reason) => return self.refuse(ticket, None, Some(caller), reason),
        };
        match self.answer(ticket, caller, &request) {
            Ok(answer) => self.send(ticket, &answer),
            Err(reason) => self.refuse(ticket, Some(request.op()), Some(caller), reason),
        }
    }

    /// Drop every hold `peer` had, stopping each clock it held alone; it has
    /// ended.
    pub fn peer_exited(&mut self, peer: ProcId) {
        for at in 0..self.clocks.len() {
            let clock = &mut self.clocks[at];
            let before = clock.holders.len();
            clock.holders.retain(|holder| holder.instance != peer);
            if clock.holders.len() != before {
                let id = clock.id;
                self.log.record(Record::Abandoned {
                    clock: id,
                    holder: peer,
                });
                self.release_if_unheld(at);
            }
        }
        self.supplier.unwatch(peer);
    }

    fn answer(
        &mut self,
        ticket: u64,
        caller: ProcId,
        request: &ClockRequest,
    ) -> Result<Answer, Errno> {
        let link = request.link();
        if link.endpoint() != self.endpoint {
            return Err(Errno::OutOfRange);
        }
        if !self
            .supplier
            .caller_holds(ticket, &HwResource::request(link))?
        {
            return Err(Errno::PermissionDenied);
        }
        let at = Self::served(link)?;
        match *request {
            ClockRequest::Describe(_) => self.describe(at, caller),
            ClockRequest::Run { hz, .. } => self.run(
                at,
                Holder {
                    instance: caller,
                    link: *link,
                },
                hz,
            ),
            ClockRequest::Release(_) => self.release(
                at,
                Holder {
                    instance: caller,
                    link: *link,
                },
            ),
        }
    }

    /// The clock `link` names, when the endpoint serves it.
    fn served(link: &LinkRequest) -> Result<usize, Errno> {
        let &[id] = link.selector() else {
            return Err(Errno::NotSupported);
        };
        SERVED
            .iter()
            .position(|&(served, _)| served == id)
            .ok_or(Errno::NotSupported)
    }

    fn describe(&self, at: usize, caller: ProcId) -> Result<Answer, Errno> {
        let hz = self
            .cprman
            .rate(self.clocks[at].generator)
            .map_err(refusal)?;
        Ok(Answer::Described(ClockState {
            hz,
            held_elsewhere: self.held_elsewhere(at, caller),
        }))
    }

    fn run(&mut self, at: usize, holder: Holder, hz: u64) -> Result<Answer, Errno> {
        let (setting, made) = self.cprman.nearest(hz).map_err(refusal)?;
        let retune = self.clocks[at].setting != Some(setting);
        if retune {
            self.drop_ended_holders(at, holder.instance)?;
            if self.held_elsewhere(at, holder.instance) {
                return Err(Errno::Busy);
            }
        }
        if !self.holds_any(holder.instance) {
            self.supplier.watch(holder.instance)?;
        }
        let (id, generator) = (self.clocks[at].id, self.clocks[at].generator);
        if retune {
            match self.cprman.run(generator, setting) {
                Ok(halted) => self.note_halt(id, halted),
                Err(err) => return Err(self.abandon_run(at, holder.instance, err)),
            }
            self.log.record(Record::Ran {
                clock: id,
                hz: made,
                holder: holder.instance,
            });
        } else if !self.clocks[at].holders.contains(&holder)
            && self.held_elsewhere(at, holder.instance)
        {
            self.log.record(Record::Joined {
                clock: id,
                holder: holder.instance,
            });
        }
        let clock = &mut self.clocks[at];
        clock.setting = Some(setting);
        if !clock.holders.contains(&holder) {
            clock.holders.push(holder);
        }
        Ok(Answer::Running(made))
    }

    /// Forget the clock at `at` after a run that failed: whatever the holder
    /// had it running at went with the attempt.
    fn abandon_run(&mut self, at: usize, instance: ProcId, err: DriverError) -> Errno {
        let clock = &mut self.clocks[at];
        clock.holders.clear();
        clock.setting = None;
        let id = clock.id;
        if err == DriverError::DeviceFault {
            self.log.record(Record::Stuck { clock: id });
        }
        self.unwatch_if_idle(instance);
        refusal(err)
    }

    /// Drop the holds of each other holder of the clock at `at` that has
    /// ended, which the kernel's refusal to watch it says, so a consumer's
    /// restarted driver is not refused for its predecessor's hold.
    fn drop_ended_holders(&mut self, at: usize, caller: ProcId) -> Result<(), Errno> {
        let mut others: Vec<ProcId> = Vec::new();
        for held in &self.clocks[at].holders {
            if held.instance != caller && !others.contains(&held.instance) {
                others.push(held.instance);
            }
        }
        for other in others {
            match self.supplier.watch(other) {
                Ok(()) => {}
                Err(Errno::NotFound) => self.peer_exited(other),
                Err(reason) => return Err(reason),
            }
        }
        Ok(())
    }

    fn held_elsewhere(&self, at: usize, instance: ProcId) -> bool {
        self.clocks[at]
            .holders
            .iter()
            .any(|holder| holder.instance != instance)
    }

    fn release(&mut self, at: usize, holder: Holder) -> Result<Answer, Errno> {
        let clock = &mut self.clocks[at];
        let before = clock.holders.len();
        clock.holders.retain(|held| *held != holder);
        if clock.holders.len() == before {
            return Err(Errno::NotFound);
        }
        self.release_if_unheld(at);
        self.unwatch_if_idle(holder.instance);
        Ok(Answer::Released)
    }

    /// Stop the clock at `at` once nobody holds it.
    fn release_if_unheld(&mut self, at: usize) {
        let clock = &mut self.clocks[at];
        if !clock.holders.is_empty() || clock.setting.take().is_none() {
            return;
        }
        let (id, generator) = (clock.id, clock.generator);
        match self.cprman.stop(generator) {
            Ok(halted) => {
                self.note_halt(id, halted);
                self.log.record(Record::Stopped { clock: id });
            }
            Err(_) => self.log.record(Record::Stuck { clock: id }),
        }
    }

    fn note_halt(&mut self, clock: u32, halted: Halted) {
        if halted == Halted::Killed {
            self.log.record(Record::Killed { clock });
        }
    }

    fn holds_any(&self, instance: ProcId) -> bool {
        self.clocks
            .iter()
            .any(|clock| clock.holders.iter().any(|held| held.instance == instance))
    }

    fn unwatch_if_idle(&mut self, instance: ProcId) {
        if !self.holds_any(instance) {
            self.supplier.unwatch(instance);
        }
    }

    fn send(&mut self, ticket: u64, answer: &Answer) {
        let mut out = [0u8; CLOCK_MAX_REPLY];
        let encoded = match *answer {
            Answer::Described(state) => encode_describe_reply(&mut out, state),
            Answer::Running(hz) => encode_run_reply(&mut out, hz),
            Answer::Released => encode_release_reply(&mut out),
        };
        // A caller that ends before its answer arrives has its exit do the
        // cleaning up; there is nobody left to tell.
        if let Ok(len) = encoded {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }

    fn refuse(&mut self, ticket: u64, op: Option<ClockOp>, caller: Option<ProcId>, reason: Errno) {
        self.log.record(Record::Refused { op, caller, reason });
        let mut out = [0u8; CLOCK_MAX_REPLY];
        if let Ok(len) = encode_error_reply(&mut out, reason) {
            let _ = self.supplier.reply(ticket, &out[..len]);
        }
    }
}
