//! What a pipeline's shaders reach and how: bind set layouts, the bind sets
//! made whole against them, pipeline layouts, and shader modules.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Access, Admitted, Binding, BindingKind, ImageShape, Layout, Stages};

use crate::device::{owned, Buffer, Context, Shared};
use crate::format::{Aspects, Format, FormatFeatures};
use crate::image::{ImageUsage, ViewDimension};
use crate::info::{BufferUsage, DeviceInfo};
use tairix_util::fallible;

use crate::memory::Charge;
use crate::model::{
    exhausted, shared_bytes, BindSetKey, BindSetLayoutKey, BufferState, ContextKey, ContextState,
    Model, PipelineLayoutKey, ShaderModuleKey,
};
use crate::objects::{BufferView, ImageView, Sampler};
use crate::resource::{BufferViewState, SamplerState, ViewState};
use crate::sampler::Filter;
use crate::Error;

/// One binding of a bind set layout.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LayoutBinding {
    /// Its number within the set.
    pub binding: u32,
    /// What it holds; an image binding states the shape a module declares.
    pub kind: BindingKind,
    /// How many descriptors it holds.
    pub count: u32,
    /// How the shaders it is bound for may access it.
    pub access: Access,
    /// The stages that see it.
    pub stages: Stages,
}

impl LayoutBinding {
    /// Refuse a binding of no descriptors or stages, past the device's
    /// binding numbers, or of an access or shape its kind does not take.
    fn check(&self, device: &DeviceInfo) -> Result<(), Error> {
        if self.count == 0 || self.stages == Stages::NONE {
            return Err(Error::Empty);
        }
        if self.binding >= device.limits.max_binding_number {
            return Err(Error::OutOfRange);
        }
        let allowed = if writable(self.kind) {
            Access::READ.with(Access::WRITE)
        } else {
            Access::READ
        };
        let shaped = match self.kind {
            BindingKind::Sampler | BindingKind::UniformBuffer | BindingKind::StorageBuffer => true,
            BindingKind::SampledImage(shape) | BindingKind::CombinedImageSampler(shape) => {
                image_shape(shape, true)
            }
            BindingKind::StorageImage(shape) => image_shape(shape, false) && storage_format(shape),
            BindingKind::UniformTexelBuffer(shape) => texel_shape(shape),
            BindingKind::StorageTexelBuffer(shape) => texel_shape(shape) && storage_format(shape),
        };
        if shaped && Stages::ALL.contains(self.stages) && allowed.contains(self.access) {
            Ok(())
        } else {
            Err(Error::Shape)
        }
    }
}

const fn writable(kind: BindingKind) -> bool {
    matches!(
        kind,
        BindingKind::StorageImage(_)
            | BindingKind::StorageTexelBuffer(_)
            | BindingKind::StorageBuffer
    )
}

/// Whether an image binding's shape is one an admitted module declares: no
/// three-dimensional image has layers, and only a sampled rectangle may be
/// multisampled.
fn image_shape(shape: ImageShape, sampled: bool) -> bool {
    let dim = matches!(
        shape.dim,
        Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE
    );
    let layered = !shape.arrayed || shape.dim != Dim::DIM_3D;
    let multisampling = !shape.multisampled || (sampled && shape.dim == Dim::DIM_2D);
    dim && layered && multisampling
}

fn texel_shape(shape: ImageShape) -> bool {
    shape.dim == Dim::BUFFER && !shape.arrayed && !shape.multisampled
}

/// Whether a storage binding's format is unknown, or one a format's channels
/// read as the binding says.
fn storage_format(shape: ImageShape) -> bool {
    shape.format == ImageFormat::UNKNOWN
        || Format::ALL.iter().any(|format| {
            format.storage_format() == Some(shape.format) && format.component() == shape.component
        })
}

/// Whether a view of shape `have` serves a binding of shape `need`: the same
/// dimension, layers, samples and channels, and for storage the binding's
/// format unless it names none.
fn serves(have: ImageShape, need: ImageShape, storage: bool) -> bool {
    let format = !storage || need.format == ImageFormat::UNKNOWN || need.format == have.format;
    have.dim == need.dim
        && have.arrayed == need.arrayed
        && have.multisampled == need.multisampled
        && have.component == need.component
        && format
}

