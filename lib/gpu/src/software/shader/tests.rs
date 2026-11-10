//! The interpreter's instructions, each run in a compute shader on the
//! software device, or in the stage only it runs in: a case names the values
//! each lane reads, the body that computes from them, and the words each lane
//! must write. The coverage test holds every instruction the validator admits
//! to a case that runs it.

mod arith;
mod composite;
mod control;
mod convert;
mod glsl;
mod group;
mod image;
mod memory;
mod robust;
mod sampling;
mod stage;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use tairix_spirv::assemble::{admit, admitted_body_ops, admitted_glsl, glsl_number};
use tairix_spirv::{Access, BindingKind, Op, Stages};

use crate::device_tests::{context, device, memory, over, queue, submit};
use crate::{BindSetEntry, ComputePipelineDesc, Context, LayoutBinding, PushConstants, Resource};

/// A value a case reads or writes, and how it travels as words.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ty {
    U32,
    I32,
    F32,
    Bool,
    U64,
    I64,
    F64,
    U16,
    I16,
    F16,
    /// A vector of a scalar.
    V(Scalar, u32),
    /// A two-member structure of one type, named in the template.
    Pair(&'static str, Scalar, u32),
}

/// The scalars a vector or pair holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Scalar {
    U32,
    I32,
    F32,
    Bool,
    U64,
    I64,
    F64,
    U16,
    I16,
    F16,
}

impl Scalar {
    const fn ty(self) -> Ty {
        match self {
            Self::U32 => Ty::U32,
            Self::I32 => Ty::I32,
            Self::F32 => Ty::F32,
            Self::Bool => Ty::Bool,
            Self::U64 => Ty::U64,
            Self::I64 => Ty::I64,
            Self::F64 => Ty::F64,
            Self::U16 => Ty::U16,
            Self::I16 => Ty::I16,
            Self::F16 => Ty::F16,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::U32 => "uint",
            Self::I32 => "int",
            Self::F32 => "float",
            Self::Bool => "bool",
            Self::U64 => "ulong",
            Self::I64 => "long",
            Self::F64 => "double",
            Self::U16 => "ushort",
            Self::I16 => "short",
            Self::F16 => "half",
        }
    }
}

impl Ty {
    /// How many words a lane's value takes in the buffer.
    const fn words(self) -> u32 {
        match self {
            Self::U64 | Self::I64 | Self::F64 => 2,
            Self::V(scalar, n) => scalar.ty().words() * n,
            Self::Pair(_, scalar, n) => scalar.ty().words() * n * 2,
            _ => 1,
        }
    }

    fn name(self) -> String {
        match self {
            Self::V(scalar, n) => format!("%v{n}{}", scalar.name()),
            Self::Pair(name, _, _) => format!("%{name}"),
            other => match other {
                Self::U32 => "%uint".into(),
                Self::I32 => "%int".into(),
                Self::F32 => "%float".into(),
                Self::Bool => "%bool".into(),
                Self::U64 => "%ulong".into(),
                Self::I64 => "%long".into(),
                Self::F64 => "%double".into(),
                Self::U16 => "%ushort".into(),
                Self::I16 => "%short".into(),
                _ => "%half".into(),
            },
        }
    }
}

/// One instruction's test: the capabilities and declarations past the
/// template's, what each lane reads as `%a`, `%b`, `%c` and `%d`, the body
/// defining `%r`, and each lane's words in and words expected out.
pub(crate) struct Case {
    pub(crate) caps: &'static str,
    pub(crate) decls: String,
    /// The global variables `decls` declares that the entry point reaches,
    /// and the decorations they take.
    pub(crate) interface: &'static str,
    pub(crate) annotations: &'static str,
    pub(crate) inputs: Vec<Ty>,
    pub(crate) output: Ty,
    pub(crate) body: String,
    pub(crate) lanes: Vec<(Vec<u32>, Vec<u32>)>,
    /// How many units in the last place each single written may be from the
    /// one expected: zero for exact words.
    pub(crate) ulps: u32,
}

