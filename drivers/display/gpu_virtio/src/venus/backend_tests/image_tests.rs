//! Images, views, samplers and texel buffers through the Venus backend on
//! the test's renderer: each made as the host's valid usage allows, an image
//! reading zero once placed, and copies, clears and dispatches leaving what
//! the software device leaves.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_gpu::{
    AddressMode, AllocationDesc, Aspects, BindSetEntry, BufferImageCopy, BufferUsage,
    BufferViewDesc, ClearValue, CompareOp, ComputePipelineDesc, ContextDesc, DescriptorType,
    Device, Error, Extent3d, Features, Format, FormatFeatures, ImageCopy, ImageDesc,
    ImageDimension, ImageRange, ImageSubresource, ImageUsage, ImageViewDesc, LayoutBinding, Loss,
    MemoryProperties, NoThreads, Origin3d, PushConstants, Resource, SampleType, SamplerDesc,
    SamplerType, SoftwareDesc, Submission, ViewDimension, DEFAULT_SUBGROUP_SIZE,
};
use tairix_spirv::assemble::admit;
use tairix_spirv::{Admitted, BindingKind, Stage, Stages};

use super::host_images::{image_objects, HostImage};
use super::{context, let_go, Misbehaviour, Object, Rig, SECOND};
use crate::venus::info::host_format;
use crate::venus::testing::{drawn, Still, DRAW_FEATURES};
use crate::venus::{api_version, image, queue, Structure};

/// The uses the backend states of a format through images and texel
/// buffers, a pass's aside.
const SERVED: FormatFeatures = FormatFeatures::SAMPLED
    .with(FormatFeatures::FILTERED)
    .with(FormatFeatures::STORAGE)
    .with(FormatFeatures::STORAGE_READ_WITHOUT_FORMAT)
    .with(FormatFeatures::STORAGE_WRITE_WITHOUT_FORMAT)
    .with(TEXEL_USES);

/// A texel buffer's uses, each through a binding naming its format or not.
const TEXEL_USES: FormatFeatures = FormatFeatures::UNIFORM_TEXEL_BUFFER
    .with(FormatFeatures::STORAGE_TEXEL_BUFFER)
    .with(FormatFeatures::TEXEL_BUFFER_READ_WITHOUT_FORMAT)
    .with(FormatFeatures::TEXEL_BUFFER_WRITE_WITHOUT_FORMAT);

const fn extent(width: u32, height: u32) -> Extent3d {
    Extent3d {
        width,
        height,
        depth: 1,
    }
}

const fn desc(
    format: Format,
    (size, levels, layers): (Extent3d, u32, u32),
    usage: ImageUsage,
) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: size,
        mip_levels: levels,
        array_layers: layers,
        samples: 1,
        usage,
    }
}

/// A context of `device`, and the first of its memory types the CPU reaches.
fn on(device: &Device) -> (tairix_gpu::Context, u32) {
    let context = device
        .create_context(&ContextDesc::default())
        .expect("a context");
    let visible = (0u32..)
        .zip(&device.info().memory_types)
        .find(|(_, kind)| kind.properties.contains(MemoryProperties::HOST_VISIBLE))
        .map(|(index, _)| index)
        .expect("a host-visible memory type");
    (context, visible)
}

/// An image of `desc` at the start of memory of its own.
fn placed(
    context: &tairix_gpu::Context,
    desc: &ImageDesc,
) -> (tairix_gpu::Allocation, tairix_gpu::Image) {
    let required = context.image_requirements(desc).expect("requirements");
    let memory = context
        .allocate(&AllocationDesc {
            size: required.size,
            memory_type: required.memory_types.trailing_zeros(),
        })
        .expect("memory");
    let image = context.create_image(&memory, 0, desc).expect("an image");
    (memory, image)
}

/// A buffer of `size` bytes made for `usage` in memory of host-visible type
/// `visible`, holding `bytes`.
fn visible(
    (context, visible): (&tairix_gpu::Context, u32),
    (size, usage): (u64, BufferUsage),
    bytes: &[u8],
) -> (tairix_gpu::Allocation, tairix_gpu::Buffer) {
    let memory = context
        .allocate(&AllocationDesc {
            size,
            memory_type: visible,
        })
        .expect("memory");
    memory.write(0, bytes).expect("written");
    let buffer = context
        .create_buffer(
            &memory,
            &tairix_gpu::BufferDesc {
                offset: 0,
                size,
                usage,
            },
        )
        .expect("a buffer");
    (memory, buffer)
}

fn run(context: &tairix_gpu::Context, commands: &tairix_gpu::CommandBuffer) {
    context
        .queue(0, 0)
        .expect("a queue")
        .submit(&Submission {
            command_buffers: &[commands],
            waits: &[],
            signals: &[],
            deadline: SECOND,
        })
        .map_err(|refused| refused.error)
        .expect("submitted")
        .wait(SECOND)
        .expect("ran");
}

/// A copy of `extent` texels of `aspect` of `layers` layers of `level`,
/// tightly packed at `offset` with texels of `bytes` bytes.
fn packed(
    offset: u64,
    (aspect, level, layers): (Aspects, u32, u32),
    (size, bytes): (Extent3d, u32),
) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset: offset,
        bytes_per_row: size.width * bytes,
        rows_per_image: size.height,
        image: ImageSubresource {
            aspect,
            level,
            base_layer: 0,
            layers,
        },
        origin: Origin3d::default(),
        extent: size,
    }
}

