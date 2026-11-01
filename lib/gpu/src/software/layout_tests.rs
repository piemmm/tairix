use super::ImageLayout;
use crate::format::Format;
use crate::image::{Extent3d, ImageDesc, ImageDimension, ImageUsage};

fn image(format: Format, width: u32, height: u32, levels: u32, layers: u32) -> ImageDesc {
    ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width,
            height,
            depth: 1,
        },
        mip_levels: levels,
        array_layers: layers,
        samples: 1,
        usage: ImageUsage::SAMPLED,
    }
}

#[test]
fn levels_follow_one_another_each_layer_aligned() {
    let layout = ImageLayout::of(&image(Format::R8Unorm, 5, 3, 3, 2)).expect("a layout");
    let [first, second, third] = [0, 1, 2].map(|l| layout.levels[l]);
    assert_eq!(
        (first.row, first.slice, first.layer),
        (8, 24, 32),
        "rows of whole words"
    );
    assert_eq!(second.base, 64, "two layers of the first level");
    assert_eq!((second.extent.width, second.extent.height), (2, 1));
    assert_eq!(second.layer, 16);
    assert_eq!(third.base, 96);
    assert_eq!(layout.size, 128);
}

#[test]
fn a_texel_lies_where_its_level_layer_row_and_sample_put_it() {
    let layout = ImageLayout::of(&image(Format::Rgba8Unorm, 8, 8, 2, 3)).expect("a layout");
    assert_eq!(layout.texel_offset(0, 0, [0, 0, 0], 0), 0);
    assert_eq!(layout.texel_offset(0, 1, [0, 0, 0], 0), 256);
    assert_eq!(
        layout.texel_offset(0, 2, [3, 2, 0], 0),
        512 + 2 * 32 + 3 * 4
    );
    assert_eq!(layout.texel_offset(1, 0, [1, 1, 0], 0), 768 + 16 + 4);
    let mut sampled = image(Format::R32Float, 4, 4, 1, 1);
    sampled.samples = 4;
    let layout = ImageLayout::of(&sampled).expect("a layout");
    assert_eq!(layout.texel_offset(0, 0, [1, 0, 0], 2), (4 + 2) * 4);
    assert_eq!(layout.size, 4 * 4 * 4 * 4);
}

#[test]
fn a_depth_and_stencil_texel_holds_both_planes() {
    let layout =
        ImageLayout::of(&image(Format::Depth32FloatStencil8, 2, 2, 1, 1)).expect("a layout");
    assert_eq!(layout.texel, 8);
    assert_eq!(layout.size, 32);
}

#[test]
fn a_layout_past_what_a_size_can_count_is_refused() {
    let mut huge = image(Format::Rgba32Float, u32::MAX, u32::MAX, 1, u32::MAX);
    huge.dimension = ImageDimension::D2;
    assert_eq!(ImageLayout::of(&huge), Err(crate::Error::TooLarge));
}

#[test]
fn the_widest_image_with_its_whole_chain_is_laid_out_in_no_more_slots_than_it_needs() {
    let widest = super::super::MAX_IMAGE_2D;
    let levels = 32 - widest.leading_zeros();
    let layout =
        ImageLayout::of(&image(Format::R8Unorm, widest, widest, levels, 1)).expect("a layout");
    assert_eq!(layout.levels.len(), levels as usize);
    assert_eq!(
        super::MAX_LEVELS,
        levels as usize,
        "no slot no image can use"
    );
    let longer = image(Format::R8Unorm, widest * 2, widest * 2, levels + 1, 1);
    assert_eq!(ImageLayout::of(&longer), Err(crate::Error::TooLarge));
}
