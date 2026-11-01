//! Compute pipelines on the software device: made, refused, and dispatched.

use alloc::vec;
use alloc::vec::Vec;

use tairix_spirv::assemble::admit;
use tairix_spirv::{Access, BindingKind, Disagreement, SpecValue, Stages};

use crate::device_tests::{context, device, memory, over, queue, submit};
use crate::{
    BindSetEntry, ComputePipeline, ComputePipelineDesc, Context, Error, LayoutBinding,
    PipelineLayout, PushConstants, Resource,
};

/// A storage buffer at set 0, binding 0, read and written by compute.
pub(crate) const fn storage(binding: u32) -> LayoutBinding {
    LayoutBinding {
        binding,
        kind: BindingKind::StorageBuffer,
        count: 1,
        access: Access::READ.with(Access::WRITE),
        stages: Stages::COMPUTE,
    }
}

/// A compute shader writing each invocation's global index times two into
/// a storage buffer of words, its workgroups of `size`.
pub(crate) fn doubles(size: u32) -> alloc::string::String {
    alloc::format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %buf
OpExecutionMode %main LocalSize {size} 1 1
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
%two = OpConstant %uint 2
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%x = OpCompositeExtract %uint %id 0
%doubled = OpIMul %uint %x %two
%at = OpAccessChain %word_ptr %buf %zero %x
OpStore %at %doubled
OpReturn
OpFunctionEnd
"#
    )
}

/// A pipeline of `text`'s entry point `main` over one storage buffer, its
/// layout, and the bind set binding `bytes` of a fresh buffer.
pub(crate) struct Rig {
    pub(crate) pipeline: ComputePipeline,
    pub(crate) set: crate::BindSet,
    pub(crate) memory: crate::Allocation,
    _buffer: crate::Buffer,
    _layout: PipelineLayout,
    _set_layout: crate::BindSetLayout,
}

pub(crate) fn rig(
    context: &Context,
    text: &str,
    bytes: u64,
    specialization: &[SpecValue],
) -> Result<Rig, Error> {
    let module = context.create_shader_module(admit(text).expect("admitted"))?;
    let set_layout = context.create_bind_set_layout(&[storage(0)])?;
    let layout = context.create_pipeline_layout(&[&set_layout], PushConstants::NONE)?;
    let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
        module: &module,
        entry_point: "main",
        layout: &layout,
        specialization,
    })?;
    let memory = memory(context, bytes);
    let buffer = over(context, &memory, 0, bytes);
    let set = context.create_bind_set(
        &set_layout,
        &[BindSetEntry {
            binding: 0,
            resources: &[Resource::Buffer {
                buffer: &buffer,
                offset: 0,
                size: bytes,
            }],
        }],
    )?;
    Ok(Rig {
        pipeline,
        set,
        memory,
        _buffer: buffer,
        _layout: layout,
        _set_layout: set_layout,
    })
}

