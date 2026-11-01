use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::Component;

use super::{
    level_extent, BufferImageCopy, ClearValue, Extent3d, ImageCopy, ImageDesc, ImageDimension,
    ImageRange, ImageSubresource, ImageUsage, ImageViewDesc, Origin3d, ViewDimension,
};
use crate::format::{Aspects, Format};
use crate::Error;

fn device() -> crate::DeviceInfo {
    crate::software::info(1 << 31).expect("a device")
}

fn square(size: u32, layers: u32) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format: Format::Rgba8Unorm,
        extent: Extent3d {
            width: size,
            height: size,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: layers,
        samples: 1,
        usage: ImageUsage::SAMPLED.with(ImageUsage::TRANSFER_DST),
    }
}

fn view(dimension: ViewDimension, base_layer: u32, layers: u32) -> ImageViewDesc {
    ImageViewDesc {
        format: Format::Rgba8Unorm,
        dimension,
        range: ImageRange {
            aspects: Aspects::COLOR,
            base_level: 0,
            levels: 1,
            base_layer,
            layers,
        },
    }
}

#[test]
fn an_image_takes_only_the_shape_its_dimension_has() {
    let mut desc = square(64, 1);
    assert_eq!(desc.check(&device()), Ok(()));
    desc.extent.depth = 2;
    assert_eq!(desc.check(&device()), Err(Error::Shape));
    let mut line = square(64, 1);
    line.dimension = ImageDimension::D1;
    assert_eq!(
        line.check(&device()),
        Err(Error::Shape),
        "a row is one texel tall"
    );
    let mut volume = square(16, 2);
    volume.dimension = ImageDimension::D3;
    assert_eq!(
        volume.check(&device()),
        Err(Error::Shape),
        "a box has one layer"
    );
    let mut empty = square(16, 1);
    empty.extent.width = 0;
    assert_eq!(empty.check(&device()), Err(Error::Empty));
}

#[test]
fn an_image_is_held_to_the_devices_sizes_and_its_own_mip_chain() {
    let mut wide = square(1, 1);
    wide.extent.width = device().limits.max_image_dimension_2d + 1;
    assert_eq!(wide.check(&device()), Err(Error::TooLarge));
    let mut chained = square(64, 1);
    chained.mip_levels = 7;
    assert_eq!(
        chained.check(&device()),
        Ok(()),
        "64 halves to 1 in seven levels"
    );
    chained.mip_levels = 8;
    assert_eq!(chained.check(&device()), Err(Error::TooLarge));
    assert_eq!(
        level_extent(chained.extent, ImageDimension::D2, 3),
        Extent3d {
            width: 8,
            height: 8,
            depth: 1
        }
    );
    assert_eq!(
        level_extent(chained.extent, ImageDimension::D2, 40).width,
        1
    );
}

#[test]
fn a_usage_needs_its_formats_feature_and_samples_need_an_attachment() {
    let mut stored = square(8, 1);
    stored.format = Format::Rgba8UnormSrgb;
    stored.usage = ImageUsage::STORAGE;
    assert_eq!(stored.check(&device()), Err(Error::Unsupported));
    let mut depth = square(8, 1);
    depth.format = Format::Depth32Float;
    depth.usage = ImageUsage::COLOR_ATTACHMENT;
    assert_eq!(depth.check(&device()), Err(Error::Unsupported));
    let mut sampled = square(8, 1);
    sampled.samples = 4;
    assert_eq!(
        sampled.check(&device()),
        Err(Error::Samples),
        "no attachment use"
    );
    sampled.usage = ImageUsage::COLOR_ATTACHMENT.with(ImageUsage::SAMPLED);
    assert_eq!(sampled.check(&device()), Ok(()));
    sampled.samples = 2;
    assert_eq!(sampled.check(&device()), Err(Error::Samples));
}

#[test]
fn a_view_sees_only_what_its_image_holds_in_a_shape_it_takes() {
    let image = square(32, 12);
    assert_eq!(view(ViewDimension::D2Array, 0, 12).check(&image), Ok(()));
    assert_eq!(view(ViewDimension::Cube, 6, 6).check(&image), Ok(()));
    assert_eq!(view(ViewDimension::CubeArray, 0, 12).check(&image), Ok(()));
    assert_eq!(
        view(ViewDimension::CubeArray, 0, 9).check(&image),
        Err(Error::Shape)
    );
    assert_eq!(
        view(ViewDimension::Cube, 7, 6).check(&image),
        Err(Error::OutOfRange)
    );
    assert_eq!(
        view(ViewDimension::D2, 0, 2).check(&image),
        Err(Error::Shape)
    );
    assert_eq!(
        view(ViewDimension::D3, 0, 1).check(&image),
        Err(Error::Shape)
    );
    let mut tall = square(32, 6);
    tall.extent.height = 16;
    assert_eq!(
        view(ViewDimension::Cube, 0, 6).check(&tall),
        Err(Error::Shape)
    );
    let mut depth = view(ViewDimension::D2, 0, 1);
    depth.range.aspects = Aspects::DEPTH;
    assert_eq!(depth.check(&image), Err(Error::Aspect));
}

#[test]
fn a_views_shape_is_what_a_module_declares() {
    let mut image = square(32, 6);
    image.format = Format::R32Float;
    let cubes = ImageViewDesc {
        format: Format::R32Float,
        ..view(ViewDimension::CubeArray, 0, 6)
    };
    let shape = cubes.shape(&image);
    assert_eq!((shape.dim, shape.arrayed), (Dim::CUBE, true));
    assert_eq!(shape.format, ImageFormat::R32F);
    assert_eq!(shape.component, Component::Float);
    let mut stencil = square(8, 1);
    stencil.format = Format::Depth32FloatStencil8;
    let mut seen = view(ViewDimension::D2, 0, 1);
    seen.format = Format::Depth32FloatStencil8;
    seen.range.aspects = Aspects::STENCIL;
    assert_eq!(seen.shape(&stencil).component, Component::Uint);
}

