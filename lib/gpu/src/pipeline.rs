//! Compute pipelines: an entry point of a shader module, its pipeline layout
//! and its specialization, checked and made once, with what its dispatches
//! need of the device's pool reserved then.

use alloc::sync::Arc;

use tairix_collections::Key;
use tairix_spirv::{Admitted, EntryPoint, SpecError, SpecValue, Specialized, Stage};

use crate::backend::{Backend, Grown, Hold, Need, Pool};
use crate::binding::{BindSet, PipelineLayout, PipelineLayoutState, ShaderModule};
use crate::command::{push_words, set_keys, Command, Count, DispatchRecord};
use crate::device::{Buffer, CommandBuffer, Context, Shared};
use crate::host::owned;
use crate::info::Limits;
use crate::memory::Charge;
use crate::model::{
    shared_bytes, ComputePipelineKey, ContextKey, ContextState, Model, PickPipelines,
    PipelineLayoutKey, Pipelines, ShaderModuleKey,
};
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

/// A pipeline as its commands run it: its stages `P` as its backend made
/// them, its layout, and what it needs of the device's pool, held while it
/// lives.
pub(crate) struct PipelineState<B: Backend, P> {
    pub(crate) stages: P,
    pub(crate) layout: Arc<PipelineLayoutState>,
    pub(crate) need: Need<B>,
    _hold: Hold<B>,
    _charge: Charge,
}

/// A compute pipeline: its one program.
pub(crate) type ComputePipelineState<B> = PipelineState<B, <B as Backend>::Compute>;

/// A module a pipeline's stage is made from.
pub(crate) type Module = Arc<crate::binding::ShaderModuleState>;

/// Which kind of pipeline of stages `P` a request makes.
pub(crate) type PickPipeline<B, K, P> = PickPipelines<B, K, Arc<PipelineState<B, P>>>;

impl<B: Backend, P> core::fmt::Debug for PipelineState<B, P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PipelineState").finish_non_exhaustive()
    }
}

impl<B: Backend, P> PipelineState<B, P> {
    /// Its hold let go, so the pool can be fitted without it; its stages and
    /// the charge for them answered, to be let go without the device's lock.
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
/// device's lock: what the pool let go, and its stages with the charge for
/// them.
pub(crate) struct Released<B: Backend, P> {
    _grown: [Grown<B>; 2],
    _stages: Option<P>,
    // After the stages, so it stands for them until they are gone.
    _charge: Option<Charge>,
}

impl<B: Backend, P> Default for Released<B, P> {
    fn default() -> Self {
        Self {
            _grown: Default::default(),
            _stages: None,
            _charge: None,
        }
    }
}

/// An entry point of a module, agreed with its pipeline's layout and
/// specialized: what a backend makes a pipeline's stage from.
pub(crate) struct StagePlan<'m> {
    pub(crate) module: &'m Admitted,
    pub(crate) entry: &'m EntryPoint,
    pub(crate) specialized: Specialized,
    /// Its workgroup's size along each axis: one invocation for a stage that
    /// has no workgroup.
    pub(crate) workgroup: [u32; 3],
    pub(crate) layout: &'m PipelineLayoutState,
}

/// The entry point of `stage` named `name` in `module`, agreed with `layout`
/// and specialized under `specialization`, its workgroup within `limits`.
pub(crate) fn plan_stage<'m>(
    limits: &Limits,
    module: &'m Admitted,
    (stage, name, specialization): (Stage, &str, &[SpecValue]),
    layout: &'m PipelineLayoutState,
) -> Result<StagePlan<'m>, Error> {
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
    let workgroup = if stage == Stage::Compute {
        let size = specialized.workgroup_size(index).ok_or(Error::Malformed)?;
        let invocations = size
            .iter()
            .fold(1u64, |n, &s| n.saturating_mul(u64::from(s)));
        let fits = size
            .iter()
            .zip(&limits.max_workgroup_size)
            .all(|(axis, most)| axis <= most);
        if !fits || invocations > u64::from(limits.max_workgroup_invocations) {
            return Err(Error::TooLarge);
        }
        size
    } else {
        [1, 1, 1]
    };
    Ok(StagePlan {
        module,
        entry,
        specialized,
        workgroup,
        layout,
    })
}

impl Context {
    /// A compute pipeline: its entry point agreed with its layout,
    /// specialized, and made.
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
        owned(&desc.module.host, desc.module.context, &self.host, self.key)?;
        owned(&desc.layout.host, desc.layout.context, &self.host, self.key)?;
        let key = self.host.create_compute_pipeline(
            self.key,
            (desc.module.key, desc.entry_point, desc.specialization),
            desc.layout.key,
        )?;
        Ok(ComputePipeline {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
        })
    }
}

