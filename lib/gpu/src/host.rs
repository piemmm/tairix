//! The one way a handle reaches its device: a [`Host`], the model over a
//! backend in this process or a channel to a device a driver serves.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::Duration64;
use tairix_spirv::{Admitted, SpecValue};

use crate::backend::Backend;
use crate::binding::{LayoutBinding, LayoutTables, PushConstants, SetRequest};
use crate::command::Recorded;
use crate::device::{AllocationDesc, BufferDesc, Shared};
use crate::image::{BufferViewDesc, ImageDesc, ImageViewDesc};
use crate::info::DeviceInfo;
use crate::memory::{Charge, MemoryUse};
use crate::model::{
    AllocationKey, BindSetKey, BindSetLayoutKey, BufferKey, BufferViewKey, ComputePipelineKey,
    ContextKey, ExportKey, ImageKey, Imported, PipelineLayoutKey, Point, RenderPipelineKey,
    Request, SamplerKey, ShaderModuleKey, TimelineKey, ViewKey, Watch,
};
use crate::render::RenderRequest;
use crate::resource::MemoryRequirements;
use crate::sampler::SamplerDesc;
use crate::{Error, SubmitError};

/// An object a context files, as its handle names it to let it go.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Object {
    Allocation(AllocationKey),
    Buffer(BufferKey),
    Timeline(TimelineKey),
    Image(ImageKey),
    View(ViewKey),
    BufferView(BufferViewKey),
    Sampler(SamplerKey),
    BindSetLayout(BindSetLayoutKey),
    BindSet(BindSetKey),
    PipelineLayout(PipelineLayoutKey),
    ShaderModule(ShaderModuleKey),
    ComputePipeline(ComputePipelineKey),
    RenderPipeline(RenderPipelineKey),
}

/// An export as its device names it: an entry of the model in this process,
/// or the token a served device answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExportId {
    Model(ExportKey),
    Token(crate::wire::ops::ExportToken),
}

/// What a handle asks of its device, every object named by its context and
/// key. Each operation the model takes in phases is one call, so a device in
/// another process answers it in one exchange.
pub(crate) trait Host: Send + Sync {
    /// The device behind the host, for tests that look into it.
    #[cfg(test)]
    fn as_any(&self) -> &dyn core::any::Any;

    fn info(&self) -> &DeviceInfo;
    fn create_context(&self, memory_limit: Option<u64>) -> Result<ContextKey, Error>;
    fn destroy_context(&self, context: ContextKey);
    fn status(&self, context: ContextKey) -> Result<(), Error>;
    fn memory_used(&self, context: ContextKey) -> MemoryUse;
    fn release(&self, context: ContextKey, object: Object);

