//! An image read at its edges: a fetch moved by its offset, a projective
//! coordinate longer than its view needs, the border colour of a format of
//! fewer channels or of a stencil, a depth reference past a normalized
//! format's range, and a cube's level of detail from its gradients.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{BindingKind, Component, ImageShape};

use super::f;
use super::image::{
    dispatch, filled, module, nearest, plain, shape, small, square, stores, view, Bound,
};
use crate::device_tests::{context, device};
use crate::sampler::{AddressMode, BorderColor};
use crate::{
    Aspects, CompareOp, Extent3d, Filter, Format, ImageDesc, ImageRange, ImageUsage, ImageViewDesc,
    Resource, SamplerDesc, ViewDimension,
};

/// The words of each of `values`, a float apiece, stored in turn.
fn floats_out(body: &mut String, values: &[(&str, u32)]) {
    let mut names = Vec::new();
    for (k, (value, component)) in values.iter().enumerate() {
        let _ = writeln!(
            body,
            "%e{k} = OpCompositeExtract %float {value} {component}\n%u{k} = OpBitcast %uint %e{k}"
        );
        names.push(alloc::format!("%u{k}"));
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    body.push_str(&stores(&names, false));
}

/// A 4-by-4 float image whose texel `(x, y)` holds `x + 4y`, and its view.
fn counted(context: &crate::Context) -> (crate::Allocation, crate::Image, crate::ImageView) {
    let (held, picture) = filled(
        context,
        &square(Format::R32Float, ImageUsage::SAMPLED, 1),
        Aspects::COLOR,
        |_, _, x, y| f(small(x + 4 * y)),
    );
    let texels = view(
        context,
        &picture,
        &plain(Format::R32Float, Aspects::COLOR, 1),
    );
    (held, picture, texels)
}

const FETCH_DECLS: &str = r"%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%image_ptr = OpTypePointer UniformConstant %image_t
%sampled = OpVariable %image_ptr UniformConstant
%itwo = OpConstant %int 2
%ithree = OpConstant %int 3
%iminus = OpConstant %int -1
%origin = OpConstantComposite %v2int %izero %izero
%last = OpConstantComposite %v2int %ithree %izero
%inner = OpConstantComposite %v2int %ione %ione
%right = OpConstantComposite %v2int %ione %izero
%down = OpConstantComposite %v2int %izero %itwo
%left = OpConstantComposite %v2int %iminus %izero";

#[test]
fn a_fetch_reads_the_texel_its_offset_moves_it_to() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, _picture, texels) = counted(&context);
    let mut body = String::from(
        "%img = OpLoad %image_t %sampled\n%a = OpImageFetch %v4float %img %origin Lod|ConstOffset %izero %right\n%b = OpImageFetch %v4float %img %origin Lod|ConstOffset %izero %down\n%c = OpImageFetch %v4float %img %inner Lod|ConstOffset %izero %left\n%d = OpImageFetch %v4float %img %last Lod|ConstOffset %izero %right\n",
    );
    floats_out(&mut body, &[("%a", 0), ("%b", 0), ("%c", 0), ("%d", 0)]);
    let text = module(
        "%sampled",
        "OpDecorate %sampled DescriptorSet 0\nOpDecorate %sampled Binding 1",
        FETCH_DECLS,
        &body,
        1,
    );
    let kind = BindingKind::SampledImage(shape(Component::Float, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(kind, alloc::vec![Resource::Image(&texels)])],
        &[],
        4,
    );
    assert_eq!(
        out,
        [f(1.0), f(8.0), f(4.0), f(0.0)],
        "(1, 0), (0, 2) and (0, 1), and past the right edge zero"
    );
}

const PROJECTIVE_DECLS: &str = r"%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%combined = OpVariable %si_ptr UniformConstant
%f2 = OpConstant %float 2.0
%f075 = OpConstant %float 0.75
%f125 = OpConstant %float 1.25
%projected = OpConstantComposite %v4float %f075 %f125 %f2 %fzero";

#[test]
fn a_projective_coordinate_divides_by_the_component_past_its_view_s_axes() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, _picture, texels) = counted(&context);
    let point = context.create_sampler(&nearest()).expect("a sampler");
    let mut body = String::from(
        "%c = OpLoad %si_t %combined\n%v = OpImageSampleProjExplicitLod %v4float %c %projected Lod %fzero\n",
    );
    floats_out(&mut body, &[("%v", 0)]);
    let text = module(
        "%combined",
        "OpDecorate %combined DescriptorSet 0\nOpDecorate %combined Binding 1",
        PROJECTIVE_DECLS,
        &body,
        1,
    );
    let kind =
        BindingKind::CombinedImageSampler(shape(Component::Float, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&texels, &point)],
        )],
        &[],
        1,
    );
    assert_eq!(
        out[0],
        f(9.0),
        "(0.75, 1.25) over 2.0 is (1.5, 2.5) in texels, the fourth component unused"
    );
}

