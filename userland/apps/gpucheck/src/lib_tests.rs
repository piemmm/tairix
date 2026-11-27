//! The work held to two software devices, which agree, and the software
//! device's render work held to what Vulkan defines for its draws; a device
//! that runs transfers alone, refused at the first pass, and one whose first
//! queue family runs transfers alone, whose dispatches run on its second; and
//! each kind of difference found where it first is.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;
use std::sync::Mutex;

use tairix_abi::time::MonotonicClock;
use tairix_gpu::{
    BufferViewDesc, Commands, Device, DeviceInfo, Error, Features, FormatFeatures, FormatTable,
    Hardware, ImageDesc, ImageViewDesc, Job, JobCommand, LayoutBinding, Loss, MemoryProperties,
    MemoryRequirements, PipelineStage, QueueCapabilities, RenderStages, SamplerDesc, SetResource,
};
use tairix_shaders::SET;

use super::{
    clearing, command, compare, family, reference, work, Answer, Command, CommandError, Difference,
    Refusal, Rendered, DEFAULT_WORDS, MAX_WORDS, PAST_COUNTS, TRANSFERS,
};

struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

fn software() -> Device {
    reference(Arc::new(Still)).expect("a software device")
}

fn expected() -> Answer {
    work(&software()).expect("the reference ran")
}

/// Memory of the test's own device.
#[derive(Debug)]
struct Bytes(Mutex<Vec<u8>>);

/// A device running transfers on the CPU, whose sets and compute pipelines
/// are made and whose dispatches do nothing.
struct Transfers;

fn index(at: u64) -> usize {
    usize::try_from(at).expect("small")
}

impl Hardware for Transfers {
    type Context = ();
    type Memory = Bytes;
    type Image = Infallible;
    type View = Infallible;
    type BufferView = Infallible;
    type Sampler = Infallible;
    type Set = ();
    type Compute = ();
    type Render = Infallible;

    fn create_context(&self) -> Result<(), Error> {
        Ok(())
    }

    fn allocate(&self, _: &Arc<()>, _: u32, size: u64) -> Result<Bytes, Error> {
        Ok(Bytes(Mutex::new(vec![0; index(size)])))
    }

