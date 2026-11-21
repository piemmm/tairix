//! The software device every conformance case runs a committed pass on, and
//! the pictures and words the cases are drawn from.

use std::sync::Arc;

use tairix_abi::time::{Duration64, MonotonicClock};
use tairix_fuzzseed::Prng;
use tairix_gpu::{
    AllocationDesc, BindSetEntry, BufferDesc, BufferUsage, ComputePipelineDesc, Context,
    ContextDesc, Device, LayoutBinding, NoThreads, PushConstants, Resource, SoftwareDesc,
    Submission, DEFAULT_SUBGROUP_SIZE,
};
use tairix_parallel::Serial;
use tairix_raster::{Pixel, Surface};
use tairix_spirv::{Admitted, Module, Stages};

/// A clock that never moves, so no deadline passes however slowly the
/// interpreter runs.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// The software device and the context every pass runs in.
pub(crate) struct Gpu {
    device: Device,
    context: Context,
}

impl Gpu {
    /// The longest the device lets a submission run, and how long a wait
    /// for one waits: the clock never moves, so neither is reached.
    fn longest(&self) -> Duration64 {
        self.device.info().limits.max_job_time
    }
}

pub(crate) fn gpu() -> Gpu {
    let device = Device::software(&SoftwareDesc {
        memory_bytes: 1 << 30,
        clock: Arc::new(Still),
        runner: Arc::new(Serial),
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device");
    let context = device
        .create_context(&ContextDesc::default())
        .expect("a context");
    Gpu { device, context }
}

/// A pipeline of the committed pass `spirv` with `push` bytes of push
/// constants, and the layout of the bindings its entry point needs.
struct Made {
    pipeline: tairix_gpu::ComputePipeline,
    set_layout: tairix_gpu::BindSetLayout,
    bindings: Vec<LayoutBinding>,
}

fn made(gpu: &Gpu, spirv: &[u8], push: u32) -> Made {
    let context = &gpu.context;
    let admitted = Admitted::new(Module::decode(spirv).expect("decoded")).expect("admitted");
    let entry = admitted
        .entry_points()
        .first()
        .expect("an entry point")
        .clone();
    let bindings: Vec<LayoutBinding> = entry
        .bindings()
        .iter()
        .map(|binding| LayoutBinding {
            binding: binding.binding,
            kind: binding.kind,
            count: 1,
            access: binding.access,
            stages: Stages::COMPUTE,
        })
        .collect();
    let module = context.create_shader_module(admitted).expect("a module");
    let set_layout = context
        .create_bind_set_layout(&bindings)
        .expect("a set layout");
    let layout = context
        .create_pipeline_layout(
            &[&set_layout],
            PushConstants {
                bytes: push,
                stages: Stages::COMPUTE,
            },
        )
        .expect("a pipeline layout");
    let pipeline = context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    Made {
        pipeline,
        set_layout,
        bindings,
    }
}

/// Run the committed pass `spirv` over `buffers`, the one at each index bound
/// there, with `push` as its push constants and `dispatched` workgroups:
/// what each buffer holds afterwards.
pub(crate) fn run(
    gpu: &Gpu,
    spirv: &[u8],
    push: &[u32],
    buffers: &[&[u32]],
    dispatched: [u32; 3],
) -> Vec<Vec<u32>> {
    let context = &gpu.context;
    let Made {
        pipeline,
        set_layout,
        bindings,
    } = made(
        gpu,
        spirv,
        u32::try_from(push.len() * 4).expect("a few words"),
    );
    let held: Vec<_> = buffers
        .iter()
        .map(|words| {
            let size = u64::try_from(words.len().max(1) * 4).expect("small");
            let memory = context
                .allocate(&AllocationDesc {
                    size,
                    memory_type: 0,
                })
                .expect("memory");
            let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
            memory.write(0, &bytes).expect("written");
            let buffer = context
                .create_buffer(
                    &memory,
                    &BufferDesc {
                        offset: 0,
                        size,
                        usage: BufferUsage::ALL,
                    },
                )
                .expect("a buffer");
            (memory, buffer, size)
        })
        .collect();
    let resources: Vec<[Resource<'_>; 1]> = held
        .iter()
        .map(|(_, buffer, size)| {
            [Resource::Buffer {
                buffer,
                offset: 0,
                size: *size,
            }]
        })
        .collect();
    let entries: Vec<BindSetEntry<'_>> = bindings
        .iter()
        .map(|binding| BindSetEntry {
            binding: binding.binding,
            resources: &resources[usize::try_from(binding.binding).expect("small")],
        })
        .collect();
    let set = context
        .create_bind_set(&set_layout, &entries)
        .expect("a bind set");
    let push: Vec<u8> = push.iter().flat_map(|word| word.to_le_bytes()).collect();
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&set], &push, dispatched)
        .expect("recorded");
    let queue = context.queue(0, 0).expect("a queue");
    queue
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[],
            signals: &[],
            deadline: gpu.longest(),
        })
        .expect("submitted")
        .wait(gpu.longest())
        .expect("completed");
    held.iter()
        .zip(buffers)
        .map(|((memory, _, _), words)| {
            let mut bytes = vec![0u8; words.len() * 4];
            memory.read(0, &mut bytes).expect("read");
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u32::from_le_bytes(*word))
                .collect()
        })
        .collect()
}