/// The words of `allocation`'s first `count`.
pub(crate) fn words(allocation: &crate::Allocation, count: usize) -> Vec<u32> {
    let mut bytes = vec![0u8; count * 4];
    allocation.read(0, &mut bytes).expect("readable");
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

/// Dispatch `groups` of the rig's pipeline and wait for it.
pub(crate) fn run(context: &Context, rig: &Rig, groups: [u32; 3]) -> Result<(), Error> {
    let mut commands = context.command_buffer();
    commands.dispatch(&rig.pipeline, &[&rig.set], &[], groups)?;
    submit(&queue(context), &commands)
        .map(|_| ())
        .map_err(|e| e.error)
}

#[test]
fn a_dispatch_runs_every_invocation_of_every_workgroup() {
    let (device, _) = device();
    let context = context(&device);
    for size in [1, 7, 32, 64, 100, 256] {
        let groups = 3;
        let count = size * groups;
        let rig = rig(&context, &doubles(size), u64::from(count) * 4, &[]).expect("a pipeline");
        run(&context, &rig, [groups, 1, 1]).expect("dispatched");
        let expected: Vec<u32> = (0..count).map(|x| x * 2).collect();
        assert_eq!(
            words(&rig.memory, count as usize),
            expected,
            "workgroups of {size}"
        );
    }
}

#[test]
fn a_pipeline_refuses_an_entry_point_or_layout_it_does_not_have() {
    let (device, _) = device();
    let context = context(&device);
    let module = context
        .create_shader_module(admit(&doubles(4)).expect("admitted"))
        .expect("a module");
    let set_layout = context
        .create_bind_set_layout(&[storage(0)])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
        .expect("a pipeline layout");
    let desc = |entry_point, layout| ComputePipelineDesc {
        module: &module,
        entry_point,
        layout,
        specialization: &[],
    };
    assert_eq!(
        context
            .create_compute_pipeline(&desc("other", &layout))
            .err(),
        Some(Error::EntryPoint)
    );
    let empty = context
        .create_pipeline_layout(&[], PushConstants::NONE)
        .expect("an empty layout");
    assert_eq!(
        context.create_compute_pipeline(&desc("main", &empty)).err(),
        Some(Error::Disagreement(Disagreement::Missing(0, 0)))
    );
    context
        .create_compute_pipeline(&desc("main", &layout))
        .expect("the entry point and its layout");
}

/// A shader of workgroups `x` by `y` by `z` storing each invocation's index
/// through a private array of `private` words and a workgroup array of
/// `shared`, neither empty.
fn sized([x, y, z]: [u32; 3], private: u32, shared: u32) -> alloc::string::String {
    alloc::format!(
        r#"
OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %lid %buf %private %shared
OpExecutionMode %main LocalSize {x} {y} {z}
OpDecorate %lid BuiltIn LocalInvocationIndex
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%private_len = OpConstant %uint {private}
%shared_len = OpConstant %uint {shared}
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%lid_ptr = OpTypePointer Input %uint
%lid = OpVariable %lid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
%private_t = OpTypeArray %uint %private_len
%private_ptr = OpTypePointer Private %private_t
%private_word = OpTypePointer Private %uint
%private = OpVariable %private_ptr Private
%shared_t = OpTypeArray %uint %shared_len
%shared_ptr = OpTypePointer Workgroup %shared_t
%shared_word = OpTypePointer Workgroup %uint
%shared = OpVariable %shared_ptr Workgroup
%main = OpFunction %void None %fn
%entry = OpLabel
%l = OpLoad %uint %lid
%pp = OpAccessChain %private_word %private %zero
OpStore %pp %l
%held = OpLoad %uint %pp
%sp = OpAccessChain %shared_word %shared %zero
OpStore %sp %held
%at = OpAccessChain %word_ptr %buf %zero %l
OpStore %at %held
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn a_pipeline_refuses_a_workgroup_or_a_working_set_past_the_device_s_limits() {
    let (device, _) = device();
    let context = context(&device);
    let fits =
        |size, private, shared| rig(&context, &sized(size, private, shared), 4096, &[]).map(|_| ());
    fits([32, 32, 1], 4, 4).expect("a workgroup of the most invocations");
    fits([64, 1, 1], 4000, 8000).expect("the most working memory, near enough");
    for size in [[1025, 1, 1], [1, 1, 65], [64, 64, 1]] {
        assert_eq!(
            fits(size, 4, 4),
            Err(Error::TooLarge),
            "workgroups of {size:?}"
        );
    }
    assert_eq!(
        fits([64, 1, 1], 5000, 4),
        Err(Error::TooLarge),
        "20 KiB of an invocation's own"
    );
    assert_eq!(
        fits([64, 1, 1], 4, 9000),
        Err(Error::TooLarge),
        "36 KiB of a workgroup's"
    );
}

#[test]
fn working_memory_a_dropped_pipeline_held_goes_back_before_the_heap_refuses() {
    // A heap of 2 MiB; a pipeline of 64 invocations each of 3000 private
    // words holds 750 KiB of it while it lives.
    let (device, _) = device();
    // A limit past the heap's, so the heap is what refuses.
    let context = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(u64::MAX),
        })
        .expect("a context");
    let large = 3 << 19;
    let big = || rig(&context, &sized([64, 1, 1], 3000, 4), 256, &[]).expect("a pipeline");
    let small = rig(&context, &sized([4, 1, 1], 4, 4), 256, &[]).expect("a pipeline");
    let allocate = |bytes| {
        context.allocate(&crate::AllocationDesc {
            size: bytes,
            memory_type: 0,
        })
    };

    let held = big();
    assert_eq!(allocate(large).err(), Some(Error::HeapFull));
    drop(held);
    drop(allocate(large).expect("the pool fitted to the small pipeline"));

    // Work waiting to run keeps the pipeline alive past its handle.
    let held = big();
    let gate = context.create_timeline(0).expect("a timeline");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&held.pipeline, &[&held.set], &[], [1, 1, 1])
        .expect("recorded");
    let fence = queue(&context)
        .submit(&crate::Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: tairix_abi::Duration64::from_secs(1),
        })
        .expect("admitted");
    drop((held, commands));
    assert_eq!(
        allocate(large).err(),
        Some(Error::HeapFull),
        "the waiting work still needs it"
    );
    gate.signal(1).expect("signalled");
    fence
        .wait(tairix_abi::Duration64::from_secs(1))
        .expect("the work ran");
    drop(allocate(large).expect("pruned when the heap would refuse"));
    run(&context, &small, [1, 1, 1]).expect("the small pipeline still has room to run");
    assert_eq!(words(&small.memory, 4), [0, 1, 2, 3]);
}

