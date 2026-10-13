//! The `clock-v1` endpoint, over any clock controller's clocks.
//!
//! A request counts only once the kernel attests the caller holds the clock
//! link it quotes. A clock is held by every process instance that runs or
//! floors it, and its policy is the hardware's to state: an exact clock runs
//! at the rate its first holder set, another joining at that rate and never
//! retuning it, so one consumer's stream is never retimed under it; a floored
//! clock runs at no less than the highest floor any holder states. The last
//! holder's release, or its end, returns the clock unheld.

use alloc::vec::Vec;

use tairix_abi::driver::clock::{
    encode_describe_reply, encode_error_reply, encode_release_reply, encode_run_reply, ClockOp,
    ClockRequest, ClockState, CLOCK_MAX_REPLY,
};
use tairix_abi::hwlink::LinkRequest;
use tairix_abi::hwtree::HwResource;
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_drvrt::{refusal, Recorder, Supplier, SupplierHost};

/// How a clock is shared among its holders.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Policy {
    /// Run at the rate the first holder set; another holder joins at it.
    Exact,
    /// Run at no less than the highest floor any holder states.
    Floor,
    /// Run at the rate something else sets: a link may describe it, and
    /// never run or floor it.
    Fixed,
}

/// How a clock's change went, once made.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Halt {
    /// It changed cleanly.
    Clean,
    /// The old setting was cut off mid-period to make it.
    Cut,
}

/// A change a clock made: how it went, and the rate the clock now runs at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Applied {
    /// How the change went.
    pub halt: Halt,
    /// The rate the clock runs at now.
    pub hz: u64,
}

/// A controller's clocks, as the endpoint drives them.
pub trait ClockHardware {
    /// What running one clock at one rate takes.
    type Setting: Copy + Eq;

    /// How many clocks the endpoint serves.
    fn count(&self) -> usize;

    /// The clock a link's `selector` names, when the endpoint serves it.
    fn served(&self, selector: &[u32]) -> Option<usize>;

    /// The binding's id of clock `at`, as records name it.
    fn id(&self, at: usize) -> u32;

    /// How clock `at` is shared.
    fn policy(&self, at: usize) -> Policy;

    /// The rate clock `at` runs at; `0` while it is stopped.
    ///
    /// # Errors
    ///
    /// A register the rate could not be read from.
    fn rate(&self, at: usize) -> Result<u64, DriverError>;

    /// The highest rate clock `at` can run at: for a clock something else
    /// sets, the rate it runs at.
    ///
    /// # Errors
    ///
    /// A register the rate could not be read from.
    fn max_rate(&self, at: usize) -> Result<u64, DriverError>;

    /// The setting nearest `hz` for clock `at`: for a floored clock, the one
    /// asking for the lowest rate at or above `hz`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for a rate the clock cannot make.
    fn nearest(&self, at: usize, hz: u64) -> Result<Self::Setting, DriverError>;

    /// Run clock `at` at `setting`, answering the rate it then runs at, which
    /// only the hardware may know until it has made it.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a clock that would not change. An
    /// error leaves the clock as it was.
    fn run(&mut self, at: usize, setting: Self::Setting) -> Result<Applied, DriverError>;

    /// Return clock `at` unheld: stopped, or for a clock something else needs
    /// running, the rate it ran at before it was held.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for a clock that would not change.
    fn stop(&mut self, at: usize) -> Result<Halt, DriverError>;
}

/// A decision the endpoint records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Record {
    /// A clock was set running, retuned, or raised to a floor.
    Ran {
        /// The clock's binding id.
        clock: u32,
        /// The rate it runs at.
        hz: u64,
        /// The holder whose request changed it.
        holder: ProcId,
    },
    /// A process joined a clock another holds, at the rate it runs at.
    Joined {
        /// The clock's binding id.
        clock: u32,
        /// The new holder.
        holder: ProcId,
    },
    /// A clock's last holder gave it up, or ended, and it was returned.
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
    /// A clock's old setting was cut off mid-period.
    Cut {
        /// The clock's binding id.
        clock: u32,
    },
    /// A clock would not change; it is left as it is.
    Stuck {
        /// The clock's binding id.
        clock: u32,
    },
    /// A clock its last holder left could not be returned unheld; it runs on
    /// at the rate it was held at.
    Unreturned {
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

/// One holder of a clock: the instance, the link it holds it through, and
/// for a floored clock the floor it states.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Holder {
    instance: ProcId,
    link: LinkRequest,
    floor: u64,
}

