//! Deterministic fuzz harness for the software device's shader interpreter.
//!
//! Each round generates a random compute module the validator admits —
//! arithmetic over every scalar kind, vectors, conversions, selections and
//! data-dependent loops with early breaks, calls, barriers, subgroup
//! operations, and private, workgroup and input-buffer accesses at indexes
//! drawn from the data — lowers it, and dispatches it over random inputs;
//! then mutates its words and runs whichever mutants the validator admits.
//! The invariants are:
//!
//! 1. Nothing panics, and no admitted module faults the device: lowering and
//!    interpretation are total over every bit pattern.
//! 2. A dispatch completes or is stopped at its deadline, never later than a
//!    check of the clock past it.
//! 3. Nothing is written outside the bound output range: the allocation's
//!    other bytes, the read-only input among them, are unchanged.
//! 4. A generated module, which writes only its own invocation's words, gives
//!    the same bytes run on one core and split across several.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use std::fmt::Write as _;
use std::string::String;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::vec::Vec;

use tairix_abi::time::MonotonicClock;
use tairix_abi::Duration64;
use tairix_fuzzseed::Prng;
use tairix_gpu::{
    AllocationDesc, BindSetEntry, BufferDesc, BufferUsage, ComputePipelineDesc, ContextDesc,
    Device, Error, LayoutBinding, Loss, NoThreads, PushConstants, Resource, SoftwareDesc,
    Submission, DEFAULT_SUBGROUP_SIZE,
};
use tairix_spirv::{Access, Admitted, BindingKind, Module, Stages};

/// Rounds per sweep.
const ROUNDS_PER_SWEEP: u32 = 40;

/// Mutants of each round's module.
const MUTANTS_PER_ROUND: u32 = 12;

/// The words of the output range, and where it and the input lie in their
/// one allocation, sentinel bytes about both.
const OUTPUT_WORDS: u64 = 4096;
const OUTPUT_AT: u64 = 256;
const INPUT_WORDS: u64 = 256;
const INPUT_AT: u64 = OUTPUT_AT + OUTPUT_WORDS * 4 + 256;
const ALLOCATION: u64 = INPUT_AT + INPUT_WORDS * 4 + 256;
const SENTINEL: u8 = 0xa5;

/// A byte of the allocation, as an index.
fn place(byte: u64) -> usize {
    usize::try_from(byte).expect("within the allocation")
}

/// The words each invocation owns, one per kind of value it ends with.
const OWN_WORDS: u32 = 6;

/// A clock a nanosecond on at every reading, so a submission given a
/// nanosecond is stopped at its second check, deterministically.
#[derive(Default)]
struct Ticking(AtomicU64);