#[test]
fn a_pipeline_s_working_memory_goes_back_as_the_last_work_holding_it_completes() {
    let (device, _) = device();
    let context = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(u64::MAX),
        })
        .expect("a context");
    let _small = rig(&context, &sized([4, 1, 1], 4, 4), 256, &[]).expect("a pipeline");
    let held = rig(&context, &sized([64, 1, 1], 3000, 4), 256, &[]).expect("a pipeline");
    let heap = || context.shared.model.lock().work_heap_taken();
    let gate = context.create_timeline(0).expect("a timeline");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&held.pipeline, &[&held.set], &[], [1, 1, 1])
        .expect("recorded");
    let fence = queue(&context)
        .submit(&crate::Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: tairix_abi::Duration64::from_secs(1),
        })
        .expect("admitted");
    drop((held, commands));
    let waiting = heap();
    gate.signal(1).expect("signalled");
    fence
        .wait(tairix_abi::Duration64::from_secs(1))
        .expect("the work ran");
    assert!(
        heap() + (512 << 10) < waiting,
        "the large workspace went back with the work: {} of {waiting}",
        heap()
    );
}

#[test]
fn a_pipeline_past_both_its_context_s_limit_and_the_heap_is_refused_for_the_limit() {
    let (device, _) = device();
    let made = |context: &Context| {
        let module = context
            .create_shader_module(admit(&doubles(64)).expect("admitted"))
            .expect("a module");
        let set_layout = context
            .create_bind_set_layout(&[storage(0)])
            .expect("a set layout");
        let layout = context
            .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
            .expect("a layout");
        (module, set_layout, layout)
    };
    let create =
        |context: &Context, held: &(crate::ShaderModule, crate::BindSetLayout, PipelineLayout)| {
            context.create_compute_pipeline(&ComputePipelineDesc {
                module: &held.0,
                entry_point: "main",
                layout: &held.2,
                specialization: &[],
            })
        };
    let (short, held) = crate::device_tests::short_of(&device, made, |context, held| {
        create(context, held).expect("a pipeline")
    });
    let _full = crate::device_tests::fill_heap(&device);
    assert_eq!(
        create(&short, &held).err(),
        Some(Error::OverLimit),
        "though its claim alone is within the limit and its working memory past the heap"
    );
}

