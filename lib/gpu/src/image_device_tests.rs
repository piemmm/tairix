//! Images on the software device: their memory, their views, and the copies
//! and clears that move their texels.

use alloc::vec;
use alloc::vec::Vec;

use crate::device_tests::{buffer, context, device, memory, over, queue, submit};
use crate::{
    Aspects, BufferImageCopy, ClearValue, Context, Error, Extent3d, Format, Image, ImageCopy,
    ImageDesc, ImageDimension, ImageRange, ImageSubresource, ImageUsage, ImageViewDesc, Origin3d,
    ViewDimension,
};

fn desc(format: Format, width: u32, height: u32, layers: u32) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width,
            height,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: layers,
        samples: 1,
        usage: ImageUsage::TRANSFER_SRC
            .with(ImageUsage::TRANSFER_DST)
            .with(ImageUsage::SAMPLED),
    }
}

/// An image of `desc` in an allocation of its own, which it returns too.
pub(crate) fn image(context: &Context, desc: &ImageDesc) -> (crate::Allocation, Image) {
    let needs = context.image_requirements(desc).expect("requirements");
    assert!(needs.alignment.is_power_of_two());
    assert_eq!(needs.memory_types & 1, 1);
    let allocation = memory(context, needs.size);
    let image = context
        .create_image(&allocation, 0, desc)
        .expect("an image");
    (allocation, image)
}

fn whole(aspect: Aspects, layers: u32) -> ImageSubresource {
    ImageSubresource {
        aspect,
        level: 0,
        base_layer: 0,
        layers,
    }
}

fn region(extent: Extent3d, bytes_per_row: u32, rows: u32, layers: u32) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset: 0,
        bytes_per_row,
        rows_per_image: rows,
        image: whole(Aspects::COLOR, layers),
        origin: Origin3d::default(),
        extent,
    }
}

fn run(context: &Context, record: impl FnOnce(&mut crate::CommandBuffer)) -> Result<(), Error> {
    let mut commands = context.command_buffer();
    record(&mut commands);
    submit(&queue(context), &commands)
        .map(|_| ())
        .map_err(|e| e.error)
}

fn read(allocation: &crate::Allocation, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    allocation.read(0, &mut out).expect("readable");
    out
}

#[test]
fn texels_copied_into_an_image_come_back_out_as_they_went_in() {
    let (device, _) = device();
    let context = context(&device);
    let extent = Extent3d {
        width: 5,
        height: 3,
        depth: 1,
    };
    for format in [
        Format::R8Unorm,
        Format::Rg16Float,
        Format::Rgba8Unorm,
        Format::Rgba32Float,
    ] {
        let texel = format.copy_bytes(Aspects::COLOR).expect("colour") as usize;
        let (_, picture) = image(&context, &desc(format, 5, 3, 2));
        let row = 5 * texel;
        let per_row = (row + texel).next_multiple_of(texel.max(4));
        let len = per_row * 3 * 2;
        let (source, upload) = buffer(&context, len as u64);
        let pattern: Vec<u8> = (0..len)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        source.write(0, &pattern).expect("writable");
        let (target, download) = buffer(&context, len as u64);
        let copy = region(extent, u32::try_from(per_row).unwrap_or(0), 3, 2);
        run(&context, |c| {
            c.copy_buffer_to_image(&upload, &picture, &[copy])
                .expect("recorded");
            c.copy_image_to_buffer(&picture, &download, &[copy])
                .expect("recorded");
        })
        .expect("admitted");
        let back = read(&target, len);
        for slice in 0..6 {
            let at = slice * per_row;
            assert_eq!(
                back[at..at + row],
                pattern[at..at + row],
                "{format:?} row {slice}"
            );
            assert!(
                back[at + row..at + per_row].iter().all(|&b| b == 0),
                "padding untouched"
            );
        }
    }
}

