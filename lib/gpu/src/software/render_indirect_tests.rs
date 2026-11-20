//! Indirect draws on the software device: each record drawn as the direct
//! draw it names, in order, read as its pass reaches it, its vertex and
//! instance numbers wrapping and its indices' place not, a first instance the
//! device does not state lost as a fault, and its records counted against the
//! deadline.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;
use core::sync::atomic::Ordering;

use tairix_spirv::assemble::admit;

use super::tests::ADD;
use crate::device_tests::{context, memory, over, queue, submit};
use crate::render::tests::{bare, painting, passing, read, real, target, whole, Target, VEC4};
use crate::render::tests::{FILLED, ONE_SAMPLE};
use crate::{
    Blend, ClearValue, ColourAttachment, ColourMask, ColourTarget, Context, Draw, Drawer, Error,
    Format, IndexFormat, Indices, Indirect, Load, Loss, Pass, RenderPipeline, RenderPipelineDesc,
    StageDesc, Topology, VertexAttribute, VertexBuffer, VertexStep, DRAW_RECORD_BYTES,
    INDEXED_DRAW_RECORD_BYTES,
};

/// The side of the target the rig draws into.
const SIDE: u32 = 16;

/// The colours the rig's instances take, one apiece, each a sum no other
/// set of them makes.
const INSTANCE_COLOURS: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 1.0],
    [0.0, 2.0, 0.0, 1.0],
    [0.0, 0.0, 4.0, 1.0],
    [8.0, 0.0, 0.0, 1.0],
];

/// Three triangles over the target, overlapping, their corners in pixels.
const TRIANGLES: [[(u32, u32); 3]; 3] = [
    [(1, 1), (9, 1), (1, 9)],
    [(5, 5), (15, 5), (5, 15)],
    [(2, 12), (10, 4), (14, 14)],
];

/// Indices naming the triangles: in order, again from the fourth, the last
/// reversed, and three past every vertex.
const INDICES: [u32; 15] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 7, 6, 12, 13, 14];

/// A pipeline drawing each vertex's position with its instance's colour into
/// a float target, blended as `blend` says, and the buffers it draws from.
struct Rig {
    pipeline: RenderPipeline,
    target: Target,
    _held: Vec<crate::Allocation>,
    positions: crate::Buffer,
    colours: crate::Buffer,
}

/// The position of pixel corner `(x, y)` of the target, at middle depth.
fn ndc((x, y): (u32, u32)) -> [f32; 4] {
    let side = real(SIDE);
    [
        real(x) / side * 2.0 - 1.0,
        real(y) / side * 2.0 - 1.0,
        0.5,
        1.0,
    ]
}

/// A buffer of `bytes`, made for every use, its memory held in `held`.
fn holding(context: &Context, held: &mut Vec<crate::Allocation>, bytes: &[u8]) -> crate::Buffer {
    let size = (bytes.len() as u64).next_multiple_of(16).max(16);
    let memory = memory(context, size);
    memory.write(0, bytes).expect("written");
    let buffer = over(context, &memory, 0, size);
    held.push(memory);
    buffer
}

fn floats_bytes(floats: impl IntoIterator<Item = f32>) -> Vec<u8> {
    floats.into_iter().flat_map(f32::to_le_bytes).collect()
}

fn words_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

