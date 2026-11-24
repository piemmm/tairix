//! The image work: copies between buffers and images and between images,
//! clears of colour, depth and stencil, and a dispatch that fetches a uniform
//! texel buffer, writes a storage texel buffer and a storage image, samples an
//! integer image and compares a depth image with a reference. Every value it
//! leaves is exact on any conforming device: integers, depths, and colours of
//! nought and one, the only ones every device converts to an unsigned
//! normalised value alike.

use alloc::vec;
use alloc::vec::Vec;

use tairix_gpu::{
    Allocation, AllocationDesc, Aspects, BindSetEntry, BindSetLayout, Buffer, BufferImageCopy,
    BufferUsage, BufferViewDesc, ClearValue, CompareOp, ComputePipeline, ComputePipelineDesc,
    Context, ContextDesc, DescriptorType, Device, Error, Extent3d, Filter, Format, FormatFeatures,
    Image, ImageCopy, ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage,
    ImageViewDesc, LayoutBinding, Origin3d, PushConstants, QueueCapabilities, Resource, SampleType,
    SamplerDesc, SamplerType, ViewDimension,
};
use tairix_spirv::enumerant::{
    BuiltIn, Capability, Decoration, Dim, ExecutionMode, ExecutionModel, ImageFormat,
    ImageOperands, StorageClass,
};
use tairix_spirv::{
    Access, Admitted, Arg, BuildError, Builder, Function, Id, ImageShape, Op, Stages, Version,
};

use super::{buffer, family, submit};

/// What the work asks of each format it uses, beside copying it.
const USES: [(Format, FormatFeatures); 4] = [
    (Format::Rgba8Unorm, FormatFeatures::SAMPLED),
    (Format::Depth24PlusStencil8, FormatFeatures::SAMPLED),
    (Format::Depth32Float, FormatFeatures::SAMPLED),
    (
        Format::R32Uint,
        FormatFeatures::SAMPLED
            .with(FormatFeatures::STORAGE)
            .with(FormatFeatures::UNIFORM_TEXEL_BUFFER)
            .with(FormatFeatures::STORAGE_TEXEL_BUFFER),
    ),
];

/// The side of the square grid the dispatch runs over and its images hold.
const SIDE: u32 = 4;

/// The texels the dispatch's images and texel buffers hold.
const TEXELS: u32 = SIDE * SIDE;

/// The words the dispatch writes for each invocation.
const WORDS: u32 = 3;

/// What the image work leaves on `device`, through host-visible memory of
/// type `near`; `None` where the device states not every use it makes of
/// the formats it reads.
pub(crate) fn work(device: &Device, near: u32) -> Result<Option<Vec<u8>>, Error> {
    let formats = &device.info().formats;
    if !USES
        .iter()
        .all(|&(format, uses)| formats.get(format).contains(uses))
    {
        return Ok(None);
    }
    let transfer = family(device, QueueCapabilities::TRANSFER)?;
    let compute = family(device, QueueCapabilities::COMPUTE)?;
    let context = device.create_context(&ContextDesc::default())?;
    let mut left = transfers(&context, (near, transfer))?;
    left.extend(dispatch(&context, (near, transfer, compute))?);
    Ok(Some(left))
}

const fn square(format: Format, (levels, layers): (u32, u32), usage: ImageUsage) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width: SIDE,
            height: SIDE,
            depth: 1,
        },
        mip_levels: levels,
        array_layers: layers,
        samples: 1,
        usage,
    }
}

/// An image of `desc` in memory of its own, of the first type it may lie in.
pub(crate) fn placed(context: &Context, desc: &ImageDesc) -> Result<(Allocation, Image), Error> {
    let required = context.image_requirements(desc)?;
    let memory = context.allocate(&AllocationDesc {
        size: required.size,
        memory_type: required.memory_types.trailing_zeros(),
    })?;
    let image = context.create_image(&memory, 0, desc)?;
    Ok((memory, image))
}

