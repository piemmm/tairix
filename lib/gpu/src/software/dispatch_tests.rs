//! Dispatches on the software device: stopped at their deadline however
//! their work is shaped.

use alloc::string::String;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::device_tests::{context, device, Clock};
use crate::pipeline::tests::{rig, run, words, Rig};
use crate::software::WORK_PER_CHECK;
use crate::{Context, Error, Loss};

/// What every shader here starts with: a storage buffer of words at
/// binding 0 and each invocation's global id.
const HEADER: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %buf
OpExecutionMode %main LocalSize 64 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v3uint = OpTypeVector %uint 3
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
"#;

/// The readings a submission takes besides its work's: one to start it,
/// and one to reset the engine it overran.
const OVERRUN_READINGS: u64 = 2;

/// The readings a completed submission takes besides its work's: one to
/// start it.
const DONE_READINGS: u64 = 1;

/// Submit one workgroup of `rig` while the clock steps `step` nanoseconds
/// a reading, past the submission's one-second deadline at the reading
/// that takes it beyond: what the context's state then is, and the
/// readings the submission took.
fn timed(clock: &Clock, context: &Context, rig: &Rig, step: u64) -> (Result<(), Error>, u64) {
    clock.readings.store(0, Ordering::SeqCst);
    clock.step.store(step, Ordering::SeqCst);
    let submitted = run(context, rig, [1, 1, 1]);
    clock.step.store(0, Ordering::SeqCst);
    let readings = clock.readings.load(Ordering::SeqCst);
    (submitted.and_then(|()| context.status()), readings)
}

const MILLISECOND: u64 = 1_000_000;

/// A device with room for the large programs some tests lower: each
/// keeps host memory charged to its context.
fn roomy() -> (crate::Device, alloc::sync::Arc<Clock>) {
    let clock = alloc::sync::Arc::new(Clock::default());
    let device = crate::device_tests::software(
        64 << 20,
        alloc::sync::Arc::clone(&clock)
            as alloc::sync::Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync>,
    );
    (device, clock)
}

#[test]
fn a_module_looping_forever_through_empty_blocks_stops_at_the_first_check_past_its_deadline() {
    let looping = [
        // A loop of one block, its own continue target.
        "OpBranch %head\n%head = OpLabel\nOpLoopMerge %exit %head None\nOpBranch %head\n%exit = OpLabel\nOpReturn",
        // A header and a continue block, neither doing anything.
        "OpBranch %head\n%head = OpLabel\nOpLoopMerge %exit %next None\nOpBranch %next\n%next = OpLabel\nOpBranch %head\n%exit = OpLabel\nOpReturn",
    ];
    for body in looping {
        let (device, clock) = device();
        let context = context(&device);
        let text = alloc::format!(
            "{HEADER}%main = OpFunction %void None %fn\n%entry = OpLabel\n{body}\nOpFunctionEnd\n"
        );
        let rig = rig(&context, &text, 256, &[]).expect("a pipeline");
        // The checks read 0.3, 0.6, 0.9 and then 1.2 seconds.
        let (state, readings) = timed(&clock, &context, &rig, 300 * MILLISECOND);
        assert_eq!(state, Err(Error::Lost(Loss::Overran)), "in\n{body}");
        assert_eq!(readings, OVERRUN_READINGS + 4, "in\n{body}");
    }
}

/// A block of `adds` additions of one to each invocation's index, the sum
/// stored at its index.
fn straight_line(adds: u64) -> String {
    let mut text = String::from(HEADER);
    text.push_str("%main = OpFunction %void None %fn\n%entry = OpLabel\n%id = OpLoad %v3uint %gid\n%v0 = OpCompositeExtract %uint %id 0\n");
    for k in 1..=adds {
        let _ = writeln!(text, "%v{k} = OpIAdd %uint %v{} %one", k - 1);
    }
    let _ = write!(text, "%at = OpAccessChain %word_ptr %buf %zero %v0\nOpStore %at %v{adds}\nOpReturn\nOpFunctionEnd\n");
    text
}

#[test]
fn a_huge_straight_line_block_reads_the_clock_at_each_fixed_amount_of_its_work() {
    let (device, clock) = roomy();
    let context = context(&device);
    // Two and a half checks' work over a tile's 64 lanes, a word apiece.
    let adds = WORK_PER_CHECK * 5 / 2 / 64;
    let rig = rig(&context, &straight_line(adds), 256, &[]).expect("a pipeline");
    let added = u32::try_from(adds).expect("a few thousand");
    let (state, readings) = timed(&clock, &context, &rig, 0);
    assert_eq!(state, Ok(()));
    assert_eq!(
        readings,
        DONE_READINGS + 2,
        "a reading at each check's worth of work, and no more"
    );
    let expected: alloc::vec::Vec<u32> = (0..64).map(|lane| lane + added).collect();
    assert_eq!(words(&rig.memory, 64), expected);

    // The second check reads 1.2 seconds, in the block's middle.
    let (state, readings) = timed(&clock, &context, &rig, 600 * MILLISECOND);
    assert_eq!(state, Err(Error::Lost(Loss::Overran)));
    assert_eq!(readings, OVERRUN_READINGS + 2);
}

#[test]
fn a_dispatch_whose_last_work_is_done_is_never_stopped() {
    let (device, clock) = roomy();
    let watcher = context(&device);
    // One block; one whose lanes then go on to an empty block that ends them;
    // and one whose lanes part for two such blocks.
    let shapes: [fn(u64) -> String; 3] = [straight_line, |adds| conditioned(adds, EVERY), parted];
    for (shape, text) in shapes.into_iter().enumerate() {
        let readings = |adds: u64| {
            let rig = rig(&watcher, &text(adds), 256, &[]).expect("a pipeline");
            timed(&clock, &watcher, &rig, 0).1
        };
        // The fewest additions whose work reaches the second check: it falls
        // on the last place the clock is read.
        let (mut low, mut high) = (WORK_PER_CHECK / 64, WORK_PER_CHECK * 2 / 64);
        while low + 1 < high {
            let middle = low + (high - low) / 2;
            if readings(middle) >= DONE_READINGS + 2 {
                high = middle;
            } else {
                low = middle;
            }
        }
        for adds in high - 2..=high + 2 {
            // Its own context, as an overrun loses it, the output read from
            // another's import.
            let runner = context(&device);
            let rig = rig(&runner, &text(adds), 256, &[]).expect("a pipeline");
            let output = watcher
                .import(rig.memory.export().expect("exported"))
                .expect("imported");
            // The second check reads 1.2 seconds, past the deadline.
            let (state, _) = timed(&clock, &runner, &rig, 600 * MILLISECOND);
            let added = u32::try_from(adds).expect("a few thousand");
            let done = words(&output, 64)
                .into_iter()
                .eq((0..64).map(|lane| lane + added));
            assert_eq!(
                state.is_ok(),
                done,
                "shape {shape}, {adds} additions: stopped only before its last work"
            );
        }
    }
}

/// Submit a fill of `filled` bytes of `buffer` and then a dispatch of no
/// workgroups of `pipeline` over `set`, while every reading of the clock past
/// the first passes the job's deadline: the context's state then, and the
/// readings the job took.
fn fill_then_nothing(
    (clock, context): (&Clock, &Context),
    (buffer, filled): (&crate::Buffer, u64),
    (pipeline, sets): (&crate::ComputePipeline, &[&crate::BindSet]),
) -> (Result<(), Error>, u64) {
    let mut commands = context.command_buffer();
    commands
        .fill_buffer(buffer, 0, filled, 7)
        .expect("recorded");
    commands
        .dispatch(pipeline, sets, &[], [0, 0, 0])
        .expect("recorded");
    clock.readings.store(0, Ordering::SeqCst);
    clock.step.store(2_000 * MILLISECOND, Ordering::SeqCst);
    let submitted = crate::device_tests::submit(&crate::device_tests::queue(context), &commands);
    clock.step.store(0, Ordering::SeqCst);
    assert!(submitted.is_ok(), "admitted");
    (context.status(), clock.readings.load(Ordering::SeqCst))
}

#[test]
fn a_dispatch_of_no_workgroups_after_a_job_s_last_work_reads_no_clock() {
    let (device, clock) = roomy();
    let context = context(&device);
    let (memory, buffer) = crate::device_tests::buffer(&context, WORK_PER_CHECK);
    // Four descriptors over one memory: merging, locking and viewing it and
    // resolving them bring the fill to a unit short of a check, and the
    // dispatch's one row of descriptors to it.
    let wide = wide_of(&context, 4).expect("a pipeline over four descriptors");
    let (state, readings) = fill_then_nothing(
        (&clock, &context),
        (&buffer, WORK_PER_CHECK - 8),
        (&wide.pipeline, &[&wide.set]),
    );
    assert_eq!(state, Ok(()), "its last work was done");
    assert_eq!(readings, DONE_READINGS);
    assert_eq!(words(&memory, 1), [7]);
}

/// A pipeline over no sets whose workgroups of `invocations` do nothing.
fn idle(context: &Context, invocations: u32) -> crate::ComputePipeline {
    let text = alloc::format!(
        "OpCapability Shader\nOpMemoryModel Logical GLSL450\nOpEntryPoint GLCompute %main \"main\"\nOpExecutionMode %main LocalSize {invocations} 1 1\n%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%main = OpFunction %void None %fn\n%entry = OpLabel\nOpReturn\nOpFunctionEnd\n"
    );
    let module = context
        .create_shader_module(tairix_spirv::assemble::admit(&text).expect("admitted"))
        .expect("a module");
    let layout = context
        .create_pipeline_layout(&[], crate::PushConstants::NONE)
        .expect("a layout of no sets");
    context
        .create_compute_pipeline(&crate::ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline")
}

#[test]
fn a_dispatch_of_nothing_reads_no_clock_however_much_came_before() {
    let (device, clock) = roomy();
    let context = context(&device);
    let (_memory, buffer) = crate::device_tests::buffer(&context, WORK_PER_CHECK);
    let pipeline = idle(&context, 1);
    // A check's whole work, and then a dispatch with none.
    let (state, readings) = fill_then_nothing(
        (&clock, &context),
        (&buffer, WORK_PER_CHECK),
        (&pipeline, &[]),
    );
    assert_eq!(state, Ok(()), "its last work was done");
    assert_eq!(readings, DONE_READINGS);
}

#[test]
fn a_stopped_job_reads_the_clock_no_more() {
    let clock = Clock::default();
    let stop = AtomicBool::new(true);
    let mut meter = crate::software::Meter {
        stop: &stop,
        clock: &clock,
        deadline: u64::MAX,
        unchecked: WORK_PER_CHECK,
        read: false,
    };
    assert!(!meter.proceed(0));
    assert_eq!(clock.readings.load(Ordering::SeqCst), 0);
}

#[test]
fn a_clear_reads_the_clock_each_check_s_work_of_it() {
    let clock = Clock::default();
    let stop = AtomicBool::new(false);
    let mut meter = crate::software::Meter {
        stop: &stop,
        clock: &clock,
        deadline: u64::MAX,
        unchecked: 0,
        read: false,
    };
    let per = usize::try_from(WORK_PER_CHECK).expect("a check fits");
    let mut words = alloc::vec![7u32; 3 * per];
    assert!(super::clear(&mut words, &mut meter));
    assert!(words.iter().all(|&w| w == 0));
    assert_eq!(
        clock.readings.load(Ordering::SeqCst),
        2,
        "before the second and the third"
    );
    // Once stopped, nothing more is cleared.
    words.fill(7);
    stop.store(true, Ordering::SeqCst);
    assert!(!super::clear(&mut words, &mut meter));
    assert!(words.iter().all(|&w| w == 7));
}

#[test]
fn the_memories_a_dispatch_views_are_counted_as_it_views_them() {
    use tairix_spirv::{Access, BindingKind, Stages};
    let (device, clock) = roomy();
    let context = context(&device);
    let (_memory, buffer) = crate::device_tests::buffer(&context, WORK_PER_CHECK);
    // Three memories bound where the shader reaches one, so a single
    // descriptor is resolved: merging and locking them leave the fill two
    // units short of a check, and viewing them passes it.
    let module = context
        .create_shader_module(tairix_spirv::assemble::admit(&straight_line(1)).expect("admitted"))
        .expect("a module");
    let set_layout = context
        .create_bind_set_layout(&[crate::LayoutBinding {
            binding: 0,
            kind: BindingKind::StorageBuffer,
            count: 3,
            access: Access::READ.with(Access::WRITE),
            stages: Stages::COMPUTE,
        }])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], crate::PushConstants::NONE)
        .expect("a pipeline layout");
    let pipeline = context
        .create_compute_pipeline(&crate::ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let bound: alloc::vec::Vec<_> = (0..3)
        .map(|_| crate::device_tests::buffer(&context, 256))
        .collect();
    let resources: alloc::vec::Vec<crate::Resource<'_>> = bound
        .iter()
        .map(|(_, buffer)| crate::Resource::Buffer {
            buffer,
            offset: 0,
            size: 256,
        })
        .collect();
    let set = context
        .create_bind_set(
            &set_layout,
            &[crate::BindSetEntry {
                binding: 0,
                resources: &resources,
            }],
        )
        .expect("a set");
    let (state, readings) = fill_then_nothing(
        (&clock, &context),
        (&buffer, WORK_PER_CHECK - 8),
        (&pipeline, &[&set]),
    );
    assert_eq!(
        state,
        Err(Error::Lost(Loss::Overran)),
        "the clock is read among the views, before the rest of the work"
    );
    assert_eq!(readings, OVERRUN_READINGS + 1);
}

