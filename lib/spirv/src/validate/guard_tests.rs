//! The guard held to what it promises: every dynamic index into bounded
//! memory clamped, in every place a block may stand, every module it writes
//! admitted again and confirmed, and nothing changed where nothing needs it.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use super::{GuardError, Guarded};
use crate::assemble::{admit, admit_version};
use crate::enumerant::Capability;
use crate::{Admitted, Version, MAX_NESTING};

const V1_3: Version = Version { major: 1, minor: 3 };

/// The declarations every module here shares, its entry point's interface
/// as `interface` lists it.
fn head(capabilities: &str, interface: &str) -> String {
    format!(
        r#"OpCapability Shader
{capabilities}
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid {interface}
OpExecutionMode %main LocalSize 4 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
OpDecorate %push Block
OpMemberDecorate %push 0 Offset 0
OpDecorate %pc_words ArrayStride 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%v4float = OpTypeVector %float 4
%v3uint = OpTypeVector %uint 3
%c0 = OpConstant %uint 0
%c1 = OpConstant %uint 1
%c2 = OpConstant %uint 2
%c3 = OpConstant %uint 3
%c4 = OpConstant %uint 4
%c8 = OpConstant %uint 8
%workgroup = OpConstant %uint 2
%relaxed = OpConstant %uint 0
%arr4 = OpTypeArray %uint %c4
%arr8 = OpTypeArray %uint %c8
%vecs = OpTypeArray %v4float %c3
%pair = OpTypeArray %uint %c2
%pairs = OpTypeArray %pair %c3
%mat = OpTypeMatrix %v4float 4
%pc_words = OpTypeArray %uint %c4
%push = OpTypeStruct %pc_words
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%out_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%gid_ptr = OpTypePointer Input %v3uint
%fn_arr4 = OpTypePointer Function %arr4
%fn_uint = OpTypePointer Function %uint
%priv_vecs = OpTypePointer Private %vecs
%priv_v4 = OpTypePointer Private %v4float
%priv_float = OpTypePointer Private %float
%wg_arr8 = OpTypePointer Workgroup %arr8
%wg_uint = OpTypePointer Workgroup %uint
%fn_pairs = OpTypePointer Function %pairs
%fn_pair = OpTypePointer Function %pair
%fn_mat = OpTypePointer Function %mat
%fn_v4 = OpTypePointer Function %v4float
%pc_ptr = OpTypePointer PushConstant %push
%pc_uint = OpTypePointer PushConstant %uint
%gid = OpVariable %gid_ptr Input
%out = OpVariable %out_ptr StorageBuffer
%pc = OpVariable %pc_ptr PushConstant
%shared = OpVariable %wg_arr8 Workgroup
%private = OpVariable %priv_vecs Private
"#
    )
}

/// What a module of `version` lists in its interface: before 1.4, only its
/// inputs and outputs.
const fn interface(version: Version) -> &'static str {
    if version.minor >= 4 {
        "%out %pc %shared %private"
    } else {
        ""
    }
}

/// Every kind of access the pass bounds, each through a dynamic index.
fn accesses(version: Version) -> String {
    let mut text = head("", interface(version));
    text.push_str(
        r"%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %fn_arr4 Function
%pairs_var = OpVariable %fn_pairs Function
%mat_var = OpVariable %fn_mat Function
%gidv = OpLoad %v3uint %gid
%i = OpCompositeExtract %uint %gidv 0
%lp = OpAccessChain %fn_uint %local %i
OpStore %lp %c3
%lv = OpLoad %uint %lp
%pv = OpAccessChain %priv_v4 %private %i
%v = OpLoad %v4float %pv
%pf = OpAccessChain %priv_float %pv %i
%f = OpLoad %float %pf
%pf2 = OpCopyObject %priv_float %pf
OpStore %pf2 %f
%pp = OpAccessChain %fn_pair %pairs_var %i
%pair_v = OpLoad %pair %pp
%pp0 = OpAccessChain %fn_pair %pairs_var %c0
OpStore %pp0 %pair_v
%mc = OpAccessChain %fn_v4 %mat_var %i
%col = OpLoad %v4float %mc
%wp = OpAccessChain %wg_uint %shared %i
%old = OpAtomicIAdd %uint %wp %workgroup %relaxed %c1
OpAtomicStore %wp %workgroup %relaxed %old
%pcp = OpAccessChain %pc_uint %pc %c0 %i
%pcv = OpLoad %uint %pcp
OpCopyMemory %lp %pcp
%l0 = OpAccessChain %fn_uint %local %c0
OpCopyMemory %l0 %lp
OpCopyMemory %pp %pp0
%e = OpVectorExtractDynamic %float %col %i
%w = OpVectorInsertDynamic %v4float %v %e %i
%op = OpAccessChain %word_ptr %out %c0 %i
OpStore %op %pcv
OpReturn
OpFunctionEnd
",
    );
    text
}