impl MonotonicClock for Ticking {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

fn device(runner: Arc<dyn tairix_parallel::JobRunner + Send>) -> Device {
    Device::software(&SoftwareDesc {
        memory_bytes: 64 << 20,
        clock: Arc::new(Ticking::default()),
        runner,
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device")
}

/// The kinds of value a generated module computes with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Uint,
    Int,
    Float,
    Bool,
    Vec4,
    Uvec2,
}

const KINDS: [Kind; 6] = [
    Kind::Uint,
    Kind::Int,
    Kind::Float,
    Kind::Bool,
    Kind::Vec4,
    Kind::Uvec2,
];

impl Kind {
    const fn ty(self) -> &'static str {
        match self {
            Self::Uint => "%uint",
            Self::Int => "%int",
            Self::Float => "%float",
            Self::Bool => "%bool",
            Self::Vec4 => "%v4float",
            Self::Uvec2 => "%v2uint",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Uint => 0,
            Self::Int => 1,
            Self::Float => 2,
            Self::Bool => 3,
            Self::Vec4 => 4,
            Self::Uvec2 => 5,
        }
    }
}

const HEADER: &str = r#"OpCapability Shader
OpCapability GroupNonUniform
OpCapability GroupNonUniformVote
OpCapability GroupNonUniformArithmetic
OpCapability GroupNonUniformBallot
%glsl = OpExtInstImport "GLSL.std.450"
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %lid %out %in %push %shared %private
OpExecutionMode %main LocalSize {size} 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %lid BuiltIn LocalInvocationIndex
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %in DescriptorSet 0
OpDecorate %in Binding 1
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
OpDecorate %push_block Block
OpMemberDecorate %push_block 0 Offset 0
OpDecorate %push_words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%float = OpTypeFloat 32
%v2uint = OpTypeVector %uint 2
%v3uint = OpTypeVector %uint 3
%v4uint = OpTypeVector %uint 4
%v4float = OpTypeVector %float 4
%c0 = OpConstant %uint 0
%c1 = OpConstant %uint 1
%c3 = OpConstant %uint 3
%c4 = OpConstant %uint 4
%c16 = OpConstant %uint 16
%c64 = OpConstant %uint 64
%own = OpConstant %uint {own}
%far = OpConstant %uint 268435456
%subgroup = OpConstant %uint 3
%workgroup = OpConstant %uint 2
%shared_sem = OpConstant %uint 264
%true = OpConstantTrue %bool
%false = OpConstantFalse %bool
{constants}%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%push_words = OpTypeArray %uint %c4
%push_block = OpTypeStruct %push_words
%push_ptr = OpTypePointer PushConstant %push_block
%push_word = OpTypePointer PushConstant %uint
%shared_t = OpTypeArray %uint %c64
%shared_ptr = OpTypePointer Workgroup %shared_t
%shared_word = OpTypePointer Workgroup %uint
%private_t = OpTypeArray %uint %c16
%private_ptr = OpTypePointer Private %private_t
%private_word = OpTypePointer Private %uint
%v3_in = OpTypePointer Input %v3uint
%uint_in = OpTypePointer Input %uint
%gid = OpVariable %v3_in Input
%lid = OpVariable %uint_in Input
%out = OpVariable %block_ptr StorageBuffer
%in = OpVariable %block_ptr StorageBuffer
%push = OpVariable %push_ptr PushConstant
%shared = OpVariable %shared_ptr Workgroup
%private = OpVariable %private_ptr Private
%helper_fn = OpTypeFunction %uint %uint %uint
%helper = OpFunction %uint None %helper_fn
%ha = OpFunctionParameter %uint
%hb = OpFunctionParameter %uint
%h_entry = OpLabel
%hc = OpIMul %uint %ha %hb
%hd = OpBitwiseXor %uint %hc %ha
%he = OpULessThan %bool %hd %hb
OpSelectionMerge %h_merge None
OpBranchConditional %he %h_early %h_merge
%h_early = OpLabel
OpReturnValue %hd
%h_merge = OpLabel
%hf = OpIAdd %uint %hd %c1
OpReturnValue %hf
OpFunctionEnd
%main = OpFunction %void None %fn
%entry = OpLabel
%g = OpLoad %v3uint %gid
%gx = OpCompositeExtract %uint %g 0
%lane = OpLoad %uint %lid
"#;

/// Integer and float operations by their spelling, each over operands of
/// its result's kind.
const UINT_BINARY: [&str; 11] = [
    "OpIAdd",
    "OpISub",
    "OpIMul",
    "OpUDiv",
    "OpUMod",
    "OpBitwiseAnd",
    "OpBitwiseOr",
    "OpBitwiseXor",
    "OpShiftLeftLogical",
    "OpShiftRightLogical",
    "OpShiftRightArithmetic",
];
const INT_BINARY: [&str; 6] = ["OpIAdd", "OpISub", "OpIMul", "OpSDiv", "OpSRem", "OpSMod"];
const FLOAT_BINARY: [&str; 6] = ["OpFAdd", "OpFSub", "OpFMul", "OpFDiv", "OpFRem", "OpFMod"];
const UINT_GLSL: [&str; 3] = ["FindUMsb", "FindILsb", "FindSMsb"];
const UINT_GLSL_BINARY: [&str; 2] = ["UMin", "UMax"];
const INT_GLSL: [&str; 2] = ["SAbs", "SSign"];
const INT_GLSL_BINARY: [&str; 2] = ["SMin", "SMax"];
const FLOAT_GLSL: [&str; 17] = [
    "Sin",
    "Cos",
    "Tanh",
    "Asin",
    "Exp",
    "Log",
    "Exp2",
    "Log2",
    "Sqrt",
    "InverseSqrt",
    "Floor",
    "Ceil",
    "Fract",
    "FAbs",
    "FSign",
    "Round",
    "RoundEven",
];
const FLOAT_GLSL_BINARY: [&str; 5] = ["FMin", "FMax", "Pow", "Atan2", "Step"];
const FLOAT_GLSL_TERNARY: [&str; 4] = ["FClamp", "FMix", "Fma", "SmoothStep"];
const COMPARISONS: [(&str, Kind); 8] = [
    ("OpIEqual", Kind::Uint),
    ("OpINotEqual", Kind::Int),
    ("OpULessThan", Kind::Uint),
    ("OpSGreaterThanEqual", Kind::Int),
    ("OpFOrdLessThan", Kind::Float),
    ("OpFUnordEqual", Kind::Float),
    ("OpFOrdGreaterThanEqual", Kind::Float),
    ("OpFUnordNotEqual", Kind::Float),
];

/// A module being generated: its text, a fresh name for each value, and the
/// values in scope, by kind.
struct Generator<'r> {
    rng: &'r mut Prng,
    text: String,
    next: u32,
    scope: [Vec<String>; 6],
    /// The label of the block instructions are being written into.
    block: String,
    budget: u32,
}

