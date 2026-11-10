//! Each committed pass run on the software device against the software path
//! it is held to, bit for bit, over pictures drawn from the run's seed: the
//! frost against `Surface::frost_halved_from` and `halved_reference`, the
//! over against `blend_span`, the encode against `ChannelOrder::encode_run`,
//! and the copy and fill against the rows they lay.

use std::sync::Arc;

use tairix_abi::time::{Duration64, MonotonicClock};
use tairix_display::ChannelOrder;
use tairix_fuzzseed::Prng;
use tairix_gpu::{
    AllocationDesc, BindSetEntry, BufferDesc, BufferUsage, ComputePipelineDesc, Context,
    ContextDesc, Device, LayoutBinding, NoThreads, PushConstants, Resource, SoftwareDesc,
    Submission, DEFAULT_SUBGROUP_SIZE,
};
use tairix_parallel::Serial;
use tairix_raster::{
    blend_span, halved_reference, BlurScratch, Blurring, DitherRow, Pixel, Surface,
};
use tairix_shaders::{compose, frost, groups};
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
struct Gpu {
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

fn gpu() -> Gpu {
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
fn run(
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

fn word(pixel: Pixel) -> u32 {
    u32::from_le_bytes([pixel.r, pixel.g, pixel.b, pixel.a])
}

fn pixel(word: u32) -> Pixel {
    let [r, g, b, a] = word.to_le_bytes();
    Pixel { r, g, b, a }
}

/// A premultiplied pixel, its alpha at either extreme a third of the time.
fn premultiplied(rng: &mut Prng) -> Pixel {
    let a = rng.edge_biased_u8();
    let mut channel = || u8::try_from(rng.below(usize::from(a) + 1)).unwrap_or(a);
    Pixel {
        r: channel(),
        g: channel(),
        b: channel(),
        a,
    }
}

fn picture(rng: &mut Prng, width: u32, height: u32) -> Surface {
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

fn words(surface: &Surface) -> Vec<u32> {
    surface.pixels().iter().copied().map(word).collect()
}

/// The frost's first three passes over `rect` of the `stride`-wide `plane`:
/// the blurred field, `rect`'s width a row.
fn field(
    gpu: &Gpu,
    plane: &[u32],
    stride: u32,
    rect: (u32, u32, u32, u32),
    radius: u32,
) -> Vec<u32> {
    let (left, top, width, height) = rect;
    let reduce = frost::reduce::Push {
        stride,
        left,
        top,
        width,
        height,
    };
    let (across, down) = reduce.cells();
    let cells = vec![0; usize::try_from(across * down).expect("small")];
    let reduced = run(
        gpu,
        frost::reduce::SPIRV,
        &reduce.words(),
        &[plane, &cells],
        groups(across, down),
    );
    let mut blurred = reduced[1].clone();
    for vertical in [false, true] {
        let pass = frost::blur::Push {
            across,
            down,
            reach: radius / 2,
            vertical,
        };
        blurred = run(
            gpu,
            frost::blur::SPIRV,
            &pass.words(),
            &[&blurred, &cells],
            groups(across, down),
        )
        .swap_remove(1);
    }
    let out = vec![0; usize::try_from(width * height).expect("small")];
    let spread = frost::spread::Push {
        stride: width,
        left: 0,
        top: 0,
        width,
        height,
    };
    run(
        gpu,
        frost::spread::SPIRV,
        &spread.words(),
        &[&blurred, &out],
        groups(width, height),
    )
    .swap_remove(1)
}

#[test]
fn the_frost_passes_draw_the_half_resolution_blur_to_the_bit() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_frost_passes_draw_the_half_resolution_blur_to_the_bit",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    for (width, height, radius) in [
        (1, 1, 5),
        (1, 9, 3),
        (7, 1, 43),
        (13, 11, 7),
        (24, 17, 43),
        (31, 6, 1),
    ] {
        let picture = picture(&mut rng, width, height);
        let expected = halved_reference(
            picture.pixels(),
            usize::try_from(width).expect("small"),
            usize::try_from(height).expect("small"),
            radius,
        );
        let blurred = field(&gpu, &words(&picture), width, (0, 0, width, height), radius);
        let blurred: Vec<Pixel> = blurred.into_iter().map(pixel).collect();
        assert!(blurred == expected, "{width} x {height} at radius {radius}");
    }
}

#[test]
fn the_spread_lays_its_rectangle_at_its_place_in_a_surface_and_nowhere_else() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_spread_lays_its_rectangle_at_its_place_in_a_surface_and_nowhere_else",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (width, height, radius) = (15, 10, 9);
    let picture = picture(&mut rng, width, height);
    let blurred = field(&gpu, &words(&picture), width, (0, 0, width, height), radius);
    let (surface_w, surface_h, left, top) = (26, 19, 5, 7);
    let reduce = frost::reduce::Push {
        stride: width,
        left: 0,
        top: 0,
        width,
        height,
    };
    let (across, down) = reduce.cells();
    let reduced = run(
        &gpu,
        frost::reduce::SPIRV,
        &reduce.words(),
        &[
            &words(&picture),
            &vec![0; usize::try_from(across * down).expect("small")],
        ],
        groups(across, down),
    );
    let mut cells = reduced[1].clone();
    for vertical in [false, true] {
        let pass = frost::blur::Push {
            across,
            down,
            reach: radius / 2,
            vertical,
        };
        cells = run(
            &gpu,
            frost::blur::SPIRV,
            &pass.words(),
            &[&cells, &cells],
            groups(across, down),
        )
        .swap_remove(1);
    }
    let surface: Vec<u32> = (0..surface_w * surface_h).map(|_| rng.next_u32()).collect();
    let spread = frost::spread::Push {
        stride: surface_w,
        left,
        top,
        width,
        height,
    };
    let laid = run(
        &gpu,
        frost::spread::SPIRV,
        &spread.words(),
        &[&cells, &surface],
        groups(width, height),
    )
    .swap_remove(1);
    for y in 0..surface_h {
        for x in 0..surface_w {
            let at = usize::try_from(y * surface_w + x).expect("small");
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            let expected = if inside {
                blurred[usize::try_from((y - top) * width + (x - left)).expect("small")]
            } else {
                surface[at]
            };
            assert_eq!(laid[at], expected, "({x}, {y})");
        }
    }
}

#[test]
fn the_frost_mixed_over_a_surface_is_the_software_frost_to_the_bit() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_frost_mixed_over_a_surface_is_the_software_frost_to_the_bit",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (screen_w, screen_h) = (37, 29);
    for (rect, radius) in [
        ((3, 2, 21, 19), 9),
        ((0, 0, 37, 29), 43),
        ((30, 25, 7, 4), 5),
        ((11, 0, 1, 29), 3),
    ] {
        let backdrop = picture(&mut rng, screen_w, screen_h);
        let mut software = picture(&mut rng, screen_w, screen_h);
        let surface = words(&software);
        let (left, top, width, height) = rect;
        let coverage: Vec<u8> = (0..width * height).map(|_| rng.edge_biased_u8()).collect();
        let mut plane = backdrop.clone();
        let bands = [(left..left + width, top..top + height)];
        let frosted = software.frost_halved_from(
            &mut plane,
            &Blurring {
                rect,
                bands: &bands,
                radius,
            },
            &mut BlurScratch::new(),
            &Serial,
            |x, y| coverage[usize::try_from((y - top) * width + (x - left)).expect("small")],
        );
        assert!(frosted, "the software frost was written");

        let blurred = field(&gpu, &words(&backdrop), screen_w, rect, radius);
        let mut packed = vec![0u32; coverage.len().div_ceil(4)];
        for (at, level) in coverage.iter().enumerate() {
            packed[at / 4] |= u32::from(*level) << ((at % 4) * 8);
        }
        let mix = frost::mix::Push {
            stride: screen_w,
            left,
            top,
            width,
            height,
            blur_stride: width,
            blur_left: 0,
            blur_top: 0,
            coverage_stride: width,
            dithered: true,
        };
        let mixed = run(
            &gpu,
            frost::mix::SPIRV,
            &mix.words(),
            &[&surface, &blurred, &packed],
            groups(width, height),
        );
        let mixed: Vec<Pixel> = mixed[0].iter().copied().map(pixel).collect();
        assert!(mixed == software.pixels(), "{rect:?} at radius {radius}");
        assert!(
            words(&software) != surface,
            "{rect:?}: the frost changed nothing"
        );
    }
}

