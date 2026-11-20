//! Bind sets, dispatches and render passes, direct and indirect, on a device
//! of one participant and on its twin of several.
//!
//! Every request is drawn sound, or carrying faults from a stated catalogue,
//! each the refusal it earns. A bind set is refused with one of its faults'
//! refusals or made; a submission is refused at its first command carrying
//! faults — a pass's draws checked in order, and only then each for reaching
//! what its pass renders to — with one of that command's, and changes no
//! byte. An admitted submission runs on both devices, the twin drawing each
//! indirect record as the direct draw it names and each indirect dispatch as
//! the direct one of the counts it reads: every byte of the two devices'
//! memory is then alike, every buffer byte is the reference's — a dispatch's
//! stores, the transfers' — and an image's bytes change only where a pass
//! renders to it or a clear writes it. A dispatch whose counts, read as it
//! runs, pass the device's loses its context, as the reference says it does.

use std::ops::Range;
use std::sync::Arc;
use std::vec::Vec;

use tairix_abi::Duration64;
use tairix_fuzzseed::Prng;
use tairix_gpu::{
    AllocationDesc, Aspects, BindSet, BindSetEntry, BindSetLayout, Blend, BufferDesc, BufferUsage,
    ClearValue, ColourAttachment, ColourMask, ColourTarget, CommandBuffer, CompareOp,
    ComputePipeline, ComputePipelineDesc, Context, ContextDesc, CullMode, DepthStencilAttachment,
    DepthStencilState, Draw, Error, Extent3d, Format, FrontFace, ImageDesc, ImageDimension,
    ImageRange, ImageUsage, ImageViewDesc, IndexFormat, Indices, Indirect, LayoutBinding, Load,
    Loss, Multisample, Pass, PushConstants, Rasterization, Rect, RenderPipeline,
    RenderPipelineDesc, Resource, Sampler, SamplerDesc, StageDesc, Submission, Topology,
    VertexAttribute, VertexBuffer, VertexStep, ViewDimension, Viewport, DRAW_RECORD_BYTES,
    INDEXED_DRAW_RECORD_BYTES,
};
use tairix_spirv::assemble::admit;
use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{Access, BindingKind, Component, ImageShape, Stages};

use super::buffers::{self, Recorded, Shape};
use super::{below, index_of, shaders};

/// Rounds per sweep. Miri interprets every operation, so it drives one.
pub(super) const ROUNDS: u32 = if cfg!(miri) { 1 } else { 40 };

/// Submissions per round.
const SUBMISSIONS_PER_ROUND: u32 = if cfg!(miri) { 3 } else { 16 };

/// The target every pass renders to: wide enough for two bins, which the
/// twin's participants draw at once.
const WIDE: u32 = 136;
const HIGH: u32 = 8;

/// The vertices of the vertex buffer, and the bytes from one to the next.
const VERTICES: u32 = 12;
const VERTEX_BYTES: u64 = 32;

/// The index buffer: sixteen-bit indices in its first half, thirty-two-bit
/// ones in its second.
const INDEX_BYTES: u64 = 256;

/// The records buffer: records of every word small first, read by any draw
/// at any word; indexed records after, whose added word may be negative, read
/// by indexed draws at their starts alone.
const PLAIN_RECORDS: u64 = 256;
const RECORD_BYTES: u64 = 512;

/// The dispatch counts buffer.
const COUNT_BYTES: u64 = 64;

/// The bytes the uniform buffer holds past the most a uniform binding spans:
/// room for a binding past it to be refused as larger.
const UNIFORM_PAST: u64 = 64;

/// The bytes of the moves the uniform buffer holds, which sets bind.
const MOVES_BYTES: u64 = 4096;

/// The storage buffer a dispatch writes.
const STORED_BYTES: u64 = 1024;

/// The buffers of a round, by place.
const VB: usize = 0;
const IB: usize = 1;
const RB: usize = 2;
const DB: usize = 3;
const UB: usize = 4;
const SB: usize = 5;
/// A buffer over all of the colour target's memory.
const AB: usize = 6;

/// The images of a round, by place, and the view of each by the same place:
/// the colour target, the depth target, the texture the textured pipeline
/// samples, a smaller colour target, an image only sampled, and one of two
/// levels; and an array view of the texture past them.
const C1: usize = 0;
const D: usize = 1;
const T: usize = 2;
const C2: usize = 3;
const U: usize = 4;
const M: usize = 5;
const TA: usize = 6;

/// The allocations of a round, by place.
const DATA: usize = 0;
const STORED: usize = 1;
const COLOUR: usize = 2;
const DEPTH: usize = 3;
const TEXTURE: usize = 4;
const OTHERS: usize = 5;

/// A world's pipelines and what they are made of: one device's.
struct World {
    _device: tairix_gpu::Device,
    limits: tairix_gpu::Limits,
    context: Context,
    queue: tairix_gpu::Queue,
    stored: BindSetLayout,
    textured_layout: BindSetLayout,
    keyed: ComputePipeline,
    plain: RenderPipeline,
    textured: RenderPipeline,
    sampler: Sampler,
}

/// The shape a sampled two-dimensional float image binding takes.
const SAMPLED_2D: ImageShape = ImageShape {
    dim: Dim::DIM_2D,
    arrayed: false,
    multisampled: false,
    component: Component::Float,
    format: ImageFormat::UNKNOWN,
};

/// The attributes of the vertex buffer: a position, then a colour.
const ATTRIBUTES: [VertexAttribute; 2] = [
    VertexAttribute {
        location: 0,
        format: Format::Rgba32Float,
        offset: 0,
    },
    VertexAttribute {
        location: 1,
        format: Format::Rgba32Float,
        offset: 16,
    },
];

/// A module of `text`, admitted, made `context`'s.
fn module(context: &Context, text: &str) -> tairix_gpu::ShaderModule {
    context
        .create_shader_module(admit(text).expect("admitted"))
        .expect("a module")
}

/// A render pipeline of `vertex` and `fragment` under `layout`, drawing the
/// vertex buffer's vertices as `topology`, restarting where `restart` says,
/// into an `Rgba8Unorm` target blended as `blend`, depth-tested as `depth`.
fn render_pipeline(
    context: &Context,
    layout: &tairix_gpu::PipelineLayout,
    (vertex, fragment): (&str, &str),
    (topology, restart): (Topology, bool),
    (depth, blend): (Option<DepthStencilState>, Option<Blend>),
) -> RenderPipeline {
    let (vertex, fragment) = (module(context, vertex), module(context, fragment));
    let stage = |module| StageDesc {
        module,
        entry_point: "main",
        specialization: &[],
    };
    context
        .create_render_pipeline(&RenderPipelineDesc {
            layout,
            vertex: stage(&vertex),
            fragment: Some(stage(&fragment)),
            vertex_buffers: &[VertexBuffer {
                stride: u32::try_from(VERTEX_BYTES).expect("a small stride"),
                step: VertexStep::Vertex,
                attributes: &ATTRIBUTES,
            }],
            topology,
            primitive_restart: restart,
            rasterization: Rasterization {
                cull: CullMode::None,
                front_face: FrontFace::CounterClockwise,
                depth_bias: None,
                depth_clamp: false,
                line_width: 1.0,
            },
            depth_stencil: depth,
            multisample: Multisample {
                samples: 1,
                mask: u32::MAX,
                alpha_to_coverage: false,
            },
            targets: &[ColourTarget {
                format: Format::Rgba8Unorm,
                blend,
                write_mask: ColourMask::ALL,
            }],
            logic_op: None,
        })
        .expect("a render pipeline")
}

/// The layouts of the keyed pipeline's storage set and of the textured
/// pipeline's set of a sampled image and the moves its vertices take.
fn set_layouts(context: &Context) -> (BindSetLayout, BindSetLayout) {
    let binding = |binding, kind, access, stages| LayoutBinding {
        binding,
        kind,
        count: 1,
        access,
        stages,
    };
    let stored = context
        .create_bind_set_layout(&[binding(
            0,
            BindingKind::StorageBuffer,
            Access::READ.with(Access::WRITE),
            Stages::COMPUTE,
        )])
        .expect("a layout");
    let textured = context
        .create_bind_set_layout(&[
            binding(
                0,
                BindingKind::CombinedImageSampler(SAMPLED_2D),
                Access::READ,
                Stages::FRAGMENT,
            ),
            binding(1, BindingKind::UniformBuffer, Access::READ, Stages::VERTEX),
        ])
        .expect("a layout");
    (stored, textured)
}