/// Guarded accesses in every place a block may stand: a loop header, a
/// single-block loop, a selection header storing twice before the branch
/// whose merge's phi names it, a switch header, and a case falling through.
fn shapes(version: Version) -> String {
    let mut text = head("", interface(version));
    text.push_str(
        r"%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %fn_arr4 Function
%gidv = OpLoad %v3uint %gid
%i = OpCompositeExtract %uint %gidv 0
OpBranch %loop
%loop = OpLabel
%n = OpPhi %uint %c0 %entry %next %continue
%hp = OpAccessChain %fn_uint %local %n
OpStore %hp %n
%more = OpULessThan %bool %n %c8
OpLoopMerge %after %continue None
OpBranchConditional %more %body %after
%body = OpLabel
OpBranch %continue
%continue = OpLabel
%cp = OpAccessChain %fn_uint %local %n
OpStore %cp %c2
%next = OpIAdd %uint %n %c1
OpBranch %loop
%after = OpLabel
OpBranch %single
%single = OpLabel
%m = OpPhi %uint %c0 %after %m1 %single
%sp = OpAccessChain %fn_uint %local %m
OpStore %sp %m
%m1 = OpIAdd %uint %m %c1
%again = OpULessThan %bool %m1 %c8
OpLoopMerge %done %single None
OpBranchConditional %again %single %done
%done = OpLabel
%sel_p = OpAccessChain %fn_uint %local %i
OpStore %sel_p %c1
%sel_q = OpAccessChain %fn_uint %local %m1
OpStore %sel_q %c2
%odd = OpULessThan %bool %i %c2
OpSelectionMerge %joined None
OpBranchConditional %odd %then %joined
%then = OpLabel
OpBranch %joined
%joined = OpLabel
%which = OpPhi %uint %c1 %then %c2 %done
%sw_p = OpAccessChain %fn_uint %local %which
OpStore %sw_p %c3
OpSelectionMerge %switched None
OpSwitch %i %switched 0 %case0 1 %case1
%case0 = OpLabel
%c0p = OpAccessChain %fn_uint %local %i
OpStore %c0p %c0
OpBranch %case1
%case1 = OpLabel
OpBranch %switched
%switched = OpLabel
OpReturn
OpFunctionEnd
",
    );
    text
}

/// `text` admitted as `version`.
fn admitted(text: &str, version: Version) -> Admitted {
    admit_version(text, version).unwrap_or_else(|refusal| panic!("{refusal:?} in\n{text}"))
}

/// `text`, which needs the pass, guarded.
fn guard(text: &str, version: Version) -> Guarded {
    assert!(
        Guarded::new(admitted(text, version)).is_none(),
        "unguarded, the module does not pass the check"
    );
    admitted(text, version)
        .guarded()
        .unwrap_or_else(|error| panic!("{error:?} guarding\n{text}"))
}

#[test]
fn every_kind_of_access_is_bounded_at_every_version() {
    for version in [V1_3, Version::NEWEST] {
        guard(&accesses(version), version);
    }
}

#[test]
fn every_place_a_block_may_stand_is_split_as_the_validator_admits() {
    for version in [V1_3, Version::NEWEST] {
        guard(&shapes(version), version);
    }
}

#[test]
fn a_module_needing_nothing_is_kept_as_it_stands() {
    let mut text = head("", interface(Version::NEWEST));
    text.push_str(
        r"%main = OpFunction %void None %fn
%entry = OpLabel
%local = OpVariable %fn_arr4 Function
%gidv = OpLoad %v3uint %gid
%i = OpCompositeExtract %uint %gidv 0
%lp = OpAccessChain %fn_uint %local %c3
%lv = OpLoad %uint %lp
%op = OpAccessChain %word_ptr %out %c0 %i
OpStore %op %lv
OpReturn
OpFunctionEnd
",
    );
    let source = admitted(&text, Version::NEWEST);
    let bytes = source.module().bytes().to_vec();
    let guarded = source.guarded().expect("guarded");
    assert_eq!(guarded.admitted().module().bytes(), bytes.as_slice());
}