    fn allocate(&self, context: ContextKey, desc: &AllocationDesc) -> Result<AllocationKey, Error>;
    fn write(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error>;
    fn read(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        out: &mut [u8],
    ) -> Result<(), Error>;
    fn export(&self, context: ContextKey, allocation: AllocationKey) -> Result<ExportId, Error>;
    fn import(&self, context: ContextKey, export: ExportId) -> Result<Imported, Error>;
    fn drop_export(&self, export: ExportId);
    fn create_buffer(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        desc: &BufferDesc,
    ) -> Result<BufferKey, Error>;

    fn create_timeline(&self, context: ContextKey, initial: u64) -> Result<TimelineKey, Error>;
    fn timeline_value(&self, context: ContextKey, timeline: TimelineKey) -> Result<u64, Error>;
    fn signal(&self, context: ContextKey, timeline: TimelineKey, value: u64) -> Result<(), Error>;
    fn wait(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
        timeout: Duration64,
    ) -> Result<(), Error>;
    fn reached(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
    ) -> Result<bool, Error>;
    fn submit(
        &self,
        context: ContextKey,
        request: &Request<'_>,
    ) -> Result<(TimelineKey, u64), SubmitError>;
    fn record_update(&self, context: ContextKey, bytes: Vec<u8>) -> Result<Arc<Recorded>, Error>;

    fn create_bind_set_layout(
        &self,
        context: ContextKey,
        bindings: &[LayoutBinding],
    ) -> Result<BindSetLayoutKey, Error>;
    fn create_bind_set(
        &self,
        context: ContextKey,
        request: &SetRequest<'_>,
    ) -> Result<BindSetKey, Error>;
    fn create_pipeline_layout(
        &self,
        context: ContextKey,
        sets: &[BindSetLayoutKey],
        push_constants: PushConstants,
    ) -> Result<PipelineLayoutKey, Error>;
    fn create_shader_module(
        &self,
        context: ContextKey,
        module: &Arc<Admitted>,
    ) -> Result<ShaderModuleKey, Error>;

    fn image_requirements(&self, desc: &ImageDesc) -> Result<MemoryRequirements, Error>;
    fn create_image(
        &self,
        context: ContextKey,
        placed: (AllocationKey, u64),
        desc: &ImageDesc,
    ) -> Result<ImageKey, Error>;
    fn create_view(
        &self,
        context: ContextKey,
        image: ImageKey,
        desc: &ImageViewDesc,
    ) -> Result<ViewKey, Error>;
    fn create_buffer_view(
        &self,
        context: ContextKey,
        buffer: BufferKey,
        desc: &BufferViewDesc,
    ) -> Result<BufferViewKey, Error>;
    fn create_sampler(&self, context: ContextKey, desc: &SamplerDesc) -> Result<SamplerKey, Error>;

    fn create_compute_pipeline(
        &self,
        context: ContextKey,
        stage: (ShaderModuleKey, &str, &[SpecValue]),
        layout: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey, Error>;
    fn create_render_pipeline(
        &self,
        context: ContextKey,
        request: &RenderRequest<'_>,
    ) -> Result<RenderPipelineKey, Error>;
}

/// A device in this process, as the runtime serving it to other processes
/// reaches it.
pub(crate) trait Served: Host {
    /// The device's clock, which every deadline is read from.
    fn now(&self) -> u64;
    /// When the soonest running job's deadline falls, where its device needs
    /// that watched.
    fn earliest_deadline(&self) -> Option<u64>;
    /// Answer every running job past its deadline at `now` as overran.
    fn expire(&self, now: u64);
    /// Tell `watch` of every timeline that advances and context that ends.
    fn watch(&self, watch: Arc<dyn Watch>);
    /// Charge `bytes` of host memory to the context, for as long as the
    /// charge is held.
    fn charge(&self, context: ContextKey, bytes: u64) -> Result<Charge, Error>;
    /// The description of the context's image `image`, where it has one.
    fn image_desc(&self, context: ContextKey, image: ImageKey) -> Option<ImageDesc>;
}

/// Whether a handle of `host`'s device and `context` belongs where it is
/// used.
pub(crate) fn owned(
    host: &Arc<dyn Host>,
    context: ContextKey,
    by: &Arc<dyn Host>,
    key: ContextKey,
) -> Result<(), Error> {
    if Arc::ptr_eq(host, by) && context == key {
        Ok(())
    } else {
        Err(Error::Foreign)
    }
}

impl<B: Backend> Host for Shared<B> {
    #[cfg(test)]
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn create_context(&self, memory_limit: Option<u64>) -> Result<ContextKey, Error> {
        // Made before the model files it, as a device may be waited on for
        // it; a refusal lets it go.
        let device = self.backend.create_context()?;
        self.model.lock().create_context(memory_limit, device)
    }

    fn destroy_context(&self, context: ContextKey) {
        let released = self.model.lock().destroy_context(context);
        // What it held goes without the device's lock, its memories among it.
        drop(released);
        let pruned = self.model.lock().prune_pool();
        drop(pruned);
    }

    fn status(&self, context: ContextKey) -> Result<(), Error> {
        self.model.lock().status(context)
    }

    fn memory_used(&self, context: ContextKey) -> MemoryUse {
        self.model.lock().memory_taken(context).unwrap_or_default()
    }

    fn release(&self, context: ContextKey, object: Object) {
        // Each is taken out under the lock and let go after it: any may hold
        // the last of a memory.
        match object {
            Object::Allocation(key) => self.take(context, |c| &mut c.allocations, key),
            Object::Buffer(key) => self.take(context, |c| &mut c.buffers, key),
            Object::Timeline(key) => {
                let released = self.model.lock().destroy_timeline(context, key);
                drop(released);
            }
            Object::Image(key) => self.take(context, |c| &mut c.images, key),
            Object::View(key) => self.take(context, |c| &mut c.views, key),
            Object::BufferView(key) => self.take(context, |c| &mut c.buffer_views, key),
            Object::Sampler(key) => self.take(context, |c| &mut c.samplers, key),
            Object::BindSetLayout(key) => self.take(context, |c| &mut c.bind_set_layouts, key),
            Object::BindSet(key) => self.take(context, |c| &mut c.bind_sets, key),
            Object::PipelineLayout(key) => self.take(context, |c| &mut c.pipeline_layouts, key),
            Object::ShaderModule(key) => self.take(context, |c| &mut c.shader_modules, key),
            Object::ComputePipeline(key) => {
                let released = self.model.lock().destroy_compute_pipeline(context, key);
                drop(released);
            }
            Object::RenderPipeline(key) => {
                let released = self.model.lock().destroy_render_pipeline(context, key);
                drop(released);
            }
        }
    }

    fn allocate(&self, context: ContextKey, desc: &AllocationDesc) -> Result<AllocationKey, Error> {
        let (device, charges) = {
            let mut model = self.model.lock();
            let device = model.device(context)?;
            (
                device,
                model.charge_allocation(context, desc.size, desc.memory_type)?,
            )
        };
        // Made between two holds of the device's lock, as zeroing it takes as
        // long as it is large; its charges stand for it meanwhile.
        #[cfg(test)]
        crate::fair::held::note_making();
        let made = self
            .backend
            .allocate(&device, (desc.memory_type, desc.size), charges.into())
            .and_then(crate::shared);
        let memory = match made {
            Ok(memory) => memory,
            Err(error) => {
                self.model.lock().unreserve_allocation(context);
                return Err(error);
            }
        };
        let filed = self
            .model
            .lock()
            .file_allocation(context, memory, desc.memory_type, desc.size);
        filed.map_err(|(error, refused)| {
            drop(refused);
            error
        })
    }

    fn write(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        let memory = self.host_memory(context, allocation, offset, data.len())?;
        self.backend.write(&memory, offset, data)
    }

    fn read(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        out: &mut [u8],
    ) -> Result<(), Error> {
        let memory = self.host_memory(context, allocation, offset, out.len())?;
        self.backend.read(&memory, offset, out)
    }

    fn export(&self, context: ContextKey, allocation: AllocationKey) -> Result<ExportId, Error> {
        self.model
            .lock()
            .export(context, allocation)
            .map(ExportId::Model)
    }

    fn import(&self, context: ContextKey, export: ExportId) -> Result<Imported, Error> {
        let ExportId::Model(export) = export else {
            return Err(Error::Foreign);
        };
        let mut model = self.model.lock();
        let (filed, refused) = model.import(context, export);
        drop(model);
        drop(refused);
        filed
    }

    fn drop_export(&self, export: ExportId) {
        if let ExportId::Model(export) = export {
            let released = self.model.lock().drop_export(export);
            drop(released);
        }
    }

    fn create_buffer(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        desc: &BufferDesc,
    ) -> Result<BufferKey, Error> {
        self.model
            .lock()
            .create_buffer(context, allocation, desc.offset, desc.size, desc.usage)
    }

    fn create_timeline(&self, context: ContextKey, initial: u64) -> Result<TimelineKey, Error> {
        self.model.lock().create_timeline(context, initial)
    }

    fn timeline_value(&self, context: ContextKey, timeline: TimelineKey) -> Result<u64, Error> {
        self.model.lock().timeline_value(context, timeline)
    }

    fn signal(&self, context: ContextKey, timeline: TimelineKey, value: u64) -> Result<(), Error> {
        let mut model = self.model.lock();
        let waited = model.signal(context, timeline, value)?;
        // Before the work it readied runs, which may be on this thread.
        if let Some(wake) = waited {
            wake.notify_all();
        }
        self.drive(model);
        Ok(())
    }

    fn wait(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
        timeout: Duration64,
    ) -> Result<(), Error> {
        let limit = self
            .clock
            .now_ns()
            .saturating_add(timeout.saturating_total_nanos());
        let mut model = self.model.lock();
        loop {
            let wake = match model.point(context, timeline, value)? {
                Point::Reached => return Ok(()),
                Point::Pending(wake) => wake,
            };
            let now = self.clock.now_ns();
            if now >= limit {
                return Err(Error::TimedOut);
            }
            model = self
                .model
                .wait_timeout(model, &wake, Duration64::from_nanos(limit - now));
        }
    }

    fn reached(
        &self,
        context: ContextKey,
        timeline: TimelineKey,
        value: u64,
    ) -> Result<bool, Error> {
        let point = self.model.lock().point(context, timeline, value)?;
        Ok(matches!(point, Point::Reached))
    }

    fn submit(
        &self,
        context: ContextKey,
        request: &Request<'_>,
    ) -> Result<(TimelineKey, u64), SubmitError> {
        // Before the lock, so what a refusal leaves in it goes after it.
        let mut resolved = Vec::new();
        let mut model = self.model.lock();
        let fence = model.submit(context, request, &mut resolved)?;
        self.drive(model);
        Ok(fence)
    }

    fn record_update(&self, context: ContextKey, bytes: Vec<u8>) -> Result<Arc<Recorded>, Error> {
        let charge = self
            .model
            .lock()
            .host_charge(context, Recorded::held(&bytes))?;
        crate::shared(Recorded::new(bytes, charge))
    }

    fn create_bind_set_layout(
        &self,
        context: ContextKey,
        bindings: &[LayoutBinding],
    ) -> Result<BindSetLayoutKey, Error> {
        let tables = LayoutTables::of(&self.info, bindings)?;
        self.model.lock().create_bind_set_layout(context, tables)
    }

    fn create_bind_set(
        &self,
        context: ContextKey,
        request: &SetRequest<'_>,
    ) -> Result<BindSetKey, Error> {
        Self::create_bind_set(self, context, request)
    }

    fn create_pipeline_layout(
        &self,
        context: ContextKey,
        sets: &[BindSetLayoutKey],
        push_constants: PushConstants,
    ) -> Result<PipelineLayoutKey, Error> {
        self.model
            .lock()
            .create_pipeline_layout(context, sets, push_constants)
    }

    fn create_shader_module(
        &self,
        context: ContextKey,
        module: &Arc<Admitted>,
    ) -> Result<ShaderModuleKey, Error> {
        self.model.lock().create_shader_module(context, module)
    }

    fn image_requirements(&self, desc: &ImageDesc) -> Result<MemoryRequirements, Error> {
        self.image_form(desc).map(|(_, requirements)| requirements)
    }

    fn create_image(
        &self,
        context: ContextKey,
        placed: (AllocationKey, u64),
        desc: &ImageDesc,
    ) -> Result<ImageKey, Error> {
        Self::create_image(self, context, placed, desc)
    }

    fn create_view(
        &self,
        context: ContextKey,
        image: ImageKey,
        desc: &ImageViewDesc,
    ) -> Result<ViewKey, Error> {
        self.model.lock().create_view(context, image, desc)
    }

    fn create_buffer_view(
        &self,
        context: ContextKey,
        buffer: BufferKey,
        desc: &BufferViewDesc,
    ) -> Result<BufferViewKey, Error> {
        self.model.lock().create_buffer_view(context, buffer, desc)
    }

    fn create_sampler(&self, context: ContextKey, desc: &SamplerDesc) -> Result<SamplerKey, Error> {
        self.model.lock().create_sampler(context, desc)
    }

    fn create_compute_pipeline(
        &self,
        context: ContextKey,
        stage: (ShaderModuleKey, &str, &[SpecValue]),
        layout: PipelineLayoutKey,
    ) -> Result<ComputePipelineKey, Error> {
        Self::create_compute_pipeline(self, context, stage, layout)
    }

    fn create_render_pipeline(
        &self,
        context: ContextKey,
        request: &RenderRequest<'_>,
    ) -> Result<RenderPipelineKey, Error> {
        Self::create_render_pipeline(self, context, request)
    }
}

impl<B: Backend> Served for Shared<B> {
    fn now(&self) -> u64 {
        self.clock.now_ns()
    }

    fn earliest_deadline(&self) -> Option<u64> {
        self.model.lock().earliest_deadline()
    }

    fn watch(&self, watch: Arc<dyn Watch>) {
        self.model.lock().watch(watch);
    }

    fn charge(&self, context: ContextKey, bytes: u64) -> Result<Charge, Error> {
        self.model.lock().host_charge(context, bytes)
    }

    fn image_desc(&self, context: ContextKey, image: ImageKey) -> Option<ImageDesc> {
        let model = self.model.lock();
        model
            .live(context)
            .ok()?
            .images
            .get(image)
            .map(|state| state.desc)
    }

    fn expire(&self, now: u64) {
        loop {
            let mut model = self.model.lock();
            let Some(device) = model.expire_one(now) else {
                // The engines it emptied take what is ready next.
                self.drive(model);
                return;
            };
            drop(model);
            self.backend.lost(&device);
        }
    }
}

impl<B: Backend> Shared<B> {
    /// The memory the CPU reaches the allocation through, once `len` bytes at
    /// `offset` are proved within it.
    fn host_memory(
        &self,
        context: ContextKey,
        allocation: AllocationKey,
        offset: u64,
        len: usize,
    ) -> Result<Arc<B::Memory>, Error> {
        let (memory, size) = self.model.lock().host_memory(context, allocation)?;
        offset
            .checked_add(len as u64)
            .filter(|&end| end <= size)
            .ok_or(Error::OutOfRange)?;
        Ok(memory)
    }
}