#[test]
fn depth_and_stencil_are_copied_a_plane_at_a_time() {
    let (device, _) = device();
    let context = context(&device);
    let mut both = desc(Format::Depth32FloatStencil8, 2, 2, 1);
    both.usage = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
    let (_, picture) = image(&context, &both);
    let (depths, depth_buffer) = buffer(&context, 16);
    let depth_values: Vec<u8> = [0.25f32, 0.5, 0.75, 1.0]
        .iter()
        .flat_map(|d| d.to_le_bytes())
        .collect();
    depths.write(0, &depth_values).expect("writable");
    let (stencils, stencil_buffer) = buffer(&context, 16);
    stencils
        .write(0, &[7, 8, 0, 0, 9, 10, 0, 0])
        .expect("writable");
    let extent = Extent3d {
        width: 2,
        height: 2,
        depth: 1,
    };
    let depth_copy = BufferImageCopy {
        image: whole(Aspects::DEPTH, 1),
        ..region(extent, 8, 2, 1)
    };
    let stencil_copy = BufferImageCopy {
        image: whole(Aspects::STENCIL, 1),
        ..region(extent, 4, 2, 1)
    };
    let (out_depths, out_depth_buffer) = buffer(&context, 16);
    let (out_stencils, out_stencil_buffer) = buffer(&context, 16);
    run(&context, |c| {
        c.copy_buffer_to_image(&depth_buffer, &picture, &[depth_copy])
            .expect("recorded");
        c.copy_buffer_to_image(&stencil_buffer, &picture, &[stencil_copy])
            .expect("recorded");
        c.copy_image_to_buffer(&picture, &out_depth_buffer, &[depth_copy])
            .expect("recorded");
        c.copy_image_to_buffer(&picture, &out_stencil_buffer, &[stencil_copy])
            .expect("recorded");
    })
    .expect("admitted");
    assert_eq!(read(&out_depths, 16), depth_values);
    assert_eq!(read(&out_stencils, 8), [7, 8, 0, 0, 9, 10, 0, 0]);
    let joint = BufferImageCopy {
        image: whole(Aspects::DEPTH_STENCIL, 1),
        ..region(extent, 8, 2, 1)
    };
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&depth_buffer, &picture, &[joint])
            .expect("recorded")),
        Err(Error::Aspect)
    );
    let mut chosen = both;
    chosen.format = Format::Depth24PlusStencil8;
    let (_, device_chosen) = image(&context, &chosen);
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&depth_buffer, &device_chosen, &[depth_copy])
            .expect("recorded")),
        Err(Error::Aspect),
        "the device's own depth bits are never copied out"
    );
}

#[test]
fn a_clear_writes_every_texel_of_its_ranges_and_no_other() {
    let (device, _) = device();
    let context = context(&device);
    let (_, picture) = image(&context, &desc(Format::Rgba8UnormSrgb, 3, 2, 2));
    let range = ImageRange {
        aspects: Aspects::COLOR,
        base_level: 0,
        levels: 1,
        base_layer: 1,
        layers: 1,
    };
    let (target, download) = buffer(&context, 48);
    let copy = region(
        Extent3d {
            width: 3,
            height: 2,
            depth: 1,
        },
        12,
        2,
        2,
    );
    run(&context, |c| {
        c.clear_image(
            &picture,
            ClearValue::Float([0.0, 0.214_041_14, 1.0, 0.5]),
            &[range],
        )
        .expect("recorded");
        c.copy_image_to_buffer(&picture, &download, &[copy])
            .expect("recorded");
    })
    .expect("admitted");
    let back = read(&target, 48);
    assert!(back[..24].iter().all(|&b| b == 0), "layer 0 untouched");
    for texel in back[24..].as_chunks::<4>().0 {
        assert_eq!(
            *texel,
            [0, 128, 255, 128],
            "linear 0.214 encodes to sRGB 128"
        );
    }
}

