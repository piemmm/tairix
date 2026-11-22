//! A backend of its own: memory of its own, and an image layout that is not
//! the software device's — padded, aligned wider and placed only in the
//! first memory type, as a hardware backend's may be. The model is generic
//! over it as over the software device, so what it refuses and admits over
//! one it does over the other.

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_rt::sync::Mutex;

use super::{Backend, Pool, Prepared};
use crate::binding::Bound;
use crate::command::Op;
use crate::device::Shared;
use crate::fair::FairGuard;
use crate::image::ImageDesc;
use crate::info::DeviceInfo;
use crate::memory::{Budget, Charge};
use crate::model::Model;
use crate::pipeline::StagePlan;
use crate::render::RenderPlan;
use crate::resource::MemoryRequirements;
use crate::Error;

/// The test backend. Its jobs are queued and never run: what it is held to
/// is what the model checks.
pub(crate) struct Test;

/// What an image of its own layout takes past the software device's: its
/// texels padded out to whole pages, and a page more.
pub(crate) const PAGE: u64 = 4096;

/// An allocation's bytes, and the charges that pay for them.
pub(crate) struct Bytes {
    bytes: Mutex<Vec<u8>>,
    _charges: [Charge; 2],
}

impl core::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bytes").finish_non_exhaustive()
    }
}

/// An image as the test backend lays it out: all it needs is its
/// requirements, which the model keeps.
#[derive(Debug)]
pub(crate) struct Paged;

/// The pool of a backend that keeps none.
pub(crate) struct NoPool;

impl Pool for NoPool {
    type Need = ();
    type Hold = ();
    type Plan = ();
    type Grown = ();
    type Lease = ();

    fn wider((): (), (): ()) {}

    fn prune(&mut self) {}

    fn plan(&mut self, (): (), _: (&Arc<Budget>, u64), _: &Arc<Budget>) -> (Result<(), Error>, ()) {
        (Ok(()), ())
    }

    fn make((): ()) -> Result<((), ()), Error> {
        Ok(((), ()))
    }

    fn install(&mut self, (): ()) {}

    fn take(&mut self, (): ()) {}

    fn give(&mut self, (): ()) -> [(); 2] {
        [(), ()]
    }
}

impl Backend for Test {
    type Memory = Bytes;
    type Image = Paged;
    type Set = ();
    type Compute = ();
    type Render = ();
    type Pool = NoPool;

    fn overhead(_: u64) -> u64 {
        crate::model::shared_bytes::<Bytes>()
    }

    fn allocate(&self, size: u64, charges: [Charge; 2]) -> Result<Bytes, Error> {
        let len = usize::try_from(size).map_err(|_| Error::OutOfMemory)?;
        let bytes = tairix_util::fallible::filled(len, 0u8).ok_or(Error::OutOfMemory)?;
        Ok(Bytes {
            bytes: Mutex::new(bytes),
            _charges: charges,
        })
    }

    fn write(&self, memory: &Bytes, offset: u64, data: &[u8]) -> Result<(), Error> {
        let start = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        let mut bytes = memory.bytes.lock();
        bytes
            .get_mut(start..start.saturating_add(data.len()))
            .ok_or(Error::OutOfRange)?
            .copy_from_slice(data);
        Ok(())
    }

    fn read(&self, memory: &Bytes, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let start = usize::try_from(offset).map_err(|_| Error::OutOfRange)?;
        let bytes = memory.bytes.lock();
        out.copy_from_slice(
            bytes
                .get(start..start.saturating_add(out.len()))
                .ok_or(Error::OutOfRange)?,
        );
        Ok(())
    }

    fn image(
        &self,
        _: &DeviceInfo,
        desc: &ImageDesc,
    ) -> Result<(Paged, MemoryRequirements), Error> {
        let texels = crate::software::layout::ImageLayout::of(desc)?.size;
        let size = texels.div_ceil(PAGE).saturating_add(1).saturating_mul(PAGE);
        let requirements = MemoryRequirements {
            size,
            alignment: PAGE,
            memory_types: 1,
        };
        Ok((Paged, requirements))
    }