impl<B: Backend> Shared<B> {
    /// The context's module `first` names, the one `second` names where it
    /// names one, and the pipeline layout `layout` names, looked up under one
    /// hold of the device's lock.
    pub(crate) fn modules_and_layout(
        &self,
        context: ContextKey,
        (first, second): (ShaderModuleKey, Option<ShaderModuleKey>),
        layout: PipelineLayoutKey,
    ) -> Result<(Module, Option<Module>, Arc<PipelineLayoutState>), Error> {
        let model = self.model.lock();
        let live = model.live(context)?;
        let module = |key| live.shader_modules.get(key).cloned().ok_or(Error::Stale);
        let first = module(first)?;
        let second = second.map(module).transpose()?;
        let layout = live
            .pipeline_layouts
            .get(layout)
            .cloned()
            .ok_or(Error::Stale)?;
        Ok((first, second, layout))
    }

    /// A compute pipeline of the context: the compute entry point `entry` of
    /// `module` under `specialization`, laid out as `layout`, checked and
    /// made.
    pub(crate) fn create_compute_pipeline(
        &self,
        context: ContextKey,
        (module, entry, specialization): (ShaderModuleKey, &str, &[SpecValue]),
        layout: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey, Error> {
        let (module, _, layout) = self.modules_and_layout(context, (module, None), layout)?;
        let plan = plan_stage(
            &self.info.limits,
            &module.admitted,
            (Stage::Compute, entry, specialization),
            &layout,
        )?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let prepared = self.backend.compute(&self.info, &plan)?;
        drop(plan);
        let held = shared_bytes::<ComputePipelineState<B>>().saturating_add(prepared.held);
        self.file_pipeline(
            context,
            computes,
            (prepared.form, layout),
            (prepared.need, held),
        )
    }

    /// File `stages` under `layout` as a pipeline of the context of the kind
    /// `pick` names, needing `need` of the pool and keeping `held` bytes
    /// beside: its room and charges planned, what the pool lacks of it made
    /// between two holds of the device's lock, as zeroing it takes as long as
    /// it is large, and filed.
    pub(crate) fn file_pipeline<K: Key, P>(
        &self,
        context: ContextKey,
        pick: PickPipeline<B, K, P>,
        (stages, layout): (P, Arc<PipelineLayoutState>),
        (need, held): (Need<B>, u64),
    ) -> Result<K, Error> {
        let (planned, released) = self.model.lock().plan_pipeline((context, pick), need, held);
        drop(released);
        let (plan, charge) = planned?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let made = match <B::Pool as Pool>::make(plan) {
            Ok(made) => made,
            Err(error) => {
                self.model.lock().unreserve_pipeline((context, pick));
                return Err(error);
            }
        };
        let (key, released) = self.model.lock().create_pipeline(
            (context, pick),
            (stages, layout, need),
            (made, charge),
        );
        drop(released);
        key
    }
}

/// The context's compute pipelines.
pub(crate) fn computes<B: Backend>(
    context: &mut ContextState<B>,
) -> &mut Pipelines<ComputePipelineKey, Arc<ComputePipelineState<B>>> {
    &mut context.compute_pipelines
}

impl<B: Backend> Model<B> {
    /// File a pipeline of `stages` of the kind `pick` names in the context,
    /// in the room [`Self::plan_pipeline`] kept for it under `charge` and
    /// `hold`, what `grown` made for it installed in the pool. Answers what
    /// was let go, to be dropped without the device's lock.
    pub(crate) fn create_pipeline<K: Key, P>(
        &mut self,
        (key, pick): (ContextKey, PickPipeline<B, K, P>),
        (stages, layout, need): (P, Arc<PipelineLayoutState>, Need<B>),
        ((hold, grown), charge): ((Hold<B>, Grown<B>), Charge),
    ) -> (Result<K, Error>, Released<B, P>) {
        self.unreserve_pipeline((key, pick));
        let installed = self.install_pool(grown);
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
                    _grown: [installed, self.prune_pool()],
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
                    _grown: [installed, Default::default()],
                    ..Released::default()
                };
                (Ok(filed), released)
            }
            Err(error) => {
                // Its last holder: the hold goes here, before the pool is
                // fitted without it.
                let (stages, charge) = Arc::into_inner(kept).map(PipelineState::let_go).unzip();
                let released = Released {
                    _grown: [installed, self.prune_pool()],
                    _stages: stages,
                    _charge: charge,
                };
                (Err(error), released)
            }
        }
    }

    /// Release the pipeline of the kind `pick` names, its hold going with
    /// its last holder, which may be queued work. Answers what was let go —
    /// its stages, where this was its last holder, and what the pool let go
    /// once fitted without it — to be dropped without the device's lock.
    pub(crate) fn destroy_pipeline<K: Key, P>(
        &mut self,
        (key, pick): (ContextKey, PickPipeline<B, K, P>),
        pipeline: K,
    ) -> Released<B, P> {
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
            _grown: [self.prune_pool(), Default::default()],
            _stages: stages,
            _charge: charge,
        }
    }

    pub(crate) fn destroy_compute_pipeline(
        &mut self,
        key: ContextKey,
        pipeline: ComputePipelineKey,
    ) -> Released<B, B::Compute> {
        self.destroy_pipeline((key, computes), pipeline)
    }
}

handle!(
    /// A compute entry point made with its layout and specialization, ready
    /// to dispatch.
    ComputePipeline,
    ComputePipelineKey,
    ComputePipeline
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
        owned(&pipeline.host, pipeline.context, &self.host, self.context)?;
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