#[test]
fn a_pipeline_is_charged_all_it_keeps_before_its_working_memory_is_made() {
    let (device, _) = device();
    let context = context(&device);
    let module = context
        .create_shader_module(admit(&doubles(64)).expect("admitted"))
        .expect("a module");
    let set_layout = context
        .create_bind_set_layout(&[storage(0)])
        .expect("a set layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
        .expect("a layout");
    let desc = ComputePipelineDesc {
        module: &module,
        entry_point: "main",
        layout: &layout,
        specialization: &[],
    };
    let (admitted, state) = {
        let model = context.shared.model.lock();
        let live = model.live(context.key).expect("live");
        let admitted = live.shader_modules.get(module.key).cloned();
        let state = live.pipeline_layouts.get(layout.key).cloned();
        (admitted.expect("the module"), state.expect("the layout"))
    };
    let program = super::lower_compute(&device.info().limits, &admitted.admitted, &desc, &state)
        .expect("lowered");
    let (planned, released) = context.shared.model.lock().plan_pipeline(
        context.key,
        crate::software::dispatch::need(&program),
        super::held_bytes(&program),
    );
    drop(released);
    let (plan, charge) = planned.expect("planned");
    let grown = plan.make().expect("made");
    // The heap filled between the two phases, as another context's work may.
    let _full = crate::device_tests::fill_heap(&device);
    let (filed, released) = context.shared.model.lock().create_compute_pipeline(
        context.key,
        (program, state),
        (grown, charge),
    );
    drop(released);
    assert!(
        filed.is_ok(),
        "nothing left to charge once its working memory is made"
    );
}

#[test]
fn a_released_pipeline_hands_out_its_program_only_as_its_last_holder() {
    let (device, _) = device();
    let context = context(&device);
    let objects = || context.memory_used().objects;
    let before = crate::fair::held::drops_under();
    let idle = rig(&context, &sized([4, 1, 1], 4, 4), 256, &[]).expect("a pipeline");
    let released = context
        .shared
        .model
        .lock()
        .destroy_compute_pipeline(idle.pipeline.context, idle.pipeline.key);
    let handed = objects();
    drop(released);
    assert!(objects() < handed, "its program handed out with its charge");
    assert_eq!(
        crate::fair::held::drops_under(),
        before,
        "and let go without the device's lock"
    );

    let used = rig(&context, &sized([4, 1, 1], 4, 4), 256, &[]).expect("a pipeline");
    let gate = context.create_timeline(0).expect("a timeline");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&used.pipeline, &[&used.set], &[], [1, 1, 1])
        .expect("recorded");
    let fence = queue(&context)
        .submit(&crate::Submission {
            command_buffers: &[&commands],
            waits: &[(&gate, 1)],
            signals: &[],
            deadline: tairix_abi::Duration64::from_secs(1),
        })
        .expect("admitted");
    let released = context
        .shared
        .model
        .lock()
        .destroy_compute_pipeline(used.pipeline.context, used.pipeline.key);
    let handed = objects();
    drop(released);
    assert_eq!(
        objects(),
        handed,
        "nothing handed out: the queued work holds it"
    );
    gate.signal(1).expect("signalled");
    fence
        .wait(tairix_abi::Duration64::from_secs(1))
        .expect("the work ran");
    assert_eq!(words(&used.memory, 4), [0, 1, 2, 3]);
    assert_eq!(
        crate::fair::held::drops_under(),
        before,
        "the work that held it last let it go without the device's lock"
    );
}

#[test]
fn constants_past_what_specialization_holds_refuse_the_module_not_its_values() {
    use core::fmt::Write as _;
    let (device, _) = device();
    let context = context(&device);
    let mut decls =
        alloc::string::String::from("%len = OpConstant %uint 2097152\n%one = OpConstant %uint 1\n");
    let mut element = alloc::string::String::from("%uint");
    for level in 1..=20 {
        let _ = writeln!(decls, "%a{level} = OpTypeArray {element} %len");
        element = alloc::format!("%a{level}");
    }
    let _ = writeln!(
        decls,
        "%null = OpConstantNull %a20\n%deep = OpSpecConstantOp %a20 CompositeInsert %one %null{}",
        " 0".repeat(20)
    );
    let text = alloc::format!(
        "OpCapability Shader\nOpMemoryModel Logical GLSL450\nOpEntryPoint GLCompute %main \"main\"\nOpExecutionMode %main LocalSize 1 1 1\n%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%uint = OpTypeInt 32 0\n{decls}%main = OpFunction %void None %fn\n%entry = OpLabel\nOpReturn\nOpFunctionEnd\n"
    );
    assert_eq!(
        rig(&context, &text, 256, &[]).err(),
        Some(Error::TooLarge),
        "no value was given, so none is refused"
    );
}