#[test]
fn a_workgroup_s_tile_starts_read_the_clock_among_them() {
    // Sixteen tiles of one workgroup, each start clearing three thousand
    // private words a lane, and nothing else to run.
    let text = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1024 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%c3000 = OpConstant %uint 3000
%words = OpTypeArray %uint %c3000
%local_ptr = OpTypePointer Function %words
%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %local_ptr Function
OpReturn
OpFunctionEnd
"#;
    let (device, clock) = roomy();
    let context = context(&device);
    let rig = rig(&context, text, 256, &[]).expect("a pipeline");
    let (state, readings) = timed(&clock, &context, &rig, 0);
    assert_eq!(state, Ok(()));
    assert!(
        readings >= DONE_READINGS + 2,
        "three million words of tile starts read the clock among them: {readings}"
    );
    // The second reading falls among the tile starts, past the deadline.
    let (state, _) = timed(&clock, &context, &rig, 600 * MILLISECOND);
    assert_eq!(state, Err(Error::Lost(Loss::Overran)));
}

#[test]
fn a_piece_that_claims_no_workgroups_leaves_the_job_s_count_to_those_that_do() {
    let clock = alloc::sync::Arc::new(Clock::default());
    let device = crate::device_tests::software_on(
        64 << 20,
        alloc::sync::Arc::clone(&clock)
            as alloc::sync::Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync>,
        alloc::sync::Arc::new(tairix_parallel::Reversed::new(2)),
    );
    let context = context(&device);
    // Workgroups past what a fill left of a check, every one claimed by the
    // piece run first, which reads the clock among them; the other finds none.
    let groups: u32 = 64;
    let adds = WORK_PER_CHECK * 3 / 20 / 64 / u64::from(groups);
    let rig =
        rig(&context, &straight_line(adds), u64::from(groups) * 256, &[]).expect("a pipeline");
    let most = WORK_PER_CHECK * 9 / 10 / 4 * 4;
    let (_memory, buffer) = crate::device_tests::buffer(&context, most);
    let mut commands = context.command_buffer();
    commands.fill_buffer(&buffer, 0, most, 7).expect("recorded");
    commands
        .dispatch(&rig.pipeline, &[&rig.set], &[], [groups, 1, 1])
        .expect("recorded");
    // Fills that pass another check only from what the idle piece was given.
    commands
        .fill_buffer(&buffer, 0, WORK_PER_CHECK / 2, 7)
        .expect("recorded");
    commands.fill_buffer(&buffer, 0, 4, 7).expect("recorded");
    clock.readings.store(0, Ordering::SeqCst);
    crate::device_tests::submit(&crate::device_tests::queue(&context), &commands)
        .expect("admitted");
    assert_eq!(
        clock.readings.load(Ordering::SeqCst),
        DONE_READINGS + 1,
        "one check passed, read by the piece that worked"
    );
}

#[test]
fn pieces_run_in_turn_on_one_thread_read_the_clock_as_one_piece_would() {
    let clock = alloc::sync::Arc::new(Clock::default());
    let device = crate::device_tests::software_on(
        64 << 20,
        alloc::sync::Arc::clone(&clock)
            as alloc::sync::Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync>,
        alloc::sync::Arc::new(tairix_parallel::Reversed::new(8)),
    );
    let context = context(&device);
    // Eight workgroups of two fifths of a check's work each, one a piece.
    let adds = WORK_PER_CHECK * 2 / 5 / 64;
    let rig = rig(&context, &straight_line(adds), 8 * 256, &[]).expect("a pipeline");
    clock.readings.store(0, Ordering::SeqCst);
    run(&context, &rig, [8, 1, 1]).expect("dispatched");
    assert_eq!(
        clock.readings.load(Ordering::SeqCst),
        DONE_READINGS + 3,
        "three and a fifth checks' work, whichever pieces did it"
    );
}

/// A block of `adds` additions to each invocation's index, the sum stored
/// at its index, its lanes then parting for two empty blocks that end them.
fn parted(adds: u64) -> String {
    let mut text = String::from(HEADER);
    text.push_str("%bool = OpTypeBool\n%c32 = OpConstant %uint 32\n%main = OpFunction %void None %fn\n%entry = OpLabel\n%id = OpLoad %v3uint %gid\n%v0 = OpCompositeExtract %uint %id 0\n%early = OpULessThan %bool %v0 %c32\n");
    for k in 1..=adds {
        let _ = writeln!(text, "%v{k} = OpIAdd %uint %v{} %one", k - 1);
    }
    let _ = write!(text, "%at = OpAccessChain %word_ptr %buf %zero %v0\nOpStore %at %v{adds}\nOpSelectionMerge %merge None\nOpBranchConditional %early %leave %merge\n%leave = OpLabel\nOpReturn\n%merge = OpLabel\nOpReturn\nOpFunctionEnd\n");
    text
}

/// The condition every lane of a tile holds for.
const EVERY: &str = "%first = OpIEqual %bool %v0 %zero\n%last = OpIEqual %bool %v0 %c63\n%taken = OpULessThan %bool %v0 %c64";

/// A block of `adds` additions to each invocation's index, run by the
/// lanes `condition` holds for, the sum stored at its index.
fn conditioned(adds: u64, condition: &str) -> String {
    let mut text = String::from(HEADER);
    text.push_str("%bool = OpTypeBool\n%c63 = OpConstant %uint 63\n%c64 = OpConstant %uint 64\n%main = OpFunction %void None %fn\n%entry = OpLabel\n%id = OpLoad %v3uint %gid\n%v0 = OpCompositeExtract %uint %id 0\n");
    let _ = writeln!(text, "{condition}\nOpSelectionMerge %merge None\nOpBranchConditional %taken %work %merge\n%work = OpLabel");
    for k in 1..=adds {
        let _ = writeln!(text, "%v{k} = OpIAdd %uint %v{} %one", k - 1);
    }
    let _ = write!(text, "%at = OpAccessChain %word_ptr %buf %zero %v0\nOpStore %at %v{adds}\nOpBranch %merge\n%merge = OpLabel\nOpReturn\nOpFunctionEnd\n");
    text
}

#[test]
fn work_is_counted_over_the_lanes_a_tile_computes_however_few_it_keeps() {
    let (device, clock) = roomy();
    let context = context(&device);
    let adds = WORK_PER_CHECK * 5 / 2 / 64;
    let ends = "%first = OpIEqual %bool %v0 %zero\n%last = OpIEqual %bool %v0 %c63\n%taken = OpLogicalOr %bool %first %last";
    let mut taken = alloc::vec::Vec::new();
    for condition in [EVERY, ends] {
        let rig = rig(&context, &conditioned(adds, condition), 256, &[]).expect("a pipeline");
        let (state, readings) = timed(&clock, &context, &rig, 0);
        assert_eq!(state, Ok(()));
        taken.push(readings);
    }
    assert_eq!(
        taken,
        [DONE_READINGS + 2; 2],
        "the first and last lanes alone compute all 64 between them"
    );
}

/// A module calling, 2<sup>`depth`</sup> times over, a function of `nops`
/// instructions that make nothing.
fn calls_of_nothing(depth: usize, nops: usize) -> String {
    let mut text = String::from(HEADER);
    let _ = writeln!(text, "%g0 = OpFunction %void None %fn\n%n0 = OpLabel");
    for _ in 0..nops {
        text.push_str("OpNop\n");
    }
    text.push_str("OpReturn\nOpFunctionEnd\n");
    for k in 1..=depth {
        let below = k - 1;
        let _ = writeln!(
            text,
            "%g{k} = OpFunction %void None %fn\n%n{k} = OpLabel\n%x{k} = OpFunctionCall %void %g{below}\n%y{k} = OpFunctionCall %void %g{below}\nOpReturn\nOpFunctionEnd"
        );
    }
    let _ = write!(
        text,
        "%main = OpFunction %void None %fn\n%entry = OpLabel\n%c = OpFunctionCall %void %g{depth}\nOpReturn\nOpFunctionEnd\n"
    );
    text
}

#[test]
fn calls_of_a_function_that_makes_nothing_are_refused_past_the_lowering_budget() {
    let (device, _) = device();
    let context = context(&device);
    rig(&context, &calls_of_nothing(3, 4096), 256, &[]).expect("eight calls are within it");
    assert_eq!(
        rig(&context, &calls_of_nothing(9, 4096), 256, &[]).err(),
        Some(Error::TooLarge),
        "512 instances of the function each need its tables, though none makes an instruction"
    );
}

/// A chain of `blocks` blocks, each adding one to the invocation's index.
fn chain(blocks: u64) -> String {
    let mut text = String::from(HEADER);
    text.push_str("%main = OpFunction %void None %fn\n%entry = OpLabel\n%id = OpLoad %v3uint %gid\n%v0 = OpCompositeExtract %uint %id 0\nOpBranch %b1\n");
    for k in 1..=blocks {
        let _ = writeln!(
            text,
            "%b{k} = OpLabel\n%v{k} = OpIAdd %uint %v{} %one",
            k - 1
        );
        if k < blocks {
            let _ = writeln!(text, "OpBranch %b{}", k + 1);
        }
    }
    let _ = write!(
        text,
        "%at = OpAccessChain %word_ptr %buf %zero %v0\nOpStore %at %v{blocks}\nOpReturn\nOpFunctionEnd\n"
    );
    text
}

#[test]
fn a_chain_of_many_blocks_reads_the_clock_only_as_its_work_says() {
    let (device, clock) = roomy();
    let context = context(&device);
    // Each block's addition and branch over 64 lanes: two and a half checks'
    // work in all.
    let blocks = WORK_PER_CHECK * 5 / 2 / 128;
    let rig = rig(&context, &chain(blocks), 256, &[]).expect("a pipeline");
    let (state, readings) = timed(&clock, &context, &rig, 0);
    assert_eq!(state, Ok(()));
    assert_eq!(
        readings,
        DONE_READINGS + 2,
        "the next block found from the last, not looked for from the first"
    );
    let added = u32::try_from(blocks).expect("a few thousand");
    let expected: alloc::vec::Vec<u32> = (0..64).map(|lane| lane + added).collect();
    assert_eq!(words(&rig.memory, 64), expected);
}

/// Functions `%f0` to `%f{depth}`, each but the first calling the one
/// below it twice and adding one to the counter its parameter points to.
fn fan_out(depth: usize) -> String {
    let mut text = String::from("%counter_ptr = OpTypePointer Function %uint\n%counter_fn = OpTypeFunction %void %counter_ptr\n");
    text.push_str("%f0 = OpFunction %void None %counter_fn\n%p0 = OpFunctionParameter %counter_ptr\n%e0 = OpLabel\n%h0 = OpLoad %uint %p0\n%s0 = OpIAdd %uint %h0 %one\nOpStore %p0 %s0\nOpReturn\nOpFunctionEnd\n");
    for k in 1..=depth {
        let below = k - 1;
        let _ = write!(
            text,
            "%f{k} = OpFunction %void None %counter_fn\n%p{k} = OpFunctionParameter %counter_ptr\n%e{k} = OpLabel\n%a{k} = OpFunctionCall %void %f{below} %p{k}\n%b{k} = OpFunctionCall %void %f{below} %p{k}\nOpReturn\nOpFunctionEnd\n"
        );
    }
    text
}

/// A module calling the fan-out of `depth` once, or forever.
fn calling(depth: usize, forever: bool) -> String {
    let call = if forever {
        alloc::format!("OpBranch %head\n%head = OpLabel\nOpLoopMerge %exit %body None\nOpBranch %body\n%body = OpLabel\n%c = OpFunctionCall %void %f{depth} %counter\nOpBranch %head\n%exit = OpLabel\nOpReturn")
    } else {
        alloc::format!("%c = OpFunctionCall %void %f{depth} %counter\n%n = OpLoad %uint %counter\n%id = OpLoad %v3uint %gid\n%x = OpCompositeExtract %uint %id 0\n%at = OpAccessChain %word_ptr %buf %zero %x\nOpStore %at %n\nOpReturn")
    };
    alloc::format!(
        "{HEADER}{}%main = OpFunction %void None %fn\n%entry = OpLabel\n%counter = OpVariable %counter_ptr Function %zero\n{call}\nOpFunctionEnd\n",
        fan_out(depth)
    )
}

#[test]
fn a_fan_out_of_calls_runs_inlined_stops_at_its_deadline_and_is_refused_past_the_lowering_budget() {
    let (device, clock) = device();
    let context = context(&device);
    let fits = rig(&context, &calling(8, false), 256, &[]).expect("a pipeline");
    run(&context, &fits, [1, 1, 1]).expect("dispatched");
    assert_eq!(
        words(&fits.memory, 64),
        [256; 64],
        "every one of 2^8 calls made"
    );

    let looping = rig(&context, &calling(8, true), 256, &[]).expect("a pipeline");
    let (state, readings) = timed(&clock, &context, &looping, 300 * MILLISECOND);
    assert_eq!(state, Err(Error::Lost(Loss::Overran)));
    assert_eq!(readings, OVERRUN_READINGS + 4);

    let other = crate::device_tests::context(&device);
    assert_eq!(
        rig(&other, &calling(30, false), 256, &[]).err(),
        Some(Error::TooLarge),
        "2^30 calls inlined are past the budget, found before they are made"
    );
}

