//! The corpus's images: samples and gathers at an explicit level, the image
//! a sampled image holds, the size, level and sample queries, a storage
//! image read and written, and atomics through a texel pointer; over the
//! image work's sampled images, a storage image of known words, and a
//! multisampled image only queried.

use alloc::vec::Vec;

use tairix_gpu::{
    Aspects, BufferUsage, ComputePipelineDesc, ContextDesc, DescriptorType, Device, Error, Format,
    FormatFeatures, ImageDesc, ImageUsage, PushConstants, QueueCapabilities, Resource, SampleType,
};
use tairix_spirv::enumerant::{
    Capability, Dim, ImageFormat, ImageOperands, MemorySemantics, Scope, StorageClass,
};
use tairix_spirv::{
    Admitted, Arg, BuildError, Component, Function, Id, ImageShape, ImageType, Op, Stage, Stages,
};

use super::{declare_sampled, exact, layout, op, sampled_bindings, through_buffer, Family, Io};
use crate::images::{image_type, packed, placed, square, whole_view, Sampled, SampledTypes, SIDE};
use crate::{buffer, family, in_binding_order, near, states_uses, submit};

pub(super) const FAMILY: Family =
    Family::compute("the corpus's images", &[Capability::IMAGE_QUERY], images).binding_images();

/// Where the storage image and the multisampled image are bound, past the
/// sampled ones.
const STORAGE_AT: u32 = 4;
const MULTISAMPLED_AT: u32 = 5;

/// How the multisampled image is bound: sampled, never filtered.
const MULTISAMPLED: DescriptorType = DescriptorType::SampledImage(
    ImageShape {
        dim: Dim::DIM_2D,
        arrayed: false,
        multisampled: true,
        component: Component::Float,
        format: ImageFormat::UNKNOWN,
    },
    SampleType::UnfilterableFloat,
);

/// The samples each texel of the multisampled image holds.
const SAMPLES: u32 = 4;

/// What the family asks of each format it reads.
const USES: [(Format, FormatFeatures); 3] = [
    (
        Format::R32Uint,
        FormatFeatures::SAMPLED.with(FormatFeatures::STORAGE),
    ),
    (Format::Depth32Float, FormatFeatures::SAMPLED),
    (
        Format::Rgba8Unorm,
        FormatFeatures::SAMPLED
            .with(FormatFeatures::MULTISAMPLED)
            .with(FormatFeatures::COLOR_ATTACHMENT),
    ),
];

/// The bytes of the words the storage image holds before the kernel runs:
/// each texel's index times five plus two.
fn storage_start() -> Result<Vec<u8>, Error> {
    let texels = SIDE * SIDE;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(texels * 4).map_err(|_| Error::TooLarge)?)
        .map_err(|_| Error::OutOfMemory)?;
    bytes.extend((0..texels).flat_map(|at| (at * 5 + 2).to_le_bytes()));
    Ok(bytes)
}

/// The images the family binds beside the sampled ones, each its type and
/// its variable: the storage image and the multisampled one.
struct Bound {
    storage: (Id, Id),
    multisampled: (Id, Id),
}

/// Each image instruction at a texel's centre or a whole texel, a depth
/// reference that ties with none of the depths, and through the texel
/// pointer an atomic and the word it left.
fn images(f: &mut Function<'_>, io: &mut Io) -> Result<(), BuildError> {
    let kit = io.kit;
    let (types, sampled_at) = declare_sampled(f, io)?;
    let storage_type = image_type(
        f.module(),
        kit.uint,
        (Dim::DIM_2D, false, true),
        ImageFormat::R32UI,
    )?;
    let multisampled_type = f.module().image(&ImageType {
        sampled: kit.float,
        dim: Dim::DIM_2D,
        depth: 0,
        arrayed: false,
        multisampled: true,
        storage: false,
        format: ImageFormat::UNKNOWN,
    })?;
    let bound = Bound {
        storage: (storage_type, io.bound(f, (storage_type, STORAGE_AT))?),
        multisampled: (
            multisampled_type,
            io.bound(f, (multisampled_type, MULTISAMPLED_AT))?,
        ),
    };
    sampled(f, io, (types, sampled_at))?;
    stored(f, io, bound.storage)?;
    let (ty, variable) = bound.multisampled;
    let multisampled = f.op(Op::LOAD, ty, &[variable])?;
    op(
        f,
        io,
        exact("ImageQuerySamples"),
        (Op::IMAGE_QUERY_SAMPLES, kit.uint, &[multisampled]),
    )?;
    op(
        f,
        io,
        exact("ImageQuerySize"),
        (Op::IMAGE_QUERY_SIZE, kit.uvec(2), &[multisampled]),
    )
}