#[test]
fn a_clear_of_one_plane_keeps_the_other() {
    let (device, _) = device();
    let context = context(&device);
    let mut both = desc(Format::Depth32FloatStencil8, 2, 1, 1);
    both.usage = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
    let (_, picture) = image(&context, &both);
    let all = ImageRange {
        aspects: Aspects::DEPTH_STENCIL,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    let depth_only = ImageRange {
        aspects: Aspects::DEPTH,
        ..all
    };
    let (stencils, stencil_buffer) = buffer(&context, 16);
    let (depths, depth_buffer) = buffer(&context, 16);
    let extent = Extent3d {
        width: 2,
        height: 1,
        depth: 1,
    };
    run(&context, |c| {
        c.clear_image(
            &picture,
            ClearValue::DepthStencil {
                depth: 1.0,
                stencil: 5,
            },
            &[all],
        )
        .expect("recorded");
        c.clear_image(
            &picture,
            ClearValue::DepthStencil {
                depth: 0.5,
                stencil: 9,
            },
            &[depth_only],
        )
        .expect("recorded");
        c.copy_image_to_buffer(
            &picture,
            &stencil_buffer,
            &[BufferImageCopy {
                image: whole(Aspects::STENCIL, 1),
                ..region(extent, 4, 1, 1)
            }],
        )
        .expect("recorded");
        c.copy_image_to_buffer(
            &picture,
            &depth_buffer,
            &[BufferImageCopy {
                image: whole(Aspects::DEPTH, 1),
                ..region(extent, 8, 1, 1)
            }],
        )
        .expect("recorded");
    })
    .expect("admitted");
    assert_eq!(
        read(&stencils, 2),
        [5, 5],
        "the stencil keeps the first clear"
    );
    let half = 0.5f32.to_le_bytes();
    assert_eq!(read(&depths, 8), [half, half].concat());
}

#[test]
fn a_clear_of_the_stencil_alone_keeps_the_depth_across_a_row() {
    let (device, _) = device();
    let context = context(&device);
    // Three texels a row, so the row ends partway through the texels' repeat.
    let mut both = desc(Format::Depth32FloatStencil8, 3, 2, 1);
    both.usage = ImageUsage::TRANSFER_SRC.with(ImageUsage::TRANSFER_DST);
    let (_, picture) = image(&context, &both);
    let all = ImageRange {
        aspects: Aspects::DEPTH_STENCIL,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    let stencil_only = ImageRange {
        aspects: Aspects::STENCIL,
        ..all
    };
    let (stencils, stencil_buffer) = buffer(&context, 32);
    let (depths, depth_buffer) = buffer(&context, 32);
    let extent = Extent3d {
        width: 3,
        height: 2,
        depth: 1,
    };
    run(&context, |c| {
        c.clear_image(
            &picture,
            ClearValue::DepthStencil {
                depth: 0.75,
                stencil: 5,
            },
            &[all],
        )
        .expect("recorded");
        c.clear_image(
            &picture,
            ClearValue::DepthStencil {
                depth: 0.25,
                stencil: 200,
            },
            &[stencil_only],
        )
        .expect("recorded");
        c.copy_image_to_buffer(
            &picture,
            &stencil_buffer,
            &[BufferImageCopy {
                image: whole(Aspects::STENCIL, 1),
                ..region(extent, 4, 2, 1)
            }],
        )
        .expect("recorded");
        c.copy_image_to_buffer(
            &picture,
            &depth_buffer,
            &[BufferImageCopy {
                image: whole(Aspects::DEPTH, 1),
                ..region(extent, 12, 2, 1)
            }],
        )
        .expect("recorded");
    })
    .expect("admitted");
    let stencil_rows = [200, 200, 200, 0, 200, 200, 200, 0];
    assert_eq!(read(&stencils, 8), stencil_rows, "the stencil cleared");
    let kept = 0.75f32.to_le_bytes();
    assert_eq!(read(&depths, 24), [kept; 6].concat(), "the depth kept");
}

#[test]
fn copies_between_images_move_texels_of_one_size() {
    let (device, _) = device();
    let context = context(&device);
    let (_, colours) = image(&context, &desc(Format::Rgba8Unorm, 4, 4, 1));
    let (_, words) = image(&context, &desc(Format::R32Uint, 4, 4, 1));
    let (_, small) = image(&context, &desc(Format::R8Unorm, 4, 4, 1));
    let (source, upload) = buffer(&context, 64);
    let pattern: Vec<u8> = (0..64).collect();
    source.write(0, &pattern).expect("writable");
    let extent = Extent3d {
        width: 4,
        height: 4,
        depth: 1,
    };
    let copy = ImageCopy {
        src: whole(Aspects::COLOR, 1),
        src_origin: Origin3d::default(),
        dst: whole(Aspects::COLOR, 1),
        dst_origin: Origin3d::default(),
        extent,
    };
    let (target, download) = buffer(&context, 64);
    run(&context, |c| {
        c.copy_buffer_to_image(&upload, &colours, &[region(extent, 16, 4, 1)])
            .expect("recorded");
        c.copy_image(&colours, &words, &[copy]).expect("recorded");
        c.copy_image_to_buffer(&words, &download, &[region(extent, 16, 4, 1)])
            .expect("recorded");
    })
    .expect("admitted");
    assert_eq!(
        read(&target, 64),
        pattern,
        "a 32-bit texel is copied whatever it means"
    );
    assert_eq!(
        run(&context, |c| c
            .copy_image(&colours, &small, &[copy])
            .expect("recorded")),
        Err(Error::Shape),
        "texels of different sizes"
    );
    let overlapping = ImageCopy {
        dst_origin: Origin3d { x: 2, y: 2, z: 0 },
        extent: Extent3d {
            width: 2,
            height: 2,
            depth: 1,
        },
        ..copy
    };
    let reads_itself = ImageCopy {
        dst_origin: Origin3d { x: 1, y: 1, z: 0 },
        extent: Extent3d {
            width: 2,
            height: 2,
            depth: 1,
        },
        ..copy
    };
    assert_eq!(
        run(&context, |c| c
            .copy_image(&colours, &colours, &[reads_itself])
            .expect("recorded")),
        Err(Error::Overlap)
    );
    assert_eq!(
        run(&context, |c| c
            .copy_image(&colours, &colours, &[overlapping])
            .expect("recorded")),
        Ok(()),
        "disjoint halves of one image"
    );
}

#[test]
fn a_copy_is_refused_past_either_side_or_where_its_objects_share_memory() {
    let (device, _) = device();
    let context = context(&device);
    let picture_desc = desc(Format::Rgba8Unorm, 4, 4, 1);
    let needs = context
        .image_requirements(&picture_desc)
        .expect("requirements");
    let shared = memory(&context, needs.size + 256);
    let picture = context
        .create_image(&shared, 0, &picture_desc)
        .expect("an image");
    let alias = over(&context, &shared, 0, 64);
    let apart = over(&context, &shared, needs.size.next_multiple_of(16), 64);
    let extent = Extent3d {
        width: 4,
        height: 4,
        depth: 1,
    };
    let copy = region(extent, 16, 4, 1);
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&alias, &picture, &[copy])
            .expect("recorded")),
        Err(Error::Overlap),
        "the buffer is the image's own memory"
    );
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&apart, &picture, &[copy])
            .expect("recorded")),
        Ok(())
    );
    let (_, short) = buffer(&context, 48);
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&short, &picture, &[copy])
            .expect("recorded")),
        Err(Error::OutOfRange)
    );
    let twice = [copy, copy];
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&apart, &picture, &twice)
            .expect("recorded")),
        Err(Error::Overlap),
        "a texel written twice"
    );
    let mut sealed = picture_desc;
    sealed.usage = ImageUsage::SAMPLED;
    let (_, unwritable) = image(&context, &sealed);
    assert_eq!(
        run(&context, |c| c
            .copy_buffer_to_image(&apart, &unwritable, &[copy])
            .expect("recorded")),
        Err(Error::Usage)
    );
}