/// Workgroups of 100 hashing their global index through integer and float
/// arithmetic, sharing it through workgroup memory across a barrier,
/// scanning and balloting it across subgroups, and adding into one counter
/// per workgroup and one for the dispatch: four words per invocation, then
/// the counters.
const MIXED: &str = r#"OpCapability Shader
OpCapability GroupNonUniform
OpCapability GroupNonUniformArithmetic
OpCapability GroupNonUniformBallot
%glsl = OpExtInstImport "GLSL.std.450"
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %lid %wid %buf %shared
OpExecutionMode %main LocalSize 100 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %lid BuiltIn LocalInvocationIndex
OpDecorate %wid BuiltIn WorkgroupId
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%four = OpConstant %uint 4
%seven = OpConstant %uint 7
%sixteen = OpConstant %uint 16
%hundred = OpConstant %uint 100
%row_len = OpConstant %uint 700
%golden = OpConstant %uint 2654435761
%byte = OpConstant %uint 255
%half = OpConstant %uint 65535
%counters = OpConstant %uint 8400
%total = OpConstant %uint 8421
%device = OpConstant %uint 1
%workgroup = OpConstant %uint 2
%subgroup = OpConstant %uint 3
%relaxed = OpConstant %uint 0
%shared_sem = OpConstant %uint 264
%scale = OpConstant %float 0.001
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%v3_ptr = OpTypePointer Input %v3uint
%uint_in = OpTypePointer Input %uint
%gid = OpVariable %v3_ptr Input
%lid = OpVariable %uint_in Input
%wid = OpVariable %v3_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
%shared_t = OpTypeArray %uint %hundred
%shared_ptr = OpTypePointer Workgroup %shared_t
%shared_word = OpTypePointer Workgroup %uint
%shared = OpVariable %shared_ptr Workgroup
%main = OpFunction %void None %fn
%entry = OpLabel
%g = OpLoad %v3uint %gid
%gx = OpCompositeExtract %uint %g 0
%gy = OpCompositeExtract %uint %g 1
%row = OpIMul %uint %gy %row_len
%index = OpIAdd %uint %row %gx
%l = OpLoad %uint %lid
%h1 = OpIMul %uint %index %golden
%h2 = OpShiftRightLogical %uint %h1 %sixteen
%h3 = OpBitwiseXor %uint %h1 %h2
%f = OpConvertUToF %float %h3
%fs = OpFMul %float %f %scale
%sine = OpExtInst %float %glsl Sin %fs
%sb = OpBitcast %uint %sine
%h = OpBitwiseXor %uint %h3 %sb
%mine = OpAccessChain %shared_word %shared %l
OpStore %mine %h
OpControlBarrier %workgroup %workgroup %shared_sem
%after = OpIAdd %uint %l %one
%next = OpUMod %uint %after %hundred
%theirs = OpAccessChain %shared_word %shared %next
%neighbour = OpLoad %uint %theirs
%low = OpBitwiseAnd %uint %h %byte
%scan = OpGroupNonUniformIAdd %uint %subgroup InclusiveScan %low
%bit = OpBitwiseAnd %uint %h %one
%odd = OpIEqual %bool %bit %one
%ballot = OpGroupNonUniformBallot %v4uint %subgroup %odd
%odds = OpGroupNonUniformBallotBitCount %uint %subgroup Reduce %ballot
%base = OpIMul %uint %index %four
%a1 = OpIAdd %uint %base %one
%a2 = OpIAdd %uint %a1 %one
%a3 = OpIAdd %uint %a2 %one
%p0 = OpAccessChain %word_ptr %buf %zero %base
OpStore %p0 %h
%p1 = OpAccessChain %word_ptr %buf %zero %a1
OpStore %p1 %neighbour
%p2 = OpAccessChain %word_ptr %buf %zero %a2
OpStore %p2 %scan
%p3 = OpAccessChain %word_ptr %buf %zero %a3
OpStore %p3 %odds
%w = OpLoad %v3uint %wid
%wx = OpCompositeExtract %uint %w 0
%wy = OpCompositeExtract %uint %w 1
%wrow = OpIMul %uint %wy %seven
%wlinear = OpIAdd %uint %wrow %wx
%counter_at = OpIAdd %uint %counters %wlinear
%counter = OpAccessChain %word_ptr %buf %zero %counter_at
%part = OpBitwiseAnd %uint %h %half
%was = OpAtomicIAdd %uint %counter %device %relaxed %part
%all = OpAccessChain %word_ptr %buf %zero %total
%before = OpAtomicIAdd %uint %all %device %relaxed %one
OpReturn
OpFunctionEnd
"#;

#[test]
fn one_module_on_one_core_and_on_several_gives_the_same_bytes() {
    // Interpreted, two workgroups still run as two pieces at once.
    let groups = if cfg!(miri) { [2, 1, 1] } else { [7, 3, 1] };
    let invocations: u32 = groups.iter().product::<u32>() * 100;
    let words_out = 8422;
    let bytes = words_out as u64 * 4;
    let on = |runner: alloc::sync::Arc<dyn tairix_parallel::JobRunner + Send>| {
        let clock: alloc::sync::Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync> =
            alloc::sync::Arc::new(Clock::default());
        let device = crate::device_tests::software_on(crate::device_tests::RAM, clock, runner);
        let context = context(&device);
        let rig = rig(&context, MIXED, bytes, &[]).expect("a pipeline");
        run(&context, &rig, groups).expect("dispatched");
        words(&rig.memory, words_out)
    };
    let reversed = alloc::sync::Arc::new(tairix_parallel::Reversed::new(8));
    let one = on(alloc::sync::Arc::new(tairix_parallel::Serial));
    assert_eq!(
        one.get(8421),
        Some(&invocations),
        "every invocation ran once"
    );
    assert_eq!(on(reversed.clone()), one, "the pieces run last to first");
    assert!(reversed.widest() > 1, "the work was split");
    assert_eq!(
        on(alloc::sync::Arc::new(tairix_parallel::Threaded::new(4))),
        one,
        "the pieces run at once"
    );
}

const DIRT: u32 = 0xdead_beef;
const DIRTY_REGISTERS: usize = 48;

/// What both isolation shaders declare: arrays of 64 words in private,
/// function and workgroup memory, and the types the reader's undefined
/// values take.
fn arrays() -> String {
    alloc::format!(
        r#"OpCapability Shader
OpCapability GroupNonUniform
OpCapability GroupNonUniformBallot
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %buf %private %shared
OpExecutionMode %main LocalSize 64 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%nine = OpConstant %uint 9
%ten = OpConstant %uint 10
%sixty_four = OpConstant %uint 64
%dirt = OpConstant %uint {DIRT}
%subgroup = OpConstant %uint 3
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
%array = OpTypeArray %uint %sixty_four
%private_ptr = OpTypePointer Private %array
%private_word = OpTypePointer Private %uint
%private = OpVariable %private_ptr Private
%function_ptr = OpTypePointer Function %array
%function_word = OpTypePointer Function %uint
%shared_ptr = OpTypePointer Workgroup %array
%shared_word = OpTypePointer Workgroup %uint
%shared = OpVariable %shared_ptr Workgroup
%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %function_ptr Function
%g = OpLoad %v3uint %gid
%lane = OpCompositeExtract %uint %g 0
"#
    )
}

/// Every lane filling all three arrays and a run of registers with dirt.
fn dirty() -> String {
    let mut text = arrays();
    text.push_str("OpBranch %head\n%head = OpLabel\n%i = OpPhi %uint %zero %entry %next %body\nOpLoopMerge %done %body None\nOpBranch %check\n%check = OpLabel\n%more = OpULessThan %bool %i %sixty_four\nOpBranchConditional %more %body %done\n%body = OpLabel\n%value = OpBitwiseXor %uint %dirt %i\n%pp = OpAccessChain %private_word %private %i\nOpStore %pp %value\n%fp = OpAccessChain %function_word %local %i\nOpStore %fp %value\n%sp = OpAccessChain %shared_word %shared %i\nOpStore %sp %value\n%next = OpIAdd %uint %i %one\nOpBranch %head\n%done = OpLabel\n");
    let mut last = String::from("%dirt");
    for k in 0..DIRTY_REGISTERS {
        let _ = writeln!(
            text,
            "%x{k} = OpBitwiseXor %uint {last} %lane\n%r{k} = OpIAdd %uint %x{k} %dirt"
        );
        last = alloc::format!("%r{k}");
    }
    let _ = write!(text, "%at = OpAccessChain %word_ptr %buf %zero %lane\nOpStore %at {last}\nOpReturn\nOpFunctionEnd\n");
    text
}

/// Every lane reading each form of value nothing defined: undefined scalars,
/// vector components and phi arms, an undefined shuffle component, the
/// three arrays unwritten, a component past its vector, and a broadcast from
/// a lane not running; nine words apiece.
fn reader() -> String {
    let mut text = arrays();
    text.push_str(
        r"%u = OpUndef %uint
%uv = OpUndef %v4uint
%u2 = OpCompositeExtract %uint %uv 2
%vec = OpCompositeConstruct %v4uint %lane %dirt %lane %dirt
%shuffled = OpVectorShuffle %v4uint %vec %vec 4294967295 1 4294967295 3
%s0 = OpCompositeExtract %uint %shuffled 0
%pp = OpAccessChain %private_word %private %lane
%pv = OpLoad %uint %pp
%fp = OpAccessChain %function_word %local %lane
%fv = OpLoad %uint %fp
%sp = OpAccessChain %shared_word %shared %lane
%sv = OpLoad %uint %sp
%past = OpVectorExtractDynamic %uint %vec %ten
%parity = OpBitwiseAnd %uint %lane %one
%even = OpIEqual %bool %parity %zero
OpSelectionMerge %merged None
OpBranchConditional %even %evens %merged
%evens = OpLabel
%from_odd = OpGroupNonUniformBroadcast %uint %subgroup %lane %one
OpBranch %merged
%merged = OpLabel
%broadcast = OpPhi %uint %from_odd %evens %u %entry
",
    );
    let values = [
        "%u",
        "%u2",
        "%s0",
        "%pv",
        "%fv",
        "%sv",
        "%past",
        "%broadcast",
    ];
    let _ = writeln!(text, "%base = OpIMul %uint %lane %nine");
    for (k, value) in values.iter().enumerate() {
        let _ = writeln!(text, "%o{k} = OpIAdd %uint %base %c{k}_at\n%q{k} = OpAccessChain %word_ptr %buf %zero %o{k}\nOpStore %q{k} {value}");
    }
    text.push_str("OpReturn\nOpFunctionEnd\n");
    let mut indexes = String::new();
    for k in 0..values.len() {
        let _ = writeln!(indexes, "%c{k}_at = OpConstant %uint {k}");
    }
    text.replacen(
        "%main = OpFunction",
        &alloc::format!("{indexes}%main = OpFunction"),
        1,
    )
}

#[test]
fn a_second_context_reads_every_undefined_form_as_zero_after_the_first_ran() {
    let (device, _) = device();
    let first = context(&device);
    let dirty = rig(&first, &dirty(), 256, &[]).expect("a pipeline");
    run(&first, &dirty, [1, 1, 1]).expect("dispatched");
    let dirt: alloc::vec::Vec<u32> = (0..64)
        .map(|lane| (0..DIRTY_REGISTERS).fold(DIRT, |last, _| (last ^ lane).wrapping_add(DIRT)))
        .collect();
    assert_eq!(words(&dirty.memory, 64), dirt, "the first left its dirt");

    let second = crate::device_tests::context(&device);
    let reader = rig(&second, &reader(), 64 * 9 * 4, &[]).expect("a pipeline");
    reader
        .memory
        .write(0, &[0xff; 64 * 9 * 4])
        .expect("writable");
    run(&second, &reader, [1, 1, 1]).expect("dispatched");
    let read = words(&reader.memory, 64 * 9);
    for lane in 0..64 {
        let mine = &read[lane * 9..lane * 9 + 8];
        assert_eq!(mine, [0; 8], "lane {lane}: undefined, undefined component, undefined shuffle component, private, function, workgroup, past the vector, broadcast from a lane not running");
    }
}

#[test]
fn an_indirect_dispatch_reads_its_counts_as_it_runs_and_counts_past_the_device_s_lose_the_context()
{
    let (device, _) = device();
    let context = context(&device);
    let rig = rig(&context, &crate::pipeline::tests::doubles(4), 256, &[]).expect("a pipeline");
    let (counts, arguments) = crate::device_tests::buffer(&context, 64);
    let plain = context
        .create_buffer(
            &counts,
            &crate::BufferDesc {
                offset: 0,
                size: 64,
                usage: crate::BufferUsage::STORAGE,
            },
        )
        .expect("a buffer");
    let indirect = |buffer: &crate::Buffer, offset: u64| {
        let mut commands = context.command_buffer();
        commands
            .dispatch_indirect(&rig.pipeline, &[&rig.set], &[], (buffer, offset))
            .expect("recorded");
        commands
    };
    let refused = |commands| run_commands(&context, &commands).err();
    assert_eq!(refused(indirect(&plain, 16)), Some(Error::Usage));
    assert_eq!(refused(indirect(&arguments, 18)), Some(Error::Misaligned));
    assert_eq!(
        refused(indirect(&arguments, 56)),
        Some(Error::OutOfRange),
        "twelve bytes past 56 are past 64"
    );

    let commands = indirect(&arguments, 16);
    let write_counts = |groups: [u32; 3]| {
        let bytes: alloc::vec::Vec<u8> = groups.iter().flat_map(|g| g.to_le_bytes()).collect();
        counts.write(16, &bytes).expect("writable");
    };
    write_counts([3, 1, 1]);
    run_commands(&context, &commands).expect("dispatched");
    let doubled: alloc::vec::Vec<u32> = (0..12)
        .map(|x| x * 2)
        .chain(core::iter::repeat_n(0, 52))
        .collect();
    assert_eq!(
        words(&rig.memory, 64),
        doubled,
        "the counts written after it was recorded"
    );

    rig.memory.write(0, &[0; 256]).expect("writable");
    write_counts([0, 5, 1]);
    run_commands(&context, &commands).expect("dispatched");
    assert_eq!(context.status(), Ok(()));
    assert_eq!(
        words(&rig.memory, 64),
        [0; 64],
        "no workgroups along one axis"
    );

    write_counts([65_536, 1, 1]);
    run_commands(&context, &commands).expect("admitted");
    assert_eq!(context.status(), Err(Error::Lost(Loss::Fault)));
}

fn run_commands(context: &Context, commands: &crate::CommandBuffer) -> Result<(), Error> {
    crate::device_tests::submit(&crate::device_tests::queue(context), commands)
        .map(|_| ())
        .map_err(|refused| refused.error)
}

/// Hold room for `need` as a pipeline's creation does: planned, made and
/// installed, what the pool let go dropped as it would be.
fn hold(
    pool: &mut super::Workspaces,
    need: super::Need,
    budget: &alloc::sync::Arc<crate::memory::Budget>,
    heap: &alloc::sync::Arc<crate::memory::Budget>,
) -> Result<super::Hold, Error> {
    let (plan, released) = pool.plan(need, (budget, 0), heap);
    drop(released);
    let grown = plan?.make()?;
    let (held, released) = pool.install(grown);
    drop(released);
    held.ok_or(Error::Malformed)
}