/// The sampled images: gathers and samples at an explicit level, plain and
/// projective, compared and not; the image the integer one holds, fetched and
/// queried.
fn sampled(
    f: &mut Function<'_>,
    io: &mut Io,
    (types, [combined_at, depth_at, comparing_at]): (SampledTypes, [Id; 3]),
) -> Result<(), BuildError> {
    let kit = io.kit;
    let texel_type = kit.uvec(4);
    // Texel (1, 1)'s centre, and as a projective coordinate of divisor two.
    let centre = io.floats(f, &[0.375, 0.375])?;
    let projected = io.floats(f, &[0.75, 0.75, 2.0])?;
    let (level, lod) = (io.int(f, 0)?, io.float(f, 0.0)?);
    let (lod_operand, lod_value) = (Arg::Word(ImageOperands::LOD.0), Arg::Id(lod));
    let combined = f.op(Op::LOAD, types.combined, &[combined_at])?;
    let first = f.module().uint(0)?;
    op(
        f,
        io,
        exact("ImageGather"),
        (Op::IMAGE_GATHER, texel_type, &[combined, centre, first]),
    )?;
    let image = f.op(Op::IMAGE, types.integer, &[combined])?;
    let texel = io.ints(f, &[2, 1])?;
    let fetched = f.op_args(
        Op::IMAGE_FETCH,
        texel_type,
        &[Arg::Id(image), Arg::Id(texel), lod_operand, Arg::Id(level)],
    )?;
    io.out(f, exact("Image"), (fetched, texel_type))?;
    op(
        f,
        io,
        exact("ImageQueryLevels"),
        (Op::IMAGE_QUERY_LEVELS, kit.uint, &[image]),
    )?;
    op(
        f,
        io,
        exact("ImageQuerySizeLod"),
        (Op::IMAGE_QUERY_SIZE_LOD, kit.uvec(2), &[image, level]),
    )?;
    for (case, code, at) in [
        (
            "ImageSampleExplicitLod",
            Op::IMAGE_SAMPLE_EXPLICIT_LOD,
            centre,
        ),
        (
            "ImageSampleProjExplicitLod",
            Op::IMAGE_SAMPLE_PROJ_EXPLICIT_LOD,
            projected,
        ),
    ] {
        let args = [Arg::Id(combined), Arg::Id(at), lod_operand, lod_value];
        let sampled = f.op_args(code, texel_type, &args)?;
        io.out(f, exact(case), (sampled, texel_type))?;
    }
    let depth = f.op(Op::LOAD, types.depth, &[depth_at])?;
    let comparing = f.op(Op::LOAD, types.comparing, &[comparing_at])?;
    let compared = f.op(Op::SAMPLED_IMAGE, types.depth_sampled, &[depth, comparing])?;
    // A projective comparison divides its reference by the divisor too.
    let (reference, projected_reference) = (io.float(f, 0.4)?, io.float(f, 0.8)?);
    op(
        f,
        io,
        exact("ImageDrefGather"),
        (
            Op::IMAGE_DREF_GATHER,
            kit.vec(4),
            &[compared, centre, reference],
        ),
    )?;
    for (case, code, (at, against)) in [
        (
            "ImageSampleDrefExplicitLod",
            Op::IMAGE_SAMPLE_DREF_EXPLICIT_LOD,
            (centre, reference),
        ),
        (
            "ImageSampleProjDrefExplicitLod",
            Op::IMAGE_SAMPLE_PROJ_DREF_EXPLICIT_LOD,
            (projected, projected_reference),
        ),
    ] {
        let args = [
            Arg::Id(compared),
            Arg::Id(at),
            Arg::Id(against),
            lod_operand,
            lod_value,
        ];
        let passed = f.op_args(code, kit.float, &args)?;
        io.out(f, exact(case), (passed, kit.float))?;
    }
    Ok(())
}

