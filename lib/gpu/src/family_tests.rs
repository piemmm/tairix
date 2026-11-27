//! What each kind of queue family runs, held on the software device stating
//! a second family beside its own.

use alloc::sync::Arc;

use tairix_parallel::Serial;

use crate::device_tests::{buffer, contents, context, submit, RAM, SECOND};
use crate::image_device_tests::{image, whole};
use crate::{
    Aspects, BufferImageCopy, ClearValue, Context, CopyRegion, Device, Error, Extent3d, Format,
    Granularity, Image, ImageCopy, ImageDesc, ImageDimension, ImageRange, ImageUsage, Origin3d,
    QueueCapabilities, QueueFamily, SubmitError,
};

/// A family running transfers alone, its image copies of `granularity`.
const fn copy_engine(granularity: Granularity) -> QueueFamily {
    QueueFamily {
        capabilities: QueueCapabilities::TRANSFER,
        queues: 1,
        granularity,
    }
}

/// A family that computes and transfers but does not draw.
const COMPUTING: QueueFamily = QueueFamily {
    capabilities: QueueCapabilities::TRANSFER.with(QueueCapabilities::COMPUTE),
    queues: 1,
    granularity: Granularity::TEXEL,
};

/// A family whose engine draws, its draws not stated: transfers reaching
/// depth and stencil, and dispatches.
const DRAWING_ENGINE: QueueFamily = QueueFamily {
    capabilities: QueueCapabilities::TRANSFER
        .with(QueueCapabilities::COMPUTE)
        .with(QueueCapabilities::DEPTH_STENCIL),
    queues: 1,
    granularity: Granularity::TEXEL,
};

/// The software device, its family running everything at 0 and `second` at
/// 1.
fn stating(second: QueueFamily) -> Device {
    crate::testing::software(RAM, Arc::new(Serial), Some(second)).expect("a device")
}

/// Run `commands` on the first queue of family `family`, refused or done.
fn run(context: &Context, family: u32, commands: &crate::CommandBuffer) -> Result<(), SubmitError> {
    submit(&context.queue(family, 0).expect("a queue"), commands)?
        .wait(SECOND)
        .map_err(SubmitError::from)
}

fn refused(error: Error) -> Result<(), SubmitError> {
    Err(SubmitError {
        command: Some(0),
        error,
    })
}

fn desc(format: Format, (width, height): (u32, u32), samples: u32) -> ImageDesc {
    let attachment = if format.aspects() == Aspects::COLOR {
        ImageUsage::COLOR_ATTACHMENT
    } else {
        ImageUsage::DEPTH_STENCIL_ATTACHMENT
    };
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width,
            height,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples,
        usage: ImageUsage::TRANSFER_SRC
            .with(ImageUsage::TRANSFER_DST)
            .with(attachment),
    }
}

const fn every(aspects: Aspects) -> ImageRange {
    ImageRange {
        aspects,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    }
}

/// A copy of the texels at `origin` of `extent`, each `texel` bytes, packed
/// from `buffer_offset`.
const fn texels(
    (buffer_offset, texel): (u64, u32),
    aspect: Aspects,
    (origin, extent): (Origin3d, Extent3d),
) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset,
        bytes_per_row: extent.width * texel,
        rows_per_image: extent.height,
        image: whole(aspect, 1),
        origin,
        extent,
    }
}

const fn at(x: u32, y: u32) -> Origin3d {
    Origin3d { x, y, z: 0 }
}

const fn sized(width: u32, height: u32) -> Extent3d {
    Extent3d {
        width,
        height,
        depth: 1,
    }
}

fn clear(image: &Image, context: &Context, value: ClearValue) -> crate::CommandBuffer {
    let mut commands = context.command_buffer();
    commands
        .clear_image(image, value, &[every(image.desc().format.aspects())])
        .expect("recorded");
    commands
}

#[test]
fn a_copy_engine_copies_fills_and_updates_and_clears_nothing() {
    let device = stating(copy_engine(Granularity::TEXEL));
    let context = context(&device);
    let (memory, held) = buffer(&context, 64);
    let mut moved = context.command_buffer();
    moved
        .update_buffer(&held, 0, &[1, 2, 3, 4])
        .expect("recorded");
    moved
        .fill_buffer(&held, 4, 8, 0x0505_0505)
        .expect("recorded");
    moved
        .copy_buffer(
            &held,
            &held,
            &[CopyRegion {
                src_offset: 0,
                dst_offset: 16,
                size: 12,
            }],
        )
        .expect("recorded");
    assert_eq!(run(&context, 1, &moved), Ok(()));
    let left = contents(&memory);
    assert_eq!(left[..12], [1, 2, 3, 4, 5, 5, 5, 5, 5, 5, 5, 5]);
    assert_eq!(left[16..28], left[..12]);
    let (_colour_memory, colour) = image(&context, &desc(Format::Rgba8Unorm, (4, 4), 1));
    let (_depth_memory, depth) = image(&context, &desc(Format::Depth32Float, (4, 4), 1));
    let colour_clear = clear(&colour, &context, ClearValue::Float([0.5; 4]));
    let depth_clear = clear(
        &depth,
        &context,
        ClearValue::DepthStencil {
            depth: 0.5,
            stencil: 0,
        },
    );
    assert_eq!(run(&context, 1, &colour_clear), refused(Error::Unsupported));
    assert_eq!(run(&context, 1, &depth_clear), refused(Error::Unsupported));
    assert_eq!(run(&context, 0, &colour_clear), Ok(()));
    assert_eq!(run(&context, 0, &depth_clear), Ok(()));
}