impl Case {
    pub(crate) fn new(inputs: &[Ty], output: Ty, body: &str) -> Self {
        Self {
            caps: "",
            decls: String::new(),
            interface: "",
            annotations: "",
            inputs: inputs.to_vec(),
            output,
            body: body.into(),
            lanes: Vec::new(),
            ulps: 0,
        }
    }

    /// Hold each word written, as a single, within `ulps` of the expected.
    pub(crate) fn within(mut self, ulps: u32) -> Self {
        self.ulps = ulps;
        self
    }

    pub(crate) fn decls(mut self, decls: &str) -> Self {
        self.decls = decls.into();
        self
    }

    pub(crate) fn caps(mut self, caps: &'static str) -> Self {
        self.caps = caps;
        self
    }

    pub(crate) fn interface(mut self, interface: &'static str) -> Self {
        self.interface = interface;
        self
    }

    pub(crate) fn annotate(mut self, annotations: &'static str) -> Self {
        self.annotations = annotations;
        self
    }

    /// A lane reading `input` and expected to write `output`.
    pub(crate) fn lane(mut self, input: &[u32], output: &[u32]) -> Self {
        self.lanes.push((input.to_vec(), output.to_vec()));
        self
    }
}

const SCALARS: [Scalar; 10] = [
    Scalar::U32,
    Scalar::I32,
    Scalar::F32,
    Scalar::Bool,
    Scalar::U64,
    Scalar::I64,
    Scalar::F64,
    Scalar::U16,
    Scalar::I16,
    Scalar::F16,
];

/// The module a case runs as.
pub(crate) fn text(case: &Case) -> String {
    let in_words: u32 = case.inputs.iter().map(|t| t.words()).sum();
    let out_words = case.output.words();
    let lanes = case.lanes.len().max(1);
    let mut t = String::new();
    let _ = write!(
        t,
        r#"OpCapability Shader
OpCapability Int64
OpCapability Float64
OpCapability Int16
OpCapability Float16
{caps}
%glsl = OpExtInstImport "GLSL.std.450"
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %in %out {interface}
OpExecutionMode %main LocalSize {lanes} 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %in DescriptorSet 0
OpDecorate %in Binding 0
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 1
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
{annotations}
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%float = OpTypeFloat 32
%ulong = OpTypeInt 64 0
%long = OpTypeInt 64 1
%double = OpTypeFloat 64
%ushort = OpTypeInt 16 0
%short = OpTypeInt 16 1
%half = OpTypeFloat 16
"#,
        caps = case.caps,
        interface = case.interface,
        annotations = case.annotations
    );
    for scalar in SCALARS {
        for n in 2..=4 {
            let _ = writeln!(t, "%v{n}{0} = OpTypeVector %{0} {n}", scalar.name());
        }
    }
    let _ = writeln!(t, "%zero = OpConstant %uint 0\n%one = OpConstant %uint 1");
    for k in 0..=in_words.max(out_words) {
        let _ = writeln!(t, "%c{k} = OpConstant %uint {k}");
    }
    let _ = writeln!(
        t,
        "%scratch_at = OpConstant %uint {}",
        in_words as usize * lanes
    );
    let _ = write!(
        t,
        r"%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%gid = OpVariable %gid_ptr Input
%in = OpVariable %block_ptr StorageBuffer
%out = OpVariable %block_ptr StorageBuffer
{decls}
%main = OpFunction %void None %fn
%entry = OpLabel
%gidv = OpLoad %v3uint %gid
%lane = OpCompositeExtract %uint %gidv 0
%in_base = OpIMul %uint %lane %c{in_words}
%out_base = OpIMul %uint %lane %c{out_words}
",
        decls = case.decls
    );
    let mut word = 0;
    for (index, &input) in case.inputs.iter().enumerate() {
        let name = ["%a", "%b", "%c", "%d"].get(index).copied().unwrap_or("%e");
        read(&mut t, name, input, &mut word);
    }
    let _ = writeln!(t, "{}", case.body);
    let mut out = 0;
    write_out(&mut t, "%r", case.output, &mut out);
    t.push_str("OpReturn\nOpFunctionEnd\n");
    t
}