const BORDER_DECLS: &str = r"%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%white = OpVariable %si_ptr UniformConstant
%black = OpVariable %si_ptr UniformConstant
%fhalf = OpConstant %float 0.5
%fminus = OpConstant %float -0.5
%outside = OpConstantComposite %v2float %fminus %fhalf";

fn bordered(border: BorderColor) -> SamplerDesc {
    SamplerDesc {
        address: [AddressMode::ClampToBorder; 3],
        border,
        ..nearest()
    }
}

#[test]
fn a_border_fills_only_the_channels_the_format_holds() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, _picture, texels) = counted(&context);
    let white = context
        .create_sampler(&bordered(BorderColor::OpaqueWhite))
        .expect("a sampler");
    let black = context
        .create_sampler(&bordered(BorderColor::TransparentBlack))
        .expect("a sampler");
    let mut body = String::from(
        "%w = OpLoad %si_t %white\n%b = OpLoad %si_t %black\n%wv = OpImageSampleExplicitLod %v4float %w %outside Lod %fzero\n%bv = OpImageSampleExplicitLod %v4float %b %outside Lod %fzero\n",
    );
    let values: Vec<(&str, u32)> = (0..4)
        .map(|c| ("%wv", c))
        .chain((0..4).map(|c| ("%bv", c)))
        .collect();
    floats_out(&mut body, &values);
    let text = module(
        "%white %black",
        "OpDecorate %white DescriptorSet 0\nOpDecorate %white Binding 1\nOpDecorate %black DescriptorSet 0\nOpDecorate %black Binding 2",
        BORDER_DECLS,
        &body,
        1,
    );
    let kind =
        BindingKind::CombinedImageSampler(shape(Component::Float, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[
            Bound(kind, alloc::vec![Resource::ImageSampler(&texels, &white)]),
            Bound(kind, alloc::vec![Resource::ImageSampler(&texels, &black)]),
        ],
        &[],
        8,
    );
    assert_eq!(out[..4], [1.0, 0.0, 0.0, 1.0].map(f), "white in red alone");
    assert_eq!(
        out[4..],
        [0.0, 0.0, 0.0, 1.0].map(f),
        "black, and the alpha a texel lacks"
    );
}

const STENCIL_DECLS: &str = r"%image_t = OpTypeImage %uint 2D 0 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%stencil = OpVariable %si_ptr UniformConstant
%fhalf = OpConstant %float 0.5
%fminus = OpConstant %float -0.5
%outside = OpConstantComposite %v2float %fminus %fhalf";

#[test]
fn a_stencil_view_s_border_reads_as_integers() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, picture) = filled(
        &context,
        &square(Format::Depth32FloatStencil8, ImageUsage::SAMPLED, 1),
        Aspects::STENCIL,
        |_, _, _, _| 7,
    );
    let stencil = view(
        &context,
        &picture,
        &plain(Format::Depth32FloatStencil8, Aspects::STENCIL, 1),
    );
    let white = context
        .create_sampler(&bordered(BorderColor::OpaqueWhite))
        .expect("a sampler");
    let mut body = String::from(
        "%s = OpLoad %si_t %stencil\n%v = OpImageSampleExplicitLod %v4uint %s %outside Lod %fzero\n",
    );
    let mut names = Vec::new();
    for c in 0..4 {
        let _ = writeln!(body, "%s{c} = OpCompositeExtract %uint %v {c}");
        names.push(alloc::format!("%s{c}"));
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    body.push_str(&stores(&names, false));
    let text = module(
        "%stencil",
        "OpDecorate %stencil DescriptorSet 0\nOpDecorate %stencil Binding 1",
        STENCIL_DECLS,
        &body,
        1,
    );
    let kind =
        BindingKind::CombinedImageSampler(shape(Component::Uint, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&stencil, &white)],
        )],
        &[],
        4,
    );
    assert_eq!(out, [1, 0, 0, 1], "a one, not a float's bits");
}

const REFERENCE_DECLS: &str = r"%image_t = OpTypeImage %float 2D 1 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%greater = OpVariable %si_ptr UniformConstant
%less = OpVariable %si_ptr UniformConstant
%fhalf = OpConstant %float 0.5
%centre = OpConstantComposite %v2float %fhalf %fhalf
%above = OpConstant %float 1.5
%under = OpConstant %float -0.5";

