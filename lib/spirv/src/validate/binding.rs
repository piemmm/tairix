//! Resources: what each descriptor an entry point statically uses is — its
//! set and binding, kind, count and how it is accessed — and whether a
//! pipeline's layout provides it.
//!
//! One binding names one resource of one kind: two resource variables of
//! an entry point at one set and binding are refused, so agreement with a
//! layout is decided once per binding.

use alloc::vec::Vec;

use super::entry::Reach;
use super::types::{Image, Scalar, Ty};
use super::{Limit, Rule, Validator, MAX_BINDINGS, NONE};
use crate::enumerant::{Decoration, Dim, ImageFormat, StorageClass};

tairix_util::flags! {
    /// How a resource is accessed.
    pub struct Access(u8) {
        /// Read: loaded from, sampled, fetched, gathered or read.
        const READ = 1;
        /// Written: stored to, written, or changed by an atomic.
        const WRITE = 1 << 1;
    }
}

/// What a texel's or an interface value's components hold.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Component {
    /// Floating-point values.
    Float,
    /// Signed integers.
    Sint,
    /// Unsigned integers.
    Uint,
}

/// An image's shape, which the image bound to a binding must have.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ImageShape {
    /// Its dimensionality.
    pub dim: Dim,
    /// Whether it is an array of layers.
    pub arrayed: bool,
    /// Whether it is multisampled.
    pub multisampled: bool,
    /// What its texels read as.
    pub component: Component,
    /// A storage image's format; [`ImageFormat::UNKNOWN`] where any will do.
    pub format: ImageFormat,
}

/// What a binding holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BindingKind {
    /// A sampler.
    Sampler,
    /// An image for sampling.
    SampledImage(ImageShape),
    /// An image read and written by texel.
    StorageImage(ImageShape),
    /// An image and the sampler it is sampled with.
    CombinedImageSampler(ImageShape),
    /// A buffer of texels read through a format.
    UniformTexelBuffer(ImageShape),
    /// A buffer of texels read and written through a format.
    StorageTexelBuffer(ImageShape),
    /// A uniform buffer.
    UniformBuffer,
    /// A storage buffer.
    StorageBuffer,
}

/// One binding: what an entry point needs at a set and binding, or what a
/// layout provides there.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    /// The descriptor set.
    pub set: u32,
    /// The binding within the set.
    pub binding: u32,
    /// What it holds.
    pub kind: BindingKind,
    /// How many descriptors: an entry point's array length, or a layout's
    /// count.
    pub count: u32,
    /// An entry point's accesses, or what a layout allows.
    pub access: Access,
}

/// What a pipeline's layout provides: its bindings, by ascending set and
/// binding, and the bytes of push constants it holds.
#[derive(Copy, Clone, Debug)]
pub struct Layout<'a> {
    /// The bindings.
    pub bindings: &'a [Binding],
    /// The push-constant bytes.
    pub push_constants: u32,
}

/// Why an entry point and a layout disagree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Disagreement {
    /// The layout's bindings are not in ascending order of set and binding,
    /// one apiece.
    Unordered,
    /// The layout provides nothing at `(set, binding)`.
    Missing(u32, u32),
    /// The layout provides something else there.
    Kind(u32, u32),
    /// Fewer descriptors than the entry point's array.
    Count(u32, u32),
    /// An access the layout does not allow.
    Access(u32, u32),
    /// More push-constant bytes than the layout holds.
    PushConstants,
}

impl super::EntryPoint {
    /// Whether `layout` provides everything this entry point uses.
    ///
    /// # Errors
    ///
    /// The first binding, in ascending order, the layout does not provide as
    /// the entry point needs it.
    pub fn agree(&self, layout: &Layout<'_>) -> Result<(), Disagreement> {
        let key = |b: &Binding| (b.set, b.binding);
        if layout.bindings.windows(2).any(|w| key(&w[0]) >= key(&w[1])) {
            return Err(Disagreement::Unordered);
        }
        for need in &self.bindings {
            let at = (need.set, need.binding);
            let have = layout
                .bindings
                .binary_search_by_key(&at, key)
                .ok()
                .and_then(|i| layout.bindings.get(i))
                .ok_or(Disagreement::Missing(at.0, at.1))?;
            if !kind_agrees(need.kind, have.kind) {
                return Err(Disagreement::Kind(at.0, at.1));
            }
            if have.count < need.count {
                return Err(Disagreement::Count(at.0, at.1));
            }
            if !have.access.contains(need.access) {
                return Err(Disagreement::Access(at.0, at.1));
            }
        }
        if self
            .push_constants
            .is_some_and(|bytes| bytes > layout.push_constants)
        {
            return Err(Disagreement::PushConstants);
        }
        Ok(())
    }
}