    fn set(&self, _: &[Bound<Self>]) -> Result<((), u64), Error> {
        Ok(((), 0))
    }

    fn compute(&self, _: &DeviceInfo, _: &StagePlan<'_>) -> Result<Prepared<Self, ()>, Error> {
        Ok(Prepared {
            form: (),
            need: (),
            held: 0,
        })
    }

    fn render(&self, _: &DeviceInfo, _: &RenderPlan<'_>) -> Result<Prepared<Self, ()>, Error> {
        Ok(Prepared {
            form: (),
            need: (),
            held: 0,
        })
    }

    fn run_bytes(_: &NoPool, _: &[Op<Self>]) -> u64 {
        0
    }

    fn drive(_: &Shared<Self>, _: FairGuard<'_, Model<Self>>) {}
}

/// The software device's description with a second memory type, device-local
/// alone, on the same heap.
fn two_types() -> DeviceInfo {
    let mut info = crate::software::info(crate::device_tests::RAM).expect("the software device");
    let heap = info
        .memory_types
        .first()
        .map(|kind| kind.heap)
        .expect("a type");
    let _ = info.memory_types.try_push(crate::info::MemoryType {
        heap,
        properties: crate::info::MemoryProperties::DEVICE_LOCAL,
    });
    info
}

fn software(info: &DeviceInfo) -> crate::Device {
    let clock: Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync> =
        Arc::new(crate::device_tests::Clock::default());
    crate::Device::software_stating(
        info.clone(),
        clock,
        Arc::new(tairix_parallel::Serial),
        &crate::NoThreads,
    )
    .expect("the software device")
}

fn test(info: &DeviceInfo) -> Shared<Test> {
    Shared {
        model: crate::fair::Fair::new(Model::new(info.clone(), &[1], NoPool).expect("a model")),
        info: info.clone(),
        clock: Arc::new(crate::device_tests::Clock::default()),
        backend: Test,
    }
}

/// `size` bytes of memory type `memory_type` for the context.
fn allocation<B: Backend>(
    shared: &Shared<B>,
    context: crate::model::ContextKey,
    (size, memory_type): (u64, u32),
) -> crate::model::AllocationKey {
    let (charge, overhead) = shared
        .model
        .lock()
        .charge_allocation(context, size, memory_type)
        .expect("charged");
    let memory = shared
        .backend
        .allocate(size, [charge, overhead])
        .and_then(crate::shared)
        .expect("made");
    shared
        .model
        .lock()
        .file_allocation(context, memory, memory_type, size)
        .map_err(|(error, _)| error)
        .expect("filed")
}

/// An 8×8 image of two levels and two layers, sampled and rendered to.
const fn layered() -> ImageDesc {
    ImageDesc {
        dimension: crate::ImageDimension::D2,
        format: crate::Format::Rgba8Unorm,
        extent: crate::Extent3d {
            width: 8,
            height: 8,
            depth: 1,
        },
        mip_levels: 2,
        array_layers: 2,
        samples: 1,
        usage: crate::ImageUsage::SAMPLED.with(crate::ImageUsage::COLOR_ATTACHMENT),
    }
}

/// Where an image may be placed on `shared`: in the second memory type, off
/// its alignment, and holding just less than it needs.
fn placements<B: Backend>(shared: &Shared<B>) -> [Option<Error>; 3] {
    let context = shared.model.lock().create_context(None).expect("a context");
    let needs = shared
        .image_form(&layered())
        .map(|(_, needs)| needs)
        .expect("requirements");
    let room = needs.size.saturating_mul(4);
    let first = allocation(shared, context, (room, 0));
    let second = allocation(shared, context, (room, 1));
    let short = allocation(shared, context, (needs.size + needs.alignment - 1, 0));
    let refused =
        |at: (crate::model::AllocationKey, u64)| shared.create_image(context, at, &layered()).err();
    assert_eq!(refused((first, 0)), None, "placed where it is asked");
    [
        refused((second, 0)),
        refused((first, needs.alignment / 2)),
        refused((short, needs.alignment)),
    ]
}

#[test]
fn an_image_is_placed_as_its_backend_requires() {
    let info = two_types();
    let device = software(&info);
    assert_eq!(
        placements(&device.shared),
        [None, Some(Error::Misaligned), Some(Error::OutOfRange)],
        "the software device places an image in either type, at its texels' alignment"
    );
    assert_eq!(
        placements(&test(&info)),
        [
            Some(Error::Usage),
            Some(Error::Misaligned),
            Some(Error::OutOfRange)
        ],
        "the test backend places one only in the first type, a page apart, padded"
    );
}

/// The attachment views of an image, an image placed over it and one past
/// it, and whether a sampled view of each, seen by the fragment stage,
/// reaches the first image's level 0, layer 1.
fn reaches<B: Backend>(shared: &Shared<B>) -> Vec<bool> {
    use crate::binding::{Named, SetRequest};
    let context = shared.model.lock().create_context(None).expect("a context");
    let needs = shared
        .image_form(&layered())
        .map(|(_, needs)| needs)
        .expect("requirements");
    let past = needs.size.next_multiple_of(needs.alignment);
    let memory = allocation(shared, context, (past + needs.size, 0));
    let image = |offset| {
        shared
            .create_image(context, (memory, offset), &layered())
            .expect("an image")
    };
    let (rendered, aliasing, apart) = (image(0), image(0), image(past));
    let view = |image, level, layer| {
        shared
            .model
            .lock()
            .create_view(
                context,
                image,
                &crate::ImageViewDesc {
                    format: crate::Format::Rgba8Unorm,
                    dimension: crate::ViewDimension::D2,
                    range: crate::ImageRange {
                        aspects: crate::Aspects::COLOR,
                        base_level: level,
                        levels: 1,
                        base_layer: layer,
                        layers: 1,
                    },
                },
            )
            .expect("a view")
    };
    let target = view(rendered, 0, 1);
    let tables = crate::binding::LayoutTables::of(
        &shared.info,
        &[crate::LayoutBinding {
            binding: 0,
            kind: tairix_spirv::BindingKind::SampledImage(tairix_spirv::ImageShape {
                dim: tairix_spirv::enumerant::Dim::DIM_2D,
                arrayed: false,
                multisampled: false,
                component: tairix_spirv::Component::Float,
                format: tairix_spirv::enumerant::ImageFormat::UNKNOWN,
            }),
            count: 1,
            access: tairix_spirv::Access::READ,
            stages: tairix_spirv::Stages::FRAGMENT,
        }],
    )
    .expect("tables");
    let layout = shared
        .model
        .lock()
        .create_bind_set_layout(context, tables)
        .expect("a set layout");
    let sampled = [
        view(rendered, 0, 1),
        view(rendered, 1, 1),
        view(rendered, 0, 0),
        view(aliasing, 1, 0),
        view(apart, 0, 1),
    ];
    sampled
        .iter()
        .map(|&sampled| {
            let set = shared
                .create_bind_set(
                    context,
                    &SetRequest {
                        layout,
                        entries: &[(0, 1)],
                        resources: &[Named::Image(sampled)],
                    },
                )
                .expect("a set");
            let model = shared.model.lock();
            let live = model.live(context).expect("live");
            let touched = crate::pass::touched(live.views.get(target).expect("the target"));
            live.bind_sets
                .get(set)
                .expect("the set")
                .reach
                .meets(&touched, false)
        })
        .collect()
}

#[test]
fn what_a_binding_reaches_is_the_same_whatever_the_backend_lays_out() {
    let info = two_types();
    let device = software(&info);
    let expected = alloc::vec![true, false, false, true, false];
    assert_eq!(
        reaches(&device.shared),
        expected,
        "over the software device"
    );
    assert_eq!(reaches(&test(&info)), expected, "over the test backend");
}