#[test]
fn the_over_pass_is_the_span_composite_to_the_bit() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_over_pass_is_the_span_composite_to_the_bit",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (screen_w, screen_h) = (29, 21);
    for (rect, factor, dithered) in [
        ((0, 0, 29, 21), 255, true),
        ((5, 3, 17, 12), 128, true),
        ((13, 9, 16, 12), 77, false),
        ((0, 20, 29, 1), 0, true),
    ] {
        let (left, top, width, height) = rect;
        let mut software = picture(&mut rng, screen_w, screen_h);
        let target = words(&software);
        let (source_w, source_h) = (width + 3, height + 2);
        let source: Vec<Pixel> = (0..source_w * source_h)
            .map(|_| premultiplied(&mut rng))
            .collect();
        let (source_left, source_top) = (2, 1);
        for row in 0..height {
            let y = top + row;
            let start = usize::try_from(y * screen_w + left).expect("small");
            let dst =
                &mut software.pixels_mut()[start..start + usize::try_from(width).expect("small")];
            let from = usize::try_from((source_top + row) * source_w + source_left).expect("small");
            let src = &source[from..from + usize::try_from(width).expect("small")];
            let dither = if dithered {
                DitherRow::at(y)
            } else {
                DitherRow::NEAREST
            };
            blend_span(dst, src, factor, dither, left);
        }
        let over = compose::over::Push {
            stride: screen_w,
            left,
            top,
            source_stride: source_w,
            source_left,
            source_top,
            width,
            height,
            factor,
            dithered,
        };
        let source_words: Vec<u32> = source.iter().copied().map(word).collect();
        let laid = run(
            &gpu,
            compose::over::SPIRV,
            &over.words(),
            &[&target, &source_words],
            groups(width, height),
        );
        let laid: Vec<Pixel> = laid[0].iter().copied().map(pixel).collect();
        assert!(
            laid == software.pixels(),
            "{rect:?} at {factor}, dithered {dithered}"
        );
        assert_eq!(
            words(&software) == target,
            factor == 0,
            "{rect:?}: only a factor of nought lays nothing"
        );
    }
}