/// Instructions reading a value of `ty` into `name` from the input words.
fn read(t: &mut String, name: &str, ty: Ty, word: &mut u32) {
    let mut load = |t: &mut String| -> String {
        let w = format!("{name}_w{word}");
        let _ = writeln!(
            t,
            "{w}_i = OpIAdd %uint %in_base %c{word}\n{w}_p = OpAccessChain %word_ptr %in %zero {w}_i\n{w} = OpLoad %uint {w}_p"
        );
        *word += 1;
        w
    };
    match ty {
        Ty::U32 => {
            let w = load(t);
            let _ = writeln!(t, "{name} = OpCopyObject %uint {w}");
        }
        Ty::I32 | Ty::F32 => {
            let w = load(t);
            let _ = writeln!(t, "{name} = OpBitcast {} {w}", ty.name());
        }
        Ty::Bool => {
            let w = load(t);
            let _ = writeln!(t, "{name} = OpINotEqual %bool {w} %zero");
        }
        Ty::U64 | Ty::I64 | Ty::F64 => {
            let (lo, hi) = (load(t), load(t));
            let _ = writeln!(t, "{name}_pair = OpCompositeConstruct %v2uint {lo} {hi}\n{name} = OpBitcast {} {name}_pair", ty.name());
        }
        Ty::U16 | Ty::I16 | Ty::F16 => {
            let w = load(t);
            let _ = writeln!(t, "{name}_n = OpUConvert %ushort {w}");
            if ty == Ty::U16 {
                let _ = writeln!(t, "{name} = OpCopyObject %ushort {name}_n");
            } else {
                let _ = writeln!(t, "{name} = OpBitcast {} {name}_n", ty.name());
            }
        }
        Ty::V(scalar, n) => {
            let mut parts = Vec::new();
            for k in 0..n {
                let part = format!("{name}_{k}");
                read(t, &part, scalar.ty(), word);
                parts.push(part);
            }
            let _ = writeln!(
                t,
                "{name} = OpCompositeConstruct {} {}",
                ty.name(),
                parts.join(" ")
            );
        }
        Ty::Pair(..) => panic!("a pair is only an output"),
    }
}

/// Instructions writing `name`, a value of `ty`, to the output words.
fn write_out(t: &mut String, name: &str, ty: Ty, word: &mut u32) {
    let mut store = |t: &mut String, value: &str| {
        let w = format!("%o{word}");
        let _ = writeln!(
            t,
            "{w}_i = OpIAdd %uint %out_base %c{word}\n{w}_p = OpAccessChain %word_ptr %out %zero {w}_i\nOpStore {w}_p {value}"
        );
        *word += 1;
    };
    match ty {
        Ty::U32 => store(t, name),
        Ty::I32 | Ty::F32 => {
            let _ = writeln!(t, "{name}_u = OpBitcast %uint {name}");
            store(t, &format!("{name}_u"));
        }
        Ty::Bool => {
            let _ = writeln!(t, "{name}_u = OpSelect %uint {name} %one %zero");
            store(t, &format!("{name}_u"));
        }
        Ty::U64 | Ty::I64 | Ty::F64 => {
            let _ = writeln!(
                t,
                "{name}_p = OpBitcast %v2uint {name}\n{name}_lo = OpCompositeExtract %uint {name}_p 0\n{name}_hi = OpCompositeExtract %uint {name}_p 1"
            );
            store(t, &format!("{name}_lo"));
            store(t, &format!("{name}_hi"));
        }
        Ty::U16 | Ty::I16 | Ty::F16 => {
            if ty == Ty::U16 {
                let _ = writeln!(t, "{name}_n = OpCopyObject %ushort {name}");
            } else {
                let _ = writeln!(t, "{name}_n = OpBitcast %ushort {name}");
            }
            let _ = writeln!(t, "{name}_u = OpUConvert %uint {name}_n");
            store(t, &format!("{name}_u"));
        }
        Ty::V(scalar, n) => {
            for k in 0..n {
                let part = format!("{name}_{k}");
                let _ = writeln!(
                    t,
                    "{part} = OpCompositeExtract %{} {name} {k}",
                    scalar.name()
                );
                write_out(t, &part, scalar.ty(), word);
            }
        }
        Ty::Pair(_, scalar, n) => {
            let member = if n == 1 {
                scalar.ty()
            } else {
                Ty::V(scalar, n)
            };
            for k in 0..2 {
                let part = format!("{name}_m{k}");
                let _ = writeln!(
                    t,
                    "{part} = OpCompositeExtract {} {name} {k}",
                    member.name()
                );
                write_out(t, &part, member, word);
            }
        }
    }
}