#[test]
fn a_refused_pipeline_leaves_the_working_memory_as_it_was() {
    let (device, _) = device();
    let text = sized([64, 1, 1], 3000, 4);
    // What the pipeline is made from, and the pipeline from it, all kept.
    let made = |context: &crate::Context| {
        let module = context.create_shader_module(admit(&text).expect("admitted"))?;
        let set_layout = context.create_bind_set_layout(&[storage(0)])?;
        let layout = context.create_pipeline_layout(&[&set_layout], PushConstants::NONE)?;
        let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        });
        Ok::<_, Error>((module, set_layout, layout, pipeline))
    };
    let total = {
        let probe = context(&device);
        let held = made(&probe).expect("made");
        assert!(held.3.is_ok(), "a pipeline");
        probe.memory_used().total()
    };
    // Everything the pipeline and what it is made from take, but a byte.
    let tight = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(total - 1),
        })
        .expect("a context");
    let heap = || tight.shared.model.lock().work_heap_taken();
    let before = heap();
    let held = made(&tight).expect("what it is made from fits");
    assert_eq!(held.3.err(), Some(Error::OverLimit));
    assert!(
        heap() - before < 64 << 10,
        "no workspace was made for the refused pipeline: {} bytes kept",
        heap() - before
    );
}

#[test]
fn the_pool_shrinks_in_place_so_a_full_heap_still_gets_memory_back() {
    // Two participants: the large pipeline's two workspaces take 1.5 MiB of
    // the 2 MiB heap, which a medium one's two beside them would pass.
    let clock: alloc::sync::Arc<dyn tairix_abi::time::MonotonicClock + Send + Sync> =
        alloc::sync::Arc::new(crate::device_tests::Clock::default());
    let device = crate::device_tests::software_on(
        crate::device_tests::RAM,
        clock,
        alloc::sync::Arc::new(tairix_parallel::Reversed::new(2)),
    );
    // A limit past the heap's, so the heap is what refuses.
    let context = device
        .create_context(&crate::ContextDesc {
            memory_limit: Some(u64::MAX),
        })
        .expect("a context");
    let large = rig(&context, &sized([64, 1, 1], 3000, 4), 256, &[]).expect("a pipeline");
    let medium = rig(&context, &sized([64, 1, 1], 1500, 4), 512, &[]).expect("a pipeline");
    drop(large);
    let mebibyte = context.allocate(&crate::AllocationDesc {
        size: 1 << 20,
        memory_type: 0,
    });
    assert!(mebibyte.is_ok(), "both workspaces shrunk in place");
    run(&context, &medium, [2, 1, 1]).expect("dispatched");
    let expected: Vec<u32> = (0..64).collect();
    assert_eq!(
        words(&medium.memory, 64),
        expected,
        "each workgroup stores at its local indexes"
    );
}

/// A shader storing its two push-constant words into its buffer.
const PUSHED: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf %pc
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
OpDecorate %pcs Block
OpMemberDecorate %pcs 0 Offset 0
OpMemberDecorate %pcs 1 Offset 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%sb_block = OpTypePointer StorageBuffer %block
%buf = OpVariable %sb_block StorageBuffer
%sb_uint = OpTypePointer StorageBuffer %uint
%pcs = OpTypeStruct %uint %uint
%pc_ptr = OpTypePointer PushConstant %pcs
%pc = OpVariable %pc_ptr PushConstant
%pc_uint = OpTypePointer PushConstant %uint
%main = OpFunction %void None %fn
%entry = OpLabel
%p0 = OpAccessChain %pc_uint %pc %zero
%v0 = OpLoad %uint %p0
%p1 = OpAccessChain %pc_uint %pc %one
%v1 = OpLoad %uint %p1
%d0 = OpAccessChain %sb_uint %buf %zero %zero
OpStore %d0 %v0
%d1 = OpAccessChain %sb_uint %buf %zero %one
OpStore %d1 %v1
OpReturn
OpFunctionEnd
"#;