/// A bind set layout: its bindings, by ascending number.
#[derive(Debug)]
pub(crate) struct BindSetLayoutState {
    pub(crate) bindings: Vec<LayoutBinding>,
    /// Where each binding's first descriptor sits among a bind set's.
    pub(crate) firsts: Vec<u64>,
    /// How many descriptors a bind set of it holds.
    descriptors: u64,
    _charge: Charge,
}

impl BindSetLayoutState {
    /// The place of binding `binding` among the layout's, and where its
    /// descriptors start.
    pub(crate) fn find(&self, binding: u32) -> Option<(usize, u64)> {
        let at = self
            .bindings
            .binary_search_by_key(&binding, |b| b.binding)
            .ok()?;
        Some((at, *self.firsts.get(at)?))
    }

    /// Whether a bind set of this layout serves one of `other`'s: the same
    /// bindings, whichever object declared them.
    pub(crate) fn serves(self: &Arc<Self>, other: &Arc<Self>) -> bool {
        Arc::ptr_eq(self, other) || self.bindings == other.bindings
    }
}

/// One descriptor of a bind set: what it binds, held for as long as the set
/// is.
#[derive(Debug)]
pub(crate) enum Bound {
    Sampler(Arc<SamplerState>),
    Image(Arc<ViewState>),
    ImageSampler(Arc<ViewState>, Arc<SamplerState>),
    TexelBuffer(Arc<BufferViewState>),
    /// `size` bytes from `offset` into the buffer.
    Buffer {
        buffer: Arc<BufferState>,
        offset: u64,
        size: u64,
    },
}

impl Bound {
    /// The memory it reaches: none for a sampler.
    pub(crate) fn memory(&self) -> Option<&Arc<crate::memory::Memory>> {
        match self {
            Self::Buffer { buffer, .. } => Some(&buffer.memory),
            Self::TexelBuffer(texels) => Some(&texels.buffer.memory),
            Self::Image(view) | Self::ImageSampler(view, _) => Some(&view.image.memory),
            Self::Sampler(_) => None,
        }
    }
}

/// A bind set: every descriptor of its layout, binding by binding in the
/// layout's order, every memory they reach, once each, in the order memories
/// are locked in, and each descriptor's memory by its place among those.
#[derive(Debug)]
pub(crate) struct BindSetState {
    pub(crate) layout: Arc<BindSetLayoutState>,
    pub(crate) resources: Vec<Bound>,
    pub(crate) memories: Vec<Arc<crate::memory::Memory>>,
    pub(crate) places: Vec<u32>,
    _charge: Charge,
}

/// The push constants a pipeline layout holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PushConstants {
    /// How many bytes: whole words, at most the device's
    /// [`max_push_constant_bytes`](crate::Limits::max_push_constant_bytes).
    pub bytes: u32,
    /// The stages that see them: some where there are bytes, none where
    /// there are not.
    pub stages: Stages,
}

impl PushConstants {
    /// No push constants.
    pub const NONE: Self = Self {
        bytes: 0,
        stages: Stages::NONE,
    };
}

/// A pipeline layout: its bind set layouts, set by set, and its push
/// constants.
#[derive(Debug)]
pub(crate) struct PipelineLayoutState {
    pub(crate) sets: Vec<Arc<BindSetLayoutState>>,
    pub(crate) push_constants: PushConstants,
    /// Every set's bindings, by ascending set and binding.
    bindings: Vec<Binding>,
    _charge: Charge,
}

/// An admitted module, and the charge for the host memory it keeps.
pub(crate) struct ShaderModuleState {
    pub(crate) admitted: Arc<Admitted>,
    _charge: Charge,
}

impl PipelineLayoutState {
    /// The layout an entry point is agreed against.
    pub(crate) fn agreement(&self) -> Layout<'_> {
        Layout {
            bindings: &self.bindings,
            push_constants: self.push_constants.bytes,
            push_constant_stages: self.push_constants.stages,
        }
    }
}