/// Whether a binding of `have` serves a use of `need`: the same kind and
/// shape, a storage image of unknown format taking any format.
fn kind_agrees(need: BindingKind, have: BindingKind) -> bool {
    let shape = |n: ImageShape, h: ImageShape| {
        n.format == ImageFormat::UNKNOWN
            && ImageShape {
                format: h.format,
                ..n
            } == h
            || n == h
    };
    match (need, have) {
        (BindingKind::StorageImage(n), BindingKind::StorageImage(h))
        | (BindingKind::StorageTexelBuffer(n), BindingKind::StorageTexelBuffer(h)) => shape(n, h),
        _ => need == have,
    }
}

impl Validator<'_> {
    /// The resource bindings the call tree statically uses, and the bytes
    /// of push constants it reads.
    pub(crate) fn resources(&mut self, reach: &Reach) -> Result<(Vec<Binding>, Option<u32>), Rule> {
        let mut bindings: Vec<Binding> = Vec::new();
        let mut push_constants = None;
        for &(slot, access) in &reach.globals {
            let Some(&(id, _)) = self.m.definitions.get(slot as usize) else {
                continue;
            };
            let class = self.class_of(id)?;
            let Ty::Pointer { pointee, .. } = self.ty(self.value_type(id)?).ty else {
                continue;
            };
            if class == StorageClass::PUSH_CONSTANT {
                if push_constants.is_some() {
                    return Err(Rule::Resource);
                }
                let extent = self.buffer_layout(pointee, class)?;
                let bytes = extent
                    .size
                    .and_then(|s| u32::try_from(s).ok())
                    .ok_or(Rule::ExplicitLayout)?;
                push_constants = Some(bytes);
                continue;
            }
            if !matches!(
                class,
                StorageClass::UNIFORM_CONSTANT
                    | StorageClass::UNIFORM
                    | StorageClass::STORAGE_BUFFER
            ) {
                continue;
            }
            let (element, count) = match self.ty(pointee).ty {
                Ty::Array { element, length } => (element, length),
                _ => (pointee, 1),
            };
            let kind = self.binding_kind(class, element)?;
            let set = self
                .decoration_value_of(id, NONE, Decoration::DESCRIPTOR_SET)
                .ok_or(Rule::Resource)?;
            let binding = self
                .decoration_value_of(id, NONE, Decoration::BINDING)
                .ok_or(Rule::Resource)?;
            super::push(
                &mut bindings,
                Binding {
                    set,
                    binding,
                    kind,
                    count,
                    access,
                },
            )
            .map_err(|r| r.rule)?;
        }
        if bindings.len() > MAX_BINDINGS {
            return Err(Rule::Limit(Limit::Bindings));
        }
        bindings.sort_unstable_by_key(|b| (b.set, b.binding));
        if bindings
            .windows(2)
            .any(|w| (w[0].set, w[0].binding) == (w[1].set, w[1].binding))
        {
            return Err(Rule::Resource);
        }
        Ok((bindings, push_constants))
    }

    /// The kind of descriptor a variable of `class` holding `element`
    /// needs.
    fn binding_kind(&self, class: StorageClass, element: u32) -> Result<BindingKind, Rule> {
        let shape = |image: &Image| -> Result<ImageShape, Rule> {
            let component = match self.ty(image.sampled_type).ty {
                Ty::Scalar(Scalar::Float { .. }) => Component::Float,
                Ty::Scalar(Scalar::Int { signed: true, .. }) => Component::Sint,
                Ty::Scalar(Scalar::Int { signed: false, .. }) => Component::Uint,
                _ => return Err(Rule::Resource),
            };
            Ok(ImageShape {
                dim: image.dim,
                arrayed: image.arrayed,
                multisampled: image.multisampled,
                component,
                format: image.format,
            })
        };
        match (class, self.ty(element).ty) {
            (StorageClass::UNIFORM_CONSTANT, Ty::Sampler) => Ok(BindingKind::Sampler),
            (StorageClass::UNIFORM_CONSTANT, Ty::Image(image)) => {
                let s = shape(&image)?;
                Ok(match (image.dim == Dim::BUFFER, image.sampled) {
                    (true, 1) => BindingKind::UniformTexelBuffer(s),
                    (true, _) => BindingKind::StorageTexelBuffer(s),
                    (false, 1) => BindingKind::SampledImage(s),
                    (false, _) => BindingKind::StorageImage(s),
                })
            }
            (StorageClass::UNIFORM_CONSTANT, Ty::SampledImage { image }) => match self.ty(image).ty
            {
                Ty::Image(image) => Ok(BindingKind::CombinedImageSampler(shape(&image)?)),
                _ => Err(Rule::Resource),
            },
            (StorageClass::UNIFORM, Ty::Struct { .. }) => Ok(BindingKind::UniformBuffer),
            (StorageClass::STORAGE_BUFFER, Ty::Struct { .. }) => Ok(BindingKind::StorageBuffer),
            _ => Err(Rule::Resource),
        }
    }
}
