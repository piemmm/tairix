//! The seam between the model and what runs a device's work.
//!
//! The model checks, charges, files and orders everything a context asks;
//! a backend is what the work runs on: the memory it reaches, the form of
//! each object it reads, and the engines its jobs run on.

use alloc::sync::Arc;
use core::fmt::Debug;

use crate::binding::Bound;
use crate::command::Op;
use crate::device::Shared;
use crate::fair::FairGuard;
use crate::image::ImageDesc;
use crate::info::DeviceInfo;
use crate::memory::{Budget, Charge};
use crate::model::Model;
use crate::pipeline::StagePlan;
use crate::render::RenderPlan;
use crate::resource::MemoryRequirements;
use crate::Error;

/// What a device's work runs on.
pub(crate) trait Backend: Sized + Send + Sync + 'static {
    /// A context as the device holds it, let go once the model's context and
    /// the last of its memory and work are.
    type Context: Send + Sync;
    /// A memory as the device holds it, standing for the charges paying for
    /// it until its last holder lets it go.
    type Memory: Debug + Send + Sync;
    /// How an image's texels lie in its memory.
    type Image: Debug + Send + Sync;
    /// A bind set's descriptors as the device reads them.
    type Set: Debug + Send + Sync;
    /// A compute pipeline's program.
    type Compute: Debug + Send + Sync;
    /// A render pipeline's programs and the tables a draw reads.
    type Render: Debug + Send + Sync;
    /// What the device keeps for its work under its lock.
    type Pool: Pool;

    /// Whether the model's deadlines need watching from outside: a backend
    /// that meters its own work ends an overrun itself.
    const WATCHED: bool;

    /// The host bytes a memory of `size` keeps beside the bytes asked for.
    fn overhead(size: u64) -> u64;

    /// A context's form on the device, made without the device's lock: a
    /// device may be waited on for it.
    fn create_context(&self) -> Result<Self::Context, Error>;

    /// `size` zeroed bytes of memory type `memory_type` in `context`,
    /// standing for `charges`: its bytes, then what it keeps besides. Made
    /// without the device's lock.
    fn allocate(
        &self,
        context: &Arc<Self::Context>,
        memory: (u32, u64),
        charges: [Charge; 2],
    ) -> Result<Self::Memory, Error>;

    /// `context` was lost: a device running work it cannot stop lets the
    /// context go once that work is answered.
    fn lost(&self, context: &Arc<Self::Context>);

    /// Write `data` at `offset` from the CPU, the range proved within it.
    fn write(&self, memory: &Self::Memory, offset: u64, data: &[u8]) -> Result<(), Error>;

    /// Read `out.len()` bytes at `offset` from the CPU, the range proved
    /// within it.
    fn read(&self, memory: &Self::Memory, offset: u64, out: &mut [u8]) -> Result<(), Error>;

    /// How an image of `desc`, which the device of `info` admits, lies in
    /// memory, and what it needs of the memory it is placed in.
    fn image(
        &self,
        info: &DeviceInfo,
        desc: &ImageDesc,
    ) -> Result<(Self::Image, MemoryRequirements), Error>;

    /// The descriptors of `resources`, a bind set's every resource in its
    /// layout's order, and the host bytes they keep.
    fn set(&self, resources: &[Bound<Self>]) -> Result<(Self::Set, u64), Error>;

    /// A compute pipeline's program from its checked entry point.
    fn compute(
        &self,
        info: &DeviceInfo,
        stage: &StagePlan<'_>,
    ) -> Result<Prepared<Self, Self::Compute>, Error>;

    /// A render pipeline's programs and tables from its checked stages and
    /// state.
    fn render(
        &self,
        info: &DeviceInfo,
        plan: &RenderPlan<'_>,
    ) -> Result<Prepared<Self, Self::Render>, Error>;

    /// The host bytes a job of `ops` makes as it runs, beyond what they hold
    /// queued, on a device whose pool is `pool`.
    fn run_bytes(pool: &Self::Pool, ops: &[Op<Self>]) -> u64;

    /// Have the work the model holds ready run: `model` is the device's lock,
    /// held as the change that made it ready left it.
    fn drive(shared: &Shared<Self>, model: FairGuard<'_, Model<Self>>);
}

