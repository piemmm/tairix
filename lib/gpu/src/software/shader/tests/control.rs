//! Control flow across a tile's lanes: selections whose lanes part and
//! meet, loops each lane leaves at its own count, switches falling through,
//! calls inlined with their early returns and pointer parameters, and code
//! no lane reaches.

use alloc::string::String;
use alloc::vec::Vec;

use super::{Case, Ty};

/// A helper adding `%n` to the value `%p` points to, returning early where
/// that value is already past `%limit`: its loop's lanes leave it as they
/// each reach their own count.
const HELPERS: &str = r"%uint_fn = OpTypePointer Function %uint
%add_fn = OpTypeFunction %uint %uint_fn %uint
%count_fn = OpTypeFunction %uint %uint
%limit = OpConstant %uint 100
%add = OpFunction %uint None %add_fn
%p = OpFunctionParameter %uint_fn
%n = OpFunctionParameter %uint
%add_entry = OpLabel
%held = OpLoad %uint %p
%past = OpUGreaterThan %bool %held %limit
OpSelectionMerge %add_merge None
OpBranchConditional %past %add_early %add_merge
%add_early = OpLabel
OpReturnValue %held
%add_merge = OpLabel
%sum = OpIAdd %uint %held %n
OpStore %p %sum
OpReturnValue %sum
OpFunctionEnd
%count = OpFunction %uint None %count_fn
%to = OpFunctionParameter %uint
%count_entry = OpLabel
%counter = OpVariable %uint_fn Function
OpBranch %count_head
%count_head = OpLabel
%i = OpPhi %uint %zero %count_entry %next %count_continue
OpLoopMerge %count_merge %count_continue None
OpBranch %count_body
%count_body = OpLabel
%done = OpUGreaterThanEqual %bool %i %to
OpSelectionMerge %count_check None
OpBranchConditional %done %count_out %count_check
%count_out = OpLabel
%final = OpLoad %uint %counter
OpReturnValue %final
%count_check = OpLabel
%ignored = OpFunctionCall %uint %add %counter %one
OpBranch %count_continue
%count_continue = OpLabel
%next = OpIAdd %uint %i %one
OpBranch %count_head
%count_merge = OpLabel
OpUnreachable
OpFunctionEnd
";

pub(super) fn cases() -> Vec<Case> {
    let mut cases = parted();
    cases.extend(laid_out());
    cases
}

