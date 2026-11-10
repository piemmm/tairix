//! Compute pipelines: an entry point of a shader module, its pipeline layout
//! and its specialization, lowered once when made, with the working memory
//! its dispatches run in reserved from the device then.

use alloc::sync::Arc;

use tairix_collections::Key;
use tairix_spirv::{Admitted, EntryPoint, SpecError, SpecValue, Stage};

use crate::binding::{BindSet, PipelineLayout, PipelineLayoutState, ShaderModule};
use crate::command::{push_words, set_keys, Command, Count, DispatchRecord};
use crate::device::{owned, Buffer, CommandBuffer, Context};
use crate::info::Limits;
use crate::memory::Charge;
use crate::model::{
    shared_bytes, ComputePipelineKey, ContextKey, ContextState, Model, PickPipelines, Pipelines,
};
use crate::software::dispatch::{self, Grown, Hold, Need};
use crate::software::shader::ir::{Program, LANES};
use crate::software::shader::lower;
use crate::Error;

/// How a compute pipeline is made.
#[derive(Copy, Clone, Debug)]
pub struct ComputePipelineDesc<'a> {
    /// The module holding its entry point.
    pub module: &'a ShaderModule,
    /// The entry point's name: a compute entry point of the module.
    pub entry_point: &'a str,
    /// The layout its bind sets and push constants follow.
    pub layout: &'a PipelineLayout,
    /// The values its specialization constants take, by `SpecId`.
    pub specialization: &'a [SpecValue],
}

/// A pipeline as its commands run it: its lowered stages `P`, its layout,
/// and its need of the device's workspaces, held while it lives.
pub(crate) struct PipelineState<P> {
    pub(crate) stages: P,
    pub(crate) layout: Arc<PipelineLayoutState>,
    pub(crate) need: Need,
    _hold: Hold,
    _charge: Charge,
}

/// A compute pipeline: its one program.
pub(crate) type ComputePipelineState = PipelineState<Program>;

impl<P> core::fmt::Debug for PipelineState<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PipelineState").finish_non_exhaustive()
    }
}

impl<P> PipelineState<P> {
    /// Its hold let go, so the workspaces can be fitted without it; its
    /// stages and the charge for them answered, to be let go without the
    /// device's lock.
    fn let_go(self) -> (P, Charge) {
        let Self {
            stages,
            _hold: hold,
            _charge: charge,
            ..
        } = self;
        drop(hold);
        (stages, charge)
    }
}

/// What a pipeline's making or release let go, to be dropped without the
/// device's lock: what the workspaces let go, and its stages with the charge
/// for them.
pub(crate) struct Released<P> {
    _grown: [Grown; 2],
    _stages: Option<P>,
    // After the stages, so it stands for them until they are gone.
    _charge: Option<Charge>,
}

impl<P> Default for Released<P> {
    fn default() -> Self {
        Self {
            _grown: [Grown::default(), Grown::default()],
            _stages: None,
            _charge: None,
        }
    }
}