/// A tightly packed copy of `layers` layers of plane `aspect` of level
/// `level`, `side` texels square of `bytes` bytes each, at `offset`.
pub(crate) const fn packed(
    offset: u64,
    (aspect, level, layers): (Aspects, u32, u32),
    (side, bytes): (u32, u32),
) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset: offset,
        bytes_per_row: side * bytes,
        rows_per_image: side,
        image: ImageSubresource {
            aspect,
            level,
            base_layer: 0,
            layers,
        },
        origin: Origin3d { x: 0, y: 0, z: 0 },
        extent: Extent3d {
            width: side,
            height: side,
            depth: 1,
        },
    }
}

/// What each of `parts` reads of its image — `regions` of it into `size`
/// bytes of its own, the parts one after another — in one submission on a
/// queue of family `transfer`, into host-visible memory of type `near`.
pub(crate) fn read_back(
    context: &Context,
    (near, transfer): (u32, u32),
    parts: &[(&Image, &[BufferImageCopy], u64)],
) -> Result<Vec<u8>, Error> {
    let total = parts
        .iter()
        .try_fold(0u64, |total, &(_, _, size)| total.checked_add(size))
        .ok_or(Error::TooLarge)?;
    let (memory, out) = buffer(context, (near, total), BufferUsage::TRANSFER_DST)?;
    let mut commands = context.command_buffer();
    let mut base = 0;
    for &(image, regions, size) in parts {
        let placed: Vec<BufferImageCopy> = regions
            .iter()
            .map(|region| BufferImageCopy {
                buffer_offset: region.buffer_offset + base,
                ..*region
            })
            .collect();
        commands.copy_image_to_buffer(image, &out, &placed)?;
        base += size;
    }
    submit(context, transfer, &commands)?;
    let mut bytes = vec![0; usize::try_from(total).map_err(|_| Error::TooLarge)?];
    memory.read(0, &mut bytes)?;
    Ok(bytes)
}

/// A layer of a colour image's first level.
const fn layer(layer: u32) -> ImageSubresource {
    ImageSubresource {
        aspect: Aspects::COLOR,
        level: 0,
        base_layer: layer,
        layers: 1,
    }
}