#[test]
fn a_growth_the_heap_has_no_room_for_changes_nothing() {
    use super::{Need, Workspaces};
    let width = 4;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 20,
        masks: 16,
    };
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(
        small.bytes() * 2 * width as u64,
        Error::HeapFull,
    ));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _held = hold(&mut pool, small, &context, &heap).expect("room for the small");
    assert_eq!(pool.free.len(), width);
    assert_eq!(
        hold(&mut pool, large, &context, &heap).err(),
        Some(Error::HeapFull)
    );
    assert_eq!(
        pool.free.len(),
        width,
        "every participant keeps a workspace"
    );
    assert!(pool.free.iter().all(|w| w.size() == small));
    assert_eq!(
        heap.taken(),
        small.bytes() * width as u64,
        "nothing of the refused size is kept"
    );
    assert_eq!(
        context.taken(),
        small.bytes() * width as u64,
        "the context is charged for the small pipeline's workspaces alone"
    );
    let [words, masks] = large.classes().expect("classes");
    assert_eq!(pool.needs.words[words].load(Ordering::Relaxed), 0);
    assert_eq!(
        pool.needs.masks[masks].load(Ordering::Relaxed),
        1,
        "the small one's alone"
    );
    assert_eq!(pool.size, small, "the refused need never counted");
}

#[test]
fn a_growth_refused_after_its_look_shrank_the_set_leaves_it_fitted_to_what_is_held() {
    use super::{Need, Workspaces};
    let width = 2;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let middle = Need {
        words: 1 << 14,
        masks: 16,
    };
    let large = Need {
        words: 1 << 20,
        masks: 16,
    };
    // Room for the middle one's growth beside the small set, far short of the
    // large one's.
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(
        (small.bytes() + middle.words as u64 * 4) * width as u64 + 1024,
        Error::HeapFull,
    ));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _small = hold(&mut pool, small, &context, &heap).expect("held");
    drop(hold(&mut pool, middle, &context, &heap).expect("held"));
    assert_eq!(
        hold(&mut pool, large, &context, &heap).err(),
        Some(Error::HeapFull)
    );
    assert!(pool.free.iter().all(|w| w.size() == small), "shrunk to it");
    assert_eq!(heap.taken(), small.bytes() * width as u64);
    assert_eq!(pool.size, small);
}

#[test]
fn a_growth_refused_while_a_job_holds_the_pool_leaves_it_empty() {
    use super::{Need, Workspaces};
    let width = 4;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 20,
        masks: 16,
    };
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(
        small.bytes() * 2 * width as u64,
        Error::HeapFull,
    ));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _held = hold(&mut pool, small, &context, &heap).expect("room for the small");
    let running = pool.take(small);
    assert_eq!(running.len(), width);
    assert_eq!(
        hold(&mut pool, large, &context, &heap).err(),
        Some(Error::HeapFull)
    );
    assert!(
        pool.free.is_empty(),
        "nothing made while the job held the pool"
    );
    assert_eq!(heap.taken(), small.bytes() * width as u64);
    drop(pool.give(running));
    assert_eq!(pool.free.len(), width);
}

/// A pool `width` wide holding a small pipeline's need and a large one's,
/// a job of the small one lent its workspaces, fitted to the large, and the
/// large pipeline let go while it runs: the pool, the large one's
/// workspaces and the small one's hold.
fn lent_past_the_large(
    width: usize,
    (small, large): (super::Need, super::Need),
    (context, heap): (
        &alloc::sync::Arc<crate::memory::Budget>,
        &alloc::sync::Arc<crate::memory::Budget>,
    ),
) -> (
    super::Workspaces,
    alloc::vec::Vec<super::Workspace>,
    super::Hold,
) {
    let mut pool = super::Workspaces::new().expect("a pool");
    pool.set_width(width);
    let kept = hold(&mut pool, small, context, heap).expect("the small");
    let held = hold(&mut pool, large, context, heap).expect("the large");
    let running = pool.take(small);
    assert_eq!(running.len(), width);
    assert!(running.iter().all(|w| w.size() == large));
    drop(held);
    (pool, running, kept)
}

#[test]
fn a_pipeline_let_go_while_a_job_holds_what_it_sized_stays_charged_until_they_come_back() {
    use super::Need;
    let width = 4;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 16,
        masks: 16,
    };
    let claim = |need: Need| need.bytes() * width as u64;
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::HeapFull));
    // Room for the two pipelines' claims and no more.
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(
        claim(small) + claim(large),
        Error::OverLimit,
    ));
    let (mut pool, running, _small) = lent_past_the_large(width, (small, large), (&context, &heap));
    assert_eq!(
        context.taken(),
        claim(small) + claim(large),
        "what the job holds is still paid for"
    );
    assert_eq!(
        hold(&mut pool, large, &context, &heap).err(),
        Some(Error::OverLimit),
        "so the large one again would pass the limit"
    );
    assert_eq!(heap.taken(), claim(large), "and nothing else was made");
    drop(pool.give(running));
    assert_eq!(context.taken(), claim(small), "given back with them");
    assert_eq!(heap.taken(), claim(small), "the pool fitted to the small");
}

#[test]
fn a_pipeline_made_while_a_job_holds_workspaces_that_cover_it_makes_no_more() {
    use super::Need;
    let width = 4;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 16,
        masks: 16,
    };
    let claim = |need: Need| need.bytes() * width as u64;
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::HeapFull));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let (mut pool, running, _small) = lent_past_the_large(width, (small, large), (&context, &heap));
    let again = hold(&mut pool, large, &context, &heap).expect("the large again");
    assert_eq!(heap.taken(), claim(large), "the job's workspaces cover it");
    assert_eq!(
        context.taken(),
        claim(small) + 2 * claim(large),
        "what the job holds paid for by the pipeline let go, the new one by its own"
    );
    drop(pool.give(running));
    assert_eq!(heap.taken(), claim(large), "kept for the new one");
    assert!(pool.free.iter().all(|w| w.size() == large));
    assert_eq!(context.taken(), claim(small) + claim(large));
    drop(again);
}

#[test]
fn a_pool_is_lent_to_one_job_at_a_time() {
    use super::{Need, Workspaces};
    let width = 4;
    let need = Need {
        words: 1024,
        masks: 16,
    };
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::HeapFull));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _held = hold(&mut pool, need, &context, &heap).expect("held");
    let first = pool.take(need);
    assert_eq!(first.len(), width);
    assert!(
        pool.take(need).is_empty(),
        "none while a job holds them, as its claim pays for one pool"
    );
    assert_eq!(heap.taken(), need.bytes() * width as u64);
    drop(pool.give(first));
    assert_eq!(pool.take(need).len(), width);
}

#[test]
fn a_hold_let_go_between_a_look_and_the_lending_is_fitted_out_of_the_set_lent() {
    use super::{Need, Workspaces};
    let width = 4;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 16,
        masks: 16,
    };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _small = hold(&mut pool, small, &context, &heap).expect("the small");
    let held = hold(&mut pool, large, &context, &heap).expect("the large");
    // A dispatch's look while the large is held, then its last holder gone
    // elsewhere before the lending.
    drop(pool.prune());
    drop(held);
    let lent = pool.take(small);
    assert_eq!(lent.len(), width);
    assert!(
        lent.iter().all(|w| w.size() == small),
        "fitted to what is held as it is lent"
    );
    let paid = small.bytes() * width as u64;
    assert_eq!(heap.taken(), paid, "nothing kept that no claim pays for");
    assert_eq!(context.taken(), paid);
    drop(pool.give(lent));
}

#[test]
fn a_dispatch_lends_the_set_fitted_to_what_is_still_held() {
    use super::Need;
    use crate::device_tests::{submit, unstarted, Unstarted};
    let threads = Unstarted::default();
    let device = unstarted(&threads);
    let context = context(&device);
    let small = rig(&context, &straight_line(1), 256, &[]).expect("a pipeline");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&small.pipeline, &[&small.set], &[], [1, 1, 1])
        .expect("recorded");
    let _fence = submit(&crate::device_tests::queue(&context), &commands)
        .expect("queued, for no worker runs it");
    let mut model = context.shared.model.lock();
    let fitted = model
        .live(context.key)
        .ok()
        .and_then(|state| state.compute_pipelines.get(small.pipeline.key))
        .and_then(|pipeline| pipeline.need.classes())
        .map(Need::of_classes)
        .expect("the small pipeline's need");
    // A larger need held as a pipeline's is, and let go with nothing looking.
    let large = Need {
        words: 1 << 16,
        masks: 64,
    };
    let (plan, released) = model.plan_workspaces(context.key, large, 0);
    drop(released);
    let (held, released) = model.install_workspaces(plan.expect("planned").make().expect("made"));
    drop(released);
    drop(held);
    let (next, pruned) = model.dispatch(&|| 0);
    drop(pruned);
    let mut dispatch = next.expect("the small pipeline's job");
    assert!(!dispatch.workspaces.is_empty());
    assert!(
        dispatch.workspaces.iter().all(|w| w.size() == fitted),
        "fitted to the small pipeline alone"
    );
    let woken = model.complete(dispatch.engine, dispatch.job, None);
    let given = model.give_back(core::mem::take(&mut dispatch.workspaces));
    drop(model);
    drop((dispatch, woken, given));
}

/// Budgets without limit, for a heap and a context.
fn unlimited() -> (
    alloc::sync::Arc<crate::memory::Budget>,
    alloc::sync::Arc<crate::memory::Budget>,
) {
    use crate::memory::Budget;
    (
        alloc::sync::Arc::new(Budget::new(u64::MAX, Error::HeapFull)),
        alloc::sync::Arc::new(Budget::new(u64::MAX, Error::OverLimit)),
    )
}

#[test]
fn a_growth_while_a_job_holds_the_set_keeps_only_what_its_refit_takes() {
    use super::{Need, Workspaces};
    let width = 4;
    let wide = Need {
        words: 1 << 16,
        masks: 16,
    };
    let branchy = Need {
        words: 64,
        masks: 1024,
    };
    let both = Need {
        words: 1 << 16,
        masks: 1024,
    };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _wide = hold(&mut pool, wide, &context, &heap).expect("held");
    let running = pool.take(wide);
    let lent = heap.taken();
    let _branchy = hold(&mut pool, branchy, &context, &heap).expect("held");
    assert!(pool.free.is_empty(), "no set beside the lent one");
    assert_eq!(
        heap.taken(),
        lent + 1024 * 8 * width as u64,
        "masks for the lent set made, and nothing more"
    );
    drop(pool.give(running));
    assert!(pool.free.iter().all(|w| w.size() == both));
    assert_eq!(
        heap.taken(),
        both.bytes() * width as u64,
        "the masks they replaced let go"
    );
}

#[test]
fn the_last_pipeline_covering_a_job_s_workspaces_pays_for_them_whatever_sized_them() {
    use super::{Need, Workspaces};
    let width = 4;
    let need = |words: usize| Need { words, masks: 16 };
    let claim = |n: Need| n.bytes() * width as u64;
    let (heap, own) = unlimited();
    let (_, other) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _small = hold(&mut pool, need(1024), &own, &heap).expect("held");
    let sizing = hold(&mut pool, need(1 << 16), &own, &heap).expect("held");
    let running = pool.take(need(1024));
    let larger = hold(&mut pool, need(1 << 17), &other, &heap).expect("held");
    drop(sizing);
    assert_eq!(
        own.taken(),
        claim(need(1024)),
        "a pipeline that sized the lent set pays nothing more while a larger one covers it"
    );
    drop(larger);
    drop(pool.prune());
    assert_eq!(
        other.taken(),
        claim(need(1 << 17)),
        "the last pipeline covering the lent set pays for it until it comes back"
    );
    drop(pool.give(running));
    assert_eq!(other.taken(), 0);
    assert_eq!(own.taken(), claim(need(1024)));
    assert_eq!(heap.taken(), claim(need(1024)));
}

#[test]
fn a_set_lent_while_a_larger_growth_is_planned_is_paid_for_by_the_last_pipeline_covering_it() {
    use super::{Need, Workspaces};
    let width = 4;
    let need = |words: usize| Need { words, masks: 16 };
    let claim = |n: Need| n.bytes() * width as u64;
    let (heap, own) = unlimited();
    let (_, other) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _small = hold(&mut pool, need(1024), &own, &heap).expect("held");
    let sizing = hold(&mut pool, need(1 << 16), &own, &heap).expect("held");
    // Planned and made, not yet installed, as a pipeline's making is between
    // two holds of the device's lock.
    let (plan, released) = pool.plan(need(1 << 17), (&other, 0), &heap);
    drop(released);
    let grown = plan.expect("planned").make().expect("made");
    drop(sizing);
    drop(pool.prune());
    let running = pool.take(need(1024));
    assert!(
        running.iter().all(|w| w.size() == need(1 << 16)),
        "not shrunk below the need planned"
    );
    let (larger, released) = pool.install(grown);
    drop(released);
    drop(larger);
    assert_eq!(
        other.taken(),
        claim(need(1 << 17)),
        "the last pipeline covering the lent set pays for it until it comes back"
    );
    drop(pool.give(running));
    assert_eq!(other.taken(), 0);
    assert_eq!(own.taken(), claim(need(1024)));
    assert_eq!(heap.taken(), claim(need(1024)));
}

#[test]
fn a_job_counts_on_from_a_dispatch_s_pieces_and_nothing_a_reading_covered() {
    use super::counted_on;
    let check = WORK_PER_CHECK;
    assert_eq!(
        counted_on(check / 2, core::iter::empty()),
        (check / 2, false),
        "where no piece claimed workgroups, the job's count stands"
    );
    assert_eq!(
        counted_on(
            check / 2,
            [(check / 2 + 10, false), (check / 2 + 30, false)].into_iter()
        ),
        (check / 2 + 30, false),
        "where none read the clock, the most any left"
    );
    assert_eq!(
        counted_on(
            check - 5,
            [(100, true), (check - 5 + 40, false)].into_iter()
        ),
        (100, true),
        "a reading covers the job's count, so another piece counts its own work alone"
    );
    assert_eq!(
        counted_on(check - 5, [(10, true), (check - 5 + 40, false)].into_iter()),
        (40, true),
        "however little the reader then did"
    );
}