impl Context {
    /// A compute pipeline: its entry point agreed with its layout,
    /// specialized, and lowered.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a module or layout of another context,
    /// [`Error::EntryPoint`] for a name no compute entry point has,
    /// [`Error::Disagreement`] for a layout that does not provide what the
    /// entry point uses, [`Error::Specialization`] for values the module
    /// refuses, [`Error::TooLarge`] for a workgroup, a working set or a
    /// program past the device's, and [`Error::OutOfMemory`] or
    /// [`Error::HeapFull`] for working memory, host memory or table room the
    /// device cannot reserve, asked before any working memory is made.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_compute_pipeline(
        &self,
        desc: &ComputePipelineDesc<'_>,
    ) -> Result<ComputePipeline, Error> {
        owned(
            &desc.module.shared,
            desc.module.context,
            &self.shared,
            self.key,
        )?;
        owned(
            &desc.layout.shared,
            desc.layout.context,
            &self.shared,
            self.key,
        )?;
        let (module, layout) = {
            let model = self.shared.model.lock();
            let context = model.live(self.key)?;
            let module = context
                .shader_modules
                .get(desc.module.key)
                .cloned()
                .ok_or(Error::Stale)?;
            let layout = context
                .pipeline_layouts
                .get(desc.layout.key)
                .cloned()
                .ok_or(Error::Stale)?;
            (module, layout)
        };
        let limits = &self.shared.info.limits;
        let program = lower_compute(limits, &module.admitted, desc, &layout)?;
        let (need, held) = (dispatch::need(&program), held_bytes(&program));
        Ok(ComputePipeline {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key: self.file_pipeline(computes, (program, layout), (need, held))?,
        })
    }

    /// File `stages` under `layout` as a pipeline of the kind `pick` names,
    /// needing `need` of the workspaces and keeping `held` bytes beside: its
    /// room and charges planned, its working memory made between two holds
    /// of the device's lock, as zeroing it takes as long as it is large, and
    /// filed.
    pub(crate) fn file_pipeline<K: Key, P>(
        &self,
        pick: PickPipelines<K, Arc<PipelineState<P>>>,
        (stages, layout): (P, Arc<PipelineLayoutState>),
        (need, held): (Need, u64),
    ) -> Result<K, Error> {
        let (planned, released) =
            self.shared
                .model
                .lock()
                .plan_pipeline((self.key, pick), need, held);
        drop(released);
        let (plan, charge) = planned?;
        let grown = match plan.make() {
            Ok(grown) => grown,
            Err(error) => {
                self.shared
                    .model
                    .lock()
                    .unreserve_pipeline((self.key, pick));
                return Err(error);
            }
        };
        let (key, released) = self.shared.model.lock().create_pipeline(
            (self.key, pick),
            (stages, layout, need),
            (grown, charge),
        );
        drop(released);
        key
    }
}

/// Each of `program`'s bindings placed among the descriptors of the bind set
/// its layout takes there, and their total: fixed for every set a dispatch
/// binds, as each serves its layout's set.
fn place_bindings(program: &mut Program, layout: &PipelineLayoutState) -> Result<(), Error> {
    let mut descriptors = 0u64;
    for binding in &mut program.bindings {
        let set = layout
            .sets
            .get(binding.set as usize)
            .ok_or(Error::Malformed)?;
        let (_, first) = set.find(binding.number).ok_or(Error::Malformed)?;
        binding.first = u32::try_from(first).map_err(|_| Error::Malformed)?;
        descriptors = descriptors.saturating_add(u64::from(binding.count));
    }
    program.descriptors = descriptors;
    Ok(())
}

/// The context's compute pipelines.
pub(crate) fn computes(
    context: &mut ContextState,
) -> &mut Pipelines<ComputePipelineKey, Arc<ComputePipelineState>> {
    &mut context.compute_pipelines
}

/// The host memory a pipeline of `program` keeps beside its working memory.
fn held_bytes(program: &Program) -> u64 {
    shared_bytes::<ComputePipelineState>().saturating_add(program.held_bytes())
}

/// The program a compute entry point lowers to, checked against `limits`.
fn lower_compute(
    limits: &Limits,
    module: &Admitted,
    desc: &ComputePipelineDesc<'_>,
    layout: &PipelineLayoutState,
) -> Result<Program, Error> {
    let (program, _) = lower_stage(
        limits,
        module,
        (Stage::Compute, desc.entry_point, desc.specialization),
        layout,
    )?;
    Ok(program)
}