impl World {
    fn new(runner: Arc<dyn tairix_parallel::JobRunner + Send>) -> Self {
        let device = super::device(runner);
        let context = device
            .create_context(&ContextDesc::default())
            .expect("a context");
        let queue = context.queue(0, 0).expect("the queue");
        let (stored, textured_layout) = set_layouts(&context);
        let bare = context
            .create_pipeline_layout(&[], PushConstants::NONE)
            .expect("a layout");
        let keyed_layout = context
            .create_pipeline_layout(
                &[&stored],
                PushConstants {
                    bytes: 4,
                    stages: Stages::COMPUTE,
                },
            )
            .expect("a layout");
        let keyed_module = module(&context, shaders::KEYED);
        let keyed = context
            .create_compute_pipeline(&ComputePipelineDesc {
                module: &keyed_module,
                entry_point: "main",
                layout: &keyed_layout,
                specialization: &[],
            })
            .expect("a compute pipeline");
        let plain = render_pipeline(
            &context,
            &bare,
            (shaders::PLAIN_VERTEX, shaders::PLAIN_FRAGMENT),
            (Topology::Triangles, false),
            (
                Some(DepthStencilState {
                    format: Format::Depth32Float,
                    depth_compare: CompareOp::LessOrEqual,
                    depth_write: true,
                    stencil: None,
                }),
                Some(Blend::OVER),
            ),
        );
        let textured_pipeline_layout = context
            .create_pipeline_layout(
                &[&textured_layout],
                PushConstants {
                    bytes: 16,
                    stages: Stages::FRAGMENT,
                },
            )
            .expect("a layout");
        let textured = render_pipeline(
            &context,
            &textured_pipeline_layout,
            (shaders::MOVED_VERTEX, shaders::TEXTURED_FRAGMENT),
            (Topology::TriangleStrip, true),
            (None, None),
        );
        let sampler = context
            .create_sampler(&SamplerDesc::LINEAR)
            .expect("a sampler");
        Self {
            limits: device.info().limits,
            _device: device,
            context,
            queue,
            stored,
            textured_layout,
            keyed,
            plain,
            textured,
            sampler,
        }
    }

    fn pipeline(&self, pipe: Pipe) -> &RenderPipeline {
        match pipe {
            Pipe::Plain => &self.plain,
            Pipe::Textured => &self.textured,
        }
    }
}

/// The two worlds, kept while their contexts live: made again after one is
/// lost; and what the sweep has exercised.
pub(super) struct Worlds {
    pair: Option<[World; 2]>,
    tally: Tally,
}

/// What a sweep exercised: each verdict, each kind of indirect work run, and
/// each verdict of a set.
#[derive(Debug, Default)]
pub(super) struct Tally {
    refused: usize,
    admitted: usize,
    lost: usize,
    indirect_draws: usize,
    indirect_dispatches: usize,
    sets_made: usize,
    sets_refused: usize,
}

impl Worlds {
    pub(super) fn new() -> Self {
        Self {
            pair: None,
            tally: Tally::default(),
        }
    }

    /// Every verdict a round forces was reached.
    pub(super) fn exercised(&self) {
        let tally = &self.tally;
        std::println!("[fuzz_gpu] work exercised: {tally:?}");
        assert!(
            tally.refused > 0
                && tally.admitted > 0
                && tally.indirect_draws > 0
                && tally.indirect_dispatches > 0
                && tally.sets_made > 0
                && tally.sets_refused > 0,
            "a verdict went unexercised: {tally:?}"
        );
    }

    pub(super) fn run_round(&mut self, rng: &mut Prng) {
        let pair = self.pair.get_or_insert_with(|| {
            [
                World::new(Arc::new(tairix_parallel::Serial)),
                World::new(Arc::new(tairix_parallel::Threaded::new(3))),
            ]
        });
        let tally = &mut self.tally;
        let mut round = Round::new(rng, pair, tally);
        let (sound, refused) = round.forced();
        assert_eq!(
            round.exchange(pair, &sound, tally),
            Exchanged::Done,
            "a sound pass and dispatch, indirect, are admitted"
        );
        assert_eq!(
            round.exchange(pair, &refused, tally),
            Exchanged::Refused,
            "records off a word are refused"
        );
        for _ in 0..SUBMISSIONS_PER_ROUND {
            if rng.below(3) == 0 {
                round.scribble(rng);
            }
            let works: Vec<Work> = (0..=rng.below(3)).map(|_| round.draw_work(rng)).collect();
            if round.exchange(pair, &works, tally) == Exchanged::Lost {
                self.pair = None;
                return;
            }
        }
    }
}

/// The pipelines a draw names.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pipe {
    Plain,
    Textured,
}

/// What a draw makes, and where its counts come from.
#[derive(Clone, Debug)]
enum Count {
    Direct {
        positions: Range<u32>,
        base_vertex: i32,
        instances: Range<u32>,
    },
    Indirect {
        buffer: usize,
        offset: u64,
        count: u32,
        stride: u32,
    },
}

#[derive(Clone, Debug)]
struct DrawSpec {
    pipe: Pipe,
    sets: Vec<usize>,
    push: Vec<u8>,
    vertex_buffers: Vec<(usize, u64)>,
    index: Option<(usize, u64, IndexFormat)>,
    viewport: Viewport,
    scissor: Rect,
    blend_constant: [f32; 4],
    count: Count,
    /// The refusals its own checks give.
    faults: Vec<Error>,
    /// Whether it reaches what its pass renders to.
    reaches: bool,
}

#[derive(Clone, Debug)]
struct PassSpec {
    colour: Vec<(usize, Load)>,
    depth: Option<(usize, Load)>,
    area: Rect,
    faults: Vec<Error>,
    draws: Vec<DrawSpec>,
}

#[derive(Clone, Debug)]
enum Groups {
    Direct([u32; 3]),
    Indirect { buffer: usize, offset: u64 },
}

#[derive(Clone, Debug)]
struct DispatchSpec {
    sets: Vec<usize>,
    push: Vec<u8>,
    groups: Groups,
    faults: Vec<Error>,
}

#[derive(Clone, Debug)]
enum Work {
    Transfer(Recorded),
    Clear {
        value: ClearValue,
        faults: Vec<Error>,
    },
    Dispatch(DispatchSpec),
    Pass(PassSpec),
}

/// What the reference knows of a bind set.
#[derive(Copy, Clone, Debug)]
enum SetInfo {
    Stored {
        buffer: usize,
        offset: u64,
        size: u64,
    },
    Textured {
        view: usize,
    },
}

/// How an exchange ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Exchanged {
    Refused,
    Done,
    Lost,
}

/// Where a round's objects lie, alike in both worlds: each memory's size,
/// each buffer's and image's place, each view, and the bytes each image takes
/// of its memory.
struct Layout {
    memory_sizes: [u64; 6],
    buffers: [(usize, u64, u64, BufferUsage); 7],
    images: [(usize, ImageDesc); 6],
    placements: [u64; 6],
    views: Vec<(usize, ImageViewDesc)>,
    spans: Vec<(usize, Range<usize>)>,
}

impl Layout {
    fn of(world: &World) -> Self {
        let images = image_plan();
        let needs: Vec<tairix_gpu::MemoryRequirements> = images
            .iter()
            .map(|(_, desc)| world.context.image_requirements(desc).expect("an image"))
            .collect();
        let room = |image: usize| needs[image].size.next_multiple_of(needs[image].alignment);
        let mut placements = [0; 6];
        let mut others = 0;
        for (image, (memory, _)) in images.iter().enumerate() {
            if *memory == OTHERS {
                placements[image] = others;
                others += room(image);
            }
        }
        let (buffers, data_bytes) = buffer_plan(&world.limits, room(C1));
        let spans = images
            .iter()
            .zip(&needs)
            .zip(&placements)
            .map(|(((memory, _), needs), &at)| (*memory, index_of(at)..index_of(at + needs.size)))
            .collect();
        Self {
            memory_sizes: [data_bytes, STORED_BYTES, room(C1), room(D), room(T), others],
            buffers,
            views: view_plan(&images),
            images,
            placements,
            spans,
        }
    }

    /// The round's objects in `world`.
    fn make(&self, world: &World) -> Objects {
        let context = &world.context;
        let allocations: Vec<tairix_gpu::Allocation> = self
            .memory_sizes
            .iter()
            .map(|&size| {
                context
                    .allocate(&AllocationDesc {
                        size,
                        memory_type: 0,
                    })
                    .expect("memory")
            })
            .collect();
        let buffers = self
            .buffers
            .iter()
            .map(|&(memory, offset, size, usage)| {
                let desc = BufferDesc {
                    offset,
                    size,
                    usage,
                };
                context
                    .create_buffer(&allocations[memory], &desc)
                    .expect("a buffer")
            })
            .collect();
        let images: Vec<tairix_gpu::Image> = self
            .images
            .iter()
            .zip(&self.placements)
            .map(|((memory, desc), &offset)| {
                context
                    .create_image(&allocations[*memory], offset, desc)
                    .expect("an image")
            })
            .collect();
        let views = self
            .views
            .iter()
            .map(|(image, desc)| {
                context
                    .create_image_view(&images[*image], desc)
                    .expect("a view")
            })
            .collect();
        Objects {
            allocations,
            buffers,
            images,
            views,
            sets: Vec::new(),
        }
    }
}