impl Generator<'_> {
    fn fresh(&mut self, stem: &str) -> String {
        self.next += 1;
        format!("%{stem}{}", self.next)
    }

    fn some(&mut self, kind: Kind) -> String {
        let values = &self.scope[kind.index()];
        values[self.rng.below(values.len())].clone()
    }

    fn line(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// Define a value of `kind` as `rest` computes it.
    fn define(&mut self, kind: Kind, rest: &str) -> String {
        let name = self.fresh("v");
        self.line(&format!("{name} = {rest}"));
        self.scope[kind.index()].push(name.clone());
        name
    }

    fn value(&mut self, kind: Kind) {
        let ty = kind.ty();
        let rest = match kind {
            Kind::Uint => self.uint(ty),
            Kind::Int => self.int(ty),
            Kind::Float => self.float(ty),
            Kind::Bool => self.boolean(ty),
            Kind::Vec4 => self.vec4(ty),
            Kind::Uvec2 => self.uvec2(ty),
        };
        self.define(kind, &rest);
    }

    /// An unsigned integer computed from what is in scope.
    fn uint(&mut self, ty: &str) -> String {
        match self.rng.below(9) {
            0..=2 => {
                let op = *self.rng.pick(&UINT_BINARY);
                format!(
                    "{op} {ty} {} {}",
                    self.some(Kind::Uint),
                    self.some(Kind::Uint)
                )
            }
            3 => format!(
                "OpExtInst {ty} %glsl {} {}",
                self.rng.pick(&UINT_GLSL),
                self.some(Kind::Uint)
            ),
            4 => {
                let op = *self.rng.pick(&UINT_GLSL_BINARY);
                format!(
                    "OpExtInst {ty} %glsl {op} {} {}",
                    self.some(Kind::Uint),
                    self.some(Kind::Uint)
                )
            }
            5 => format!("OpConvertFToU {ty} {}", self.some(Kind::Float)),
            6 => {
                let from = *self.rng.pick(&[Kind::Int, Kind::Float]);
                format!("OpBitcast {ty} {}", self.some(from))
            }
            7 => format!(
                "OpSelect {ty} {} {} {}",
                self.some(Kind::Bool),
                self.some(Kind::Uint),
                self.some(Kind::Uint)
            ),
            _ => format!(
                "OpCompositeExtract {ty} {} {}",
                self.some(Kind::Uvec2),
                self.rng.below(2)
            ),
        }
    }

    /// A signed integer computed from what is in scope.
    fn int(&mut self, ty: &str) -> String {
        match self.rng.below(6) {
            0..=2 => {
                let op = *self.rng.pick(&INT_BINARY);
                format!(
                    "{op} {ty} {} {}",
                    self.some(Kind::Int),
                    self.some(Kind::Int)
                )
            }
            3 => format!(
                "OpExtInst {ty} %glsl {} {}",
                self.rng.pick(&INT_GLSL),
                self.some(Kind::Int)
            ),
            4 => {
                let op = *self.rng.pick(&INT_GLSL_BINARY);
                format!(
                    "OpExtInst {ty} %glsl {op} {} {}",
                    self.some(Kind::Int),
                    self.some(Kind::Int)
                )
            }
            _ => match self.rng.below(2) {
                0 => format!("OpConvertFToS {ty} {}", self.some(Kind::Float)),
                _ => format!("OpBitcast {ty} {}", self.some(Kind::Uint)),
            },
        }
    }

    /// A float computed from what is in scope.
    fn float(&mut self, ty: &str) -> String {
        match self.rng.below(10) {
            0..=2 => {
                let op = *self.rng.pick(&FLOAT_BINARY);
                format!(
                    "{op} {ty} {} {}",
                    self.some(Kind::Float),
                    self.some(Kind::Float)
                )
            }
            3 => format!(
                "OpExtInst {ty} %glsl {} {}",
                self.rng.pick(&FLOAT_GLSL),
                self.some(Kind::Float)
            ),
            4 => {
                let op = *self.rng.pick(&FLOAT_GLSL_BINARY);
                format!(
                    "OpExtInst {ty} %glsl {op} {} {}",
                    self.some(Kind::Float),
                    self.some(Kind::Float)
                )
            }
            5 => {
                let op = *self.rng.pick(&FLOAT_GLSL_TERNARY);
                format!(
                    "OpExtInst {ty} %glsl {op} {} {} {}",
                    self.some(Kind::Float),
                    self.some(Kind::Float),
                    self.some(Kind::Float)
                )
            }
            6 => match self.rng.below(3) {
                0 => format!("OpConvertUToF {ty} {}", self.some(Kind::Uint)),
                1 => format!("OpConvertSToF {ty} {}", self.some(Kind::Int)),
                _ => format!("OpBitcast {ty} {}", self.some(Kind::Uint)),
            },
            7 => format!(
                "OpDot {ty} {} {}",
                self.some(Kind::Vec4),
                self.some(Kind::Vec4)
            ),
            8 => format!(
                "OpVectorExtractDynamic {ty} {} {}",
                self.some(Kind::Vec4),
                self.some(Kind::Uint)
            ),
            _ => format!(
                "OpExtInst {ty} %glsl Ldexp {} {}",
                self.some(Kind::Float),
                self.some(Kind::Int)
            ),
        }
    }

    /// A Boolean computed from what is in scope.
    fn boolean(&mut self, ty: &str) -> String {
        match self.rng.below(5) {
            0..=2 => {
                let (op, of) = *self.rng.pick(&COMPARISONS);
                format!("{op} {ty} {} {}", self.some(of), self.some(of))
            }
            3 => format!(
                "{} {ty} {}",
                self.rng.pick(&["OpIsNan", "OpIsInf"]),
                self.some(Kind::Float)
            ),
            _ => format!(
                "{} {ty} {} {}",
                self.rng
                    .pick(&["OpLogicalAnd", "OpLogicalOr", "OpLogicalNotEqual"]),
                self.some(Kind::Bool),
                self.some(Kind::Bool)
            ),
        }
    }

    /// A vector of four floats computed from what is in scope.
    fn vec4(&mut self, ty: &str) -> String {
        match self.rng.below(5) {
            0 => format!(
                "OpCompositeConstruct {ty} {} {} {} {}",
                self.some(Kind::Float),
                self.some(Kind::Float),
                self.some(Kind::Float),
                self.some(Kind::Float)
            ),
            1 => format!(
                "OpVectorTimesScalar {ty} {} {}",
                self.some(Kind::Vec4),
                self.some(Kind::Float)
            ),
            2 => format!(
                "OpFAdd {ty} {} {}",
                self.some(Kind::Vec4),
                self.some(Kind::Vec4)
            ),
            3 => {
                let lanes: Vec<String> = (0..4)
                    .map(|_| {
                        if self.rng.below(5) == 0 {
                            String::from("4294967295")
                        } else {
                            self.rng.below(8).to_string()
                        }
                    })
                    .collect();
                format!(
                    "OpVectorShuffle {ty} {} {} {}",
                    self.some(Kind::Vec4),
                    self.some(Kind::Vec4),
                    lanes.join(" ")
                )
            }
            _ => format!(
                "OpVectorInsertDynamic {ty} {} {} {}",
                self.some(Kind::Vec4),
                self.some(Kind::Float),
                self.some(Kind::Uint)
            ),
        }
    }

    /// A pair of unsigned integers computed from what is in scope.
    fn uvec2(&mut self, ty: &str) -> String {
        match self.rng.below(2) {
            0 => format!(
                "OpCompositeConstruct {ty} {} {}",
                self.some(Kind::Uint),
                self.some(Kind::Uint)
            ),
            _ => format!(
                "OpIAdd {ty} {} {}",
                self.some(Kind::Uvec2),
                self.some(Kind::Uvec2)
            ),
        }
    }

    /// Reads and writes of memory at indexes from the data, past each
    /// object's end as often as not.
    fn memory(&mut self) {
        // A constant index past a fixed array is refused, so each is made a
        // value of the run.
        let constant = self.some(Kind::Uint);
        let index = self.fresh("index");
        self.line(&format!(
            "{index} = OpBitwiseXor %uint {constant} %dynamic_zero"
        ));
        let pointer = self.fresh("p");
        match self.rng.below(4) {
            0 => {
                self.line(&format!(
                    "{pointer} = OpAccessChain %word_ptr %in %c0 {index}"
                ));
                self.define(Kind::Uint, &format!("OpLoad %uint {pointer}"));
            }
            1 => {
                self.line(&format!(
                    "{pointer} = OpAccessChain %push_word %push %c0 {index}"
                ));
                self.define(Kind::Uint, &format!("OpLoad %uint {pointer}"));
            }
            2 => {
                self.line(&format!(
                    "{pointer} = OpAccessChain %private_word %private {index}"
                ));
                let stored = self.some(Kind::Uint);
                self.line(&format!("OpStore {pointer} {stored}"));
                self.define(Kind::Uint, &format!("OpLoad %uint {pointer}"));
            }
            _ => {
                self.line(&format!(
                    "{pointer} = OpAccessChain %shared_word %shared {index}"
                ));
                if self.rng.below(2) == 0 {
                    let stored = self.some(Kind::Uint);
                    self.line(&format!("OpStore {pointer} {stored}"));
                }
                self.define(Kind::Uint, &format!("OpLoad %uint {pointer}"));
            }
        }
    }

    fn subgroup(&mut self) {
        let rest = match self.rng.below(7) {
            0 => format!(
                "OpGroupNonUniformIAdd %uint %subgroup {} {}",
                self.rng.pick(&["Reduce", "InclusiveScan", "ExclusiveScan"]),
                self.some(Kind::Uint)
            ),
            1 => format!(
                "OpGroupNonUniformUMax %uint %subgroup Reduce {}",
                self.some(Kind::Uint)
            ),
            2 => format!(
                "OpGroupNonUniformBroadcastFirst %uint %subgroup {}",
                self.some(Kind::Uint)
            ),
            3 => format!(
                "OpGroupNonUniformBroadcast %uint %subgroup {} %c3",
                self.some(Kind::Uint)
            ),
            4 => {
                let ballot = self.fresh("ballot");
                let held = self.some(Kind::Bool);
                self.line(&format!(
                    "{ballot} = OpGroupNonUniformBallot %v4uint %subgroup {held}"
                ));
                format!("OpGroupNonUniformBallotBitCount %uint %subgroup Reduce {ballot}")
            }
            5 => {
                let vote = self.fresh("vote");
                let held = self.some(Kind::Bool);
                let op = *self
                    .rng
                    .pick(&["OpGroupNonUniformAll", "OpGroupNonUniformAny"]);
                self.line(&format!("{vote} = {op} %bool %subgroup {held}"));
                format!("OpSelect %uint {vote} %c1 %c0")
            }
            _ => {
                let sum = self.fresh("fsum");
                let held = self.some(Kind::Float);
                self.line(&format!(
                    "{sum} = OpGroupNonUniformFAdd %float %subgroup Reduce {held}"
                ));
                format!("OpBitcast %uint {sum}")
            }
        };
        self.define(Kind::Uint, &rest);
    }

    /// The last value of each kind in scope.
    fn latest(&self) -> [String; 6] {
        core::array::from_fn(|k| self.scope[k].last().cloned().unwrap_or_default())
    }

    fn selection(&mut self, depth: u32) {
        let condition = self.some(Kind::Bool);
        let (then, other, merge) = (self.fresh("then"), self.fresh("else"), self.fresh("merge"));
        let saved: [usize; 6] = core::array::from_fn(|k| self.scope[k].len());
        self.line(&format!(
            "OpSelectionMerge {merge} None\nOpBranchConditional {condition} {then} {other}"
        ));
        let mut arms = Vec::new();
        for label in [&then, &other] {
            self.line(&format!("{label} = OpLabel"));
            self.block.clone_from(label);
            self.body(depth + 1);
            arms.push((self.latest(), self.block.clone()));
            self.line(&format!("OpBranch {merge}"));
            for (k, values) in self.scope.iter_mut().enumerate() {
                values.truncate(saved[k]);
            }
        }
        self.line(&format!("{merge} = OpLabel"));
        self.block = merge;
        for kind in KINDS {
            let k = kind.index();
            let name = self.fresh("phi");
            let (a, a_from) = (&arms[0].0[k], &arms[0].1);
            let (b, b_from) = (&arms[1].0[k], &arms[1].1);
            let _ = writeln!(
                self.text,
                "{name} = OpPhi {} {a} {a_from} {b} {b_from}",
                kind.ty()
            );
            self.scope[k].push(name);
        }
    }

    /// A loop of as many iterations as a value says, at most sixteen,
    /// carrying a sum of what its body computes, and breaking early where a
    /// value says to.
    fn iteration(&mut self, depth: u32) {
        let bound = self.some(Kind::Uint);
        let trips = self.define(
            Kind::Uint,
            &format!("OpExtInst %uint %glsl UMin {bound} %c16"),
        );
        let start = self.some(Kind::Uint);
        let (head, check, body, next, merge) = (
            self.fresh("head"),
            self.fresh("check"),
            self.fresh("body"),
            self.fresh("cont"),
            self.fresh("merge"),
        );
        let (i, sum, i_next, sum_next) = (
            self.fresh("i"),
            self.fresh("sum"),
            self.fresh("i"),
            self.fresh("sum"),
        );
        let before = self.block.clone();
        self.line(&format!("OpBranch {head}\n{head} = OpLabel"));
        self.line(&format!("{i} = OpPhi %uint %c0 {before} {i_next} {next}\n{sum} = OpPhi %uint {start} {before} {sum_next} {next}"));
        self.line(&format!(
            "OpLoopMerge {merge} {next} None\nOpBranch {check}\n{check} = OpLabel"
        ));
        let go = self.fresh("go");
        self.line(&format!("{go} = OpULessThan %bool {i} {trips}\nOpBranchConditional {go} {body} {merge}\n{body} = OpLabel"));
        let saved: [usize; 6] = core::array::from_fn(|k| self.scope[k].len());
        self.scope[Kind::Uint.index()].extend([i.clone(), sum.clone()]);
        self.block = body;
        self.body(depth + 1);
        if self.rng.below(3) == 0 {
            let leave = self.some(Kind::Bool);
            let stay = self.fresh("stay");
            self.line(&format!("OpSelectionMerge {stay} None\nOpBranchConditional {leave} {merge} {stay}\n{stay} = OpLabel"));
            self.block = stay;
        }
        let carried = self.some(Kind::Uint);
        self.line(&format!("{sum_next} = OpIAdd %uint {sum} {carried}\nOpBranch {next}\n{next} = OpLabel\n{i_next} = OpIAdd %uint {i} %c1\nOpBranch {head}\n{merge} = OpLabel"));
        for (k, values) in self.scope.iter_mut().enumerate() {
            values.truncate(saved[k]);
        }
        self.block = merge;
        self.scope[Kind::Uint.index()].extend([i, sum]);
    }

    fn body(&mut self, depth: u32) {
        let items = if depth == 0 {
            8 + self.rng.below(24)
        } else {
            1 + self.rng.below(8)
        };
        for _ in 0..items {
            if self.budget == 0 {
                return;
            }
            self.budget -= 1;
            match self.rng.below(20) {
                0..=11 => {
                    let kind = *self.rng.pick(&KINDS);
                    self.value(kind);
                }
                12 | 13 => self.memory(),
                14 => self.subgroup(),
                15 if depth < 3 => self.selection(depth),
                16 if depth < 2 => self.iteration(depth),
                17 => self.line("OpControlBarrier %workgroup %workgroup %shared_sem"),
                18 => {
                    let (a, b) = (self.some(Kind::Uint), self.some(Kind::Uint));
                    self.define(Kind::Uint, &format!("OpFunctionCall %uint %helper {a} {b}"));
                }
                _ => self.store_own(),
            }
        }
    }

    /// The module closed: the last value of each kind stored as a word of
    /// the invocation's own.
    fn finish(mut self) -> String {
        let finals = self.latest();
        for (word, (kind, value)) in KINDS.iter().zip(&finals).enumerate() {
            let cast = self.fresh("w");
            match kind {
                Kind::Uint => self.line(&format!("{cast} = OpCopyObject %uint {value}")),
                Kind::Int | Kind::Float => self.line(&format!("{cast} = OpBitcast %uint {value}")),
                Kind::Bool => self.line(&format!("{cast} = OpSelect %uint {value} %c1 %c0")),
                Kind::Vec4 => {
                    let part = self.fresh("w");
                    self.line(&format!("{part} = OpCompositeExtract %float {value} 1\n{cast} = OpBitcast %uint {part}"));
                }
                Kind::Uvec2 => self.line(&format!("{cast} = OpCompositeExtract %uint {value} 1")),
            }
            let (slot, at) = (self.fresh("slot"), self.fresh("at"));
            self.line(&format!(
                "{slot} = OpIAdd %uint %base %k{word}\n{at} = OpAccessChain %word_ptr %out %c0 {slot}\nOpStore {at} {cast}"
            ));
        }
        self.line("OpReturn\nOpFunctionEnd");
        self.text
    }

    /// One word of this invocation's own, or one past the output range.
    fn store_own(&mut self) {
        let slot = self.fresh("slot");
        let word = self.rng.below(OWN_WORDS as usize);
        let base = if self.rng.below(4) == 0 {
            "%far"
        } else {
            "%base"
        };
        let at = self.fresh("at");
        let value = self.some(Kind::Uint);
        self.line(&format!("{slot} = OpIAdd %uint {base} %k{word}\n{at} = OpAccessChain %word_ptr %out %c0 {slot}\nOpStore {at} {value}"));
    }
}