#[test]
fn only_a_family_that_draws_reaches_depth_and_stencil_through_buffers_and_clears() {
    let device = stating(COMPUTING);
    let context = context(&device);
    let (_memory, held) = buffer(&context, 256);
    let (_colour_memory, colour) = image(&context, &desc(Format::Rgba8Unorm, (4, 4), 1));
    let (_depth_memory, depth) = image(&context, &desc(Format::Depth32Float, (4, 4), 1));
    assert_eq!(
        run(
            &context,
            1,
            &clear(&colour, &context, ClearValue::Float([1.0; 4]))
        ),
        Ok(()),
        "a family that computes clears colour"
    );
    let depth_clear = clear(
        &depth,
        &context,
        ClearValue::DepthStencil {
            depth: 1.0,
            stencil: 0,
        },
    );
    let region = texels((0, 4), Aspects::DEPTH, (at(0, 0), sized(4, 4)));
    let mut into = context.command_buffer();
    into.copy_buffer_to_image(&held, &depth, &[region])
        .expect("recorded");
    let mut out = context.command_buffer();
    out.copy_image_to_buffer(&depth, &held, &[region])
        .expect("recorded");
    for commands in [&depth_clear, &into, &out] {
        assert_eq!(run(&context, 1, commands), refused(Error::Unsupported));
        assert_eq!(run(&context, 0, commands), Ok(()));
    }
}

#[test]
fn a_copy_engine_starts_a_buffer_s_side_of_an_image_copy_on_a_whole_word() {
    let device = stating(copy_engine(Granularity::TEXEL));
    let context = context(&device);
    let (_memory, held) = buffer(&context, 64);
    let (_image_memory, bytes) = image(&context, &desc(Format::R8Unorm, (4, 4), 1));
    for (offset, engine) in [(2, refused(Error::Misaligned)), (4, Ok(()))] {
        let mut commands = context.command_buffer();
        let region = texels((offset, 1), Aspects::COLOR, (at(0, 0), sized(4, 4)));
        commands
            .copy_buffer_to_image(&held, &bytes, &[region])
            .expect("recorded");
        assert_eq!(run(&context, 1, &commands), engine, "from byte {offset}");
        assert_eq!(run(&context, 0, &commands), Ok(()), "from byte {offset}");
    }
}

#[test]
fn a_copy_engine_keeps_its_image_copies_to_its_granularity() {
    let blocks = Granularity::Blocks { log2: [2, 2, 0] };
    let (width, height) = (10, 8);
    let placed = [
        (at(4, 4), sized(4, 4), Ok(())),
        (at(8, 0), sized(2, 4), Ok(())),
        (at(0, 4), sized(10, 4), Ok(())),
        (at(2, 0), sized(4, 4), refused(Error::Misaligned)),
        (at(0, 0), sized(3, 4), refused(Error::Misaligned)),
        (at(4, 2), sized(4, 4), refused(Error::Misaligned)),
    ];
    for (granularity, cases) in [
        (blocks, &placed[..]),
        (
            Granularity::Levels,
            &[
                (at(0, 0), sized(width, height), Ok(())),
                (at(4, 4), sized(4, 4), refused(Error::Misaligned)),
                (at(0, 0), sized(8, 8), refused(Error::Misaligned)),
            ][..],
        ),
    ] {
        let device = stating(copy_engine(granularity));
        let context = context(&device);
        let (_memory, held) = buffer(&context, 512);
        let colour = desc(Format::Rgba8Unorm, (width, height), 1);
        let (_src_memory, src) = image(&context, &colour);
        let (_dst_memory, dst) = image(&context, &colour);
        for &(origin, extent, ref engine) in cases {
            let mut copies = context.command_buffer();
            copies
                .copy_buffer_to_image(
                    &held,
                    &src,
                    &[texels((0, 4), Aspects::COLOR, (origin, extent))],
                )
                .expect("recorded");
            let mut between = context.command_buffer();
            between
                .copy_image(
                    &src,
                    &dst,
                    &[ImageCopy {
                        src: whole(Aspects::COLOR, 1),
                        src_origin: origin,
                        dst: whole(Aspects::COLOR, 1),
                        dst_origin: origin,
                        extent,
                    }],
                )
                .expect("recorded");
            for commands in [&copies, &between] {
                let case = (granularity, origin, extent);
                assert_eq!(run(&context, 1, commands), *engine, "{case:?}");
                assert_eq!(run(&context, 0, commands), Ok(()), "{case:?}");
            }
        }
    }
}