#[test]
fn the_pass_writes_one_module_for_one_input() {
    let text = accesses(V1_3);
    let first = admitted(&text, V1_3).guarded().expect("guarded");
    let second = admitted(&text, V1_3).guarded().expect("guarded");
    assert_eq!(
        first.admitted().module().bytes(),
        second.admitted().module().bytes()
    );
}

/// A 16-bit index into 16-bit push constants a module may only move: the
/// index widened, so its arithmetic needs nothing, and the value selected,
/// which does.
#[test]
fn narrow_data_gains_the_arithmetic_capability_it_is_selected_with() {
    for version in [V1_3, Version::NEWEST] {
        let entry = if version.minor >= 4 { "%pc %out" } else { "" };
        let text = format!(
            r#"OpCapability Shader
OpCapability StoragePushConstant16
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" {entry}
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %push Block
OpMemberDecorate %push 0 Offset 0
OpMemberDecorate %push 1 Offset 8
OpDecorate %halves ArrayStride 2
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%float = OpTypeFloat 32
%half = OpTypeFloat 16
%ushort = OpTypeInt 16 0
%c0 = OpConstant %uint 0
%c1 = OpConstant %uint 1
%c4 = OpConstant %uint 4
%halves = OpTypeArray %half %c4
%push = OpTypeStruct %halves %ushort
%block = OpTypeStruct %float
%pc_ptr = OpTypePointer PushConstant %push
%pc_half = OpTypePointer PushConstant %half
%pc_ushort = OpTypePointer PushConstant %ushort
%out_ptr = OpTypePointer StorageBuffer %block
%out_float = OpTypePointer StorageBuffer %float
%pc = OpVariable %pc_ptr PushConstant
%out = OpVariable %out_ptr StorageBuffer
%main = OpFunction %void None %fn
%entry = OpLabel
%ip = OpAccessChain %pc_ushort %pc %c1
%i = OpLoad %ushort %ip
%hp = OpAccessChain %pc_half %pc %c0 %i
%h = OpLoad %half %hp
%f = OpFConvert %float %h
%op = OpAccessChain %out_float %out %c0
OpStore %op %f
OpReturn
OpFunctionEnd
"#
        );
        let source = admitted(&text, version);
        assert!(source
            .capabilities()
            .binary_search(&Capability::FLOAT16)
            .is_err());
        let guarded = guard(&text, version);
        let capabilities = guarded.admitted().capabilities();
        assert!(capabilities.binary_search(&Capability::FLOAT16).is_ok());
        assert!(capabilities.binary_search(&Capability::INT16).is_err());
    }
}

/// A module with no Boolean and no 32-bit integer: the pass declares both.
#[test]
fn the_types_a_module_lacks_are_declared() {
    let entry = "%pc %private";
    let text = format!(
        r#"OpCapability Shader
OpCapability Int64
OpCapability Int16
OpCapability StoragePushConstant16
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" {entry}
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %push Block
OpMemberDecorate %push 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%ulong = OpTypeInt 64 0
%ushort = OpTypeInt 16 0
%zero = OpConstant %ulong 0
%one = OpConstant %ulong 1
%four = OpConstant %ulong 4
%longs = OpTypeArray %ulong %four
%push = OpTypeStruct %ushort
%pc_ptr = OpTypePointer PushConstant %push
%pc_ushort = OpTypePointer PushConstant %ushort
%priv_ptr = OpTypePointer Private %longs
%priv_ulong = OpTypePointer Private %ulong
%pc = OpVariable %pc_ptr PushConstant
%private = OpVariable %priv_ptr Private
%main = OpFunction %void None %fn
%entry = OpLabel
%ip = OpAccessChain %pc_ushort %pc %zero
%i = OpLoad %ushort %ip
%p = OpAccessChain %priv_ulong %private %i
OpStore %p %one
%wide = OpUConvert %ulong %i
%q = OpAccessChain %priv_ulong %private %wide
%v = OpLoad %ulong %q
OpStore %p %v
OpReturn
OpFunctionEnd
"#
    );
    guard(&text, Version::NEWEST);
}