#[test]
fn push_constants_reach_the_shader_each_submission_and_only_the_layouts_length_is_taken() {
    let (device, _) = device();
    let context = context(&device);
    let module = context
        .create_shader_module(admit(PUSHED).expect("admitted"))
        .expect("a module");
    let set_layout = context
        .create_bind_set_layout(&[storage(0)])
        .expect("a layout");
    let layout = context
        .create_pipeline_layout(
            &[&set_layout],
            PushConstants {
                bytes: 8,
                stages: tairix_spirv::Stages::COMPUTE,
            },
        )
        .expect("a layout");
    let pipeline = context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let held = memory(&context, 16);
    let buffer = over(&context, &held, 0, 16);
    let set = context
        .create_bind_set(
            &set_layout,
            &[BindSetEntry {
                binding: 0,
                resources: &[Resource::Buffer {
                    buffer: &buffer,
                    offset: 0,
                    size: 16,
                }],
            }],
        )
        .expect("a set");
    let pushed = |bytes: &[u8]| {
        let mut commands = context.command_buffer();
        commands
            .dispatch(&pipeline, &[&set], bytes, [1, 1, 1])
            .expect("recorded");
        commands
    };
    let commands = pushed(&[7, 0, 0, 0, 0x78, 0x56, 0x34, 0x12]);
    for _ in 0..2 {
        submit(&queue(&context), &commands).expect("submitted");
        assert_eq!(words(&held, 2), [7, 0x1234_5678]);
    }
    for bytes in [&[1u8, 2, 3, 4, 5, 6][..], &[1, 2, 3, 4], &[], &[0; 12]] {
        assert_eq!(
            submit(&queue(&context), &pushed(bytes))
                .err()
                .map(|e| e.error),
            Some(Error::Shape),
            "{} bytes",
            bytes.len()
        );
    }
    assert_eq!(context.status(), Ok(()));
}