#[test]
fn a_pipeline_made_and_dropped_while_a_job_holds_the_set_leaves_nothing_beside_it() {
    use super::{Need, Workspaces};
    let width = 4;
    let need = |words: usize| Need { words, masks: 16 };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _held = hold(&mut pool, need(1 << 16), &context, &heap).expect("held");
    let running = pool.take(need(1 << 16));
    let lent = heap.taken();
    let larger = hold(&mut pool, need(1 << 17), &context, &heap).expect("held");
    drop(larger);
    drop(pool.prune());
    assert!(pool.free.is_empty());
    assert_eq!(
        heap.taken(),
        lent,
        "nothing kept or made beside the lent set"
    );
    drop(pool.give(running));
    assert_eq!(heap.taken(), lent, "the set comes back as it went");
}

#[test]
fn a_kept_claim_pays_for_every_part_it_covers() {
    use super::{Need, Workspaces};
    let width = 4;
    let (heap, running) = unlimited();
    let (_, sizing) = unlimited();
    let (_, other) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let small = Need {
        words: 64,
        masks: 16,
    };
    let wide = Need {
        words: 1 << 16,
        masks: 1024,
    };
    let branchy = Need {
        words: 64,
        masks: 1024,
    };
    let _small = hold(&mut pool, small, &running, &heap).expect("held");
    let wide_hold = hold(&mut pool, wide, &sizing, &heap).expect("held");
    let branchy_hold = hold(&mut pool, branchy, &other, &heap).expect("held");
    let lent = pool.take(small);
    // The last of the lent words' size, whose claim covers the masks too.
    drop(wide_hold);
    drop(branchy_hold);
    assert_eq!(other.taken(), 0, "the masks are paid for by the claim kept");
    assert_eq!(sizing.taken(), wide.bytes() * width as u64);
    drop(pool.give(lent));
    assert_eq!(sizing.taken(), 0);
    assert!(
        pool.free.iter().all(|w| w.size() == small),
        "both parts fitted down"
    );
    assert_eq!(heap.taken(), small.bytes() * width as u64);
}

#[test]
fn a_growth_while_a_job_holds_the_set_is_made_for_the_workspaces_lent() {
    use super::{Need, Workspaces};
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 2048,
        masks: 16,
    };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(4);
    let _small = hold(&mut pool, small, &context, &heap).expect("held");
    // Wider than the set the job is lent.
    pool.set_width(8);
    let running = pool.take(small);
    assert_eq!(running.len(), 4);
    let lent = heap.taken();
    let _large = hold(&mut pool, large, &context, &heap).expect("held");
    assert_eq!(heap.taken(), lent + 4 * large.words as u64 * 4);
    drop(pool.give(running));
}

#[test]
fn growths_planned_together_each_cover_their_own_pipeline_whichever_installs_first() {
    use super::{Need, Workspaces};
    let need = |words: usize| Need { words, masks: 16 };
    for (based, larger_first) in [(false, false), (false, true), (true, false), (true, true)] {
        let (heap, context) = unlimited();
        let mut pool = Workspaces::new().expect("a pool");
        pool.set_width(2);
        // With a base held both grow the set; without, both make it whole.
        let _base = based.then(|| hold(&mut pool, need(64), &context, &heap).expect("held"));
        // Both planned against the set as it stands, neither yet installed.
        let smaller = pool
            .plan(need(1024), (&context, 0), &heap)
            .0
            .expect("planned");
        let larger = pool
            .plan(need(4096), (&context, 0), &heap)
            .0
            .expect("planned");
        let (smaller, larger) = (smaller.make().expect("made"), larger.make().expect("made"));
        let order = if larger_first {
            [larger, smaller]
        } else {
            [smaller, larger]
        };
        let mut holds = alloc::vec::Vec::new();
        for grown in order {
            let (hold, released) = pool.install(grown);
            drop(released);
            holds.push(hold.expect("held"));
            assert!(
                pool.free.iter().all(|w| w.size().words >= 1024),
                "the first installed covered, larger first {larger_first}"
            );
        }
        assert_eq!(pool.free.len(), 2);
        assert!(
            pool.free.iter().all(|w| w.size() == need(4096)),
            "both covered, a base held {based}, larger first {larger_first}"
        );
        assert_eq!(
            heap.taken(),
            need(4096).bytes() * 2,
            "what they replaced let go"
        );
    }
}

#[test]
fn a_growth_planned_before_a_job_takes_the_set_is_kept_until_it_comes_back() {
    use super::{Need, Workspaces};
    let need = |words: usize| Need { words, masks: 16 };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(2);
    let _base = hold(&mut pool, need(64), &context, &heap).expect("held");
    let plan = pool
        .plan(need(4096), (&context, 0), &heap)
        .0
        .expect("planned");
    let grown = plan.make().expect("made");
    let running = pool.take(need(64));
    let (held, released) = pool.install(grown);
    drop(released);
    assert!(held.is_some());
    assert!(
        running.iter().all(|w| w.size() == need(64)),
        "the job's set untouched"
    );
    drop(pool.prune());
    drop(pool.give(running));
    assert!(
        pool.free.iter().all(|w| w.size() == need(4096)),
        "grown as it came back"
    );
    assert_eq!(heap.taken(), need(4096).bytes() * 2);
}

#[test]
fn a_set_made_whole_before_a_job_takes_the_set_outlasts_growths_kept_meanwhile() {
    use super::{Need, Workspaces};
    let need = |words: usize| Need { words, masks: 16 };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(2);
    // Both planned while the pool has no set, so each makes it whole.
    let largest = pool
        .plan(need(4096), (&context, 0), &heap)
        .0
        .expect("planned");
    let first = pool
        .plan(need(64), (&context, 0), &heap)
        .0
        .expect("planned");
    let (largest, first) = (largest.make().expect("made"), first.make().expect("made"));
    let (_first, released) = pool.install(first);
    drop(released);
    let running = pool.take(need(64));
    // Kept for the set first, growing it part by part.
    let _middle = hold(&mut pool, need(1024), &context, &heap).expect("held");
    let (held, released) = pool.install(largest);
    drop(released);
    assert!(held.is_some());
    drop(pool.prune());
    drop(pool.give(running));
    assert!(
        pool.free.iter().all(|w| w.size() == need(4096)),
        "the largest covered as the set comes back"
    );
    assert_eq!(heap.taken(), need(4096).bytes() * 2, "the rest let go");
}

#[test]
fn a_set_no_pipeline_holds_is_let_go_before_the_heap_refuses_a_new_one() {
    use super::{Need, Workspaces};
    let width = 2;
    let old = Need {
        words: 1 << 16,
        masks: 16,
    };
    let new = Need {
        words: 1 << 17,
        masks: 16,
    };
    // Room for the new set, not for it beside the old.
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(
        new.bytes() * width as u64,
        Error::HeapFull,
    ));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    drop(hold(&mut pool, old, &context, &heap).expect("the old"));
    // Nothing has looked at the pool since its last hold went.
    assert_eq!(heap.taken(), old.bytes() * width as u64);
    let _new = hold(&mut pool, new, &context, &heap).expect("the old set let go first");
    assert!(pool.free.iter().all(|w| w.size() == new));
    assert_eq!(heap.taken(), new.bytes() * width as u64);
}

#[test]
fn a_pipeline_made_while_a_job_holds_the_set_counts_what_is_kept_for_it() {
    use super::{Need, Workspaces};
    let width = 4;
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 16,
        masks: 16,
    };
    let (lent, growth) = (
        small.bytes() * width as u64,
        (large.words * 4 * width) as u64,
    );
    // Room for the set and one growth of it, not two.
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(
        lent + growth + 1024,
        Error::HeapFull,
    ));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _small = hold(&mut pool, small, &context, &heap).expect("held");
    let running = pool.take(small);
    let _first = hold(&mut pool, large, &context, &heap).expect("the first large one");
    let _second = hold(&mut pool, large, &context, &heap)
        .expect("a second, which what is kept for the set covers");
    assert_eq!(heap.taken(), lent + growth, "nothing made for the second");
    drop(pool.give(running));
    assert!(pool.free.iter().all(|w| w.size() == large));
}

#[test]
fn a_pipeline_made_while_a_job_holds_the_set_counts_a_set_kept_whole_for_it() {
    use super::{Need, Workspaces};
    let need = |words: usize| Need { words, masks: 16 };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(2);
    // Both planned while there is no set, so each makes it whole.
    let largest = pool
        .plan(need(4096), (&context, 0), &heap)
        .0
        .expect("planned");
    let first = pool
        .plan(need(64), (&context, 0), &heap)
        .0
        .expect("planned");
    let (largest, first) = (largest.make().expect("made"), first.make().expect("made"));
    let (_first, released) = pool.install(first);
    drop(released);
    let running = pool.take(need(64));
    let (_largest, released) = pool.install(largest);
    drop(released);
    let taken = heap.taken();
    let _covered = hold(&mut pool, need(2048), &context, &heap).expect("held");
    assert_eq!(heap.taken(), taken, "what is kept whole covers it");
    drop(pool.give(running));
    assert!(pool.free.iter().all(|w| w.size() == need(4096)));
}

#[test]
fn what_a_lent_sets_keep_lets_go_goes_as_it_was_made() {
    use super::{Need, Workspaces};
    let width = 2;
    let need = |words: usize| Need { words, masks: 16 };
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(width);
    let _held = hold(&mut pool, need(1 << 16), &context, &heap).expect("held");
    let running = pool.take(need(1 << 16));
    drop(hold(&mut pool, need(1 << 17), &context, &heap).expect("held"));
    let gone = pool.prune();
    assert_eq!(
        gone.words.len(),
        width,
        "every part kept for the set let go"
    );
    assert!(
        gone.words.iter().all(|part| part.values.len() == 1 << 17),
        "unshrunk, as nothing more is done with what goes"
    );
    drop(gone);
    drop(pool.give(running));
}

#[test]
fn a_growth_kept_for_a_lent_set_outlasts_a_look_at_the_pool_in_either_kind() {
    use super::{Need, Workspaces};
    let width = 2;
    let base = Need {
        words: 64,
        masks: 16,
    };
    for grown in [
        Need {
            words: 4096,
            masks: 16,
        },
        Need {
            words: 64,
            masks: 1024,
        },
    ] {
        let (heap, context) = unlimited();
        let mut pool = Workspaces::new().expect("a pool");
        pool.set_width(width);
        let _base = hold(&mut pool, base, &context, &heap).expect("held");
        let running = pool.take(base);
        let _grown = hold(&mut pool, grown, &context, &heap).expect("held");
        drop(pool.prune());
        drop(pool.give(running));
        assert!(
            pool.free.iter().all(|w| w.size() == base.max(grown)),
            "{grown:?} kept"
        );
        assert_eq!(heap.taken(), base.max(grown).bytes() * width as u64);
    }
}

#[test]
fn growths_kept_for_a_lent_set_together_cover_every_pipeline_whichever_came_first() {
    use super::{Need, Workspaces};
    let width = 2;
    let need = |words: usize, masks: usize| Need { words, masks };
    let base = need(64, 16);
    for pair in [
        [need(2048, 16), need(4096, 16)],
        [need(4096, 16), need(2048, 16)],
        [need(4096, 16), need(64, 1024)],
        [need(64, 1024), need(4096, 16)],
    ] {
        let (heap, context) = unlimited();
        let mut pool = Workspaces::new().expect("a pool");
        pool.set_width(width);
        let _base = hold(&mut pool, base, &context, &heap).expect("held");
        let running = pool.take(base);
        let _holds = pair.map(|grown| hold(&mut pool, grown, &context, &heap).expect("held"));
        drop(pool.give(running));
        let covering = pair.iter().fold(base, |all, &grown| all.max(grown));
        assert!(
            pool.free.iter().all(|w| w.size() == covering),
            "{pair:?} covered"
        );
        assert_eq!(heap.taken(), covering.bytes() * width as u64);
    }
}

#[test]
fn growths_planned_together_in_different_kinds_each_cover_their_own() {
    use super::{Need, Workspaces};
    let width = 2;
    let wide = Need {
        words: 4096,
        masks: 16,
    };
    let branchy = Need {
        words: 64,
        masks: 1024,
    };
    let both = wide.max(branchy);
    for (based, wide_first) in [(false, false), (false, true), (true, false), (true, true)] {
        let (heap, context) = unlimited();
        let mut pool = Workspaces::new().expect("a pool");
        pool.set_width(width);
        let base = Need {
            words: 64,
            masks: 16,
        };
        let _base = based.then(|| hold(&mut pool, base, &context, &heap).expect("held"));
        let wide = pool.plan(wide, (&context, 0), &heap).0.expect("planned");
        let branchy = pool.plan(branchy, (&context, 0), &heap).0.expect("planned");
        let (wide, branchy) = (wide.make().expect("made"), branchy.make().expect("made"));
        let order = if wide_first {
            [wide, branchy]
        } else {
            [branchy, wide]
        };
        let _holds = order.map(|grown| {
            let (hold, released) = pool.install(grown);
            drop(released);
            hold.expect("held")
        });
        assert!(
            pool.free.iter().all(|w| w.size() == both),
            "a base held {based}, the wide one first {wide_first}"
        );
        assert_eq!(heap.taken(), both.bytes() * width as u64);
    }
}