/// The bytes `regions` read of `image` into a buffer of `size` bytes.
fn read_back(
    (context, visible_type): (&tairix_gpu::Context, u32),
    image: &tairix_gpu::Image,
    regions: &[BufferImageCopy],
    size: u64,
) -> Vec<u8> {
    let (memory, buffer) = visible(
        (context, visible_type),
        (size, BufferUsage::TRANSFER_DST),
        &[],
    );
    let mut commands = context.command_buffer();
    commands
        .copy_image_to_buffer(image, &buffer, regions)
        .expect("recorded");
    run(context, &commands);
    let mut seen = vec![0; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    seen
}

/// The software device a Venus device's work is held to.
pub(super) fn software() -> Device {
    Device::software(&SoftwareDesc {
        memory_bytes: 64 << 20,
        clock: Arc::new(Still),
        runner: Arc::new(tairix_parallel::Serial),
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("the software device")
}

/// What `pick` takes of every object the renderer holds, across every
/// context, copied out so a failed assertion never holds the host's lock.
fn held<T>(rig: &Rig, pick: impl Fn(&Object) -> Option<T>) -> Vec<T> {
    let state = rig.host.state.lock().expect("unpoisoned");
    state
        .contexts
        .iter()
        .flat_map(|(_, host)| host.objects.values())
        .filter_map(pick)
        .collect()
}

/// The images the renderer holds.
fn host_images(rig: &Rig) -> Vec<HostImage> {
    held(rig, |object| match object {
        Object::Image(image) => Some(image.clone()),
        _ => None,
    })
}

/// What of `served` the backend states of `format`'s own uses: both texel
/// uses, as a texel buffer's one buffer is made for both, or neither.
fn both_texel_uses(format: Format, served: FormatFeatures) -> FormatFeatures {
    let texels = FormatFeatures::UNIFORM_TEXEL_BUFFER.with(FormatFeatures::STORAGE_TEXEL_BUFFER);
    let offered = FormatFeatures::from_bits(format.features().bits() & served.bits());
    if offered.contains(texels) {
        offered
    } else {
        offered.without(TEXEL_USES)
    }
}

#[test]
fn the_device_states_what_the_host_serves_of_each_format_and_its_sampler_features() {
    let rig = Rig::new();
    let device = rig.device();
    let info = device.info();
    for format in Format::ALL {
        assert_eq!(
            info.formats.get(format),
            both_texel_uses(format, SERVED.with(drawn(format))),
            "{format:?}: its encoding's uses the host offers, both texel uses or neither, a depth \
             of 24 bits through its stand-in"
        );
    }
    assert_eq!(
        info.features,
        Features::SAMPLER_ANISOTROPY
            .with(Features::MIRROR_CLAMP_TO_EDGE)
            .with(DRAW_FEATURES)
    );
    let made = context(&device);
    {
        let state = rig.host.state.lock().expect("unpoisoned");
        let enabled = state.enabled.as_ref().expect("a device made");
        assert!(enabled
            .extensions
            .iter()
            .any(|name| name == crate::venus::extension::SAMPLER_MIRROR_CLAMP_TO_EDGE));
        let (_, core) = enabled.features.first().expect("the core features");
        assert!(core[19], "anisotropic filtering enabled");
        let robust = enabled
            .features
            .iter()
            .find(|(raw, _)| *raw == Structure::PhysicalDeviceRobustness2Features as i32)
            .map(|(_, members)| members.clone());
        assert_eq!(robust, Some(vec![true, true, false]));
    }
    drop((made, device));
    let_go(&rig.finish());

    let rig = Rig::new();
    rig.offer(|offered| {
        offered
            .withheld
            .push((Structure::PhysicalDeviceRobustness2Features, 1));
    });
    let device = rig.device();
    for format in Format::ALL {
        assert_eq!(
            device.info().formats.get(format),
            both_texel_uses(format, TEXEL_USES.with(drawn(format))),
            "{format:?}: no image a shader reads past its edges, a pass's uses aside"
        );
    }
    drop(device);
    let_go(&rig.finish());
}

#[test]
fn a_host_is_asked_its_formats_as_it_states_them() {
    for (minor, extended) in [(3, false), (2, true), (2, false)] {
        let rig = Rig::new();
        rig.offer(|offered| {
            offered.api_version = api_version(minor);
            if !extended {
                offered
                    .extensions
                    .retain(|name| name != crate::venus::extension::FORMAT_FEATURE_FLAGS2);
            }
        });
        // The host refuses Vulkan 1.3's format features asked of a host not
        // stating them, and an older host states none of their bits, nor
        // draws.
        let device = rig.device();
        for format in Format::ALL {
            let passes = if minor >= 3 {
                drawn(format)
            } else {
                FormatFeatures::NONE
            };
            assert_eq!(
                device.info().formats.get(format),
                both_texel_uses(format, SERVED.with(passes)),
                "{format:?}, Vulkan 1.{minor}, the extension offered: {extended}"
            );
        }
        let made = context(&device);
        {
            let state = rig.host.state.lock().expect("unpoisoned");
            let enabled = state.enabled.as_ref().expect("a device made");
            assert_eq!(
                enabled
                    .extensions
                    .iter()
                    .any(|name| name == crate::venus::extension::FORMAT_FEATURE_FLAGS2),
                minor < 3 && extended,
                "Vulkan 1.{minor}"
            );
        }
        drop((made, device));
        let_go(&rig.finish());
    }
}

#[test]
fn an_image_is_made_as_its_description_asks_and_reads_zero_whatever_its_memory_held() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let size = extent(8, 4);
    let made = desc(
        Format::Rgba8Unorm,
        (size, 2, 3),
        ImageUsage::TRANSFER_SRC.with(ImageUsage::SAMPLED),
    );
    let required = context.image_requirements(&made).expect("requirements");
    assert_eq!(
        (required.alignment, required.memory_types),
        (512, 0b11),
        "the host's, of the stated types"
    );
    let memory = context
        .allocate(&AllocationDesc {
            size: required.alignment + required.size,
            memory_type: 1,
        })
        .expect("memory");
    let len = usize::try_from(required.alignment + required.size).expect("small");
    memory.write(0, &vec![0xee; len]).expect("written");
    let image = context
        .create_image(&memory, required.alignment, &made)
        .expect("an image");
    let held = host_images(&rig);
    let [host] = held.as_slice() else {
        panic!("one image on the host: {held:?}");
    };
    assert_eq!(
        (
            host.kind,
            host.format,
            host.extent,
            host.levels,
            host.layers
        ),
        (1, host_format(Format::Rgba8Unorm), [8, 4, 1], 2, 3)
    );
    assert_eq!(
        host.usage,
        image::TRANSFER_SRC | image::TRANSFER_DST | image::SAMPLED,
        "its uses, and the clear it is placed with"
    );
    assert_eq!(
        host.flags,
        image::MUTABLE_FORMAT,
        "seen as its sRGB twin, and as no cube with too few layers"
    );
    assert_eq!(
        host.view_formats,
        [
            host_format(Format::Rgba8Unorm),
            host_format(Format::Rgba8UnormSrgb)
        ]
    );
    assert!(host.general);
    assert_eq!(host.bound.map(|(_, offset)| offset), Some(512));
    let regions = [
        packed(0, (Aspects::COLOR, 0, 3), (size, 4)),
        packed(384, (Aspects::COLOR, 1, 3), (extent(4, 2), 4)),
    ];
    let seen = read_back((&context, 1), &image, &regions, 480);
    assert!(
        seen.iter().all(|&byte| byte == 0),
        "every level and layer reads zero"
    );
    drop((image, memory, context, device));
    let_go(&rig.finish());
}

#[test]
fn an_image_s_memory_is_asked_with_its_making_where_the_host_answers_it_of_its_description() {
    for (minor, described) in [(3, 2), (2, 0)] {
        let rig = Rig::new();
        rig.offer(|offered| offered.api_version = api_version(minor));
        let device = rig.device();
        let context = context(&device);
        let size = extent(8, 4);
        let made = desc(Format::Rgba8Unorm, (size, 1, 1), ImageUsage::TRANSFER_SRC);
        let required = context.image_requirements(&made).expect("requirements");
        let memory = context
            .allocate(&AllocationDesc {
                size: required.size,
                memory_type: 1,
            })
            .expect("memory");
        let image = context.create_image(&memory, 0, &made).expect("an image");
        let asked = rig.host.state.lock().expect("unpoisoned").described;
        assert_eq!(asked, described, "a host of Vulkan 1.{minor}");
        let region = packed(0, (Aspects::COLOR, 0, 1), (size, 4));
        let seen = read_back((&context, 1), &image, &[region], 128);
        assert!(seen.iter().all(|&byte| byte == 0), "Vulkan 1.{minor}");
        drop((image, memory, context, device));
        let_go(&rig.finish());
    }
}

#[test]
fn a_host_whose_family_runs_no_graphics_work_makes_no_depth_or_stencil_image() {
    let rig = Rig::new();
    rig.offer(|offered| offered.family_flags = queue::COMPUTE | queue::TRANSFER);
    let device = rig.device();
    let formats = &device.info().formats;
    for format in [
        Format::Depth32Float,
        Format::Depth24PlusStencil8,
        Format::Stencil8,
    ] {
        assert_eq!(formats.get(format), FormatFeatures::NONE, "{format:?}");
    }
    assert!(formats
        .get(Format::R32Uint)
        .contains(FormatFeatures::STORAGE));
    let context = context(&device);
    let size = extent(4, 4);
    let depth = desc(Format::Depth32Float, (size, 1, 1), ImageUsage::TRANSFER_SRC);
    assert_eq!(
        context.image_requirements(&depth).err(),
        Some(Error::Unsupported),
        "its placing clear alone would need graphics work"
    );
    let colour = desc(Format::Rgba8Unorm, (size, 1, 1), ImageUsage::TRANSFER_SRC);
    let (memory, image) = placed(&context, &colour);
    assert_eq!(host_images(&rig).len(), 1);
    drop((image, memory, context, device));
    let_go(&rig.finish());
}

#[test]
fn an_integer_clear_writes_what_its_format_holds_of_its_value() {
    let rig = Rig::new();
    let device = rig.device();
    let (context, visible) = on(&device);
    let size = extent(2, 2);
    let usage = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
    let made = desc(Format::Rgba8Uint, (size, 1, 1), usage);
    let (memory, image) = placed(&context, &made);
    let whole = ImageRange {
        aspects: Aspects::COLOR,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    let mut commands = context.command_buffer();
    commands
        .clear_image(&image, ClearValue::Uint([300, 255, 256, 7]), &[whole])
        .expect("recorded");
    run(&context, &commands);
    let region = packed(0, (Aspects::COLOR, 0, 1), (size, 4));
    let seen = read_back((&context, visible), &image, &[region], 16);
    assert_eq!(seen, [44, 255, 0, 7].repeat(4), "each channel's low bits");
    drop((image, memory, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_texel_buffer_of_a_format_the_host_offers_one_texel_use_is_refused_before_the_host_hears_of_it()
{
    let rig = Rig::new();
    let device = rig.device();
    let (context, visible_type) = on(&device);
    let (_memory, buffer) = visible(
        (&context, visible_type),
        (256, BufferUsage::UNIFORM_TEXEL),
        &[],
    );
    let srgb = BufferViewDesc {
        offset: 0,
        size: 64,
        format: Format::Rgba8UnormSrgb,
    };
    assert_eq!(
        context.create_buffer_view(&buffer, &srgb).err(),
        Some(Error::Unsupported),
        "its one buffer is made for both texel uses"
    );
    assert_eq!(context.status(), Ok(()));
    drop((buffer, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_view_the_host_answers_unreadably_loses_its_context() {
    let rig = Rig::new();
    let device = rig.device();
    let (context, visible_type) = on(&device);
    let made = desc(
        Format::Rgba8Unorm,
        (extent(4, 4), 1, 1),
        ImageUsage::SAMPLED,
    );
    let (memory, image) = placed(&context, &made);
    let (_filled_memory, filled) = visible(
        (&context, visible_type),
        (64, BufferUsage::TRANSFER_DST),
        &[],
    );
    let whole = ImageViewDesc {
        format: Format::Rgba8Unorm,
        dimension: ViewDimension::D2,
        range: ImageRange {
            aspects: Aspects::COLOR,
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        },
    };
    rig.misbehave(Misbehaviour::GarbleViews);
    assert!(context.create_image_view(&image, &whole).is_err());
    rig.misbehave(Misbehaviour::Nothing);
    let sent = rig.host.state.lock().expect("unpoisoned").submits;
    let mut commands = context.command_buffer();
    commands.fill_buffer(&filled, 0, 64, 1).expect("recorded");
    assert_eq!(
        super::submit(&context, &commands).err(),
        Some(Error::Lost(Loss::Fault)),
        "what else the host made is unknown, so nothing more is sent"
    );
    let now = rig.host.state.lock().expect("unpoisoned").submits;
    assert_eq!(now, sent);
    drop((commands, image, memory, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_bordered_sampler_the_host_makes_half_of_lets_that_half_go() {
    let rig = Rig::new();
    let device = rig.device();
    let (context, _) = on(&device);
    let border = SamplerDesc {
        address: [AddressMode::ClampToBorder; 3],
        ..SamplerDesc::LINEAR
    };
    rig.misbehave(Misbehaviour::RefuseIntegerBorders);
    assert_eq!(
        context.create_sampler(&border).err(),
        Some(Error::OutOfMemory)
    );
    rig.misbehave(Misbehaviour::Nothing);
    rig.wait_for(|state| {
        state
            .contexts
            .iter()
            .flat_map(|(_, held)| held.objects.values())
            .all(|object| !matches!(object, Object::Sampler(_)))
    });
    assert!(
        context.create_sampler(&border).is_ok(),
        "the context goes on"
    );
    drop((context, device));
    let_go(&rig.finish());
}

/// A layer of a colour image's first level.
const fn colour_layer(layer: u32) -> ImageSubresource {
    ImageSubresource {
        aspect: Aspects::COLOR,
        level: 0,
        base_layer: layer,
        layers: 1,
    }
}

/// Copy part of `staging` into a corner of `a`'s second layer and the whole
/// of its second level, then part of that layer into `b`'s first.
fn copy_around(
    commands: &mut tairix_gpu::CommandBuffer,
    staging: &tairix_gpu::Buffer,
    (a, b): (&tairix_gpu::Image, &tairix_gpu::Image),
) {
    let into_corner = BufferImageCopy {
        buffer_offset: 8,
        bytes_per_row: 16,
        rows_per_image: 3,
        image: colour_layer(1),
        origin: Origin3d { x: 1, y: 1, z: 0 },
        extent: extent(2, 3),
    };
    let whole_level_one = packed(64, (Aspects::COLOR, 1, 2), (extent(2, 2), 4));
    commands
        .copy_buffer_to_image(staging, a, &[into_corner, whole_level_one])
        .expect("recorded");
    let between = ImageCopy {
        src: colour_layer(1),
        src_origin: Origin3d { x: 1, y: 0, z: 0 },
        dst: colour_layer(0),
        dst_origin: Origin3d { x: 0, y: 2, z: 0 },
        extent: extent(3, 2),
    };
    commands.copy_image(a, b, &[between]).expect("recorded");
}

/// Clear `b`'s second level, both of `d`'s planes in its first layer, and
/// only the stencil of its second.
fn clear_around(
    commands: &mut tairix_gpu::CommandBuffer,
    (b, d): (&tairix_gpu::Image, &tairix_gpu::Image),
) {
    let level_one = ImageRange {
        aspects: Aspects::COLOR,
        base_level: 1,
        levels: 1,
        base_layer: 0,
        layers: 2,
    };
    commands
        .clear_image(b, ClearValue::Float([1.0, 0.5, 0.0, 0.25]), &[level_one])
        .expect("recorded");
    let first_layer = ImageRange {
        aspects: Aspects::DEPTH_STENCIL,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    let second_stencil = ImageRange {
        aspects: Aspects::STENCIL,
        base_layer: 1,
        ..first_layer
    };
    for (range, depth, stencil) in [(first_layer, 0.75, 9), (second_stencil, 0.25, 3)] {
        commands
            .clear_image(d, ClearValue::DepthStencil { depth, stencil }, &[range])
            .expect("recorded");
    }
}

/// Copies between buffers and images, between images, and clears, on
/// `device`: everything they leave, read back.
fn transfers(device: &Device) -> Vec<u8> {
    let (context, visible_type) = on(device);
    let at = (&context, visible_type);
    let size = extent(4, 4);
    let usage = ImageUsage::TRANSFER_SRC
        .with(ImageUsage::TRANSFER_DST)
        .with(ImageUsage::SAMPLED);
    let colour = desc(Format::Rgba8Unorm, (size, 2, 2), usage);
    let (_a_memory, a) = placed(&context, &colour);
    let (_b_memory, b) = placed(&context, &colour);
    let depth = desc(Format::Depth32FloatStencil8, (size, 1, 2), usage);
    let (_d_memory, d) = placed(&context, &depth);
    let pattern: Vec<u8> = (0u32..512)
        .map(|at| u8::try_from(at * 7 % 251).expect("a byte"))
        .collect();
    let (_staging_memory, staging) = visible(at, (512, BufferUsage::TRANSFER_SRC), &pattern);
    let mut commands = context.command_buffer();
    copy_around(&mut commands, &staging, (&a, &b));
    clear_around(&mut commands, (&b, &d));
    run(&context, &commands);
    let colour_regions = [
        packed(0, (Aspects::COLOR, 0, 2), (size, 4)),
        packed(128, (Aspects::COLOR, 1, 2), (extent(2, 2), 4)),
    ];
    let depth_regions = [
        packed(0, (Aspects::DEPTH, 0, 2), (size, 4)),
        packed(128, (Aspects::STENCIL, 0, 2), (size, 1)),
    ];
    let mut seen = read_back(at, &a, &colour_regions, 160);
    seen.extend(read_back(at, &b, &colour_regions, 160));
    seen.extend(read_back(at, &d, &depth_regions, 160));
    seen
}

#[test]
fn copies_and_clears_leave_what_the_software_device_leaves() {
    let rig = Rig::new();
    let device = rig.device();
    let seen = transfers(&device);
    assert_eq!(seen, transfers(&software()));
    let b_level_one = &seen[160 + 128..160 + 160];
    assert!(
        b_level_one
            .chunks(4)
            .all(|texel| texel == [255, 128, 0, 64]),
        "the clear, as unsigned normalised bytes: {b_level_one:?}"
    );
    let stencil = &seen[320 + 128..320 + 160];
    assert_eq!(stencil, [[9u8; 16], [3u8; 16]].concat().as_slice());
    drop(device);
    rig.wait_for(|state| image_objects(state) == 0);
    let_go(&rig.finish());
}

#[test]
fn a_depth_of_24_bits_is_made_as_the_host_s_stand_in_and_its_stencil_read_back() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let made = desc(
        Format::Depth24PlusStencil8,
        (extent(4, 2), 1, 1),
        ImageUsage::TRANSFER_SRC
            .with(ImageUsage::TRANSFER_DST)
            .with(ImageUsage::SAMPLED),
    );
    let (memory, image) = placed(&context, &made);
    let held = host_images(&rig);
    assert_eq!(
        held.iter().map(|image| image.format).collect::<Vec<_>>(),
        [host_format(Format::Depth32FloatStencil8)],
        "the host renders no depth of 24 bits"
    );
    let mut commands = context.command_buffer();
    let whole = ImageRange {
        aspects: Aspects::DEPTH_STENCIL,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    commands
        .clear_image(
            &image,
            ClearValue::DepthStencil {
                depth: 0.5,
                stencil: 7,
            },
            &[whole],
        )
        .expect("recorded");
    run(&context, &commands);
    let stencil = packed(0, (Aspects::STENCIL, 0, 1), (extent(4, 2), 1));
    assert_eq!(read_back((&context, 1), &image, &[stencil], 8), [7; 8]);
    drop((image, memory, context, device));
    let_go(&rig.finish());
}

#[test]
fn an_image_past_what_the_host_makes_is_refused_and_leaves_nothing_behind() {
    let rig = Rig::new();
    rig.offer(|offered| offered.image_extent = 64);
    let device = rig.device();
    let context = context(&device);
    let usage = ImageUsage::TRANSFER_SRC.with(ImageUsage::SAMPLED);
    let wide = desc(Format::Rgba8Unorm, (extent(128, 8), 1, 1), usage);
    assert_eq!(
        context.image_requirements(&wide).err(),
        Some(Error::Unsupported)
    );
    let memory = context
        .allocate(&AllocationDesc {
            size: 1 << 20,
            memory_type: 0,
        })
        .expect("memory");
    assert!(matches!(
        context.create_image(&memory, 0, &wide),
        Err(Error::Unsupported)
    ));
    rig.wait_for(|state| image_objects(state) == 0);
    let narrow = desc(Format::Rgba8Unorm, (extent(64, 8), 1, 1), usage);
    let image = context
        .create_image(&memory, 0, &narrow)
        .expect("within what the host makes");
    drop((image, memory));
    rig.wait_for(|state| image_objects(state) == 0);
    drop((context, device));
    let_go(&rig.finish());
}

#[test]
fn an_image_the_host_makes_no_cube_of_is_made_seen_as_anything_but_one() {
    let faces = |dimension| ImageViewDesc {
        format: Format::Rgba8Unorm,
        dimension,
        range: ImageRange {
            aspects: Aspects::COLOR,
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 6,
        },
    };
    let six = desc(
        Format::Rgba8Unorm,
        (extent(8, 8), 1, 6),
        ImageUsage::SAMPLED,
    );
    for uncubed in [false, true] {
        let rig = Rig::new();
        if uncubed {
            rig.offer(|offered| offered.uncubed.push(host_format(Format::Rgba8Unorm)));
        }
        let device = rig.device();
        let context = context(&device);
        let (memory, image) = placed(&context, &six);
        let flags = host_images(&rig).first().map(|made| made.flags);
        assert_eq!(
            flags.map(|flags| flags & crate::venus::image::CUBE_COMPATIBLE != 0),
            Some(!uncubed),
            "made able to be seen as a cube where the host makes one"
        );
        let cube = context.create_image_view(&image, &faces(ViewDimension::Cube));
        if uncubed {
            assert_eq!(cube.as_ref().err(), Some(&Error::Unsupported));
        } else {
            assert!(cube.is_ok(), "a cube of its faces");
        }
        let layers = context
            .create_image_view(&image, &faces(ViewDimension::D2Array))
            .expect("its layers, either way");
        drop((layers, cube, image, memory, context, device));
        let_go(&rig.finish());
    }
}

#[test]
fn only_a_square_of_one_sample_with_six_layers_is_made_able_to_be_a_cube() {
    use super::super::images::cubed;
    let six = desc(
        Format::Rgba8Unorm,
        (extent(8, 8), 1, 6),
        ImageUsage::SAMPLED,
    );
    assert!(cubed(&six));
    let samples = ImageDesc { samples: 4, ..six };
    assert!(!cubed(&samples), "no multisampled image is a cube's");
    assert!(!cubed(&ImageDesc {
        extent: extent(8, 4),
        ..six
    }));
    assert!(!cubed(&ImageDesc {
        array_layers: 5,
        ..six
    }));
    assert!(!cubed(&ImageDesc {
        dimension: ImageDimension::D3,
        extent: Extent3d {
            depth: 1,
            ..six.extent
        },
        ..six
    }));
}

#[test]
fn views_samplers_and_texel_buffers_are_made_as_the_host_allows_and_let_go() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let usage = ImageUsage::SAMPLED.with(ImageUsage::STORAGE);
    let (memory, image) = placed(
        &context,
        &desc(Format::Rgba8Unorm, (extent(8, 8), 1, 6), usage),
    );
    let twin = context
        .create_image_view(
            &image,
            &ImageViewDesc {
                format: Format::Rgba8UnormSrgb,
                dimension: ViewDimension::Cube,
                range: ImageRange {
                    aspects: Aspects::COLOR,
                    base_level: 0,
                    levels: 1,
                    base_layer: 0,
                    layers: 6,
                },
            },
        )
        .expect("a cube of its sRGB twin");
    let border = SamplerDesc {
        address: [AddressMode::ClampToBorder; 3],
        ..SamplerDesc::LINEAR
    };
    let bordered = context.create_sampler(&border).expect("a sampler");
    let anisotropic = SamplerDesc {
        max_anisotropy: 8,
        address: [AddressMode::MirrorClampToEdge; 3],
        ..SamplerDesc::LINEAR
    };
    let filtered = context.create_sampler(&anisotropic).expect("a sampler");
    let (_texel_memory, texel_buffer) =
        visible((&context, 1), (256, BufferUsage::STORAGE_TEXEL), &[]);
    let texels = context
        .create_buffer_view(
            &texel_buffer,
            &BufferViewDesc {
                offset: 64,
                size: 128,
                format: Format::R32Uint,
            },
        )
        .expect("a texel buffer");
    let views = held(&rig, |object| match object {
        Object::View(view) => Some(*view),
        _ => None,
    });
    let [view] = views.as_slice() else {
        panic!("one view: {views:?}");
    };
    assert_eq!(
        (view.kind, view.format, view.usage),
        (3, host_format(Format::Rgba8UnormSrgb), image::SAMPLED),
        "made only for the uses its format offers"
    );
    let samplers = held(&rig, |object| match object {
        Object::Sampler(sampler) => Some(*sampler),
        _ => None,
    });
    let borders: Vec<i32> = samplers.iter().map(|sampler| sampler.border).collect();
    assert_eq!(
        borders,
        [0, 1, 0],
        "a border read as floats and as integers, and one sampler where none is read"
    );
    assert!(samplers
        .iter()
        .any(|sampler| sampler.anisotropy == Some(8.0) && sampler.address == [4; 3]));
    let made = held(&rig, |object| match object {
        Object::Texels(texels) => Some(*texels),
        _ => None,
    });
    let [texels_made] = made.as_slice() else {
        panic!("one texel buffer: {made:?}");
    };
    assert_eq!(
        (
            texels_made.format,
            texels_made.offset % 64,
            texels_made.range
        ),
        (host_format(Format::R32Uint), 0, 128),
        "at the buffer's start and the view's"
    );
    drop((
        twin,
        bordered,
        filtered,
        texels,
        texel_buffer,
        image,
        memory,
    ));
    rig.wait_for(|state| image_objects(state) == 0);
    drop((context, device));
    let_go(&rig.finish());
}

/// Fetch a uniform texel buffer, write a storage texel buffer, and mark a
/// storage image, each invocation of a 4 × 4 grid at its own texel.
const STORED: &str = r#"
OpCapability Shader
OpCapability SampledBuffer
OpCapability ImageBuffer
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %fetched %stored %surface
OpExecutionMode %main LocalSize 4 4 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %fetched DescriptorSet 0
OpDecorate %fetched Binding 0
OpDecorate %stored DescriptorSet 0
OpDecorate %stored Binding 1
OpDecorate %surface DescriptorSet 0
OpDecorate %surface Binding 2
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%v2int = OpTypeVector %int 2
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%fetched_t = OpTypeImage %uint Buffer 0 0 0 1 Unknown
%fetched_ptr = OpTypePointer UniformConstant %fetched_t
%fetched = OpVariable %fetched_ptr UniformConstant
%stored_t = OpTypeImage %uint Buffer 0 0 0 2 R32ui
%stored_ptr = OpTypePointer UniformConstant %stored_t
%stored = OpVariable %stored_ptr UniformConstant
%surface_t = OpTypeImage %uint 2D 0 0 0 2 R32ui
%surface_ptr = OpTypePointer UniformConstant %surface_t
%surface = OpVariable %surface_ptr UniformConstant
%zero = OpConstant %uint 0
%four = OpConstant %uint 4
%hundred = OpConstant %uint 100
%thousand = OpConstant %uint 1000
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%x = OpCompositeExtract %uint %id 0
%y = OpCompositeExtract %uint %id 1
%row = OpIMul %uint %y %four
%index = OpIAdd %uint %row %x
%at = OpBitcast %int %index
%f = OpLoad %fetched_t %fetched
%texel = OpImageFetch %v4uint %f %at
%value = OpCompositeExtract %uint %texel 0
%plus = OpIAdd %uint %value %hundred
%written = OpCompositeConstruct %v4uint %plus %zero %zero %zero
%s = OpLoad %stored_t %stored
OpImageWrite %s %at %written
%xi = OpBitcast %int %x
%yi = OpBitcast %int %y
%place = OpCompositeConstruct %v2int %xi %yi
%mark = OpIAdd %uint %index %thousand
%marked = OpCompositeConstruct %v4uint %mark %zero %zero %zero
%img = OpLoad %surface_t %surface
OpImageWrite %img %place %marked
OpReturn
OpFunctionEnd
"#;

/// Sample a colour image at each texel's centre through a combined sampler,
/// and compare a depth image's texel with a reference through a separate
/// comparing sampler, writing both for each invocation of a 4 × 4 grid.
const SAMPLED: &str = r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %colour %depth %compare %out
OpExecutionMode %main LocalSize 4 4 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %colour DescriptorSet 0
OpDecorate %colour Binding 0
OpDecorate %depth DescriptorSet 0
OpDecorate %depth Binding 1
OpDecorate %compare DescriptorSet 0
OpDecorate %compare Binding 2
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 3
OpDecorate %texels ArrayStride 16
OpMemberDecorate %block 0 Offset 0
OpDecorate %block Block
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v2float = OpTypeVector %float 2
%v3uint = OpTypeVector %uint 3
%v4float = OpTypeVector %float 4
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%colour_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%colour_si_t = OpTypeSampledImage %colour_t
%colour_ptr = OpTypePointer UniformConstant %colour_si_t
%colour = OpVariable %colour_ptr UniformConstant
%depth_t = OpTypeImage %float 2D 1 0 0 1 Unknown
%depth_ptr = OpTypePointer UniformConstant %depth_t
%depth = OpVariable %depth_ptr UniformConstant
%sampler_t = OpTypeSampler
%sampler_ptr = OpTypePointer UniformConstant %sampler_t
%compare = OpVariable %sampler_ptr UniformConstant
%depth_si_t = OpTypeSampledImage %depth_t
%texels = OpTypeRuntimeArray %v4float
%block = OpTypeStruct %texels
%out_ptr = OpTypePointer StorageBuffer %block
%out = OpVariable %out_ptr StorageBuffer
%texel_ptr = OpTypePointer StorageBuffer %v4float
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%four = OpConstant %uint 4
%fzero = OpConstant %float 0
%half = OpConstant %float 0.5
%quarter = OpConstant %float 0.25
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%x = OpCompositeExtract %uint %id 0
%y = OpCompositeExtract %uint %id 1
%row = OpIMul %uint %y %four
%index = OpIAdd %uint %row %x
%xf = OpConvertUToF %float %x
%yf = OpConvertUToF %float %y
%xc = OpFAdd %float %xf %half
%yc = OpFAdd %float %yf %half
%u = OpFMul %float %xc %quarter
%v = OpFMul %float %yc %quarter
%at = OpCompositeConstruct %v2float %u %v
%c = OpLoad %colour_si_t %colour
%sampled = OpImageSampleExplicitLod %v4float %c %at Lod %fzero
%d = OpLoad %depth_t %depth
%s = OpLoad %sampler_t %compare
%ds = OpSampledImage %depth_si_t %d %s
%dref = OpFMul %float %xf %quarter
%compared = OpImageSampleDrefExplicitLod %float %ds %at %dref Lod %fzero
%second = OpCompositeConstruct %v4float %compared %dref %u %v
%first_at = OpIMul %uint %index %two
%second_at = OpIAdd %uint %first_at %one
%p0 = OpAccessChain %texel_ptr %out %zero %first_at
OpStore %p0 %sampled
%p1 = OpAccessChain %texel_ptr %out %zero %second_at
OpStore %p1 %second
OpReturn
OpFunctionEnd
"#;

/// The pipeline of `text`'s `main` over one set, each binding of the kind
/// `kind_of` gives it, and the set's layout.
fn pipeline(
    context: &tairix_gpu::Context,
    text: &str,
    kind_of: impl Fn(BindingKind) -> DescriptorType,
) -> (tairix_gpu::ComputePipeline, tairix_gpu::BindSetLayout) {
    let admitted: Admitted = admit(text).unwrap_or_else(|refused| panic!("{refused:?}"));
    let entry = admitted
        .entry_point("main", Stage::Compute)
        .expect("an entry point");
    let bindings: Vec<LayoutBinding> = entry
        .bindings()
        .iter()
        .map(|binding| LayoutBinding {
            binding: binding.binding,
            kind: kind_of(binding.kind),
            count: 1,
            access: binding.access,
            stages: Stages::COMPUTE,
        })
        .collect();
    let layout = context
        .create_bind_set_layout(&bindings)
        .expect("a set layout");
    let laid_out = context
        .create_pipeline_layout(&[&layout], PushConstants::NONE)
        .expect("a pipeline layout");
    let module = context.create_shader_module(admitted).expect("a module");
    let made = context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &laid_out,
            specialization: &[],
        })
        .expect("a pipeline");
    (made, layout)
}

/// The kind of a binding of `kind` that samples nothing.
fn unsampled(kind: BindingKind) -> DescriptorType {
    match kind {
        BindingKind::StorageImage(shape) => DescriptorType::StorageImage(shape),
        BindingKind::UniformTexelBuffer(shape) => DescriptorType::UniformTexelBuffer(shape),
        BindingKind::StorageTexelBuffer(shape) => DescriptorType::StorageTexelBuffer(shape),
        BindingKind::UniformBuffer => DescriptorType::UniformBuffer,
        BindingKind::StorageBuffer => DescriptorType::StorageBuffer,
        sampled => panic!("{sampled:?} samples"),
    }
}

/// [`STORED`] run on `device`: what it left in the storage texel buffer and
/// the storage image.
fn stored(device: &Device) -> (Vec<u8>, Vec<u8>) {
    let (context, visible_type) = on(device);
    let at = (&context, visible_type);
    let (pipeline, layout) = pipeline(&context, STORED, unsampled);
    let fetched: Vec<u8> = (0u32..16)
        .flat_map(|at| (at * at + 3).to_le_bytes())
        .collect();
    let (_uniform_memory, uniform) = visible(at, (64, BufferUsage::UNIFORM_TEXEL), &fetched);
    let (stored_memory, storage) = visible(at, (64, BufferUsage::STORAGE_TEXEL), &[]);
    let texels = |buffer: &tairix_gpu::Buffer| {
        context
            .create_buffer_view(
                buffer,
                &BufferViewDesc {
                    offset: 0,
                    size: 64,
                    format: Format::R32Uint,
                },
            )
            .expect("a texel buffer")
    };
    let (fetch, store) = (texels(&uniform), texels(&storage));
    let usage = ImageUsage::STORAGE.with(ImageUsage::TRANSFER_SRC);
    let (_surface_memory, surface) = placed(
        &context,
        &desc(Format::R32Uint, (extent(4, 4), 1, 1), usage),
    );
    let marked = context
        .create_image_view(
            &surface,
            &ImageViewDesc {
                format: Format::R32Uint,
                dimension: ViewDimension::D2,
                range: ImageRange {
                    aspects: Aspects::COLOR,
                    base_level: 0,
                    levels: 1,
                    base_layer: 0,
                    layers: 1,
                },
            },
        )
        .expect("a view");
    let set = context
        .create_bind_set(
            &layout,
            &[
                BindSetEntry {
                    binding: 0,
                    resources: &[Resource::TexelBuffer(&fetch)],
                },
                BindSetEntry {
                    binding: 1,
                    resources: &[Resource::TexelBuffer(&store)],
                },
                BindSetEntry {
                    binding: 2,
                    resources: &[Resource::Image(&marked)],
                },
            ],
        )
        .expect("a set");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&set], &[], [1, 1, 1])
        .expect("recorded");
    run(&context, &commands);
    let mut texels_left = vec![0; 64];
    stored_memory.read(0, &mut texels_left).expect("read");
    let image_left = read_back(
        at,
        &surface,
        &[packed(0, (Aspects::COLOR, 0, 1), (extent(4, 4), 4))],
        64,
    );
    (texels_left, image_left)
}

#[test]
fn a_dispatch_reads_and_writes_texel_buffers_and_storage_images_on_the_host() {
    let rig = Rig::new();
    let device = rig.device();
    let (texels, surface) = stored(&device);
    let words = |bytes: &[u8]| -> Vec<u32> {
        bytes
            .chunks(4)
            .map(|word| u32::from_le_bytes(word.try_into().expect("a word")))
            .collect()
    };
    let expected: Vec<u32> = (0u32..16).map(|at| at * at + 3 + 100).collect();
    assert_eq!(words(&texels), expected, "each texel fetched, then stored");
    let marked: Vec<u32> = (0u32..16).map(|at| at + 1000).collect();
    assert_eq!(words(&surface), marked, "each texel of the image marked");
    assert_eq!((texels, surface), stored(&software()));
    let dispatched = rig.host.state.lock().expect("unpoisoned").dispatched;
    assert_eq!(dispatched, 1);
    drop(device);
    rig.wait_for(|state| image_objects(state) == 0);
    let_go(&rig.finish());
}

/// A colour image whose texels are a pattern, and a depth image of
/// `depth_format` whose depth is a ramp where it is copied and a constant
/// where only a clear sets it.
fn sampled_images(
    at: (&tairix_gpu::Context, u32),
    depth_format: Format,
) -> [(tairix_gpu::Allocation, tairix_gpu::Image); 2] {
    let (context, _) = at;
    let size = extent(4, 4);
    let usage = ImageUsage::SAMPLED.with(ImageUsage::TRANSFER_DST);
    let (colour_memory, colour) = placed(context, &desc(Format::Rgba8Unorm, (size, 1, 1), usage));
    let (depth_memory, depth) = placed(context, &desc(depth_format, (size, 1, 1), usage));
    let texels: Vec<u8> = (0u32..64)
        .map(|at| u8::try_from(at * 37 % 256).expect("a byte"))
        .collect();
    let depths: Vec<u8> = (0u32..16)
        .flat_map(|at| (f32::from(u8::try_from(at % 5).expect("small")) * 0.2).to_le_bytes())
        .collect();
    let (_staging_memory, staging) = visible(
        at,
        (128, BufferUsage::TRANSFER_SRC),
        &[texels, depths].concat(),
    );
    let mut commands = context.command_buffer();
    let into = |offset, aspect| [packed(offset, (aspect, 0, 1), (size, 4))];
    commands
        .copy_buffer_to_image(&staging, &colour, &into(0, Aspects::COLOR))
        .expect("recorded");
    if depth_format.copy_bytes(Aspects::DEPTH).is_some() {
        commands
            .copy_buffer_to_image(&staging, &depth, &into(64, Aspects::DEPTH))
            .expect("recorded");
    } else {
        let whole = ImageRange {
            aspects: depth_format.aspects(),
            base_level: 0,
            levels: 1,
            base_layer: 0,
            layers: 1,
        };
        let value = ClearValue::DepthStencil {
            depth: 0.375,
            stencil: 1,
        };
        commands
            .clear_image(&depth, value, &[whole])
            .expect("recorded");
    }
    run(context, &commands);
    [(colour_memory, colour), (depth_memory, depth)]
}

/// A two-dimensional view of `image` as `format`, seeing `aspects`.
fn flat_view(
    context: &tairix_gpu::Context,
    image: &tairix_gpu::Image,
    (format, aspects): (Format, Aspects),
) -> tairix_gpu::ImageView {
    context
        .create_image_view(
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
        .expect("a view")
}

/// [`SAMPLED`] run on `device` over [`sampled_images`]: what it wrote.
fn sampled(device: &Device, depth_format: Format) -> Vec<u8> {
    let (context, visible_type) = on(device);
    let at = (&context, visible_type);
    let (pipeline, layout) = pipeline(&context, SAMPLED, |kind| match kind {
        BindingKind::CombinedImageSampler(shape) => {
            DescriptorType::CombinedImageSampler(shape, SampleType::Float, SamplerType::Filtering)
        }
        BindingKind::SampledImage(shape) => DescriptorType::SampledImage(shape, SampleType::Depth),
        BindingKind::Sampler => DescriptorType::Sampler(SamplerType::Comparison),
        other => unsampled(other),
    });
    let [(_colour_memory, colour), (_depth_memory, depth)] = sampled_images(at, depth_format);
    let colour_view = flat_view(&context, &colour, (Format::Rgba8Unorm, Aspects::COLOR));
    let depth_view = flat_view(&context, &depth, (depth_format, Aspects::DEPTH));
    let filtering = context
        .create_sampler(&SamplerDesc::LINEAR)
        .expect("a sampler");
    let comparing = context
        .create_sampler(&SamplerDesc {
            compare: Some(CompareOp::LessOrEqual),
            ..SamplerDesc::LINEAR
        })
        .expect("a sampler");
    let (out_memory, out) = visible(at, (512, BufferUsage::STORAGE), &[]);
    let resources = [
        Resource::ImageSampler(&colour_view, &filtering),
        Resource::Image(&depth_view),
        Resource::Sampler(&comparing),
        Resource::Buffer {
            buffer: &out,
            offset: 0,
            size: 512,
        },
    ];
    let entries: Vec<BindSetEntry<'_>> = (0u32..)
        .zip(&resources)
        .map(|(binding, resource)| BindSetEntry {
            binding,
            resources: core::slice::from_ref(resource),
        })
        .collect();
    let set = context.create_bind_set(&layout, &entries).expect("a set");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&set], &[], [1, 1, 1])
        .expect("recorded");
    run(&context, &commands);
    let mut seen = vec![0; 512];
    out_memory.read(0, &mut seen).expect("read");
    seen
}

#[test]
fn a_dispatch_samples_and_compares_images_on_the_host_as_the_software_device_does() {
    for depth in [Format::Depth32Float, Format::Depth24PlusStencil8] {
        let rig = Rig::new();
        let device = rig.device();
        let seen = sampled(&device, depth);
        assert_eq!(seen, sampled(&software(), depth), "{depth:?}");
        let floats: Vec<f32> = seen
            .chunks(4)
            .map(|word| f32::from_le_bytes(word.try_into().expect("a word")))
            .collect();
        let first = &floats[..4];
        assert_eq!(
            first,
            [0.0, 37.0 / 255.0, 74.0 / 255.0, 111.0 / 255.0],
            "{depth:?}: the first texel, sampled at its centre"
        );
        let compared: Vec<f32> = floats.chunks(8).map(|pair| pair[4]).collect();
        assert!(
            compared.contains(&1.0) && compared.contains(&0.0),
            "{depth:?}: comparisons that pass and fail: {compared:?}"
        );
        drop(device);
        rig.wait_for(|state| image_objects(state) == 0);
        let_go(&rig.finish());
    }
}