/// One round's objects in each world, alike, and the reference beside them,
/// and the limits of the device both worlds are.
struct Round {
    limits: tairix_gpu::Limits,
    objects: [Objects; 2],
    shapes: Vec<Shape>,
    memories: Vec<Vec<u8>>,
    /// Each image's memory and the bytes of it the image takes.
    spans: Vec<(usize, Range<usize>)>,
    sets: Vec<SetInfo>,
}

struct Objects {
    allocations: Vec<tairix_gpu::Allocation>,
    buffers: Vec<tairix_gpu::Buffer>,
    images: Vec<tairix_gpu::Image>,
    views: Vec<tairix_gpu::ImageView>,
    sets: Vec<BindSet>,
}

/// The uniform buffer's bytes on a device of `limits`.
const fn uniform_bytes(limits: &tairix_gpu::Limits) -> u64 {
    limits.max_uniform_buffer_range + UNIFORM_PAST
}

/// How the round's buffers are made on a device of `limits` — memory,
/// offset, size, usage — and the bytes of the data memory they share.
fn buffer_plan(
    limits: &tairix_gpu::Limits,
    colour_memory: u64,
) -> ([(usize, u64, u64, BufferUsage); 7], u64) {
    let both = BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST);
    let after = |at: u64, size: u64| (at + size).next_multiple_of(limits.buffer_alignment);
    let index_at = after(0, u64::from(VERTICES) * VERTEX_BYTES);
    let records_at = after(index_at, INDEX_BYTES);
    let counts_at = after(records_at, RECORD_BYTES);
    let uniforms_at = after(counts_at, COUNT_BYTES);
    let plan = [
        (
            DATA,
            0,
            u64::from(VERTICES) * VERTEX_BYTES,
            both.with(BufferUsage::VERTEX),
        ),
        (DATA, index_at, INDEX_BYTES, both.with(BufferUsage::INDEX)),
        (
            DATA,
            records_at,
            RECORD_BYTES,
            BufferUsage::INDIRECT.with(BufferUsage::TRANSFER_DST),
        ),
        (
            DATA,
            counts_at,
            COUNT_BYTES,
            BufferUsage::INDIRECT.with(BufferUsage::TRANSFER_DST),
        ),
        (
            DATA,
            uniforms_at,
            uniform_bytes(limits),
            BufferUsage::UNIFORM,
        ),
        (STORED, 0, STORED_BYTES, both.with(BufferUsage::STORAGE)),
        (COLOUR, 0, colour_memory, BufferUsage::ALL),
    ];
    (plan, uniforms_at + uniform_bytes(limits))
}

/// How the round's images are made, and in which memory.
fn image_plan() -> [(usize, ImageDesc); 6] {
    let desc = |format, (width, height), mip_levels, usage| ImageDesc {
        dimension: ImageDimension::D2,
        format,
        extent: Extent3d {
            width,
            height,
            depth: 1,
        },
        mip_levels,
        array_layers: 1,
        samples: 1,
        usage,
    };
    let drawn = ImageUsage::COLOR_ATTACHMENT
        .with(ImageUsage::SAMPLED)
        .with(ImageUsage::TRANSFER_SRC);
    [
        (COLOUR, desc(Format::Rgba8Unorm, (WIDE, HIGH), 1, drawn)),
        (
            DEPTH,
            desc(
                Format::Depth32Float,
                (WIDE, HIGH),
                1,
                ImageUsage::DEPTH_STENCIL_ATTACHMENT.with(ImageUsage::SAMPLED),
            ),
        ),
        (
            TEXTURE,
            desc(
                Format::Rgba8Unorm,
                (4, 4),
                2,
                ImageUsage::SAMPLED.with(ImageUsage::TRANSFER_DST),
            ),
        ),
        (
            OTHERS,
            desc(
                Format::Rgba8Unorm,
                (WIDE / 2, HIGH / 2),
                1,
                ImageUsage::COLOR_ATTACHMENT,
            ),
        ),
        (
            OTHERS,
            desc(Format::Rgba8Unorm, (WIDE, HIGH), 1, ImageUsage::SAMPLED),
        ),
        (
            OTHERS,
            desc(
                Format::Rgba8Unorm,
                (WIDE, HIGH),
                2,
                ImageUsage::COLOR_ATTACHMENT,
            ),
        ),
    ]
}

/// The view of each image by its place, and the texture's array view.
fn view_plan(images: &[(usize, ImageDesc); 6]) -> Vec<(usize, ImageViewDesc)> {
    let mut views: Vec<(usize, ImageViewDesc)> = images
        .iter()
        .enumerate()
        .map(|(image, (_, desc))| {
            (
                image,
                ImageViewDesc {
                    format: desc.format,
                    dimension: ViewDimension::D2,
                    range: ImageRange {
                        aspects: desc.format.aspects(),
                        base_level: 0,
                        levels: desc.mip_levels,
                        base_layer: 0,
                        layers: 1,
                    },
                },
            )
        })
        .collect();
    let (_, texture) = views[T];
    views.push((
        T,
        ImageViewDesc {
            dimension: ViewDimension::D2Array,
            ..texture
        },
    ));
    views
}

/// A float of whole sixty-fourths, from `low` to `high` of them: exact.
fn sixty_fourths(rng: &mut Prng, (low, high): (i16, i16)) -> f32 {
    let drawn = below(rng, u32::from(high.abs_diff(low)) + 1);
    let k = i32::from(low) + i32::try_from(drawn).expect("within the span");
    f32::from(i16::try_from(k).expect("between the bounds")) / 64.0
}

/// `pixels` in sixty-fourths.
fn in_sixty_fourths(pixels: u32) -> i16 {
    i16::try_from(pixels * 64).expect("a side in sixty-fourths")
}

/// `n`, a small count, as a float: exactly.
fn real(n: u32) -> f32 {
    f32::from(u16::try_from(n).expect("a small count"))
}

fn words_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// `words` little-endian words of `memory` from byte `at`.
fn words_at(memory: &[u8], at: usize, words: usize) -> Vec<u32> {
    memory[at..at + 4 * words]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect()
}

/// A vertex buffer's worth of vertices over the target and about it.
fn draw_vertices(rng: &mut Prng) -> Vec<u8> {
    let mut floats = Vec::new();
    for _ in 0..VERTICES {
        let position = [
            sixty_fourths(rng, (-80, 80)),
            sixty_fourths(rng, (-80, 80)),
            sixty_fourths(rng, (0, 64)),
            *rng.pick(&[1.0, 1.0, 1.0, 0.5, 2.0]),
        ];
        let colour = [
            sixty_fourths(rng, (0, 64)),
            sixty_fourths(rng, (0, 64)),
            sixty_fourths(rng, (0, 64)),
            sixty_fourths(rng, (16, 64)),
        ];
        floats.extend(position.into_iter().chain(colour));
    }
    if rng.below(8) == 0 {
        let at = rng.below(floats.len());
        floats[at] = *rng.pick(&[f32::NAN, f32::INFINITY, -1.0e30]);
    }
    floats.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// An index of `bits`: a vertex, one past them, or the restart index.
fn draw_index(rng: &mut Prng, bits: u32) -> u32 {
    match rng.below(10) {
        0 => u32::MAX >> (32 - bits),
        1 => VERTICES + below(rng, 3),
        _ => below(rng, VERTICES),
    }
}

fn draw_indices(rng: &mut Prng) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..INDEX_BYTES / 4 {
        let index = u16::try_from(draw_index(rng, 16)).expect("sixteen bits");
        bytes.extend(index.to_le_bytes());
    }
    for _ in 0..INDEX_BYTES / 8 {
        bytes.extend(draw_index(rng, 32).to_le_bytes());
    }
    bytes
}

/// A record whose every word is small: whatever reads it draws little.
fn draw_plain_record(rng: &mut Prng) -> [u32; 4] {
    [below(rng, 13), below(rng, 4), below(rng, 13), below(rng, 4)]
}

/// An indexed record, its added word within four of nought either way.
fn draw_indexed_record(rng: &mut Prng) -> [u32; 5] {
    [
        below(rng, 13),
        below(rng, 4),
        below(rng, 41),
        (below(rng, 9).cast_signed() - 4).cast_unsigned(),
        below(rng, 4),
    ]
}

fn draw_records(rng: &mut Prng) -> Vec<u8> {
    let mut words = Vec::new();
    for _ in 0..PLAIN_RECORDS / 16 {
        words.extend(draw_plain_record(rng));
    }
    for _ in 0..(RECORD_BYTES - PLAIN_RECORDS) / 20 {
        words.extend(draw_indexed_record(rng));
    }
    let mut bytes = words_bytes(&words);
    bytes.resize(index_of(RECORD_BYTES), 0);
    bytes
}

/// Workgroup counts: few, or one time in `past_one_in` one past the
/// device's `most` along the first axis.
fn draw_counts(rng: &mut Prng, (past_one_in, most): (usize, [u32; 3])) -> [u32; 3] {
    if rng.below(past_one_in) == 0 {
        [most[0].saturating_add(1), 1, 1]
    } else {
        [below(rng, 7), below(rng, 4), below(rng, 4)]
    }
}