#[test]
fn a_plan_whose_parts_cannot_be_made_gives_back_all_it_was_charged() {
    use super::{Need, Workspaces};
    // Past what any host can hold, so making it is refused before it is
    // asked of the allocator.
    let vast = Need {
        words: 1 << 61,
        masks: 16,
    };
    let base = Need {
        words: 64,
        masks: 16,
    };
    for lent in [false, true] {
        let (heap, context) = unlimited();
        let mut pool = Workspaces::new().expect("a pool");
        let _base = hold(&mut pool, base, &context, &heap).expect("held");
        let running = lent.then(|| pool.take(base));
        let (taken, charged) = (heap.taken(), context.taken());
        let plan = pool
            .plan(vast, (&context, 0), &heap)
            .0
            .expect("charged within both");
        assert!(plan.make().is_err(), "the host cannot make it, lent {lent}");
        assert_eq!(heap.taken(), taken, "its room given back, lent {lent}");
        assert_eq!(context.taken(), charged, "and its claim, lent {lent}");
        if let Some(running) = running {
            drop(pool.give(running));
        }
        drop(pool.prune());
        assert_eq!(pool.size, base, "its need forgotten, lent {lent}");
    }
}

#[test]
fn a_change_in_one_part_refits_that_part_alone() {
    use super::{Need, Workspaces};
    let (heap, context) = unlimited();
    let mut pool = Workspaces::new().expect("a pool");
    pool.set_width(2);
    let _wide = hold(
        &mut pool,
        Need {
            words: 1 << 16,
            masks: 16,
        },
        &context,
        &heap,
    )
    .expect("held");
    for workspace in &mut pool.free {
        if let Some(word) = workspace.words.first_mut() {
            *word = 7;
        }
    }
    let _branchy = hold(
        &mut pool,
        Need {
            words: 64,
            masks: 1024,
        },
        &context,
        &heap,
    )
    .expect("held");
    assert!(pool.free.iter().all(|w| w.masks.len() == 1024));
    assert!(
        pool.free.iter().all(|w| w.words.first() == Some(&7)),
        "the words are kept, not made again"
    );
}

#[test]
fn a_dropped_pipeline_s_need_is_forgotten_as_its_hold_goes() {
    use super::{Need, Workspaces};
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::HeapFull));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    let need = |words: usize| Need { words, masks: 1 };
    let mut holds: alloc::vec::Vec<_> = (1..=64)
        .map(|k| hold(&mut pool, need(k * 4), &context, &heap).expect("held"))
        .collect();
    assert_eq!(pool.size, need(256));
    holds.truncate(32);
    drop(pool.prune());
    assert_eq!(pool.size, need(128), "the largest still held");
    assert!(pool.free.iter().all(|w| w.size() == need(128)));
    drop(holds);
    drop(pool.prune());
    assert!(pool.free.is_empty() && pool.size == Need::default());
    assert_eq!(context.taken(), 0, "every claim given back");
    assert_eq!(heap.taken(), 0);
}

#[test]
fn a_need_rounds_up_to_a_class_at_most_an_eighth_above_it() {
    use super::{bound_of, class_of, CLASSES};
    let mut last = 0;
    for n in (0..4096).chain([65_535, 1 << 20, (1 << 20) + 1, usize::MAX >> 4]) {
        let class = class_of(n).expect("a class");
        let bound = bound_of(class);
        assert!(bound >= n, "{n} rounds up, to {bound}");
        assert!(
            bound - n <= n / 8,
            "{n} rounds to {bound}, past an eighth above it"
        );
        assert_eq!(class_of(bound), Some(class), "{bound} is its own class");
        if n < 4096 {
            assert!(class >= last, "classes rise with sizes");
            last = class;
        }
    }
    assert_eq!(bound_of(CLASSES - 1), 0xf << (usize::BITS - 4));
    assert_eq!(class_of(usize::MAX), None, "past every class");
}

#[test]
fn a_lone_workspace_shrinks_in_place_without_room_for_a_smaller_one() {
    use super::{Need, Workspaces};
    let small = Need {
        words: 1024,
        masks: 16,
    };
    let large = Need {
        words: 1 << 20,
        masks: 16,
    };
    // Room for the large workspace and a little, not for both at once.
    let heap = alloc::sync::Arc::new(crate::memory::Budget::new(
        large.bytes() + small.bytes() / 2,
        Error::HeapFull,
    ));
    let context = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut pool = Workspaces::new().expect("a pool");
    let held = hold(&mut pool, large, &context, &heap).expect("room for the large");
    let _small = hold(&mut pool, small, &context, &heap)
        .expect("room for the small, which the large covers");
    assert_eq!(heap.taken(), large.bytes());
    drop(held);
    drop(pool.prune());
    assert_eq!(pool.free.len(), 1);
    assert_eq!(
        heap.taken(),
        small.bytes(),
        "the large one shrinks in place, though the heap had no room for both"
    );
    let workspace = pool.free.first().expect("the workspace");
    assert_eq!(
        (
            tairix_util::fallible::held(&workspace.words),
            tairix_util::fallible::held(&workspace.masks),
        ),
        (small.words as u64 * 4, small.masks as u64 * 8),
        "its block given back to the allocator, not only its length"
    );
}

/// Submit `count` dispatches of `groups` workgroups of `pipeline` over `set`
/// as one job while the clock steps `step` nanoseconds a reading: what the
/// context's state then is.
fn many(
    clock: &Clock,
    context: &Context,
    (pipeline, set): (&crate::ComputePipeline, &crate::BindSet),
    (count, groups): (usize, [u32; 3]),
    step: u64,
) -> Result<(), Error> {
    let mut commands = context.command_buffer();
    for _ in 0..count {
        commands
            .dispatch(pipeline, &[set], &[], groups)
            .expect("recorded");
    }
    clock.step.store(step, Ordering::SeqCst);
    let submitted = crate::device_tests::submit(&crate::device_tests::queue(context), &commands);
    clock.step.store(0, Ordering::SeqCst);
    submitted.map(|_| ()).map_err(|e| e.error)?;
    context.status()
}

#[test]
fn a_job_of_dispatches_each_short_of_a_check_still_stops_past_its_deadline() {
    let (device, clock) = device();
    let context = context(&device);
    // An eighth of a check's work each, four checks' in all.
    let adds = WORK_PER_CHECK / 8 / 64;
    let rig = rig(&context, &straight_line(adds), 256, &[]).expect("a pipeline");
    let job = (&rig.pipeline, &rig.set);
    assert_eq!(many(&clock, &context, job, (32, [1, 1, 1]), 0), Ok(()));
    // The second check, four dispatches' worth in, reads 1.2 seconds.
    assert_eq!(
        many(&clock, &context, job, (32, [1, 1, 1]), 600 * MILLISECOND),
        Err(Error::Lost(Loss::Overran))
    );
}

/// A pipeline storing through the first of `DESCRIPTORS` storage buffers
/// bound as one array, and a set binding one buffer as every one of them.
struct Wide {
    pipeline: crate::ComputePipeline,
    set: crate::BindSet,
    buffer: crate::Buffer,
    _memory: crate::Allocation,
    _layout: crate::PipelineLayout,
    _set_layout: crate::BindSetLayout,
}

const DESCRIPTORS: u32 = 4096;

fn wide(context: &Context) -> Result<Wide, Error> {
    wide_of(context, DESCRIPTORS)
}

/// [`wide`], of `descriptors` storage buffers.
fn wide_of(context: &Context, descriptors: u32) -> Result<Wide, Error> {
    use tairix_spirv::{Access, BindingKind, Stages};
    let text = alloc::format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %bufs
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %bufs DescriptorSet 0
OpDecorate %bufs Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%count = OpConstant %uint {descriptors}
%block = OpTypeStruct %uint
%blocks = OpTypeArray %block %count
%blocks_ptr = OpTypePointer StorageBuffer %blocks
%word_ptr = OpTypePointer StorageBuffer %uint
%bufs = OpVariable %blocks_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%at = OpAccessChain %word_ptr %bufs %zero %zero
OpStore %at %one
OpReturn
OpFunctionEnd
"#
    );
    let module =
        context.create_shader_module(tairix_spirv::assemble::admit(&text).expect("admitted"))?;
    let set_layout = context.create_bind_set_layout(&[crate::LayoutBinding {
        binding: 0,
        kind: BindingKind::StorageBuffer,
        count: descriptors,
        access: Access::READ.with(Access::WRITE),
        stages: Stages::COMPUTE,
    }])?;
    let layout = context.create_pipeline_layout(&[&set_layout], crate::PushConstants::NONE)?;
    let pipeline = context.create_compute_pipeline(&crate::ComputePipelineDesc {
        module: &module,
        entry_point: "main",
        layout: &layout,
        specialization: &[],
    })?;
    let memory = context.allocate(&crate::AllocationDesc {
        size: 256,
        memory_type: 0,
    })?;
    let buffer = context.create_buffer(
        &memory,
        &crate::BufferDesc {
            offset: 0,
            size: 256,
            usage: crate::BufferUsage::ALL,
        },
    )?;
    let resources: alloc::vec::Vec<crate::Resource<'_>> = (0..descriptors)
        .map(|_| crate::Resource::Buffer {
            buffer: &buffer,
            offset: 0,
            size: 4,
        })
        .collect();
    let set = context.create_bind_set(
        &set_layout,
        &[crate::BindSetEntry {
            binding: 0,
            resources: &resources,
        }],
    )?;
    Ok(Wide {
        pipeline,
        set,
        buffer,
        _memory: memory,
        _layout: layout,
        _set_layout: set_layout,
    })
}

#[test]
fn a_dispatch_of_no_workgroups_still_counts_the_descriptors_it_resolves() {
    let (device, clock) = device();
    let context = context(&device);
    let wide = wide(&context).expect("a pipeline over a wide set");
    // A check's worth of descriptors every 256 dispatches.
    let job = (&wide.pipeline, &wide.set);
    assert_eq!(many(&clock, &context, job, (1024, [0, 0, 0]), 0), Ok(()));
    assert_eq!(
        many(&clock, &context, job, (1024, [0, 0, 0]), 600 * MILLISECOND),
        Err(Error::Lost(Loss::Overran))
    );
}

#[test]
fn a_job_s_descriptors_are_counted_as_they_are_resolved() {
    let clock = alloc::sync::Arc::new(Clock::default());
    let device = crate::device_tests::software(
        1 << 30,
        alloc::sync::Arc::clone(&clock)
            as alloc::sync::Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync>,
    );
    let context = context(&device);
    // Sets of the most descriptors a layout holds, dispatched until two
    // checks' worth and a little are resolved: they pass two checks before
    // any workgroup runs.
    let descriptors = device.info().limits.max_bind_set_descriptors;
    let wide = wide_of(&context, descriptors).expect("a pipeline over a wide set");
    let dispatches = (2 * WORK_PER_CHECK + 1024).div_ceil(u64::from(descriptors));
    clock.readings.store(0, Ordering::SeqCst);
    let job = (&wide.pipeline, &wide.set);
    let count = usize::try_from(dispatches).expect("a few dozen");
    assert_eq!(many(&clock, &context, job, (count, [0, 0, 0]), 0), Ok(()));
    assert_eq!(
        clock.readings.load(Ordering::SeqCst),
        DONE_READINGS + 2,
        "a reading at each check's worth of descriptors"
    );
}

#[test]
fn a_queued_dispatch_holds_its_sets_and_nothing_for_each_memory_they_reach() {
    let (device, _) = device();
    let context = context(&device);
    let wide = wide(&context).expect("a pipeline over a wide set");
    let memory = {
        let model = context.shared.model.lock();
        let live = model.live(context.key).expect("live");
        let set = live.bind_sets.get(wide.set.key).expect("filed");
        assert_eq!(
            set.memories.len(),
            1,
            "one memory however often it is bound"
        );
        alloc::sync::Arc::clone(&live.buffers.get(wide.buffer.key).expect("filed").memory)
    };
    let gate = context.create_timeline(0).expect("timeline");
    let mut commands = context.command_buffer();
    for _ in 0..64 {
        commands
            .dispatch(&wide.pipeline, &[&wide.set], &[], [1, 1, 1])
            .expect("recorded");
    }
    let before = alloc::sync::Arc::strong_count(&memory);
    let fence = crate::device_tests::queue(&context)
        .submit(&crate::Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: tairix_abi::Duration64::from_secs(1),
        })
        .expect("admitted");
    assert_eq!(fence.is_signalled(), Ok(false), "queued behind its gate");
    assert_eq!(
        alloc::sync::Arc::strong_count(&memory),
        before,
        "the queued dispatches hold the memory only through their set"
    );
    gate.signal(1).expect("advances");
    assert_eq!(fence.is_signalled(), Ok(true));
    assert_eq!(context.status(), Ok(()));
}

#[test]
fn a_workgroup_past_four_billion_has_its_own_id() {
    use super::workgroup_id;
    let groups = [65535, 32769, 2];
    let past = (1u64 << 32) + 7;
    assert_eq!(workgroup_id(past, groups), [8, 32768, 1]);
    let last = u64::from(groups[0]) * u64::from(groups[1]) * u64::from(groups[2]) - 1;
    assert_eq!(workgroup_id(last, groups), [65534, 32768, 1]);
    assert_eq!(workgroup_id(0, groups), [0, 0, 0]);
}

#[test]
fn a_dispatch_past_any_32_bit_count_is_split_whole() {
    use super::per_claim;
    let total = (1u64 << 32) - 4;
    assert_eq!(per_claim(total, 64, 1), 1 << 32, "one claim of whole tiles");
    let most = 65535u64 * 65535 * 65535;
    for claims in [1, 3, 7, 64] {
        let per = per_claim(most, 64, claims);
        assert_eq!(per % 64, 0, "whole tiles");
        assert!(per * claims as u64 >= most, "every workgroup in a claim");
        assert!(per * (claims as u64 - 1) < most, "no claim of nothing");
    }
}

#[test]
fn a_run_s_tables_are_counted_past_any_word_without_wrapping() {
    use super::table_bytes;
    assert_eq!(
        table_bytes([(3, 8), (1, 16), (5, 4), (0, 40), (2, 48), (4, 8)]),
        188
    );
    assert_eq!(
        table_bytes([
            (u64::MAX / 2, 40),
            (1, 8),
            (1, 4),
            (u64::MAX, 40),
            (1, 48),
            (64, 8)
        ]),
        u64::MAX,
        "past the counter, held at its most"
    );
}