/// The words past every lane's input a case may use as it likes: what
/// atomics work on.
pub(crate) const SCRATCH_WORDS: usize = 16;

/// Run `case`, answering each lane's output words.
pub(crate) fn run_case(context: &Context, case: &Case) -> Vec<Vec<u32>> {
    let text = text(case);
    let module = admit(&text).unwrap_or_else(|refusal| panic!("{refusal:?} in\n{text}"));
    let module = context.create_shader_module(module).expect("a module");
    let storage = |binding| LayoutBinding {
        binding,
        kind: BindingKind::StorageBuffer,
        count: 1,
        access: Access::READ.with(Access::WRITE),
        stages: Stages::COMPUTE,
    };
    let set_layout = context
        .create_bind_set_layout(&[storage(0), storage(1)])
        .expect("a set layout");
    let layout = context
        .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
        .expect("a layout");
    let pipeline = context
        .create_compute_pipeline(&ComputePipelineDesc {
            module: &module,
            entry_point: "main",
            layout: &layout,
            specialization: &[],
        })
        .expect("a pipeline");
    let in_words: usize = case.inputs.iter().map(|t| t.words() as usize).sum();
    let out_words = case.output.words() as usize;
    let lanes = case.lanes.len().max(1);
    let in_bytes = ((in_words * lanes + SCRATCH_WORDS) * 4) as u64;
    let out_bytes = (out_words * lanes * 4).max(16) as u64;
    let (in_memory, out_memory) = (memory(context, in_bytes), memory(context, out_bytes));
    let (in_buffer, out_buffer) = (
        over(context, &in_memory, 0, in_bytes),
        over(context, &out_memory, 0, out_bytes),
    );
    let mut input = Vec::new();
    for (words, _) in &case.lanes {
        assert_eq!(words.len(), in_words, "a lane's input words");
        for word in words {
            input.extend_from_slice(&word.to_le_bytes());
        }
    }
    in_memory.write(0, &input).expect("writable");
    let set = context
        .create_bind_set(
            &set_layout,
            &[
                BindSetEntry {
                    binding: 0,
                    resources: &[Resource::Buffer {
                        buffer: &in_buffer,
                        offset: 0,
                        size: in_bytes,
                    }],
                },
                BindSetEntry {
                    binding: 1,
                    resources: &[Resource::Buffer {
                        buffer: &out_buffer,
                        offset: 0,
                        size: out_bytes,
                    }],
                },
            ],
        )
        .expect("a bind set");
    let mut commands = context.command_buffer();
    commands
        .dispatch(&pipeline, &[&set], &[], [1, 1, 1])
        .expect("recorded");
    submit(&queue(context), &commands).expect("dispatched");
    let mut bytes = alloc::vec![0u8; out_words * lanes * 4];
    out_memory.read(0, &mut bytes).expect("readable");
    let words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect();
    words
        .chunks(out_words.max(1))
        .map(<[u32]>::to_vec)
        .collect()
}