/// The program the entry point of `stage` named `name` lowers to under
/// `specialization` and `layout`, checked against `limits`, and the entry
/// point.
pub(crate) fn lower_stage<'m>(
    limits: &Limits,
    module: &'m Admitted,
    (stage, name, specialization): (Stage, &str, &[SpecValue]),
    layout: &PipelineLayoutState,
) -> Result<(Program, &'m EntryPoint), Error> {
    let index = module
        .entry_points()
        .iter()
        .position(|e| e.stage() == stage && e.name() == name)
        .ok_or(Error::EntryPoint)?;
    let entry = module.entry_points().get(index).ok_or(Error::EntryPoint)?;
    entry
        .agree(&layout.agreement())
        .map_err(Error::Disagreement)?;
    let specialized = module
        .specialize(specialization)
        .map_err(|refusal| match refusal {
            SpecError::OutOfMemory => Error::OutOfMemory,
            SpecError::Budget => Error::TooLarge,
            SpecError::Malformed => Error::Malformed,
            values => Error::Specialization(values),
        })?;
    let size = if stage == Stage::Compute {
        let size = specialized.workgroup_size(index).ok_or(Error::Malformed)?;
        let invocations = size
            .iter()
            .fold(1u64, |n, &s| n.saturating_mul(u64::from(s)));
        let fits = size
            .iter()
            .zip(&limits.max_workgroup_size)
            .all(|(axis, most)| axis <= most);
        if !fits
            || invocations > u64::from(limits.max_workgroup_invocations)
            || invocations > (dispatch::MAX_TILES * LANES) as u64
        {
            return Err(Error::TooLarge);
        }
        size
    } else {
        [1, 1, 1]
    };
    let mut program = lower::lower(module, entry, &specialized, size)?;
    place_bindings(&mut program, layout)?;
    let per_lane = u64::from(program.varying_words) + u64::from(program.private_words);
    let per_tile = u64::from(program.uniform_words)
        .saturating_sub(program.constants.len() as u64)
        .saturating_add(u64::from(program.shared_words));
    let memory = u64::from(limits.max_invocation_memory);
    if per_lane * 4 > memory
        || per_tile * 4 > memory
        || u64::from(program.workgroup_words) * 4 > u64::from(limits.max_workgroup_memory)
    {
        return Err(Error::TooLarge);
    }
    Ok((program, entry))
}

impl Model {
    /// File a pipeline of `stages` of the kind `pick` names in the context,
    /// in the room [`Self::plan_pipeline`] kept for it under `charge`, the
    /// workspaces `grown` made for it installed. Answers what was let go, to
    /// be dropped without the device's lock.
    pub(crate) fn create_pipeline<K: Key, P>(
        &mut self,
        (key, pick): (ContextKey, PickPipelines<K, Arc<PipelineState<P>>>),
        (stages, layout, need): (P, Arc<PipelineLayoutState>, Need),
        (grown, charge): (Grown, Charge),
    ) -> (Result<K, Error>, Released<P>) {
        self.unreserve_pipeline((key, pick));
        let (hold, installed) = self.install_workspaces(grown);
        let Some(hold) = hold else {
            let released = Released {
                _grown: [installed, Grown::default()],
                _stages: Some(stages),
                _charge: Some(charge),
            };
            return (Err(Error::Malformed), released);
        };
        let state = match crate::shared(PipelineState {
            stages,
            layout,
            need,
            _hold: hold,
            _charge: charge,
        }) {
            Ok(state) => state,
            // Its hold went with it, so the pool is fitted without it.
            Err(error) => {
                let released = Released {
                    _grown: [installed, self.prune_workspaces()],
                    ..Released::default()
                };
                return (Err(error), released);
            }
        };
        let kept = Arc::clone(&state);
        let filed = self
            .live_mut(key)
            .and_then(|context| pick(context).table.try_insert(state));
        match filed {
            Ok(filed) => {
                drop(kept);
                let released = Released {
                    _grown: [installed, Grown::default()],
                    ..Released::default()
                };
                (Ok(filed), released)
            }
            Err(error) => {
                // Its last holder: the hold goes here, before the pool is
                // fitted without it.
                let (stages, charge) = Arc::into_inner(kept).map(PipelineState::let_go).unzip();
                let released = Released {
                    _grown: [installed, self.prune_workspaces()],
                    _stages: stages,
                    _charge: charge,
                };
                (Err(error), released)
            }
        }
    }

