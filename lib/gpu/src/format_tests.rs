use alloc::vec::Vec;

use tairix_spirv::enumerant::ImageFormat;
use tairix_spirv::Component;

use super::{Aspects, Format, FormatFeatures};

#[test]
fn every_format_is_listed_once() {
    for (at, format) in Format::ALL.iter().enumerate() {
        assert!(
            !Format::ALL[..at].contains(format),
            "{format:?} is listed twice"
        );
    }
}

#[test]
fn features_imply_what_they_build_on() {
    for format in Format::ALL {
        let features = format.software_features();
        let implies = |feature, needed| {
            assert!(
                !features.contains(feature) || features.contains(needed),
                "{format:?} states {feature:?} without {needed:?}"
            );
        };
        implies(FormatFeatures::FILTERED, FormatFeatures::SAMPLED);
        assert!(
            features.contains(FormatFeatures::SAMPLED),
            "{format:?} cannot be sampled"
        );
        let depth_stencil = format.aspects() != Aspects::COLOR;
        assert_eq!(
            features.contains(FormatFeatures::DEPTH_STENCIL),
            depth_stencil,
            "{format:?}"
        );
        if depth_stencil {
            assert!(!features.contains(FormatFeatures::COLOR_ATTACHMENT));
            assert!(!features.contains(FormatFeatures::STORAGE));
        }
        if format.component() != Component::Float {
            assert!(
                !features.contains(FormatFeatures::FILTERED),
                "{format:?} filters integers"
            );
        }
    }
}

#[test]
fn a_storage_format_names_one_format_and_one_module_format() {
    let mut named: Vec<ImageFormat> = Vec::new();
    for format in Format::ALL {
        let stored = format.software_features().contains(FormatFeatures::STORAGE);
        assert_eq!(format.storage_format().is_some(), stored, "{format:?}");
        if let Some(module) = format.storage_format() {
            assert!(!named.contains(&module), "{module:?} names two formats");
            named.push(module);
        }
    }
    assert_eq!(
        named.len(),
        39,
        "every storage format a module may declare but the 64-bit ones"
    );
}

#[test]
fn a_copy_moves_one_aspect_at_a_time() {
    assert_eq!(Format::Rgba8Unorm.copy_bytes(Aspects::COLOR), Some(4));
    assert_eq!(Format::Rgba8Unorm.copy_bytes(Aspects::DEPTH), None);
    assert_eq!(Format::Rgba32Float.copy_bytes(Aspects::COLOR), Some(16));
    assert_eq!(Format::Rg11B10Float.copy_bytes(Aspects::COLOR), Some(4));
    assert_eq!(
        Format::Depth32FloatStencil8.copy_bytes(Aspects::DEPTH),
        Some(4)
    );
    assert_eq!(
        Format::Depth32FloatStencil8.copy_bytes(Aspects::STENCIL),
        Some(1)
    );
    assert_eq!(
        Format::Depth32FloatStencil8.copy_bytes(Aspects::DEPTH_STENCIL),
        None
    );
    assert_eq!(Format::Depth16Unorm.copy_bytes(Aspects::DEPTH), Some(2));
    assert_eq!(Format::Stencil8.copy_bytes(Aspects::STENCIL), Some(1));
    for format in Format::ALL {
        assert!(format.texel_bytes().is_power_of_two(), "{format:?}");
    }
}
