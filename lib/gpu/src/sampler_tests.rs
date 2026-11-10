use super::{CompareOp, Filter, SamplerDesc, MAX_LOD_BIAS};
use crate::Error;

fn device() -> crate::DeviceInfo {
    crate::software::info(1 << 21).expect("a device")
}

#[test]
fn a_linear_sampler_is_made() {
    assert_eq!(SamplerDesc::LINEAR.check(&device()), Ok(()));
}

#[test]
fn level_of_detail_clamps_and_bias_are_held_to_their_range() {
    let reversed = SamplerDesc {
        lod_min: 2.0,
        lod_max: 1.0,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(reversed.check(&device()), Err(Error::OutOfRange));
    let negative = SamplerDesc {
        lod_min: -1.0,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(negative.check(&device()), Err(Error::OutOfRange));
    let infinite = SamplerDesc {
        lod_max: f32::INFINITY,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(infinite.check(&device()), Err(Error::OutOfRange));
    let biased = SamplerDesc {
        lod_bias: MAX_LOD_BIAS + 1.0,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(biased.check(&device()), Err(Error::OutOfRange));
}

#[test]
fn anisotropy_needs_linear_filtering_and_stays_within_the_device() {
    let anisotropic = SamplerDesc {
        max_anisotropy: 16,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(anisotropic.check(&device()), Ok(()));
    let past = SamplerDesc {
        max_anisotropy: 17,
        ..anisotropic
    };
    assert_eq!(past.check(&device()), Err(Error::OutOfRange));
    let nearest = SamplerDesc {
        mipmap_filter: Filter::Nearest,
        ..anisotropic
    };
    assert_eq!(nearest.check(&device()), Err(Error::Unsupported));
    let none = SamplerDesc {
        max_anisotropy: 0,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(none.check(&device()), Err(Error::OutOfRange));
}

#[test]
fn unnormalised_coordinates_take_their_restricted_form_only() {
    let texels = SamplerDesc {
        mag_filter: Filter::Nearest,
        min_filter: Filter::Nearest,
        mipmap_filter: Filter::Nearest,
        lod_max: 0.0,
        unnormalized: true,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(texels.check(&device()), Ok(()));
    let mipped = SamplerDesc {
        lod_max: 1.0,
        ..texels
    };
    assert_eq!(mipped.check(&device()), Err(Error::Unsupported));
    let wrapped = SamplerDesc {
        address: [super::AddressMode::Repeat; 3],
        ..texels
    };
    assert_eq!(wrapped.check(&device()), Err(Error::Unsupported));
    let compared = SamplerDesc {
        compare: Some(CompareOp::Less),
        ..texels
    };
    assert_eq!(compared.check(&device()), Err(Error::Unsupported));
}

#[test]
fn an_unstated_feature_is_refused() {
    let mut plain = device();
    plain.features = crate::Features::NONE;
    let anisotropic = SamplerDesc {
        max_anisotropy: 4,
        ..SamplerDesc::LINEAR
    };
    assert_eq!(anisotropic.check(&plain), Err(Error::Unsupported));
    let mirrored = SamplerDesc {
        address: [super::AddressMode::MirrorClampToEdge; 3],
        ..SamplerDesc::LINEAR
    };
    assert_eq!(mirrored.check(&plain), Err(Error::Unsupported));
    assert_eq!(mirrored.check(&device()), Ok(()));
}