/// A module indexing a private array by `index`, computed in `body` from a
/// push-constant word `%x` and a second `%y`.
fn computed(body: &str, index: &str) -> String {
    format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %pc %private
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %push Block
OpMemberDecorate %push 0 Offset 0
OpMemberDecorate %push 1 Offset 4
%void = OpTypeVoid
%fn = OpTypeFunction %void
%bool = OpTypeBool
%uint = OpTypeInt 32 0
%int = OpTypeInt 32 1
%c0 = OpConstant %uint 0
%c1 = OpConstant %uint 1
%c2 = OpConstant %uint 2
%c3 = OpConstant %uint 3
%c4 = OpConstant %uint 4
%c5 = OpConstant %uint 5
%words = OpTypeArray %uint %c4
%push = OpTypeStruct %uint %uint
%pc_ptr = OpTypePointer PushConstant %push
%pc_uint = OpTypePointer PushConstant %uint
%priv_ptr = OpTypePointer Private %words
%priv_uint = OpTypePointer Private %uint
%pc = OpVariable %pc_ptr PushConstant
%private = OpVariable %priv_ptr Private
%main = OpFunction %void None %fn
%entry = OpLabel
%xp = OpAccessChain %pc_uint %pc %c0
%x = OpLoad %uint %xp
%yp = OpAccessChain %pc_uint %pc %c1
%y = OpLoad %uint %yp
{body}
%p = OpAccessChain %priv_uint %private {index}
OpStore %p %c1
OpReturn
OpFunctionEnd
"#
    )
}

#[test]
fn the_check_believes_a_clamp_whoever_wrote_it_and_nothing_less() {
    let clamp =
        |compare: &str, select: &str| computed(&format!("%below = {compare}\n%k = {select}"), "%k");
    let honest = clamp("OpULessThan %bool %x %c4", "OpSelect %uint %below %x %c3");
    assert!(Guarded::new(admitted(&honest, Version::NEWEST)).is_some());
    let forged = [
        (
            "a bound past the level",
            clamp("OpULessThan %bool %x %c5", "OpSelect %uint %below %x %c4"),
        ),
        (
            "a last element short of the bound",
            clamp("OpULessThan %bool %x %c4", "OpSelect %uint %below %x %c2"),
        ),
        (
            "a signed comparison",
            clamp("OpSLessThan %bool %x %c4", "OpSelect %uint %below %x %c3"),
        ),
        (
            "another value compared",
            clamp("OpULessThan %bool %y %c4", "OpSelect %uint %below %x %c3"),
        ),
        (
            "the choices swapped",
            clamp("OpULessThan %bool %x %c4", "OpSelect %uint %below %c3 %x"),
        ),
        ("an unclamped index", computed("", "%x")),
    ];
    for (forgery, text) in forged {
        assert!(
            Guarded::new(admitted(&text, Version::NEWEST)).is_none(),
            "{forgery} is believed"
        );
    }
}

/// Selections nested `depth` deep, the innermost storing through a dynamic
/// index the guard splits around.
fn nested(depth: usize) -> String {
    let mut text = head("", interface(Version::NEWEST));
    text.push_str(
        "%main = OpFunction %void None %fn\n%entry = OpLabel\n%local = OpVariable %fn_arr4 Function\n%gidv = OpLoad %v3uint %gid\n%i = OpCompositeExtract %uint %gidv 0\n%t = OpULessThan %bool %i %c2\n",
    );
    for level in 0..depth {
        let _ = writeln!(
            text,
            "OpSelectionMerge %m{level} None\nOpBranchConditional %t %b{level} %m{level}\n%b{level} = OpLabel"
        );
    }
    text.push_str("%p = OpAccessChain %fn_uint %local %i\nOpStore %p %c1\n");
    for level in (0..depth).rev() {
        let _ = writeln!(text, "OpBranch %m{level}\n%m{level} = OpLabel");
    }
    text.push_str("OpReturn\nOpFunctionEnd\n");
    text
}

#[test]
fn a_split_past_the_deepest_nesting_is_too_large() {
    let deepest = (1..=MAX_NESTING)
        .rev()
        .find(|&depth| admit(&nested(depth)).is_ok())
        .expect("a nesting the validator admits");
    assert_eq!(
        deepest, MAX_NESTING,
        "the validator admits the deepest nesting"
    );
    let shallower = admitted(&nested(deepest - 1), Version::NEWEST);
    assert!(
        shallower.guarded().is_ok(),
        "one split short of the bound fits"
    );
    let deepest = admitted(&nested(deepest), Version::NEWEST);
    assert_eq!(deepest.guarded().err(), Some(GuardError::TooLarge));
}