/// A random module, its workgroups `size` invocations.
fn generate(rng: &mut Prng, size: u32) -> String {
    let mut constants = String::new();
    let mut uints = Vec::new();
    for (n, value) in [
        0u32,
        2,
        7,
        31,
        32,
        33,
        255,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
        rng.next_u32(),
        rng.next_u32(),
    ]
    .iter()
    .enumerate()
    {
        let _ = writeln!(constants, "%u{n} = OpConstant %uint {value}");
        uints.push(format!("%u{n}"));
    }
    let mut ints = Vec::new();
    for (n, value) in [0i32, 1, -1, i32::MIN, i32::MAX, -7].iter().enumerate() {
        let _ = writeln!(constants, "%i{n} = OpConstant %int {value}");
        ints.push(format!("%i{n}"));
    }
    let mut floats = Vec::new();
    for (n, value) in [
        "0.0", "-0.0", "0.5", "1.0", "-1.5", "3.25", "1e30", "-1e-30", "65504.0",
    ]
    .iter()
    .enumerate()
    {
        let _ = writeln!(constants, "%f{n} = OpConstant %float {value}");
        floats.push(format!("%f{n}"));
    }
    for word in 0..OWN_WORDS {
        let _ = writeln!(constants, "%k{word} = OpConstant %uint {word}");
    }
    let _ = writeln!(
        constants,
        "%inf_bits = OpConstant %uint {}\n%nan_bits = OpConstant %uint {}",
        f32::INFINITY.to_bits(),
        f32::NAN.to_bits() | 1 << 31
    );
    let mut text = HEADER
        .replace("{size}", &size.to_string())
        .replace("{own}", &OWN_WORDS.to_string())
        .replace("{constants}", &constants);
    // Infinities, NaNs and subnormals reach the floats through their bits.
    text.push_str("%dynamic_zero = OpISub %uint %lane %lane\n%base = OpIMul %uint %gx %own\n%inf = OpBitcast %float %inf_bits\n%nan = OpBitcast %float %nan_bits\n%tiny = OpBitcast %float %c1\n");
    text.push_str("%v4a = OpCompositeConstruct %v4float %f3 %nan %f2 %tiny\n%u2a = OpCompositeConstruct %v2uint %lane %gx\n");
    floats.extend(["%inf".into(), "%nan".into(), "%tiny".into()]);
    uints.extend(["%gx".into(), "%lane".into(), "%base".into()]);
    let mut generator = Generator {
        rng,
        text,
        next: 0,
        scope: [
            uints,
            ints,
            floats,
            vec!["%true".into(), "%false".into()],
            vec!["%v4a".into()],
            vec!["%u2a".into()],
        ],
        block: String::from("%entry"),
        budget: 160,
    };
    generator.body(0);
    generator.finish()
}