/// The transfers, on a queue of family `transfer`: a pattern copied into a
/// corner of one image's second layer and the whole of its second level,
/// part of that layer copied into another image, colour cleared into that
/// one's second level, a depth image filled from memory, and a depth and
/// stencil image cleared whole and then only in its stencil; every image
/// read back.
fn transfers(context: &Context, (near, transfer): (u32, u32)) -> Result<Vec<u8>, Error> {
    let usage = ImageUsage::TRANSFER_SRC
        .with(ImageUsage::TRANSFER_DST)
        .with(ImageUsage::SAMPLED);
    let colour = square(Format::Rgba8Unorm, (2, 2), usage);
    let (_a_memory, a) = placed(context, &colour)?;
    let (_b_memory, b) = placed(context, &colour)?;
    let (_depth_memory, depth) = placed(context, &square(Format::Depth32Float, (1, 1), usage))?;
    let (_planes_memory, planes) =
        placed(context, &square(Format::Depth24PlusStencil8, (1, 2), usage))?;
    let (staging_memory, staging) = buffer(context, (near, 512), BufferUsage::TRANSFER_SRC)?;
    staging_memory.write(0, &super::pattern(512))?;
    staging_memory.write(DEPTHS_AT, &depths())?;
    let mut commands = context.command_buffer();
    let corner = BufferImageCopy {
        buffer_offset: 8,
        bytes_per_row: 16,
        rows_per_image: 3,
        image: layer(1),
        origin: Origin3d { x: 1, y: 1, z: 0 },
        extent: Extent3d {
            width: 2,
            height: 3,
            depth: 1,
        },
    };
    let second_level = packed(64, (Aspects::COLOR, 1, 2), (SIDE / 2, 4));
    commands.copy_buffer_to_image(&staging, &a, &[corner, second_level])?;
    commands.copy_image(
        &a,
        &b,
        &[ImageCopy {
            src: layer(1),
            src_origin: Origin3d { x: 1, y: 0, z: 0 },
            dst: layer(0),
            dst_origin: Origin3d { x: 0, y: 2, z: 0 },
            extent: Extent3d {
                width: 3,
                height: 2,
                depth: 1,
            },
        }],
    )?;
    let second = ImageRange {
        aspects: Aspects::COLOR,
        base_level: 1,
        levels: 1,
        base_layer: 0,
        layers: 2,
    };
    commands.clear_image(&b, ClearValue::Float([1.0, 0.0, 1.0, 0.0]), &[second])?;
    commands.copy_buffer_to_image(&staging, &depth, &[depth_ramp()])?;
    let whole = ImageRange {
        aspects: Aspects::DEPTH_STENCIL,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 2,
    };
    let second_stencil = ImageRange {
        aspects: Aspects::STENCIL,
        base_layer: 1,
        layers: 1,
        ..whole
    };
    for (range, depth, stencil) in [(whole, 0.75, 9), (second_stencil, 0.25, 3)] {
        commands.clear_image(
            &planes,
            ClearValue::DepthStencil { depth, stencil },
            &[range],
        )?;
    }
    submit(context, transfer, &commands)?;
    let colour_regions = [
        packed(0, (Aspects::COLOR, 0, 2), (SIDE, 4)),
        packed(128, (Aspects::COLOR, 1, 2), (SIDE / 2, 4)),
    ];
    let depth_plane = [packed(0, (Aspects::DEPTH, 0, 1), (SIDE, 4))];
    let stencil = [packed(0, (Aspects::STENCIL, 0, 2), (SIDE, 1))];
    read_back(
        context,
        (near, transfer),
        &[
            (&a, &colour_regions, 160),
            (&b, &colour_regions, 160),
            (&depth, &depth_plane, 64),
            (&planes, &stencil, 32),
        ],
    )
}

/// Where [`depths`] lie in a staging buffer.
const DEPTHS_AT: u64 = 256;

/// A copy of [`depths`] into a depth image from where they lie.
const fn depth_ramp() -> BufferImageCopy {
    packed(DEPTHS_AT, (Aspects::DEPTH, 0, 1), (SIDE, 4))
}

/// The bytes of a depth image `SIDE` texels square: depths within the range
/// every device keeps, none tying with a comparison the dispatch makes.
fn depths() -> Vec<u8> {
    (0..TEXELS)
        .flat_map(|at| {
            let step = u16::try_from(at % 5).map_or(0.0, f32::from);
            (step * 0.2 + 0.125).to_le_bytes()
        })
        .collect()
}

/// The shape of a two-dimensional image a module samples or stores of
/// `component`.
const fn flat(component: tairix_spirv::Component, format: ImageFormat) -> ImageShape {
    ImageShape {
        dim: Dim::DIM_2D,
        arrayed: false,
        multisampled: false,
        component,
        format,
    }
}

/// The dispatch's bindings, in [`program`]'s order.
fn bindings() -> [LayoutBinding; 7] {
    use tairix_spirv::Component::{Float, Uint};
    let texel_buffer = ImageShape {
        dim: Dim::BUFFER,
        ..flat(Uint, ImageFormat::UNKNOWN)
    };
    let stored = ImageShape {
        format: ImageFormat::R32UI,
        ..texel_buffer
    };
    let kinds = [
        (
            DescriptorType::UniformTexelBuffer(texel_buffer),
            Access::READ,
        ),
        (DescriptorType::StorageTexelBuffer(stored), Access::WRITE),
        (
            DescriptorType::StorageImage(flat(Uint, ImageFormat::R32UI)),
            Access::WRITE,
        ),
        (
            DescriptorType::CombinedImageSampler(
                flat(Uint, ImageFormat::UNKNOWN),
                SampleType::Uint,
                SamplerType::Integer,
            ),
            Access::READ,
        ),
        (
            DescriptorType::SampledImage(
                flat(Float, ImageFormat::UNKNOWN),
                SampleType::UnfilterableDepth,
            ),
            Access::READ,
        ),
        (
            DescriptorType::Sampler(SamplerType::NonFilteringComparison),
            Access::READ,
        ),
        (DescriptorType::StorageBuffer, Access::WRITE),
    ];
    let mut next = 0;
    kinds.map(|(kind, access)| {
        next += 1;
        LayoutBinding {
            binding: next - 1,
            kind,
            count: 1,
            access,
            stages: Stages::COMPUTE,
        }
    })
}