pub(crate) fn word(pixel: Pixel) -> u32 {
    u32::from_le_bytes([pixel.r, pixel.g, pixel.b, pixel.a])
}

pub(crate) fn pixel(word: u32) -> Pixel {
    let [r, g, b, a] = word.to_le_bytes();
    Pixel { r, g, b, a }
}

/// A premultiplied pixel, its alpha at either extreme a third of the time.
pub(crate) fn premultiplied(rng: &mut Prng) -> Pixel {
    let a = rng.edge_biased_u8();
    let mut channel = || u8::try_from(rng.below(usize::from(a) + 1)).unwrap_or(a);
    Pixel {
        r: channel(),
        g: channel(),
        b: channel(),
        a,
    }
}

pub(crate) fn picture(rng: &mut Prng, width: u32, height: u32) -> Surface {
    let mut surface = Surface::new(width, height).expect("a surface");
    // A smooth field under noise, so the blur has gradients to keep.
    let (dx, dy) = (rng.next_u8(), rng.next_u8());
    for (at, slot) in surface.pixels_mut().iter_mut().enumerate() {
        let at = u32::try_from(at).expect("small");
        let (x, y) = (at % width, at / width);
        *slot = if rng.below(3) == 0 {
            premultiplied(rng)
        } else {
            let level = u8::try_from((x * u32::from(dx) + y * u32::from(dy)) % 256).unwrap_or(0);
            Pixel {
                r: level,
                g: level / 2,
                b: 255 - level,
                a: 255,
            }
        };
    }
    surface
}

pub(crate) fn words(surface: &Surface) -> Vec<u32> {
    surface.pixels().iter().copied().map(word).collect()
}

/// A seeded generator for the case `name`.
pub(crate) fn rng(name: &str) -> Prng {
    Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV))
}

/// A number in `low..=high`.
pub(crate) fn between(rng: &mut Prng, low: u32, high: u32) -> u32 {
    low + u32::try_from(rng.at_most(usize::try_from(high - low).expect("small"))).expect("small")
}

/// A pixel a client may present: premultiplied, or now and then one whose
/// colour passes its alpha, nought included.
pub(crate) fn presented(rng: &mut Prng) -> Pixel {
    if rng.below(6) == 0 {
        Pixel {
            r: rng.next_u8(),
            g: rng.next_u8(),
            b: rng.next_u8(),
            a: [0, 1, 7][rng.below(3)],
        }
    } else {
        premultiplied(rng)
    }
}