/// A depth reference above one compared Greater against a texel of one,
/// and one below zero compared Less against a texel of zero, through a
/// depth view of `format`.
fn references(format: Format) -> Vec<u32> {
    let (device, _) = device();
    let context = context(&device);
    let fill = |value: f32| {
        let bits = if format == Format::Depth16Unorm {
            if value > 0.0 {
                0xffff
            } else {
                0
            }
        } else {
            f(value)
        };
        let (held, picture) = filled(
            &context,
            &square(format, ImageUsage::SAMPLED, 1),
            Aspects::DEPTH,
            move |_, _, _, _| bits,
        );
        let depth = view(&context, &picture, &plain(format, Aspects::DEPTH, 1));
        (held, picture, depth)
    };
    let (_one_memory, _one, one) = fill(1.0);
    let (_zero_memory, _zero, zero) = fill(0.0);
    let compare = |op| {
        context
            .create_sampler(&SamplerDesc {
                compare: Some(op),
                ..nearest()
            })
            .expect("a sampler")
    };
    let (greater, less) = (compare(CompareOp::Greater), compare(CompareOp::Less));
    let mut body = String::from(
        "%g = OpLoad %si_t %greater\n%l = OpLoad %si_t %less\n%gv = OpImageSampleDrefExplicitLod %float %g %centre %above Lod %fzero\n%lv = OpImageSampleDrefExplicitLod %float %l %centre %under Lod %fzero\n%gu = OpBitcast %uint %gv\n%lu = OpBitcast %uint %lv\n",
    );
    body.push_str(&stores(&["%gu", "%lu"], false));
    let text = module(
        "%greater %less",
        "OpDecorate %greater DescriptorSet 0\nOpDecorate %greater Binding 1\nOpDecorate %less DescriptorSet 0\nOpDecorate %less Binding 2",
        REFERENCE_DECLS,
        &body,
        1,
    );
    let kind =
        BindingKind::CombinedImageSampler(shape(Component::Float, ImageFormat::UNKNOWN, false));
    dispatch(
        &context,
        &text,
        &[
            Bound(kind, alloc::vec![Resource::ImageSampler(&one, &greater)]),
            Bound(kind, alloc::vec![Resource::ImageSampler(&zero, &less)]),
        ],
        &[],
        2,
    )
}

#[test]
fn a_depth_reference_is_held_to_the_unit_range_only_for_a_normalized_format() {
    assert_eq!(
        references(Format::Depth32Float),
        [f(1.0), f(1.0)],
        "a float format compares the reference as given"
    );
    assert_eq!(
        references(Format::Depth16Unorm),
        [f(0.0), f(0.0)],
        "a normalized one holds it to [0, 1] first"
    );
}

const GRADIENT_DECLS: &str = r"%cube_t = OpTypeImage %float Cube 0 0 0 1 Unknown
%cube_si = OpTypeSampledImage %cube_t
%cube_ptr = OpTypePointer UniformConstant %cube_si
%cube = OpVariable %cube_ptr UniformConstant
%f1 = OpConstant %float 1.0
%fhalf = OpConstant %float 0.5
%along_x = OpConstant %float 1.0
%direction = OpConstantComposite %v3float %along_x %fzero %fzero
%across = OpConstantComposite %v3float %fzero %fzero %fhalf
%down = OpConstantComposite %v3float %fzero %fhalf %fzero
%outward = OpConstantComposite %v3float %fhalf %fzero %fzero
%still = OpConstantComposite %v3float %fzero %fzero %fzero";

#[test]
fn a_cube_s_level_of_detail_follows_its_gradients_across_the_face() {
    let (device, _) = device();
    let context = context(&device);
    let desc = ImageDesc {
        extent: Extent3d {
            width: 64,
            height: 64,
            depth: 1,
        },
        array_layers: 6,
        ..square(Format::R32Float, ImageUsage::SAMPLED, 7)
    };
    let (_memory, picture) = filled(&context, &desc, Aspects::COLOR, |level, _, _, _| {
        f(small(level))
    });
    let faces = view(
        &context,
        &picture,
        &ImageViewDesc {
            format: Format::R32Float,
            dimension: ViewDimension::Cube,
            range: ImageRange {
                aspects: Aspects::COLOR,
                base_level: 0,
                levels: 7,
                base_layer: 0,
                layers: 6,
            },
        },
    );
    let levels = context
        .create_sampler(&SamplerDesc {
            mipmap_filter: Filter::Nearest,
            ..nearest()
        })
        .expect("a sampler");
    let mut body = String::from(
        "%c = OpLoad %cube_si %cube\n%s = OpImageSampleExplicitLod %v4float %c %direction Grad %across %still\n%t = OpImageSampleExplicitLod %v4float %c %direction Grad %down %still\n%m = OpImageSampleExplicitLod %v4float %c %direction Grad %outward %still\n",
    );
    floats_out(&mut body, &[("%s", 0), ("%t", 0), ("%m", 0)]);
    let text = module(
        "%cube",
        "OpDecorate %cube DescriptorSet 0\nOpDecorate %cube Binding 1",
        GRADIENT_DECLS,
        &body,
        1,
    );
    let kind = BindingKind::CombinedImageSampler(ImageShape {
        dim: Dim::CUBE,
        ..shape(Component::Float, ImageFormat::UNKNOWN, false)
    });
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&faces, &levels)],
        )],
        &[],
        3,
    );
    assert_eq!(
        out,
        [f(4.0), f(4.0), f(0.0)],
        "half a unit across the +X face is a quarter of it, 16 of 64 texels: \
         level 4 along s and along t; along the major axis it does not move"
    );
}