impl Rig {
    fn new(context: &Context, blend: Option<Blend>) -> Self {
        let layout = bare(context);
        let vertex = context
            .create_shader_module(admit(&passing(VEC4, "")).expect("admitted"))
            .expect("a module");
        let fragment = context
            .create_shader_module(admit(&painting("")).expect("admitted"))
            .expect("a module");
        let targets = [ColourTarget {
            format: Format::Rgba32Float,
            blend,
            write_mask: ColourMask::ALL,
        }];
        let stage = |module| StageDesc {
            module,
            entry_point: "main",
            specialization: &[],
        };
        let position = [VertexAttribute {
            location: 0,
            format: Format::Rgba32Float,
            offset: 0,
        }];
        let colour = [VertexAttribute {
            location: 1,
            format: Format::Rgba32Float,
            offset: 0,
        }];
        let pipeline = context
            .create_render_pipeline(&RenderPipelineDesc {
                layout: &layout,
                vertex: stage(&vertex),
                fragment: Some(stage(&fragment)),
                vertex_buffers: &[
                    VertexBuffer {
                        stride: 16,
                        step: VertexStep::Vertex,
                        attributes: &position,
                    },
                    VertexBuffer {
                        stride: 16,
                        step: VertexStep::Instance,
                        attributes: &colour,
                    },
                ],
                topology: Topology::Triangles,
                primitive_restart: false,
                rasterization: FILLED,
                depth_stencil: None,
                multisample: ONE_SAMPLE,
                targets: &targets,
                logic_op: None,
            })
            .expect("a pipeline");
        let mut held = Vec::new();
        let corners = TRIANGLES.iter().flatten().flat_map(|&corner| ndc(corner));
        let positions = holding(context, &mut held, &floats_bytes(corners));
        let colours = holding(
            context,
            &mut held,
            &floats_bytes(INSTANCE_COLOURS.iter().flatten().copied()),
        );
        Self {
            pipeline,
            target: target(context, Format::Rgba32Float, (SIDE, SIDE), 1),
            _held: held,
            positions,
            colours,
        }
    }

    /// The target's bytes after one pass, cleared first, of the draws
    /// `record` records with the rig's `draw`.
    fn drawn(
        &self,
        context: &Context,
        record: &dyn Fn(&mut Drawer<'_>, &Draw<'_>) -> Result<(), Error>,
    ) -> Result<Vec<u8>, Error> {
        let mut commands = context.command_buffer();
        self.record_pass(&mut commands, record)?;
        submit(&queue(context), &commands)
            .map_err(|refused| refused.error)?
            .wait(tairix_abi::Duration64::ZERO)?;
        Ok(read(context, &self.target))
    }