/// What a pipeline needs of its device's pool.
pub(crate) type Need<B> = <<B as Backend>::Pool as Pool>::Need;

/// What a device's pool lacks of a need, to be made without its lock.
pub(crate) type Plan<B> = <<B as Backend>::Pool as Pool>::Plan;

/// What a device's pool made or let go, to be installed or dropped without
/// its lock.
pub(crate) type Grown<B> = <<B as Backend>::Pool as Pool>::Grown;

/// What a live pipeline holds of its device's pool.
pub(crate) type Hold<B> = <<B as Backend>::Pool as Pool>::Hold;

/// What a running job borrows of its device's pool.
pub(crate) type Lease<B> = <<B as Backend>::Pool as Pool>::Lease;

/// A pipeline's form as its backend made it, what it needs of the pool, and
/// the host bytes it keeps besides its working memory.
pub(crate) struct Prepared<B: Backend, P> {
    pub(crate) form: P,
    pub(crate) need: Need<B>,
    pub(crate) held: u64,
}

/// What a backend keeps for the work it runs, under the device's lock:
/// memory pipelines reserve when made and their jobs borrow as they run.
pub(crate) trait Pool: Send + Sized {
    /// What a pipeline needs of the pool.
    type Need: Copy + Default + Send + Sync;
    /// What a live pipeline holds of the pool.
    type Hold: Send + Sync;
    /// What the pool lacks of a need, to be made without the device's lock.
    type Plan: Send;
    /// What a plan made, or what the pool let go, to be dropped or installed
    /// without the device's lock.
    type Grown: Send + Default;
    /// What a running job borrows of the pool.
    type Lease: Send;

    /// The larger of two needs, part by part.
    fn wider(a: Self::Need, b: Self::Need) -> Self::Need;

    /// Fit the pool to the pipelines still alive, answering what it let go.
    fn prune(&mut self) -> Self::Grown;

    /// Room for a pipeline needing `need`, which charges its context's
    /// `budget` and the `heap` `besides` bytes beside: the claim charged,
    /// and what the pool lacks of it, to be made; and what the pool let go.
    fn plan(
        &mut self,
        need: Self::Need,
        besides: (&Arc<Budget>, u64),
        heap: &Arc<Budget>,
    ) -> (Result<Self::Plan, Error>, Self::Grown);

    /// What `plan` lacked, made, and the pipeline's hold it was planned for.
    fn make(plan: Self::Plan) -> Result<(Self::Hold, Self::Grown), Error>;

    /// Install what a plan made, answering what the pool let go.
    fn install(&mut self, grown: Self::Grown) -> Self::Grown;

    /// Lend what a job needing `need` runs in.
    fn take(&mut self, need: Self::Need) -> Self::Lease;

    /// Take back what a job ran in, answering what the pool let go.
    fn give(&mut self, lease: Self::Lease) -> [Self::Grown; 2];
}

/// The pool of a backend that keeps none.
pub(crate) struct NoPool;

impl Pool for NoPool {
    type Need = ();
    type Hold = ();
    type Plan = ();
    type Grown = ();
    type Lease = ();

    fn wider((): (), (): ()) {}

    fn prune(&mut self) {}

    fn plan(&mut self, (): (), _: (&Arc<Budget>, u64), _: &Arc<Budget>) -> (Result<(), Error>, ()) {
        (Ok(()), ())
    }

    fn make((): ()) -> Result<((), ()), Error> {
        Ok(((), ()))
    }

    fn install(&mut self, (): ()) {}

    fn take(&mut self, (): ()) {}

    fn give(&mut self, (): ()) -> [(); 2] {
        [(), ()]
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
pub(crate) mod tests;