    /// Release the pipeline of the kind `pick` names, its hold going with
    /// its last holder, which may be queued work. Answers what was let go —
    /// its stages, where this was its last holder, and what the workspaces
    /// let go once fitted without it — to be dropped without the device's
    /// lock.
    pub(crate) fn destroy_pipeline<K: Key, P>(
        &mut self,
        (key, pick): (ContextKey, PickPipelines<K, Arc<PipelineState<P>>>),
        pipeline: K,
    ) -> Released<P> {
        // Into its inner state, so that of this and a job dropping its own
        // hold elsewhere, exactly the last lets it go.
        let held = self
            .existing_mut(key)
            .ok()
            .and_then(|context| pick(context).table.remove(pipeline))
            .and_then(Arc::into_inner)
            .map(PipelineState::let_go);
        let (stages, charge) = held.unzip();
        Released {
            _grown: [self.prune_workspaces(), Grown::default()],
            _stages: stages,
            _charge: charge,
        }
    }

    pub(crate) fn destroy_compute_pipeline(
        &mut self,
        key: ContextKey,
        pipeline: ComputePipelineKey,
    ) -> Released<Program> {
        self.destroy_pipeline((key, computes), pipeline)
    }
}

handle!(
    /// A compute entry point lowered with its layout and specialization,
    /// ready to dispatch.
    ComputePipeline,
    ComputePipelineKey,
    destroy_compute_pipeline,
    releasing
);

/// The most bind sets one dispatch names.
pub const MAX_DISPATCH_SETS: usize = 32;

/// The most push-constant bytes one dispatch carries.
pub const MAX_PUSH_BYTES: usize = 4096;

impl CommandBuffer {
    /// Run `groups` workgroups of `pipeline` along each axis, with `sets`
    /// bound set by set and `push_constants` as its push constants. A
    /// workgroup's invocations run in no order against another's.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an object of another context, and
    /// [`Error::TooMany`] past [`MAX_DISPATCH_SETS`] sets or
    /// [`MAX_PUSH_BYTES`] bytes.
    pub fn dispatch(
        &mut self,
        pipeline: &ComputePipeline,
        sets: &[&BindSet],
        push_constants: &[u8],
        groups: [u32; 3],
    ) -> Result<(), Error> {
        self.record_dispatch(pipeline, sets, push_constants, Count::Direct(groups))
    }

    /// [`Self::dispatch`] with its workgroup counts three little-endian
    /// words read from `buffer` at `offset` when the dispatch runs. Counts
    /// past the device's lose the context.
    ///
    /// # Errors
    ///
    /// As [`Self::dispatch`].
    pub fn dispatch_indirect(
        &mut self,
        pipeline: &ComputePipeline,
        sets: &[&BindSet],
        push_constants: &[u8],
        (buffer, offset): (&Buffer, u64),
    ) -> Result<(), Error> {
        let buffer = self.named(buffer)?;
        self.record_dispatch(
            pipeline,
            sets,
            push_constants,
            Count::Indirect { buffer, offset },
        )
    }

    fn record_dispatch(
        &mut self,
        pipeline: &ComputePipeline,
        sets: &[&BindSet],
        push_constants: &[u8],
        groups: Count,
    ) -> Result<(), Error> {
        owned(
            &pipeline.shared,
            pipeline.context,
            &self.shared,
            self.context,
        )?;
        if sets.len() > MAX_DISPATCH_SETS || push_constants.len() > MAX_PUSH_BYTES {
            return Err(Error::TooMany);
        }
        let keys = set_keys(self, sets)?;
        let push = push_words(push_constants)?;
        self.record(Command::Dispatch(DispatchRecord {
            pipeline: pipeline.key,
            sets: keys,
            push,
            push_bytes: push_constants.len(),
            groups,
        }))
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
pub(crate) mod tests;