#[test]
fn an_image_or_view_of_another_context_is_refused_and_a_released_one_is_stale() {
    let (device, _) = device();
    let mine = context(&device);
    let theirs = context(&device);
    let picture_desc = desc(Format::Rgba8Unorm, 4, 4, 1);
    let (_, their_picture) = image(&theirs, &picture_desc);
    let view = ImageViewDesc {
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
    assert_eq!(
        mine.create_image_view(&their_picture, &view).err(),
        Some(Error::Foreign)
    );
    let (_, upload) = buffer(&mine, 64);
    let mut commands = mine.command_buffer();
    assert_eq!(
        commands
            .copy_buffer_to_image(
                &upload,
                &their_picture,
                &[region(
                    Extent3d {
                        width: 4,
                        height: 4,
                        depth: 1
                    },
                    16,
                    4,
                    1
                )]
            )
            .err(),
        Some(Error::Foreign)
    );
    let (allocation, picture) = image(&mine, &picture_desc);
    let seen = mine.create_image_view(&picture, &view).expect("a view");
    drop(picture);
    drop(allocation);
    assert_eq!(
        seen.desc().format,
        Format::Rgba8Unorm,
        "a view holds its image"
    );
    let placed = memory(&mine, 1024);
    assert_eq!(
        mine.create_image(&placed, 8, &picture_desc).err(),
        Some(Error::Misaligned)
    );
    assert_eq!(
        mine.create_image(&placed, 976, &picture_desc).err(),
        Some(Error::OutOfRange)
    );
}

#[test]
fn a_storage_image_past_what_an_invocation_addresses_is_refused() {
    let (device, _) = device();
    let context = context(&device);
    let most = device.info().limits.max_storage_image_bytes;
    // 16384 by 16384 words a layer: a gibibyte each.
    let layers = |count: u32, usage: ImageUsage| ImageDesc {
        usage,
        ..desc(Format::R32Uint, 16384, 16384, count)
    };
    let storage = ImageUsage::STORAGE.with(ImageUsage::TRANSFER_DST);
    let fits = context
        .image_requirements(&layers(3, storage))
        .expect("three gibibytes are addressed");
    assert!(fits.size <= most);
    assert_eq!(
        context.image_requirements(&layers(5, storage)).err(),
        Some(Error::TooLarge),
        "a texel past four gibibytes is no 32-bit offset"
    );
    let sampled = context
        .image_requirements(&layers(5, ImageUsage::SAMPLED))
        .expect("an image only sampled is read through 64-bit offsets");
    assert!(sampled.size > most);
}

#[test]
fn what_an_image_needs_is_answered_without_the_devices_lock() {
    let (device, _) = device();
    let context = context(&device);
    let before = crate::fair::held::takings();
    let needs = context
        .image_requirements(&desc(Format::Rgba8Unorm, 64, 64, 1))
        .expect("requirements");
    assert_eq!(needs.size, 64 * 64 * 4);
    assert_eq!(
        context
            .image_requirements(&desc(Format::Rgba8Unorm, 0, 64, 1))
            .err(),
        Some(Error::Empty)
    );
    assert_eq!(crate::fair::held::takings(), before);
}
