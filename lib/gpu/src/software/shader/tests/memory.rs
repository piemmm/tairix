//! Memory from a shader: workgroup memory shared across a workgroup's two
//! tiles through a barrier, every atomic applied in lane order to a buffer's
//! word and to a workgroup's, function and private variables, copies between
//! them, a runtime array's length, push constants and uniform buffers, and
//! accesses past every object reading zero and writing nothing.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_spirv::assemble::admit;
use tairix_spirv::{Access, Stages};

use crate::DescriptorType;

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

/// The constants and memory the cases bounding dynamic indexes share: a
/// private array of four words and another, a workgroup one, and a private
/// array of four vectors.
const BOUNDED: &str = "%g2 = OpConstant %uint 2\n%g4 = OpConstant %uint 4\n%g5 = OpConstant %uint 5\n%g6 = OpConstant %uint 6\n%g9 = OpConstant %uint 9\n%g10 = OpConstant %uint 10\n%g_wg_scope = OpConstant %uint 2\n%g_relaxed = OpConstant %uint 0\n%g_wg_sem = OpConstant %uint 264\n%g_words = OpTypeArray %uint %g4\n%g_priv = OpTypePointer Private %g_words\n%g_word = OpTypePointer Private %uint\n%g_wg = OpTypePointer Workgroup %g_words\n%g_wg_word = OpTypePointer Workgroup %uint\n%g_vecs = OpTypeArray %v4uint %g4\n%g_pvecs = OpTypePointer Private %g_vecs\n%g_pvec = OpTypePointer Private %v4uint\n%g_held = OpVariable %g_priv Private\n%g_other = OpVariable %g_priv Private\n%g_shared = OpVariable %g_wg Workgroup\n%g_vv = OpVariable %g_pvecs Private";