#[test]
fn memories_are_merged_from_the_sets_lists_in_order_once_each() {
    use super::merge;
    let budget = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut held: alloc::vec::Vec<alloc::sync::Arc<crate::memory::Memory>> = (0..6)
        .map(|_| {
            let charge = |held| crate::memory::Charge::take(&[&budget], 4, held).expect("charged");
            alloc::sync::Arc::new(
                crate::memory::Memory::zeroed(
                    4,
                    charge(crate::memory::Held::Allocation),
                    charge(crate::memory::Held::Host),
                )
                .expect("made"),
            )
        })
        .collect();
    held.sort_unstable_by_key(alloc::sync::Arc::as_ptr);
    let pick = |at: &[usize]| -> alloc::vec::Vec<alloc::sync::Arc<crate::memory::Memory>> {
        at.iter()
            .map(|&k| alloc::sync::Arc::clone(&held[k]))
            .collect()
    };
    let (a, b, c) = (pick(&[0, 2, 4]), pick(&[1, 2, 5]), pick(&[3]));
    let mut lists: alloc::vec::Vec<&[alloc::sync::Arc<crate::memory::Memory>]> =
        alloc::vec![&a, &b, &a, &c];
    let mut merged = alloc::vec::Vec::new();
    let stop = core::sync::atomic::AtomicBool::new(false);
    let clock = Clock::default();
    let mut meter = crate::software::Meter {
        stop: &stop,
        clock: &clock,
        deadline: u64::MAX,
        unchecked: 0,
        read: false,
    };
    assert!(merge(&mut lists, &mut merged, &mut meter));
    let addresses: alloc::vec::Vec<_> =
        merged.iter().map(|m| alloc::sync::Arc::as_ptr(m)).collect();
    let expected: alloc::vec::Vec<_> = held.iter().map(alloc::sync::Arc::as_ptr).collect();
    assert_eq!(addresses, expected, "every memory once, by address");
}

#[test]
fn workgroups_are_claimed_in_turn_from_one_cursor() {
    // Each workgroup's first invocation takes the next of a counter's values
    // and stores it at the workgroup's own word.
    let text = alloc::format!(
        "{HEADER}%bool = OpTypeBool\n%c63 = OpConstant %uint 63\n%c6 = OpConstant %uint 6\n%device = OpConstant %uint 1\n%main = OpFunction %void None %fn\n%entry = OpLabel\n%id = OpLoad %v3uint %gid\n%v0 = OpCompositeExtract %uint %id 0\n%lane = OpBitwiseAnd %uint %v0 %c63\n%first = OpIEqual %bool %lane %zero\nOpSelectionMerge %done None\nOpBranchConditional %first %count %done\n%count = OpLabel\n%counter = OpAccessChain %word_ptr %buf %zero %zero\n%taken = OpAtomicIAdd %uint %counter %device %zero %one\n%group = OpShiftRightLogical %uint %v0 %c6\n%slot = OpIAdd %uint %group %one\n%at = OpAccessChain %word_ptr %buf %zero %slot\nOpStore %at %taken\nOpBranch %done\n%done = OpLabel\nOpReturn\nOpFunctionEnd\n"
    );
    // One thread running four pieces in reverse: the first it runs claims
    // every workgroup, in order, where fixed ranges would run the last first.
    let runner = alloc::sync::Arc::new(tairix_parallel::Reversed::new(4));
    let device = crate::device_tests::software_on(
        crate::device_tests::RAM,
        alloc::sync::Arc::new(Clock::default()),
        alloc::sync::Arc::clone(&runner) as alloc::sync::Arc<dyn tairix_parallel::JobRunner + Send>,
    );
    let context = context(&device);
    let rig = rig(&context, &text, 17 * 4, &[]).expect("a pipeline");
    run(&context, &rig, [16, 1, 1]).expect("dispatched");
    assert!(runner.widest() <= 4, "a piece for each participant at most");
    let order = words(&rig.memory, 17);
    let expected: alloc::vec::Vec<u32> = core::iter::once(16).chain(0..16).collect();
    assert_eq!(order, expected);
}

#[test]
fn every_object_a_context_makes_is_charged_to_it_until_released() {
    let (device, _) = device();
    let context = context(&device);
    let fresh = context.memory_used();
    assert_eq!(fresh.allocations, 0);
    assert!(fresh.objects > 0, "its own queues and fences");
    let round = || {
        let wide = wide(&context).expect("a pipeline over a wide set");
        let used = context.memory_used();
        assert_eq!(used.allocations, 256);
        assert!(
            used.objects > fresh.objects + u64::from(DESCRIPTORS) * 24,
            "the set's descriptors, its pipeline's program and its layouts: {used:?}"
        );
        drop(wide);
        context.memory_used()
    };
    let kept = round();
    assert_eq!(kept.allocations, 0);
    assert_eq!(
        round(),
        kept,
        "every object let go with its handle, the room its tables grew to kept"
    );
}

#[test]
fn objects_past_a_context_s_limit_are_refused() {
    let (device, _) = device();
    let small = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(32 << 10),
        })
        .expect("a context");
    assert_eq!(
        wide(&small).err(),
        Some(Error::OverLimit),
        "a set of four thousand descriptors is past 32 KiB"
    );
    let kept = small.memory_used();
    assert_eq!(
        kept.allocations, 0,
        "and what was made on the way is let go"
    );
    assert_eq!(wide(&small).err(), Some(Error::OverLimit));
    assert_eq!(
        small.memory_used(),
        kept,
        "but for the room its tables grew to, the same again"
    );
}

/// A dispatch of `wide`'s pipeline over one workgroup, in a submission of
/// its own: what the submission answered.
fn dispatched(context: &Context, wide: &Wide) -> Result<(), Error> {
    let mut commands = context.command_buffer();
    commands
        .dispatch(&wide.pipeline, &[&wide.set], &[], [1, 1, 1])
        .expect("recorded");
    crate::device_tests::submit(&crate::device_tests::queue(context), &commands)
        .map(|_| ())
        .map_err(|e| e.error)
}

#[test]
fn a_submission_whose_dispatch_tables_pass_the_limit_is_refused_before_it_queues() {
    let (device, _) = device();
    let objects = {
        let probe = context(&device);
        let wide = wide(&probe).expect("a pipeline over a wide set");
        let used = probe.memory_used().total();
        drop(wide);
        used
    };
    // Room for the objects, but not for the tables a run over four thousand
    // descriptors makes.
    let tight = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(objects + (4 << 10)),
        })
        .expect("a context");
    let wide_tight = wide(&tight).expect("the objects fit");
    let before = tight.memory_used();
    assert_eq!(dispatched(&tight, &wide_tight), Err(Error::OverLimit));
    assert_eq!(tight.memory_used(), before, "nothing was queued or kept");
    assert_eq!(tight.status(), Ok(()), "and the context goes on");

    let roomy = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(objects + (1 << 20)),
        })
        .expect("a context");
    let wide_roomy = wide(&roomy).expect("the objects fit");
    assert_eq!(dispatched(&roomy, &wide_roomy), Ok(()));
    let before = roomy.memory_used();
    assert_eq!(dispatched(&roomy, &wide_roomy), Ok(()));
    assert_eq!(
        roomy.memory_used(),
        before,
        "the job's charge is given back once it is done, the room it took kept"
    );
}

#[test]
fn memory_whose_last_holder_is_a_bind_set_goes_without_the_devices_lock() {
    let (device, _) = device();
    let context = context(&device);
    let Wide {
        pipeline,
        set,
        buffer,
        _memory: memory,
        _layout: layout,
        _set_layout: set_layout,
    } = wide_of(&context, 4).expect("a pipeline over four descriptors");
    drop((pipeline, layout, set_layout, memory, buffer));
    assert_eq!(context.memory_used().allocations, 256, "the set holds it");
    let before = crate::fair::held::drops_under();
    drop(set);
    assert_eq!(context.memory_used().allocations, 0);
    assert_eq!(crate::fair::held::drops_under(), before);
}

#[test]
fn a_job_is_charged_once_for_the_placements_its_dispatches_share() {
    let (device, _) = device();
    let context = context(&device);
    let queue = crate::device_tests::queue(&context);
    let (narrow, wide) = (idle(&context, 1), idle(&context, 1024));
    // What a job of `dispatches` is charged while it waits.
    let charged = |dispatches: &[&crate::ComputePipeline]| {
        let gate = context.create_timeline(0).expect("timeline");
        let mut commands = context.command_buffer();
        for pipeline in dispatches {
            commands
                .dispatch(pipeline, &[], &[], [1, 1, 1])
                .expect("recorded");
        }
        let before = context.memory_used().objects;
        let fence = queue
            .submit(&crate::Submission {
                command_buffers: &[&commands],
                waits: &[(&gate, 1)],
                signals: &[],
                deadline: tairix_abi::Duration64::from_secs(1),
            })
            .expect("admitted");
        let waiting = context.memory_used().objects - before;
        gate.signal(1).expect("advances");
        fence.wait(tairix_abi::Duration64::ZERO).expect("done");
        waiting
    };
    // The queue's tables made room for one job first.
    let _ = charged(&[&narrow]);
    let placement = size_of::<super::Placement>() as u64;
    // A serial runner's one participant: fifteen tiles more.
    assert_eq!(charged(&[&wide]) - charged(&[&narrow]), 15 * placement);
    assert_eq!(
        charged(&[&wide, &wide]) - charged(&[&narrow, &narrow]),
        15 * placement,
        "a job's dispatches share their placements"
    );
}

/// A shader storing one, two, three and four through the two descriptors of
/// binding 1 of set 0 and of binding 3 of set 1.
const TWO_SETS: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %low %high
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %low DescriptorSet 0
OpDecorate %low Binding 1
OpDecorate %high DescriptorSet 1
OpDecorate %high Binding 3
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%three = OpConstant %uint 3
%four = OpConstant %uint 4
%block = OpTypeStruct %uint
%blocks = OpTypeArray %block %two
%blocks_ptr = OpTypePointer StorageBuffer %blocks
%word_ptr = OpTypePointer StorageBuffer %uint
%low = OpVariable %blocks_ptr StorageBuffer
%high = OpVariable %blocks_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%a = OpAccessChain %word_ptr %low %zero %zero
OpStore %a %one
%b = OpAccessChain %word_ptr %low %one %zero
OpStore %b %two
%c = OpAccessChain %word_ptr %high %zero %zero
OpStore %c %three
%d = OpAccessChain %word_ptr %high %one %zero
OpStore %d %four
OpReturn
OpFunctionEnd
"#;

#[test]
fn bindings_of_two_sets_over_shared_and_interleaved_memories_reach_their_own() {
    use tairix_spirv::{Access, BindingKind, Stages};
    let (device, _) = device();
    let context = context(&device);
    let module = context
        .create_shader_module(tairix_spirv::assemble::admit(TWO_SETS).expect("admitted"))
        .expect("a module");
    let binding = |binding, count| crate::LayoutBinding {
        binding,
        kind: BindingKind::StorageBuffer,
        count,
        access: Access::READ.with(Access::WRITE),
        stages: Stages::COMPUTE,
    };
    // Bindings ahead of the ones used, so each starts past a set's first.
    let low_layout = context
        .create_bind_set_layout(&[binding(0, 3), binding(1, 2)])
        .expect("a layout");
    let high_layout = context
        .create_bind_set_layout(&[binding(2, 1), binding(3, 2)])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&low_layout, &high_layout], crate::PushConstants::NONE)
        .expect("a layout");
    let pipeline = context
        .create_compute_pipeline(&crate::ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let memories: alloc::vec::Vec<crate::Allocation> = (0..4)
        .map(|_| crate::device_tests::memory(&context, 64))
        .collect();
    let buffers: alloc::vec::Vec<crate::Buffer> = memories
        .iter()
        .map(|memory| crate::device_tests::over(&context, memory, 0, 64))
        .collect();
    let at = |buffer: usize, offset: u64| crate::Resource::Buffer {
        buffer: &buffers[buffer],
        offset,
        size: 4,
    };
    // The low set reaches memories 0 and 2, the high one 3 and 0 again.
    let filler = [at(1, 0), at(1, 16), at(1, 32)];
    let low = context
        .create_bind_set(
            &low_layout,
            &[
                crate::BindSetEntry {
                    binding: 0,
                    resources: &filler,
                },
                crate::BindSetEntry {
                    binding: 1,
                    resources: &[at(2, 0), at(0, 16)],
                },
            ],
        )
        .expect("a set");
    let high = context
        .create_bind_set(
            &high_layout,
            &[
                crate::BindSetEntry {
                    binding: 2,
                    resources: &[at(1, 48)],
                },
                crate::BindSetEntry {
                    binding: 3,
                    resources: &[at(0, 32), at(3, 48)],
                },
            ],
        )
        .expect("a set");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&low, &high], &[], [1, 1, 1])
        .expect("recorded");
    crate::device_tests::submit(&crate::device_tests::queue(&context), &commands)
        .expect("submitted");
    assert_eq!(context.status(), Ok(()));
    let word = |memory: usize, at: usize| words(&memories[memory], 16)[at];
    assert_eq!(
        (word(0, 4), word(0, 8), word(2, 0), word(3, 12)),
        (2, 3, 1, 4)
    );
    assert_eq!(words(&memories[1], 16), [0; 16], "bound but unused");
    let written: u32 = (0..4)
        .map(|m| words(&memories[m], 16).iter().sum::<u32>())
        .sum();
    assert_eq!(written, 1 + 2 + 3 + 4, "nothing written elsewhere");
}

