//! Memory from a shader: workgroup memory shared across a workgroup's two
//! tiles through a barrier, every atomic applied in lane order to a buffer's
//! word and to a workgroup's, function and private variables, copies between
//! them, a runtime array's length, push constants and uniform buffers, and
//! accesses past every object reading zero and writing nothing.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_spirv::assemble::admit;
use tairix_spirv::{Access, BindingKind, Stages};

use super::{Case, Ty};
use crate::device_tests::{context, device, memory, over, queue, submit};
use crate::{BindSetEntry, ComputePipelineDesc, LayoutBinding, PushConstants, Resource};

const DEVICE: &str = "%device = OpConstant %uint 1\n%workgroup_scope = OpConstant %uint 2\n%sem = OpConstant %uint 72\n%store_sem = OpConstant %uint 68\n%load_sem = OpConstant %uint 66\n%wg_sem = OpConstant %uint 264";

/// An atomic applied by each lane in turn to one word: each lane's value is
/// `value(lane)`, and the case's body reads it as `%a`.
struct Step {
    body: &'static str,
    start: u32,
    apply: fn(u32, u32, u32) -> u32,
    value: fn(u32) -> u32,
}

const ATOMICS: [Step; 13] = [
    Step {
        body: "OpAtomicIAdd %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w.wrapping_add(v),
        value: |l| l + 1,
    },
    Step {
        body: "OpAtomicISub %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w.wrapping_sub(v),
        value: |l| l,
    },
    Step {
        body: "OpAtomicIIncrement %uint %sp %device %sem",
        start: 0,
        apply: |w, _, _| w.wrapping_add(1),
        value: |_| 0,
    },
    Step {
        body: "OpAtomicIDecrement %uint %sp %device %sem",
        start: 0,
        apply: |w, _, _| w.wrapping_sub(1),
        value: |_| 0,
    },
    Step {
        body: "OpAtomicExchange %uint %sp %device %sem %a",
        start: 0,
        apply: |_, v, _| v,
        value: |l| l * 7,
    },
    Step {
        body: "OpAtomicUMin %uint %sp %device %sem %a",
        start: u32::MAX,
        apply: |w, v, _| w.min(v),
        value: |l| (l * 37) % 64,
    },
    Step {
        body: "OpAtomicUMax %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w.max(v),
        value: |l| (l * 37) % 64,
    },
    Step {
        body: "OpAtomicSMin %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w.cast_signed().min(v.cast_signed()).cast_unsigned(),
        value: |l| l.wrapping_mul(0x9e37_79b9),
    },
    Step {
        body: "OpAtomicSMax %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w.cast_signed().max(v.cast_signed()).cast_unsigned(),
        value: |l| l.wrapping_mul(0x9e37_79b9),
    },
    Step {
        body: "OpAtomicAnd %uint %sp %device %sem %a",
        start: u32::MAX,
        apply: |w, v, _| w & v,
        value: |l| !(1 << (l % 32)),
    },
    Step {
        body: "OpAtomicOr %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w | v,
        value: |l| 1 << (l % 32),
    },
    Step {
        body: "OpAtomicXor %uint %sp %device %sem %a",
        start: 0,
        apply: |w, v, _| w ^ v,
        value: |l| l * 3,
    },
    Step {
        body: "OpAtomicCompareExchange %uint %sp %device %sem %load_sem %a %lane",
        start: 0,
        apply: |w, v, lane| if w == lane { v } else { w },
        value: |l| l + 1,
    },
];