/// A shader running a loop of four rounds, its header's `phis` beside the
/// round's count and its body `round`, then storing each of `results`, words
/// the loop's merge block reads, at its index.
fn looped(phis: &str, round: &str, results: &[&str]) -> alloc::string::String {
    use core::fmt::Write as _;
    let (mut indices, mut writes) = (alloc::string::String::new(), alloc::string::String::new());
    for (at, result) in results.iter().enumerate() {
        let _ = writeln!(indices, "%c{at} = OpConstant %uint {at}");
        let _ = writeln!(
            writes,
            "%d{at} = OpAccessChain %word_ptr %buf %zero %c{at}\nOpStore %d{at} {result}"
        );
    }
    alloc::format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%v2uint = OpTypeVector %uint 2
%v4uint = OpTypeVector %uint 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%four = OpConstant %uint 4
%pair = OpConstantComposite %v2uint %one %one
%base = OpConstantComposite %v4uint %one %two %four %zero
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%buf = OpVariable %block_ptr StorageBuffer
{indices}%main = OpFunction %void None %fn
%entry = OpLabel
OpBranch %header
%header = OpLabel
{phis}%i = OpPhi %uint %zero %entry %next %continue
%more = OpULessThan %bool %i %four
OpLoopMerge %merge %continue None
OpBranchConditional %more %body %merge
%body = OpLabel
{round}OpBranch %continue
%continue = OpLabel
%next = OpIAdd %uint %i %one
OpBranch %header
%merge = OpLabel
{writes}OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn a_phi_reading_a_part_of_another_phi_on_the_same_edge_reads_it_before_it_changes() {
    let (device, _) = device();
    let context = context(&device);
    // The older Fibonacci number is carried in a phi of its own, read as a
    // part of the pair the same edge replaces; the phi reading it comes first,
    // so its move would run last were it not ordered before the pair's.
    let text = looped(
        "%s = OpPhi %uint %zero %entry %e %continue\n%v = OpPhi %v2uint %pair %entry %w %continue\n",
        "%e = OpCompositeExtract %uint %v 1\n%x = OpCompositeExtract %uint %v 0\n%sum = OpIAdd %uint %x %e\n%w = OpCompositeConstruct %v2uint %e %sum\n",
        &["%s", "%vx", "%vy"],
    )
    .replace(
        "%merge = OpLabel\n",
        "%merge = OpLabel\n%vx = OpCompositeExtract %uint %v 0\n%vy = OpCompositeExtract %uint %v 1\n",
    );
    let rig = rig(&context, &text, 16, &[]).expect("a pipeline");
    run(&context, &rig, [1, 1, 1]).expect("dispatched");
    assert_eq!(words(&rig.memory, 3), [5, 5, 8]);
}

#[test]
fn values_an_insertion_is_made_from_keep_their_words_while_they_are_read() {
    let (device, _) = device();
    let context = context(&device);
    // Each round inserts into a value from before the loop, which every round
    // reads again, and into its own last value, which it reads once more
    // after the next insertion.
    let text = looped(
        "%acc = OpPhi %uint %zero %entry %sum %continue\n",
        "%a = OpCompositeInsert %v4uint %i %base 0\n%b = OpCompositeInsert %v4uint %i %a 3\n%c = OpCompositeInsert %v4uint %four %b 1\n%ax = OpCompositeExtract %uint %a 3\n%bx = OpCompositeExtract %uint %b 1\n%cx = OpCompositeExtract %uint %c 1\n%cw = OpCompositeExtract %uint %c 3\n%p = OpIAdd %uint %ax %bx\n%q = OpIAdd %uint %cx %cw\n%r = OpIAdd %uint %p %q\n%sum = OpIAdd %uint %acc %r\n",
        &["%acc"],
    );
    let rig = rig(&context, &text, 16, &[]).expect("a pipeline");
    run(&context, &rig, [1, 1, 1]).expect("dispatched");
    // a = (i, 2, 4, 0), b = (i, 2, 4, i), c = (i, 4, 4, i): each round adds
    // 0 + 2 + 4 + i.
    let expected: u32 = (0..4).map(|i| 6 + i).sum();
    assert_eq!(words(&rig.memory, 1), [expected]);
}

/// Each of 64 lanes runs `2 + (lane & 3)` rounds, inserting the round into a
/// varying pair made before the loop, its first component in even rounds and
/// its second in odd ones, and adds the result's two components up.
const INSERTED_EACH_ROUND: &str = r#"OpCapability Shader
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
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%v2uint = OpTypeVector %uint 2
%v3uint = OpTypeVector %uint 3
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%three = OpConstant %uint 3
%ten = OpConstant %uint 10
%thousand = OpConstant %uint 1000
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%buf = OpVariable %block_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%id = OpLoad %v3uint %gid
%x = OpCompositeExtract %uint %id 0
%low = OpBitwiseAnd %uint %x %three
%rounds = OpIAdd %uint %low %two
%vx = OpIMul %uint %x %ten
%vy = OpIAdd %uint %x %thousand
%pair = OpCompositeConstruct %v2uint %vx %vy
OpBranch %header
%header = OpLabel
%i = OpPhi %uint %zero %entry %next %continue
%acc = OpPhi %uint %zero %entry %sum %continue
%more = OpULessThan %bool %i %rounds
OpLoopMerge %merge %continue None
OpBranchConditional %more %body %merge
%body = OpLabel
%parity = OpBitwiseAnd %uint %i %one
%odd = OpINotEqual %bool %parity %zero
OpSelectionMerge %joined None
OpBranchConditional %odd %odd_side %even_side
%even_side = OpLabel
%even = OpCompositeInsert %v2uint %i %pair 0
OpBranch %joined
%odd_side = OpLabel
%odd_v = OpCompositeInsert %v2uint %i %pair 1
OpBranch %joined
%joined = OpLabel
%changed = OpPhi %v2uint %even %even_side %odd_v %odd_side
%cx = OpCompositeExtract %uint %changed 0
%cy = OpCompositeExtract %uint %changed 1
%both = OpIAdd %uint %cx %cy
%sum = OpIAdd %uint %acc %both
OpBranch %continue
%continue = OpLabel
%next = OpIAdd %uint %i %one
OpBranch %header
%merge = OpLabel
%at = OpAccessChain %word_ptr %buf %zero %x
OpStore %at %acc
OpReturn
OpFunctionEnd
"#;

#[test]
fn a_value_from_before_a_loop_keeps_its_words_through_insertions_into_it_each_round() {
    let (device, _) = device();
    let context = context(&device);
    let rig = rig(&context, INSERTED_EACH_ROUND, 64 * 4, &[]).expect("a pipeline");
    run(&context, &rig, [1, 1, 1]).expect("dispatched");
    let expected: Vec<u32> = (0..64u32)
        .map(|lane| {
            let (vx, vy) = (lane * 10, lane + 1000);
            (0..2 + (lane & 3))
                .map(|round| {
                    if round % 2 == 1 {
                        vx + round
                    } else {
                        round + vy
                    }
                })
                .sum()
        })
        .collect();
    assert_eq!(words(&rig.memory, 64), expected);
}