struct Clock<S> {
    holders: Vec<Holder>,
    /// What the endpoint set the clock running at, while it is held.
    setting: Option<S>,
    /// The rate that setting made.
    hz: u64,
}

enum Answer {
    Described(ClockState),
    Running(u64),
    Released,
}

/// A clock controller's endpoint.
pub struct Controller<H: ClockHardware, S: SupplierHost, L: Recorder<Record>> {
    hardware: H,
    supplier: S,
    log: L,
    endpoint: u64,
    clocks: Vec<Clock<H::Setting>>,
}

impl<H: ClockHardware, S: SupplierHost, L: Recorder<Record>> Controller<H, S, L> {
    /// The endpoint `endpoint` for `hardware`, asking about its callers
    /// through `supplier` and recording to `log`. It touches no clock until
    /// asked.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the clocks' table cannot be made.
    pub fn new(hardware: H, supplier: S, log: L, endpoint: u64) -> Result<Self, Errno> {
        let mut clocks = Vec::new();
        clocks
            .try_reserve_exact(hardware.count())
            .map_err(|_| Errno::OutOfMemory)?;
        clocks.extend((0..hardware.count()).map(|_| Clock {
            holders: Vec::new(),
            setting: None,
            hz: 0,
        }));
        Ok(Self {
            hardware,
            supplier,
            log,
            endpoint,
            clocks,
        })
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
        let at = self
            .hardware
            .served(link.selector())
            .ok_or(Errno::NotSupported)?;
        let holder = |floor| Holder {
            instance: caller,
            link: *link,
            floor,
        };
        match (*request, self.hardware.policy(at)) {
            (ClockRequest::Describe(_), _) => self.describe(at, caller),
            (ClockRequest::Run { hz, .. }, Policy::Exact) => self.run(at, holder(0), hz),
            (ClockRequest::Floor { hz, .. }, Policy::Floor) => self.floor(at, holder(hz)),
            (ClockRequest::Run { .. }, Policy::Floor | Policy::Fixed)
            | (ClockRequest::Floor { .. }, Policy::Exact | Policy::Fixed) => {
                Err(Errno::NotSupported)
            }
            (ClockRequest::Release(_), _) => self.release(at, caller, link),
        }
    }

    fn describe(&self, at: usize, caller: ProcId) -> Result<Answer, Errno> {
        let hz = self.hardware.rate(at).map_err(refusal)?;
        let max_hz = self.hardware.max_rate(at).map_err(refusal)?;
        Ok(Answer::Described(ClockState {
            hz,
            max_hz: max_hz.max(hz),
            held_elsewhere: self.held_elsewhere(at, caller),
        }))
    }

    fn run(&mut self, at: usize, holder: Holder, hz: u64) -> Result<Answer, Errno> {
        let setting = self.hardware.nearest(at, hz).map_err(refusal)?;
        let retune = self.clocks[at].setting != Some(setting);
        if retune {
            self.drop_ended_holders(at, holder.instance)?;
            if self.held_elsewhere(at, holder.instance) {
                return Err(Errno::Busy);
            }
        }
        self.watch(holder.instance)?;
        if retune {
            self.change(at, setting, holder.instance)?;
        } else if !self.clocks[at].holders.contains(&holder)
            && self.held_elsewhere(at, holder.instance)
        {
            let id = self.hardware.id(at);
            self.log.record(Record::Joined {
                clock: id,
                holder: holder.instance,
            });
        }
        let clock = &mut self.clocks[at];
        if !clock.holders.contains(&holder) {
            clock.holders.push(holder);
        }
        Ok(Answer::Running(clock.hz))
    }

    fn floor(&mut self, at: usize, holder: Holder) -> Result<Answer, Errno> {
        self.drop_ended_holders(at, holder.instance)?;
        let highest = self.clocks[at]
            .holders
            .iter()
            .filter(|held| (held.instance, held.link) != (holder.instance, holder.link))
            .map(|held| held.floor)
            .fold(holder.floor, u64::max);
        let setting = self.hardware.nearest(at, highest).map_err(refusal)?;
        self.watch(holder.instance)?;
        if self.clocks[at].setting != Some(setting) {
            self.change(at, setting, holder.instance)?;
        }
        let clock = &mut self.clocks[at];
        match clock
            .holders
            .iter_mut()
            .find(|held| (held.instance, held.link) == (holder.instance, holder.link))
        {
            Some(held) => held.floor = holder.floor,
            None => clock.holders.push(holder),
        }
        Ok(Answer::Running(clock.hz))
    }