/// The case running `step` on a word of `root` by every one of `lanes`.
fn atomic(step: &Step, root: &'static str, lanes: u32) -> Case {
    let (pointer, decls, scope) = if root == "buffer" {
        (
            "%sp = OpAccessChain %word_ptr %in %zero %scratch_at",
            DEVICE,
            "%device",
        )
    } else {
        (
            "%sp = OpAccessChain %wg_word %shared %zero",
            WORKGROUP_WORD,
            "%workgroup_scope",
        )
    };
    // Every lane stores the starting value first, all before any applies
    // the operation.
    let body = alloc::format!(
        "{pointer}\nOpAtomicStore %sp {scope} %store_sem %start\n%r = {}",
        step.body.replace("%device", scope)
    );
    let decls = alloc::format!("{decls}\n%start = OpConstant %uint {}", step.start);
    let mut word = step.start;
    let mut case = Case::new(&[Ty::U32], Ty::U32, &body).decls(&decls);
    if root != "buffer" {
        case = case.interface("%shared");
    }
    for lane in 0..lanes {
        let value = (step.value)(lane);
        let old = word;
        word = (step.apply)(word, value, lane);
        case = case.lane(&[value], &[old]);
    }
    case
}

const WORKGROUP_WORD: &str = "%device = OpConstant %uint 1\n%workgroup_scope = OpConstant %uint 2\n%sem = OpConstant %uint 264\n%store_sem = OpConstant %uint 260\n%load_sem = OpConstant %uint 258\n%wg_sem = OpConstant %uint 264\n%wg_one = OpTypeArray %uint %one\n%wg_one_ptr = OpTypePointer Workgroup %wg_one\n%wg_word = OpTypePointer Workgroup %uint\n%shared = OpVariable %wg_one_ptr Workgroup";

/// A copy through two of a function's variables, the one never written
/// reading zero.
const COPY: &str = r"%four = OpConstant %uint 4
%arr4 = OpTypeArray %uint %four
%arr_fn = OpTypePointer Function %arr4
%word_fn = OpTypePointer Function %uint
%copy_fn = OpTypeFunction %uint %uint
%copy = OpFunction %uint None %copy_fn
%x = OpFunctionParameter %uint
%copy_entry = OpLabel
%src = OpVariable %arr_fn Function
%dst = OpVariable %arr_fn Function
%s1 = OpInBoundsAccessChain %word_fn %src %one
OpStore %s1 %x
OpCopyMemory %dst %src
%d1 = OpAccessChain %word_fn %dst %one
%v = OpLoad %uint %d1
%d0 = OpAccessChain %word_fn %dst %zero
%z = OpLoad %uint %d0
%sum = OpIAdd %uint %v %z
OpReturnValue %sum
OpFunctionEnd";

pub(super) fn cases() -> Vec<Case> {
    let mut cases: Vec<Case> = ATOMICS
        .iter()
        .flat_map(|step| [atomic(step, "buffer", 64), atomic(step, "workgroup", 64)])
        .collect();
    let shared = (0..128u32).fold(
        Case::new(
            &[Ty::U32],
            Ty::U32,
            "%slot = OpAccessChain %wg_word %shared %lane\nOpStore %slot %a\nOpControlBarrier %workgroup_scope %workgroup_scope %wg_sem\n%next = OpIAdd %uint %lane %one\n%wrapped = OpUMod %uint %next %c128\n%theirs = OpAccessChain %wg_word %shared %wrapped\n%r = OpLoad %uint %theirs",
        )
        .decls("%workgroup_scope = OpConstant %uint 2\n%wg_sem = OpConstant %uint 264\n%c128 = OpConstant %uint 128\n%wg_array = OpTypeArray %uint %c128\n%wg_ptr = OpTypePointer Workgroup %wg_array\n%wg_word = OpTypePointer Workgroup %uint\n%shared = OpVariable %wg_ptr Workgroup")
        .interface("%shared"),
        |c, lane| c.lane(&[lane * 3 + 1], &[((lane + 1) % 128) * 3 + 1]),
    );
    cases.push(shared);
    cases.extend([
        // Every lane stores, then every lane loads what the last one stored.
        (0..8u32).fold(
            Case::new(&[Ty::U32], Ty::U32, "%sp = OpAccessChain %word_ptr %in %zero %scratch_at\nOpAtomicStore %sp %device %store_sem %a\nOpMemoryBarrier %device %sem\n%r = OpAtomicLoad %uint %sp %device %load_sem").decls(DEVICE),
            |c, lane| c.lane(&[lane + 10], &[17]),
        ),
        Case::new(&[Ty::U32], Ty::U32, "%r = OpFunctionCall %uint %copy %a").decls(COPY).lane(&[42], &[42]),
        Case::new(&[Ty::U32], Ty::U32, "%held = OpLoad %uint %pv\n%r = OpIAdd %uint %held %a")
            .decls("%priv_ptr = OpTypePointer Private %uint\n%seven = OpConstant %uint 7\n%pv = OpVariable %priv_ptr Private %seven")
            .interface("%pv")
            .lane(&[5], &[12]),
        Case::new(&[Ty::U32], Ty::U32, "%r = OpArrayLength %uint %in 0").lane(&[0], &[1 + u32::try_from(super::SCRATCH_WORDS).expect("a few")]),
        // Past the buffer a load reads zero and a store writes nothing.
        Case::new(
            &[Ty::U32],
            Ty::U32,
            "%fp = OpAccessChain %word_ptr %in %zero %far\n%gone = OpLoad %uint %fp\n%op = OpAccessChain %word_ptr %out %zero %far\nOpStore %op %a\n%r = OpIAdd %uint %gone %a",
        )
        .decls("%far = OpConstant %uint 1000000")
        .lane(&[9], &[9]),
    ]);
    cases
}