fn copy(extent: Extent3d, bytes_per_row: u32, rows_per_image: u32) -> BufferImageCopy {
    BufferImageCopy {
        buffer_offset: 0,
        bytes_per_row,
        rows_per_image,
        image: ImageSubresource {
            aspect: Aspects::COLOR,
            level: 0,
            base_layer: 0,
            layers: 2,
        },
        origin: Origin3d::default(),
        extent,
    }
}

#[test]
fn a_buffer_copy_spans_its_rows_and_layers() {
    let image = square(16, 2);
    let region = Extent3d {
        width: 4,
        height: 3,
        depth: 1,
    };
    assert_eq!(copy(region, 32, 4).check(&image), Ok(32 * 4 + 32 * 2 + 16));
    assert_eq!(
        copy(region, 12, 3).check(&image),
        Err(Error::Misaligned),
        "a row is 16 bytes"
    );
    assert_eq!(
        copy(region, 18, 3).check(&image),
        Err(Error::Misaligned),
        "not whole texels"
    );
    assert_eq!(
        copy(region, 16, 2).check(&image),
        Err(Error::Misaligned),
        "fewer rows than the region"
    );
    let past = BufferImageCopy {
        origin: Origin3d { x: 13, y: 0, z: 0 },
        ..copy(region, 16, 3)
    };
    assert_eq!(past.check(&image), Err(Error::OutOfRange));
}

#[test]
fn a_copy_between_images_moves_one_plane_between_equal_layers() {
    let image = square(16, 2);
    let part = ImageSubresource {
        aspect: Aspects::COLOR,
        level: 0,
        base_layer: 0,
        layers: 1,
    };
    let region = ImageCopy {
        src: part,
        src_origin: Origin3d::default(),
        dst: ImageSubresource {
            base_layer: 1,
            ..part
        },
        dst_origin: Origin3d { x: 8, y: 8, z: 0 },
        extent: Extent3d {
            width: 8,
            height: 8,
            depth: 1,
        },
    };
    assert_eq!(region.check(&image, &image), Ok(()));
    let uneven = ImageCopy {
        dst: ImageSubresource { layers: 2, ..part },
        ..region
    };
    assert_eq!(uneven.check(&image, &image), Err(Error::Shape));
}

#[test]
fn a_clear_value_is_of_the_kind_its_format_holds() {
    assert_eq!(
        ClearValue::Float([0.0; 4]).check(Format::Rgba8Unorm),
        Ok(())
    );
    assert_eq!(
        ClearValue::Uint([0; 4]).check(Format::Rgba8Unorm),
        Err(Error::Shape)
    );
    assert_eq!(ClearValue::Sint([0; 4]).check(Format::Rg16Sint), Ok(()));
    let depth = ClearValue::DepthStencil {
        depth: 1.0,
        stencil: 3,
    };
    assert_eq!(depth.check(Format::Depth32FloatStencil8), Ok(()));
    assert_eq!(depth.check(Format::R32Float), Err(Error::Shape));
    let beyond = ClearValue::DepthStencil {
        depth: 1.5,
        stencil: 0,
    };
    assert_eq!(beyond.check(Format::Depth16Unorm), Err(Error::Shape));
}

#[test]
fn a_view_reads_its_image_as_itself_or_its_srgb_twin() {
    let image = square(8, 1);
    let srgb = ImageViewDesc {
        format: Format::Rgba8UnormSrgb,
        ..view(ViewDimension::D2, 0, 1)
    };
    assert_eq!(srgb.check(&image), Ok(()));
    let other = ImageViewDesc {
        format: Format::Bgra8Unorm,
        ..view(ViewDimension::D2, 0, 1)
    };
    assert_eq!(other.check(&image), Err(Error::Shape));
}

#[test]
fn a_texel_buffer_holds_whole_texels_of_a_format_stated_for_its_use() {
    use super::BufferViewDesc;
    use crate::BufferUsage;
    let device = device();
    let view = |offset, size, format| BufferViewDesc {
        offset,
        size,
        format,
    };
    let uniform = BufferUsage::UNIFORM_TEXEL;
    assert_eq!(
        view(16, 64, Format::Rgba8Unorm).check(256, uniform, &device),
        Ok(())
    );
    assert_eq!(
        view(4, 64, Format::Rgba8Unorm).check(256, uniform, &device),
        Err(Error::Misaligned)
    );
    assert_eq!(
        view(0, 6, Format::Rg16Float).check(256, uniform, &device),
        Err(Error::Misaligned)
    );
    assert_eq!(
        view(240, 32, Format::R32Uint).check(256, uniform, &device),
        Err(Error::OutOfRange)
    );
    assert_eq!(
        view(0, 64, Format::Rgba8UnormSrgb).check(256, BufferUsage::STORAGE_TEXEL, &device),
        Err(Error::Unsupported),
        "an sRGB texel is never written by texel"
    );
    assert_eq!(
        view(0, 64, Format::Rgba8Unorm).check(256, BufferUsage::STORAGE, &device),
        Err(Error::Usage)
    );
    assert_eq!(
        view(0, 64, Format::Depth32Float).check(256, uniform, &device),
        Err(Error::Unsupported)
    );
    assert_eq!(view(0, 64, Format::R32Float).shape().dim, Dim::BUFFER);
}