/// The storage image of `ty` at `variable`: a texel read, one written and
/// read back by the same invocation, its size, and an atomic through a texel
/// pointer and the word it left.
fn stored(f: &mut Function<'_>, io: &mut Io, (ty, variable): (Id, Id)) -> Result<(), BuildError> {
    let kit = io.kit;
    let texel_type = kit.uvec(4);
    let storage = f.op(Op::LOAD, ty, &[variable])?;
    let read_at = io.ints(f, &[1, 2])?;
    op(
        f,
        io,
        exact("ImageRead"),
        (Op::IMAGE_READ, texel_type, &[storage, read_at]),
    )?;
    let (written_at, value) = (io.ints(f, &[0, 3])?, io.uints(f, &[77, 0, 0, 0])?);
    f.emit(Op::IMAGE_WRITE, &[storage, written_at, value])?;
    op(
        f,
        io,
        exact("ImageWrite"),
        (Op::IMAGE_READ, texel_type, &[storage, written_at]),
    )?;
    op(
        f,
        io,
        exact("ImageQuerySize"),
        (Op::IMAGE_QUERY_SIZE, kit.uvec(2), &[storage]),
    )?;
    let word = f.module().pointer(StorageClass::IMAGE, kit.uint)?;
    let (atomic_at, sample) = (io.ints(f, &[3, 0])?, f.module().uint(0)?);
    let pointer = f.op(
        Op::IMAGE_TEXEL_POINTER,
        word,
        &[variable, atomic_at, sample],
    )?;
    let device = f.module().uint(Scope::DEVICE.0)?;
    let relaxed = f.module().uint(MemorySemantics::RELAXED.0)?;
    let added = io.word(f, 6)?;
    op(
        f,
        io,
        exact("ImageTexelPointer"),
        (
            Op::ATOMIC_I_ADD,
            kit.uint,
            &[pointer, device, relaxed, added],
        ),
    )?;
    op(
        f,
        io,
        exact("ImageTexelPointer"),
        (Op::ATOMIC_LOAD, kit.uint, &[pointer, device, relaxed]),
    )
}

/// The family's kernel `module` dispatched on `device` once over its buffer
/// starting as `start`: the words it left; `None` where the device does not
/// state the uses the family makes of its formats.
pub(super) fn dispatch(
    device: &Device,
    module: Admitted,
    start: &[u8],
) -> Result<Option<Vec<u32>>, Error> {
    if !states_uses(device, &USES) {
        return Ok(None);
    }
    let near = near(device)?;
    let (transfer, compute) = (
        family(device, QueueCapabilities::TRANSFER)?,
        family(device, QueueCapabilities::COMPUTE)?,
    );
    let context = device.create_context(&ContextDesc::default())?;
    let sampled = Sampled::new(&context, (near, transfer))?;
    let (_storage_memory, storage) = placed(
        &context,
        &square(
            Format::R32Uint,
            (1, 1),
            ImageUsage::STORAGE.with(ImageUsage::TRANSFER_DST),
        ),
    )?;
    let bytes = storage_start()?;
    let size = u64::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
    let (staging_memory, staging) = buffer(&context, (near, size), BufferUsage::TRANSFER_SRC)?;
    staging_memory.write(0, &bytes)?;
    let mut filling = context.command_buffer();
    filling.copy_buffer_to_image(
        &staging,
        &storage,
        &[packed(0, (Aspects::COLOR, 0, 1), (SIDE, 4))],
    )?;
    submit(&context, transfer, &filling)?;
    let (_multisampled_memory, multisampled) = placed(
        &context,
        &ImageDesc {
            samples: SAMPLES,
            ..square(
                Format::Rgba8Unorm,
                (1, 1),
                ImageUsage::SAMPLED.with(ImageUsage::COLOR_ATTACHMENT),
            )
        },
    )?;
    let storage_view = whole_view(&context, &storage, (Format::R32Uint, Aspects::COLOR))?;
    let multisampled_view = whole_view(
        &context,
        &multisampled,
        (Format::Rgba8Unorm, Aspects::COLOR),
    )?;
    let [first, second, third] = sampled_bindings();
    let bindings = layout(
        &module,
        (Stage::Compute, Stages::COMPUTE),
        &[first, second, third, (MULTISAMPLED_AT, MULTISAMPLED)],
    )?;
    let set_layout = context.create_bind_set_layout(&bindings)?;
    let pipeline_layout = context.create_pipeline_layout(&[&set_layout], PushConstants::NONE)?;
    let shader = context.create_shader_module(module)?;
    let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
        module: &shader,
        entry_point: "main",
        layout: &pipeline_layout,
        specialization: &[],
    })?;
    through_buffer(&context, near, start, |words, size| {
        let [combined, depth, comparing] = sampled.resources();
        let resources = [
            Resource::Buffer {
                buffer: words,
                offset: 0,
                size,
            },
            combined,
            depth,
            comparing,
            Resource::Image(&storage_view),
            Resource::Image(&multisampled_view),
        ];
        let entries = in_binding_order(&resources);
        let set = context.create_bind_set(&set_layout, &entries)?;
        let mut commands = context.command_buffer();
        commands.dispatch(&pipeline, &[&set], &[], [1, 1, 1])?;
        submit(&context, compute, &commands)
    })
    .map(Some)
}
