//! What a task switch saves of a processor and gives back.

use super::{BadState, Cpu, State};
use crate::cp15::Task;
use crate::fpa::Fpa;
use crate::vfp::Vfp;

/// A processor's context: its registers, the FPA and whether it has a
/// context to work in, the VFP and Advanced SIMD state, and CP15's task
/// registers. The decoded pages, the rest of CP15 and the interrupt lines
/// are the machine's, and stay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Context {
    state: State,
    fpa: Fpa,
    fpa_active: bool,
    vfp: Vfp,
    task: Task,
}

impl Context {
    /// Its registers.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }
}

impl Cpu {
    /// The processor's context.
    #[must_use]
    pub fn context(&self) -> Context {
        Context {
            state: self.state(),
            fpa: self.fpa,
            fpa_active: self.fpa_active,
            vfp: self.vfp,
            task: self.cp15.task(),
        }
    }

    /// The context the processor's profile has at reset.
    #[must_use]
    pub fn reset_context(&self) -> Context {
        Self::new(self.profile, 0).context()
    }

    /// Makes `context` the processor's, the exclusive monitor and the event
    /// register cleared as a task switch clears them; refused, and nothing
    /// changed, if its state names a mode the profile does not have.
    pub fn resume(&mut self, context: &Context) -> Result<(), BadState> {
        self.set_state(&context.state)?;
        self.fpa = context.fpa;
        self.fpa_active = context.fpa_active;
        self.vfp = context.vfp;
        self.cp15.set_task(context.task);
        self.event = false;
        self.it_written = false;
        Ok(())
    }
}