/// What one dispatch of a module left: its context's state and every byte
/// of its allocation, read through a context that imported it.
struct Ran {
    state: Result<(), Error>,
    bytes: Vec<u8>,
}

/// The inputs a round runs its modules over.
struct Inputs {
    allocation: Vec<u8>,
    push: [u8; 16],
    groups: u32,
}

/// Lower and dispatch `admitted` on `device`, or answer why a pipeline was
/// not made.
fn dispatch(device: &Device, admitted: Admitted, inputs: &Inputs) -> Result<Ran, Error> {
    let context = device.create_context(&ContextDesc::default())?;
    let watcher = device.create_context(&ContextDesc::default())?;
    let module = context.create_shader_module(admitted)?;
    let binding = |binding, access| LayoutBinding {
        binding,
        kind: BindingKind::StorageBuffer,
        count: 1,
        access,
        stages: Stages::COMPUTE,
    };
    let set_layout = context.create_bind_set_layout(&[
        binding(0, Access::READ.with(Access::WRITE)),
        binding(1, Access::READ),
    ])?;
    let layout = context.create_pipeline_layout(
        &[&set_layout],
        PushConstants {
            bytes: 16,
            stages: Stages::COMPUTE,
        },
    )?;
    let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
        module: &module,
        entry_point: "main",
        layout: &layout,
        specialization: &[],
    })?;
    let memory = context.allocate(&AllocationDesc {
        size: ALLOCATION,
        memory_type: 0,
    })?;
    memory.write(0, &inputs.allocation)?;
    let seen = watcher.import(memory.export()?)?;
    let buffer = context.create_buffer(
        &memory,
        &BufferDesc {
            offset: 0,
            size: ALLOCATION,
            usage: BufferUsage::STORAGE,
        },
    )?;
    let output = [Resource::Buffer {
        buffer: &buffer,
        offset: OUTPUT_AT,
        size: OUTPUT_WORDS * 4,
    }];
    let input = [Resource::Buffer {
        buffer: &buffer,
        offset: INPUT_AT,
        size: INPUT_WORDS * 4,
    }];
    let set = context.create_bind_set(
        &set_layout,
        &[
            BindSetEntry {
                binding: 0,
                resources: &output,
            },
            BindSetEntry {
                binding: 1,
                resources: &input,
            },
        ],
    )?;
    let mut commands = context.command_buffer();
    commands.dispatch(&pipeline, &[&set], &inputs.push, [inputs.groups, 1, 1])?;
    let queue = context.queue(0, 0)?;
    let submitted = queue.submit(&Submission {
        command_buffers: &[&commands],
        waits: &[],
        signals: &[],
        deadline: Duration64::from_nanos(1),
    });
    if let Err(refused) = submitted {
        panic!("a checked dispatch was refused: {refused:?}");
    }
    let mut bytes = vec![0u8; place(ALLOCATION)];
    seen.read(0, &mut bytes)?;
    Ok(Ran {
        state: context.status(),
        bytes,
    })
}