/// A uniform-constant variable of `ty` at set 0, `binding`.
fn resource(b: &mut Builder, ty: Id, binding: u32) -> Result<Id, BuildError> {
    let pointer = b.pointer(StorageClass::UNIFORM_CONSTANT, ty)?;
    let variable = b.variable(pointer, StorageClass::UNIFORM_CONSTANT)?;
    b.decorate(variable, Decoration::DESCRIPTOR_SET, &[0])?;
    b.decorate(variable, Decoration::BINDING, &[binding])?;
    Ok(variable)
}

/// An image type of `texel`s: `dim`, depth where `depth`, sampled or
/// stored as `sampled` says (one or two), of `format`.
fn image_type(
    b: &mut Builder,
    texel: Id,
    (dim, depth, sampled): (Dim, bool, u32),
    format: ImageFormat,
) -> Result<Id, BuildError> {
    b.declare(
        Op::TYPE_IMAGE,
        None,
        &[
            Arg::Id(texel),
            Arg::Word(dim.0),
            Arg::Word(u32::from(depth)),
            Arg::Word(0),
            Arg::Word(0),
            Arg::Word(sampled),
            Arg::Word(format.0),
        ],
    )
}

/// The ids the dispatch's body reads.
struct Kit {
    uint: Id,
    int: Id,
    float: Id,
    v2int: Id,
    v2float: Id,
    v3uint: Id,
    v4uint: Id,
    global: Id,
    fetched: (Id, Id),
    stored: (Id, Id),
    surface: (Id, Id),
    combined: (Id, Id),
    depth: (Id, Id),
    comparing: (Id, Id),
    depth_sampled: Id,
    out: Id,
    word: Id,
}

/// The module's types and resources, each at its binding in [`bindings`]'s
/// order: a resource's type and variable.
fn declared(b: &mut Builder) -> Result<Kit, BuildError> {
    let (uint, int, float) = (b.int(32, false)?, b.int(32, true)?, b.float(32)?);
    let (v2int, v2float) = (b.vector(int, 2)?, b.vector(float, 2)?);
    let (v3uint, v4uint) = (b.vector(uint, 3)?, b.vector(uint, 4)?);
    let input = b.pointer(StorageClass::INPUT, v3uint)?;
    let global = b.variable(input, StorageClass::INPUT)?;
    b.decorate(
        global,
        Decoration::BUILT_IN,
        &[BuiltIn::GLOBAL_INVOCATION_ID.0],
    )?;
    let fetched_t = image_type(b, uint, (Dim::BUFFER, false, 1), ImageFormat::UNKNOWN)?;
    let stored_t = image_type(b, uint, (Dim::BUFFER, false, 2), ImageFormat::R32UI)?;
    let surface_t = image_type(b, uint, (Dim::DIM_2D, false, 2), ImageFormat::R32UI)?;
    let integer_t = image_type(b, uint, (Dim::DIM_2D, false, 1), ImageFormat::UNKNOWN)?;
    let combined_t = b.declare(Op::TYPE_SAMPLED_IMAGE, None, &[Arg::Id(integer_t)])?;
    let depth_t = image_type(b, float, (Dim::DIM_2D, true, 1), ImageFormat::UNKNOWN)?;
    let depth_sampled = b.declare(Op::TYPE_SAMPLED_IMAGE, None, &[Arg::Id(depth_t)])?;
    let comparing_t = b.declare(Op::TYPE_SAMPLER, None, &[])?;
    let words = b.runtime_array(uint)?;
    b.decorate(words, Decoration::ARRAY_STRIDE, &[4])?;
    let block = b.structure(&[words])?;
    b.decorate(block, Decoration::BLOCK, &[])?;
    b.member_decorate(block, 0, Decoration::OFFSET, &[0])?;
    let block_ptr = b.pointer(StorageClass::STORAGE_BUFFER, block)?;
    let out = b.variable(block_ptr, StorageClass::STORAGE_BUFFER)?;
    b.decorate(out, Decoration::DESCRIPTOR_SET, &[0])?;
    b.decorate(out, Decoration::BINDING, &[6])?;
    let word = b.pointer(StorageClass::STORAGE_BUFFER, uint)?;
    Ok(Kit {
        uint,
        int,
        float,
        v2int,
        v2float,
        v3uint,
        v4uint,
        global,
        fetched: (fetched_t, resource(b, fetched_t, 0)?),
        stored: (stored_t, resource(b, stored_t, 1)?),
        surface: (surface_t, resource(b, surface_t, 2)?),
        combined: (combined_t, resource(b, combined_t, 3)?),
        depth: (depth_t, resource(b, depth_t, 4)?),
        comparing: (comparing_t, resource(b, comparing_t, 5)?),
        depth_sampled,
        out,
        word,
    })
}