/// Run `case` and hold each lane to its expected words.
pub(crate) fn check(case: &Case) {
    let (device, _) = device();
    let context = context(&device);
    let got = run_case(&context, case);
    for (lane, ((input, expected), got)) in case.lanes.iter().zip(&got).enumerate() {
        if case.ulps == 0 {
            assert_eq!(
                got, expected,
                "lane {lane} reading {input:x?} in\n{}",
                case.body
            );
            continue;
        }
        for (g, e) in got.iter().zip(expected) {
            let apart = ordered(*g).abs_diff(ordered(*e));
            assert!(
                apart <= u64::from(case.ulps),
                "lane {lane} reading {input:x?}: {} is {apart} ulps from {} in\n{}",
                f32::from_bits(*g),
                f32::from_bits(*e),
                case.body
            );
        }
    }
}

/// Each instruction `body` names, and each `GLSL.std.450` instruction.
pub(crate) fn named(body: &str, ops: &mut Vec<Op>, glsl: &mut Vec<u32>) {
    let admitted: Vec<Op> = admitted_body_ops().collect();
    for line in body.lines() {
        let mut tokens = line.split_whitespace();
        let Some(name) = tokens.by_ref().find(|t| t.starts_with("Op")) else {
            continue;
        };
        if name == "OpExtInst" {
            if let Some(number) = tokens.nth(2).and_then(glsl_number) {
                glsl.push(number);
            }
        }
        if let Some(&op) = admitted.iter().find(|op| op.name() == Some(name)) {
            ops.push(op);
        }
    }
}

/// Every case of every family.
fn all_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    cases.extend(arith::cases());
    cases.extend(convert::cases());
    cases.extend(composite::cases());
    cases.extend(glsl::cases());
    cases.extend(control::cases());
    cases.extend(group::cases());
    cases.extend(memory::cases());
    cases
}

#[test]
fn every_case_runs_as_expected() {
    for case in all_cases() {
        check(&case);
    }
}

#[test]
fn every_admitted_instruction_has_a_case_that_runs_it() {
    let (mut ops, mut numbers) = (Vec::new(), Vec::new());
    for case in all_cases() {
        named(&case.body, &mut ops, &mut numbers);
    }
    for case in all_cases() {
        named(&case.decls, &mut ops, &mut numbers);
    }
    for body in image::bodies()
        .into_iter()
        .chain(group::bodies())
        .chain(memory::bodies())
        .chain(control::bodies())
        .chain(stage::bodies())
    {
        named(&body, &mut ops, &mut numbers);
    }
    let missing: Vec<&str> = admitted_body_ops()
        .filter(|op| !ops.contains(op))
        .filter_map(Op::name)
        .collect();
    assert_eq!(missing, Vec::<&str>::new(), "instructions no case runs");
    let missing_glsl: Vec<u32> = admitted_glsl().filter(|n| !numbers.contains(n)).collect();
    assert_eq!(
        missing_glsl,
        Vec::<u32>::new(),
        "GLSL.std.450 instructions no case runs"
    );
}

/// The bits of a single.
pub(crate) const fn f(x: f32) -> u32 {
    x.to_bits()
}

/// The two words of a double, low first.
pub(crate) const fn d(x: f64) -> [u32; 2] {
    let bits = x.to_bits();
    [
        crate::software::shader::ops::low32(bits),
        crate::software::shader::ops::high32(bits),
    ]
}

/// A single's bits as an integer whose order is the floats': so the gap
/// between two is their distance in units in the last place.
fn ordered(bits: u32) -> i64 {
    let value = i64::from(bits & 0x7fff_ffff);
    if bits >> 31 == 0 {
        value
    } else {
        -value
    }
}