#[test]
fn multisampled_depth_is_copied_only_on_a_family_that_draws() {
    let device = stating(COMPUTING);
    let context = context(&device);
    let copy = ImageCopy {
        src: whole(Aspects::DEPTH, 1),
        src_origin: at(0, 0),
        dst: whole(Aspects::DEPTH, 1),
        dst_origin: at(0, 0),
        extent: sized(4, 4),
    };
    for (samples, computing) in [(4, refused(Error::Unsupported)), (1, Ok(()))] {
        let depth = desc(Format::Depth32Float, (4, 4), samples);
        let (_src_memory, src) = image(&context, &depth);
        let (_dst_memory, dst) = image(&context, &depth);
        let mut commands = context.command_buffer();
        commands.copy_image(&src, &dst, &[copy]).expect("recorded");
        assert_eq!(run(&context, 1, &commands), computing, "{samples} samples");
        assert_eq!(run(&context, 0, &commands), Ok(()), "{samples} samples");
    }
}

#[test]
fn a_family_that_computes_or_draws_states_single_texels() {
    let mut info = crate::software::info(RAM).expect("the software device's statement");
    let family = |capabilities, granularity| QueueFamily {
        capabilities,
        queues: 1,
        granularity,
    };
    let coarse = Granularity::Blocks { log2: [1, 0, 0] };
    let widest = Granularity::Blocks {
        log2: [crate::MAX_GRANULARITY_LOG2; 3],
    };
    let past = Granularity::Blocks {
        log2: [0, crate::MAX_GRANULARITY_LOG2 + 1, 0],
    };
    let transfer = QueueCapabilities::TRANSFER;
    for (stated, admitted) in [
        (family(transfer, Granularity::Levels), true),
        (family(transfer, widest), true),
        (family(transfer, past), false),
        (family(COMPUTING.capabilities, Granularity::TEXEL), true),
        (family(COMPUTING.capabilities, Granularity::Levels), false),
        (family(COMPUTING.capabilities, coarse), false),
        (family(QueueCapabilities::ALL, coarse), false),
    ] {
        info.queue_families.truncate(1);
        info.queue_families.try_push(stated).expect("room");
        assert_eq!(info.check().is_ok(), admitted, "{stated:?}");
    }
}

#[test]
fn an_engine_that_draws_reaches_depth_and_stencil_though_its_draws_are_not_stated() {
    let device = stating(DRAWING_ENGINE);
    let context = context(&device);
    let (_memory, held) = buffer(&context, 256);
    let (_depth_memory, depth) = image(&context, &desc(Format::Depth32Float, (4, 4), 1));
    let region = texels((0, 4), Aspects::DEPTH, (at(0, 0), sized(4, 4)));
    let mut moved = context.command_buffer();
    moved
        .copy_buffer_to_image(&held, &depth, &[region])
        .expect("recorded");
    moved
        .copy_image_to_buffer(&depth, &held, &[region])
        .expect("recorded");
    assert_eq!(run(&context, 1, &moved), Ok(()));
    let cleared = clear(
        &depth,
        &context,
        ClearValue::DepthStencil {
            depth: 0.25,
            stencil: 0,
        },
    );
    assert_eq!(run(&context, 1, &cleared), Ok(()));
}

#[test]
fn what_a_family_states_of_depth_and_stencil_holds_together_and_needs_transfers() {
    let mut info = crate::software::info(RAM).expect("the software device's statement");
    let family = |capabilities| QueueFamily {
        capabilities,
        queues: 1,
        granularity: Granularity::TEXEL,
    };
    let reaching = QueueCapabilities::DEPTH_STENCIL;
    for (capabilities, admitted) in [
        (DRAWING_ENGINE.capabilities, true),
        (reaching, false),
        (reaching.with(QueueCapabilities::COMPUTE), false),
        (
            QueueCapabilities::TRANSFER.with(QueueCapabilities::GRAPHICS),
            false,
        ),
        (QueueCapabilities::GRAPHICS, true),
    ] {
        info.queue_families.truncate(1);
        info.queue_families
            .try_push(family(capabilities))
            .expect("room");
        assert_eq!(info.check().is_ok(), admitted, "{capabilities:?}");
    }
    let coarse = QueueFamily {
        granularity: Granularity::Levels,
        ..DRAWING_ENGINE
    };
    info.queue_families.truncate(1);
    info.queue_families.try_push(coarse).expect("room");
    assert!(info.check().is_err(), "an engine that draws copies texels");
    let computing_alone = family(QueueCapabilities::COMPUTE);
    assert!(
        !computing_alone.clears_colour() && !computing_alone.transfers_depth_stencil(),
        "a family that transfers nothing clears and moves nothing"
    );
}