/// One invocation's place in the grid: its column and row, and its index
/// among the grid's texels, as a word and as a signed one.
struct Place {
    column: Id,
    line: Id,
    index: Id,
    at: Id,
}

impl Kit {
    /// Component `at` of `of`, a vector of words.
    fn extract(&self, f: &mut Function<'_>, of: Id, at: u32) -> Result<Id, BuildError> {
        f.op_args(
            Op::COMPOSITE_EXTRACT,
            self.uint,
            &[Arg::Id(of), Arg::Word(at)],
        )
    }

    /// The invocation's place.
    fn place(&self, f: &mut Function<'_>) -> Result<Place, BuildError> {
        let id = f.op(Op::LOAD, self.v3uint, &[self.global])?;
        let (column, line) = (self.extract(f, id, 0)?, self.extract(f, id, 1)?);
        let side = f.module().uint(SIDE)?;
        let start = f.op(Op::I_MUL, self.uint, &[line, side])?;
        let index = f.op(Op::I_ADD, self.uint, &[start, column])?;
        let at = f.op(Op::BITCAST, self.int, &[index])?;
        Ok(Place {
            column,
            line,
            index,
            at,
        })
    }

    /// The four-word texel holding `value` alone.
    fn texel(&self, f: &mut Function<'_>, value: Id) -> Result<Id, BuildError> {
        let zero = f.module().uint(0)?;
        f.op(
            Op::COMPOSITE_CONSTRUCT,
            self.v4uint,
            &[value, zero, zero, zero],
        )
    }

    /// Fetch the uniform texel buffer at `place`, store it plus a hundred
    /// into the storage texel buffer there, and mark the storage image there
    /// with its index plus a thousand: what was fetched.
    fn fetch_store_and_mark(&self, f: &mut Function<'_>, place: &Place) -> Result<Id, BuildError> {
        let fetching = f.op(Op::LOAD, self.fetched.0, &[self.fetched.1])?;
        let fetched_texel = f.op(Op::IMAGE_FETCH, self.v4uint, &[fetching, place.at])?;
        let fetched = self.extract(f, fetched_texel, 0)?;
        let hundred = f.module().uint(100)?;
        let plus = f.op(Op::I_ADD, self.uint, &[fetched, hundred])?;
        let stored = self.texel(f, plus)?;
        let storing = f.op(Op::LOAD, self.stored.0, &[self.stored.1])?;
        f.emit(Op::IMAGE_WRITE, &[storing, place.at, stored])?;
        let column = f.op(Op::BITCAST, self.int, &[place.column])?;
        let line = f.op(Op::BITCAST, self.int, &[place.line])?;
        let coordinate = f.op(Op::COMPOSITE_CONSTRUCT, self.v2int, &[column, line])?;
        let thousand = f.module().uint(1000)?;
        let mark = f.op(Op::I_ADD, self.uint, &[place.index, thousand])?;
        let marked = self.texel(f, mark)?;
        let surface = f.op(Op::LOAD, self.surface.0, &[self.surface.1])?;
        f.emit(Op::IMAGE_WRITE, &[surface, coordinate, marked])?;
        Ok(fetched)
    }