/// A shader reading two push constants and a uniform buffer's vector, and
/// writing their sum per component.
const PUSH_AND_UNIFORM: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %push %ubo %out
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %pblock Block
OpMemberDecorate %pblock 0 Offset 0
OpMemberDecorate %pblock 1 Offset 4
OpDecorate %ublock Block
OpMemberDecorate %ublock 0 Offset 0
OpDecorate %ubo DescriptorSet 0
OpDecorate %ubo Binding 0
OpDecorate %oblock Block
OpMemberDecorate %oblock 0 Offset 0
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v4uint = OpTypeVector %uint 4
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%pblock = OpTypeStruct %uint %uint
%ublock = OpTypeStruct %v4uint
%oblock = OpTypeStruct %v4uint
%pptr = OpTypePointer PushConstant %pblock
%uptr = OpTypePointer Uniform %ublock
%optr = OpTypePointer StorageBuffer %oblock
%pword = OpTypePointer PushConstant %uint
%uvec = OpTypePointer Uniform %v4uint
%ovec = OpTypePointer StorageBuffer %v4uint
%push = OpVariable %pptr PushConstant
%ubo = OpVariable %uptr Uniform
%out = OpVariable %optr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%p0 = OpAccessChain %pword %push %zero
%p1 = OpAccessChain %pword %push %one
%x = OpLoad %uint %p0
%y = OpLoad %uint %p1
%xy = OpIAdd %uint %x %y
%u = OpAccessChain %uvec %ubo %zero
%v = OpLoad %v4uint %u
%add = OpCompositeConstruct %v4uint %xy %xy %xy %xy
%sum = OpIAdd %v4uint %v %add
%o = OpAccessChain %ovec %out %zero
OpStore %o %sum
OpReturn
OpFunctionEnd
"#;