/// Hold one run to the first three invariants.
fn check(ran: &Ran, inputs: &Inputs) {
    assert!(
        matches!(ran.state, Ok(()) | Err(Error::Lost(Loss::Overran))),
        "a dispatch ended {:?}",
        ran.state
    );
    let output = place(OUTPUT_AT)..place(OUTPUT_AT + OUTPUT_WORDS * 4);
    for (at, (&now, &before)) in ran.bytes.iter().zip(&inputs.allocation).enumerate() {
        if !output.contains(&at) {
            assert_eq!(now, before, "byte {at}, outside the output, was written");
        }
    }
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

fn mutate(rng: &mut Prng, words: &mut [u32]) {
    for _ in 0..=rng.below(3) {
        let at = 5 + rng.below(words.len() - 5);
        match rng.below(4) {
            0 => words[at] ^= 1 << rng.below(32),
            1 => {
                let any = rng.next_u32();
                words[at] = *rng.pick(&[0, 1, 2, 3, 4, 31, 32, 64, 0x8000_0000, u32::MAX, any]);
            }
            2 => {
                let other = 5 + rng.below(words.len() - 5);
                words.swap(at, other);
            }
            _ => words[at] = words[at].wrapping_add(1),
        }
    }
}

fn run_round(rng: &mut Prng, serial: &Device, split: &Device) {
    let size = *rng.pick(&[1u32, 7, 32, 64, 100]);
    let text = generate(rng, size);
    let admitted = tairix_spirv::assemble::admit(&text)
        .unwrap_or_else(|refusal| panic!("{refusal:?} refused the generated module\n{text}"));
    let mut allocation = vec![SENTINEL; place(ALLOCATION)];
    let input = place(INPUT_AT)..place(INPUT_AT + INPUT_WORDS * 4);
    rng.fill(&mut allocation[input]);
    let mut push = [0u8; 16];
    rng.fill(&mut push);
    let inputs = Inputs {
        allocation,
        push,
        groups: 1 + rng.next_u32() % 3,
    };
    let module_words = words(admitted.module().bytes());
    match dispatch(serial, admitted, &inputs) {
        Ok(ran) => {
            check(&ran, &inputs);
            let again = tairix_spirv::assemble::admit(&text).expect("admitted again");
            let other = dispatch(split, again, &inputs).expect("made on the other device too");
            check(&other, &inputs);
            if ran.state.is_ok() && other.state.is_ok() {
                assert!(
                    ran.bytes == other.bytes,
                    "one core and several disagree over\n{text}"
                );
            }
        }
        Err(Error::TooLarge) => {}
        Err(error) => panic!("{error:?} making a pipeline of\n{text}"),
    }
    for _ in 0..MUTANTS_PER_ROUND {
        let mut mutant = module_words.clone();
        mutate(rng, &mut mutant);
        let bytes: Vec<u8> = mutant.iter().flat_map(|w| w.to_le_bytes()).collect();
        let Ok(module) = Module::decode(&bytes) else {
            continue;
        };
        let Ok(admitted) = Admitted::new(module) else {
            continue;
        };
        match dispatch(serial, admitted, &inputs) {
            Ok(ran) => check(&ran, &inputs),
            Err(
                Error::TooLarge | Error::EntryPoint | Error::Disagreement(_) | Error::Unsupported,
            ) => {}
            Err(error) => panic!("{error:?} making a pipeline of a mutant"),
        }
    }
}

#[test]
fn the_interpreter_is_total_contained_and_deterministic_over_generated_modules() {
    let serial = device(Arc::new(tairix_parallel::Serial));
    let split = device(Arc::new(tairix_parallel::Reversed::new(4)));
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_interpreter_is_total_contained_and_deterministic_over_generated_modules",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..ROUNDS_PER_SWEEP {
            run_round(&mut rng, &serial, &split);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