    fn write(&self, memory: &Bytes, offset: u64, data: &[u8]) -> Result<(), Error> {
        let at = index(offset);
        memory.0.lock().expect("unpoisoned")[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn read(&self, memory: &Bytes, offset: u64, out: &mut [u8]) -> Result<(), Error> {
        let at = index(offset);
        out.copy_from_slice(&memory.0.lock().expect("unpoisoned")[at..at + out.len()]);
        Ok(())
    }

    fn image(&self, _: &Arc<()>, _: &ImageDesc) -> Result<(Infallible, MemoryRequirements), Error> {
        Err(Error::Unsupported)
    }

    fn place(&self, image: &Infallible, _: &Bytes, _: u64) -> Result<(), Error> {
        match *image {}
    }

    fn view(
        &self,
        image: &Infallible,
        _: &ImageDesc,
        _: &ImageViewDesc,
    ) -> Result<Infallible, Error> {
        match *image {}
    }

    fn buffer_view(&self, _: &Bytes, _: u64, _: &BufferViewDesc) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn sampler(&self, _: &Arc<()>, _: &SamplerDesc) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn set(
        &self,
        _: &Arc<()>,
        _: &[LayoutBinding],
        _: &[SetResource<'_, Self>],
    ) -> Result<(), Error> {
        Ok(())
    }

    fn compute(&self, _: &Arc<()>, _: PipelineStage<'_>) -> Result<(), Error> {
        Ok(())
    }

    fn render(&self, _: &Arc<()>, _: RenderStages<'_>) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn run(&self, job: Job<Self>) {
        for command in job.commands().iter() {
            match command {
                JobCommand::Copy { src, dst, spans } => {
                    for span in spans {
                        let mut bytes = vec![0; index(span.len)];
                        let _ = self.read(src, span.src, &mut bytes);
                        let _ = self.write(dst, span.dst, &bytes);
                    }
                }
                JobCommand::Fill {
                    dst,
                    offset,
                    size,
                    word,
                } => {
                    let bytes: Vec<u8> = (0..size / 4).flat_map(|_| word.to_le_bytes()).collect();
                    let _ = self.write(dst, offset, &bytes);
                }
                JobCommand::Update { dst, offset, data } => {
                    let _ = self.write(dst, offset, data);
                }
                JobCommand::BufferImage { image, .. }
                | JobCommand::ClearImage { image, .. }
                | JobCommand::CopyImage { src: image, .. } => match *image {},
                JobCommand::Dispatch { .. } => {}
                JobCommand::Pass {
                    targets,
                    depth_stencil,
                    ..
                } => {
                    if let Some(target) = targets.iter().next() {
                        match *target.view {}
                    }
                    if let Some(target) = depth_stencil {
                        match *target.view {}
                    }
                }
            }
        }
        job.finish(Ok(()));
    }

    fn run_bytes(_: Commands<'_, Self>) -> u64 {
        0
    }

    fn lost(&self, _: &Arc<()>) {}
}

/// The test's device: the software device's description narrowed to
/// transfers, its one memory type host-visible.
fn transfers() -> Device {
    let reference = software();
    let mut info: DeviceInfo = reference.info().clone();
    info.capabilities.clear();
    info.features = Features::NONE;
    info.formats = FormatTable::from_fn(|_| FormatFeatures::NONE);
    for family in &mut info.queue_families {
        family.capabilities = QueueCapabilities::TRANSFER;
    }
    assert!(info.memory_types[0]
        .properties
        .contains(MemoryProperties::HOST_VISIBLE));
    Device::hardware(Transfers, info, &[1], Arc::new(Still)).expect("a device")
}

/// A device whose first queue family runs transfers alone and whose second
/// runs every kind of work, stating no format's use beside copying it.
fn transferring_first() -> Device {
    let mut info: DeviceInfo = software().info().clone();
    info.formats = FormatTable::from_fn(|_| FormatFeatures::NONE);
    let every = info.queue_families[0];
    info.queue_families.clear();
    for capabilities in [QueueCapabilities::TRANSFER, QueueCapabilities::ALL] {
        let mut family = every;
        family.capabilities = capabilities;
        info.queue_families
            .try_push(family)
            .expect("room for two families");
    }
    Device::hardware(Transfers, info, &[1, 1], Arc::new(Still)).expect("a device")
}

#[test]
fn a_clear_is_never_given_a_family_that_transfers_nothing() {
    let mut info: DeviceInfo = software().info().clone();
    let every = info.queue_families[0];
    info.queue_families.clear();
    for capabilities in [QueueCapabilities::COMPUTE, QueueCapabilities::ALL] {
        let mut family = every;
        family.capabilities = capabilities;
        info.queue_families
            .try_push(family)
            .expect("room for two families");
    }
    let device = Device::hardware(Transfers, info, &[1, 1], Arc::new(Still)).expect("a device");
    assert_eq!(clearing(&device), Ok(1));
}

#[test]
fn each_step_runs_on_the_first_queue_family_stating_its_work() {
    let device = transferring_first();
    assert_eq!(family(&device, QueueCapabilities::TRANSFER), Ok(0));
    assert_eq!(family(&device, QueueCapabilities::COMPUTE), Ok(1));
    assert_eq!(family(&device, QueueCapabilities::GRAPHICS), Ok(1));
    assert_eq!(
        family(&transfers(), QueueCapabilities::COMPUTE),
        Err(Error::Unsupported)
    );
    let answer = work(&device).expect("every dispatch is submitted to the second family");
    let families: Vec<u32> = answer.transfers.iter().map(|left| left.family).collect();
    assert_eq!(
        families,
        [0, 1],
        "the transfers run on every family stating them"
    );
    for (pinned, left) in SET.iter().zip(&answer.passes) {
        let sample = (pinned.sample)().expect("a sample");
        assert_eq!(*left, sample.buffers, "{} ran, doing nothing", pinned.name);
    }
    assert_eq!((answer.images, answer.render), (None, None));
    assert_eq!(answer.past_counts, Some(Ok(())));
}

/// What the render work's first three passes leave by Vulkan's rules: the
/// colour target, the tags, the stencil and the float target, each a plane
/// of 16 × 16 read back in turn.
fn passes_by_the_rules() -> Vec<u8> {
    const SIDE: u32 = 16;
    let (mut colours, mut tags, mut stencils, mut floats) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for y in 0..SIDE {
        for x in 0..SIDE {
            let inside = |[left, top, right, bottom]: [u32; 4]| {
                (left..right).contains(&x) && (top..bottom).contains(&y)
            };
            let place = y * SIDE + x;
            // The first pass's area ends a row short of its images.
            let (mut colour, mut tag, mut stencil) = ([255u8; 4], 9, 2u8);
            if y < SIDE - 1 {
                (colour, tag, stencil) = ([0, 0, 0, 255], 7, 0);
                let (first, second) = (inside([2, 2, 10, 10]), inside([6, 6, 14, 14]));
                if first {
                    (colour, tag, stencil) = ([255, 0, 0, 255], place, 5);
                } else if second {
                    (colour, tag, stencil) = ([0, 0, 255, 255], 256 + place, 5);
                }
                if first && second {
                    // Behind the first: counted up.
                    stencil = 6;
                }
                // The mirrored rectangle at [8, 1, 12, 5] is culled.
                if inside([0, 8, 4, 11]) || inside([10, 0, 16, 4]) {
                    if stencil == 0 {
                        (colour, tag, stencil) = ([0, 255, 0, 255], 3 * 256 + place, 1);
                    } else {
                        stencil = !stencil;
                    }
                }
                if inside([1, 12, 9, 15]) {
                    for (channel, added) in colour.iter_mut().zip([0, 255, 255, 0]) {
                        *channel = channel.saturating_add(added);
                    }
                }
            }
            if inside([12, 0, 16, 4]) {
                (colour[0], colour[3]) = (255, 0);
            }
            if inside([14, 5, 16, 9]) {
                colour = [0, 255, 255, 255];
            }
            if inside([0, 0, 2, 4]) || inside([14, 10, 16, 14]) {
                colour = [255, 255, 0, 255];
            }
            let corner = [0, SIDE - 1].contains(&x) && [0, SIDE - 1].contains(&y);
            let float: f32 = if [3, 11].contains(&x) && [11, 13].contains(&y) {
                0.25
            } else if corner {
                -2.5
            } else if y == 7 {
                0.75
            } else {
                -1.0
            };
            colours.extend(colour);
            tags.extend(tag.to_le_bytes());
            stencils.push(stencil);
            floats.extend(float.to_le_bytes());
        }
    }
    [colours, tags, stencils, floats].concat()
}

/// What the draw from a record naming its first instance leaves by Vulkan's
/// rules: that instance's rectangle at 1, 1 three by two in 8 × 8.
fn first_instance_by_the_rules() -> Vec<u8> {
    (0..8u8)
        .flat_map(|y| {
            (0..8u8).flat_map(move |x| {
                if (1..4).contains(&x) && (1..3).contains(&y) {
                    [255, 0, 255, 255]
                } else {
                    [0; 4]
                }
            })
        })
        .collect()
}

/// What the four-sample pass resolves to by Vulkan's rules, as half floats:
/// in each pixel of 8 × 8, a quarter of the colour (1, 0, 0.5, 1) for each of
/// the standard sample positions the mask names that lies within 2.5 × 6.5
/// of the corner.
fn resolved_by_the_rules() -> Vec<u8> {
    const STANDARD: [(f32, f32); 4] = [
        (0.375, 0.125),
        (0.875, 0.375),
        (0.125, 0.625),
        (0.625, 0.875),
    ];
    const MASK: u32 = 0b0111;
    let mut bytes = Vec::new();
    for y in 0..8u8 {
        for x in 0..8u8 {
            let covered = (0..)
                .zip(STANDARD)
                .filter(|&(sample, (across, down))| {
                    MASK & 1 << sample != 0
                        && f32::from(x) + across < 2.5
                        && f32::from(y) + down < 6.5
                })
                .count();
            // Nought to three quarters, and half of each.
            let quarters: [u16; 4] = [0, 0x3400, 0x3800, 0x3a00];
            let eighths: [u16; 4] = [0, 0x3000, 0x3400, 0x3600];
            let channels = [quarters[covered], 0, eighths[covered], quarters[covered]];
            bytes.extend(channels.iter().flat_map(|channel| channel.to_le_bytes()));
        }
    }
    bytes
}

#[test]
fn the_render_work_leaves_what_vulkan_defines_for_its_draws() {
    let Some(Rendered {
        passes,
        resolved,
        first_instance,
        states_first_instance,
    }) = expected().render
    else {
        panic!("the software device states every use the render work makes");
    };
    let rules = passes_by_the_rules();
    assert_eq!(passes.len(), rules.len());
    if let Some(at) = passes
        .iter()
        .zip(&rules)
        .position(|(left, held)| left != held)
    {
        panic!(
            "byte {at} of the passes reads {} where the rules leave {}",
            passes[at], rules[at]
        );
    }
    assert_eq!(resolved, Some(resolved_by_the_rules()));
    assert!(states_first_instance);
    assert_eq!(first_instance, Ok(first_instance_by_the_rules()));
}

#[test]
fn a_draw_naming_its_first_instance_is_held_to_what_the_device_states() {
    let expected = expected();
    let with = |states: bool, answer: Result<Vec<u8>, Error>| {
        let mut answered = expected.clone();
        if let Some(render) = answered.render.as_mut() {
            (render.states_first_instance, render.first_instance) = (states, answer);
        }
        compare(&answered, &expected)
    };
    let lost = Err(Error::Lost(Loss::Fault));
    assert_eq!(with(false, lost.clone()), Ok(()), "lost, stating none");
    assert_eq!(
        with(false, Ok(first_instance_by_the_rules())),
        Err(Difference::FirstInstanceAnswer {
            device: Ok(()),
            expected: Err(Error::Lost(Loss::Fault)),
        }),
        "drawn, stating none"
    );
    assert_eq!(
        with(true, lost),
        Err(Difference::FirstInstanceAnswer {
            device: Err(Error::Lost(Loss::Fault)),
            expected: Ok(()),
        }),
        "lost, stating them"
    );
    let mut drawn = first_instance_by_the_rules();
    drawn[13] ^= 0x10;
    let Err(Difference::FirstInstance {
        at,
        device,
        reference,
    }) = with(true, Ok(drawn))
    else {
        panic!("a byte the draw left");
    };
    assert_eq!((at, device ^ reference), (13, 0x10));
}

#[test]
fn the_work_leaves_its_pattern_its_fill_and_both_copied_back() {
    let answer = expected();
    let [only] = answer.transfers.as_slice() else {
        panic!("the reference's one family runs the transfers");
    };
    let bytes = &only.bytes;
    assert_eq!(&bytes[..4096], super::pattern(4096).as_slice());
    assert_eq!(&bytes[4096..4100], &0x5a5a_a5a5_u32.to_le_bytes());
    // Copied out at 400 and back from 0, so the copy begins 400 bytes of
    // zeroes before the pattern.
    let back = 16 << 10;
    assert!(bytes[back..back + 400].iter().all(|&byte| byte == 0));
    assert_eq!(
        &bytes[back + 400..back + 400 + 4096],
        super::pattern(4096).as_slice()
    );
    assert!(only.refused.is_some(), "a fill past its buffer is refused");
}

/// A device whose first queue family runs every kind of work and whose
/// second is a copy engine, stating no format's use beside copying it.
fn copying_beside() -> Device {
    let mut info: DeviceInfo = software().info().clone();
    info.formats = FormatTable::from_fn(|_| FormatFeatures::NONE);
    let mut engine = info.queue_families[0];
    engine.capabilities = QueueCapabilities::TRANSFER;
    info.queue_families
        .try_push(engine)
        .expect("room for a copy engine");
    Device::hardware(Transfers, info, &[1, 1], Arc::new(Still)).expect("a device")
}

#[test]
fn the_transfers_run_on_every_family_stating_them_each_held_to_the_reference() {
    let answer = work(&copying_beside()).expect("the work ran");
    let held = &expected().transfers[0];
    let families: Vec<u32> = answer.transfers.iter().map(|left| left.family).collect();
    assert_eq!(families, [0, 1], "the copy engine among them");
    for left in &answer.transfers {
        assert_eq!(
            (&left.bytes, left.refused),
            (&held.bytes, held.refused),
            "family {}",
            left.family
        );
    }
    let mut copied = answer.clone();
    copied.transfers[1].bytes[9] ^= 2;
    let reference = Answer {
        transfers: alloc::vec![held.clone()],
        ..answer
    };
    assert_eq!(
        compare(&copied, &reference),
        Err(Difference::Byte {
            family: 1,
            at: 9,
            device: held.bytes[9] ^ 2,
            reference: held.bytes[9],
        }),
        "the copy engine's difference names it"
    );
}

#[test]
fn the_work_runs_every_pinned_pass_and_loses_a_context_past_the_device_s_counts() {
    let answer = expected();
    assert_eq!(answer.passes.len(), SET.len());
    for (pinned, left) in SET.iter().zip(&answer.passes) {
        let sample = (pinned.sample)().expect("a sample");
        let lengths: Vec<usize> = left.iter().map(Vec::len).collect();
        let given: Vec<usize> = sample.buffers.iter().map(Vec::len).collect();
        assert_eq!(lengths, given, "{}", pinned.name);
        assert_ne!(*left, sample.buffers, "{} ran", pinned.name);
    }
    assert_eq!(answer.past_counts, Some(Err(Error::Lost(Loss::Fault))));
}

/// The words `bytes` hold, in order.
fn words(bytes: &[u8]) -> Vec<u32> {
    let (words, rest) = bytes.as_chunks::<4>();
    assert!(rest.is_empty());
    words.iter().map(|word| u32::from_le_bytes(*word)).collect()
}

#[test]
fn the_image_work_leaves_its_copies_clears_and_what_its_dispatch_wrote() {
    let images = expected()
        .images
        .expect("the software device states every use the work makes");
    // Two colour images of 160 bytes, a depth plane, two stencil layers, then
    // the dispatch's words, its stored texels and its marks.
    let (colour, rest) = images.split_at(320);
    let (depth, rest) = rest.split_at(64);
    let (stencil, rest) = rest.split_at(32);
    let (written, rest) = rest.split_at(192);
    let (stored, marks) = rest.split_at(64);
    assert_eq!(marks.len(), 64);
    let second_level = &colour[160 + 128..];
    assert!(
        second_level
            .chunks(4)
            .all(|texel| texel == [255, 0, 255, 0]),
        "the clear, as unsigned normalised bytes: {second_level:?}"
    );
    let ramp: Vec<f32> = (0u8..16)
        .map(|at| f32::from(at % 5) * 0.2 + 0.125)
        .collect();
    let copied: Vec<f32> = words(depth).into_iter().map(f32::from_bits).collect();
    assert_eq!(copied, ramp, "the depths copied in");
    assert_eq!(stencil, [[9u8; 16], [3u8; 16]].concat().as_slice());
    let expected_words: Vec<u32> = (0u32..16)
        .flat_map(|at| {
            let column = f32::from(u8::try_from(at % 4).expect("small")) / 4.0;
            let compared = u32::from(column <= ramp[at as usize]);
            [at * at + 3, at * 7 + 1, compared]
        })
        .collect();
    assert_eq!(words(written), expected_words, "fetched, sampled, compared");
    let fetched_back: Vec<u32> = (0u32..16).map(|at| at * at + 3 + 100).collect();
    assert_eq!(words(stored), fetched_back);
    let marked: Vec<u32> = (0u32..16).map(|at| at + 1000).collect();
    assert_eq!(words(marks), marked);
}

#[test]
fn a_device_stating_less_than_the_image_work_uses_is_not_held_to_it() {
    let expected = expected();
    let mut stating_less = expected.clone();
    stating_less.images = None;
    assert_eq!(compare(&stating_less, &expected), Ok(()));
    let mut differing = expected.clone();
    if let Some(images) = differing.images.as_mut() {
        images[200] ^= 4;
    }
    differing.past_counts = Some(Ok(()));
    let Err(Difference::Image {
        at,
        device,
        reference,
    }) = compare(&differing, &expected)
    else {
        panic!("an image byte, before the counts");
    };
    assert_eq!((at, device ^ reference), (200, 4));
}

#[test]
fn a_device_drawing_less_than_the_render_work_uses_is_not_held_to_it() {
    let expected = expected();
    let mut drawing_nothing = expected.clone();
    drawing_nothing.render = None;
    assert_eq!(compare(&drawing_nothing, &expected), Ok(()));
    let mut one_sample = expected.clone();
    if let Some(render) = one_sample.render.as_mut() {
        render.resolved = None;
    }
    assert_eq!(compare(&one_sample, &expected), Ok(()));

    let mut resolving = expected.clone();
    if let Some(resolved) = resolving
        .render
        .as_mut()
        .and_then(|render| render.resolved.as_mut())
    {
        resolved[9] ^= 2;
    }
    resolving.past_counts = Some(Ok(()));
    let Err(Difference::Resolved {
        at,
        device,
        reference,
    }) = compare(&resolving, &expected)
    else {
        panic!("a resolved byte, before the counts");
    };
    assert_eq!((at, device ^ reference), (9, 2));
    let mut drawing = resolving.clone();
    if let Some(render) = drawing.render.as_mut() {
        render.passes[700] ^= 8;
    }
    let Err(Difference::Render {
        at,
        device,
        reference,
    }) = compare(&drawing, &expected)
    else {
        panic!("a byte of the passes, before the resolved samples");
    };
    assert_eq!((at, device ^ reference), (700, 8));
    if let Some(images) = drawing.images.as_mut() {
        images[0] ^= 1;
    }
    assert!(
        matches!(
            compare(&drawing, &expected),
            Err(Difference::Image { at: 0, .. })
        ),
        "an image byte, before the render work"
    );
}

#[test]
fn two_devices_running_the_work_alike_agree() {
    let answered = work(&software()).expect("the device ran");
    assert_eq!(compare(&answered, &expected()), Ok(()));
}

#[test]
fn a_device_running_transfers_alone_is_refused_at_the_first_pass() {
    let Err(Refusal { step, .. }) = work(&transfers()) else {
        panic!("refused");
    };
    assert_eq!(step, SET[0].name);
}

#[test]
fn every_difference_is_found_where_it_first_is() {
    let expected = expected();
    let mut byte = expected.clone();
    byte.transfers[0].bytes[16 << 10] ^= 1;
    byte.passes[3][0][5] ^= 1;
    let Err(Difference::Byte {
        family,
        at,
        device,
        reference,
    }) = compare(&byte, &expected)
    else {
        panic!("a byte, the first difference");
    };
    assert_eq!((family, at, device ^ reference), (0, 16 << 10, 1));

    let mut refusal = expected.clone();
    refusal.transfers[0].refused = None;
    assert!(matches!(
        compare(&refusal, &expected),
        Err(Difference::Refusal {
            family: 0,
            device: None,
            ..
        })
    ));

    let mut word = expected.clone();
    let last = SET.len() - 1;
    word.passes[last][1][1] ^= 0x100;
    word.past_counts = Some(Ok(()));
    let Err(Difference::Pass {
        name,
        binding,
        at,
        device,
        reference,
    }) = compare(&word, &expected)
    else {
        panic!("a pass's word, before the counts");
    };
    assert_eq!((name, binding, at), (SET[last].name, 1, 1));
    assert_eq!(device ^ reference, 0x100);

    let mut ran = expected.clone();
    ran.past_counts = Some(Ok(()));
    assert_eq!(
        compare(&ran, &expected),
        Err(Difference::PastCounts {
            device: Some(Ok(())),
            reference: expected.past_counts,
        })
    );
    let mut unbounded = expected.clone();
    unbounded.past_counts = None;
    assert_eq!(
        compare(&unbounded, &expected),
        Ok(()),
        "a device no count is past is not held to it"
    );
}

#[test]
fn a_refusal_names_its_step_and_a_transfer_difference_its_queue_family() {
    let refused = expected().transfers[0].refused;
    let text = alloc::format!(
        "{}",
        Difference::Refusal {
            family: 1,
            device: None,
            reference: refused,
        }
    );
    assert!(
        text.starts_with(
            "on queue family 1, a fill past its buffer was run where the software device's was refused ("
        ),
        "{text}"
    );
    let byte = Difference::Byte {
        family: 0,
        at: 7,
        device: 1,
        reference: 0x10,
    };
    assert_eq!(
        alloc::format!("{byte}"),
        "on queue family 0, byte 7 reads 0x01 where the software device left 0x10"
    );
    let step = Difference::Refused(Refusal {
        step: PAST_COUNTS,
        family: None,
        why: Error::Unsupported,
    });
    assert!(
        alloc::format!("{step}").starts_with("refused a dispatch past the device's counts: "),
        "{step}"
    );
    let on_family = Refusal {
        step: TRANSFERS,
        family: Some(1),
        why: Error::Unsupported,
    };
    assert!(
        alloc::format!("{on_family}").starts_with("the transfers on queue family 1: "),
        "{on_family}"
    );
}

#[test]
fn a_difference_reads_as_what_each_device_did() {
    let pass = Difference::Pass {
        name: "fill",
        binding: 0,
        at: 9,
        device: 0x8040_2010,
        reference: 0x8040_2011,
    };
    assert_eq!(
        alloc::format!("{pass}"),
        "the fill pass left 0x80402010 at word 9 of binding 0 where the software device left 0x80402011"
    );
    let counts = Difference::PastCounts {
        device: Some(Ok(())),
        reference: Some(Err(Error::Lost(Loss::Fault))),
    };
    assert!(
        alloc::format!("{counts}").starts_with(
            "a dispatch past the device's counts was run where the software device's was answered ("
        ),
        "{counts}"
    );
    let image = Difference::Image {
        at: 3,
        device: 0xff,
        reference: 0,
    };
    assert_eq!(
        alloc::format!("{image}"),
        "byte 3 of the image work reads 0xff where the software device left 0x00"
    );
    let render = Difference::Render {
        at: 12,
        device: 0x33,
        reference: 0x99,
    };
    assert_eq!(
        alloc::format!("{render}"),
        "byte 12 of the render work reads 0x33 where the software device left 0x99"
    );
    let resolved = Difference::Resolved {
        at: 0,
        device: 0x66,
        reference: 0x33,
    };
    assert_eq!(
        alloc::format!("{resolved}"),
        "byte 0 of the resolved samples reads 0x66 where the software device left 0x33"
    );
    let first = Difference::FirstInstance {
        at: 5,
        device: 0,
        reference: 0x33,
    };
    assert_eq!(
        alloc::format!("{first}"),
        "byte 5 of the draw naming its first instance reads 0x00 where the software device left 0x33"
    );
    let answered = Difference::FirstInstanceAnswer {
        device: Ok(()),
        expected: Err(Error::Lost(Loss::Fault)),
    };
    let text = alloc::format!("{answered}");
    assert!(
        text.starts_with("a draw naming its first instance was run where it must be answered ("),
        "{text}"
    );
}

/// Every locale's document names the switches the parser accepts: the
/// tokens are language-neutral, so each translation carries them as the
/// canonical one does. Read from the bundle's own on-disk `Help/` tree.
#[test]
fn help_documents_the_parser_switches() {
    let help_root = alloc::format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
    for locale in tairix_help::REQUIRED_LOCALES {
        let path = alloc::format!("{help_root}/{locale}/gpucheck.md");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {path}: {err}"));
        for switch in ["`-h, -?`", "`--present`", "`--shader FILE`", "`--words N`"] {
            assert!(
                text.contains(switch),
                "{locale}/gpucheck.md must document {switch}"
            );
        }
    }
}

#[test]
fn the_command_line_takes_present_and_the_help_switches_and_nothing_else() {
    assert_eq!(command(&[]), Ok(Command::Check));
    assert_eq!(command(&["--present"]), Ok(Command::Present));
    assert_eq!(command(&["--pres"]), Ok(Command::Present), "abbreviated");
    for help in ["-h", "-?", "--help"] {
        assert_eq!(command(&[help]), Ok(Command::Help), "{help}");
        assert_eq!(command(&["--present", help]), Ok(Command::Help), "{help}");
        assert_eq!(command(&[help, "now"]), Ok(Command::Help), "{help}");
    }
    for refused in [&["-x"][..], &["--present=yes"], &["--check"]] {
        assert!(
            matches!(command(refused), Err(CommandError::Option(_))),
            "{refused:?}"
        );
    }
    assert_eq!(
        command(&["--present", "now"])
            .map_err(|refused| alloc::string::ToString::to_string(&refused)),
        Err(alloc::string::String::from("extra operand 'now'"))
    );
}

#[test]
fn the_command_line_takes_a_kernel_and_the_length_of_its_buffer() {
    let shader = |file, words| Ok(Command::Shader { file, words });
    assert_eq!(
        command(&["--shader", "k.wgsl"]),
        shader("k.wgsl", DEFAULT_WORDS)
    );
    assert_eq!(
        command(&["--shader=k.spv", "--words", "64"]),
        shader("k.spv", 64)
    );
    assert_eq!(
        command(&["--words=1", "--shader", "a", "--shader", "b"]),
        shader("b", 1),
        "the last kernel named"
    );
    let most = alloc::format!("{MAX_WORDS}");
    assert_eq!(
        command(&["--shader", "k", "--words", &most]),
        shader("k", MAX_WORDS)
    );
    let past = alloc::format!("{}", MAX_WORDS + 1);
    for count in ["0", "-1", "x", "1.5", "", past.as_str()] {
        assert_eq!(
            command(&["--shader", "k", "--words", count]),
            Err(CommandError::Words(count.into())),
            "{count:?}"
        );
    }
    assert_eq!(
        command(&["--words", "4"]),
        Err(CommandError::WordsWithoutShader)
    );
    assert_eq!(
        command(&["--present", "--shader", "k"]),
        Err(CommandError::PresentAndShader)
    );
    assert!(matches!(
        command(&["--shader"]),
        Err(CommandError::Option(_))
    ));
    assert!(matches!(
        command(&["--shader", "k", "extra"]),
        Err(CommandError::ExtraOperand(_))
    ));
}