#[test]
fn push_constants_and_a_uniform_buffer_are_read() {
    let (device, _) = device();
    let context = context(&device);
    let module = context
        .create_shader_module(admit(PUSH_AND_UNIFORM).expect("admitted"))
        .expect("a module");
    let binding = |binding, kind, access| LayoutBinding {
        binding,
        kind,
        count: 1,
        access,
        stages: Stages::COMPUTE,
    };
    let set_layout = context
        .create_bind_set_layout(&[
            binding(0, BindingKind::UniformBuffer, Access::READ),
            binding(
                1,
                BindingKind::StorageBuffer,
                Access::READ.with(Access::WRITE),
            ),
        ])
        .expect("a set layout");
    let layout = context
        .create_pipeline_layout(
            &[&set_layout],
            PushConstants {
                bytes: 8,
                stages: Stages::COMPUTE,
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
    let held = memory(&context, 64);
    let uniform = over(&context, &held, 0, 16);
    let written = over(&context, &held, 32, 16);
    let mut values = Vec::new();
    for word in [1u32, 2, 3, 4] {
        values.extend_from_slice(&word.to_le_bytes());
    }
    held.write(0, &values).expect("writable");
    let set = context
        .create_bind_set(
            &set_layout,
            &[
                BindSetEntry {
                    binding: 0,
                    resources: &[Resource::Buffer {
                        buffer: &uniform,
                        offset: 0,
                        size: 16,
                    }],
                },
                BindSetEntry {
                    binding: 1,
                    resources: &[Resource::Buffer {
                        buffer: &written,
                        offset: 0,
                        size: 16,
                    }],
                },
            ],
        )
        .expect("a set");
    let mut push = Vec::new();
    push.extend_from_slice(&100u32.to_le_bytes());
    push.extend_from_slice(&20u32.to_le_bytes());
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&set], &push, [1, 1, 1])
        .expect("recorded");
    submit(&queue(&context), &commands).expect("dispatched");
    let mut out = [0u8; 16];
    held.read(32, &mut out).expect("readable");
    let words: Vec<u32> = out
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect();
    assert_eq!(words, [121, 122, 123, 124]);
    let mut short = context.command_buffer();
    short
        .dispatch(&pipeline, &[&set], &push[..4], [1, 1, 1])
        .expect("recorded");
    assert_eq!(
        submit(&queue(&context), &short).err().map(|e| e.error),
        Some(crate::Error::Shape),
        "push constants other than the layout's"
    );
}

/// The shaders this family runs beside its cases.
pub(super) fn bodies() -> Vec<String> {
    alloc::vec![PUSH_AND_UNIFORM.into()]
}

#[test]
fn a_chain_of_no_indexes_keeps_pointing_at_a_whole_array_of_buffers() {
    use super::image::{dispatch, module, stores, Bound};
    let (device, _) = device();
    let context = context(&device);
    let held: Vec<_> = (0..4u32)
        .map(|k| {
            let allocation = memory(&context, 4);
            allocation
                .write(0, &(10 + k).to_le_bytes())
                .expect("writable");
            let buffer = over(&context, &allocation, 0, 4);
            (allocation, buffer)
        })
        .collect();
    let resources: Vec<Resource<'_>> = held
        .iter()
        .map(|(_, buffer)| Resource::Buffer {
            buffer,
            offset: 0,
            size: 4,
        })
        .collect();
    let mut body = String::from(
        "%whole = OpAccessChain %blocks_ptr %bufs\n%third = OpAccessChain %one_ptr %whole %k2\n%w = OpAccessChain %word_ptr %third %zero\n%v = OpLoad %uint %w\n",
    );
    body.push_str(&stores(&["%v"], false));
    let text = module(
        "%bufs",
        "OpDecorate %bufs DescriptorSet 0\nOpDecorate %bufs Binding 1\nOpDecorate %one_block Block\nOpMemberDecorate %one_block 0 Offset 0",
        "%one_block = OpTypeStruct %uint\n%blocks = OpTypeArray %one_block %four\n%blocks_ptr = OpTypePointer StorageBuffer %blocks\n%one_ptr = OpTypePointer StorageBuffer %one_block\n%bufs = OpVariable %blocks_ptr StorageBuffer",
        &body,
        1,
    );
    let out = dispatch(
        &context,
        &text,
        &[Bound(BindingKind::StorageBuffer, resources)],
        &[],
        1,
    );
    assert_eq!(
        out,
        [12],
        "the third buffer, through the whole array's pointer"
    );
}