/// Accesses through dynamic indexes, within their arrays and past them, in
/// every place the guard pass splits a block: the entry, a loop's header,
/// a single-block loop, a workgroup atomic, a copy, and a vector's component
/// through an element's pointer.
fn bounded() -> Vec<Case> {
    let loop_head = "%g_first = OpAccessChain %g_word %g_held %a\nOpStore %g_first %g6\nOpBranch %g_loop\n%g_loop = OpLabel\n%g_n = OpPhi %uint %zero %entry %g_n1 %g_cont\n%g_at = OpIAdd %uint %g_n %g2\n%g_slot = OpAccessChain %g_word %g_held %g_at\n%g_ten = OpIMul %uint %g_n %g10\nOpStore %g_slot %g_ten\n%g_go = OpULessThan %bool %g_n %g5\nOpLoopMerge %g_exit %g_cont None\nOpBranchConditional %g_go %g_body %g_exit\n%g_body = OpLabel\nOpBranch %g_cont\n%g_cont = OpLabel\n%g_n1 = OpIAdd %uint %g_n %one\nOpBranch %g_loop\n%g_exit = OpLabel\n%g_back = OpAccessChain %g_word %g_held %b\n%r = OpLoad %uint %g_back";
    let single = "OpBranch %g_spin\n%g_spin = OpLabel\n%g_k = OpPhi %uint %zero %entry %g_k1 %g_spin\n%g_kp = OpAccessChain %g_word %g_held %g_k\n%g_was = OpLoad %uint %g_kp\n%g_now = OpIAdd %uint %g_was %g_k\nOpStore %g_kp %g_now\n%g_k1 = OpIAdd %uint %g_k %one\n%g_more = OpULessThan %bool %g_k1 %g6\nOpLoopMerge %g_spun %g_spin None\nOpBranchConditional %g_more %g_spin %g_spun\n%g_spun = OpLabel\n%g_rp = OpAccessChain %g_word %g_held %a\n%r = OpLoad %uint %g_rp";
    let atomic = "%g_mine = OpAccessChain %g_wg_word %g_shared %lane\nOpStore %g_mine %zero\nOpControlBarrier %g_wg_scope %g_wg_scope %g_wg_sem\n%g_wp = OpAccessChain %g_wg_word %g_shared %a\n%g_old = OpAtomicIAdd %uint %g_wp %g_wg_scope %g_relaxed %one\nOpControlBarrier %g_wg_scope %g_wg_scope %g_wg_sem\n%g_seen = OpAtomicLoad %uint %g_wp %g_wg_scope %g_relaxed\n%r = OpIAdd %uint %g_seen %g_old";
    let copy = "%g_src = OpAccessChain %g_word %g_held %a\nOpStore %g_src %g6\n%g_fixed = OpAccessChain %g_word %g_other %g2\nOpStore %g_fixed %g9\n%g_dst = OpAccessChain %g_word %g_other %b\nOpCopyMemory %g_dst %g_src\n%r = OpLoad %uint %g_fixed";
    let component = "%g_e = OpAccessChain %g_pvec %g_vv %a\n%g_init = OpCompositeConstruct %v4uint %g2 %g4 %g6 %g9\nOpStore %g_e %g_init\n%g_c = OpAccessChain %g_word %g_e %b\n%r = OpLoad %uint %g_c";
    let pair = [Ty::U32, Ty::U32];
    alloc::vec![
        Case::new(&pair, Ty::U32, loop_head)
            .decls(BOUNDED)
            .interface("%g_held")
            .lane(&[0, 0], &[6])
            .lane(&[5, 3], &[10])
            .lane(&[1, 9], &[0])
            .lane(&[3, 3], &[10])
            .lane(&[1, 1], &[6])
            .lane(&[u32::MAX, 1], &[0]),
        Case::new(&[Ty::U32], Ty::U32, single)
            .decls(BOUNDED)
            .interface("%g_held")
            .lane(&[1], &[1])
            .lane(&[3], &[3])
            .lane(&[4], &[0])
            .lane(&[7], &[0]),
        Case::new(&[Ty::U32], Ty::U32, atomic)
            .decls(BOUNDED)
            .interface("%g_shared")
            .lane(&[1], &[1])
            .lane(&[9], &[0])
            .lane(&[2], &[1]),
        Case::new(&pair, Ty::U32, copy)
            .decls(BOUNDED)
            .interface("%g_held %g_other")
            .lane(&[1, 2], &[6])
            .lane(&[7, 2], &[0])
            .lane(&[1, 8], &[9])
            .lane(&[7, 8], &[9]),
        Case::new(&pair, Ty::U32, component)
            .decls(BOUNDED)
            .interface("%g_vv")
            .lane(&[1, 2], &[6])
            .lane(&[1, 4], &[0])
            .lane(&[4, 1], &[0]),
    ]
}

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
    cases.extend(bounded());
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
            binding(0, DescriptorType::UniformBuffer, Access::READ),
            binding(
                1,
                DescriptorType::StorageBuffer,
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
        &[Bound(DescriptorType::StorageBuffer, resources)],
        &[],
        1,
    );
    assert_eq!(
        out,
        [12],
        "the third buffer, through the whole array's pointer"
    );
}

