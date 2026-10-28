//! Images: sampling, fetching, gathering, reading, writing and querying,
//! each against the image type it reaches; the image operands each takes;
//! and each texel access charged to the image's variable.

use super::body::{Access, Args, Body, Needs, Root, Typing};
use super::types::{Image, Scalar, Ty};
use super::{Rule, Validator, What};
use crate::enumerant::{Capability, Dim, ImageFormat, ImageOperands};
use crate::{Id, Op, Value};

/// Which kind of image instruction image operands are read for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Use {
    Sample { explicit: bool },
    Fetch,
    Gather,
    Read,
    Write,
}

impl Validator<'_> {
    /// Check one image instruction.
    #[expect(
        clippy::too_many_lines,
        reason = "each image instruction's operands read in its own arm"
    )]
    pub(crate) fn image(
        &mut self,
        body: &mut Body<'_>,
        typing: Typing,
        args: &Args<'_>,
    ) -> Result<(), Rule> {
        let r = args.result_type;
        match typing {
            Typing::SampledImage => {
                let Ty::SampledImage { image } = self.ty(r).ty else {
                    return Err(Rule::ResultType);
                };
                let Ty::Image(of) = self.ty(image).ty else {
                    return Err(Rule::ResultType);
                };
                let (given, root) = self.image_operand(args, 0)?;
                if !given.matches(&of) {
                    return Err(args.wrong(0));
                }
                if !matches!(self.ty(self.arg_type(args, 1)?).ty, Ty::Sampler) {
                    return Err(args.wrong(1));
                }
                self.keep_root(args, root);
                Ok(())
            }
            Typing::Image => {
                let (image, root) = self.sampled_operand(args, 0)?;
                if !matches!(self.ty(r).ty, Ty::Image(i) if i.matches(&image)) {
                    return Err(Rule::ResultType);
                }
                self.keep_root(args, root);
                Ok(())
            }
            Typing::Sample(form) => {
                if !form.explicit {
                    body.facts.needs = body.facts.needs.with(Needs::FRAGMENT);
                }
                let (image, root) = self.sampled_operand(args, 0)?;
                let dims = match image.dim {
                    Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D => true,
                    Dim::CUBE => !form.proj,
                    _ => false,
                };
                if !dims
                    || image.multisampled
                    || (form.proj && image.arrayed)
                    || (form.dref && image.dim == Dim::DIM_3D)
                {
                    return Err(args.wrong(0));
                }
                if form.dref {
                    if r != image.sampled_type {
                        return Err(Rule::ResultType);
                    }
                } else {
                    self.texels(r, &image)?;
                }
                let needed = image.coordinates() + u32::from(image.arrayed) + u32::from(form.proj);
                self.float_coordinates(args, 1, needed)?;
                let mut next = 2;
                if form.dref {
                    self.float32(args, 2)?;
                    next = 3;
                }
                self.image_operands(
                    args,
                    next,
                    &image,
                    Use::Sample {
                        explicit: form.explicit,
                    },
                    r,
                )?;
                self.texel_access(body, root, Access::READ)
            }
            Typing::Fetch => {
                let (image, root) = self.image_operand(args, 0)?;
                if image.sampled != 1 || image.dim == Dim::CUBE {
                    return Err(args.wrong(0));
                }
                self.texels(r, &image)?;
                self.int_coordinates(args, 1, &image)?;
                self.image_operands(args, 2, &image, Use::Fetch, r)?;
                self.texel_access(body, root, Access::READ)
            }
            Typing::Gather { dref } => {
                let (image, root) = self.sampled_operand(args, 0)?;
                if !matches!(image.dim, Dim::DIM_2D | Dim::CUBE) || image.multisampled {
                    return Err(args.wrong(0));
                }
                self.texels(r, &image)?;
                self.float_coordinates(args, 1, image.coordinates() + u32::from(image.arrayed))?;
                if dref {
                    self.float32(args, 2)?;
                } else {
                    let component = self.arg_id(args, 2)?;
                    let in_range = self.constant_word(component).is_some_and(|c| c <= 3);
                    if !in_range || self.def(component)?.what != What::Constant {
                        return Err(args.wrong(2));
                    }
                }
                self.image_operands(args, 3, &image, Use::Gather, r)?;
                self.texel_access(body, root, Access::READ)
            }
            Typing::ImageRead => {
                let (image, root) = self.image_operand(args, 0)?;
                if image.sampled != 2 {
                    return Err(args.wrong(0));
                }
                if image.format == ImageFormat::UNKNOWN
                    && !self
                        .enabled
                        .has(Capability::STORAGE_IMAGE_READ_WITHOUT_FORMAT)
                {
                    return Err(args.wrong(0));
                }
                self.texels(r, &image)?;
                self.int_coordinates(args, 1, &image)?;
                self.image_operands(args, 2, &image, Use::Read, r)?;
                self.texel_access(body, root, Access::READ)
            }
            Typing::ImageWrite => {
                let (image, root) = self.image_operand(args, 0)?;
                if image.sampled != 2 {
                    return Err(args.wrong(0));
                }
                if image.format == ImageFormat::UNKNOWN
                    && !self
                        .enabled
                        .has(Capability::STORAGE_IMAGE_WRITE_WITHOUT_FORMAT)
                {
                    return Err(args.wrong(0));
                }
                self.int_coordinates(args, 1, &image)?;
                let texel = self.arg_type(args, 2)?;
                match self.components(texel) {
                    Some((s, count))
                        if self.scalar_index(s) == Some(image.sampled_type)
                            && count >= channels(image.format) => {}
                    _ => return Err(args.wrong(2)),
                }
                self.image_operands(args, 3, &image, Use::Write, texel)?;
                self.texel_access(body, root, Access::WRITE)
            }
            Typing::QuerySizeLod | Typing::QuerySize => {
                let (image, _) = self.image_operand(args, 0)?;
                let lod = typing == Typing::QuerySizeLod;
                let admitted = if lod {
                    image.sampled == 1
                        && matches!(
                            image.dim,
                            Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE
                        )
                        && !image.multisampled
                } else {
                    match image.dim {
                        Dim::BUFFER => true,
                        Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE => {
                            image.multisampled || image.sampled == 2
                        }
                        _ => false,
                    }
                };
                if !admitted {
                    return Err(args.wrong(0));
                }
                let size = match image.dim {
                    Dim::DIM_3D => 3,
                    Dim::DIM_2D | Dim::CUBE => 2,
                    _ => 1,
                } + u32::from(image.arrayed);
                match self.components(r) {
                    Some((s, count)) if s.is_int() && count == size => {}
                    _ => return Err(Rule::ResultType),
                }
                if lod {
                    self.int32(args, 1)?;
                }
                Ok(())
            }
            Typing::QueryLod => {
                body.facts.needs = body.facts.needs.with(Needs::FRAGMENT);
                let (image, _) = self.sampled_operand(args, 0)?;
                if !matches!(
                    image.dim,
                    Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE
                ) || image.multisampled
                {
                    return Err(args.wrong(0));
                }
                match self.ty(r).ty {
                    Ty::Vector {
                        scalar: Scalar::Float { .. },
                        count: 2,
                    } => {}
                    _ => return Err(Rule::ResultType),
                }
                self.float_coordinates(args, 1, image.coordinates())
            }
            Typing::QueryLevels | Typing::QuerySamples => {
                let (image, _) = self.image_operand(args, 0)?;
                let admitted = if typing == Typing::QueryLevels {
                    image.sampled == 1
                        && matches!(
                            image.dim,
                            Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE
                        )
                        && !image.multisampled
                } else {
                    image.dim == Dim::DIM_2D && image.multisampled
                };
                if !admitted {
                    return Err(args.wrong(0));
                }
                match self.ty(r).ty {
                    Ty::Scalar(s) if s.is_int() => Ok(()),
                    _ => Err(Rule::ResultType),
                }
            }
            _ => Err(Rule::NotAdmitted(args.op)),
        }
    }

    /// Operand `n`, an image, and the root of the variable it was loaded
    /// from.
    fn image_operand(&self, args: &Args<'_>, n: usize) -> Result<(Image, Option<Root>), Rule> {
        let id = self.arg_id(args, n)?;
        let ty = self.value_type(id).map_err(|_| args.wrong(n))?;
        match self.ty(ty).ty {
            Ty::Image(image) => Ok((image, self.root(id))),
            _ => Err(args.wrong(n)),
        }
    }

    /// Operand `n`, a sampled image, its image type, and its root.
    fn sampled_operand(&self, args: &Args<'_>, n: usize) -> Result<(Image, Option<Root>), Rule> {
        let id = self.arg_id(args, n)?;
        let ty = self.value_type(id).map_err(|_| args.wrong(n))?;
        let Ty::SampledImage { image } = self.ty(ty).ty else {
            return Err(args.wrong(n));
        };
        match self.ty(image).ty {
            Ty::Image(image) => Ok((image, self.root(id))),
            _ => Err(args.wrong(n)),
        }
    }

    fn keep_root(&mut self, args: &Args<'_>, root: Option<Root>) {
        if let (Some(result), Some(root)) = (args.result, root) {
            self.set_root(result, root);
        }
    }

    /// A texel read or write charged to the image's variable, refused where
    /// its decorations forbid it.
    fn texel_access(
        &self,
        body: &mut Body<'_>,
        root: Option<Root>,
        access: Access,
    ) -> Result<(), Rule> {
        let root = root.ok_or(Rule::OpaqueUse)?;
        self.access(body, self.texel_root(root.slot), access)
    }

    /// `ty` is four components of the image's sampled type.
    fn texels(&self, ty: u32, image: &Image) -> Result<(), Rule> {
        match self.ty(ty).ty {
            Ty::Vector { scalar, count: 4 }
                if self.scalar_index(scalar) == Some(image.sampled_type) =>
            {
                Ok(())
            }
            _ => Err(Rule::ResultType),
        }
    }

    /// Operand `n` is a 32-bit float scalar or vector of at least `needed`
    /// components.
    fn float_coordinates(&self, args: &Args<'_>, n: usize, needed: u32) -> Result<(), Rule> {
        match self.components(self.arg_type(args, n)?) {
            Some((Scalar::Float { width: 32 }, count)) if count >= needed => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    /// Operand `n` is a 32-bit integer scalar or vector addressing a texel
    /// of `image`, its array layer included.
    fn int_coordinates(&self, args: &Args<'_>, n: usize, image: &Image) -> Result<(), Rule> {
        let needed = image.coordinates() + u32::from(image.arrayed);
        match self.components(self.arg_type(args, n)?) {
            Some((Scalar::Int { width: 32, .. }, count)) if count >= needed => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    fn float32(&self, args: &Args<'_>, n: usize) -> Result<(), Rule> {
        match self.ty(self.arg_type(args, n)?).ty {
            Ty::Scalar(Scalar::Float { width: 32 }) => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    fn int32(&self, args: &Args<'_>, n: usize) -> Result<(), Rule> {
        match self.ty(self.arg_type(args, n)?).ty {
            Ty::Scalar(Scalar::Int { width: 32, .. }) => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    /// The image operands from operand `from`: the mask, then each set
    /// bit's operands in ascending bit order, each admitted for the
    /// instruction and the image, and exactly the operands the mask names.
    fn image_operands(
        &self,
        args: &Args<'_>,
        from: usize,
        image: &Image,
        used: Use,
        texel: u32,
    ) -> Result<(), Rule> {
        let refused = Rule::ImageOperands;
        let mask = match args.operands.get(from).map(|o| self.m.value(o)) {
            None => ImageOperands::NONE,
            Some(Value::Word(mask)) => ImageOperands(mask),
            Some(_) => return Err(args.wrong(from)),
        };
        let has = |bit: ImageOperands| mask.0 & bit.0 != 0;
        let mut n = from + usize::from(args.len() > from);
        let explicit_lod = matches!(used, Use::Sample { explicit: true });
        let lod_dims = matches!(
            image.dim,
            Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE
        ) && !image.multisampled;
        if has(ImageOperands::BIAS) {
            if !matches!(used, Use::Sample { explicit: false }) || !lod_dims {
                return Err(refused);
            }
            self.float32(args, n)?;
            n += 1;
        }
        if has(ImageOperands::LOD) {
            match used {
                Use::Sample { explicit: true } => self.float32(args, n)?,
                Use::Fetch if lod_dims => self.int32(args, n)?,
                _ => return Err(refused),
            }
            if !lod_dims {
                return Err(refused);
            }
            n += 1;
        }
        if has(ImageOperands::GRAD) {
            if !explicit_lod || has(ImageOperands::LOD) || image.multisampled {
                return Err(refused);
            }
            for _ in 0..2 {
                match self.components(self.arg_type(args, n)?) {
                    Some((Scalar::Float { width: 32 }, count)) if count == image.coordinates() => {}
                    _ => return Err(args.wrong(n)),
                }
                n += 1;
            }
        }
        if explicit_lod && !has(ImageOperands::LOD) && !has(ImageOperands::GRAD) {
            return Err(refused);
        }
        n = self.offset_operands(args, n, image, used, mask)?;
        let sample = matches!(used, Use::Fetch | Use::Read | Use::Write) && image.multisampled;
        if has(ImageOperands::SAMPLE) != sample {
            return Err(refused);
        }
        if has(ImageOperands::SAMPLE) {
            self.int32(args, n)?;
            n += 1;
        }
        let extends = has(ImageOperands::SIGN_EXTEND) || has(ImageOperands::ZERO_EXTEND);
        if has(ImageOperands::SIGN_EXTEND) && has(ImageOperands::ZERO_EXTEND) {
            return Err(refused);
        }
        if extends && !self.components(texel).is_some_and(|(s, _)| s.is_int()) {
            return Err(refused);
        }
        let known = ImageOperands::BIAS.0
            | ImageOperands::LOD.0
            | ImageOperands::GRAD.0
            | ImageOperands::CONST_OFFSET.0
            | ImageOperands::OFFSET.0
            | ImageOperands::CONST_OFFSETS.0
            | ImageOperands::SAMPLE.0
            | ImageOperands::SIGN_EXTEND.0
            | ImageOperands::ZERO_EXTEND.0
            | ImageOperands::NONTEMPORAL.0;
        if mask.0 & !known != 0 || n != args.len() {
            return Err(refused);
        }
        Ok(())
    }

    /// The offset operands at operand `n`, at most one of them, each where
    /// the instruction and the image take it; the operand after them.
    fn offset_operands(
        &self,
        args: &Args<'_>,
        mut n: usize,
        image: &Image,
        used: Use,
        mask: ImageOperands,
    ) -> Result<usize, Rule> {
        let refused = Rule::ImageOperands;
        let has = |bit: ImageOperands| mask.0 & bit.0 != 0;
        let offsets = [
            ImageOperands::CONST_OFFSET,
            ImageOperands::OFFSET,
            ImageOperands::CONST_OFFSETS,
        ]
        .into_iter()
        .filter(|&bit| has(bit))
        .count();
        if offsets > 1
            || (offsets == 1
                && (image.dim == Dim::CUBE
                    || image.sampled != 1
                    || matches!(used, Use::Read | Use::Write)))
        {
            return Err(refused);
        }
        if has(ImageOperands::CONST_OFFSET) {
            self.offset(args, n, image, true)?;
            n += 1;
        }
        if has(ImageOperands::OFFSET) {
            // A computed offset is portable only for a gather.
            if used != Use::Gather {
                return Err(refused);
            }
            self.offset(args, n, image, false)?;
            n += 1;
        }
        if has(ImageOperands::CONST_OFFSETS) {
            if used != Use::Gather {
                return Err(refused);
            }
            self.const_offsets(args, n)?;
            n += 1;
        }
        Ok(n)
    }

    /// An offset: a 32-bit integer scalar or vector with a component per
    /// coordinate; where `constant`, a constant whose every component is in
    /// the range every device takes.
    fn offset(&self, args: &Args<'_>, n: usize, image: &Image, constant: bool) -> Result<(), Rule> {
        let id = self.arg_id(args, n)?;
        match self.components(self.arg_type(args, n)?) {
            Some((Scalar::Int { width: 32, .. }, count)) if count == image.coordinates() => {}
            _ => return Err(args.wrong(n)),
        }
        if constant && !self.offsets_portable(id) {
            return Err(args.wrong(n));
        }
        Ok(())
    }

    /// `ConstOffsets`: a constant array of four two-component 32-bit integer
    /// vectors, each component in the range every device takes.
    fn const_offsets(&self, args: &Args<'_>, n: usize) -> Result<(), Rule> {
        let id = self.arg_id(args, n)?;
        let Ty::Array { element, length: 4 } = self.ty(self.arg_type(args, n)?).ty else {
            return Err(args.wrong(n));
        };
        match self.components(element) {
            Some((Scalar::Int { width: 32, .. }, 2)) if self.offsets_portable(id) => Ok(()),
            _ => Err(args.wrong(n)),
        }
    }

    /// Whether `id` is a constant — not one a specialization may change —
    /// whose every integer, through its constituents, is in -8..=7: the
    /// texel and gather offsets every device takes.
    fn offsets_portable(&self, id: Id) -> bool {
        let Ok(instruction) = self.defining(id) else {
            return false;
        };
        match instruction.op() {
            Op::CONSTANT => self
                .integer_constant(id)
                .and_then(|(value, _)| u32::try_from(value).ok())
                .is_some_and(|word| word <= 7 || word >= (-8i32).cast_unsigned()),
            Op::CONSTANT_COMPOSITE => self.m.operands(instruction).iter().skip(2).all(|o| {
                matches!(self.m.value(o), Value::Id(constituent) if self.offsets_portable(constituent))
            }),
            Op::CONSTANT_NULL => true,
            _ => false,
        }
    }
}

/// How many channels an image format holds; one where it says none.
const fn channels(format: ImageFormat) -> u32 {
    match format {
        ImageFormat::RGBA32F
        | ImageFormat::RGBA16F
        | ImageFormat::RGBA8
        | ImageFormat::RGBA8_SNORM
        | ImageFormat::RGBA16
        | ImageFormat::RGB10_A2
        | ImageFormat::RGBA16_SNORM
        | ImageFormat::RGBA32I
        | ImageFormat::RGBA16I
        | ImageFormat::RGBA8I
        | ImageFormat::RGBA32UI
        | ImageFormat::RGBA16UI
        | ImageFormat::RGBA8UI
        | ImageFormat::RGB10A2UI => 4,
        ImageFormat::R11F_G11F_B10F => 3,
        ImageFormat::RG32F
        | ImageFormat::RG16F
        | ImageFormat::RG16
        | ImageFormat::RG8
        | ImageFormat::RG16_SNORM
        | ImageFormat::RG8_SNORM
        | ImageFormat::RG32I
        | ImageFormat::RG16I
        | ImageFormat::RG8I
        | ImageFormat::RG32UI
        | ImageFormat::RG16UI
        | ImageFormat::RG8UI => 2,
        _ => 1,
    }
}