#[test]
fn the_copy_and_fill_passes_lay_their_rectangles_and_nothing_else() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_copy_and_fill_passes_lay_their_rectangles_and_nothing_else",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (screen_w, screen_h) = (23, 17);
    let (left, top, width, height) = (4, 3, 11, 9);
    let target: Vec<u32> = (0..screen_w * screen_h).map(|_| rng.next_u32()).collect();
    let source: Vec<u32> = (0..20 * 20).map(|_| rng.next_u32()).collect();
    let copied = run(
        &gpu,
        compose::copy::SPIRV,
        &compose::copy::Push {
            stride: screen_w,
            left,
            top,
            source_stride: 20,
            source_left: 6,
            source_top: 5,
            width,
            height,
        }
        .words(),
        &[&target, &source],
        groups(width, height),
    );
    let pixel = rng.next_u32();
    let filled = run(
        &gpu,
        compose::fill::SPIRV,
        &compose::fill::Push {
            stride: screen_w,
            left,
            top,
            width,
            height,
            pixel,
        }
        .words(),
        &[&target],
        groups(width, height),
    );
    for y in 0..screen_h {
        for x in 0..screen_w {
            let at = usize::try_from(y * screen_w + x).expect("small");
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            let (copy, fill) = if inside {
                let from = usize::try_from((y - top + 5) * 20 + (x - left + 6)).expect("small");
                (source[from], pixel)
            } else {
                (target[at], target[at])
            };
            assert_eq!(copied[0][at], copy, "copied ({x}, {y})");
            assert_eq!(filled[0][at], fill, "filled ({x}, {y})");
        }
    }
}

#[test]
fn the_encode_pass_writes_each_display_order_as_the_display_does() {
    let gpu = gpu();
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_encode_pass_writes_each_display_order_as_the_display_does",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let (width, height) = (19, 7);
    let pixels: Vec<Pixel> = (0..width * height)
        .map(|_| premultiplied(&mut rng))
        .collect();
    let source: Vec<u32> = pixels.iter().copied().map(word).collect();
    for order in [ChannelOrder::Rgba, ChannelOrder::Bgra] {
        let mut expected = vec![0u8; pixels.len() * 4];
        assert_eq!(order.encode_run(&pixels, &mut expected), pixels.len());
        let probe = order.encode(Pixel {
            r: 0,
            g: 1,
            b: 2,
            a: 3,
        });
        let frame = vec![0u32; pixels.len()];
        let encoded = run(
            &gpu,
            compose::encode::SPIRV,
            &compose::encode::Push {
                stride: width,
                left: 0,
                top: 0,
                source_stride: width,
                source_left: 0,
                source_top: 0,
                width,
                height,
                order: u32::from_le_bytes(probe),
            }
            .words(),
            &[&frame, &source],
            groups(width, height),
        );
        let bytes: Vec<u8> = encoded[0]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        assert_eq!(bytes, expected, "{order:?}");
    }
}