    /// Sample the integer image at the centre of `place`'s texel, and
    /// compare its texel of the depth image with a reference of its column
    /// over the grid's side: the value sampled and the comparison's word.
    fn sample_and_compare(
        &self,
        f: &mut Function<'_>,
        place: &Place,
    ) -> Result<(Id, Id), BuildError> {
        let side = f.module().uint(SIDE)?;
        let side_real = f.op(Op::CONVERT_U_TO_F, self.float, &[side])?;
        let half = f.module().real(0.5)?;
        let centre = |f: &mut Function<'_>, axis: Id| {
            let real = f.op(Op::CONVERT_U_TO_F, self.float, &[axis])?;
            let middle = f.op(Op::F_ADD, self.float, &[real, half])?;
            f.op(Op::F_DIV, self.float, &[middle, side_real])
        };
        let across = centre(f, place.column)?;
        let down = centre(f, place.line)?;
        let coordinate = f.op(Op::COMPOSITE_CONSTRUCT, self.v2float, &[across, down])?;
        let level = f.module().real(0.0)?;
        let explicit = |sampled: Id, reference: Option<Id>| {
            let mut args = vec![Arg::Id(sampled), Arg::Id(coordinate)];
            args.extend(reference.map(Arg::Id));
            args.extend([Arg::Word(ImageOperands::LOD.0), Arg::Id(level)]);
            args
        };
        let integer = f.op(Op::LOAD, self.combined.0, &[self.combined.1])?;
        let sampled_texel = f.op_args(
            Op::IMAGE_SAMPLE_EXPLICIT_LOD,
            self.v4uint,
            &explicit(integer, None),
        )?;
        let sampled = self.extract(f, sampled_texel, 0)?;
        let depth = f.op(Op::LOAD, self.depth.0, &[self.depth.1])?;
        let comparing = f.op(Op::LOAD, self.comparing.0, &[self.comparing.1])?;
        let compared_image = f.op(Op::SAMPLED_IMAGE, self.depth_sampled, &[depth, comparing])?;
        let column = f.op(Op::CONVERT_U_TO_F, self.float, &[place.column])?;
        let reference = f.op(Op::F_DIV, self.float, &[column, side_real])?;
        let compared_real = f.op_args(
            Op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD,
            self.float,
            &explicit(compared_image, Some(reference)),
        )?;
        let compared = f.op(Op::CONVERT_F_TO_U, self.uint, &[compared_real])?;
        Ok((sampled, compared))
    }

    /// Write `values` as the invocation at `index`'s words of the storage
    /// buffer.
    fn write(&self, f: &mut Function<'_>, index: Id, values: [Id; 3]) -> Result<(), BuildError> {
        let (zero, words) = (f.module().uint(0)?, f.module().uint(WORDS)?);
        let first = f.op(Op::I_MUL, self.uint, &[index, words])?;
        for (offset, value) in (0u32..).zip(values) {
            let step = f.module().uint(offset)?;
            let at = f.op(Op::I_ADD, self.uint, &[first, step])?;
            let pointer = f.op(Op::ACCESS_CHAIN, self.word, &[self.out, zero, at])?;
            f.emit(Op::STORE, &[pointer, value])?;
        }
        Ok(())
    }
}