    /// Run clock `at` at `setting` for `instance`.
    fn change(&mut self, at: usize, setting: H::Setting, instance: ProcId) -> Result<(), Errno> {
        let applied = match self.hardware.run(at, setting) {
            Ok(applied) => applied,
            Err(err) => return Err(self.refuse_change(at, instance, err)),
        };
        self.applied(at, setting, applied, instance);
        Ok(())
    }

    /// Note that clock `at` now runs at `setting`, as `applied` made it, for
    /// `holder`.
    fn applied(&mut self, at: usize, setting: H::Setting, applied: Applied, holder: ProcId) {
        let id = self.hardware.id(at);
        self.note_halt(id, applied.halt);
        let clock = &mut self.clocks[at];
        clock.setting = Some(setting);
        clock.hz = applied.hz;
        self.log.record(Record::Ran {
            clock: id,
            hz: applied.hz,
            holder,
        });
    }

    /// Refuse `instance` a change of clock `at` that failed. The hardware
    /// left the clock as it was, so every hold already standing stands.
    fn refuse_change(&mut self, at: usize, instance: ProcId, err: DriverError) -> Errno {
        if err == DriverError::DeviceFault {
            let id = self.hardware.id(at);
            self.log.record(Record::Stuck { clock: id });
        }
        self.unwatch_if_idle(instance);
        refusal(err)
    }

    /// Drop the holds of each other holder of clock `at` that has ended,
    /// which the kernel's refusal to watch it says, so a consumer's restarted
    /// driver is not refused for its predecessor's hold.
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

    fn release(&mut self, at: usize, caller: ProcId, link: &LinkRequest) -> Result<Answer, Errno> {
        let clock = &mut self.clocks[at];
        let before = clock.holders.len();
        clock
            .holders
            .retain(|held| (held.instance, held.link) != (caller, *link));
        if clock.holders.len() == before {
            return Err(Errno::NotFound);
        }
        self.settle(at, caller);
        self.unwatch_if_idle(caller);
        Ok(Answer::Released)
    }

    /// Bring clock `at` to what its remaining holders need after one left: a
    /// floored clock down to the highest floor left, any clock unheld once
    /// nobody holds it.
    fn settle(&mut self, at: usize, leaving: ProcId) {
        let id = self.hardware.id(at);
        if self.clocks[at].holders.is_empty() {
            if self.clocks[at].setting.take().is_none() {
                return;
            }
            match self.hardware.stop(at) {
                Ok(halt) => {
                    self.note_halt(id, halt);
                    self.log.record(Record::Stopped { clock: id });
                }
                Err(_) => self.log.record(Record::Unreturned { clock: id }),
            }
            return;
        }
        if self.hardware.policy(at) != Policy::Floor {
            return;
        }
        let highest = self.clocks[at]
            .holders
            .iter()
            .map(|held| held.floor)
            .max()
            .unwrap_or(0);
        let Ok(setting) = self.hardware.nearest(at, highest) else {
            return;
        };
        if self.clocks[at].setting == Some(setting) {
            return;
        }
        match self.hardware.run(at, setting) {
            Ok(applied) => self.applied(at, setting, applied, leaving),
            Err(_) => self.log.record(Record::Stuck { clock: id }),
        }
    }

    fn note_halt(&mut self, clock: u32, halt: Halt) {
        if halt == Halt::Cut {
            self.log.record(Record::Cut { clock });
        }
    }

    fn holds_any(&self, instance: ProcId) -> bool {
        self.clocks
            .iter()
            .any(|clock| clock.holders.iter().any(|held| held.instance == instance))
    }

    /// Watch `instance` for its end, which its first hold needs.
    fn watch(&mut self, instance: ProcId) -> Result<(), Errno> {
        if self.holds_any(instance) {
            Ok(())
        } else {
            self.supplier.watch(instance)
        }
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

impl<H: ClockHardware, S: SupplierHost, L: Recorder<Record>> Supplier for Controller<H, S, L> {
    fn serve(&mut self, ticket: u64, frame: &[u8]) {
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

    /// Drop every hold `peer` had, returning each clock it held alone.
    fn peer_exited(&mut self, peer: ProcId) {
        for at in 0..self.clocks.len() {
            let clock = &mut self.clocks[at];
            let before = clock.holders.len();
            clock.holders.retain(|holder| holder.instance != peer);
            if clock.holders.len() != before {
                let id = self.hardware.id(at);
                self.log.record(Record::Abandoned {
                    clock: id,
                    holder: peer,
                });
                self.settle(at, peer);
            }
        }
        self.supplier.unwatch(peer);
    }
}