/// Counts written as a round begins, none past the device's.
const NEVER: usize = usize::MAX;

/// One update or scribble of counts in this many writes one past the
/// device's, so a dispatch loses its context now and then.
const PAST_ONE_IN: usize = 24;

fn draw_moves(rng: &mut Prng) -> Vec<u8> {
    (0..MOVES_BYTES / 4)
        .map(|_| sixty_fourths(rng, (-16, 16)))
        .flat_map(f32::to_le_bytes)
        .collect()
}

/// A rectangle of the target: of some pixels, within it.
fn draw_area(rng: &mut Prng) -> Rect {
    let x = below(rng, WIDE);
    let y = below(rng, HIGH);
    Rect {
        x,
        y,
        width: 1 + below(rng, WIDE - x),
        height: 1 + below(rng, HIGH - y),
    }
}

fn draw_viewport(rng: &mut Prng) -> Viewport {
    let height = sixty_fourths(rng, (64, in_sixty_fourths(HIGH + 8)));
    Viewport {
        x: sixty_fourths(rng, (-512, in_sixty_fourths(WIDE - 8))),
        y: sixty_fourths(rng, (-256, in_sixty_fourths(HIGH))),
        width: sixty_fourths(rng, (64, in_sixty_fourths(WIDE + 16))),
        height: if rng.below(4) == 0 { -height } else { height },
        min_depth: sixty_fourths(rng, (0, 64)),
        max_depth: sixty_fourths(rng, (0, 64)),
    }
}

fn draw_scissor(rng: &mut Prng) -> Rect {
    Rect {
        x: below(rng, WIDE + 5),
        y: below(rng, HIGH + 5),
        width: below(rng, WIDE + 5),
        height: below(rng, HIGH + 5),
    }
}

/// The faults a request is drawn carrying: none, most of the time; else each
/// site of its catalogue carries one in turn, one time in `odds`.
struct Faults {
    odds: usize,
    errors: Vec<Error>,
}

impl Faults {
    /// Faults for a request carrying any one time in `one_in`.
    fn drawn(rng: &mut Prng, one_in: usize, odds: usize) -> Self {
        Self {
            odds: if rng.below(one_in) == 0 { odds } else { 0 },
            errors: Vec::new(),
        }
    }

    /// Whether the next site carries its fault, `error` the refusal it earns.
    fn inject(&mut self, rng: &mut Prng, error: Error) -> bool {
        let chosen = self.carried(rng);
        if chosen {
            self.errors.push(error);
        }
        chosen
    }

    /// Whether the next site carries its fault: one refused not by its own
    /// checks but for reaching what its pass renders to.
    fn carried(&mut self, rng: &mut Prng) -> bool {
        self.odds > 0 && rng.below(self.odds) == 0
    }
}

impl Round {
    fn new(rng: &mut Prng, worlds: &[World; 2], tally: &mut Tally) -> Self {
        let layout = Layout::of(&worlds[0]);
        let mut round = Self {
            limits: worlds[0].limits,
            objects: [layout.make(&worlds[0]), layout.make(&worlds[1])],
            shapes: layout
                .buffers
                .iter()
                .map(|&(memory, offset, size, usage)| Shape {
                    memory,
                    offset,
                    size,
                    usage,
                })
                .collect(),
            memories: layout
                .memory_sizes
                .iter()
                .map(|&size| vec![0; index_of(size)])
                .collect(),
            spans: layout.spans,
            sets: Vec::new(),
        };
        let most = round.limits.max_workgroup_count;
        let counts: Vec<u32> = (0..6)
            .flat_map(|_| draw_counts(rng, (NEVER, most)))
            .collect();
        let filled = [
            (VB, draw_vertices(rng)),
            (IB, draw_indices(rng)),
            (RB, draw_records(rng)),
            (DB, words_bytes(&counts[..16])),
            (UB, draw_moves(rng)),
        ];
        for (buffer, bytes) in filled {
            round.write(buffer, 0, &bytes);
        }
        round.make_sets(rng, worlds, tally);
        round
    }

    /// A sound submission every round admits — an indirect dispatch, and a
    /// pass of an indirect draw and an indexed indirect one — and one it
    /// refuses at its draw, its records off a word.
    fn forced(&self) -> (Vec<Work>, Vec<Work>) {
        let stored = self.sets_where(|set| matches!(set, SetInfo::Stored { .. }));
        let dispatch = Work::Dispatch(DispatchSpec {
            sets: vec![stored[0]],
            push: 7u32.to_le_bytes().to_vec(),
            groups: Groups::Indirect {
                buffer: DB,
                offset: 0,
            },
            faults: Vec::new(),
        });
        let (viewport, scissor) = (
            Viewport {
                x: 0.0,
                y: 0.0,
                width: real(WIDE),
                height: real(HIGH),
                min_depth: 0.0,
                max_depth: 1.0,
            },
            Rect {
                x: 0,
                y: 0,
                width: WIDE,
                height: HIGH,
            },
        );
        let draw = |index, count| DrawSpec {
            pipe: Pipe::Plain,
            sets: Vec::new(),
            push: Vec::new(),
            vertex_buffers: vec![(VB, 0)],
            index,
            viewport,
            scissor,
            blend_constant: [0.0; 4],
            count,
            faults: Vec::new(),
            reaches: false,
        };
        let records = |offset, stride| Count::Indirect {
            buffer: RB,
            offset,
            count: 2,
            stride,
        };
        let pass = |draws| {
            Work::Pass(PassSpec {
                colour: vec![(C1, Load::Clear(ClearValue::Float([0.0; 4])))],
                depth: Some((
                    D,
                    Load::Clear(ClearValue::DepthStencil {
                        depth: 1.0,
                        stencil: 0,
                    }),
                )),
                area: scissor,
                faults: Vec::new(),
                draws,
            })
        };
        let indexed = Some((IB, 0, IndexFormat::U16));
        let sound = vec![
            dispatch,
            pass(vec![
                draw(None, records(0, 16)),
                draw(indexed, records(PLAIN_RECORDS, 20)),
            ]),
        ];
        let off_a_word = DrawSpec {
            faults: vec![Error::Misaligned],
            ..draw(None, records(2, 16))
        };
        (sound, vec![pass(vec![off_a_word])])
    }