/// Lanes parting and meeting at selections, loops, switches and calls.
fn parted() -> Vec<Case> {
    use Ty::{Bool, U32, U64};
    alloc::vec![
        // Lanes part at a selection and meet at its merge, each with its own
        // value of the phi there.
        Case::new(
            &[Bool, U32],
            U32,
            "OpSelectionMerge %m None\nOpBranchConditional %a %t %e\n%t = OpLabel\n%x = OpIAdd %uint %b %one\nOpBranch %m\n%e = OpLabel\n%y = OpIMul %uint %b %b\nOpBranch %m\n%m = OpLabel\n%r = OpPhi %uint %x %t %y %e",
        )
        .lane(&[1, 5], &[6])
        .lane(&[0, 5], &[25])
        .lane(&[1, 9], &[10])
        .lane(&[0, 3], &[9]),
        // Each lane loops its own count of times: the sum of 0..n.
        Case::new(
            &[U32],
            U32,
            "OpBranch %head\n%head = OpLabel\n%i = OpPhi %uint %zero %entry %ni %cont\n%s = OpPhi %uint %zero %entry %ns %cont\nOpLoopMerge %exit %cont None\nOpBranch %body\n%body = OpLabel\n%more = OpULessThan %bool %i %a\nOpBranchConditional %more %step %exit\n%step = OpLabel\n%ns = OpIAdd %uint %s %i\nOpBranch %cont\n%cont = OpLabel\n%ni = OpIAdd %uint %i %one\nOpBranch %head\n%exit = OpLabel\n%r = OpCopyObject %uint %s",
        )
        .lane(&[0], &[0])
        .lane(&[1], &[0])
        .lane(&[5], &[10])
        .lane(&[10], &[45]),
        // A loop's phis swapping their values each iteration: the copies on
        // the back edge form a cycle.
        Case::new(
            &[U32, U32, U32],
            Ty::V(super::Scalar::U32, 2),
            "OpBranch %head\n%head = OpLabel\n%i = OpPhi %uint %zero %entry %ni %cont\n%x = OpPhi %uint %a %entry %y %cont\n%y = OpPhi %uint %b %entry %x %cont\nOpLoopMerge %exit %cont None\nOpBranch %body\n%body = OpLabel\n%more = OpULessThan %bool %i %c\nOpBranchConditional %more %cont %exit\n%cont = OpLabel\n%ni = OpIAdd %uint %i %one\nOpBranch %head\n%exit = OpLabel\n%r = OpCompositeConstruct %v2uint %x %y",
        )
        .lane(&[1, 2, 0], &[1, 2])
        .lane(&[1, 2, 1], &[2, 1])
        .lane(&[1, 2, 3], &[2, 1]),
        // A switch on a 64-bit selector, a case falling through to the next.
        Case::new(
            &[U64],
            U32,
            "OpSelectionMerge %m None\nOpSwitch %a %fallback 1 %one_case 2 %two_case 0x100000000 %wide_case\n%one_case = OpLabel\nOpBranch %two_case\n%two_case = OpLabel\n%v2 = OpPhi %uint %ten %one_case %twenty %entry\nOpBranch %m\n%wide_case = OpLabel\nOpBranch %m\n%fallback = OpLabel\nOpBranch %m\n%m = OpLabel\n%r = OpPhi %uint %v2 %two_case %thirty %wide_case %forty %fallback",
        )
        .decls("%ten = OpConstant %uint 10\n%twenty = OpConstant %uint 20\n%thirty = OpConstant %uint 30\n%forty = OpConstant %uint 40")
        .lane(&[1, 0], &[10])
        .lane(&[2, 0], &[20])
        .lane(&[0, 1], &[30])
        .lane(&[7, 0], &[40]),
        // A call inlined into a loop of its own, each lane's count its own,
        // through a pointer parameter and an early return.
        Case::new(&[U32], U32, "%r = OpFunctionCall %uint %count %a")
            .decls(HELPERS)
            .lane(&[0], &[0])
            .lane(&[3], &[3])
            .lane(&[200], &[101]),
        // A loop whose header leaves at once: its body and continue block
        // never run, though the back edge names a value only the body makes.
        Case::new(
            &[U32],
            U32,
            "OpBranch %head\n%head = OpLabel\n%i = OpPhi %uint %a %entry %next %cont\nOpLoopMerge %exit %cont None\nOpBranch %exit\n%body = OpLabel\n%next = OpIAdd %uint %i %one\nOpBranch %cont\n%cont = OpLabel\nOpBranch %head\n%exit = OpLabel\n%r = OpIAdd %uint %i %one",
        )
        .lane(&[4], &[5]),
        // A lane that reaches what cannot be reached ends, writing nothing.
        Case::new(
            &[Bool],
            U32,
            "OpNop\nOpSelectionMerge %m None\nOpBranchConditional %a %gone %m\n%gone = OpLabel\nOpUnreachable\n%m = OpLabel\n%r = OpCopyObject %uint %one",
        )
        .lane(&[0], &[1])
        .lane(&[1], &[0]),
    ]
}