/// What one descriptor of a bind set binds.
#[derive(Copy, Clone, Debug)]
pub enum Resource<'a> {
    /// A sampler, for a sampler binding.
    Sampler(&'a Sampler),
    /// A view, for a sampled or storage image binding.
    Image(&'a ImageView),
    /// A view and the sampler it is sampled with, for a combined binding.
    ImageSampler(&'a ImageView, &'a Sampler),
    /// A texel buffer, for a uniform or storage texel buffer binding.
    TexelBuffer(&'a BufferView),
    /// `size` bytes from `offset` into a buffer, for a uniform or storage
    /// buffer binding.
    Buffer {
        /// The buffer.
        buffer: &'a Buffer,
        /// Where the range starts in it.
        offset: u64,
        /// How many bytes it spans.
        size: u64,
    },
}

/// The resources a bind set binds at one binding of its layout: as many as
/// the binding's count.
#[derive(Copy, Clone, Debug)]
pub struct BindSetEntry<'a> {
    /// The binding.
    pub binding: u32,
    /// Its descriptors, in order.
    pub resources: &'a [Resource<'a>],
}

/// A bind set layout's tables, checked against the device's description and
/// built before its lock is taken: what is left under it is the filing.
pub(crate) struct LayoutTables {
    bindings: Vec<LayoutBinding>,
    firsts: Vec<u64>,
    descriptors: u64,
}

impl LayoutTables {
    pub(crate) fn of(info: &DeviceInfo, bindings: &[LayoutBinding]) -> Result<Self, Error> {
        if bindings.windows(2).any(|w| w[0].binding >= w[1].binding) {
            return Err(Error::Bindings);
        }
        let mut descriptors = 0u64;
        for binding in bindings {
            binding.check(info)?;
            descriptors = descriptors.saturating_add(u64::from(binding.count));
        }
        if descriptors > u64::from(info.limits.max_bind_set_descriptors) {
            return Err(Error::TooMany);
        }
        let mut next = 0u64;
        let firsts = fallible::collected(
            bindings.len(),
            bindings.iter().map(|binding| {
                let first = next;
                next = next.saturating_add(u64::from(binding.count));
                first
            }),
        )
        .ok_or(Error::OutOfMemory)?;
        let bindings = fallible::copied(bindings).ok_or(Error::OutOfMemory)?;
        Ok(Self {
            bindings,
            firsts,
            descriptors,
        })
    }
}

impl Model {
    pub(crate) fn create_bind_set_layout(
        &mut self,
        key: ContextKey,
        tables: LayoutTables,
    ) -> Result<BindSetLayoutKey, Error> {
        let LayoutTables {
            bindings,
            firsts,
            descriptors,
        } = tables;
        let bytes = shared_bytes::<BindSetLayoutState>()
            .saturating_add(fallible::held(&bindings))
            .saturating_add(fallible::held(&firsts));
        self.file_charged(
            key,
            |context| &mut context.bind_set_layouts,
            bytes,
            |charge| {
                crate::shared(BindSetLayoutState {
                    bindings,
                    firsts,
                    descriptors,
                    _charge: charge,
                })
            },
        )
    }