#[test]
fn a_memory_is_placed_among_merged_ones_from_where_the_last_was() {
    use super::place_from;
    let budget = alloc::sync::Arc::new(crate::memory::Budget::new(u64::MAX, Error::OverLimit));
    let mut held: alloc::vec::Vec<alloc::sync::Arc<crate::memory::Memory>> = (0..100)
        .map(|_| {
            let charge = |held| crate::memory::Charge::take(&[&budget], 4, held).expect("charged");
            alloc::sync::Arc::new(
                crate::memory::Memory::zeroed(
                    4,
                    charge(crate::memory::Held::Allocation),
                    charge(crate::memory::Held::Host),
                )
                .expect("made"),
            )
        })
        .collect();
    held.sort_unstable_by_key(alloc::sync::Arc::as_ptr);
    let stranger = held.pop().expect("a memory");
    let merged: alloc::vec::Vec<&alloc::sync::Arc<crate::memory::Memory>> = held.iter().collect();
    for stride in [1, 2, 7, 33, 98] {
        let mut from = 0;
        for at in (0..merged.len()).step_by(stride) {
            assert_eq!(
                place_from(&merged, from, &held[at]),
                Some(at),
                "stride {stride}"
            );
            from = at + 1;
        }
    }
    assert_eq!(place_from(&merged, 0, &stranger), None, "not merged");
    assert_eq!(
        place_from(&merged, 50, &held[10]),
        None,
        "behind where it looks"
    );
    assert_eq!(place_from(&merged, merged.len(), &held[0]), None);
    assert_eq!(place_from(&merged, merged.len() + 1, &held[0]), None);
}

/// A shader of workgroups of `size` invocations writing, at its invocation's
/// place in the whole dispatch of `groups`, its workgroup's id along each axis
/// and its local index, packed one byte each, after reading a private word it
/// has not written, which must read zero, and so must the word it then
/// writes and reads back match its index.
fn placed(size: u32, groups: [u32; 3], barrier: bool) -> String {
    let [gx, gy, _] = groups;
    let wait = if barrier {
        "OpControlBarrier %two %two %none\n"
    } else {
        ""
    };
    alloc::format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %wid %lix %buf
OpExecutionMode %main LocalSize {size} 1 1
OpDecorate %wid BuiltIn WorkgroupId
OpDecorate %lix BuiltIn LocalInvocationIndex
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v3uint = OpTypeVector %uint 3
%zero = OpConstant %uint 0
%two = OpConstant %uint 2
%none = OpConstant %uint 0
%size = OpConstant %uint {size}
%gx = OpConstant %uint {gx}
%gy = OpConstant %uint {gy}
%c8 = OpConstant %uint 8
%c16 = OpConstant %uint 16
%c24 = OpConstant %uint 24
%big = OpConstant %uint 0x80000000
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%in_v3 = OpTypePointer Input %v3uint
%in_uint = OpTypePointer Input %uint
%fn_uint = OpTypePointer Function %uint
%wid = OpVariable %in_v3 Input
%lix = OpVariable %in_uint Input
%buf = OpVariable %block_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%kept = OpVariable %fn_uint Function
%before = OpLoad %uint %kept
%id = OpLoad %v3uint %wid
%local = OpLoad %uint %lix
OpStore %kept %local
{wait}%after = OpLoad %uint %kept
%x = OpCompositeExtract %uint %id 0
%y = OpCompositeExtract %uint %id 1
%z = OpCompositeExtract %uint %id 2
%row = OpIMul %uint %z %gy
%plane = OpIAdd %uint %row %y
%lines = OpIMul %uint %plane %gx
%linear = OpIAdd %uint %lines %x
%first = OpIMul %uint %linear %size
%at = OpIAdd %uint %first %local
%y8 = OpShiftLeftLogical %uint %y %c8
%z16 = OpShiftLeftLogical %uint %z %c16
%l24 = OpShiftLeftLogical %uint %after %c24
%xy = OpBitwiseOr %uint %x %y8
%xyz = OpBitwiseOr %uint %xy %z16
%all = OpBitwiseOr %uint %xyz %l24
%stale = OpINotEqual %bool %before %zero
%marked = OpBitwiseOr %uint %all %big
%value = OpSelect %uint %stale %marked %all
%dst = OpAccessChain %word_ptr %buf %zero %at
OpStore %dst %value
OpReturn
OpFunctionEnd
"#
    )
    .replace(
        "%uint = OpTypeInt 32 0\n",
        "%uint = OpTypeInt 32 0\n%bool = OpTypeBool\n",
    )
}

/// What [`placed`] writes for every invocation of a dispatch of `groups`.
fn placed_expected(size: u32, groups: [u32; 3]) -> alloc::vec::Vec<u32> {
    let [gx, gy, gz] = groups;
    let mut out = alloc::vec::Vec::new();
    for z in 0..gz {
        for y in 0..gy {
            for x in 0..gx {
                for local in 0..size {
                    out.push(x | y << 8 | z << 16 | (local & 0xff) << 24);
                }
            }
        }
    }
    out
}

#[test]
fn every_invocation_knows_its_workgroup_and_place_however_its_batches_fall() {
    let (device, _) = device();
    let context = context(&device);
    // Packed sixteen to a tile, with a short last batch; a workgroup of a
    // tile's own; one of four tiles run in turn in one tile's memory; and
    // four tiles meeting at a barrier, each in its own.
    for (size, groups, barrier) in [
        (4, [7, 3, 2], false),
        (64, [3, 2, 2], false),
        (256, [2, 2, 1], false),
        (256, [2, 1, 2], true),
    ] {
        let expected = placed_expected(size, groups);
        let bytes = expected.len() as u64 * 4;
        let rig = rig(&context, &placed(size, groups, barrier), bytes, &[]).expect("a pipeline");
        run(&context, &rig, groups).expect("dispatched");
        assert_eq!(
            words(&rig.memory, expected.len()),
            expected,
            "{size} by {groups:?}"
        );
    }
}

#[test]
fn tiles_that_never_wait_on_one_another_share_one_tiles_memory() {
    let (device, _) = device();
    let context = context(&device);
    let pipeline = |barrier| {
        let rig = rig(&context, &placed(1024, [1, 1, 1], barrier), 4096, &[]).expect("a pipeline");
        let model = context.shared.model.lock();
        let live = model.live(context.key).expect("live");
        let state = live.compute_pipelines.get(rig.pipeline.key).expect("filed");
        (state.need, super::Tiling::of(&state.program))
    };
    let (alone, tiling) = pipeline(false);
    assert_eq!((tiling.tiles, tiling.resident), (16, 1));
    let (waiting, tiling) = pipeline(true);
    assert_eq!((tiling.tiles, tiling.resident), (16, 16));
    assert!(
        alone.words * 8 < waiting.words,
        "{alone:?} against {waiting:?}"
    );
    assert!(alone.masks * 8 < waiting.masks);
}

#[test]
fn a_descriptor_reaching_no_memory_has_no_place_among_a_dispatch_s() {
    use super::{Mapping, NO_MEMORY};
    let mut mapping = Mapping {
        places: alloc::vec![4, 0, 2],
        starts: tairix_inline::ArrayVec::new(),
    };
    for start in [None, Some(1)] {
        mapping.starts.try_push(start).expect("room");
    }
    assert_eq!(mapping.merged(0, 2), Some(2), "a whole set's own place");
    assert_eq!(mapping.merged(1, 1), Some(2), "a mapped set's place");
    for set in [0, 1] {
        assert_eq!(mapping.merged(set, NO_MEMORY), None, "a sampler, set {set}");
    }
    assert_eq!(mapping.merged(1, 5), None, "past the set's places");
    assert_eq!(mapping.merged(2, 0), None, "no such set");
}

#[test]
fn a_queued_dispatch_is_charged_for_its_push_constants_and_their_sharing() {
    use super::{Dispatch, Groups};
    let (device, _) = device();
    let context = context(&device);
    let wide = wide(&context).expect("a pipeline over a wide set");
    let (pipeline, set) = {
        let model = context.shared.model.lock();
        let live = model.live(context.key).expect("live");
        (
            alloc::sync::Arc::clone(
                live.compute_pipelines
                    .get(wide.pipeline.key)
                    .expect("filed"),
            ),
            alloc::sync::Arc::clone(live.bind_sets.get(wide.set.key).expect("filed")),
        )
    };
    let words: alloc::vec::Vec<u32> = alloc::vec![1, 2, 3, 4];
    let dispatch = |push| Dispatch {
        pipeline: alloc::sync::Arc::clone(&pipeline),
        sets: alloc::vec![alloc::sync::Arc::clone(&set)],
        push,
        groups: Groups::Direct([1, 1, 1]),
    };
    let none = dispatch(None).held_bytes();
    let some = dispatch(Some(alloc::sync::Arc::new(words))).held_bytes();
    assert_eq!(
        some - none,
        crate::model::shared_bytes::<alloc::vec::Vec<u32>>() + 16,
        "the words and the allocation sharing them"
    );
}

/// A shader storing, through set 0, the red channel of set 1's image sampled
/// through set 1's own sampler.
const SAMPLED_FROM_SET_1: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf %picture %filter
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
OpDecorate %picture DescriptorSet 1
OpDecorate %picture Binding 0
OpDecorate %filter DescriptorSet 1
OpDecorate %filter Binding 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v2float = OpTypeVector %float 2
%v4float = OpTypeVector %float 4
%zero = OpConstant %uint 0
%fzero = OpConstant %float 0
%half = OpConstant %float 0.5
%center = OpConstantComposite %v2float %half %half
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%buf = OpVariable %block_ptr StorageBuffer
%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%image_ptr = OpTypePointer UniformConstant %image_t
%picture = OpVariable %image_ptr UniformConstant
%sampler_t = OpTypeSampler
%sampler_ptr = OpTypePointer UniformConstant %sampler_t
%filter = OpVariable %sampler_ptr UniformConstant
%si_t = OpTypeSampledImage %image_t
%main = OpFunction %void None %fn
%entry = OpLabel
%image = OpLoad %image_t %picture
%sampler = OpLoad %sampler_t %filter
%both = OpSampledImage %si_t %image %sampler
%texel = OpImageSampleExplicitLod %v4float %both %center Lod %fzero
%red = OpCompositeExtract %float %texel 0
%bits = OpBitcast %uint %red
%at = OpAccessChain %word_ptr %buf %zero %zero
OpStore %at %bits
OpReturn
OpFunctionEnd
"#;

/// A 4-by-4 single-float image cleared to 0.75, and a view of all of it.
fn cleared_picture(context: &Context) -> (crate::Allocation, crate::Image, crate::ImageView) {
    let desc = crate::ImageDesc {
        dimension: crate::ImageDimension::D2,
        format: crate::Format::R32Float,
        extent: crate::Extent3d {
            width: 4,
            height: 4,
            depth: 1,
        },
        mip_levels: 1,
        array_layers: 1,
        samples: 1,
        usage: crate::ImageUsage::SAMPLED.with(crate::ImageUsage::TRANSFER_DST),
    };
    let (placed, image) = crate::image_device_tests::image(context, &desc);
    let range = crate::ImageRange {
        aspects: crate::Aspects::COLOR,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    let mut commands = context.command_buffer();
    commands
        .clear_image(
            &image,
            crate::ClearValue::Float([0.75, 0.0, 0.0, 1.0]),
            &[range],
        )
        .expect("recorded");
    crate::device_tests::submit(&crate::device_tests::queue(context), &commands).expect("cleared");
    let texels = context
        .create_image_view(
            &image,
            &crate::ImageViewDesc {
                format: crate::Format::R32Float,
                dimension: crate::ViewDimension::D2,
                range,
            },
        )
        .expect("a view");
    (placed, image, texels)
}

#[test]
fn a_sampler_in_a_set_of_its_own_beside_another_s_memory_is_resolved() {
    use tairix_spirv::{Access, BindingKind, Component, ImageShape, Stages};
    let (device, _) = device();
    let context = context(&device);
    let (_placed, _image, texels) = cleared_picture(&context);
    let filter = context
        .create_sampler(&crate::SamplerDesc::LINEAR)
        .expect("a sampler");
    let binding = |binding, kind| crate::LayoutBinding {
        binding,
        kind,
        count: 1,
        access: Access::READ,
        stages: Stages::COMPUTE,
    };
    let float_2d = ImageShape {
        dim: tairix_spirv::enumerant::Dim::DIM_2D,
        arrayed: false,
        multisampled: false,
        component: Component::Float,
        format: tairix_spirv::enumerant::ImageFormat::UNKNOWN,
    };
    let out_layout = context
        .create_bind_set_layout(&[crate::LayoutBinding {
            access: Access::READ.with(Access::WRITE),
            ..binding(0, BindingKind::StorageBuffer)
        }])
        .expect("a layout");
    let image_layout = context
        .create_bind_set_layout(&[
            binding(0, BindingKind::SampledImage(float_2d)),
            binding(1, BindingKind::Sampler),
        ])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&out_layout, &image_layout], crate::PushConstants::NONE)
        .expect("a layout");
    let module = context
        .create_shader_module(tairix_spirv::assemble::admit(SAMPLED_FROM_SET_1).expect("admitted"))
        .expect("a module");
    let pipeline = context
        .create_compute_pipeline(&crate::ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let held = crate::device_tests::memory(&context, 16);
    let out = crate::device_tests::over(&context, &held, 0, 16);
    let out_set = context
        .create_bind_set(
            &out_layout,
            &[crate::BindSetEntry {
                binding: 0,
                resources: &[crate::Resource::Buffer {
                    buffer: &out,
                    offset: 0,
                    size: 16,
                }],
            }],
        )
        .expect("a set");
    let image_set = context
        .create_bind_set(
            &image_layout,
            &[
                crate::BindSetEntry {
                    binding: 0,
                    resources: &[crate::Resource::Image(&texels)],
                },
                crate::BindSetEntry {
                    binding: 1,
                    resources: &[crate::Resource::Sampler(&filter)],
                },
            ],
        )
        .expect("a set");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&out_set, &image_set], &[], [1, 1, 1])
        .expect("recorded");
    crate::device_tests::submit(&crate::device_tests::queue(&context), &commands).expect("ran");
    assert_eq!(context.status(), Ok(()));
    assert_eq!(words(&held, 1), [0.75_f32.to_bits()]);
}