/// Control flow whose lanes rejoin out of the order the module writes it
/// in, each case a way a value still needed could be overwritten.
fn laid_out() -> Vec<Case> {
    use Ty::{Bool, I16, U32};
    alloc::vec![
        // A block two branches of a selection join at before its merge is
        // laid out after both, so the value it reads from before the
        // selection outlives the other branch's.
        Case::new(
            &[U32],
            U32,
            "%few = OpULessThan %bool %a %two\n%h = OpIAdd %uint %a %hundred\nOpSelectionMerge %m None\nOpBranchConditional %few %t %e\n%t = OpLabel\nOpBranch %j\n%e = OpLabel\n%w = OpIMul %uint %a %seven\nOpBranch %j\n%j = OpLabel\n%k = OpIAdd %uint %h %zero\nOpBranch %m\n%m = OpLabel\n%r = OpCopyObject %uint %k",
        )
        .decls("%two = OpConstant %uint 2\n%hundred = OpConstant %uint 100\n%seven = OpConstant %uint 7")
        .lane(&[0], &[100])
        .lane(&[1], &[101])
        .lane(&[5], &[105])
        .lane(&[9], &[109]),
        // A case falling through to a case the module declares before it:
        // the case it falls to is laid out after it.
        Case::new(
            &[U32],
            U32,
            "%h = OpIAdd %uint %a %hundred\nOpSelectionMerge %m None\nOpSwitch %a %d 1 %first 2 %second\n%second = OpLabel\nOpBranch %m\n%first = OpLabel\n%w = OpIMul %uint %a %seven\nOpBranch %second\n%d = OpLabel\nOpBranch %m\n%m = OpLabel\n%r = OpPhi %uint %h %second %zero %d",
        )
        .decls("%hundred = OpConstant %uint 100\n%seven = OpConstant %uint 7")
        .lane(&[1], &[101])
        .lane(&[2], &[102])
        .lane(&[0], &[0])
        .lane(&[5], &[0]),
        // A phi after a line instruction in its block's head is still made
        // on each edge into it.
        Case::new(
            &[Bool, U32],
            U32,
            "OpSelectionMerge %m None\nOpBranchConditional %a %t %e\n%t = OpLabel\n%x = OpIAdd %uint %b %one\nOpBranch %m\n%e = OpLabel\n%y = OpIMul %uint %b %b\nOpBranch %m\n%m = OpLabel\n%p = OpPhi %uint %x %t %y %e\nOpNoLine\n%q = OpPhi %uint %ten %t %twenty %e\n%r = OpIAdd %uint %p %q",
        )
        .decls("%ten = OpConstant %uint 10\n%twenty = OpConstant %uint 20")
        .lane(&[1, 5], &[16])
        .lane(&[0, 5], &[45])
        .lane(&[1, 9], &[20])
        .lane(&[0, 3], &[29]),
        // A signed 16-bit selector reaches its negative cases, whose literals
        // the module writes sign-extended.
        Case::new(
            &[I16],
            U32,
            "OpSelectionMerge %m None\nOpSwitch %a %d -1 %minus_one -32768 %least 5 %five\n%minus_one = OpLabel\nOpBranch %m\n%least = OpLabel\nOpBranch %m\n%five = OpLabel\nOpBranch %m\n%d = OpLabel\nOpBranch %m\n%m = OpLabel\n%r = OpPhi %uint %ten %minus_one %twenty %least %thirty %five %forty %d",
        )
        .decls("%ten = OpConstant %uint 10\n%twenty = OpConstant %uint 20\n%thirty = OpConstant %uint 30\n%forty = OpConstant %uint 40")
        .lane(&[0xffff], &[10])
        .lane(&[0x8000], &[20])
        .lane(&[5], &[30])
        .lane(&[0x7fff], &[40]),
        // Values the tile shares, made inside a loop: one read only there,
        // one read past it. A lane runs the loop again after another has
        // left it holding the second, and returns: the first, made again,
        // lands in no register the second still needs.
        Case::new(
            &[U32],
            U32,
            "OpBranch %head\n%head = OpLabel\n%i = OpPhi %uint %zero %entry %ni %cont\nOpLoopMerge %exit %cont None\nOpBranch %body\n%body = OpLabel\n%early = OpIAdd %uint %one %two\n%sum = OpIAdd %uint %early %i\n%again = OpUGreaterThan %bool %sum %three\nOpSelectionMerge %go None\nOpBranchConditional %again %done %go\n%done = OpLabel\nOpReturn\n%go = OpLabel\n%late = OpIAdd %uint %three %four\n%leave = OpIEqual %bool %a %zero\nOpBranchConditional %leave %exit %cont\n%cont = OpLabel\n%ni = OpIAdd %uint %i %one\nOpBranch %head\n%exit = OpLabel\n%r = OpIAdd %uint %late %zero",
        )
        .decls("%two = OpConstant %uint 2\n%three = OpConstant %uint 3\n%four = OpConstant %uint 4")
        .lane(&[0], &[7])
        .lane(&[1], &[0]),
    ]
}

/// The shaders this family runs beside its cases.
pub(super) fn bodies() -> Vec<String> {
    Vec::new()
}