    /// Write `bytes` at `offset` into buffer `buffer`, from the CPU, in both
    /// worlds and the reference.
    fn write(&mut self, buffer: usize, offset: u64, bytes: &[u8]) {
        let shape = self.shapes[buffer];
        for objects in &self.objects {
            objects.allocations[shape.memory]
                .write(shape.offset + offset, bytes)
                .expect("an in-range write");
        }
        let at = index_of(shape.offset + offset);
        self.memories[shape.memory][at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn scribble(&mut self, rng: &mut Prng) {
        match rng.below(4) {
            0 => {
                let vertices = draw_vertices(rng);
                self.write(VB, 0, &vertices);
            }
            1 => {
                let slot = rng.below(index_of(PLAIN_RECORDS / 16)) as u64;
                let record = words_bytes(&draw_plain_record(rng));
                self.write(RB, slot * 16, &record);
            }
            2 => {
                let slot = rng.below(index_of((RECORD_BYTES - PLAIN_RECORDS) / 20)) as u64;
                let record = words_bytes(&draw_indexed_record(rng));
                self.write(RB, PLAIN_RECORDS + slot * 20, &record);
            }
            _ => {
                let at = 12 * rng.below(5) as u64;
                let counts = draw_counts(rng, (PAST_ONE_IN, self.limits.max_workgroup_count));
                self.write(DB, at, &words_bytes(&counts));
            }
        }
    }

    /// Ask both worlds for a few sets of each layout, sound or carrying
    /// faults; keep the ones made.
    fn make_sets(&mut self, rng: &mut Prng, worlds: &[World; 2], tally: &mut Tally) {
        for _ in 0..3 {
            self.make_stored_set(rng, worlds, tally);
        }
        for _ in 0..2 {
            self.make_textured_set(rng, worlds, T, tally);
        }
        // A set sampling what a pass renders to.
        self.make_textured_set(rng, worlds, C1, tally);
        // A set of nothing, refused whatever else a round draws.
        let mut nothing = Vec::new();
        for (world, objects) in worlds.iter().zip(&self.objects) {
            let resource = [Resource::Buffer {
                buffer: &objects.buffers[SB],
                offset: 0,
                size: 0,
            }];
            nothing.push(world.context.create_bind_set(
                &world.stored,
                &[BindSetEntry {
                    binding: 0,
                    resources: &resource,
                }],
            ));
        }
        self.keep(
            nothing,
            &[Error::Empty],
            SetInfo::Stored {
                buffer: SB,
                offset: 0,
                size: 0,
            },
            tally,
        );
        let stored = self
            .sets
            .iter()
            .any(|set| matches!(set, SetInfo::Stored { .. }));
        if !stored {
            self.make_sound_stored_set(worlds);
        }
        if !self
            .sets
            .iter()
            .any(|set| matches!(set, SetInfo::Textured { view: T }))
        {
            self.make_sound_textured_set(worlds);
        }
    }

    fn make_sound_stored_set(&mut self, worlds: &[World; 2]) {
        for (world, objects) in worlds.iter().zip(&mut self.objects) {
            let resource = [Resource::Buffer {
                buffer: &objects.buffers[SB],
                offset: 0,
                size: STORED_BYTES,
            }];
            let set = world
                .context
                .create_bind_set(
                    &world.stored,
                    &[BindSetEntry {
                        binding: 0,
                        resources: &resource,
                    }],
                )
                .expect("a sound set");
            objects.sets.push(set);
        }
        self.sets.push(SetInfo::Stored {
            buffer: SB,
            offset: 0,
            size: STORED_BYTES,
        });
    }

    fn make_sound_textured_set(&mut self, worlds: &[World; 2]) {
        for (world, objects) in worlds.iter().zip(&mut self.objects) {
            let sampled = [Resource::ImageSampler(&objects.views[T], &world.sampler)];
            let moves = [Resource::Buffer {
                buffer: &objects.buffers[UB],
                offset: 0,
                size: 16,
            }];
            let set = world
                .context
                .create_bind_set(
                    &world.textured_layout,
                    &[
                        BindSetEntry {
                            binding: 0,
                            resources: &sampled,
                        },
                        BindSetEntry {
                            binding: 1,
                            resources: &moves,
                        },
                    ],
                )
                .expect("a sound set");
            objects.sets.push(set);
        }
        self.sets.push(SetInfo::Textured { view: T });
    }

    fn make_stored_set(&mut self, rng: &mut Prng, worlds: &[World; 2], tally: &mut Tally) {
        let mut faults = Faults::drawn(rng, 3, 3);
        let mut buffer = SB;
        let align = self.limits.storage_buffer_alignment;
        let mut offset = align * rng.below(index_of(STORED_BYTES / align / 2)) as u64;
        let mut size = 4 + 4 * rng.below(index_of((STORED_BYTES - offset) / 4)) as u64;
        if align > 1 && faults.inject(rng, Error::Misaligned) {
            offset += align / 2;
        }
        if faults.inject(rng, Error::Empty) {
            size = 0;
        }
        if faults.inject(rng, Error::OutOfRange) {
            size = STORED_BYTES + 16 - offset;
        }
        if faults.inject(rng, Error::Usage) {
            buffer = RB;
            offset = 0;
            size = 16;
        }
        let kind = faults.inject(rng, Error::Shape);
        let none = faults.inject(rng, Error::Bindings);
        let mut made = Vec::new();
        for (world, objects) in worlds.iter().zip(&self.objects) {
            let resource = [if kind {
                Resource::Sampler(&world.sampler)
            } else {
                Resource::Buffer {
                    buffer: &objects.buffers[buffer],
                    offset,
                    size,
                }
            }];
            let entries = [BindSetEntry {
                binding: 0,
                resources: &resource,
            }];
            let entries: &[BindSetEntry<'_>] = if none { &[] } else { &entries };
            made.push(world.context.create_bind_set(&world.stored, entries));
        }
        self.keep(
            made,
            &faults.errors,
            SetInfo::Stored {
                buffer,
                offset,
                size,
            },
            tally,
        );
    }

    fn make_textured_set(
        &mut self,
        rng: &mut Prng,
        worlds: &[World; 2],
        sampled: usize,
        tally: &mut Tally,
    ) {
        let mut faults = Faults::drawn(rng, 3, 4);
        let mut view = sampled;
        let align = self.limits.uniform_buffer_alignment;
        let uniforms = uniform_bytes(&self.limits);
        let (mut buffer, mut offset, mut size) = (
            UB,
            align * rng.below(index_of(MOVES_BYTES / align)) as u64,
            16,
        );
        if rng.below(4) == 0 {
            size = 4 * (1 + rng.below(8) as u64);
        }
        if faults.inject(rng, Error::Usage) {
            view = C2;
        }
        if faults.inject(rng, Error::Shape) {
            view = TA;
        }
        if align > 1 && faults.inject(rng, Error::Misaligned) {
            offset += align / 2;
        }
        if faults.inject(rng, Error::Empty) {
            size = 0;
        }
        if faults.inject(rng, Error::OutOfRange) {
            offset = (uniforms - 16) / align * align;
            size = uniforms - offset + 16;
        }
        if faults.inject(rng, Error::TooLarge) {
            offset = 0;
            size = self.limits.max_uniform_buffer_range + UNIFORM_PAST / 2;
        }
        if faults.inject(rng, Error::Usage) {
            buffer = SB;
            offset = 0;
            size = 16;
        }
        let kind = faults.inject(rng, Error::Shape);
        let mut made = Vec::new();
        for (world, objects) in worlds.iter().zip(&self.objects) {
            let sampled = [if kind {
                Resource::Buffer {
                    buffer: &objects.buffers[UB],
                    offset: 0,
                    size: 16,
                }
            } else {
                Resource::ImageSampler(&objects.views[view], &world.sampler)
            }];
            let moves = [Resource::Buffer {
                buffer: &objects.buffers[buffer],
                offset,
                size,
            }];
            made.push(world.context.create_bind_set(
                &world.textured_layout,
                &[
                    BindSetEntry {
                        binding: 0,
                        resources: &sampled,
                    },
                    BindSetEntry {
                        binding: 1,
                        resources: &moves,
                    },
                ],
            ));
        }
        self.keep(made, &faults.errors, SetInfo::Textured { view }, tally);
    }

    /// Hold both worlds' answers to the reference's, and keep the set where
    /// it was made.
    fn keep(
        &mut self,
        made: Vec<Result<BindSet, Error>>,
        faults: &[Error],
        info: SetInfo,
        tally: &mut Tally,
    ) {
        let mut sets = Vec::new();
        for answer in made {
            match answer {
                Ok(set) => {
                    assert!(faults.is_empty(), "a set carrying {faults:?} was made");
                    sets.push(set);
                }
                Err(error) => assert!(
                    faults.contains(&error),
                    "a set refused with {error:?} carried {faults:?}"
                ),
            }
        }
        match <[BindSet; 2]>::try_from(sets) {
            Ok([first, second]) => {
                self.objects[0].sets.push(first);
                self.objects[1].sets.push(second);
                self.sets.push(info);
                tally.sets_made += 1;
            }
            Err(sets) => {
                assert!(sets.is_empty(), "the two worlds answered alike");
                tally.sets_refused += 1;
            }
        }
    }

    /// The sets of `kind`.
    fn sets_where(&self, wanted: impl Fn(&SetInfo) -> bool) -> Vec<usize> {
        self.sets
            .iter()
            .enumerate()
            .filter(|(_, set)| wanted(set))
            .map(|(at, _)| at)
            .collect()
    }

    fn draw_work(&self, rng: &mut Prng) -> Work {
        match rng.below(8) {
            0 | 1 => Work::Transfer(draw_transfer(
                rng,
                &self.shapes,
                self.limits.max_workgroup_count,
            )),
            2 => {
                let mut faults = Faults::drawn(rng, 6, 1);
                let value = if faults.inject(rng, Error::Shape) {
                    ClearValue::Uint([1; 4])
                } else {
                    ClearValue::Float([
                        sixty_fourths(rng, (0, 64)),
                        sixty_fourths(rng, (0, 64)),
                        sixty_fourths(rng, (0, 64)),
                        1.0,
                    ])
                };
                Work::Clear {
                    value,
                    faults: faults.errors,
                }
            }
            3 | 4 => Work::Dispatch(self.draw_dispatch(rng)),
            _ => Work::Pass(self.draw_pass(rng)),
        }
    }

    fn draw_dispatch(&self, rng: &mut Prng) -> DispatchSpec {
        let stored = self.sets_where(|set| matches!(set, SetInfo::Stored { .. }));
        let textured = self.sets_where(|set| matches!(set, SetInfo::Textured { .. }));
        let mut faults = Faults::drawn(rng, 5, 4);
        let mut sets = vec![*rng.pick(&stored)];
        if faults.inject(rng, Error::Bindings) {
            match rng.below(3) {
                0 => sets.clear(),
                1 => sets.push(*rng.pick(&stored)),
                _ => sets = vec![*rng.pick(&textured)],
            }
        }
        let mut push = rng.next_u32().to_le_bytes().to_vec();
        if faults.inject(rng, Error::Shape) {
            push.extend([0; 4]);
        }
        let most = self.limits.max_workgroup_count;
        let groups = if rng.below(2) == 0 {
            let mut counts = draw_counts(rng, (NEVER, most));
            let axis = rng.below(3);
            if most[axis] < u32::MAX && faults.inject(rng, Error::TooLarge) {
                counts[axis] = most[axis] + 1;
            }
            Groups::Direct(counts)
        } else {
            let (mut buffer, mut offset) = (DB, 4 * u64::from(below(rng, 14)));
            if faults.inject(rng, Error::Usage) {
                buffer = VB;
            }
            if faults.inject(rng, Error::Misaligned) {
                offset += 2;
            }
            if faults.inject(rng, Error::OutOfRange) {
                offset = COUNT_BYTES - 8;
            }
            Groups::Indirect { buffer, offset }
        };
        DispatchSpec {
            sets,
            push,
            groups,
            faults: faults.errors,
        }
    }

    fn draw_pass(&self, rng: &mut Prng) -> PassSpec {
        let depthed = rng.below(2) == 0;
        let mut faults = Faults::drawn(rng, 8, 5);
        let colour_load = |rng: &mut Prng| {
            if rng.below(3) == 0 {
                Load::Keep
            } else {
                Load::Clear(ClearValue::Float([
                    sixty_fourths(rng, (0, 64)),
                    sixty_fourths(rng, (0, 64)),
                    sixty_fourths(rng, (0, 64)),
                    sixty_fourths(rng, (0, 64)),
                ]))
            }
        };
        let mut colour = vec![(C1, colour_load(rng))];
        let mut depth = depthed.then(|| {
            let load = if rng.below(3) == 0 {
                Load::Keep
            } else {
                Load::Clear(ClearValue::DepthStencil {
                    depth: sixty_fourths(rng, (0, 64)),
                    stencil: 0,
                })
            };
            (D, load)
        });
        let mut area = draw_area(rng);
        if faults.inject(rng, Error::Usage) {
            colour[0].0 = *rng.pick(&[U, D]);
        }
        if faults.inject(rng, Error::Shape) {
            colour[0].0 = M;
        }
        if faults.inject(rng, Error::Shape) {
            colour.push((C2, Load::Keep));
        }
        if faults.inject(rng, Error::Overlap) {
            colour.push((C1, Load::Keep));
        }
        if faults.inject(rng, Error::Shape) {
            colour[0].1 = Load::Clear(ClearValue::Uint([1; 4]));
        }
        if depth.is_some() && faults.inject(rng, Error::Shape) {
            depth = Some((
                D,
                Load::Clear(ClearValue::DepthStencil {
                    depth: 2.0,
                    stencil: 0,
                }),
            ));
        }
        if faults.inject(rng, Error::Empty) {
            area.width = 0;
        }
        if faults.inject(rng, Error::OutOfRange) {
            area.x = WIDE - area.width + 1;
        }
        if faults.inject(rng, Error::Empty) {
            colour.clear();
            depth = None;
        }
        let pipe = if depthed { Pipe::Plain } else { Pipe::Textured };
        let draws = (0..rng.below(4))
            .map(|_| self.draw_draw(rng, pipe))
            .collect();
        PassSpec {
            colour,
            depth,
            area,
            faults: faults.errors,
            draws,
        }
    }

    fn draw_draw(&self, rng: &mut Prng, fits: Pipe) -> DrawSpec {
        let mut faults = Faults::drawn(rng, 6, 10);
        let mut reaches = false;
        let mut pipe = fits;
        if faults.inject(rng, Error::Interface) {
            pipe = match fits {
                Pipe::Plain => Pipe::Textured,
                Pipe::Textured => Pipe::Plain,
            };
        }
        let (sets, push) = self.draw_bindings(rng, pipe, (&mut faults, &mut reaches));
        let vertex_buffers = draw_vertex_buffers(rng, (&mut faults, &mut reaches));
        let (viewport, scissor, blend_constant) = draw_placing(rng, &mut faults);
        let indexed = rng.below(2) == 0;
        let index = indexed.then(|| draw_index_buffer(rng, (&mut faults, &mut reaches)));
        let count = if rng.below(2) == 0 {
            draw_direct_count(rng, indexed, &mut faults)
        } else {
            Self::draw_records(rng, indexed, (&mut faults, &mut reaches))
        };
        DrawSpec {
            pipe,
            sets,
            push,
            vertex_buffers,
            index,
            viewport,
            scissor,
            blend_constant,
            count,
            faults: faults.errors,
            reaches,
        }
    }

    /// The sets and push constants of a draw of `pipe`: its own, or one
    /// alteration of them — a set its layout does not hold, or one sampling
    /// what its pass renders to.
    fn draw_bindings(
        &self,
        rng: &mut Prng,
        pipe: Pipe,
        (faults, reaches): (&mut Faults, &mut bool),
    ) -> (Vec<usize>, Vec<u8>) {
        let textured = self.sets_where(|set| matches!(set, SetInfo::Textured { view: T }));
        let stored = self.sets_where(|set| matches!(set, SetInfo::Stored { .. }));
        let sampling_target = self.sets_where(|set| matches!(set, SetInfo::Textured { view: C1 }));
        let (mut sets, mut push) = match pipe {
            Pipe::Plain => (Vec::new(), Vec::new()),
            Pipe::Textured => (
                vec![*rng.pick(&textured)],
                [
                    sixty_fourths(rng, (32, 64)),
                    sixty_fourths(rng, (32, 64)),
                    sixty_fourths(rng, (32, 64)),
                    1.0,
                ]
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect(),
            ),
        };
        if faults.inject(rng, Error::Bindings) {
            sets = match (pipe, rng.below(2)) {
                (Pipe::Textured, 0) => Vec::new(),
                _ => vec![*rng.pick(&stored)],
            };
        } else if pipe == Pipe::Textured && !sampling_target.is_empty() && faults.carried(rng) {
            sets = vec![*rng.pick(&sampling_target)];
            *reaches = true;
        }
        if faults.inject(rng, Error::Shape) {
            push.extend([0; 8]);
        }
        (sets, push)
    }

    /// Where an indirect draw reads its records: a non-indexed one within
    /// the small records at any word, an indexed one there or at the starts
    /// of the indexed records, and a draw of none now and then within the
    /// target.
    fn draw_records(
        rng: &mut Prng,
        indexed: bool,
        (faults, reaches): (&mut Faults, &mut bool),
    ) -> Count {
        let record = u64::from(if indexed {
            INDEXED_DRAW_RECORD_BYTES
        } else {
            DRAW_RECORD_BYTES
        });
        let count = below(rng, 5);
        let (mut offset, mut stride) = if indexed && rng.below(2) == 0 {
            let stride = 20 * (1 + rng.below(2) as u64);
            let room = (RECORD_BYTES - PLAIN_RECORDS - record) / 20;
            let most = u64::from(count.max(1) - 1) * stride / 20;
            let start = rng.at_most(index_of(room.saturating_sub(most))) as u64;
            (PLAIN_RECORDS + 20 * start, stride)
        } else {
            let stride = record + 4 * rng.at_most(4) as u64;
            let span = u64::from(count.max(1) - 1) * stride + record;
            let start = rng.at_most(index_of((PLAIN_RECORDS - span) / 4)) as u64;
            (4 * start, stride)
        };
        if count <= 1 && rng.below(3) == 0 {
            stride = rng.at_most(64) as u64;
        }
        let mut buffer = RB;
        if faults.inject(rng, Error::Usage) {
            buffer = VB;
        } else if faults.inject(rng, Error::Misaligned) {
            offset += 2;
        } else if count > 1 && faults.inject(rng, Error::Misaligned) {
            stride += 2;
        } else if count > 1 && faults.inject(rng, Error::OutOfRange) {
            stride = record - 4;
        } else if faults.inject(rng, Error::OutOfRange) {
            offset = RECORD_BYTES + 4;
        } else if count == 0 && rng.below(4) == 0 {
            // No record read reaches nothing, however far within the target.
            (buffer, offset) = (AB, 4 * u64::from(1 + below(rng, 16)));
        } else if count > 0 && faults.carried(rng) {
            (buffer, offset) = (AB, 0);
            *reaches = true;
        }
        Count::Indirect {
            buffer,
            offset,
            count,
            stride: u32::try_from(stride).expect("a small stride"),
        }
    }

    /// What the reference makes of `works`: where they are refused and why,
    /// or what they write, the images they write, whether one loses the
    /// context, and the direct twin each world but the first records.
    fn expect(&self, works: &[Work]) -> Expected {
        let mut at = 0;
        for work in works {
            let refused = |at, errors: &[Error]| Expected::Refused {
                at,
                errors: errors.to_vec(),
            };
            match work {
                Work::Transfer(recorded) => {
                    if let Err(error) = buffers::judge(&self.memories, &self.shapes, recorded) {
                        return refused(at, &[error]);
                    }
                    at += 1;
                }
                Work::Clear { faults, .. } | Work::Dispatch(DispatchSpec { faults, .. }) => {
                    if !faults.is_empty() {
                        return refused(at, faults);
                    }
                    at += 1;
                }
                Work::Pass(pass) => {
                    if !pass.faults.is_empty() {
                        return refused(at, &pass.faults);
                    }
                    if let Some(k) = pass.draws.iter().position(|draw| !draw.faults.is_empty()) {
                        return refused(at + 1 + k, &pass.draws[k].faults);
                    }
                    if let Some(k) = pass.draws.iter().position(|draw| draw.reaches) {
                        return refused(at + 1 + k, &[Error::Overlap]);
                    }
                    at += 1 + pass.draws.len();
                }
            }
        }
        let mut staged = self.memories.clone();
        let (mut adopted, mut twin) = (Vec::new(), Vec::new());
        for work in works {
            match work {
                Work::Transfer(recorded) => {
                    let planned = buffers::judge(&staged, &self.shapes, recorded)
                        .expect("judged sound above");
                    buffers::land(&mut staged, planned);
                    twin.push(work.clone());
                }
                Work::Clear { .. } => {
                    adopted.push(T);
                    twin.push(work.clone());
                }
                Work::Dispatch(dispatch) => {
                    let counts = match dispatch.groups {
                        Groups::Direct(counts) => counts,
                        Groups::Indirect { buffer, offset } => {
                            let shape = self.shapes[buffer];
                            let read =
                                words_at(&staged[shape.memory], index_of(shape.offset + offset), 3);
                            [read[0], read[1], read[2]]
                        }
                    };
                    let most = &self.limits.max_workgroup_count;
                    if counts.iter().zip(most).any(|(count, most)| count > most) {
                        return Expected::Lost;
                    }
                    self.dispatched(&mut staged, dispatch, counts);
                    twin.push(Work::Dispatch(DispatchSpec {
                        groups: Groups::Direct(counts),
                        ..dispatch.clone()
                    }));
                }
                Work::Pass(pass) => {
                    adopted.extend(pass.colour.iter().map(|&(view, _)| view));
                    adopted.extend(pass.depth.iter().map(|&(view, _)| view));
                    let draws = pass
                        .draws
                        .iter()
                        .flat_map(|draw| self.direct(&staged, draw))
                        .collect();
                    twin.push(Work::Pass(PassSpec {
                        draws,
                        ..pass.clone()
                    }));
                }
            }
        }
        Expected::Admitted {
            staged,
            adopted,
            twin,
        }
    }

    /// The direct draws `draw`'s records name, read from `staged` as the
    /// pass reaches them: `draw` itself where it is direct.
    fn direct(&self, staged: &[Vec<u8>], draw: &DrawSpec) -> Vec<DrawSpec> {
        let Count::Indirect {
            buffer,
            offset,
            count,
            stride,
        } = draw.count
        else {
            return vec![draw.clone()];
        };
        let shape = self.shapes[buffer];
        let indexed = draw.index.is_some();
        (0..u64::from(count))
            .map(|record| {
                let at = index_of(shape.offset + offset + record * u64::from(stride));
                let words = words_at(&staged[shape.memory], at, if indexed { 5 } else { 4 });
                let (base_vertex, first_instance) = if indexed {
                    (words[3].cast_signed(), words[4])
                } else {
                    (0, words[3])
                };
                let end = |first: u32, count: u32| {
                    first
                        .checked_add(count)
                        .expect("records the twin draws directly")
                };
                DrawSpec {
                    count: Count::Direct {
                        positions: words[2]..end(words[2], words[0]),
                        base_vertex,
                        instances: first_instance..end(first_instance, words[1]),
                    },
                    ..draw.clone()
                }
            })
            .collect()
    }

    /// Land in `staged` the words a dispatch of `counts` of the keyed
    /// pipeline stores through its set.
    fn dispatched(&self, staged: &mut [Vec<u8>], dispatch: &DispatchSpec, counts: [u32; 3]) {
        let SetInfo::Stored {
            buffer,
            offset,
            size,
        } = self.sets[dispatch.sets[0]]
        else {
            unreachable!("a sound dispatch binds a stored set");
        };
        let key = u32::from_le_bytes([
            dispatch.push[0],
            dispatch.push[1],
            dispatch.push[2],
            dispatch.push[3],
        ]);
        let shape = self.shapes[buffer];
        let across = counts[0] * 4;
        for z in 0..counts[2] {
            for y in 0..counts[1] {
                for x in 0..across {
                    let place = x + y * across + z * across * counts[1];
                    if u64::from(place) * 4 + 4 > size {
                        continue;
                    }
                    let at = index_of(shape.offset + offset + u64::from(place) * 4);
                    staged[shape.memory][at..at + 4].copy_from_slice(&(place ^ key).to_le_bytes());
                }
            }
        }
    }

    fn record(world: &World, objects: &Objects, works: &[Work]) -> CommandBuffer {
        let mut commands = world.context.command_buffer();
        for work in works {
            let recorded = match work {
                Work::Transfer(recorded) => {
                    let buffers: Vec<&tairix_gpu::Buffer> = objects.buffers.iter().collect();
                    buffers::record(&mut commands, &buffers, recorded);
                    Ok(())
                }
                Work::Clear { value, .. } => commands.clear_image(
                    &objects.images[T],
                    *value,
                    &[ImageRange {
                        aspects: Aspects::COLOR,
                        base_level: 0,
                        levels: 2,
                        base_layer: 0,
                        layers: 1,
                    }],
                ),
                Work::Dispatch(dispatch) => {
                    let sets: Vec<&BindSet> = dispatch
                        .sets
                        .iter()
                        .map(|&set| &objects.sets[set])
                        .collect();
                    match dispatch.groups {
                        Groups::Direct(counts) => {
                            commands.dispatch(&world.keyed, &sets, &dispatch.push, counts)
                        }
                        Groups::Indirect { buffer, offset } => commands.dispatch_indirect(
                            &world.keyed,
                            &sets,
                            &dispatch.push,
                            (&objects.buffers[buffer], offset),
                        ),
                    }
                }
                Work::Pass(pass) => record_pass(&mut commands, (world, objects), pass),
            };
            assert_eq!(recorded, Ok(()), "every drawn command records");
        }
        commands
    }

    /// Submit `works` to the first world, and where the reference admits
    /// them their twin to the second; hold both to the reference.
    fn exchange(&mut self, worlds: &[World; 2], works: &[Work], tally: &mut Tally) -> Exchanged {
        let expected = self.expect(works);
        let commands = Self::record(&worlds[0], &self.objects[0], works);
        let submit = |world: &World, commands: &CommandBuffer| {
            world.queue.submit(&Submission {
                command_buffers: &[commands],
                waits: &[],
                signals: &[],
                deadline: Duration64::from_secs(1),
            })
        };
        match expected {
            Expected::Refused { at, errors } => {
                let Err(refused) = submit(&worlds[0], &commands) else {
                    panic!(
                        "admitted where the reference refuses at {at} for {errors:?}: {works:#?}"
                    );
                };
                assert_eq!(
                    refused.command,
                    Some(at),
                    "refused at {at} for {errors:?}, as {refused:?}: {works:#?}"
                );
                assert!(
                    errors.contains(&refused.error),
                    "refused for {:?} rather than one of {errors:?}: {works:#?}",
                    refused.error
                );
                self.hold_to_reference(&[]);
                tally.refused += 1;
                Exchanged::Refused
            }
            Expected::Lost => {
                let ran = submit(&worlds[0], &commands)
                    .expect("admitted")
                    .wait(Duration64::ZERO);
                assert_eq!(ran, Err(Error::Lost(Loss::Fault)));
                assert_eq!(
                    worlds[0].context.status(),
                    Err(Error::Lost(Loss::Fault)),
                    "counts past the device's, read as the dispatch runs"
                );
                tally.lost += 1;
                Exchanged::Lost
            }
            Expected::Admitted {
                staged,
                adopted,
                twin,
            } => {
                let fence = submit(&worlds[0], &commands).unwrap_or_else(|refused| {
                    panic!("refused as {refused:?} where the reference admits: {works:#?}")
                });
                fence.wait(Duration64::ZERO).expect("completed");
                let twinned = Self::record(&worlds[1], &self.objects[1], &twin);
                submit(&worlds[1], &twinned)
                    .expect("the twin admitted")
                    .wait(Duration64::ZERO)
                    .expect("the twin completed");
                self.memories = staged;
                self.hold_to_reference(&adopted);
                tally.admitted += 1;
                for work in works {
                    match work {
                        Work::Dispatch(DispatchSpec {
                            groups: Groups::Indirect { .. },
                            ..
                        }) => tally.indirect_dispatches += 1,
                        Work::Pass(pass) => {
                            tally.indirect_draws += pass
                                .draws
                                .iter()
                                .filter(|draw| matches!(draw.count, Count::Indirect { .. }))
                                .count();
                        }
                        _ => {}
                    }
                }
                Exchanged::Done
            }
        }
    }

    /// Hold every byte of both worlds' memory alike, and to the reference
    /// but in the spans of `adopted` images, which take the first world's.
    fn hold_to_reference(&mut self, adopted: &[usize]) {
        for memory in 0..self.memories.len() {
            let read = |objects: &Objects| {
                let mut seen = vec![0u8; self.memories[memory].len()];
                objects.allocations[memory]
                    .read(0, &mut seen)
                    .expect("readable");
                seen
            };
            let (first, second) = (read(&self.objects[0]), read(&self.objects[1]));
            assert!(
                first == second,
                "memory {memory} alike in both worlds, at {:?}",
                tairix_fuzzseed::first_difference(&first, &second)
            );
            for &view in adopted {
                let image = if view == TA { T } else { view };
                let (held, span) = &self.spans[image];
                if *held == memory {
                    self.memories[memory][span.clone()].copy_from_slice(&first[span.clone()]);
                }
            }
            assert!(
                first == self.memories[memory],
                "memory {memory} as the reference holds it, at {:?}",
                tairix_fuzzseed::first_difference(&first, &self.memories[memory])
            );
        }
    }
}

/// What the reference makes of a submission.
enum Expected {
    Refused {
        at: usize,
        errors: Vec<Error>,
    },
    Lost,
    Admitted {
        staged: Vec<Vec<u8>>,
        adopted: Vec<usize>,
        twin: Vec<Work>,
    },
}

/// A draw's vertex buffer, or one alteration of it: none, one not made for
/// vertices, an offset past it, or one over what the pass renders to.
fn draw_vertex_buffers(
    rng: &mut Prng,
    (faults, reaches): (&mut Faults, &mut bool),
) -> Vec<(usize, u64)> {
    let mut vertex_buffers = vec![(VB, VERTEX_BYTES * u64::from(below(rng, 3)))];
    if faults.inject(rng, Error::Bindings) {
        vertex_buffers.clear();
    } else if faults.inject(rng, Error::Usage) {
        vertex_buffers[0].0 = RB;
    } else if faults.inject(rng, Error::OutOfRange) {
        vertex_buffers[0] = (VB, u64::from(VERTICES) * VERTEX_BYTES + 4);
    } else if faults.carried(rng) {
        vertex_buffers[0] = (AB, 0);
        *reaches = true;
    }
    vertex_buffers
}

/// A draw's viewport, scissor and blend constant, each of them past what the
/// device takes now and then.
fn draw_placing(rng: &mut Prng, faults: &mut Faults) -> (Viewport, Rect, [f32; 4]) {
    let mut viewport = draw_viewport(rng);
    if faults.inject(rng, Error::OutOfRange) {
        viewport.x = f32::NAN;
    }
    if faults.inject(rng, Error::OutOfRange) {
        viewport.height = 0.0;
    }
    if faults.inject(rng, Error::OutOfRange) {
        viewport.max_depth = 1.5;
    }
    let mut scissor = draw_scissor(rng);
    if faults.inject(rng, Error::OutOfRange) {
        scissor = Rect {
            x: u32::MAX,
            width: 2,
            ..scissor
        };
    }
    let mut blend_constant = [0.5; 4];
    if faults.inject(rng, Error::OutOfRange) {
        blend_constant[rng.below(4)] = f32::NAN;
    }
    (viewport, scissor, blend_constant)
}

/// An indexed draw's indices, of either width, or one alteration of them: a
/// buffer not made for indices, an offset off an index or past them, or one
/// over what the pass renders to.
fn draw_index_buffer(
    rng: &mut Prng,
    (faults, reaches): (&mut Faults, &mut bool),
) -> (usize, u64, IndexFormat) {
    let format = *rng.pick(&[IndexFormat::U16, IndexFormat::U32]);
    let offset = match format {
        IndexFormat::U16 => 2 * u64::from(below(rng, 64)),
        IndexFormat::U32 => INDEX_BYTES / 2 + 4 * u64::from(below(rng, 32)),
    };
    let mut index = (IB, offset, format);
    if faults.inject(rng, Error::Usage) {
        index = (VB, 0, format);
    } else if faults.inject(rng, Error::Misaligned) {
        index.1 = 1;
    } else if faults.inject(rng, Error::OutOfRange) {
        index.1 = INDEX_BYTES + 4;
    } else if faults.carried(rng) {
        index = (AB, 0, format);
        *reaches = true;
    }
    index
}

/// A direct draw's positions and instances, of few, or a range of them
/// reversed now and then.
fn draw_direct_count(rng: &mut Prng, indexed: bool, faults: &mut Faults) -> Count {
    let first = below(rng, if indexed { 41 } else { 13 });
    let mut positions = first..first + below(rng, 13);
    let first_instance = below(rng, 4);
    let mut instances = first_instance..first_instance + below(rng, 4);
    if faults.inject(rng, Error::OutOfRange) {
        if rng.below(2) == 0 {
            positions = positions.end + 1..positions.end;
        } else {
            instances = instances.end + 1..instances.end;
        }
    }
    Count::Direct {
        positions,
        base_vertex: if indexed {
            below(rng, 9).cast_signed() - 4
        } else {
            0
        },
        instances,
    }
}

/// A transfer the reference judges exactly: an update of records or counts
/// whose words keep every reader small, or a fill, copy or update among the
/// vertex, index and storage buffers.
fn draw_transfer(rng: &mut Prng, shapes: &[Shape], most: [u32; 3]) -> Recorded {
    match rng.below(4) {
        0 => {
            let (offset, data) = if rng.below(2) == 0 {
                let slot = rng.below(index_of(PLAIN_RECORDS / 16)) as u64;
                (slot * 16, words_bytes(&draw_plain_record(rng)))
            } else {
                let slot = rng.below(index_of((RECORD_BYTES - PLAIN_RECORDS) / 20)) as u64;
                (
                    PLAIN_RECORDS + slot * 20,
                    words_bytes(&draw_indexed_record(rng)),
                )
            };
            let offset = if rng.below(10) == 0 {
                offset + 2
            } else {
                offset
            };
            Recorded::Update {
                dst: RB,
                offset,
                data,
            }
        }
        1 => Recorded::Update {
            dst: DB,
            offset: 12 * rng.below(5) as u64,
            data: words_bytes(&draw_counts(rng, (PAST_ONE_IN, most))),
        },
        _ => {
            let among = [VB, IB, SB];
            let movable: Vec<Shape> = among.iter().map(|&buffer| shapes[buffer]).collect();
            match buffers::draw_command(rng, &movable) {
                Recorded::Copy { src, dst, regions } => Recorded::Copy {
                    src: among[src],
                    dst: among[dst],
                    regions,
                },
                Recorded::Fill {
                    dst,
                    offset,
                    size,
                    word,
                } => Recorded::Fill {
                    dst: among[dst],
                    offset,
                    size,
                    word,
                },
                Recorded::Update { dst, offset, data } => Recorded::Update {
                    dst: among[dst],
                    offset,
                    data,
                },
            }
        }
    }
}

/// Record `pass` and its draws as the first world's objects or the second's
/// name them.
fn record_pass(
    commands: &mut CommandBuffer,
    (world, objects): (&World, &Objects),
    pass: &PassSpec,
) -> Result<(), Error> {
    let colour: Vec<ColourAttachment<'_>> = pass
        .colour
        .iter()
        .map(|&(view, load)| ColourAttachment {
            view: &objects.views[view],
            resolve: None,
            load,
        })
        .collect();
    let depth_stencil = pass.depth.map(|(view, load)| DepthStencilAttachment {
        view: &objects.views[view],
        depth_load: load,
        stencil_load: load,
    });
    commands.render_pass(
        &Pass {
            colour: &colour,
            depth_stencil,
            area: pass.area,
        },
        |drawer| {
            for draw in &pass.draws {
                let sets: Vec<&BindSet> = draw.sets.iter().map(|&set| &objects.sets[set]).collect();
                let vertex_buffers: Vec<(&tairix_gpu::Buffer, u64)> = draw
                    .vertex_buffers
                    .iter()
                    .map(|&(buffer, offset)| (&objects.buffers[buffer], offset))
                    .collect();
                let recorded = Draw {
                    pipeline: world.pipeline(draw.pipe),
                    sets: &sets,
                    push_constants: &draw.push,
                    vertex_buffers: &vertex_buffers,
                    viewport: draw.viewport,
                    scissor: draw.scissor,
                    stencil_reference: 0,
                    blend_constant: draw.blend_constant,
                };
                let indices = draw.index.map(|(buffer, offset, format)| Indices {
                    buffer: &objects.buffers[buffer],
                    offset,
                    format,
                });
                match (&draw.count, &indices) {
                    (
                        Count::Direct {
                            positions,
                            instances,
                            ..
                        },
                        None,
                    ) => drawer.draw(&recorded, positions.clone(), instances.clone())?,
                    (
                        Count::Direct {
                            positions,
                            base_vertex,
                            instances,
                        },
                        Some(indices),
                    ) => drawer.draw_indexed(
                        &recorded,
                        indices,
                        (positions.clone(), *base_vertex),
                        instances.clone(),
                    )?,
                    (
                        &Count::Indirect {
                            buffer,
                            offset,
                            count,
                            stride,
                        },
                        indices,
                    ) => {
                        let records = Indirect {
                            buffer: &objects.buffers[buffer],
                            offset,
                            count,
                            stride,
                        };
                        match indices {
                            Some(indices) => {
                                drawer.draw_indexed_indirect(&recorded, indices, &records)?;
                            }
                            None => drawer.draw_indirect(&recorded, &records)?,
                        }
                    }
                }
            }
            Ok(())
        },
    )
}