const EDGE_DECLS: &str = r"%cube_t = OpTypeImage %float Cube 0 0 0 1 Unknown
%cube_si = OpTypeSampledImage %cube_t
%cube_ptr = OpTypePointer UniformConstant %cube_si
%cube = OpVariable %cube_ptr UniformConstant
%f1 = OpConstant %float 1.0
%fminus = OpConstant %float -1.0
%edge = OpConstantComposite %v3float %f1 %fzero %fminus";

#[test]
fn a_nearest_cube_sample_on_a_face_s_edge_reads_that_face() {
    let (device, _) = device();
    let context = context(&device);
    let desc = ImageDesc {
        array_layers: 6,
        ..square(Format::R32Float, ImageUsage::SAMPLED, 1)
    };
    let (_memory, picture) = filled(&context, &desc, Aspects::COLOR, |_, face, _, _| {
        f(small(face))
    });
    let faces = view(
        &context,
        &picture,
        &ImageViewDesc {
            format: Format::R32Float,
            dimension: ViewDimension::Cube,
            range: ImageRange {
                aspects: Aspects::COLOR,
                base_level: 0,
                levels: 1,
                base_layer: 0,
                layers: 6,
            },
        },
    );
    let point = context.create_sampler(&nearest()).expect("a sampler");
    let mut body = String::from(
        "%c = OpLoad %cube_si %cube\n%v = OpImageSampleExplicitLod %v4float %c %edge Lod %fzero\n",
    );
    floats_out(&mut body, &[("%v", 0)]);
    let text = module(
        "%cube",
        "OpDecorate %cube DescriptorSet 0\nOpDecorate %cube Binding 1",
        EDGE_DECLS,
        &body,
        1,
    );
    let kind = BindingKind::CombinedImageSampler(ImageShape {
        dim: Dim::CUBE,
        ..shape(Component::Float, ImageFormat::UNKNOWN, false)
    });
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&faces, &point)],
        )],
        &[],
        1,
    );
    assert_eq!(
        out,
        [f(0.0)],
        "+X's last column, at s of exactly one, not the -Z face beside it"
    );
}

const FILTER_DECLS: &str = r"%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%combined = OpVariable %si_ptr UniformConstant
%eighth = OpConstant %float 0.125
%first_centre = OpConstantComposite %v2float %eighth %eighth";

#[test]
fn a_filter_weighing_an_infinite_texel_by_nothing_makes_the_one_quiet_nan() {
    let (device, _) = device();
    let context = context(&device);
    let (_memory, picture) = filled(
        &context,
        &square(Format::R32Float, ImageUsage::SAMPLED, 1),
        Aspects::COLOR,
        |_, _, x, y| {
            if (x, y) == (1, 0) {
                f(f32::INFINITY)
            } else {
                f(1.0)
            }
        },
    );
    let texels = view(
        &context,
        &picture,
        &plain(Format::R32Float, Aspects::COLOR, 1),
    );
    let linear = context
        .create_sampler(&SamplerDesc::LINEAR)
        .expect("a sampler");
    let mut body = String::from(
        "%c = OpLoad %si_t %combined\n%v = OpImageSampleExplicitLod %v4float %c %first_centre Lod %fzero\n",
    );
    floats_out(&mut body, &[("%v", 0)]);
    let text = module(
        "%combined",
        "OpDecorate %combined DescriptorSet 0\nOpDecorate %combined Binding 1",
        FILTER_DECLS,
        &body,
        1,
    );
    let kind =
        BindingKind::CombinedImageSampler(shape(Component::Float, ImageFormat::UNKNOWN, false));
    let out = dispatch(
        &context,
        &text,
        &[Bound(
            kind,
            alloc::vec![Resource::ImageSampler(&texels, &linear)],
        )],
        &[],
        1,
    );
    assert_eq!(
        out[0], 0x7fc0_0000,
        "nothing times infinity, rounded to the same NaN on every host"
    );
}
