//! Every refusal and every binding description round-trips, and decoding
//! refuses one cut short or of an unknown kind rather than panicking.

use alloc::vec::Vec;
use core::fmt::Debug;

use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Access, Component, Disagreement, ImageShape, SpecError, Stages};

use super::{put_error, take_error, Reader, Wire, Writer};
use crate::{DescriptorType, Error, LayoutBinding, Loss, SampleType, SamplerType};

/// `value` written, read back whole, and refused from every shorter prefix.
fn round_trips<T: Wire + PartialEq + Debug + Copy>(value: T) {
    let mut bytes = Vec::new();
    value.put(&mut Writer::new(&mut bytes)).expect("written");
    let mut reader = Reader::new(&bytes);
    assert_eq!(T::take(&mut reader), Ok(value));
    reader.finish().expect("nothing left");
    for cut in 0..bytes.len() {
        let mut short = Reader::new(&bytes[..cut]);
        let read = T::take(&mut short).and_then(|_| short.finish());
        assert!(read.is_err(), "{value:?} cut to {cut} bytes");
    }
}

const SHAPE: ImageShape = ImageShape {
    dim: Dim::DIM_2D,
    arrayed: true,
    multisampled: false,
    component: Component::Uint,
    format: ImageFormat::R32UI,
};

const SAMPLES: [SampleType; 6] = [
    SampleType::Float,
    SampleType::UnfilterableFloat,
    SampleType::Depth,
    SampleType::UnfilterableDepth,
    SampleType::Sint,
    SampleType::Uint,
];

const SAMPLERS: [SamplerType; 6] = [
    SamplerType::Filtering,
    SamplerType::NonFiltering,
    SamplerType::Integer,
    SamplerType::Comparison,
    SamplerType::NonFilteringComparison,
    SamplerType::Unnormalized,
];

#[test]
fn every_descriptor_type_round_trips_and_an_unknown_one_is_refused() {
    for sampler in SAMPLERS {
        round_trips(DescriptorType::Sampler(sampler));
        for sample in SAMPLES {
            round_trips(DescriptorType::CombinedImageSampler(SHAPE, sample, sampler));
        }
    }
    for sample in SAMPLES {
        round_trips(DescriptorType::SampledImage(SHAPE, sample));
    }
    for kind in [
        DescriptorType::StorageImage(SHAPE),
        DescriptorType::UniformTexelBuffer(SHAPE),
        DescriptorType::StorageTexelBuffer(SHAPE),
        DescriptorType::UniformBuffer,
        DescriptorType::StorageBuffer,
    ] {
        round_trips(kind);
    }
    round_trips(LayoutBinding {
        binding: 7,
        kind: DescriptorType::CombinedImageSampler(SHAPE, SampleType::Uint, SamplerType::Integer),
        count: 3,
        access: Access::READ,
        stages: Stages::FRAGMENT.with(Stages::COMPUTE),
    });
    for unknown in [[8u8, 0], [0, 6]] {
        assert_eq!(
            DescriptorType::take(&mut Reader::new(&unknown)),
            Err(Error::Malformed)
        );
    }
    let mut unknown_sample = alloc::vec![1u8];
    SHAPE
        .put(&mut Writer::new(&mut unknown_sample))
        .expect("written");
    unknown_sample.push(6);
    assert_eq!(
        DescriptorType::take(&mut Reader::new(&unknown_sample)),
        Err(Error::Malformed)
    );
}

#[test]
fn every_refusal_crosses_with_its_detail() {
    let losses = [Loss::Overran, Loss::Reset, Loss::Fault, Loss::OutOfMemory];
    let disagreements = [
        Disagreement::Unordered,
        Disagreement::Missing(1, 2),
        Disagreement::Kind(3, 4),
        Disagreement::Count(5, 6),
        Disagreement::Access(7, 8),
        Disagreement::Stage(9, 10),
        Disagreement::PushConstants,
    ];
    let specializations = [
        SpecError::UnknownId(1),
        SpecError::Duplicate(2),
        SpecError::Width(3),
        SpecError::Workgroup,
        SpecError::Budget,
        SpecError::OutOfMemory,
        SpecError::Malformed,
    ];
    let plain = [
        Error::Destroyed,
        Error::Foreign,
        Error::Stale,
        Error::OutOfMemory,
        Error::OverLimit,
        Error::HeapFull,
        Error::NoSuchMemoryType,
        Error::NoSuchQueue,
        Error::TooLarge,
        Error::Empty,
        Error::Misaligned,
        Error::OutOfRange,
        Error::Usage,
        Error::NotHostVisible,
        Error::Overlap,
        Error::TimelineOrder,
        Error::Deadline,
        Error::TooMany,
        Error::TimedOut,
        Error::Malformed,
        Error::Shape,
        Error::Samples,
        Error::Aspect,
        Error::Unsupported,
        Error::Bindings,
        Error::EntryPoint,
        Error::Interface,
        Error::Sampling((1, 2), (3, 4)),
    ];
    let every = losses
        .map(Error::Lost)
        .into_iter()
        .chain(disagreements.map(Error::Disagreement))
        .chain(specializations.map(Error::Specialization))
        .chain(plain);
    for error in every {
        let mut detail = Vec::new();
        let code = put_error(&mut Writer::new(&mut detail), error).expect("written");
        let mut reader = Reader::new(&detail);
        assert_eq!(take_error(code, &mut reader), Ok(error), "{error:?}");
        reader.finish().expect("nothing left");
    }
}