    fn record_pass(
        &self,
        commands: &mut crate::CommandBuffer,
        record: &dyn Fn(&mut Drawer<'_>, &Draw<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let (viewport, scissor) = whole((SIDE, SIDE));
        let vertex_buffers = [(&self.positions, 0), (&self.colours, 0)];
        let draw = Draw {
            pipeline: &self.pipeline,
            sets: &[],
            push_constants: &[],
            vertex_buffers: &vertex_buffers,
            viewport,
            scissor,
            stencil_reference: 0,
            blend_constant: [0.0; 4],
        };
        commands.render_pass(
            &Pass {
                colour: &[ColourAttachment {
                    view: &self.target.view,
                    resolve: None,
                    load: Load::Clear(ClearValue::Float([0.0; 4])),
                }],
                depth_stencil: None,
                area: scissor,
            },
            |drawer| record(drawer, &draw),
        )
    }
}

/// Records of `words` each, laid `stride` bytes apart, the room between them
/// filled with a pattern no record reads.
fn laid(records: &[&[u32]], stride: u32) -> Vec<u8> {
    let stride = stride as usize;
    let mut bytes = alloc::vec![0xa5u8; records.len() * stride];
    for (at, record) in records.iter().enumerate() {
        let words = words_bytes(record);
        bytes[at * stride..at * stride + words.len()].copy_from_slice(&words);
    }
    bytes
}

/// Draws of vertices as their records name them: the count, the instances,
/// the first vertex and the first instance.
const DRAWS: [[u32; 4]; 5] = [
    [3, 2, 0, 1],
    [0, 5, 3, 0],
    [6, 1, 3, 0],
    [3, 0, 6, 0],
    [4, 1, 5, 3],
];

/// Indexed draws as their records name them: the count, the instances, the
/// first index, the word added to each index and the first instance.
const INDEXED_DRAWS: [[u32; 5]; 5] = [
    [3, 1, 0, 0, 0],
    [6, 2, 3, (-3i32).cast_unsigned(), 1],
    [3, 1, 9, 0, 3],
    [3, 1, 12, 0, 0],
    [0, 3, 0, 0, 0],
];

/// The positions and instances a record of `count` from `first` names, as a
/// direct draw takes them.
fn ranges(count: u32, first: u32, instances: u32, first_instance: u32) -> (Range<u32>, Range<u32>) {
    (
        first..first + count,
        first_instance..first_instance + instances,
    )
}

/// How many texels of a float target's `bytes` hold anything.
fn painted(bytes: &[u8]) -> usize {
    bytes
        .as_chunks::<16>()
        .0
        .iter()
        .filter(|texel| texel.iter().any(|&byte| byte != 0))
        .count()
}

#[test]
fn each_record_draws_what_the_direct_draw_it_names_draws() {
    let device = crate::render::tests::rendering();
    let context = context(&device);
    for blend in [Some(ADD), None] {
        let rig = Rig::new(&context, blend);
        let mut held = Vec::new();
        let stride = DRAW_RECORD_BYTES + 8;
        let draws: Vec<&[u32]> = DRAWS.iter().map(|record| &record[..]).collect();
        let records = holding(&context, &mut held, &laid(&draws, stride));
        let indirect = rig
            .drawn(&context, &|drawer, draw| {
                drawer.draw_indirect(
                    draw,
                    &Indirect {
                        buffer: &records,
                        offset: 0,
                        count: 5,
                        stride,
                    },
                )
            })
            .expect("drawn");
        let direct = rig
            .drawn(&context, &|drawer, draw| {
                for [count, instances, first, first_instance] in DRAWS {
                    let (vertices, instances) = ranges(count, first, instances, first_instance);
                    drawer.draw(draw, vertices, instances)?;
                }
                Ok(())
            })
            .expect("drawn");
        assert!(painted(&direct) > 0, "the draws paint");
        assert!(indirect == direct, "draws of vertices, blended: {blend:?}");

        for format in [IndexFormat::U16, IndexFormat::U32] {
            let index_bytes: Vec<u8> = match format {
                IndexFormat::U16 => INDICES
                    .iter()
                    .flat_map(|&index| u16::try_from(index).expect("small").to_le_bytes())
                    .collect(),
                IndexFormat::U32 => words_bytes(&INDICES),
            };
            let index_buffer = holding(&context, &mut held, &index_bytes);
            let indices = Indices {
                buffer: &index_buffer,
                offset: 0,
                format,
            };
            let stride = INDEXED_DRAW_RECORD_BYTES + 8;
            let draws: Vec<&[u32]> = INDEXED_DRAWS.iter().map(|record| &record[..]).collect();
            let records = holding(&context, &mut held, &laid(&draws, stride));
            let indirect = rig
                .drawn(&context, &|drawer, draw| {
                    drawer.draw_indexed_indirect(
                        draw,
                        &indices,
                        &Indirect {
                            buffer: &records,
                            offset: 0,
                            count: 5,
                            stride,
                        },
                    )
                })
                .expect("drawn");
            let direct = rig
                .drawn(&context, &|drawer, draw| {
                    for [count, instances, first, base, first_instance] in INDEXED_DRAWS {
                        let (range, instances) = ranges(count, first, instances, first_instance);
                        drawer.draw_indexed(
                            draw,
                            &indices,
                            (range, base.cast_signed()),
                            instances,
                        )?;
                    }
                    Ok(())
                })
                .expect("drawn");
            assert!(painted(&direct) > 0, "the indexed draws paint");
            assert!(
                indirect == direct,
                "indexed draws of {format:?}, blended: {blend:?}"
            );
        }
    }
}

#[test]
fn no_records_draw_nothing_and_one_record_s_stride_goes_unread() {
    let device = crate::render::tests::rendering();
    let context = context(&device);
    let rig = Rig::new(&context, Some(ADD));
    let mut held = Vec::new();
    let records = holding(&context, &mut held, &words_bytes(&[3, 1, 0, 0]));
    let cleared = rig.drawn(&context, &|_, _| Ok(())).expect("drawn");
    let one = rig
        .drawn(&context, &|drawer, draw| drawer.draw(draw, 0..3, 0..1))
        .expect("drawn");
    for (count, stride, expected) in [(0, 0, &cleared), (1, 0, &one), (1, 7, &one)] {
        let drawn = rig
            .drawn(&context, &|drawer, draw| {
                drawer.draw_indirect(
                    draw,
                    &Indirect {
                        buffer: &records,
                        offset: 0,
                        count,
                        stride,
                    },
                )
            })
            .expect("drawn");
        assert!(drawn == *expected, "{count} records {stride} bytes apart");
    }
}

#[test]
fn a_record_is_read_as_its_pass_reaches_it() {
    let device = crate::render::tests::rendering();
    let context = context(&device);
    let rig = Rig::new(&context, Some(ADD));
    let mut held = Vec::new();
    let records = holding(&context, &mut held, &[0; 16]);
    let one = Indirect {
        buffer: &records,
        offset: 0,
        count: 1,
        stride: 0,
    };
    let mut commands = context.command_buffer();
    commands
        .update_buffer(&records, 0, &words_bytes(&[3, 1, 0, 0]))
        .expect("recorded");
    rig.record_pass(&mut commands, &|drawer, draw| {
        drawer.draw_indirect(draw, &one)
    })
    .expect("recorded");
    submit(&queue(&context), &commands).expect("drawn");
    let first = read(&context, &rig.target);
    let direct = rig
        .drawn(&context, &|drawer, draw| drawer.draw(draw, 0..3, 0..1))
        .expect("drawn");
    assert!(first == direct, "the record an earlier command wrote");

    let mut commands = context.command_buffer();
    rig.record_pass(&mut commands, &|drawer, draw| {
        drawer.draw_indirect(draw, &one)
    })
    .expect("recorded");
    held[0]
        .write(0, &words_bytes(&[6, 1, 3, 0]))
        .expect("written");
    submit(&queue(&context), &commands).expect("drawn");
    let second = read(&context, &rig.target);
    let direct = rig
        .drawn(&context, &|drawer, draw| drawer.draw(draw, 3..9, 0..1))
        .expect("drawn");
    assert!(second == direct, "the record written after it was recorded");
}

#[test]
fn vertex_and_instance_numbers_wrap_and_an_index_s_place_does_not() {
    let device = crate::render::tests::rendering();
    let context = context(&device);
    let rig = Rig::new(&context, Some(ADD));
    let mut held = Vec::new();
    let one = |buffer| Indirect {
        buffer,
        offset: 0,
        count: 1,
        stride: 0,
    };
    let past_vertices = holding(&context, &mut held, &words_bytes(&[6, 1, u32::MAX - 2, 0]));
    let wrapped = rig
        .drawn(&context, &|drawer, draw| {
            drawer.draw_indirect(draw, &one(&past_vertices))
        })
        .expect("drawn");
    let direct = rig
        .drawn(&context, &|drawer, draw| drawer.draw(draw, 0..3, 0..1))
        .expect("drawn");
    assert!(painted(&direct) > 0, "the triangle paints");
    assert!(
        wrapped == direct,
        "the vertices past the last wrap to the first"
    );

    let past_instances = holding(&context, &mut held, &words_bytes(&[3, 2, 0, u32::MAX]));
    let wrapped = rig
        .drawn(&context, &|drawer, draw| {
            drawer.draw_indirect(draw, &one(&past_instances))
        })
        .expect("drawn");
    // The last instance's colour lies past its buffer, as the hundredth's does.
    let direct = rig
        .drawn(&context, &|drawer, draw| {
            drawer.draw(draw, 0..3, 100..101)?;
            drawer.draw(draw, 0..3, 0..1)
        })
        .expect("drawn");
    assert!(
        wrapped == direct,
        "the instance past the last wraps to the first"
    );

    let index_bytes = words_bytes(&INDICES);
    let index_buffer = holding(&context, &mut held, &index_bytes);
    let indices = Indices {
        buffer: &index_buffer,
        offset: 0,
        format: IndexFormat::U32,
    };
    let past_indices = holding(
        &context,
        &mut held,
        &words_bytes(&[6, 1, u32::MAX - 2, 0, 0]),
    );
    let unwrapped = rig
        .drawn(&context, &|drawer, draw| {
            drawer.draw_indexed_indirect(draw, &indices, &one(&past_indices))
        })
        .expect("drawn");
    let cleared = rig.drawn(&context, &|_, _| Ok(())).expect("drawn");
    assert!(
        unwrapped == cleared,
        "indices past their buffer read nought, however far past"
    );
}

#[test]
fn a_record_naming_a_first_instance_the_device_does_not_state_loses_its_context() {
    let device = crate::binding::tests::device_stating(|info| {
        info.features = info
            .features
            .without(crate::Features::DRAW_INDIRECT_FIRST_INSTANCE);
    });
    let context = context(&device);
    let rig = Rig::new(&context, Some(ADD));
    let mut held = Vec::new();
    let records = holding(
        &context,
        &mut held,
        &laid(&[&[3, 1, 0, 0], &[3, 2, 0, 1]], DRAW_RECORD_BYTES),
    );
    let first = |count| Indirect {
        buffer: &records,
        offset: 0,
        count,
        stride: DRAW_RECORD_BYTES,
    };
    let drawn = rig
        .drawn(&context, &|drawer, draw| {
            drawer.draw_indirect(draw, &first(1))
        })
        .expect("a first instance of nought is drawn");
    let direct = rig
        .drawn(&context, &|drawer, draw| drawer.draw(draw, 0..3, 0..1))
        .expect("drawn");
    assert!(drawn == direct, "a first instance of nought");
    assert_eq!(
        rig.drawn(&context, &|drawer, draw| drawer
            .draw_indirect(draw, &first(2)))
            .err(),
        Some(Error::Lost(Loss::Fault)),
        "a record naming its first instance"
    );
    assert_eq!(context.status(), Err(Error::Lost(Loss::Fault)));
}

#[test]
fn an_indirect_draw_s_records_are_counted_against_its_deadline() {
    let clock = Arc::new(crate::device_tests::Clock::default());
    let device = crate::device_tests::software(32 << 20, Arc::clone(&clock) as Arc<_>);
    let context = context(&device);
    let rig = Rig::new(&context, Some(ADD));
    // Records of no vertices, as many as the clock is read past.
    let count = u32::try_from(crate::software::WORK_PER_CHECK / 4 + 1024).expect("a count");
    let size = u64::from(count) * u64::from(DRAW_RECORD_BYTES);
    let empty = memory(&context, size);
    let records = over(&context, &empty, 0, size);
    let read_records = |count| Indirect {
        buffer: &records,
        offset: 0,
        count,
        stride: DRAW_RECORD_BYTES,
    };
    // Every look at the clock passes the deadline.
    clock.step.store(10 * 1_000_000_000, Ordering::SeqCst);
    rig.drawn(&context, &|drawer, draw| {
        drawer.draw_indirect(draw, &read_records(1024))
    })
    .expect("too few records to look at the clock");
    assert_eq!(
        rig.drawn(&context, &|drawer, draw| {
            drawer.draw_indirect(draw, &read_records(count))
        })
        .err(),
        Some(Error::Lost(Loss::Overran)),
        "records enough to look at the clock"
    );
}