    pub(crate) fn create_bind_set(
        &mut self,
        key: ContextKey,
        layout: BindSetLayoutKey,
        entries: &[BindSetEntry<'_>],
    ) -> Result<BindSetKey, Error> {
        let info = self.info();
        let context = self.live(key)?;
        let layout = Arc::clone(context.bind_set_layouts.get(layout).ok_or(Error::Stale)?);
        let filled = entries.len() == layout.bindings.len()
            && entries
                .iter()
                .zip(&layout.bindings)
                .all(|(entry, binding)| {
                    entry.binding == binding.binding
                        && usize::try_from(binding.count).is_ok_and(|n| n == entry.resources.len())
                });
        if !filled {
            return Err(Error::Bindings);
        }
        // The set's descriptors alone are charged this much, so a set past the
        // limit is refused before any is looked up.
        let least = layout
            .descriptors
            .saturating_mul(size_of::<Bound>() as u64)
            .saturating_add(shared_bytes::<BindSetState>());
        context.budget.admit(least)?;
        let mut resources = Vec::new();
        let total = usize::try_from(layout.descriptors).map_err(exhausted)?;
        resources.try_reserve_exact(total).map_err(exhausted)?;
        for (entry, binding) in entries.iter().zip(&layout.bindings) {
            for resource in entry.resources {
                resources.push(bound(info, context, binding.kind, resource)?);
            }
        }
        let (memories, places) = crate::software::dispatch::memories_of(&resources)?;
        let bytes = shared_bytes::<BindSetState>()
            .saturating_add(fallible::held(&resources))
            .saturating_add(fallible::held(&memories))
            .saturating_add(fallible::held(&places));
        self.file_charged(
            key,
            |context| &mut context.bind_sets,
            bytes,
            |charge| {
                crate::shared(BindSetState {
                    layout,
                    resources,
                    memories,
                    places,
                    _charge: charge,
                })
            },
        )
    }

    pub(crate) fn create_pipeline_layout(
        &mut self,
        key: ContextKey,
        sets: &[&BindSetLayout],
        push_constants: PushConstants,
    ) -> Result<PipelineLayoutKey, Error> {
        let limits = &self.info().limits;
        if sets.len() > limits.max_bind_sets as usize {
            return Err(Error::TooMany);
        }
        if push_constants.bytes > limits.max_push_constant_bytes {
            return Err(Error::TooLarge);
        }
        if !push_constants.bytes.is_multiple_of(4) {
            return Err(Error::Misaligned);
        }
        if (push_constants.bytes == 0) != (push_constants.stages == Stages::NONE) {
            return Err(Error::Empty);
        }
        if !Stages::ALL.contains(push_constants.stages) {
            return Err(Error::Shape);
        }
        let context = self.live(key)?;
        let mut held = Vec::new();
        held.try_reserve_exact(sets.len()).map_err(exhausted)?;
        for set in sets {
            held.push(Arc::clone(
                context.bind_set_layouts.get(set.key).ok_or(Error::Stale)?,
            ));
        }
        let mut bindings = Vec::new();
        let count = held.iter().map(|set| set.bindings.len()).sum();
        bindings.try_reserve_exact(count).map_err(exhausted)?;
        for (set, layout) in (0u32..).zip(&held) {
            bindings.extend(layout.bindings.iter().map(|b| Binding {
                set,
                binding: b.binding,
                kind: b.kind,
                count: b.count,
                access: b.access,
                stages: b.stages,
            }));
        }
        let bytes = shared_bytes::<PipelineLayoutState>()
            .saturating_add(fallible::held(&held))
            .saturating_add(fallible::held(&bindings));
        self.file_charged(
            key,
            |context| &mut context.pipeline_layouts,
            bytes,
            |charge| {
                crate::shared(PipelineLayoutState {
                    sets: held,
                    push_constants,
                    bindings,
                    _charge: charge,
                })
            },
        )
    }

    pub(crate) fn create_shader_module(
        &mut self,
        key: ContextKey,
        module: &Arc<Admitted>,
    ) -> Result<ShaderModuleKey, Error> {
        if !module
            .capabilities()
            .iter()
            .all(|&capability| self.info().runs(capability))
        {
            return Err(Error::Unsupported);
        }
        let bytes = shared_bytes::<ShaderModuleState>()
            .saturating_add(shared_bytes::<Admitted>())
            .saturating_add(module.held_bytes());
        self.file_charged(
            key,
            |context| &mut context.shader_modules,
            bytes,
            |charge| {
                crate::shared(ShaderModuleState {
                    admitted: Arc::clone(module),
                    _charge: charge,
                })
            },
        )
    }
}

/// `resource` held for a binding of `kind`, or why it cannot be.
fn bound(
    info: &DeviceInfo,
    context: &ContextState,
    kind: BindingKind,
    resource: &Resource<'_>,
) -> Result<Bound, Error> {
    let limits = &info.limits;
    match (kind, *resource) {
        (BindingKind::Sampler, Resource::Sampler(sampler)) => {
            Ok(Bound::Sampler(sampler_state(context, sampler)?))
        }
        (BindingKind::SampledImage(shape), Resource::Image(view)) => {
            let view = view_state(context, view)?;
            sampled(info, &view, shape)?;
            Ok(Bound::Image(view))
        }
        (BindingKind::StorageImage(shape), Resource::Image(view)) => {
            let view = view_state(context, view)?;
            storage(info, &view, shape)?;
            Ok(Bound::Image(view))
        }
        (BindingKind::CombinedImageSampler(shape), Resource::ImageSampler(view, sampler)) => {
            let view = view_state(context, view)?;
            let sampler = sampler_state(context, sampler)?;
            sampled(info, &view, shape)?;
            combined(info, &view, &sampler)?;
            Ok(Bound::ImageSampler(view, sampler))
        }
        (BindingKind::UniformTexelBuffer(shape), Resource::TexelBuffer(view)) => {
            let view = texel_state(context, view)?;
            texel(info, &view, shape, false)?;
            Ok(Bound::TexelBuffer(view))
        }
        (BindingKind::StorageTexelBuffer(shape), Resource::TexelBuffer(view)) => {
            let view = texel_state(context, view)?;
            texel(info, &view, shape, true)?;
            Ok(Bound::TexelBuffer(view))
        }
        (
            BindingKind::UniformBuffer | BindingKind::StorageBuffer,
            Resource::Buffer {
                buffer,
                offset,
                size,
            },
        ) => {
            let state = context.buffers.get(buffer.key).ok_or(Error::Stale)?;
            let need = if kind == BindingKind::UniformBuffer {
                Span {
                    usage: BufferUsage::UNIFORM,
                    alignment: limits.uniform_buffer_alignment,
                    longest: limits.max_uniform_buffer_range,
                }
            } else {
                Span {
                    usage: BufferUsage::STORAGE,
                    alignment: limits.storage_buffer_alignment,
                    longest: limits.max_storage_buffer_range,
                }
            };
            need.check(state, offset, size)?;
            Ok(Bound::Buffer {
                buffer: Arc::clone(state),
                offset,
                size,
            })
        }
        _ => Err(Error::Shape),
    }
}

fn sampler_state(context: &ContextState, sampler: &Sampler) -> Result<Arc<SamplerState>, Error> {
    context
        .samplers
        .get(sampler.key)
        .cloned()
        .ok_or(Error::Stale)
}

fn view_state(context: &ContextState, view: &ImageView) -> Result<Arc<ViewState>, Error> {
    context.views.get(view.key).cloned().ok_or(Error::Stale)
}

fn texel_state(context: &ContextState, view: &BufferView) -> Result<Arc<BufferViewState>, Error> {
    context
        .buffer_views
        .get(view.key)
        .cloned()
        .ok_or(Error::Stale)
}

/// Refuse a view a sampled binding of `shape` cannot read: of an image not
/// made for sampling, of more than one plane, of a format the device does not
/// sample, or of another shape.
fn sampled(info: &DeviceInfo, view: &ViewState, shape: ImageShape) -> Result<(), Error> {
    if !view.image.desc.usage.contains(ImageUsage::SAMPLED) {
        return Err(Error::Usage);
    }
    let aspects = view.desc.range.aspects;
    if aspects != Aspects::COLOR && aspects != Aspects::DEPTH && aspects != Aspects::STENCIL {
        return Err(Error::Aspect);
    }
    if !info
        .formats
        .get(view.desc.format)
        .contains(FormatFeatures::SAMPLED)
    {
        return Err(Error::Unsupported);
    }
    if serves(view.desc.shape(&view.image.desc), shape, false) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// Refuse a view a storage binding of `shape` cannot read and write: of an
/// image not made for storage, of more than one level, of a format the device
/// does not store, or of another shape or format.
fn storage(info: &DeviceInfo, view: &ViewState, shape: ImageShape) -> Result<(), Error> {
    if !view.image.desc.usage.contains(ImageUsage::STORAGE) {
        return Err(Error::Usage);
    }
    if view.desc.range.levels != 1 {
        return Err(Error::Shape);
    }
    if !info
        .formats
        .get(view.desc.format)
        .contains(FormatFeatures::STORAGE)
    {
        return Err(Error::Unsupported);
    }
    if serves(view.desc.shape(&view.image.desc), shape, true) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// Refuse a sampler the view cannot be sampled with: filtering a format the
/// device does not filter, comparing what is not depth, or unnormalised
/// coordinates over anything but one level and layer of a row or rectangle.
fn combined(info: &DeviceInfo, view: &ViewState, sampler: &SamplerState) -> Result<(), Error> {
    let desc = &sampler.desc;
    let filtered = [desc.mag_filter, desc.min_filter, desc.mipmap_filter].contains(&Filter::Linear);
    if filtered
        && !info
            .formats
            .get(view.desc.format)
            .contains(FormatFeatures::FILTERED)
    {
        return Err(Error::Unsupported);
    }
    if desc.compare.is_some() && view.desc.range.aspects != Aspects::DEPTH {
        return Err(Error::Aspect);
    }
    let plain = matches!(view.desc.dimension, ViewDimension::D1 | ViewDimension::D2)
        && view.desc.range.levels == 1
        && view.desc.range.layers == 1;
    if desc.unnormalized && !plain {
        return Err(Error::Shape);
    }
    Ok(())
}

/// Refuse a texel buffer a binding of `shape` cannot read, or with `storage`
/// read and write: over a buffer not made for that use, of a format the
/// device does not offer for it, or of another shape or format.
fn texel(
    info: &DeviceInfo,
    view: &BufferViewState,
    shape: ImageShape,
    storage: bool,
) -> Result<(), Error> {
    let (usage, feature) = if storage {
        (
            BufferUsage::STORAGE_TEXEL,
            FormatFeatures::STORAGE_TEXEL_BUFFER,
        )
    } else {
        (
            BufferUsage::UNIFORM_TEXEL,
            FormatFeatures::UNIFORM_TEXEL_BUFFER,
        )
    };
    if !view.buffer.usage.contains(usage) {
        return Err(Error::Usage);
    }
    if !info.formats.get(view.desc.format).contains(feature) {
        return Err(Error::Unsupported);
    }
    if serves(view.desc.shape(), shape, storage) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// What a buffer binding's range needs: a buffer made for `usage`, an
/// offset a multiple of `alignment`, and a span of at most `longest`.
struct Span {
    usage: BufferUsage,
    alignment: u64,
    longest: u64,
}

impl Span {
    fn check(&self, buffer: &BufferState, offset: u64, size: u64) -> Result<(), Error> {
        if !buffer.usage.contains(self.usage) {
            return Err(Error::Usage);
        }
        if size == 0 {
            return Err(Error::Empty);
        }
        if !offset.is_multiple_of(self.alignment) {
            return Err(Error::Misaligned);
        }
        if offset.checked_add(size).is_none_or(|end| end > buffer.size) {
            return Err(Error::OutOfRange);
        }
        if size > self.longest {
            return Err(Error::TooLarge);
        }
        Ok(())
    }
}

impl Context {
    /// A bind set layout of `bindings`, by ascending binding number.
    ///
    /// # Errors
    ///
    /// [`Error::Bindings`] for bindings out of order or repeated,
    /// [`Error::Empty`] for one of no descriptors or stages,
    /// [`Error::OutOfRange`] for one past the device's binding numbers,
    /// [`Error::Shape`] for an access or shape its kind does not take, and
    /// [`Error::TooMany`] for more descriptors than the device's
    /// [`max_bind_set_descriptors`](crate::Limits::max_bind_set_descriptors).
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_bind_set_layout(
        &self,
        bindings: &[LayoutBinding],
    ) -> Result<BindSetLayout, Error> {
        let tables = LayoutTables::of(&self.shared.info, bindings)?;
        let key = self
            .shared
            .model
            .lock()
            .create_bind_set_layout(self.key, tables)?;
        Ok(BindSetLayout {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
        })
    }

    /// A bind set of `layout`, made whole: an entry for each of its bindings
    /// in order, each with as many resources as the binding's count. It holds
    /// what it binds, and through it its memory, for as long as it lives.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an object of another context,
    /// [`Error::Bindings`] for entries that do not fill the layout so, and for
    /// a resource the binding cannot take: [`Error::Shape`] for one of another
    /// kind or shape, [`Error::Usage`] for one not made for the use,
    /// [`Error::Aspect`] for a view of more than one plane or a comparison of
    /// what is not depth, [`Error::Unsupported`] for a format the device does
    /// not offer for it, and [`Error::Empty`], [`Error::Misaligned`],
    /// [`Error::OutOfRange`] or [`Error::TooLarge`] for a buffer range the
    /// binding cannot hold.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_bind_set(
        &self,
        layout: &BindSetLayout,
        entries: &[BindSetEntry<'_>],
    ) -> Result<BindSet, Error> {
        owned(&layout.shared, layout.context, &self.shared, self.key)?;
        for resource in entries.iter().flat_map(|entry| entry.resources) {
            self.owns(resource)?;
        }
        let key = self
            .shared
            .model
            .lock()
            .create_bind_set(self.key, layout.key, entries)?;
        Ok(BindSet {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
        })
    }

    fn owns(&self, resource: &Resource<'_>) -> Result<(), Error> {
        let mine = |shared: &Arc<Shared>, context: ContextKey| {
            owned(shared, context, &self.shared, self.key)
        };
        match *resource {
            Resource::Sampler(sampler) => mine(&sampler.shared, sampler.context),
            Resource::Image(view) => mine(&view.shared, view.context),
            Resource::ImageSampler(view, sampler) => {
                mine(&view.shared, view.context)?;
                mine(&sampler.shared, sampler.context)
            }
            Resource::TexelBuffer(view) => mine(&view.shared, view.context),
            Resource::Buffer { buffer, .. } => mine(&buffer.shared, buffer.context),
        }
    }

    /// A pipeline layout of `sets`, set `i` laid out by `sets[i]`, and
    /// `push_constants`.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a bind set layout of another context,
    /// [`Error::TooMany`] for more sets than the device's
    /// [`max_bind_sets`](crate::Limits::max_bind_sets), and for push constants
    /// past the device's [`Error::TooLarge`], of part of a word
    /// [`Error::Misaligned`], of bytes but no stages or stages but no bytes
    /// [`Error::Empty`], and of a stage there is not [`Error::Shape`].
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_pipeline_layout(
        &self,
        sets: &[&BindSetLayout],
        push_constants: PushConstants,
    ) -> Result<PipelineLayout, Error> {
        for set in sets {
            owned(&set.shared, set.context, &self.shared, self.key)?;
        }
        let key =
            self.shared
                .model
                .lock()
                .create_pipeline_layout(self.key, sets, push_constants)?;
        Ok(PipelineLayout {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
        })
    }

    /// A shader module of an admitted module.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a module needing a capability the device
    /// does not run.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_shader_module(&self, module: Admitted) -> Result<ShaderModule, Error> {
        let module = crate::shared(module)?;
        let key = self
            .shared
            .model
            .lock()
            .create_shader_module(self.key, &module)?;
        Ok(ShaderModule {
            shared: Arc::clone(&self.shared),
            context: self.key,
            key,
            module,
        })
    }
}

handle!(
    /// The bindings a bind set holds.
    BindSetLayout,
    BindSetLayoutKey,
    bind_set_layouts
);

handle!(
    /// The resources one set of a pipeline's bindings reaches, made whole and
    /// held for as long as the set lives.
    BindSet,
    BindSetKey,
    bind_sets
);

handle!(
    /// The bind set layouts and push constants a pipeline's shaders reach.
    PipelineLayout,
    PipelineLayoutKey,
    pipeline_layouts
);

/// An admitted shader module, made a device's.
pub struct ShaderModule {
    pub(crate) shared: Arc<Shared>,
    pub(crate) context: ContextKey,
    pub(crate) key: ShaderModuleKey,
    module: Arc<Admitted>,
}

impl ShaderModule {
    /// The module.
    #[must_use]
    pub fn module(&self) -> &Admitted {
        &self.module
    }
}

impl Drop for ShaderModule {
    fn drop(&mut self) {
        self.shared.release(
            self.context,
            |context| &mut context.shader_modules,
            self.key,
        );
    }
}

impl fmt::Debug for ShaderModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShaderModule").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