/// The dispatch's program: each invocation of a `SIDE` × `SIDE` grid
/// fetches, stores and marks at its texel, samples and compares there, and
/// writes what it fetched, sampled and compared as its words of the storage
/// buffer.
fn program() -> Result<Admitted, BuildError> {
    let mut builder = Builder::new(Version::OLDEST, 0)?;
    for capability in [
        Capability::SHADER,
        Capability::SAMPLED_BUFFER,
        Capability::IMAGE_BUFFER,
    ] {
        builder.capability(capability)?;
    }
    let kit = declared(&mut builder)?;
    let (void, main) = (builder.void()?, builder.id()?);
    builder.define(main, void, &[], |f, _| {
        let place = kit.place(f)?;
        let fetched = kit.fetch_store_and_mark(f, &place)?;
        let (sampled, compared) = kit.sample_and_compare(f, &place)?;
        kit.write(f, place.index, [fetched, sampled, compared])?;
        f.emit(Op::RETURN, &[])
    })?;
    builder.entry_point(ExecutionModel::GL_COMPUTE, main, "main", &[kit.global])?;
    builder.execution_mode(main, ExecutionMode::LOCAL_SIZE, &[SIDE, SIDE, 1])?;
    builder.finish()
}

/// A view of all of `image` as `format`, seeing `aspects`.
pub(crate) fn whole_view(
    context: &Context,
    image: &Image,
    (format, aspects): (Format, Aspects),
) -> Result<tairix_gpu::ImageView, Error> {
    context.create_image_view(
        image,
        &ImageViewDesc {
            format,
            dimension: ViewDimension::D2,
            range: ImageRange {
                aspects,
                base_level: 0,
                levels: 1,
                base_layer: 0,
                layers: 1,
            },
        },
    )
}

/// The dispatch's pipeline over [`program`], and its set's layout.
fn dispatch_pipeline(context: &Context) -> Result<(ComputePipeline, BindSetLayout), Error> {
    let module = program().map_err(|_| Error::Malformed)?;
    let layout = context.create_bind_set_layout(&bindings())?;
    let laid_out = context.create_pipeline_layout(&[&layout], PushConstants::NONE)?;
    let shader = context.create_shader_module(module)?;
    let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
        module: &shader,
        entry_point: "main",
        layout: &laid_out,
        specialization: &[],
    })?;
    Ok((pipeline, layout))
}

/// The images the dispatch samples, filled on a queue of family `transfer`:
/// an integer image of each texel's index times seven plus one, and a depth
/// image of [`depths`].
fn sampled_images(
    context: &Context,
    (near, transfer): (u32, u32),
) -> Result<[(Allocation, Image); 2], Error> {
    let usage = ImageUsage::SAMPLED.with(ImageUsage::TRANSFER_DST);
    let (integer_memory, integer) = placed(context, &square(Format::R32Uint, (1, 1), usage))?;
    let (depth_memory, depth) = placed(context, &square(Format::Depth32Float, (1, 1), usage))?;
    let (staging_memory, staging) = buffer(context, (near, 512), BufferUsage::TRANSFER_SRC)?;
    let integers: Vec<u8> = (0..TEXELS)
        .flat_map(|at| (at * 7 + 1).to_le_bytes())
        .collect();
    staging_memory.write(0, &integers)?;
    staging_memory.write(DEPTHS_AT, &depths())?;
    let mut commands = context.command_buffer();
    let colour = [packed(0, (Aspects::COLOR, 0, 1), (SIDE, 4))];
    commands.copy_buffer_to_image(&staging, &integer, &colour)?;
    commands.copy_buffer_to_image(&staging, &depth, &[depth_ramp()])?;
    submit(context, transfer, &commands)?;
    Ok([(integer_memory, integer), (depth_memory, depth)])
}