/// Bounded accesses in a module of SPIR-V 1.3, whose `OpSelect` takes no
/// composite and chooses a vector a component at a time: each lane indexes
/// private arrays of three pairs and three vectors and a workgroup array of
/// four by `5 * lane % 7`, and writes the pair it loads, a vector's last
/// component, a word it stored, and the workgroup word it added to.
const BEFORE_1_4: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid
OpExecutionMode %main LocalSize 4 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%c0 = OpConstant %uint 0
%c1 = OpConstant %uint 1
%c2 = OpConstant %uint 2
%c3 = OpConstant %uint 3
%c4 = OpConstant %uint 4
%c5 = OpConstant %uint 5
%c7 = OpConstant %uint 7
%c10 = OpConstant %uint 10
%c11 = OpConstant %uint 11
%c20 = OpConstant %uint 20
%c21 = OpConstant %uint 21
%c77 = OpConstant %uint 77
%c100 = OpConstant %uint 100
%c101 = OpConstant %uint 101
%c102 = OpConstant %uint 102
%workgroup = OpConstant %uint 2
%relaxed = OpConstant %uint 0
%wg_sem = OpConstant %uint 264
%pair = OpTypeArray %uint %c2
%pairs = OpTypeArray %pair %c3
%vecs = OpTypeArray %v4uint %c3
%arr4 = OpTypeArray %uint %c4
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%out_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%priv_pairs = OpTypePointer Private %pairs
%priv_pair = OpTypePointer Private %pair
%priv_word = OpTypePointer Private %uint
%priv_vecs = OpTypePointer Private %vecs
%priv_vec = OpTypePointer Private %v4uint
%wg_arr4 = OpTypePointer Workgroup %arr4
%wg_word = OpTypePointer Workgroup %uint
%pair0 = OpConstantComposite %pair %c0 %c1
%pair1 = OpConstantComposite %pair %c10 %c11
%pair2 = OpConstantComposite %pair %c20 %c21
%pairs_init = OpConstantComposite %pairs %pair0 %pair1 %pair2
%vec0 = OpConstantComposite %v4uint %c100 %c100 %c100 %c100
%vec1 = OpConstantComposite %v4uint %c101 %c101 %c101 %c101
%vec2 = OpConstantComposite %v4uint %c102 %c102 %c102 %c102
%vecs_init = OpConstantComposite %vecs %vec0 %vec1 %vec2
%gid = OpVariable %gid_ptr Input
%out = OpVariable %out_ptr StorageBuffer
%held_pairs = OpVariable %priv_pairs Private %pairs_init
%held_vecs = OpVariable %priv_vecs Private %vecs_init
%shared = OpVariable %wg_arr4 Workgroup
%main = OpFunction %void None %fn
%entry = OpLabel
%g = OpLoad %v3uint %gid
%lane = OpCompositeExtract %uint %g 0
%times = OpIMul %uint %lane %c5
%index = OpUMod %uint %times %c7
%mine = OpAccessChain %wg_word %shared %lane
OpStore %mine %c0
OpControlBarrier %workgroup %workgroup %wg_sem
%pp = OpAccessChain %priv_pair %held_pairs %index
%pair_v = OpLoad %pair %pp
%p0 = OpCompositeExtract %uint %pair_v 0
%p1 = OpCompositeExtract %uint %pair_v 1
%vp = OpAccessChain %priv_vec %held_vecs %index
%vec = OpLoad %v4uint %vp
%v3 = OpCompositeExtract %uint %vec 3
%sp = OpAccessChain %priv_word %held_pairs %index %c1
OpStore %sp %c77
%after = OpLoad %uint %sp
%wp = OpAccessChain %wg_word %shared %index
%old = OpAtomicIAdd %uint %wp %workgroup %relaxed %c1
OpControlBarrier %workgroup %workgroup %wg_sem
%seen = OpLoad %uint %wp
%base = OpIMul %uint %lane %c5
%o0 = OpAccessChain %word_ptr %out %c0 %base
OpStore %o0 %p0
%a1 = OpIAdd %uint %base %c1
%o1 = OpAccessChain %word_ptr %out %c0 %a1
OpStore %o1 %p1
%a2 = OpIAdd %uint %base %c2
%o2 = OpAccessChain %word_ptr %out %c0 %a2
OpStore %o2 %v3
%a3 = OpIAdd %uint %base %c3
%o3 = OpAccessChain %word_ptr %out %c0 %a3
OpStore %o3 %after
%a4 = OpIAdd %uint %base %c4
%o4 = OpAccessChain %word_ptr %out %c0 %a4
OpStore %o4 %seen
OpReturn
OpFunctionEnd
"#;

#[test]
fn before_1_4_a_guarded_module_reads_and_writes_as_its_source() {
    use super::image::dispatch_module;
    let old = tairix_spirv::Version { major: 1, minor: 3 };
    let module = || {
        tairix_spirv::assemble::admit_version(BEFORE_1_4, old)
            .unwrap_or_else(|refusal| panic!("{refusal:?}"))
    };
    let (device, _) = device();
    let context = context(&device);
    let expected = [
        [0, 1, 100, 77, 1],
        [0, 0, 0, 0, 0],
        [0, 0, 0, 0, 1],
        [10, 11, 101, 77, 1],
    ]
    .concat();
    assert_eq!(dispatch_module(&context, module(), &[], &[], 20), expected);
    let guarded = super::guarded(module());
    assert_eq!(dispatch_module(&context, guarded, &[], &[], 20), expected);
}