/// The bytes `len` long `memory` holds from its start.
fn read(memory: &Allocation, len: u64) -> Result<Vec<u8>, Error> {
    let mut bytes = vec![0; usize::try_from(len).map_err(|_| Error::TooLarge)?];
    memory.read(0, &mut bytes)?;
    Ok(bytes)
}

/// The dispatch over [`program`], on a queue of family `compute`, its images
/// filled and read back on one of family `transfer`: what it wrote to its
/// storage buffer and storage texel buffer, and what it left in its storage
/// image.
fn dispatch(
    context: &Context,
    (near, transfer, compute): (u32, u32, u32),
) -> Result<Vec<u8>, Error> {
    let (pipeline, layout) = dispatch_pipeline(context)?;
    let texel_bytes = u64::from(TEXELS) * 4;
    let fetched_words: Vec<u8> = (0..TEXELS)
        .flat_map(|at| (at * at + 3).to_le_bytes())
        .collect();
    let (fetched_memory, fetched_buffer) =
        buffer(context, (near, texel_bytes), BufferUsage::UNIFORM_TEXEL)?;
    fetched_memory.write(0, &fetched_words)?;
    let (stored_memory, stored_buffer) =
        buffer(context, (near, texel_bytes), BufferUsage::STORAGE_TEXEL)?;
    let texels = |of: &Buffer| {
        context.create_buffer_view(
            of,
            &BufferViewDesc {
                offset: 0,
                size: texel_bytes,
                format: Format::R32Uint,
            },
        )
    };
    let (fetch, store) = (texels(&fetched_buffer)?, texels(&stored_buffer)?);
    let marked_usage = ImageUsage::STORAGE.with(ImageUsage::TRANSFER_SRC);
    let (_surface_memory, surface) =
        placed(context, &square(Format::R32Uint, (1, 1), marked_usage))?;
    let [(_integer_memory, integer), (_depth_memory, depth)] =
        sampled_images(context, (near, transfer))?;
    let surface_view = whole_view(context, &surface, (Format::R32Uint, Aspects::COLOR))?;
    let integer_view = whole_view(context, &integer, (Format::R32Uint, Aspects::COLOR))?;
    let depth_view = whole_view(context, &depth, (Format::Depth32Float, Aspects::DEPTH))?;
    let nearest = SamplerDesc {
        mag_filter: Filter::Nearest,
        min_filter: Filter::Nearest,
        mipmap_filter: Filter::Nearest,
        ..SamplerDesc::LINEAR
    };
    let reading = context.create_sampler(&nearest)?;
    let comparing = context.create_sampler(&SamplerDesc {
        compare: Some(CompareOp::LessOrEqual),
        ..nearest
    })?;
    let out_bytes = u64::from(TEXELS * WORDS) * 4;
    let (out_memory, out) = buffer(context, (near, out_bytes), BufferUsage::STORAGE)?;
    let resources = [
        Resource::TexelBuffer(&fetch),
        Resource::TexelBuffer(&store),
        Resource::Image(&surface_view),
        Resource::ImageSampler(&integer_view, &reading),
        Resource::Image(&depth_view),
        Resource::Sampler(&comparing),
        Resource::Buffer {
            buffer: &out,
            offset: 0,
            size: out_bytes,
        },
    ];
    let entries: Vec<BindSetEntry<'_>> = (0u32..)
        .zip(&resources)
        .map(|(binding, resource)| BindSetEntry {
            binding,
            resources: core::slice::from_ref(resource),
        })
        .collect();
    let set = context.create_bind_set(&layout, &entries)?;
    let mut run = context.command_buffer();
    run.dispatch(&pipeline, &[&set], &[], [1, 1, 1])?;
    submit(context, compute, &run)?;
    let mut left = read(&out_memory, out_bytes)?;
    left.extend(read(&stored_memory, texel_bytes)?);
    let whole = [packed(0, (Aspects::COLOR, 0, 1), (SIDE, 4))];
    left.extend(read_back(
        context,
        (near, transfer),
        &[(&surface, &whole, texel_bytes)],
    )?);
    Ok(left)
}
