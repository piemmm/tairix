// GENERATED FILE — DO NOT EDIT.
//
// Emitted by `cargo xtask spirv --write` from the Khronos SPIR-V grammar
// in `lib/spirv/assets/` (SPIRV-Headers vulkan-sdk-1.4.363.0; MIT, see `LICENSE`
// there). `cargo xtask spirv` (run by `ci`) fails closed if this file
// drifts from a fresh generation.

use crate::grammar::{Enumerant, ExtInst, Gate, Quantity, Shape, Spec};
use crate::{Op, Version};
use enumerant::Capability;

/// The grammar's SPIR-V version (revision 7), the newest a module may
/// declare.
pub(crate) const VERSION: (u8, u8) = (1, 6);

const V1_0: Option<Version> = Some(Version { major: 1, minor: 0 });
const V1_1: Option<Version> = Some(Version { major: 1, minor: 1 });
const V1_2: Option<Version> = Some(Version { major: 1, minor: 2 });
const V1_3: Option<Version> = Some(Version { major: 1, minor: 3 });
const V1_4: Option<Version> = Some(Version { major: 1, minor: 4 });
const V1_5: Option<Version> = Some(Version { major: 1, minor: 5 });
const V1_6: Option<Version> = Some(Version { major: 1, minor: 6 });

/// Core from the first version, needing nothing.
const CORE: Gate = Gate { capabilities: &[], extensions: &[], version: V1_0, last: None };

impl Op {
    /// `OpNop`.
    pub const NOP: Self = Self(0);
    /// `OpUndef`.
    pub const UNDEF: Self = Self(1);
    /// `OpSourceContinued`.
    pub const SOURCE_CONTINUED: Self = Self(2);
    /// `OpSource`.
    pub const SOURCE: Self = Self(3);
    /// `OpSourceExtension`.
    pub const SOURCE_EXTENSION: Self = Self(4);
    /// `OpName`.
    pub const NAME: Self = Self(5);
    /// `OpMemberName`.
    pub const MEMBER_NAME: Self = Self(6);
    /// `OpString`.
    pub const STRING: Self = Self(7);
    /// `OpLine`.
    pub const LINE: Self = Self(8);
    /// `OpExtension`.
    pub const EXTENSION: Self = Self(10);
    /// `OpExtInstImport`.
    pub const EXT_INST_IMPORT: Self = Self(11);
    /// `OpExtInst`.
    pub const EXT_INST: Self = Self(12);
    /// `OpMemoryModel`.
    pub const MEMORY_MODEL: Self = Self(14);
    /// `OpEntryPoint`.
    pub const ENTRY_POINT: Self = Self(15);
    /// `OpExecutionMode`.
    pub const EXECUTION_MODE: Self = Self(16);
    /// `OpCapability`.
    pub const CAPABILITY: Self = Self(17);
    /// `OpTypeVoid`.
    pub const TYPE_VOID: Self = Self(19);
    /// `OpTypeBool`.
    pub const TYPE_BOOL: Self = Self(20);
    /// `OpTypeInt`.
    pub const TYPE_INT: Self = Self(21);
    /// `OpTypeFloat`.
    pub const TYPE_FLOAT: Self = Self(22);
    /// `OpTypeVector`.
    pub const TYPE_VECTOR: Self = Self(23);
    /// `OpTypeMatrix`.
    pub const TYPE_MATRIX: Self = Self(24);
    /// `OpTypeImage`.
    pub const TYPE_IMAGE: Self = Self(25);
    /// `OpTypeSampler`.
    pub const TYPE_SAMPLER: Self = Self(26);
    /// `OpTypeSampledImage`.
    pub const TYPE_SAMPLED_IMAGE: Self = Self(27);
    /// `OpTypeArray`.
    pub const TYPE_ARRAY: Self = Self(28);
    /// `OpTypeRuntimeArray`.
    pub const TYPE_RUNTIME_ARRAY: Self = Self(29);
    /// `OpTypeStruct`.
    pub const TYPE_STRUCT: Self = Self(30);
    /// `OpTypeOpaque`.
    pub const TYPE_OPAQUE: Self = Self(31);
    /// `OpTypePointer`.
    pub const TYPE_POINTER: Self = Self(32);
    /// `OpTypeFunction`.
    pub const TYPE_FUNCTION: Self = Self(33);
    /// `OpTypeEvent`.
    pub const TYPE_EVENT: Self = Self(34);
    /// `OpTypeDeviceEvent`.
    pub const TYPE_DEVICE_EVENT: Self = Self(35);
    /// `OpTypeReserveId`.
    pub const TYPE_RESERVE_ID: Self = Self(36);
    /// `OpTypeQueue`.
    pub const TYPE_QUEUE: Self = Self(37);
    /// `OpTypePipe`.
    pub const TYPE_PIPE: Self = Self(38);
    /// `OpTypeForwardPointer`.
    pub const TYPE_FORWARD_POINTER: Self = Self(39);
    /// `OpConstantTrue`.
    pub const CONSTANT_TRUE: Self = Self(41);
    /// `OpConstantFalse`.
    pub const CONSTANT_FALSE: Self = Self(42);
    /// `OpConstant`.
    pub const CONSTANT: Self = Self(43);
    /// `OpConstantComposite`.
    pub const CONSTANT_COMPOSITE: Self = Self(44);
    /// `OpConstantSampler`.
    pub const CONSTANT_SAMPLER: Self = Self(45);
    /// `OpConstantNull`.
    pub const CONSTANT_NULL: Self = Self(46);
    /// `OpSpecConstantTrue`.
    pub const SPEC_CONSTANT_TRUE: Self = Self(48);
    /// `OpSpecConstantFalse`.
    pub const SPEC_CONSTANT_FALSE: Self = Self(49);
    /// `OpSpecConstant`.
    pub const SPEC_CONSTANT: Self = Self(50);
    /// `OpSpecConstantComposite`.
    pub const SPEC_CONSTANT_COMPOSITE: Self = Self(51);
    /// `OpSpecConstantOp`.
    pub const SPEC_CONSTANT_OP: Self = Self(52);
    /// `OpFunction`.
    pub const FUNCTION: Self = Self(54);
    /// `OpFunctionParameter`.
    pub const FUNCTION_PARAMETER: Self = Self(55);
    /// `OpFunctionEnd`.
    pub const FUNCTION_END: Self = Self(56);
    /// `OpFunctionCall`.
    pub const FUNCTION_CALL: Self = Self(57);
    /// `OpVariable`.
    pub const VARIABLE: Self = Self(59);
    /// `OpImageTexelPointer`.
    pub const IMAGE_TEXEL_POINTER: Self = Self(60);
    /// `OpLoad`.
    pub const LOAD: Self = Self(61);
    /// `OpStore`.
    pub const STORE: Self = Self(62);
    /// `OpCopyMemory`.
    pub const COPY_MEMORY: Self = Self(63);
    /// `OpCopyMemorySized`.
    pub const COPY_MEMORY_SIZED: Self = Self(64);
    /// `OpAccessChain`.
    pub const ACCESS_CHAIN: Self = Self(65);
    /// `OpInBoundsAccessChain`.
    pub const IN_BOUNDS_ACCESS_CHAIN: Self = Self(66);
    /// `OpPtrAccessChain`.
    pub const PTR_ACCESS_CHAIN: Self = Self(67);
    /// `OpArrayLength`.
    pub const ARRAY_LENGTH: Self = Self(68);
    /// `OpGenericPtrMemSemantics`.
    pub const GENERIC_PTR_MEM_SEMANTICS: Self = Self(69);
    /// `OpInBoundsPtrAccessChain`.
    pub const IN_BOUNDS_PTR_ACCESS_CHAIN: Self = Self(70);
    /// `OpDecorate`.
    pub const DECORATE: Self = Self(71);
    /// `OpMemberDecorate`.
    pub const MEMBER_DECORATE: Self = Self(72);
    /// `OpDecorationGroup`.
    pub const DECORATION_GROUP: Self = Self(73);
    /// `OpGroupDecorate`.
    pub const GROUP_DECORATE: Self = Self(74);
    /// `OpGroupMemberDecorate`.
    pub const GROUP_MEMBER_DECORATE: Self = Self(75);
    /// `OpVectorExtractDynamic`.
    pub const VECTOR_EXTRACT_DYNAMIC: Self = Self(77);
    /// `OpVectorInsertDynamic`.
    pub const VECTOR_INSERT_DYNAMIC: Self = Self(78);
    /// `OpVectorShuffle`.
    pub const VECTOR_SHUFFLE: Self = Self(79);
    /// `OpCompositeConstruct`.
    pub const COMPOSITE_CONSTRUCT: Self = Self(80);
    /// `OpCompositeExtract`.
    pub const COMPOSITE_EXTRACT: Self = Self(81);
    /// `OpCompositeInsert`.
    pub const COMPOSITE_INSERT: Self = Self(82);
    /// `OpCopyObject`.
    pub const COPY_OBJECT: Self = Self(83);
    /// `OpTranspose`.
    pub const TRANSPOSE: Self = Self(84);
    /// `OpSampledImage`.
    pub const SAMPLED_IMAGE: Self = Self(86);
    /// `OpImageSampleImplicitLod`.
    pub const IMAGE_SAMPLE_IMPLICIT_LOD: Self = Self(87);
    /// `OpImageSampleExplicitLod`.
    pub const IMAGE_SAMPLE_EXPLICIT_LOD: Self = Self(88);
    /// `OpImageSampleDrefImplicitLod`.
    pub const IMAGE_SAMPLE_DREF_IMPLICIT_LOD: Self = Self(89);
    /// `OpImageSampleDrefExplicitLod`.
    pub const IMAGE_SAMPLE_DREF_EXPLICIT_LOD: Self = Self(90);
    /// `OpImageSampleProjImplicitLod`.
    pub const IMAGE_SAMPLE_PROJ_IMPLICIT_LOD: Self = Self(91);
    /// `OpImageSampleProjExplicitLod`.
    pub const IMAGE_SAMPLE_PROJ_EXPLICIT_LOD: Self = Self(92);
    /// `OpImageSampleProjDrefImplicitLod`.
    pub const IMAGE_SAMPLE_PROJ_DREF_IMPLICIT_LOD: Self = Self(93);
    /// `OpImageSampleProjDrefExplicitLod`.
    pub const IMAGE_SAMPLE_PROJ_DREF_EXPLICIT_LOD: Self = Self(94);
    /// `OpImageFetch`.
    pub const IMAGE_FETCH: Self = Self(95);
    /// `OpImageGather`.
    pub const IMAGE_GATHER: Self = Self(96);
    /// `OpImageDrefGather`.
    pub const IMAGE_DREF_GATHER: Self = Self(97);
    /// `OpImageRead`.
    pub const IMAGE_READ: Self = Self(98);
    /// `OpImageWrite`.
    pub const IMAGE_WRITE: Self = Self(99);
    /// `OpImage`.
    pub const IMAGE: Self = Self(100);
    /// `OpImageQueryFormat`.
    pub const IMAGE_QUERY_FORMAT: Self = Self(101);
    /// `OpImageQueryOrder`.
    pub const IMAGE_QUERY_ORDER: Self = Self(102);
    /// `OpImageQuerySizeLod`.
    pub const IMAGE_QUERY_SIZE_LOD: Self = Self(103);
    /// `OpImageQuerySize`.
    pub const IMAGE_QUERY_SIZE: Self = Self(104);
    /// `OpImageQueryLod`.
    pub const IMAGE_QUERY_LOD: Self = Self(105);
    /// `OpImageQueryLevels`.
    pub const IMAGE_QUERY_LEVELS: Self = Self(106);
    /// `OpImageQuerySamples`.
    pub const IMAGE_QUERY_SAMPLES: Self = Self(107);
    /// `OpConvertFToU`.
    pub const CONVERT_F_TO_U: Self = Self(109);
    /// `OpConvertFToS`.
    pub const CONVERT_F_TO_S: Self = Self(110);
    /// `OpConvertSToF`.
    pub const CONVERT_S_TO_F: Self = Self(111);
    /// `OpConvertUToF`.
    pub const CONVERT_U_TO_F: Self = Self(112);
    /// `OpUConvert`.
    pub const U_CONVERT: Self = Self(113);
    /// `OpSConvert`.
    pub const S_CONVERT: Self = Self(114);
    /// `OpFConvert`.
    pub const F_CONVERT: Self = Self(115);
    /// `OpQuantizeToF16`.
    pub const QUANTIZE_TO_F16: Self = Self(116);
    /// `OpConvertPtrToU`.
    pub const CONVERT_PTR_TO_U: Self = Self(117);
    /// `OpSatConvertSToU`.
    pub const SAT_CONVERT_S_TO_U: Self = Self(118);
    /// `OpSatConvertUToS`.
    pub const SAT_CONVERT_U_TO_S: Self = Self(119);
    /// `OpConvertUToPtr`.
    pub const CONVERT_U_TO_PTR: Self = Self(120);
    /// `OpPtrCastToGeneric`.
    pub const PTR_CAST_TO_GENERIC: Self = Self(121);
    /// `OpGenericCastToPtr`.
    pub const GENERIC_CAST_TO_PTR: Self = Self(122);
    /// `OpGenericCastToPtrExplicit`.
    pub const GENERIC_CAST_TO_PTR_EXPLICIT: Self = Self(123);
    /// `OpBitcast`.
    pub const BITCAST: Self = Self(124);
    /// `OpSNegate`.
    pub const S_NEGATE: Self = Self(126);
    /// `OpFNegate`.
    pub const F_NEGATE: Self = Self(127);
    /// `OpIAdd`.
    pub const I_ADD: Self = Self(128);
    /// `OpFAdd`.
    pub const F_ADD: Self = Self(129);
    /// `OpISub`.
    pub const I_SUB: Self = Self(130);
    /// `OpFSub`.
    pub const F_SUB: Self = Self(131);
    /// `OpIMul`.
    pub const I_MUL: Self = Self(132);
    /// `OpFMul`.
    pub const F_MUL: Self = Self(133);
    /// `OpUDiv`.
    pub const U_DIV: Self = Self(134);
    /// `OpSDiv`.
    pub const S_DIV: Self = Self(135);
    /// `OpFDiv`.
    pub const F_DIV: Self = Self(136);
    /// `OpUMod`.
    pub const U_MOD: Self = Self(137);
    /// `OpSRem`.
    pub const S_REM: Self = Self(138);
    /// `OpSMod`.
    pub const S_MOD: Self = Self(139);
    /// `OpFRem`.
    pub const F_REM: Self = Self(140);
    /// `OpFMod`.
    pub const F_MOD: Self = Self(141);
    /// `OpVectorTimesScalar`.
    pub const VECTOR_TIMES_SCALAR: Self = Self(142);
    /// `OpMatrixTimesScalar`.
    pub const MATRIX_TIMES_SCALAR: Self = Self(143);
    /// `OpVectorTimesMatrix`.
    pub const VECTOR_TIMES_MATRIX: Self = Self(144);
    /// `OpMatrixTimesVector`.
    pub const MATRIX_TIMES_VECTOR: Self = Self(145);
    /// `OpMatrixTimesMatrix`.
    pub const MATRIX_TIMES_MATRIX: Self = Self(146);
    /// `OpOuterProduct`.
    pub const OUTER_PRODUCT: Self = Self(147);
    /// `OpDot`.
    pub const DOT: Self = Self(148);
    /// `OpIAddCarry`.
    pub const I_ADD_CARRY: Self = Self(149);
    /// `OpISubBorrow`.
    pub const I_SUB_BORROW: Self = Self(150);
    /// `OpUMulExtended`.
    pub const U_MUL_EXTENDED: Self = Self(151);
    /// `OpSMulExtended`.
    pub const S_MUL_EXTENDED: Self = Self(152);
    /// `OpAny`.
    pub const ANY: Self = Self(154);
    /// `OpAll`.
    pub const ALL: Self = Self(155);
    /// `OpIsNan`.
    pub const IS_NAN: Self = Self(156);
    /// `OpIsInf`.
    pub const IS_INF: Self = Self(157);
    /// `OpIsFinite`.
    pub const IS_FINITE: Self = Self(158);
    /// `OpIsNormal`.
    pub const IS_NORMAL: Self = Self(159);
    /// `OpSignBitSet`.
    pub const SIGN_BIT_SET: Self = Self(160);
    /// `OpLessOrGreater`.
    pub const LESS_OR_GREATER: Self = Self(161);
    /// `OpOrdered`.
    pub const ORDERED: Self = Self(162);
    /// `OpUnordered`.
    pub const UNORDERED: Self = Self(163);
    /// `OpLogicalEqual`.
    pub const LOGICAL_EQUAL: Self = Self(164);
    /// `OpLogicalNotEqual`.
    pub const LOGICAL_NOT_EQUAL: Self = Self(165);
    /// `OpLogicalOr`.
    pub const LOGICAL_OR: Self = Self(166);
    /// `OpLogicalAnd`.
    pub const LOGICAL_AND: Self = Self(167);
    /// `OpLogicalNot`.
    pub const LOGICAL_NOT: Self = Self(168);
    /// `OpSelect`.
    pub const SELECT: Self = Self(169);
    /// `OpIEqual`.
    pub const I_EQUAL: Self = Self(170);
    /// `OpINotEqual`.
    pub const I_NOT_EQUAL: Self = Self(171);
    /// `OpUGreaterThan`.
    pub const U_GREATER_THAN: Self = Self(172);
    /// `OpSGreaterThan`.
    pub const S_GREATER_THAN: Self = Self(173);
    /// `OpUGreaterThanEqual`.
    pub const U_GREATER_THAN_EQUAL: Self = Self(174);
    /// `OpSGreaterThanEqual`.
    pub const S_GREATER_THAN_EQUAL: Self = Self(175);
    /// `OpULessThan`.
    pub const U_LESS_THAN: Self = Self(176);
    /// `OpSLessThan`.
    pub const S_LESS_THAN: Self = Self(177);
    /// `OpULessThanEqual`.
    pub const U_LESS_THAN_EQUAL: Self = Self(178);
    /// `OpSLessThanEqual`.
    pub const S_LESS_THAN_EQUAL: Self = Self(179);
    /// `OpFOrdEqual`.
    pub const F_ORD_EQUAL: Self = Self(180);
    /// `OpFUnordEqual`.
    pub const F_UNORD_EQUAL: Self = Self(181);
    /// `OpFOrdNotEqual`.
    pub const F_ORD_NOT_EQUAL: Self = Self(182);
    /// `OpFUnordNotEqual`.
    pub const F_UNORD_NOT_EQUAL: Self = Self(183);
    /// `OpFOrdLessThan`.
    pub const F_ORD_LESS_THAN: Self = Self(184);
    /// `OpFUnordLessThan`.
    pub const F_UNORD_LESS_THAN: Self = Self(185);
    /// `OpFOrdGreaterThan`.
    pub const F_ORD_GREATER_THAN: Self = Self(186);
    /// `OpFUnordGreaterThan`.
    pub const F_UNORD_GREATER_THAN: Self = Self(187);
    /// `OpFOrdLessThanEqual`.
    pub const F_ORD_LESS_THAN_EQUAL: Self = Self(188);
    /// `OpFUnordLessThanEqual`.
    pub const F_UNORD_LESS_THAN_EQUAL: Self = Self(189);
    /// `OpFOrdGreaterThanEqual`.
    pub const F_ORD_GREATER_THAN_EQUAL: Self = Self(190);
    /// `OpFUnordGreaterThanEqual`.
    pub const F_UNORD_GREATER_THAN_EQUAL: Self = Self(191);
    /// `OpShiftRightLogical`.
    pub const SHIFT_RIGHT_LOGICAL: Self = Self(194);
    /// `OpShiftRightArithmetic`.
    pub const SHIFT_RIGHT_ARITHMETIC: Self = Self(195);
    /// `OpShiftLeftLogical`.
    pub const SHIFT_LEFT_LOGICAL: Self = Self(196);
    /// `OpBitwiseOr`.
    pub const BITWISE_OR: Self = Self(197);
    /// `OpBitwiseXor`.
    pub const BITWISE_XOR: Self = Self(198);
    /// `OpBitwiseAnd`.
    pub const BITWISE_AND: Self = Self(199);
    /// `OpNot`.
    pub const NOT: Self = Self(200);
    /// `OpBitFieldInsert`.
    pub const BIT_FIELD_INSERT: Self = Self(201);
    /// `OpBitFieldSExtract`.
    pub const BIT_FIELD_S_EXTRACT: Self = Self(202);
    /// `OpBitFieldUExtract`.
    pub const BIT_FIELD_U_EXTRACT: Self = Self(203);
    /// `OpBitReverse`.
    pub const BIT_REVERSE: Self = Self(204);
    /// `OpBitCount`.
    pub const BIT_COUNT: Self = Self(205);
    /// `OpDPdx`.
    pub const D_PDX: Self = Self(207);
    /// `OpDPdy`.
    pub const D_PDY: Self = Self(208);
    /// `OpFwidth`.
    pub const FWIDTH: Self = Self(209);
    /// `OpDPdxFine`.
    pub const D_PDX_FINE: Self = Self(210);
    /// `OpDPdyFine`.
    pub const D_PDY_FINE: Self = Self(211);
    /// `OpFwidthFine`.
    pub const FWIDTH_FINE: Self = Self(212);
    /// `OpDPdxCoarse`.
    pub const D_PDX_COARSE: Self = Self(213);
    /// `OpDPdyCoarse`.
    pub const D_PDY_COARSE: Self = Self(214);
    /// `OpFwidthCoarse`.
    pub const FWIDTH_COARSE: Self = Self(215);
    /// `OpEmitVertex`.
    pub const EMIT_VERTEX: Self = Self(218);
    /// `OpEndPrimitive`.
    pub const END_PRIMITIVE: Self = Self(219);
    /// `OpEmitStreamVertex`.
    pub const EMIT_STREAM_VERTEX: Self = Self(220);
    /// `OpEndStreamPrimitive`.
    pub const END_STREAM_PRIMITIVE: Self = Self(221);
    /// `OpControlBarrier`.
    pub const CONTROL_BARRIER: Self = Self(224);
    /// `OpMemoryBarrier`.
    pub const MEMORY_BARRIER: Self = Self(225);
    /// `OpAtomicLoad`.
    pub const ATOMIC_LOAD: Self = Self(227);
    /// `OpAtomicStore`.
    pub const ATOMIC_STORE: Self = Self(228);
    /// `OpAtomicExchange`.
    pub const ATOMIC_EXCHANGE: Self = Self(229);
    /// `OpAtomicCompareExchange`.
    pub const ATOMIC_COMPARE_EXCHANGE: Self = Self(230);
    /// `OpAtomicCompareExchangeWeak`.
    pub const ATOMIC_COMPARE_EXCHANGE_WEAK: Self = Self(231);
    /// `OpAtomicIIncrement`.
    pub const ATOMIC_I_INCREMENT: Self = Self(232);
    /// `OpAtomicIDecrement`.
    pub const ATOMIC_I_DECREMENT: Self = Self(233);
    /// `OpAtomicIAdd`.
    pub const ATOMIC_I_ADD: Self = Self(234);
    /// `OpAtomicISub`.
    pub const ATOMIC_I_SUB: Self = Self(235);
    /// `OpAtomicSMin`.
    pub const ATOMIC_S_MIN: Self = Self(236);
    /// `OpAtomicUMin`.
    pub const ATOMIC_U_MIN: Self = Self(237);
    /// `OpAtomicSMax`.
    pub const ATOMIC_S_MAX: Self = Self(238);
    /// `OpAtomicUMax`.
    pub const ATOMIC_U_MAX: Self = Self(239);
    /// `OpAtomicAnd`.
    pub const ATOMIC_AND: Self = Self(240);
    /// `OpAtomicOr`.
    pub const ATOMIC_OR: Self = Self(241);
    /// `OpAtomicXor`.
    pub const ATOMIC_XOR: Self = Self(242);
    /// `OpPhi`.
    pub const PHI: Self = Self(245);
    /// `OpLoopMerge`.
    pub const LOOP_MERGE: Self = Self(246);
    /// `OpSelectionMerge`.
    pub const SELECTION_MERGE: Self = Self(247);
    /// `OpLabel`.
    pub const LABEL: Self = Self(248);
    /// `OpBranch`.
    pub const BRANCH: Self = Self(249);
    /// `OpBranchConditional`.
    pub const BRANCH_CONDITIONAL: Self = Self(250);
    /// `OpSwitch`.
    pub const SWITCH: Self = Self(251);
    /// `OpKill`.
    pub const KILL: Self = Self(252);
    /// `OpReturn`.
    pub const RETURN: Self = Self(253);
    /// `OpReturnValue`.
    pub const RETURN_VALUE: Self = Self(254);
    /// `OpUnreachable`.
    pub const UNREACHABLE: Self = Self(255);
    /// `OpLifetimeStart`.
    pub const LIFETIME_START: Self = Self(256);
    /// `OpLifetimeStop`.
    pub const LIFETIME_STOP: Self = Self(257);
    /// `OpGroupAsyncCopy`.
    pub const GROUP_ASYNC_COPY: Self = Self(259);
    /// `OpGroupWaitEvents`.
    pub const GROUP_WAIT_EVENTS: Self = Self(260);
    /// `OpGroupAll`.
    pub const GROUP_ALL: Self = Self(261);
    /// `OpGroupAny`.
    pub const GROUP_ANY: Self = Self(262);
    /// `OpGroupBroadcast`.
    pub const GROUP_BROADCAST: Self = Self(263);
    /// `OpGroupIAdd`.
    pub const GROUP_I_ADD: Self = Self(264);
    /// `OpGroupFAdd`.
    pub const GROUP_F_ADD: Self = Self(265);
    /// `OpGroupFMin`.
    pub const GROUP_F_MIN: Self = Self(266);
    /// `OpGroupUMin`.
    pub const GROUP_U_MIN: Self = Self(267);
    /// `OpGroupSMin`.
    pub const GROUP_S_MIN: Self = Self(268);
    /// `OpGroupFMax`.
    pub const GROUP_F_MAX: Self = Self(269);
    /// `OpGroupUMax`.
    pub const GROUP_U_MAX: Self = Self(270);
    /// `OpGroupSMax`.
    pub const GROUP_S_MAX: Self = Self(271);
    /// `OpReadPipe`.
    pub const READ_PIPE: Self = Self(274);
    /// `OpWritePipe`.
    pub const WRITE_PIPE: Self = Self(275);
    /// `OpReservedReadPipe`.
    pub const RESERVED_READ_PIPE: Self = Self(276);
    /// `OpReservedWritePipe`.
    pub const RESERVED_WRITE_PIPE: Self = Self(277);
    /// `OpReserveReadPipePackets`.
    pub const RESERVE_READ_PIPE_PACKETS: Self = Self(278);
    /// `OpReserveWritePipePackets`.
    pub const RESERVE_WRITE_PIPE_PACKETS: Self = Self(279);
    /// `OpCommitReadPipe`.
    pub const COMMIT_READ_PIPE: Self = Self(280);
    /// `OpCommitWritePipe`.
    pub const COMMIT_WRITE_PIPE: Self = Self(281);
    /// `OpIsValidReserveId`.
    pub const IS_VALID_RESERVE_ID: Self = Self(282);
    /// `OpGetNumPipePackets`.
    pub const GET_NUM_PIPE_PACKETS: Self = Self(283);
    /// `OpGetMaxPipePackets`.
    pub const GET_MAX_PIPE_PACKETS: Self = Self(284);
    /// `OpGroupReserveReadPipePackets`.
    pub const GROUP_RESERVE_READ_PIPE_PACKETS: Self = Self(285);
    /// `OpGroupReserveWritePipePackets`.
    pub const GROUP_RESERVE_WRITE_PIPE_PACKETS: Self = Self(286);
    /// `OpGroupCommitReadPipe`.
    pub const GROUP_COMMIT_READ_PIPE: Self = Self(287);
    /// `OpGroupCommitWritePipe`.
    pub const GROUP_COMMIT_WRITE_PIPE: Self = Self(288);
    /// `OpEnqueueMarker`.
    pub const ENQUEUE_MARKER: Self = Self(291);
    /// `OpEnqueueKernel`.
    pub const ENQUEUE_KERNEL: Self = Self(292);
    /// `OpGetKernelNDrangeSubGroupCount`.
    pub const GET_KERNEL_N_DRANGE_SUB_GROUP_COUNT: Self = Self(293);
    /// `OpGetKernelNDrangeMaxSubGroupSize`.
    pub const GET_KERNEL_N_DRANGE_MAX_SUB_GROUP_SIZE: Self = Self(294);
    /// `OpGetKernelWorkGroupSize`.
    pub const GET_KERNEL_WORK_GROUP_SIZE: Self = Self(295);
    /// `OpGetKernelPreferredWorkGroupSizeMultiple`.
    pub const GET_KERNEL_PREFERRED_WORK_GROUP_SIZE_MULTIPLE: Self = Self(296);
    /// `OpRetainEvent`.
    pub const RETAIN_EVENT: Self = Self(297);
    /// `OpReleaseEvent`.
    pub const RELEASE_EVENT: Self = Self(298);
    /// `OpCreateUserEvent`.
    pub const CREATE_USER_EVENT: Self = Self(299);
    /// `OpIsValidEvent`.
    pub const IS_VALID_EVENT: Self = Self(300);
    /// `OpSetUserEventStatus`.
    pub const SET_USER_EVENT_STATUS: Self = Self(301);
    /// `OpCaptureEventProfilingInfo`.
    pub const CAPTURE_EVENT_PROFILING_INFO: Self = Self(302);
    /// `OpGetDefaultQueue`.
    pub const GET_DEFAULT_QUEUE: Self = Self(303);
    /// `OpBuildNDRange`.
    pub const BUILD_ND_RANGE: Self = Self(304);
    /// `OpImageSparseSampleImplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_IMPLICIT_LOD: Self = Self(305);
    /// `OpImageSparseSampleExplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_EXPLICIT_LOD: Self = Self(306);
    /// `OpImageSparseSampleDrefImplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_DREF_IMPLICIT_LOD: Self = Self(307);
    /// `OpImageSparseSampleDrefExplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_DREF_EXPLICIT_LOD: Self = Self(308);
    /// `OpImageSparseSampleProjImplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_PROJ_IMPLICIT_LOD: Self = Self(309);
    /// `OpImageSparseSampleProjExplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_PROJ_EXPLICIT_LOD: Self = Self(310);
    /// `OpImageSparseSampleProjDrefImplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_PROJ_DREF_IMPLICIT_LOD: Self = Self(311);
    /// `OpImageSparseSampleProjDrefExplicitLod`.
    pub const IMAGE_SPARSE_SAMPLE_PROJ_DREF_EXPLICIT_LOD: Self = Self(312);
    /// `OpImageSparseFetch`.
    pub const IMAGE_SPARSE_FETCH: Self = Self(313);
    /// `OpImageSparseGather`.
    pub const IMAGE_SPARSE_GATHER: Self = Self(314);
    /// `OpImageSparseDrefGather`.
    pub const IMAGE_SPARSE_DREF_GATHER: Self = Self(315);
    /// `OpImageSparseTexelsResident`.
    pub const IMAGE_SPARSE_TEXELS_RESIDENT: Self = Self(316);
    /// `OpNoLine`.
    pub const NO_LINE: Self = Self(317);
    /// `OpAtomicFlagTestAndSet`.
    pub const ATOMIC_FLAG_TEST_AND_SET: Self = Self(318);
    /// `OpAtomicFlagClear`.
    pub const ATOMIC_FLAG_CLEAR: Self = Self(319);
    /// `OpImageSparseRead`.
    pub const IMAGE_SPARSE_READ: Self = Self(320);
    /// `OpSizeOf`.
    pub const SIZE_OF: Self = Self(321);
    /// `OpTypePipeStorage`.
    pub const TYPE_PIPE_STORAGE: Self = Self(322);
    /// `OpConstantPipeStorage`.
    pub const CONSTANT_PIPE_STORAGE: Self = Self(323);
    /// `OpCreatePipeFromPipeStorage`.
    pub const CREATE_PIPE_FROM_PIPE_STORAGE: Self = Self(324);
    /// `OpGetKernelLocalSizeForSubgroupCount`.
    pub const GET_KERNEL_LOCAL_SIZE_FOR_SUBGROUP_COUNT: Self = Self(325);
    /// `OpGetKernelMaxNumSubgroups`.
    pub const GET_KERNEL_MAX_NUM_SUBGROUPS: Self = Self(326);
    /// `OpTypeNamedBarrier`.
    pub const TYPE_NAMED_BARRIER: Self = Self(327);
    /// `OpNamedBarrierInitialize`.
    pub const NAMED_BARRIER_INITIALIZE: Self = Self(328);
    /// `OpMemoryNamedBarrier`.
    pub const MEMORY_NAMED_BARRIER: Self = Self(329);
    /// `OpModuleProcessed`.
    pub const MODULE_PROCESSED: Self = Self(330);
    /// `OpExecutionModeId`.
    pub const EXECUTION_MODE_ID: Self = Self(331);
    /// `OpDecorateId`.
    pub const DECORATE_ID: Self = Self(332);
    /// `OpGroupNonUniformElect`.
    pub const GROUP_NON_UNIFORM_ELECT: Self = Self(333);
    /// `OpGroupNonUniformAll`.
    pub const GROUP_NON_UNIFORM_ALL: Self = Self(334);
    /// `OpGroupNonUniformAny`.
    pub const GROUP_NON_UNIFORM_ANY: Self = Self(335);
    /// `OpGroupNonUniformAllEqual`.
    pub const GROUP_NON_UNIFORM_ALL_EQUAL: Self = Self(336);
    /// `OpGroupNonUniformBroadcast`.
    pub const GROUP_NON_UNIFORM_BROADCAST: Self = Self(337);
    /// `OpGroupNonUniformBroadcastFirst`.
    pub const GROUP_NON_UNIFORM_BROADCAST_FIRST: Self = Self(338);
    /// `OpGroupNonUniformBallot`.
    pub const GROUP_NON_UNIFORM_BALLOT: Self = Self(339);
    /// `OpGroupNonUniformInverseBallot`.
    pub const GROUP_NON_UNIFORM_INVERSE_BALLOT: Self = Self(340);
    /// `OpGroupNonUniformBallotBitExtract`.
    pub const GROUP_NON_UNIFORM_BALLOT_BIT_EXTRACT: Self = Self(341);
    /// `OpGroupNonUniformBallotBitCount`.
    pub const GROUP_NON_UNIFORM_BALLOT_BIT_COUNT: Self = Self(342);
    /// `OpGroupNonUniformBallotFindLSB`.
    pub const GROUP_NON_UNIFORM_BALLOT_FIND_LSB: Self = Self(343);
    /// `OpGroupNonUniformBallotFindMSB`.
    pub const GROUP_NON_UNIFORM_BALLOT_FIND_MSB: Self = Self(344);
    /// `OpGroupNonUniformShuffle`.
    pub const GROUP_NON_UNIFORM_SHUFFLE: Self = Self(345);
    /// `OpGroupNonUniformShuffleXor`.
    pub const GROUP_NON_UNIFORM_SHUFFLE_XOR: Self = Self(346);
    /// `OpGroupNonUniformShuffleUp`.
    pub const GROUP_NON_UNIFORM_SHUFFLE_UP: Self = Self(347);
    /// `OpGroupNonUniformShuffleDown`.
    pub const GROUP_NON_UNIFORM_SHUFFLE_DOWN: Self = Self(348);
    /// `OpGroupNonUniformIAdd`.
    pub const GROUP_NON_UNIFORM_I_ADD: Self = Self(349);
    /// `OpGroupNonUniformFAdd`.
    pub const GROUP_NON_UNIFORM_F_ADD: Self = Self(350);
    /// `OpGroupNonUniformIMul`.
    pub const GROUP_NON_UNIFORM_I_MUL: Self = Self(351);
    /// `OpGroupNonUniformFMul`.
    pub const GROUP_NON_UNIFORM_F_MUL: Self = Self(352);
    /// `OpGroupNonUniformSMin`.
    pub const GROUP_NON_UNIFORM_S_MIN: Self = Self(353);
    /// `OpGroupNonUniformUMin`.
    pub const GROUP_NON_UNIFORM_U_MIN: Self = Self(354);
    /// `OpGroupNonUniformFMin`.
    pub const GROUP_NON_UNIFORM_F_MIN: Self = Self(355);
    /// `OpGroupNonUniformSMax`.
    pub const GROUP_NON_UNIFORM_S_MAX: Self = Self(356);
    /// `OpGroupNonUniformUMax`.
    pub const GROUP_NON_UNIFORM_U_MAX: Self = Self(357);
    /// `OpGroupNonUniformFMax`.
    pub const GROUP_NON_UNIFORM_F_MAX: Self = Self(358);
    /// `OpGroupNonUniformBitwiseAnd`.
    pub const GROUP_NON_UNIFORM_BITWISE_AND: Self = Self(359);
    /// `OpGroupNonUniformBitwiseOr`.
    pub const GROUP_NON_UNIFORM_BITWISE_OR: Self = Self(360);
    /// `OpGroupNonUniformBitwiseXor`.
    pub const GROUP_NON_UNIFORM_BITWISE_XOR: Self = Self(361);
    /// `OpGroupNonUniformLogicalAnd`.
    pub const GROUP_NON_UNIFORM_LOGICAL_AND: Self = Self(362);
    /// `OpGroupNonUniformLogicalOr`.
    pub const GROUP_NON_UNIFORM_LOGICAL_OR: Self = Self(363);
    /// `OpGroupNonUniformLogicalXor`.
    pub const GROUP_NON_UNIFORM_LOGICAL_XOR: Self = Self(364);
    /// `OpGroupNonUniformQuadBroadcast`.
    pub const GROUP_NON_UNIFORM_QUAD_BROADCAST: Self = Self(365);
    /// `OpGroupNonUniformQuadSwap`.
    pub const GROUP_NON_UNIFORM_QUAD_SWAP: Self = Self(366);
    /// `OpCopyLogical`.
    pub const COPY_LOGICAL: Self = Self(400);
    /// `OpPtrEqual`.
    pub const PTR_EQUAL: Self = Self(401);
    /// `OpPtrNotEqual`.
    pub const PTR_NOT_EQUAL: Self = Self(402);
    /// `OpPtrDiff`.
    pub const PTR_DIFF: Self = Self(403);
    /// `OpColorAttachmentReadEXT`.
    pub const COLOR_ATTACHMENT_READ_EXT: Self = Self(4160);
    /// `OpDepthAttachmentReadEXT`.
    pub const DEPTH_ATTACHMENT_READ_EXT: Self = Self(4161);
    /// `OpStencilAttachmentReadEXT`.
    pub const STENCIL_ATTACHMENT_READ_EXT: Self = Self(4162);
    /// `OpBitcastExtractEXT`.
    pub const BITCAST_EXTRACT_EXT: Self = Self(4195);
    /// `OpTerminateInvocation`.
    pub const TERMINATE_INVOCATION: Self = Self(4416);
    /// `OpTypeUntypedPointerKHR`.
    pub const TYPE_UNTYPED_POINTER_KHR: Self = Self(4417);
    /// `OpUntypedVariableKHR`.
    pub const UNTYPED_VARIABLE_KHR: Self = Self(4418);
    /// `OpUntypedAccessChainKHR`.
    pub const UNTYPED_ACCESS_CHAIN_KHR: Self = Self(4419);
    /// `OpUntypedInBoundsAccessChainKHR`.
    pub const UNTYPED_IN_BOUNDS_ACCESS_CHAIN_KHR: Self = Self(4420);
    /// `OpSubgroupBallotKHR`.
    pub const SUBGROUP_BALLOT_KHR: Self = Self(4421);
    /// `OpSubgroupFirstInvocationKHR`.
    pub const SUBGROUP_FIRST_INVOCATION_KHR: Self = Self(4422);
    /// `OpUntypedPtrAccessChainKHR`.
    pub const UNTYPED_PTR_ACCESS_CHAIN_KHR: Self = Self(4423);
    /// `OpUntypedInBoundsPtrAccessChainKHR`.
    pub const UNTYPED_IN_BOUNDS_PTR_ACCESS_CHAIN_KHR: Self = Self(4424);
    /// `OpUntypedArrayLengthKHR`.
    pub const UNTYPED_ARRAY_LENGTH_KHR: Self = Self(4425);
    /// `OpUntypedPrefetchKHR`.
    pub const UNTYPED_PREFETCH_KHR: Self = Self(4426);
    /// `OpFmaKHR`.
    pub const FMA_KHR: Self = Self(4427);
    /// `OpSubgroupAllKHR`.
    pub const SUBGROUP_ALL_KHR: Self = Self(4428);
    /// `OpSubgroupAnyKHR`.
    pub const SUBGROUP_ANY_KHR: Self = Self(4429);
    /// `OpSubgroupAllEqualKHR`.
    pub const SUBGROUP_ALL_EQUAL_KHR: Self = Self(4430);
    /// `OpGroupNonUniformRotateKHR`.
    pub const GROUP_NON_UNIFORM_ROTATE_KHR: Self = Self(4431);
    /// `OpSubgroupReadInvocationKHR`.
    pub const SUBGROUP_READ_INVOCATION_KHR: Self = Self(4432);
    /// `OpExtInstWithForwardRefsKHR`.
    pub const EXT_INST_WITH_FORWARD_REFS_KHR: Self = Self(4433);
    /// `OpUntypedGroupAsyncCopyKHR`.
    pub const UNTYPED_GROUP_ASYNC_COPY_KHR: Self = Self(4434);
    /// `OpTraceRayKHR`.
    pub const TRACE_RAY_KHR: Self = Self(4445);
    /// `OpExecuteCallableKHR`.
    pub const EXECUTE_CALLABLE_KHR: Self = Self(4446);
    /// `OpConvertUToAccelerationStructureKHR`.
    pub const CONVERT_U_TO_ACCELERATION_STRUCTURE_KHR: Self = Self(4447);
    /// `OpIgnoreIntersectionKHR`.
    pub const IGNORE_INTERSECTION_KHR: Self = Self(4448);
    /// `OpTerminateRayKHR`.
    pub const TERMINATE_RAY_KHR: Self = Self(4449);
    /// `OpSDot`.
    pub const S_DOT: Self = Self(4450);
    /// `OpUDot`.
    pub const U_DOT: Self = Self(4451);
    /// `OpSUDot`.
    pub const SU_DOT: Self = Self(4452);
    /// `OpSDotAccSat`.
    pub const S_DOT_ACC_SAT: Self = Self(4453);
    /// `OpUDotAccSat`.
    pub const U_DOT_ACC_SAT: Self = Self(4454);
    /// `OpSUDotAccSat`.
    pub const SU_DOT_ACC_SAT: Self = Self(4455);
    /// `OpTypeCooperativeMatrixKHR`.
    pub const TYPE_COOPERATIVE_MATRIX_KHR: Self = Self(4456);
    /// `OpCooperativeMatrixLoadKHR`.
    pub const COOPERATIVE_MATRIX_LOAD_KHR: Self = Self(4457);
    /// `OpCooperativeMatrixStoreKHR`.
    pub const COOPERATIVE_MATRIX_STORE_KHR: Self = Self(4458);
    /// `OpCooperativeMatrixMulAddKHR`.
    pub const COOPERATIVE_MATRIX_MUL_ADD_KHR: Self = Self(4459);
    /// `OpCooperativeMatrixLengthKHR`.
    pub const COOPERATIVE_MATRIX_LENGTH_KHR: Self = Self(4460);
    /// `OpConstantCompositeReplicateEXT`.
    pub const CONSTANT_COMPOSITE_REPLICATE_EXT: Self = Self(4461);
    /// `OpSpecConstantCompositeReplicateEXT`.
    pub const SPEC_CONSTANT_COMPOSITE_REPLICATE_EXT: Self = Self(4462);
    /// `OpCompositeConstructReplicateEXT`.
    pub const COMPOSITE_CONSTRUCT_REPLICATE_EXT: Self = Self(4463);
    /// `OpTypeRayQueryKHR`.
    pub const TYPE_RAY_QUERY_KHR: Self = Self(4472);
    /// `OpRayQueryInitializeKHR`.
    pub const RAY_QUERY_INITIALIZE_KHR: Self = Self(4473);
    /// `OpRayQueryTerminateKHR`.
    pub const RAY_QUERY_TERMINATE_KHR: Self = Self(4474);
    /// `OpRayQueryGenerateIntersectionKHR`.
    pub const RAY_QUERY_GENERATE_INTERSECTION_KHR: Self = Self(4475);
    /// `OpRayQueryConfirmIntersectionKHR`.
    pub const RAY_QUERY_CONFIRM_INTERSECTION_KHR: Self = Self(4476);
    /// `OpRayQueryProceedKHR`.
    pub const RAY_QUERY_PROCEED_KHR: Self = Self(4477);
    /// `OpRayQueryGetIntersectionTypeKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_TYPE_KHR: Self = Self(4479);
    /// `OpReadClockKHR`.
    pub const READ_CLOCK_KHR: Self = Self(5056);
    /// `OpGroupNonUniformQuadAllKHR`.
    pub const GROUP_NON_UNIFORM_QUAD_ALL_KHR: Self = Self(5110);
    /// `OpGroupNonUniformQuadAnyKHR`.
    pub const GROUP_NON_UNIFORM_QUAD_ANY_KHR: Self = Self(5111);
    /// `OpTypeBufferEXT`.
    pub const TYPE_BUFFER_EXT: Self = Self(5115);
    /// `OpBufferPointerEXT`.
    pub const BUFFER_POINTER_EXT: Self = Self(5119);
    /// `OpAbortKHR`.
    pub const ABORT_KHR: Self = Self(5121);
    /// `OpUntypedImageTexelPointerEXT`.
    pub const UNTYPED_IMAGE_TEXEL_POINTER_EXT: Self = Self(5126);
    /// `OpMemberDecorateIdEXT`.
    pub const MEMBER_DECORATE_ID_EXT: Self = Self(5127);
    /// `OpConstantSizeOfEXT`.
    pub const CONSTANT_SIZE_OF_EXT: Self = Self(5129);
    /// `OpConstantDataKHR`.
    pub const CONSTANT_DATA_KHR: Self = Self(5147);
    /// `OpSpecConstantDataKHR`.
    pub const SPEC_CONSTANT_DATA_KHR: Self = Self(5148);
    /// `OpTypeVectorIdEXT`.
    pub const TYPE_VECTOR_ID_EXT: Self = Self(5288);
    /// `OpCooperativeMatrixConvertUseEXT`.
    pub const COOPERATIVE_MATRIX_CONVERT_USE_EXT: Self = Self(5293);
    /// `OpEmitMeshTasksEXT`.
    pub const EMIT_MESH_TASKS_EXT: Self = Self(5294);
    /// `OpSetMeshOutputsEXT`.
    pub const SET_MESH_OUTPUTS_EXT: Self = Self(5295);
    /// `OpGroupNonUniformPartitionEXT`.
    pub const GROUP_NON_UNIFORM_PARTITION_EXT: Self = Self(5296);
    /// `OpHitObjectRecordFromQueryEXT`.
    pub const HIT_OBJECT_RECORD_FROM_QUERY_EXT: Self = Self(5304);
    /// `OpHitObjectRecordMissEXT`.
    pub const HIT_OBJECT_RECORD_MISS_EXT: Self = Self(5305);
    /// `OpHitObjectRecordMissMotionEXT`.
    pub const HIT_OBJECT_RECORD_MISS_MOTION_EXT: Self = Self(5306);
    /// `OpHitObjectGetIntersectionTriangleVertexPositionsEXT`.
    pub const HIT_OBJECT_GET_INTERSECTION_TRIANGLE_VERTEX_POSITIONS_EXT: Self = Self(5307);
    /// `OpHitObjectGetRayFlagsEXT`.
    pub const HIT_OBJECT_GET_RAY_FLAGS_EXT: Self = Self(5308);
    /// `OpHitObjectSetShaderBindingTableRecordIndexEXT`.
    pub const HIT_OBJECT_SET_SHADER_BINDING_TABLE_RECORD_INDEX_EXT: Self = Self(5309);
    /// `OpHitObjectReorderExecuteShaderEXT`.
    pub const HIT_OBJECT_REORDER_EXECUTE_SHADER_EXT: Self = Self(5310);
    /// `OpHitObjectTraceReorderExecuteEXT`.
    pub const HIT_OBJECT_TRACE_REORDER_EXECUTE_EXT: Self = Self(5311);
    /// `OpHitObjectTraceMotionReorderExecuteEXT`.
    pub const HIT_OBJECT_TRACE_MOTION_REORDER_EXECUTE_EXT: Self = Self(5312);
    /// `OpTypeHitObjectEXT`.
    pub const TYPE_HIT_OBJECT_EXT: Self = Self(5313);
    /// `OpReorderThreadWithHintEXT`.
    pub const REORDER_THREAD_WITH_HINT_EXT: Self = Self(5314);
    /// `OpReorderThreadWithHitObjectEXT`.
    pub const REORDER_THREAD_WITH_HIT_OBJECT_EXT: Self = Self(5315);
    /// `OpHitObjectTraceRayEXT`.
    pub const HIT_OBJECT_TRACE_RAY_EXT: Self = Self(5316);
    /// `OpHitObjectTraceRayMotionEXT`.
    pub const HIT_OBJECT_TRACE_RAY_MOTION_EXT: Self = Self(5317);
    /// `OpHitObjectRecordEmptyEXT`.
    pub const HIT_OBJECT_RECORD_EMPTY_EXT: Self = Self(5318);
    /// `OpHitObjectExecuteShaderEXT`.
    pub const HIT_OBJECT_EXECUTE_SHADER_EXT: Self = Self(5319);
    /// `OpHitObjectGetCurrentTimeEXT`.
    pub const HIT_OBJECT_GET_CURRENT_TIME_EXT: Self = Self(5320);
    /// `OpHitObjectGetAttributesEXT`.
    pub const HIT_OBJECT_GET_ATTRIBUTES_EXT: Self = Self(5321);
    /// `OpHitObjectGetHitKindEXT`.
    pub const HIT_OBJECT_GET_HIT_KIND_EXT: Self = Self(5322);
    /// `OpHitObjectGetPrimitiveIndexEXT`.
    pub const HIT_OBJECT_GET_PRIMITIVE_INDEX_EXT: Self = Self(5323);
    /// `OpHitObjectGetGeometryIndexEXT`.
    pub const HIT_OBJECT_GET_GEOMETRY_INDEX_EXT: Self = Self(5324);
    /// `OpHitObjectGetInstanceIdEXT`.
    pub const HIT_OBJECT_GET_INSTANCE_ID_EXT: Self = Self(5325);
    /// `OpHitObjectGetInstanceCustomIndexEXT`.
    pub const HIT_OBJECT_GET_INSTANCE_CUSTOM_INDEX_EXT: Self = Self(5326);
    /// `OpHitObjectGetObjectRayOriginEXT`.
    pub const HIT_OBJECT_GET_OBJECT_RAY_ORIGIN_EXT: Self = Self(5327);
    /// `OpHitObjectGetObjectRayDirectionEXT`.
    pub const HIT_OBJECT_GET_OBJECT_RAY_DIRECTION_EXT: Self = Self(5328);
    /// `OpHitObjectGetWorldRayDirectionEXT`.
    pub const HIT_OBJECT_GET_WORLD_RAY_DIRECTION_EXT: Self = Self(5329);
    /// `OpHitObjectGetWorldRayOriginEXT`.
    pub const HIT_OBJECT_GET_WORLD_RAY_ORIGIN_EXT: Self = Self(5330);
    /// `OpHitObjectGetObjectToWorldEXT`.
    pub const HIT_OBJECT_GET_OBJECT_TO_WORLD_EXT: Self = Self(5331);
    /// `OpHitObjectGetWorldToObjectEXT`.
    pub const HIT_OBJECT_GET_WORLD_TO_OBJECT_EXT: Self = Self(5332);
    /// `OpHitObjectGetRayTMaxEXT`.
    pub const HIT_OBJECT_GET_RAY_T_MAX_EXT: Self = Self(5333);
    /// `OpReportIntersectionKHR`.
    pub const REPORT_INTERSECTION_KHR: Self = Self(5334);
    /// `OpRayQueryGetIntersectionTriangleVertexPositionsKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_TRIANGLE_VERTEX_POSITIONS_KHR: Self = Self(5340);
    /// `OpTypeAccelerationStructureKHR`.
    pub const TYPE_ACCELERATION_STRUCTURE_KHR: Self = Self(5341);
    /// `OpHitObjectGetRayTMinEXT`.
    pub const HIT_OBJECT_GET_RAY_T_MIN_EXT: Self = Self(5347);
    /// `OpHitObjectGetShaderBindingTableRecordIndexEXT`.
    pub const HIT_OBJECT_GET_SHADER_BINDING_TABLE_RECORD_INDEX_EXT: Self = Self(5348);
    /// `OpHitObjectGetShaderRecordBufferHandleEXT`.
    pub const HIT_OBJECT_GET_SHADER_RECORD_BUFFER_HANDLE_EXT: Self = Self(5349);
    /// `OpHitObjectIsEmptyEXT`.
    pub const HIT_OBJECT_IS_EMPTY_EXT: Self = Self(5350);
    /// `OpHitObjectIsHitEXT`.
    pub const HIT_OBJECT_IS_HIT_EXT: Self = Self(5351);
    /// `OpHitObjectIsMissEXT`.
    pub const HIT_OBJECT_IS_MISS_EXT: Self = Self(5352);
    /// `OpCooperativeMatrixGetCoordinateEXT`.
    pub const COOPERATIVE_MATRIX_GET_COORDINATE_EXT: Self = Self(5363);
    /// `OpBeginInvocationInterlockEXT`.
    pub const BEGIN_INVOCATION_INTERLOCK_EXT: Self = Self(5364);
    /// `OpEndInvocationInterlockEXT`.
    pub const END_INVOCATION_INTERLOCK_EXT: Self = Self(5365);
    /// `OpCooperativeMatrixReduceEXT`.
    pub const COOPERATIVE_MATRIX_REDUCE_EXT: Self = Self(5366);
    /// `OpCooperativeMatrixPerElementOpEXT`.
    pub const COOPERATIVE_MATRIX_PER_ELEMENT_OP_EXT: Self = Self(5369);
    /// `OpDemoteToHelperInvocation`.
    pub const DEMOTE_TO_HELPER_INVOCATION: Self = Self(5380);
    /// `OpIsHelperInvocationEXT`.
    pub const IS_HELPER_INVOCATION_EXT: Self = Self(5381);
    /// `OpAtomicFMinEXT`.
    pub const ATOMIC_F_MIN_EXT: Self = Self(5614);
    /// `OpAtomicFMaxEXT`.
    pub const ATOMIC_F_MAX_EXT: Self = Self(5615);
    /// `OpAssumeTrueKHR`.
    pub const ASSUME_TRUE_KHR: Self = Self(5630);
    /// `OpExpectKHR`.
    pub const EXPECT_KHR: Self = Self(5631);
    /// `OpDecorateString`.
    pub const DECORATE_STRING: Self = Self(5632);
    /// `OpMemberDecorateString`.
    pub const MEMBER_DECORATE_STRING: Self = Self(5633);
    /// `OpRayQueryGetRayTMinKHR`.
    pub const RAY_QUERY_GET_RAY_T_MIN_KHR: Self = Self(6016);
    /// `OpRayQueryGetRayFlagsKHR`.
    pub const RAY_QUERY_GET_RAY_FLAGS_KHR: Self = Self(6017);
    /// `OpRayQueryGetIntersectionTKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_TKHR: Self = Self(6018);
    /// `OpRayQueryGetIntersectionInstanceCustomIndexKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_INSTANCE_CUSTOM_INDEX_KHR: Self = Self(6019);
    /// `OpRayQueryGetIntersectionInstanceIdKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_INSTANCE_ID_KHR: Self = Self(6020);
    /// `OpRayQueryGetIntersectionInstanceShaderBindingTableRecordOffsetKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_INSTANCE_SHADER_BINDING_TABLE_RECORD_OFFSET_KHR: Self = Self(6021);
    /// `OpRayQueryGetIntersectionGeometryIndexKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_GEOMETRY_INDEX_KHR: Self = Self(6022);
    /// `OpRayQueryGetIntersectionPrimitiveIndexKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_PRIMITIVE_INDEX_KHR: Self = Self(6023);
    /// `OpRayQueryGetIntersectionBarycentricsKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_BARYCENTRICS_KHR: Self = Self(6024);
    /// `OpRayQueryGetIntersectionFrontFaceKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_FRONT_FACE_KHR: Self = Self(6025);
    /// `OpRayQueryGetIntersectionCandidateAABBOpaqueKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_CANDIDATE_AABB_OPAQUE_KHR: Self = Self(6026);
    /// `OpRayQueryGetIntersectionObjectRayDirectionKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_OBJECT_RAY_DIRECTION_KHR: Self = Self(6027);
    /// `OpRayQueryGetIntersectionObjectRayOriginKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_OBJECT_RAY_ORIGIN_KHR: Self = Self(6028);
    /// `OpRayQueryGetWorldRayDirectionKHR`.
    pub const RAY_QUERY_GET_WORLD_RAY_DIRECTION_KHR: Self = Self(6029);
    /// `OpRayQueryGetWorldRayOriginKHR`.
    pub const RAY_QUERY_GET_WORLD_RAY_ORIGIN_KHR: Self = Self(6030);
    /// `OpRayQueryGetIntersectionObjectToWorldKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_OBJECT_TO_WORLD_KHR: Self = Self(6031);
    /// `OpRayQueryGetIntersectionWorldToObjectKHR`.
    pub const RAY_QUERY_GET_INTERSECTION_WORLD_TO_OBJECT_KHR: Self = Self(6032);
    /// `OpAtomicFAddEXT`.
    pub const ATOMIC_F_ADD_EXT: Self = Self(6035);
    /// `OpControlBarrierArriveEXT`.
    pub const CONTROL_BARRIER_ARRIVE_EXT: Self = Self(6142);
    /// `OpControlBarrierWaitEXT`.
    pub const CONTROL_BARRIER_WAIT_EXT: Self = Self(6143);
    /// `OpArithmeticFenceEXT`.
    pub const ARITHMETIC_FENCE_EXT: Self = Self(6145);
    /// `OpGroupIMulKHR`.
    pub const GROUP_I_MUL_KHR: Self = Self(6401);
    /// `OpGroupFMulKHR`.
    pub const GROUP_F_MUL_KHR: Self = Self(6402);
    /// `OpGroupBitwiseAndKHR`.
    pub const GROUP_BITWISE_AND_KHR: Self = Self(6403);
    /// `OpGroupBitwiseOrKHR`.
    pub const GROUP_BITWISE_OR_KHR: Self = Self(6404);
    /// `OpGroupBitwiseXorKHR`.
    pub const GROUP_BITWISE_XOR_KHR: Self = Self(6405);
    /// `OpGroupLogicalAndKHR`.
    pub const GROUP_LOGICAL_AND_KHR: Self = Self(6406);
    /// `OpGroupLogicalOrKHR`.
    pub const GROUP_LOGICAL_OR_KHR: Self = Self(6407);
    /// `OpGroupLogicalXorKHR`.
    pub const GROUP_LOGICAL_XOR_KHR: Self = Self(6408);
}

pub mod enumerant {
    //! The values of the grammar's enumerated kinds, by name: a type per kind,
    //! holding a value or, for a set of bits, a mask.

    /// A `AccessQualifier` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct AccessQualifier(pub u32);

    impl AccessQualifier {
        /// `ReadOnly`.
        pub const READ_ONLY: Self = Self(0x0);
        /// `WriteOnly`.
        pub const WRITE_ONLY: Self = Self(0x1);
        /// `ReadWrite`.
        pub const READ_WRITE: Self = Self(0x2);
    }

    /// A `AddressingModel` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct AddressingModel(pub u32);

    impl AddressingModel {
        /// `Logical`.
        pub const LOGICAL: Self = Self(0x0);
        /// `Physical32`.
        pub const PHYSICAL32: Self = Self(0x1);
        /// `Physical64`.
        pub const PHYSICAL64: Self = Self(0x2);
        /// `PhysicalStorageBuffer64`.
        pub const PHYSICAL_STORAGE_BUFFER64: Self = Self(0x14e4);
    }

    /// A `BuiltIn` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct BuiltIn(pub u32);

    impl BuiltIn {
        /// `Position`.
        pub const POSITION: Self = Self(0x0);
        /// `PointSize`.
        pub const POINT_SIZE: Self = Self(0x1);
        /// `ClipDistance`.
        pub const CLIP_DISTANCE: Self = Self(0x3);
        /// `CullDistance`.
        pub const CULL_DISTANCE: Self = Self(0x4);
        /// `VertexId`.
        pub const VERTEX_ID: Self = Self(0x5);
        /// `InstanceId`.
        pub const INSTANCE_ID: Self = Self(0x6);
        /// `PrimitiveId`.
        pub const PRIMITIVE_ID: Self = Self(0x7);
        /// `InvocationId`.
        pub const INVOCATION_ID: Self = Self(0x8);
        /// `Layer`.
        pub const LAYER: Self = Self(0x9);
        /// `ViewportIndex`.
        pub const VIEWPORT_INDEX: Self = Self(0xa);
        /// `TessLevelOuter`.
        pub const TESS_LEVEL_OUTER: Self = Self(0xb);
        /// `TessLevelInner`.
        pub const TESS_LEVEL_INNER: Self = Self(0xc);
        /// `TessCoord`.
        pub const TESS_COORD: Self = Self(0xd);
        /// `PatchVertices`.
        pub const PATCH_VERTICES: Self = Self(0xe);
        /// `FragCoord`.
        pub const FRAG_COORD: Self = Self(0xf);
        /// `PointCoord`.
        pub const POINT_COORD: Self = Self(0x10);
        /// `FrontFacing`.
        pub const FRONT_FACING: Self = Self(0x11);
        /// `SampleId`.
        pub const SAMPLE_ID: Self = Self(0x12);
        /// `SamplePosition`.
        pub const SAMPLE_POSITION: Self = Self(0x13);
        /// `SampleMask`.
        pub const SAMPLE_MASK: Self = Self(0x14);
        /// `FragDepth`.
        pub const FRAG_DEPTH: Self = Self(0x16);
        /// `HelperInvocation`.
        pub const HELPER_INVOCATION: Self = Self(0x17);
        /// `NumWorkgroups`.
        pub const NUM_WORKGROUPS: Self = Self(0x18);
        /// `WorkgroupSize`.
        pub const WORKGROUP_SIZE: Self = Self(0x19);
        /// `WorkgroupId`.
        pub const WORKGROUP_ID: Self = Self(0x1a);
        /// `LocalInvocationId`.
        pub const LOCAL_INVOCATION_ID: Self = Self(0x1b);
        /// `GlobalInvocationId`.
        pub const GLOBAL_INVOCATION_ID: Self = Self(0x1c);
        /// `LocalInvocationIndex`.
        pub const LOCAL_INVOCATION_INDEX: Self = Self(0x1d);
        /// `WorkDim`.
        pub const WORK_DIM: Self = Self(0x1e);
        /// `GlobalSize`.
        pub const GLOBAL_SIZE: Self = Self(0x1f);
        /// `EnqueuedWorkgroupSize`.
        pub const ENQUEUED_WORKGROUP_SIZE: Self = Self(0x20);
        /// `GlobalOffset`.
        pub const GLOBAL_OFFSET: Self = Self(0x21);
        /// `GlobalLinearId`.
        pub const GLOBAL_LINEAR_ID: Self = Self(0x22);
        /// `SubgroupSize`.
        pub const SUBGROUP_SIZE: Self = Self(0x24);
        /// `SubgroupMaxSize`.
        pub const SUBGROUP_MAX_SIZE: Self = Self(0x25);
        /// `NumSubgroups`.
        pub const NUM_SUBGROUPS: Self = Self(0x26);
        /// `NumEnqueuedSubgroups`.
        pub const NUM_ENQUEUED_SUBGROUPS: Self = Self(0x27);
        /// `SubgroupId`.
        pub const SUBGROUP_ID: Self = Self(0x28);
        /// `SubgroupLocalInvocationId`.
        pub const SUBGROUP_LOCAL_INVOCATION_ID: Self = Self(0x29);
        /// `VertexIndex`.
        pub const VERTEX_INDEX: Self = Self(0x2a);
        /// `InstanceIndex`.
        pub const INSTANCE_INDEX: Self = Self(0x2b);
        /// `SubgroupEqMask`.
        pub const SUBGROUP_EQ_MASK: Self = Self(0x1140);
        /// `SubgroupGeMask`.
        pub const SUBGROUP_GE_MASK: Self = Self(0x1141);
        /// `SubgroupGtMask`.
        pub const SUBGROUP_GT_MASK: Self = Self(0x1142);
        /// `SubgroupLeMask`.
        pub const SUBGROUP_LE_MASK: Self = Self(0x1143);
        /// `SubgroupLtMask`.
        pub const SUBGROUP_LT_MASK: Self = Self(0x1144);
        /// `BaseVertex`.
        pub const BASE_VERTEX: Self = Self(0x1148);
        /// `BaseInstance`.
        pub const BASE_INSTANCE: Self = Self(0x1149);
        /// `DrawIndex`.
        pub const DRAW_INDEX: Self = Self(0x114a);
        /// `PrimitiveShadingRateKHR`.
        pub const PRIMITIVE_SHADING_RATE_KHR: Self = Self(0x1150);
        /// `DeviceIndex`.
        pub const DEVICE_INDEX: Self = Self(0x1156);
        /// `ViewIndex`.
        pub const VIEW_INDEX: Self = Self(0x1158);
        /// `ShadingRateKHR`.
        pub const SHADING_RATE_KHR: Self = Self(0x115c);
        /// `FragStencilRefEXT`.
        pub const FRAG_STENCIL_REF_EXT: Self = Self(0x1396);
        /// `SamplerHeapEXT`.
        pub const SAMPLER_HEAP_EXT: Self = Self(0x1402);
        /// `ResourceHeapEXT`.
        pub const RESOURCE_HEAP_EXT: Self = Self(0x1403);
        /// `FullyCoveredEXT`.
        pub const FULLY_COVERED_EXT: Self = Self(0x1490);
        /// `BaryCoordKHR`.
        pub const BARY_COORD_KHR: Self = Self(0x14a6);
        /// `BaryCoordNoPerspKHR`.
        pub const BARY_COORD_NO_PERSP_KHR: Self = Self(0x14a7);
        /// `FragSizeEXT`.
        pub const FRAG_SIZE_EXT: Self = Self(0x14ac);
        /// `FragInvocationCountEXT`.
        pub const FRAG_INVOCATION_COUNT_EXT: Self = Self(0x14ad);
        /// `PrimitivePointIndicesEXT`.
        pub const PRIMITIVE_POINT_INDICES_EXT: Self = Self(0x14ae);
        /// `PrimitiveLineIndicesEXT`.
        pub const PRIMITIVE_LINE_INDICES_EXT: Self = Self(0x14af);
        /// `PrimitiveTriangleIndicesEXT`.
        pub const PRIMITIVE_TRIANGLE_INDICES_EXT: Self = Self(0x14b0);
        /// `CullPrimitiveEXT`.
        pub const CULL_PRIMITIVE_EXT: Self = Self(0x14b3);
        /// `LaunchIdKHR`.
        pub const LAUNCH_ID_KHR: Self = Self(0x14c7);
        /// `LaunchSizeKHR`.
        pub const LAUNCH_SIZE_KHR: Self = Self(0x14c8);
        /// `WorldRayOriginKHR`.
        pub const WORLD_RAY_ORIGIN_KHR: Self = Self(0x14c9);
        /// `WorldRayDirectionKHR`.
        pub const WORLD_RAY_DIRECTION_KHR: Self = Self(0x14ca);
        /// `ObjectRayOriginKHR`.
        pub const OBJECT_RAY_ORIGIN_KHR: Self = Self(0x14cb);
        /// `ObjectRayDirectionKHR`.
        pub const OBJECT_RAY_DIRECTION_KHR: Self = Self(0x14cc);
        /// `RayTminKHR`.
        pub const RAY_TMIN_KHR: Self = Self(0x14cd);
        /// `RayTmaxKHR`.
        pub const RAY_TMAX_KHR: Self = Self(0x14ce);
        /// `InstanceCustomIndexKHR`.
        pub const INSTANCE_CUSTOM_INDEX_KHR: Self = Self(0x14cf);
        /// `ObjectToWorldKHR`.
        pub const OBJECT_TO_WORLD_KHR: Self = Self(0x14d2);
        /// `WorldToObjectKHR`.
        pub const WORLD_TO_OBJECT_KHR: Self = Self(0x14d3);
        /// `HitKindKHR`.
        pub const HIT_KIND_KHR: Self = Self(0x14d5);
        /// `HitTriangleVertexPositionsKHR`.
        pub const HIT_TRIANGLE_VERTEX_POSITIONS_KHR: Self = Self(0x14d7);
        /// `IncomingRayFlagsKHR`.
        pub const INCOMING_RAY_FLAGS_KHR: Self = Self(0x14e7);
        /// `RayGeometryIndexKHR`.
        pub const RAY_GEOMETRY_INDEX_KHR: Self = Self(0x14e8);
        /// `CullMaskKHR`.
        pub const CULL_MASK_KHR: Self = Self(0x1785);
    }

    /// A `Capability` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct Capability(pub u32);

    impl Capability {
        /// `Matrix`.
        pub const MATRIX: Self = Self(0x0);
        /// `Shader`.
        pub const SHADER: Self = Self(0x1);
        /// `Geometry`.
        pub const GEOMETRY: Self = Self(0x2);
        /// `Tessellation`.
        pub const TESSELLATION: Self = Self(0x3);
        /// `Addresses`.
        pub const ADDRESSES: Self = Self(0x4);
        /// `Linkage`.
        pub const LINKAGE: Self = Self(0x5);
        /// `Kernel`.
        pub const KERNEL: Self = Self(0x6);
        /// `Vector16`.
        pub const VECTOR16: Self = Self(0x7);
        /// `Float16Buffer`.
        pub const FLOAT16_BUFFER: Self = Self(0x8);
        /// `Float16`.
        pub const FLOAT16: Self = Self(0x9);
        /// `Float64`.
        pub const FLOAT64: Self = Self(0xa);
        /// `Int64`.
        pub const INT64: Self = Self(0xb);
        /// `Int64Atomics`.
        pub const INT64_ATOMICS: Self = Self(0xc);
        /// `ImageBasic`.
        pub const IMAGE_BASIC: Self = Self(0xd);
        /// `ImageReadWrite`.
        pub const IMAGE_READ_WRITE: Self = Self(0xe);
        /// `ImageMipmap`.
        pub const IMAGE_MIPMAP: Self = Self(0xf);
        /// `Pipes`.
        pub const PIPES: Self = Self(0x11);
        /// `Groups`.
        pub const GROUPS: Self = Self(0x12);
        /// `DeviceEnqueue`.
        pub const DEVICE_ENQUEUE: Self = Self(0x13);
        /// `LiteralSampler`.
        pub const LITERAL_SAMPLER: Self = Self(0x14);
        /// `AtomicStorage`.
        pub const ATOMIC_STORAGE: Self = Self(0x15);
        /// `Int16`.
        pub const INT16: Self = Self(0x16);
        /// `TessellationPointSize`.
        pub const TESSELLATION_POINT_SIZE: Self = Self(0x17);
        /// `GeometryPointSize`.
        pub const GEOMETRY_POINT_SIZE: Self = Self(0x18);
        /// `ImageGatherExtended`.
        pub const IMAGE_GATHER_EXTENDED: Self = Self(0x19);
        /// `StorageImageMultisample`.
        pub const STORAGE_IMAGE_MULTISAMPLE: Self = Self(0x1b);
        /// `UniformBufferArrayDynamicIndexing`.
        pub const UNIFORM_BUFFER_ARRAY_DYNAMIC_INDEXING: Self = Self(0x1c);
        /// `SampledImageArrayDynamicIndexing`.
        pub const SAMPLED_IMAGE_ARRAY_DYNAMIC_INDEXING: Self = Self(0x1d);
        /// `StorageBufferArrayDynamicIndexing`.
        pub const STORAGE_BUFFER_ARRAY_DYNAMIC_INDEXING: Self = Self(0x1e);
        /// `StorageImageArrayDynamicIndexing`.
        pub const STORAGE_IMAGE_ARRAY_DYNAMIC_INDEXING: Self = Self(0x1f);
        /// `ClipDistance`.
        pub const CLIP_DISTANCE: Self = Self(0x20);
        /// `CullDistance`.
        pub const CULL_DISTANCE: Self = Self(0x21);
        /// `ImageCubeArray`.
        pub const IMAGE_CUBE_ARRAY: Self = Self(0x22);
        /// `SampleRateShading`.
        pub const SAMPLE_RATE_SHADING: Self = Self(0x23);
        /// `ImageRect`.
        pub const IMAGE_RECT: Self = Self(0x24);
        /// `SampledRect`.
        pub const SAMPLED_RECT: Self = Self(0x25);
        /// `GenericPointer`.
        pub const GENERIC_POINTER: Self = Self(0x26);
        /// `Int8`.
        pub const INT8: Self = Self(0x27);
        /// `InputAttachment`.
        pub const INPUT_ATTACHMENT: Self = Self(0x28);
        /// `SparseResidency`.
        pub const SPARSE_RESIDENCY: Self = Self(0x29);
        /// `MinLod`.
        pub const MIN_LOD: Self = Self(0x2a);
        /// `Sampled1D`.
        pub const SAMPLED1D: Self = Self(0x2b);
        /// `Image1D`.
        pub const IMAGE1D: Self = Self(0x2c);
        /// `SampledCubeArray`.
        pub const SAMPLED_CUBE_ARRAY: Self = Self(0x2d);
        /// `SampledBuffer`.
        pub const SAMPLED_BUFFER: Self = Self(0x2e);
        /// `ImageBuffer`.
        pub const IMAGE_BUFFER: Self = Self(0x2f);
        /// `ImageMSArray`.
        pub const IMAGE_MS_ARRAY: Self = Self(0x30);
        /// `StorageImageExtendedFormats`.
        pub const STORAGE_IMAGE_EXTENDED_FORMATS: Self = Self(0x31);
        /// `ImageQuery`.
        pub const IMAGE_QUERY: Self = Self(0x32);
        /// `DerivativeControl`.
        pub const DERIVATIVE_CONTROL: Self = Self(0x33);
        /// `InterpolationFunction`.
        pub const INTERPOLATION_FUNCTION: Self = Self(0x34);
        /// `TransformFeedback`.
        pub const TRANSFORM_FEEDBACK: Self = Self(0x35);
        /// `GeometryStreams`.
        pub const GEOMETRY_STREAMS: Self = Self(0x36);
        /// `StorageImageReadWithoutFormat`.
        pub const STORAGE_IMAGE_READ_WITHOUT_FORMAT: Self = Self(0x37);
        /// `StorageImageWriteWithoutFormat`.
        pub const STORAGE_IMAGE_WRITE_WITHOUT_FORMAT: Self = Self(0x38);
        /// `MultiViewport`.
        pub const MULTI_VIEWPORT: Self = Self(0x39);
        /// `SubgroupDispatch`.
        pub const SUBGROUP_DISPATCH: Self = Self(0x3a);
        /// `NamedBarrier`.
        pub const NAMED_BARRIER: Self = Self(0x3b);
        /// `PipeStorage`.
        pub const PIPE_STORAGE: Self = Self(0x3c);
        /// `GroupNonUniform`.
        pub const GROUP_NON_UNIFORM: Self = Self(0x3d);
        /// `GroupNonUniformVote`.
        pub const GROUP_NON_UNIFORM_VOTE: Self = Self(0x3e);
        /// `GroupNonUniformArithmetic`.
        pub const GROUP_NON_UNIFORM_ARITHMETIC: Self = Self(0x3f);
        /// `GroupNonUniformBallot`.
        pub const GROUP_NON_UNIFORM_BALLOT: Self = Self(0x40);
        /// `GroupNonUniformShuffle`.
        pub const GROUP_NON_UNIFORM_SHUFFLE: Self = Self(0x41);
        /// `GroupNonUniformShuffleRelative`.
        pub const GROUP_NON_UNIFORM_SHUFFLE_RELATIVE: Self = Self(0x42);
        /// `GroupNonUniformClustered`.
        pub const GROUP_NON_UNIFORM_CLUSTERED: Self = Self(0x43);
        /// `GroupNonUniformQuad`.
        pub const GROUP_NON_UNIFORM_QUAD: Self = Self(0x44);
        /// `ShaderLayer`.
        pub const SHADER_LAYER: Self = Self(0x45);
        /// `ShaderViewportIndex`.
        pub const SHADER_VIEWPORT_INDEX: Self = Self(0x46);
        /// `UniformDecoration`.
        pub const UNIFORM_DECORATION: Self = Self(0x47);
        /// `TileImageColorReadAccessEXT`.
        pub const TILE_IMAGE_COLOR_READ_ACCESS_EXT: Self = Self(0x1046);
        /// `TileImageDepthReadAccessEXT`.
        pub const TILE_IMAGE_DEPTH_READ_ACCESS_EXT: Self = Self(0x1047);
        /// `TileImageStencilReadAccessEXT`.
        pub const TILE_IMAGE_STENCIL_READ_ACCESS_EXT: Self = Self(0x1048);
        /// `Float8EXT`.
        pub const FLOAT8_EXT: Self = Self(0x1074);
        /// `Float8CooperativeMatrixEXT`.
        pub const FLOAT8_COOPERATIVE_MATRIX_EXT: Self = Self(0x1075);
        /// `Float6EXT`.
        pub const FLOAT6_EXT: Self = Self(0x1084);
        /// `Float4EXT`.
        pub const FLOAT4_EXT: Self = Self(0x1085);
        /// `Float8UnsignedE8M0EXT`.
        pub const FLOAT8_UNSIGNED_E8_M0_EXT: Self = Self(0x1086);
        /// `MXInt8EXT`.
        pub const MX_INT8_EXT: Self = Self(0x1087);
        /// `BitcastExtractEXT`.
        pub const BITCAST_EXTRACT_EXT: Self = Self(0x1088);
        /// `FragmentShadingRateKHR`.
        pub const FRAGMENT_SHADING_RATE_KHR: Self = Self(0x1146);
        /// `SubgroupBallotKHR`.
        pub const SUBGROUP_BALLOT_KHR: Self = Self(0x1147);
        /// `DrawParameters`.
        pub const DRAW_PARAMETERS: Self = Self(0x114b);
        /// `WorkgroupMemoryExplicitLayoutKHR`.
        pub const WORKGROUP_MEMORY_EXPLICIT_LAYOUT_KHR: Self = Self(0x114c);
        /// `WorkgroupMemoryExplicitLayout8BitAccessKHR`.
        pub const WORKGROUP_MEMORY_EXPLICIT_LAYOUT8_BIT_ACCESS_KHR: Self = Self(0x114d);
        /// `WorkgroupMemoryExplicitLayout16BitAccessKHR`.
        pub const WORKGROUP_MEMORY_EXPLICIT_LAYOUT16_BIT_ACCESS_KHR: Self = Self(0x114e);
        /// `SubgroupVoteKHR`.
        pub const SUBGROUP_VOTE_KHR: Self = Self(0x114f);
        /// `StorageBuffer16BitAccess`.
        pub const STORAGE_BUFFER16_BIT_ACCESS: Self = Self(0x1151);
        /// `UniformAndStorageBuffer16BitAccess`.
        pub const UNIFORM_AND_STORAGE_BUFFER16_BIT_ACCESS: Self = Self(0x1152);
        /// `StoragePushConstant16`.
        pub const STORAGE_PUSH_CONSTANT16: Self = Self(0x1153);
        /// `StorageInputOutput16`.
        pub const STORAGE_INPUT_OUTPUT16: Self = Self(0x1154);
        /// `DeviceGroup`.
        pub const DEVICE_GROUP: Self = Self(0x1155);
        /// `MultiView`.
        pub const MULTI_VIEW: Self = Self(0x1157);
        /// `VariablePointersStorageBuffer`.
        pub const VARIABLE_POINTERS_STORAGE_BUFFER: Self = Self(0x1159);
        /// `VariablePointers`.
        pub const VARIABLE_POINTERS: Self = Self(0x115a);
        /// `AtomicStorageOps`.
        pub const ATOMIC_STORAGE_OPS: Self = Self(0x115d);
        /// `SampleMaskPostDepthCoverage`.
        pub const SAMPLE_MASK_POST_DEPTH_COVERAGE: Self = Self(0x115f);
        /// `StorageBuffer8BitAccess`.
        pub const STORAGE_BUFFER8_BIT_ACCESS: Self = Self(0x1160);
        /// `UniformAndStorageBuffer8BitAccess`.
        pub const UNIFORM_AND_STORAGE_BUFFER8_BIT_ACCESS: Self = Self(0x1161);
        /// `StoragePushConstant8`.
        pub const STORAGE_PUSH_CONSTANT8: Self = Self(0x1162);
        /// `DenormPreserve`.
        pub const DENORM_PRESERVE: Self = Self(0x1170);
        /// `DenormFlushToZero`.
        pub const DENORM_FLUSH_TO_ZERO: Self = Self(0x1171);
        /// `SignedZeroInfNanPreserve`.
        pub const SIGNED_ZERO_INF_NAN_PRESERVE: Self = Self(0x1172);
        /// `RoundingModeRTE`.
        pub const ROUNDING_MODE_RTE: Self = Self(0x1173);
        /// `RoundingModeRTZ`.
        pub const ROUNDING_MODE_RTZ: Self = Self(0x1174);
        /// `RayQueryProvisionalKHR`.
        pub const RAY_QUERY_PROVISIONAL_KHR: Self = Self(0x1177);
        /// `RayQueryKHR`.
        pub const RAY_QUERY_KHR: Self = Self(0x1178);
        /// `UntypedPointersKHR`.
        pub const UNTYPED_POINTERS_KHR: Self = Self(0x1179);
        /// `RayTraversalPrimitiveCullingKHR`.
        pub const RAY_TRAVERSAL_PRIMITIVE_CULLING_KHR: Self = Self(0x117e);
        /// `RayTracingKHR`.
        pub const RAY_TRACING_KHR: Self = Self(0x117f);
        /// `StencilExportEXT`.
        pub const STENCIL_EXPORT_EXT: Self = Self(0x1395);
        /// `Int64ImageEXT`.
        pub const INT64_IMAGE_EXT: Self = Self(0x1398);
        /// `ShaderClockKHR`.
        pub const SHADER_CLOCK_KHR: Self = Self(0x13bf);
        /// `QuadControlKHR`.
        pub const QUAD_CONTROL_KHR: Self = Self(0x13df);
        /// `BFloat16TypeKHR`.
        pub const B_FLOAT16_TYPE_KHR: Self = Self(0x13fc);
        /// `BFloat16DotProductKHR`.
        pub const B_FLOAT16_DOT_PRODUCT_KHR: Self = Self(0x13fd);
        /// `BFloat16CooperativeMatrixKHR`.
        pub const B_FLOAT16_COOPERATIVE_MATRIX_KHR: Self = Self(0x13fe);
        /// `AbortKHR`.
        pub const ABORT_KHR: Self = Self(0x1400);
        /// `DescriptorHeapEXT`.
        pub const DESCRIPTOR_HEAP_EXT: Self = Self(0x1408);
        /// `ConstantDataKHR`.
        pub const CONSTANT_DATA_KHR: Self = Self(0x141a);
        /// `ShaderViewportIndexLayerEXT`.
        pub const SHADER_VIEWPORT_INDEX_LAYER_EXT: Self = Self(0x1486);
        /// `FragmentFullyCoveredEXT`.
        pub const FRAGMENT_FULLY_COVERED_EXT: Self = Self(0x1491);
        /// `MeshShadingEXT`.
        pub const MESH_SHADING_EXT: Self = Self(0x14a3);
        /// `FragmentBarycentricKHR`.
        pub const FRAGMENT_BARYCENTRIC_KHR: Self = Self(0x14a4);
        /// `ComputeDerivativeGroupQuadsKHR`.
        pub const COMPUTE_DERIVATIVE_GROUP_QUADS_KHR: Self = Self(0x14a8);
        /// `FragmentDensityEXT`.
        pub const FRAGMENT_DENSITY_EXT: Self = Self(0x14ab);
        /// `GroupNonUniformPartitionedEXT`.
        pub const GROUP_NON_UNIFORM_PARTITIONED_EXT: Self = Self(0x14b1);
        /// `ShaderNonUniform`.
        pub const SHADER_NON_UNIFORM: Self = Self(0x14b5);
        /// `RuntimeDescriptorArray`.
        pub const RUNTIME_DESCRIPTOR_ARRAY: Self = Self(0x14b6);
        /// `InputAttachmentArrayDynamicIndexing`.
        pub const INPUT_ATTACHMENT_ARRAY_DYNAMIC_INDEXING: Self = Self(0x14b7);
        /// `UniformTexelBufferArrayDynamicIndexing`.
        pub const UNIFORM_TEXEL_BUFFER_ARRAY_DYNAMIC_INDEXING: Self = Self(0x14b8);
        /// `StorageTexelBufferArrayDynamicIndexing`.
        pub const STORAGE_TEXEL_BUFFER_ARRAY_DYNAMIC_INDEXING: Self = Self(0x14b9);
        /// `UniformBufferArrayNonUniformIndexing`.
        pub const UNIFORM_BUFFER_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14ba);
        /// `SampledImageArrayNonUniformIndexing`.
        pub const SAMPLED_IMAGE_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14bb);
        /// `StorageBufferArrayNonUniformIndexing`.
        pub const STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14bc);
        /// `StorageImageArrayNonUniformIndexing`.
        pub const STORAGE_IMAGE_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14bd);
        /// `InputAttachmentArrayNonUniformIndexing`.
        pub const INPUT_ATTACHMENT_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14be);
        /// `UniformTexelBufferArrayNonUniformIndexing`.
        pub const UNIFORM_TEXEL_BUFFER_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14bf);
        /// `StorageTexelBufferArrayNonUniformIndexing`.
        pub const STORAGE_TEXEL_BUFFER_ARRAY_NON_UNIFORM_INDEXING: Self = Self(0x14c0);
        /// `RayTracingPositionFetchKHR`.
        pub const RAY_TRACING_POSITION_FETCH_KHR: Self = Self(0x14d8);
        /// `VulkanMemoryModel`.
        pub const VULKAN_MEMORY_MODEL: Self = Self(0x14e1);
        /// `VulkanMemoryModelDeviceScope`.
        pub const VULKAN_MEMORY_MODEL_DEVICE_SCOPE: Self = Self(0x14e2);
        /// `PhysicalStorageBufferAddresses`.
        pub const PHYSICAL_STORAGE_BUFFER_ADDRESSES: Self = Self(0x14e3);
        /// `ComputeDerivativeGroupLinearKHR`.
        pub const COMPUTE_DERIVATIVE_GROUP_LINEAR_KHR: Self = Self(0x14e6);
        /// `RayTracingProvisionalKHR`.
        pub const RAY_TRACING_PROVISIONAL_KHR: Self = Self(0x14e9);
        /// `FragmentShaderSampleInterlockEXT`.
        pub const FRAGMENT_SHADER_SAMPLE_INTERLOCK_EXT: Self = Self(0x14f3);
        /// `FragmentShaderShadingRateInterlockEXT`.
        pub const FRAGMENT_SHADER_SHADING_RATE_INTERLOCK_EXT: Self = Self(0x14fc);
        /// `FragmentShaderPixelInterlockEXT`.
        pub const FRAGMENT_SHADER_PIXEL_INTERLOCK_EXT: Self = Self(0x1502);
        /// `DemoteToHelperInvocation`.
        pub const DEMOTE_TO_HELPER_INVOCATION: Self = Self(0x1503);
        /// `RayTracingOpacityMicromapKHR`.
        pub const RAY_TRACING_OPACITY_MICROMAP_KHR: Self = Self(0x1505);
        /// `ShaderInvocationReorderEXT`.
        pub const SHADER_INVOCATION_REORDER_EXT: Self = Self(0x150c);
        /// `RayQueryPositionFetchKHR`.
        pub const RAY_QUERY_POSITION_FETCH_KHR: Self = Self(0x150f);
        /// `LongVectorEXT`.
        pub const LONG_VECTOR_EXT: Self = Self(0x1531);
        /// `Shader64BitIndexingEXT`.
        pub const SHADER64_BIT_INDEXING_EXT: Self = Self(0x1532);
        /// `CooperativeMatrixConversionsEXT`.
        pub const COOPERATIVE_MATRIX_CONVERSIONS_EXT: Self = Self(0x1535);
        /// `CooperativeMatrixReductionsEXT`.
        pub const COOPERATIVE_MATRIX_REDUCTIONS_EXT: Self = Self(0x1536);
        /// `CooperativeMatrixPerElementOperationsEXT`.
        pub const COOPERATIVE_MATRIX_PER_ELEMENT_OPERATIONS_EXT: Self = Self(0x1538);
        /// `CooperativeMatrixGetCoordinateEXT`.
        pub const COOPERATIVE_MATRIX_GET_COORDINATE_EXT: Self = Self(0x153e);
        /// `AtomicFloat32MinMaxEXT`.
        pub const ATOMIC_FLOAT32_MIN_MAX_EXT: Self = Self(0x15ec);
        /// `AtomicFloat64MinMaxEXT`.
        pub const ATOMIC_FLOAT64_MIN_MAX_EXT: Self = Self(0x15ed);
        /// `AtomicFloat16MinMaxEXT`.
        pub const ATOMIC_FLOAT16_MIN_MAX_EXT: Self = Self(0x15f0);
        /// `ExpectAssumeKHR`.
        pub const EXPECT_ASSUME_KHR: Self = Self(0x15fd);
        /// `DotProductInputAll`.
        pub const DOT_PRODUCT_INPUT_ALL: Self = Self(0x1780);
        /// `DotProductInput4x8Bit`.
        pub const DOT_PRODUCT_INPUT4X8_BIT: Self = Self(0x1781);
        /// `DotProductInput4x8BitPacked`.
        pub const DOT_PRODUCT_INPUT4X8_BIT_PACKED: Self = Self(0x1782);
        /// `DotProduct`.
        pub const DOT_PRODUCT: Self = Self(0x1783);
        /// `RayCullMaskKHR`.
        pub const RAY_CULL_MASK_KHR: Self = Self(0x1784);
        /// `CooperativeMatrixKHR`.
        pub const COOPERATIVE_MATRIX_KHR: Self = Self(0x1786);
        /// `ReplicatedCompositesEXT`.
        pub const REPLICATED_COMPOSITES_EXT: Self = Self(0x1788);
        /// `BitInstructions`.
        pub const BIT_INSTRUCTIONS: Self = Self(0x1789);
        /// `GroupNonUniformRotateKHR`.
        pub const GROUP_NON_UNIFORM_ROTATE_KHR: Self = Self(0x178a);
        /// `FloatControls2`.
        pub const FLOAT_CONTROLS2: Self = Self(0x178d);
        /// `FMAKHR`.
        pub const FMAKHR: Self = Self(0x178e);
        /// `RayTracingOpacityMicromapExecutionModeKHR`.
        pub const RAY_TRACING_OPACITY_MICROMAP_EXECUTION_MODE_KHR: Self = Self(0x1790);
        /// `AtomicFloat32AddEXT`.
        pub const ATOMIC_FLOAT32_ADD_EXT: Self = Self(0x1791);
        /// `AtomicFloat64AddEXT`.
        pub const ATOMIC_FLOAT64_ADD_EXT: Self = Self(0x1792);
        /// `OptNoneEXT`.
        pub const OPT_NONE_EXT: Self = Self(0x17ce);
        /// `AtomicFloat16AddEXT`.
        pub const ATOMIC_FLOAT16_ADD_EXT: Self = Self(0x17cf);
        /// `SplitBarrierEXT`.
        pub const SPLIT_BARRIER_EXT: Self = Self(0x17fd);
        /// `ArithmeticFenceEXT`.
        pub const ARITHMETIC_FENCE_EXT: Self = Self(0x1800);
        /// `GroupUniformArithmeticKHR`.
        pub const GROUP_UNIFORM_ARITHMETIC_KHR: Self = Self(0x1900);
    }

    /// A `CooperativeMatrixOperands` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct CooperativeMatrixOperands(pub u32);

    impl CooperativeMatrixOperands {
        /// `NoneKHR`.
        pub const NONE_KHR: Self = Self(0x0);
        /// `MatrixASignedComponentsKHR`.
        pub const MATRIX_A_SIGNED_COMPONENTS_KHR: Self = Self(0x1);
        /// `MatrixBSignedComponentsKHR`.
        pub const MATRIX_B_SIGNED_COMPONENTS_KHR: Self = Self(0x2);
        /// `MatrixCSignedComponentsKHR`.
        pub const MATRIX_C_SIGNED_COMPONENTS_KHR: Self = Self(0x4);
        /// `MatrixResultSignedComponentsKHR`.
        pub const MATRIX_RESULT_SIGNED_COMPONENTS_KHR: Self = Self(0x8);
        /// `SaturatingAccumulationKHR`.
        pub const SATURATING_ACCUMULATION_KHR: Self = Self(0x10);
    }

    /// A `CooperativeMatrixReduce` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct CooperativeMatrixReduce(pub u32);

    impl CooperativeMatrixReduce {
        /// `Row`.
        pub const ROW: Self = Self(0x1);
        /// `Column`.
        pub const COLUMN: Self = Self(0x2);
        /// `2x2`.
        pub const COOPERATIVE_MATRIX_REDUCE_2X2: Self = Self(0x4);
    }

    /// A `Decoration` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct Decoration(pub u32);

    impl Decoration {
        /// `RelaxedPrecision`.
        pub const RELAXED_PRECISION: Self = Self(0x0);
        /// `SpecId`.
        pub const SPEC_ID: Self = Self(0x1);
        /// `Block`.
        pub const BLOCK: Self = Self(0x2);
        /// `BufferBlock`.
        pub const BUFFER_BLOCK: Self = Self(0x3);
        /// `RowMajor`.
        pub const ROW_MAJOR: Self = Self(0x4);
        /// `ColMajor`.
        pub const COL_MAJOR: Self = Self(0x5);
        /// `ArrayStride`.
        pub const ARRAY_STRIDE: Self = Self(0x6);
        /// `MatrixStride`.
        pub const MATRIX_STRIDE: Self = Self(0x7);
        /// `GLSLShared`.
        pub const GLSL_SHARED: Self = Self(0x8);
        /// `GLSLPacked`.
        pub const GLSL_PACKED: Self = Self(0x9);
        /// `CPacked`.
        pub const C_PACKED: Self = Self(0xa);
        /// `BuiltIn`.
        pub const BUILT_IN: Self = Self(0xb);
        /// `NoPerspective`.
        pub const NO_PERSPECTIVE: Self = Self(0xd);
        /// `Flat`.
        pub const FLAT: Self = Self(0xe);
        /// `Patch`.
        pub const PATCH: Self = Self(0xf);
        /// `Centroid`.
        pub const CENTROID: Self = Self(0x10);
        /// `Sample`.
        pub const SAMPLE: Self = Self(0x11);
        /// `Invariant`.
        pub const INVARIANT: Self = Self(0x12);
        /// `Restrict`.
        pub const RESTRICT: Self = Self(0x13);
        /// `Aliased`.
        pub const ALIASED: Self = Self(0x14);
        /// `Volatile`.
        pub const VOLATILE: Self = Self(0x15);
        /// `Constant`.
        pub const CONSTANT: Self = Self(0x16);
        /// `Coherent`.
        pub const COHERENT: Self = Self(0x17);
        /// `NonWritable`.
        pub const NON_WRITABLE: Self = Self(0x18);
        /// `NonReadable`.
        pub const NON_READABLE: Self = Self(0x19);
        /// `Uniform`.
        pub const UNIFORM: Self = Self(0x1a);
        /// `UniformId`.
        pub const UNIFORM_ID: Self = Self(0x1b);
        /// `SaturatedConversion`.
        pub const SATURATED_CONVERSION: Self = Self(0x1c);
        /// `Stream`.
        pub const STREAM: Self = Self(0x1d);
        /// `Location`.
        pub const LOCATION: Self = Self(0x1e);
        /// `Component`.
        pub const COMPONENT: Self = Self(0x1f);
        /// `Index`.
        pub const INDEX: Self = Self(0x20);
        /// `Binding`.
        pub const BINDING: Self = Self(0x21);
        /// `DescriptorSet`.
        pub const DESCRIPTOR_SET: Self = Self(0x22);
        /// `Offset`.
        pub const OFFSET: Self = Self(0x23);
        /// `XfbBuffer`.
        pub const XFB_BUFFER: Self = Self(0x24);
        /// `XfbStride`.
        pub const XFB_STRIDE: Self = Self(0x25);
        /// `FuncParamAttr`.
        pub const FUNC_PARAM_ATTR: Self = Self(0x26);
        /// `FPRoundingMode`.
        pub const FP_ROUNDING_MODE: Self = Self(0x27);
        /// `FPFastMathMode`.
        pub const FP_FAST_MATH_MODE: Self = Self(0x28);
        /// `LinkageAttributes`.
        pub const LINKAGE_ATTRIBUTES: Self = Self(0x29);
        /// `NoContraction`.
        pub const NO_CONTRACTION: Self = Self(0x2a);
        /// `InputAttachmentIndex`.
        pub const INPUT_ATTACHMENT_INDEX: Self = Self(0x2b);
        /// `Alignment`.
        pub const ALIGNMENT: Self = Self(0x2c);
        /// `MaxByteOffset`.
        pub const MAX_BYTE_OFFSET: Self = Self(0x2d);
        /// `AlignmentId`.
        pub const ALIGNMENT_ID: Self = Self(0x2e);
        /// `MaxByteOffsetId`.
        pub const MAX_BYTE_OFFSET_ID: Self = Self(0x2f);
        /// `SaturatedToLargestFloat8NormalConversionEXT`.
        pub const SATURATED_TO_LARGEST_FLOAT8_NORMAL_CONVERSION_EXT: Self = Self(0x1078);
        /// `NoSignedWrap`.
        pub const NO_SIGNED_WRAP: Self = Self(0x1175);
        /// `NoUnsignedWrap`.
        pub const NO_UNSIGNED_WRAP: Self = Self(0x1176);
        /// `ArrayStrideIdEXT`.
        pub const ARRAY_STRIDE_ID_EXT: Self = Self(0x1404);
        /// `OffsetIdEXT`.
        pub const OFFSET_ID_EXT: Self = Self(0x1405);
        /// `UTFEncodedKHR`.
        pub const UTF_ENCODED_KHR: Self = Self(0x1419);
        /// `PerPrimitiveEXT`.
        pub const PER_PRIMITIVE_EXT: Self = Self(0x1497);
        /// `PerVertexKHR`.
        pub const PER_VERTEX_KHR: Self = Self(0x14a5);
        /// `NonUniform`.
        pub const NON_UNIFORM: Self = Self(0x14b4);
        /// `RestrictPointer`.
        pub const RESTRICT_POINTER: Self = Self(0x14eb);
        /// `AliasedPointer`.
        pub const ALIASED_POINTER: Self = Self(0x14ec);
        /// `HitObjectShaderRecordBufferEXT`.
        pub const HIT_OBJECT_SHADER_RECORD_BUFFER_EXT: Self = Self(0x150d);
        /// `CooperativeMatrixTransposeEXT`.
        pub const COOPERATIVE_MATRIX_TRANSPOSE_EXT: Self = Self(0x1540);
        /// `CounterBuffer`.
        pub const COUNTER_BUFFER: Self = Self(0x1602);
        /// `UserSemantic`.
        pub const USER_SEMANTIC: Self = Self(0x1603);
    }

    /// A `Dim` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct Dim(pub u32);

    impl Dim {
        /// `1D`.
        pub const DIM_1D: Self = Self(0x0);
        /// `2D`.
        pub const DIM_2D: Self = Self(0x1);
        /// `3D`.
        pub const DIM_3D: Self = Self(0x2);
        /// `Cube`.
        pub const CUBE: Self = Self(0x3);
        /// `Rect`.
        pub const RECT: Self = Self(0x4);
        /// `Buffer`.
        pub const BUFFER: Self = Self(0x5);
        /// `SubpassData`.
        pub const SUBPASS_DATA: Self = Self(0x6);
        /// `TileImageDataEXT`.
        pub const TILE_IMAGE_DATA_EXT: Self = Self(0x104d);
    }

    /// A `ExecutionMode` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct ExecutionMode(pub u32);

    impl ExecutionMode {
        /// `Invocations`.
        pub const INVOCATIONS: Self = Self(0x0);
        /// `SpacingEqual`.
        pub const SPACING_EQUAL: Self = Self(0x1);
        /// `SpacingFractionalEven`.
        pub const SPACING_FRACTIONAL_EVEN: Self = Self(0x2);
        /// `SpacingFractionalOdd`.
        pub const SPACING_FRACTIONAL_ODD: Self = Self(0x3);
        /// `VertexOrderCw`.
        pub const VERTEX_ORDER_CW: Self = Self(0x4);
        /// `VertexOrderCcw`.
        pub const VERTEX_ORDER_CCW: Self = Self(0x5);
        /// `PixelCenterInteger`.
        pub const PIXEL_CENTER_INTEGER: Self = Self(0x6);
        /// `OriginUpperLeft`.
        pub const ORIGIN_UPPER_LEFT: Self = Self(0x7);
        /// `OriginLowerLeft`.
        pub const ORIGIN_LOWER_LEFT: Self = Self(0x8);
        /// `EarlyFragmentTests`.
        pub const EARLY_FRAGMENT_TESTS: Self = Self(0x9);
        /// `PointMode`.
        pub const POINT_MODE: Self = Self(0xa);
        /// `Xfb`.
        pub const XFB: Self = Self(0xb);
        /// `DepthReplacing`.
        pub const DEPTH_REPLACING: Self = Self(0xc);
        /// `DepthGreater`.
        pub const DEPTH_GREATER: Self = Self(0xe);
        /// `DepthLess`.
        pub const DEPTH_LESS: Self = Self(0xf);
        /// `DepthUnchanged`.
        pub const DEPTH_UNCHANGED: Self = Self(0x10);
        /// `LocalSize`.
        pub const LOCAL_SIZE: Self = Self(0x11);
        /// `LocalSizeHint`.
        pub const LOCAL_SIZE_HINT: Self = Self(0x12);
        /// `InputPoints`.
        pub const INPUT_POINTS: Self = Self(0x13);
        /// `InputLines`.
        pub const INPUT_LINES: Self = Self(0x14);
        /// `InputLinesAdjacency`.
        pub const INPUT_LINES_ADJACENCY: Self = Self(0x15);
        /// `Triangles`.
        pub const TRIANGLES: Self = Self(0x16);
        /// `InputTrianglesAdjacency`.
        pub const INPUT_TRIANGLES_ADJACENCY: Self = Self(0x17);
        /// `Quads`.
        pub const QUADS: Self = Self(0x18);
        /// `Isolines`.
        pub const ISOLINES: Self = Self(0x19);
        /// `OutputVertices`.
        pub const OUTPUT_VERTICES: Self = Self(0x1a);
        /// `OutputPoints`.
        pub const OUTPUT_POINTS: Self = Self(0x1b);
        /// `OutputLineStrip`.
        pub const OUTPUT_LINE_STRIP: Self = Self(0x1c);
        /// `OutputTriangleStrip`.
        pub const OUTPUT_TRIANGLE_STRIP: Self = Self(0x1d);
        /// `VecTypeHint`.
        pub const VEC_TYPE_HINT: Self = Self(0x1e);
        /// `ContractionOff`.
        pub const CONTRACTION_OFF: Self = Self(0x1f);
        /// `Initializer`.
        pub const INITIALIZER: Self = Self(0x21);
        /// `Finalizer`.
        pub const FINALIZER: Self = Self(0x22);
        /// `SubgroupSize`.
        pub const SUBGROUP_SIZE: Self = Self(0x23);
        /// `SubgroupsPerWorkgroup`.
        pub const SUBGROUPS_PER_WORKGROUP: Self = Self(0x24);
        /// `SubgroupsPerWorkgroupId`.
        pub const SUBGROUPS_PER_WORKGROUP_ID: Self = Self(0x25);
        /// `LocalSizeId`.
        pub const LOCAL_SIZE_ID: Self = Self(0x26);
        /// `LocalSizeHintId`.
        pub const LOCAL_SIZE_HINT_ID: Self = Self(0x27);
        /// `NonCoherentColorAttachmentReadEXT`.
        pub const NON_COHERENT_COLOR_ATTACHMENT_READ_EXT: Self = Self(0x1049);
        /// `NonCoherentDepthAttachmentReadEXT`.
        pub const NON_COHERENT_DEPTH_ATTACHMENT_READ_EXT: Self = Self(0x104a);
        /// `NonCoherentStencilAttachmentReadEXT`.
        pub const NON_COHERENT_STENCIL_ATTACHMENT_READ_EXT: Self = Self(0x104b);
        /// `SubgroupUniformControlFlowKHR`.
        pub const SUBGROUP_UNIFORM_CONTROL_FLOW_KHR: Self = Self(0x1145);
        /// `PostDepthCoverage`.
        pub const POST_DEPTH_COVERAGE: Self = Self(0x115e);
        /// `DenormPreserve`.
        pub const DENORM_PRESERVE: Self = Self(0x116b);
        /// `DenormFlushToZero`.
        pub const DENORM_FLUSH_TO_ZERO: Self = Self(0x116c);
        /// `SignedZeroInfNanPreserve`.
        pub const SIGNED_ZERO_INF_NAN_PRESERVE: Self = Self(0x116d);
        /// `RoundingModeRTE`.
        pub const ROUNDING_MODE_RTE: Self = Self(0x116e);
        /// `RoundingModeRTZ`.
        pub const ROUNDING_MODE_RTZ: Self = Self(0x116f);
        /// `StencilRefReplacingEXT`.
        pub const STENCIL_REF_REPLACING_EXT: Self = Self(0x13a3);
        /// `StencilRefUnchangedFrontAMD`.
        pub const STENCIL_REF_UNCHANGED_FRONT_AMD: Self = Self(0x13d7);
        /// `StencilRefGreaterFrontAMD`.
        pub const STENCIL_REF_GREATER_FRONT_AMD: Self = Self(0x13d8);
        /// `StencilRefLessFrontAMD`.
        pub const STENCIL_REF_LESS_FRONT_AMD: Self = Self(0x13d9);
        /// `StencilRefUnchangedBackAMD`.
        pub const STENCIL_REF_UNCHANGED_BACK_AMD: Self = Self(0x13da);
        /// `StencilRefGreaterBackAMD`.
        pub const STENCIL_REF_GREATER_BACK_AMD: Self = Self(0x13db);
        /// `StencilRefLessBackAMD`.
        pub const STENCIL_REF_LESS_BACK_AMD: Self = Self(0x13dc);
        /// `QuadDerivativesKHR`.
        pub const QUAD_DERIVATIVES_KHR: Self = Self(0x13e0);
        /// `RequireFullQuadsKHR`.
        pub const REQUIRE_FULL_QUADS_KHR: Self = Self(0x13e1);
        /// `OutputLinesEXT`.
        pub const OUTPUT_LINES_EXT: Self = Self(0x1495);
        /// `OutputPrimitivesEXT`.
        pub const OUTPUT_PRIMITIVES_EXT: Self = Self(0x1496);
        /// `DerivativeGroupQuadsKHR`.
        pub const DERIVATIVE_GROUP_QUADS_KHR: Self = Self(0x14a9);
        /// `DerivativeGroupLinearKHR`.
        pub const DERIVATIVE_GROUP_LINEAR_KHR: Self = Self(0x14aa);
        /// `OutputTrianglesEXT`.
        pub const OUTPUT_TRIANGLES_EXT: Self = Self(0x14b2);
        /// `PixelInterlockOrderedEXT`.
        pub const PIXEL_INTERLOCK_ORDERED_EXT: Self = Self(0x14f6);
        /// `PixelInterlockUnorderedEXT`.
        pub const PIXEL_INTERLOCK_UNORDERED_EXT: Self = Self(0x14f7);
        /// `SampleInterlockOrderedEXT`.
        pub const SAMPLE_INTERLOCK_ORDERED_EXT: Self = Self(0x14f8);
        /// `SampleInterlockUnorderedEXT`.
        pub const SAMPLE_INTERLOCK_UNORDERED_EXT: Self = Self(0x14f9);
        /// `ShadingRateInterlockOrderedEXT`.
        pub const SHADING_RATE_INTERLOCK_ORDERED_EXT: Self = Self(0x14fa);
        /// `ShadingRateInterlockUnorderedEXT`.
        pub const SHADING_RATE_INTERLOCK_UNORDERED_EXT: Self = Self(0x14fb);
        /// `Shader64BitIndexingEXT`.
        pub const SHADER64_BIT_INDEXING_EXT: Self = Self(0x1533);
        /// `MaximallyReconvergesKHR`.
        pub const MAXIMALLY_RECONVERGES_KHR: Self = Self(0x1787);
        /// `FPFastMathDefault`.
        pub const FP_FAST_MATH_DEFAULT: Self = Self(0x178c);
        /// `OpacityMicromapIdKHR`.
        pub const OPACITY_MICROMAP_ID_KHR: Self = Self(0x178f);
    }

    /// A `ExecutionModel` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct ExecutionModel(pub u32);

    impl ExecutionModel {
        /// `Vertex`.
        pub const VERTEX: Self = Self(0x0);
        /// `TessellationControl`.
        pub const TESSELLATION_CONTROL: Self = Self(0x1);
        /// `TessellationEvaluation`.
        pub const TESSELLATION_EVALUATION: Self = Self(0x2);
        /// `Geometry`.
        pub const GEOMETRY: Self = Self(0x3);
        /// `Fragment`.
        pub const FRAGMENT: Self = Self(0x4);
        /// `GLCompute`.
        pub const GL_COMPUTE: Self = Self(0x5);
        /// `Kernel`.
        pub const KERNEL: Self = Self(0x6);
        /// `RayGenerationKHR`.
        pub const RAY_GENERATION_KHR: Self = Self(0x14c1);
        /// `IntersectionKHR`.
        pub const INTERSECTION_KHR: Self = Self(0x14c2);
        /// `AnyHitKHR`.
        pub const ANY_HIT_KHR: Self = Self(0x14c3);
        /// `ClosestHitKHR`.
        pub const CLOSEST_HIT_KHR: Self = Self(0x14c4);
        /// `MissKHR`.
        pub const MISS_KHR: Self = Self(0x14c5);
        /// `CallableKHR`.
        pub const CALLABLE_KHR: Self = Self(0x14c6);
        /// `TaskEXT`.
        pub const TASK_EXT: Self = Self(0x14f4);
        /// `MeshEXT`.
        pub const MESH_EXT: Self = Self(0x14f5);
    }

    /// A `FPEncoding` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct FPEncoding(pub u32);

    impl FPEncoding {
        /// `BFloat16KHR`.
        pub const B_FLOAT16_KHR: Self = Self(0x0);
        /// `Float8E4M3EXT`.
        pub const FLOAT8_E4_M3_EXT: Self = Self(0x1076);
        /// `Float8E5M2EXT`.
        pub const FLOAT8_E5_M2_EXT: Self = Self(0x1077);
        /// `Float6E2M3EXT`.
        pub const FLOAT6_E2_M3_EXT: Self = Self(0x107f);
        /// `Float6E3M2EXT`.
        pub const FLOAT6_E3_M2_EXT: Self = Self(0x1080);
        /// `Float4E2M1EXT`.
        pub const FLOAT4_E2_M1_EXT: Self = Self(0x1081);
        /// `Float8UnsignedE8M0EXT`.
        pub const FLOAT8_UNSIGNED_E8_M0_EXT: Self = Self(0x1082);
        /// `MXInt8EXT`.
        pub const MX_INT8_EXT: Self = Self(0x1083);
    }

    /// A `FPFastMathMode` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct FPFastMathMode(pub u32);

    impl FPFastMathMode {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `NotNaN`.
        pub const NOT_NA_N: Self = Self(0x1);
        /// `NotInf`.
        pub const NOT_INF: Self = Self(0x2);
        /// `NSZ`.
        pub const NSZ: Self = Self(0x4);
        /// `AllowRecip`.
        pub const ALLOW_RECIP: Self = Self(0x8);
        /// `Fast`.
        pub const FAST: Self = Self(0x10);
        /// `AllowContract`.
        pub const ALLOW_CONTRACT: Self = Self(0x1_0000);
        /// `AllowReassoc`.
        pub const ALLOW_REASSOC: Self = Self(0x2_0000);
        /// `AllowTransform`.
        pub const ALLOW_TRANSFORM: Self = Self(0x4_0000);
    }

    /// A `FPRoundingMode` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct FPRoundingMode(pub u32);

    impl FPRoundingMode {
        /// `RTE`.
        pub const RTE: Self = Self(0x0);
        /// `RTZ`.
        pub const RTZ: Self = Self(0x1);
        /// `RTP`.
        pub const RTP: Self = Self(0x2);
        /// `RTN`.
        pub const RTN: Self = Self(0x3);
    }

    /// A `FunctionControl` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct FunctionControl(pub u32);

    impl FunctionControl {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `Inline`.
        pub const INLINE: Self = Self(0x1);
        /// `DontInline`.
        pub const DONT_INLINE: Self = Self(0x2);
        /// `Pure`.
        pub const PURE: Self = Self(0x4);
        /// `Const`.
        pub const CONST: Self = Self(0x8);
        /// `OptNoneEXT`.
        pub const OPT_NONE_EXT: Self = Self(0x1_0000);
    }

    /// A `FunctionParameterAttribute` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct FunctionParameterAttribute(pub u32);

    impl FunctionParameterAttribute {
        /// `Zext`.
        pub const ZEXT: Self = Self(0x0);
        /// `Sext`.
        pub const SEXT: Self = Self(0x1);
        /// `ByVal`.
        pub const BY_VAL: Self = Self(0x2);
        /// `Sret`.
        pub const SRET: Self = Self(0x3);
        /// `NoAlias`.
        pub const NO_ALIAS: Self = Self(0x4);
        /// `NoCapture`.
        pub const NO_CAPTURE: Self = Self(0x5);
        /// `NoWrite`.
        pub const NO_WRITE: Self = Self(0x6);
        /// `NoReadWrite`.
        pub const NO_READ_WRITE: Self = Self(0x7);
    }

    /// A `GroupOperation` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct GroupOperation(pub u32);

    impl GroupOperation {
        /// `Reduce`.
        pub const REDUCE: Self = Self(0x0);
        /// `InclusiveScan`.
        pub const INCLUSIVE_SCAN: Self = Self(0x1);
        /// `ExclusiveScan`.
        pub const EXCLUSIVE_SCAN: Self = Self(0x2);
        /// `ClusteredReduce`.
        pub const CLUSTERED_REDUCE: Self = Self(0x3);
        /// `PartitionedReduceEXT`.
        pub const PARTITIONED_REDUCE_EXT: Self = Self(0x6);
        /// `PartitionedInclusiveScanEXT`.
        pub const PARTITIONED_INCLUSIVE_SCAN_EXT: Self = Self(0x7);
        /// `PartitionedExclusiveScanEXT`.
        pub const PARTITIONED_EXCLUSIVE_SCAN_EXT: Self = Self(0x8);
    }

    /// A `ImageFormat` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct ImageFormat(pub u32);

    impl ImageFormat {
        /// `Unknown`.
        pub const UNKNOWN: Self = Self(0x0);
        /// `Rgba32f`.
        pub const RGBA32F: Self = Self(0x1);
        /// `Rgba16f`.
        pub const RGBA16F: Self = Self(0x2);
        /// `R32f`.
        pub const R32F: Self = Self(0x3);
        /// `Rgba8`.
        pub const RGBA8: Self = Self(0x4);
        /// `Rgba8Snorm`.
        pub const RGBA8_SNORM: Self = Self(0x5);
        /// `Rg32f`.
        pub const RG32F: Self = Self(0x6);
        /// `Rg16f`.
        pub const RG16F: Self = Self(0x7);
        /// `R11fG11fB10f`.
        pub const R11F_G11F_B10F: Self = Self(0x8);
        /// `R16f`.
        pub const R16F: Self = Self(0x9);
        /// `Rgba16`.
        pub const RGBA16: Self = Self(0xa);
        /// `Rgb10A2`.
        pub const RGB10_A2: Self = Self(0xb);
        /// `Rg16`.
        pub const RG16: Self = Self(0xc);
        /// `Rg8`.
        pub const RG8: Self = Self(0xd);
        /// `R16`.
        pub const R16: Self = Self(0xe);
        /// `R8`.
        pub const R8: Self = Self(0xf);
        /// `Rgba16Snorm`.
        pub const RGBA16_SNORM: Self = Self(0x10);
        /// `Rg16Snorm`.
        pub const RG16_SNORM: Self = Self(0x11);
        /// `Rg8Snorm`.
        pub const RG8_SNORM: Self = Self(0x12);
        /// `R16Snorm`.
        pub const R16_SNORM: Self = Self(0x13);
        /// `R8Snorm`.
        pub const R8_SNORM: Self = Self(0x14);
        /// `Rgba32i`.
        pub const RGBA32I: Self = Self(0x15);
        /// `Rgba16i`.
        pub const RGBA16I: Self = Self(0x16);
        /// `Rgba8i`.
        pub const RGBA8I: Self = Self(0x17);
        /// `R32i`.
        pub const R32I: Self = Self(0x18);
        /// `Rg32i`.
        pub const RG32I: Self = Self(0x19);
        /// `Rg16i`.
        pub const RG16I: Self = Self(0x1a);
        /// `Rg8i`.
        pub const RG8I: Self = Self(0x1b);
        /// `R16i`.
        pub const R16I: Self = Self(0x1c);
        /// `R8i`.
        pub const R8I: Self = Self(0x1d);
        /// `Rgba32ui`.
        pub const RGBA32UI: Self = Self(0x1e);
        /// `Rgba16ui`.
        pub const RGBA16UI: Self = Self(0x1f);
        /// `Rgba8ui`.
        pub const RGBA8UI: Self = Self(0x20);
        /// `R32ui`.
        pub const R32UI: Self = Self(0x21);
        /// `Rgb10a2ui`.
        pub const RGB10A2UI: Self = Self(0x22);
        /// `Rg32ui`.
        pub const RG32UI: Self = Self(0x23);
        /// `Rg16ui`.
        pub const RG16UI: Self = Self(0x24);
        /// `Rg8ui`.
        pub const RG8UI: Self = Self(0x25);
        /// `R16ui`.
        pub const R16UI: Self = Self(0x26);
        /// `R8ui`.
        pub const R8UI: Self = Self(0x27);
        /// `R64ui`.
        pub const R64UI: Self = Self(0x28);
        /// `R64i`.
        pub const R64I: Self = Self(0x29);
    }

    /// A `ImageOperands` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct ImageOperands(pub u32);

    impl ImageOperands {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `Bias`.
        pub const BIAS: Self = Self(0x1);
        /// `Lod`.
        pub const LOD: Self = Self(0x2);
        /// `Grad`.
        pub const GRAD: Self = Self(0x4);
        /// `ConstOffset`.
        pub const CONST_OFFSET: Self = Self(0x8);
        /// `Offset`.
        pub const OFFSET: Self = Self(0x10);
        /// `ConstOffsets`.
        pub const CONST_OFFSETS: Self = Self(0x20);
        /// `Sample`.
        pub const SAMPLE: Self = Self(0x40);
        /// `MinLod`.
        pub const MIN_LOD: Self = Self(0x80);
        /// `MakeTexelAvailable`.
        pub const MAKE_TEXEL_AVAILABLE: Self = Self(0x100);
        /// `MakeTexelVisible`.
        pub const MAKE_TEXEL_VISIBLE: Self = Self(0x200);
        /// `NonPrivateTexel`.
        pub const NON_PRIVATE_TEXEL: Self = Self(0x400);
        /// `VolatileTexel`.
        pub const VOLATILE_TEXEL: Self = Self(0x800);
        /// `SignExtend`.
        pub const SIGN_EXTEND: Self = Self(0x1000);
        /// `ZeroExtend`.
        pub const ZERO_EXTEND: Self = Self(0x2000);
        /// `Nontemporal`.
        pub const NONTEMPORAL: Self = Self(0x4000);
        /// `Offsets`.
        pub const OFFSETS: Self = Self(0x1_0000);
    }

    /// A `LinkageType` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct LinkageType(pub u32);

    impl LinkageType {
        /// `Export`.
        pub const EXPORT: Self = Self(0x0);
        /// `Import`.
        pub const IMPORT: Self = Self(0x1);
        /// `LinkOnceODR`.
        pub const LINK_ONCE_ODR: Self = Self(0x2);
    }

    /// A `LoopControl` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct LoopControl(pub u32);

    impl LoopControl {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `Unroll`.
        pub const UNROLL: Self = Self(0x1);
        /// `DontUnroll`.
        pub const DONT_UNROLL: Self = Self(0x2);
        /// `DependencyInfinite`.
        pub const DEPENDENCY_INFINITE: Self = Self(0x4);
        /// `DependencyLength`.
        pub const DEPENDENCY_LENGTH: Self = Self(0x8);
        /// `MinIterations`.
        pub const MIN_ITERATIONS: Self = Self(0x10);
        /// `MaxIterations`.
        pub const MAX_ITERATIONS: Self = Self(0x20);
        /// `IterationMultiple`.
        pub const ITERATION_MULTIPLE: Self = Self(0x40);
        /// `PeelCount`.
        pub const PEEL_COUNT: Self = Self(0x80);
        /// `PartialCount`.
        pub const PARTIAL_COUNT: Self = Self(0x100);
        /// `MultipleWaitQueuesQCOM`.
        pub const MULTIPLE_WAIT_QUEUES_QCOM: Self = Self(0x1000_0000);
    }

    /// A `MemoryAccess` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct MemoryAccess(pub u32);

    impl MemoryAccess {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `Volatile`.
        pub const VOLATILE: Self = Self(0x1);
        /// `Aligned`.
        pub const ALIGNED: Self = Self(0x2);
        /// `Nontemporal`.
        pub const NONTEMPORAL: Self = Self(0x4);
        /// `MakePointerAvailable`.
        pub const MAKE_POINTER_AVAILABLE: Self = Self(0x8);
        /// `MakePointerVisible`.
        pub const MAKE_POINTER_VISIBLE: Self = Self(0x10);
        /// `NonPrivatePointer`.
        pub const NON_PRIVATE_POINTER: Self = Self(0x20);
    }

    /// A `MemoryModel` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct MemoryModel(pub u32);

    impl MemoryModel {
        /// `Simple`.
        pub const SIMPLE: Self = Self(0x0);
        /// `GLSL450`.
        pub const GLSL450: Self = Self(0x1);
        /// `OpenCL`.
        pub const OPEN_CL: Self = Self(0x2);
        /// `Vulkan`.
        pub const VULKAN: Self = Self(0x3);
    }

    /// A `MemorySemantics` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct MemorySemantics(pub u32);

    impl MemorySemantics {
        /// `Relaxed`.
        pub const RELAXED: Self = Self(0x0);
        /// `Acquire`.
        pub const ACQUIRE: Self = Self(0x2);
        /// `Release`.
        pub const RELEASE: Self = Self(0x4);
        /// `AcquireRelease`.
        pub const ACQUIRE_RELEASE: Self = Self(0x8);
        /// `SequentiallyConsistent`.
        pub const SEQUENTIALLY_CONSISTENT: Self = Self(0x10);
        /// `UniformMemory`.
        pub const UNIFORM_MEMORY: Self = Self(0x40);
        /// `SubgroupMemory`.
        pub const SUBGROUP_MEMORY: Self = Self(0x80);
        /// `WorkgroupMemory`.
        pub const WORKGROUP_MEMORY: Self = Self(0x100);
        /// `CrossWorkgroupMemory`.
        pub const CROSS_WORKGROUP_MEMORY: Self = Self(0x200);
        /// `AtomicCounterMemory`.
        pub const ATOMIC_COUNTER_MEMORY: Self = Self(0x400);
        /// `ImageMemory`.
        pub const IMAGE_MEMORY: Self = Self(0x800);
        /// `OutputMemory`.
        pub const OUTPUT_MEMORY: Self = Self(0x1000);
        /// `MakeAvailable`.
        pub const MAKE_AVAILABLE: Self = Self(0x2000);
        /// `MakeVisible`.
        pub const MAKE_VISIBLE: Self = Self(0x4000);
        /// `Volatile`.
        pub const VOLATILE: Self = Self(0x8000);
    }

    /// A `PackedVectorFormat` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct PackedVectorFormat(pub u32);

    impl PackedVectorFormat {
        /// `PackedVectorFormat4x8Bit`.
        pub const PACKED_VECTOR_FORMAT4X8_BIT: Self = Self(0x0);
    }

    /// A `SamplerAddressingMode` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct SamplerAddressingMode(pub u32);

    impl SamplerAddressingMode {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `ClampToEdge`.
        pub const CLAMP_TO_EDGE: Self = Self(0x1);
        /// `Clamp`.
        pub const CLAMP: Self = Self(0x2);
        /// `Repeat`.
        pub const REPEAT: Self = Self(0x3);
        /// `RepeatMirrored`.
        pub const REPEAT_MIRRORED: Self = Self(0x4);
    }

    /// A `SamplerFilterMode` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct SamplerFilterMode(pub u32);

    impl SamplerFilterMode {
        /// `Nearest`.
        pub const NEAREST: Self = Self(0x0);
        /// `Linear`.
        pub const LINEAR: Self = Self(0x1);
    }

    /// A `Scope` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct Scope(pub u32);

    impl Scope {
        /// `CrossDevice`.
        pub const CROSS_DEVICE: Self = Self(0x0);
        /// `Device`.
        pub const DEVICE: Self = Self(0x1);
        /// `Workgroup`.
        pub const WORKGROUP: Self = Self(0x2);
        /// `Subgroup`.
        pub const SUBGROUP: Self = Self(0x3);
        /// `Invocation`.
        pub const INVOCATION: Self = Self(0x4);
        /// `QueueFamily`.
        pub const QUEUE_FAMILY: Self = Self(0x5);
        /// `ShaderCallKHR`.
        pub const SHADER_CALL_KHR: Self = Self(0x6);
    }

    /// A `SelectionControl` set of bits.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct SelectionControl(pub u32);

    impl SelectionControl {
        /// `None`.
        pub const NONE: Self = Self(0x0);
        /// `Flatten`.
        pub const FLATTEN: Self = Self(0x1);
        /// `DontFlatten`.
        pub const DONT_FLATTEN: Self = Self(0x2);
    }

    /// A `SourceLanguage` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct SourceLanguage(pub u32);

    impl SourceLanguage {
        /// `Unknown`.
        pub const UNKNOWN: Self = Self(0x0);
        /// `ESSL`.
        pub const ESSL: Self = Self(0x1);
        /// `GLSL`.
        pub const GLSL: Self = Self(0x2);
        /// `OpenCL_C`.
        pub const OPEN_CL_C: Self = Self(0x3);
        /// `OpenCL_CPP`.
        pub const OPEN_CL_CPP: Self = Self(0x4);
        /// `HLSL`.
        pub const HLSL: Self = Self(0x5);
        /// `CPP_for_OpenCL`.
        pub const CPP_FOR_OPEN_CL: Self = Self(0x6);
        /// `SYCL`.
        pub const SYCL: Self = Self(0x7);
        /// `HERO_C`.
        pub const HERO_C: Self = Self(0x8);
        /// `NZSL`.
        pub const NZSL: Self = Self(0x9);
        /// `WGSL`.
        pub const WGSL: Self = Self(0xa);
        /// `Slang`.
        pub const SLANG: Self = Self(0xb);
        /// `Zig`.
        pub const ZIG: Self = Self(0xc);
        /// `Rust`.
        pub const RUST: Self = Self(0xd);
        /// `Pred`.
        pub const PRED: Self = Self(0xe);
        /// `ApilaJai`.
        pub const APILA_JAI: Self = Self(0xf);
    }

    /// A `StorageClass` value.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
    pub struct StorageClass(pub u32);

    impl StorageClass {
        /// `UniformConstant`.
        pub const UNIFORM_CONSTANT: Self = Self(0x0);
        /// `Input`.
        pub const INPUT: Self = Self(0x1);
        /// `Uniform`.
        pub const UNIFORM: Self = Self(0x2);
        /// `Output`.
        pub const OUTPUT: Self = Self(0x3);
        /// `Workgroup`.
        pub const WORKGROUP: Self = Self(0x4);
        /// `CrossWorkgroup`.
        pub const CROSS_WORKGROUP: Self = Self(0x5);
        /// `Private`.
        pub const PRIVATE: Self = Self(0x6);
        /// `Function`.
        pub const FUNCTION: Self = Self(0x7);
        /// `Generic`.
        pub const GENERIC: Self = Self(0x8);
        /// `PushConstant`.
        pub const PUSH_CONSTANT: Self = Self(0x9);
        /// `AtomicCounter`.
        pub const ATOMIC_COUNTER: Self = Self(0xa);
        /// `Image`.
        pub const IMAGE: Self = Self(0xb);
        /// `StorageBuffer`.
        pub const STORAGE_BUFFER: Self = Self(0xc);
        /// `TileImageEXT`.
        pub const TILE_IMAGE_EXT: Self = Self(0x104c);
        /// `CallableDataKHR`.
        pub const CALLABLE_DATA_KHR: Self = Self(0x14d0);
        /// `IncomingCallableDataKHR`.
        pub const INCOMING_CALLABLE_DATA_KHR: Self = Self(0x14d1);
        /// `RayPayloadKHR`.
        pub const RAY_PAYLOAD_KHR: Self = Self(0x14da);
        /// `HitAttributeKHR`.
        pub const HIT_ATTRIBUTE_KHR: Self = Self(0x14db);
        /// `IncomingRayPayloadKHR`.
        pub const INCOMING_RAY_PAYLOAD_KHR: Self = Self(0x14de);
        /// `ShaderRecordBufferKHR`.
        pub const SHADER_RECORD_BUFFER_KHR: Self = Self(0x14df);
        /// `PhysicalStorageBuffer`.
        pub const PHYSICAL_STORAGE_BUFFER: Self = Self(0x14e5);
        /// `TaskPayloadWorkgroupEXT`.
        pub const TASK_PAYLOAD_WORKGROUP_EXT: Self = Self(0x151a);
        /// `HitObjectAttributeEXT`.
        pub const HIT_OBJECT_ATTRIBUTE_EXT: Self = Self(0x1523);
    }

}

/// A kind of operand the grammar names.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Kind {
    /// `AccessQualifier`.
    AccessQualifier,
    /// `AddressingModel`.
    AddressingModel,
    /// `BuiltIn`.
    BuiltIn,
    /// `Capability`.
    Capability,
    /// `CooperativeMatrixOperands`.
    CooperativeMatrixOperands,
    /// `CooperativeMatrixReduce`.
    CooperativeMatrixReduce,
    /// `Decoration`.
    Decoration,
    /// `Dim`.
    Dim,
    /// `ExecutionMode`.
    ExecutionMode,
    /// `ExecutionModel`.
    ExecutionModel,
    /// `FPEncoding`.
    FPEncoding,
    /// `FPFastMathMode`.
    FPFastMathMode,
    /// `FPRoundingMode`.
    FPRoundingMode,
    /// `FunctionControl`.
    FunctionControl,
    /// `FunctionParameterAttribute`.
    FunctionParameterAttribute,
    /// `GroupOperation`.
    GroupOperation,
    /// `IdMemorySemantics`.
    IdMemorySemantics,
    /// `IdRef`.
    IdRef,
    /// `IdResult`.
    IdResult,
    /// `IdResultType`.
    IdResultType,
    /// `IdScope`.
    IdScope,
    /// `ImageFormat`.
    ImageFormat,
    /// `ImageOperands`.
    ImageOperands,
    /// `LinkageType`.
    LinkageType,
    /// `LiteralContextDependentNumber`.
    LiteralContextDependentNumber,
    /// `LiteralExtInstInteger`.
    LiteralExtInstInteger,
    /// `LiteralInteger`.
    LiteralInteger,
    /// `LiteralSpecConstantOpInteger`.
    LiteralSpecConstantOpInteger,
    /// `LiteralString`.
    LiteralString,
    /// `LoopControl`.
    LoopControl,
    /// `MemoryAccess`.
    MemoryAccess,
    /// `MemoryModel`.
    MemoryModel,
    /// `MemorySemantics`.
    MemorySemantics,
    /// `PackedVectorFormat`.
    PackedVectorFormat,
    /// `PairIdRefIdRef`.
    PairIdRefIdRef,
    /// `PairIdRefLiteralInteger`.
    PairIdRefLiteralInteger,
    /// `PairLiteralIntegerIdRef`.
    PairLiteralIntegerIdRef,
    /// `SamplerAddressingMode`.
    SamplerAddressingMode,
    /// `SamplerFilterMode`.
    SamplerFilterMode,
    /// `Scope`.
    Scope,
    /// `SelectionControl`.
    SelectionControl,
    /// `SourceLanguage`.
    SourceLanguage,
    /// `StorageClass`.
    StorageClass,
}

impl Kind {
    /// How the decoder reads it.
    pub(crate) const fn shape(self) -> Shape {
        match self {
            Self::CooperativeMatrixOperands | Self::CooperativeMatrixReduce | Self::FPFastMathMode | Self::FunctionControl | Self::ImageOperands | Self::LoopControl | Self::MemoryAccess | Self::MemorySemantics | Self::SelectionControl => Shape::Bits,
            Self::AccessQualifier | Self::AddressingModel | Self::BuiltIn | Self::Capability | Self::Decoration | Self::Dim | Self::ExecutionMode | Self::ExecutionModel | Self::FPEncoding | Self::FPRoundingMode | Self::FunctionParameterAttribute | Self::GroupOperation | Self::ImageFormat | Self::LinkageType | Self::MemoryModel | Self::PackedVectorFormat | Self::SamplerAddressingMode | Self::SamplerFilterMode | Self::Scope | Self::SourceLanguage | Self::StorageClass => Shape::Enum,
            Self::IdMemorySemantics | Self::IdRef | Self::IdResult | Self::IdResultType | Self::IdScope => Shape::Id,
            Self::LiteralContextDependentNumber => Shape::Number,
            Self::PairIdRefIdRef => Shape::Pair(Kind::IdRef, Kind::IdRef),
            Self::PairIdRefLiteralInteger => Shape::Pair(Kind::IdRef, Kind::LiteralInteger),
            Self::LiteralSpecConstantOpInteger => Shape::SpecOp,
            Self::PairLiteralIntegerIdRef => Shape::SwitchTarget,
            Self::LiteralString => Shape::Text,
            Self::LiteralExtInstInteger | Self::LiteralInteger => Shape::Word,
        }
    }

    /// Its enumerants, by ascending value; empty for a kind that has none.
    pub(crate) const fn enumerants(self) -> &'static [Enumerant] {
        match self {
            Self::AccessQualifier => &ACCESS_QUALIFIER,
            Self::AddressingModel => &ADDRESSING_MODEL,
            Self::BuiltIn => &BUILT_IN,
            Self::Capability => &CAPABILITY,
            Self::CooperativeMatrixOperands => &COOPERATIVE_MATRIX_OPERANDS,
            Self::CooperativeMatrixReduce => &COOPERATIVE_MATRIX_REDUCE,
            Self::Decoration => &DECORATION,
            Self::Dim => &DIM,
            Self::ExecutionMode => &EXECUTION_MODE,
            Self::ExecutionModel => &EXECUTION_MODEL,
            Self::FPEncoding => &FP_ENCODING,
            Self::FPFastMathMode => &FP_FAST_MATH_MODE,
            Self::FPRoundingMode => &FP_ROUNDING_MODE,
            Self::FunctionControl => &FUNCTION_CONTROL,
            Self::FunctionParameterAttribute => &FUNCTION_PARAMETER_ATTRIBUTE,
            Self::GroupOperation => &GROUP_OPERATION,
            Self::ImageFormat => &IMAGE_FORMAT,
            Self::ImageOperands => &IMAGE_OPERANDS,
            Self::LinkageType => &LINKAGE_TYPE,
            Self::LoopControl => &LOOP_CONTROL,
            Self::MemoryAccess => &MEMORY_ACCESS,
            Self::MemoryModel => &MEMORY_MODEL,
            Self::MemorySemantics => &MEMORY_SEMANTICS,
            Self::PackedVectorFormat => &PACKED_VECTOR_FORMAT,
            Self::SamplerAddressingMode => &SAMPLER_ADDRESSING_MODE,
            Self::SamplerFilterMode => &SAMPLER_FILTER_MODE,
            Self::Scope => &SCOPE,
            Self::SelectionControl => &SELECTION_CONTROL,
            Self::SourceLanguage => &SOURCE_LANGUAGE,
            Self::StorageClass => &STORAGE_CLASS,
            _ => &[],
        }
    }
}

static ACCESS_QUALIFIER: [Enumerant; 3] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
];

static ADDRESSING_MODEL: [Enumerant; 4] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x14e4, parameters: &[], gate: Gate { capabilities: &[Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &["SPV_EXT_physical_storage_buffer", "SPV_KHR_physical_storage_buffer"], version: V1_5, last: None } },
];

static BUILT_IN: [Enumerant; 81] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::CLIP_DISTANCE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::CULL_DISTANCE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x7, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY, Capability::TESSELLATION, Capability::RAY_TRACING_KHR, Capability::MESH_SHADING_EXT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY, Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x9, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY, Capability::SHADER_LAYER, Capability::SHADER_VIEWPORT_INDEX_LAYER_EXT, Capability::MESH_SHADING_EXT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xa, parameters: &[], gate: Gate { capabilities: &[Capability::MULTI_VIEWPORT, Capability::SHADER_VIEWPORT_INDEX, Capability::SHADER_VIEWPORT_INDEX_LAYER_EXT, Capability::MESH_SHADING_EXT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xb, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xc, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xd, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xe, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xf, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x10, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x11, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x12, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLE_RATE_SHADING], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x13, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLE_RATE_SHADING], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x14, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x16, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x17, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x18, parameters: &[], gate: CORE },
    Enumerant { value: 0x19, parameters: &[], gate: CORE },
    Enumerant { value: 0x1a, parameters: &[], gate: CORE },
    Enumerant { value: 0x1b, parameters: &[], gate: CORE },
    Enumerant { value: 0x1c, parameters: &[], gate: CORE },
    Enumerant { value: 0x1d, parameters: &[], gate: CORE },
    Enumerant { value: 0x1e, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1f, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x20, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x21, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x22, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x24, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM, Capability::SUBGROUP_BALLOT_KHR], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x25, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x26, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x27, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x28, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x29, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM, Capability::SUBGROUP_BALLOT_KHR], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2a, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2b, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1140, parameters: &[], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &["SPV_KHR_shader_ballot"], version: V1_3, last: None } },
    Enumerant { value: 0x1141, parameters: &[], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &["SPV_KHR_shader_ballot"], version: V1_3, last: None } },
    Enumerant { value: 0x1142, parameters: &[], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &["SPV_KHR_shader_ballot"], version: V1_3, last: None } },
    Enumerant { value: 0x1143, parameters: &[], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &["SPV_KHR_shader_ballot"], version: V1_3, last: None } },
    Enumerant { value: 0x1144, parameters: &[], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &["SPV_KHR_shader_ballot"], version: V1_3, last: None } },
    Enumerant { value: 0x1148, parameters: &[], gate: Gate { capabilities: &[Capability::DRAW_PARAMETERS], extensions: &["SPV_KHR_shader_draw_parameters"], version: V1_3, last: None } },
    Enumerant { value: 0x1149, parameters: &[], gate: Gate { capabilities: &[Capability::DRAW_PARAMETERS], extensions: &["SPV_KHR_shader_draw_parameters"], version: V1_3, last: None } },
    Enumerant { value: 0x114a, parameters: &[], gate: Gate { capabilities: &[Capability::DRAW_PARAMETERS, Capability::MESH_SHADING_EXT], extensions: &["SPV_KHR_shader_draw_parameters", "SPV_EXT_mesh_shader"], version: V1_3, last: None } },
    Enumerant { value: 0x1150, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADING_RATE_KHR], extensions: &["SPV_KHR_fragment_shading_rate"], version: None, last: None } },
    Enumerant { value: 0x1156, parameters: &[], gate: Gate { capabilities: &[Capability::DEVICE_GROUP], extensions: &["SPV_KHR_device_group"], version: V1_3, last: None } },
    Enumerant { value: 0x1158, parameters: &[], gate: Gate { capabilities: &[Capability::MULTI_VIEW], extensions: &["SPV_KHR_multiview"], version: V1_3, last: None } },
    Enumerant { value: 0x115c, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADING_RATE_KHR], extensions: &["SPV_KHR_fragment_shading_rate"], version: None, last: None } },
    Enumerant { value: 0x1396, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x1402, parameters: &[], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1403, parameters: &[], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1490, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_FULLY_COVERED_EXT], extensions: &["SPV_EXT_fragment_fully_covered"], version: None, last: None } },
    Enumerant { value: 0x14a6, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_BARYCENTRIC_KHR], extensions: &["SPV_KHR_fragment_shader_barycentric"], version: None, last: None } },
    Enumerant { value: 0x14a7, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_BARYCENTRIC_KHR], extensions: &["SPV_KHR_fragment_shader_barycentric"], version: None, last: None } },
    Enumerant { value: 0x14ac, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_DENSITY_EXT], extensions: &["SPV_EXT_fragment_invocation_density"], version: None, last: None } },
    Enumerant { value: 0x14ad, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_DENSITY_EXT], extensions: &["SPV_EXT_fragment_invocation_density"], version: None, last: None } },
    Enumerant { value: 0x14ae, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14af, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14b0, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14b3, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14c7, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14c8, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14c9, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14ca, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14cb, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14cc, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14cd, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14ce, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14cf, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14d2, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14d3, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14d5, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14d7, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_POSITION_FETCH_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14e7, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14e8, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x1785, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_CULL_MASK_KHR], extensions: &["SPV_KHR_ray_cull_mask"], version: None, last: None } },
];

static CAPABILITY: [Enumerant; 181] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x5, parameters: &[], gate: CORE },
    Enumerant { value: 0x6, parameters: &[], gate: CORE },
    Enumerant { value: 0x7, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x9, parameters: &[], gate: CORE },
    Enumerant { value: 0xa, parameters: &[], gate: CORE },
    Enumerant { value: 0xb, parameters: &[], gate: CORE },
    Enumerant { value: 0xc, parameters: &[], gate: Gate { capabilities: &[Capability::INT64], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xd, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xe, parameters: &[], gate: Gate { capabilities: &[Capability::IMAGE_BASIC], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xf, parameters: &[], gate: Gate { capabilities: &[Capability::IMAGE_BASIC], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x11, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x12, parameters: &[], gate: CORE },
    Enumerant { value: 0x13, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x14, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x15, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x16, parameters: &[], gate: CORE },
    Enumerant { value: 0x17, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x18, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x19, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1b, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1c, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1d, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1e, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1f, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x20, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x21, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x22, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_CUBE_ARRAY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x23, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x24, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_RECT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x25, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x26, parameters: &[], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x27, parameters: &[], gate: CORE },
    Enumerant { value: 0x28, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x29, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2a, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2b, parameters: &[], gate: CORE },
    Enumerant { value: 0x2c, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED1D], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2d, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2e, parameters: &[], gate: CORE },
    Enumerant { value: 0x2f, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_BUFFER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x30, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x31, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x32, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x33, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x34, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x35, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x36, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x37, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x38, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x39, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3a, parameters: &[], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x3b, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x3c, parameters: &[], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x3d, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x3e, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x3f, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x40, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x41, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x42, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x43, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x44, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x45, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_5, last: None } },
    Enumerant { value: 0x46, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_5, last: None } },
    Enumerant { value: 0x47, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_6, last: None } },
    Enumerant { value: 0x1046, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_tile_image"], version: None, last: None } },
    Enumerant { value: 0x1047, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_tile_image"], version: None, last: None } },
    Enumerant { value: 0x1048, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_tile_image"], version: None, last: None } },
    Enumerant { value: 0x1074, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_float8"], version: None, last: None } },
    Enumerant { value: 0x1075, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT8_EXT, Capability::COOPERATIVE_MATRIX_KHR], extensions: &["SPV_EXT_float8"], version: None, last: None } },
    Enumerant { value: 0x1084, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_ocp_microscaling_types"], version: None, last: None } },
    Enumerant { value: 0x1085, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_ocp_microscaling_types"], version: None, last: None } },
    Enumerant { value: 0x1086, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_ocp_microscaling_types"], version: None, last: None } },
    Enumerant { value: 0x1087, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_ocp_microscaling_types"], version: None, last: None } },
    Enumerant { value: 0x1088, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_ocp_microscaling_types"], version: None, last: None } },
    Enumerant { value: 0x1146, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_fragment_shading_rate"], version: None, last: None } },
    Enumerant { value: 0x1147, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_shader_ballot"], version: None, last: None } },
    Enumerant { value: 0x114b, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_shader_draw_parameters"], version: V1_3, last: None } },
    Enumerant { value: 0x114c, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_workgroup_memory_explicit_layout"], version: None, last: None } },
    Enumerant { value: 0x114d, parameters: &[], gate: Gate { capabilities: &[Capability::WORKGROUP_MEMORY_EXPLICIT_LAYOUT_KHR], extensions: &["SPV_KHR_workgroup_memory_explicit_layout"], version: None, last: None } },
    Enumerant { value: 0x114e, parameters: &[], gate: Gate { capabilities: &[Capability::WORKGROUP_MEMORY_EXPLICIT_LAYOUT_KHR], extensions: &["SPV_KHR_workgroup_memory_explicit_layout"], version: None, last: None } },
    Enumerant { value: 0x114f, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_subgroup_vote"], version: None, last: None } },
    Enumerant { value: 0x1151, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_16bit_storage"], version: V1_3, last: None } },
    Enumerant { value: 0x1152, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_BUFFER16_BIT_ACCESS], extensions: &["SPV_KHR_16bit_storage"], version: V1_3, last: None } },
    Enumerant { value: 0x1153, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_16bit_storage"], version: V1_3, last: None } },
    Enumerant { value: 0x1154, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_16bit_storage"], version: V1_3, last: None } },
    Enumerant { value: 0x1155, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_device_group"], version: V1_3, last: None } },
    Enumerant { value: 0x1157, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_multiview"], version: V1_3, last: None } },
    Enumerant { value: 0x1159, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_variable_pointers"], version: V1_3, last: None } },
    Enumerant { value: 0x115a, parameters: &[], gate: Gate { capabilities: &[Capability::VARIABLE_POINTERS_STORAGE_BUFFER], extensions: &["SPV_KHR_variable_pointers"], version: V1_3, last: None } },
    Enumerant { value: 0x115d, parameters: &[], gate: Gate { capabilities: &[Capability::ATOMIC_STORAGE], extensions: &["SPV_KHR_shader_atomic_counter_ops"], version: None, last: None } },
    Enumerant { value: 0x115f, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_post_depth_coverage"], version: None, last: None } },
    Enumerant { value: 0x1160, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_8bit_storage"], version: V1_5, last: None } },
    Enumerant { value: 0x1161, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_BUFFER8_BIT_ACCESS], extensions: &["SPV_KHR_8bit_storage"], version: V1_5, last: None } },
    Enumerant { value: 0x1162, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_8bit_storage"], version: V1_5, last: None } },
    Enumerant { value: 0x1170, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x1171, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x1172, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x1173, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x1174, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x1177, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Enumerant { value: 0x1178, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Enumerant { value: 0x1179, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_untyped_pointers"], version: None, last: None } },
    Enumerant { value: 0x117e, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR, Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_query", "SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x117f, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x1395, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x1398, parameters: &[], gate: Gate { capabilities: &[Capability::INT64], extensions: &["SPV_EXT_shader_image_int64"], version: None, last: None } },
    Enumerant { value: 0x13bf, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_shader_clock"], version: None, last: None } },
    Enumerant { value: 0x13df, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_quad_control"], version: None, last: None } },
    Enumerant { value: 0x13fc, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_bfloat16"], version: None, last: None } },
    Enumerant { value: 0x13fd, parameters: &[], gate: Gate { capabilities: &[Capability::B_FLOAT16_TYPE_KHR], extensions: &["SPV_KHR_bfloat16"], version: None, last: None } },
    Enumerant { value: 0x13fe, parameters: &[], gate: Gate { capabilities: &[Capability::B_FLOAT16_TYPE_KHR, Capability::COOPERATIVE_MATRIX_KHR], extensions: &["SPV_KHR_bfloat16"], version: None, last: None } },
    Enumerant { value: 0x1400, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_abort"], version: None, last: None } },
    Enumerant { value: 0x1408, parameters: &[], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &["SPV_EXT_descriptor_heap"], version: None, last: None } },
    Enumerant { value: 0x141a, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_constant_data"], version: None, last: None } },
    Enumerant { value: 0x1486, parameters: &[], gate: Gate { capabilities: &[Capability::MULTI_VIEWPORT], extensions: &["SPV_EXT_shader_viewport_index_layer"], version: None, last: None } },
    Enumerant { value: 0x1491, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_fragment_fully_covered"], version: None, last: None } },
    Enumerant { value: 0x14a3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14a4, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_fragment_shader_barycentric"], version: None, last: None } },
    Enumerant { value: 0x14a8, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_compute_shader_derivatives"], version: None, last: None } },
    Enumerant { value: 0x14ab, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_fragment_invocation_density"], version: None, last: None } },
    Enumerant { value: 0x14b1, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_subgroup_partitioned"], version: None, last: None } },
    Enumerant { value: 0x14b5, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14b6, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14b7, parameters: &[], gate: Gate { capabilities: &[Capability::INPUT_ATTACHMENT], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14b8, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_BUFFER], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14b9, parameters: &[], gate: Gate { capabilities: &[Capability::IMAGE_BUFFER], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14ba, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14bb, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14bc, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14bd, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14be, parameters: &[], gate: Gate { capabilities: &[Capability::INPUT_ATTACHMENT, Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14bf, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_BUFFER, Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14c0, parameters: &[], gate: Gate { capabilities: &[Capability::IMAGE_BUFFER, Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14d8, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_ray_tracing_position_fetch"], version: None, last: None } },
    Enumerant { value: 0x14e1, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x14e2, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x14e3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_physical_storage_buffer", "SPV_KHR_physical_storage_buffer"], version: V1_5, last: None } },
    Enumerant { value: 0x14e6, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_compute_shader_derivatives"], version: None, last: None } },
    Enumerant { value: 0x14e9, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14f3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x14fc, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x1502, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x1503, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_EXT_demote_to_helper_invocation"], version: V1_6, last: None } },
    Enumerant { value: 0x1505, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_opacity_micromap", "SPV_EXT_opacity_micromap"], version: None, last: None } },
    Enumerant { value: 0x150c, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_EXT_shader_invocation_reorder"], version: None, last: None } },
    Enumerant { value: 0x150f, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_ray_tracing_position_fetch"], version: None, last: None } },
    Enumerant { value: 0x1531, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_long_vector"], version: None, last: None } },
    Enumerant { value: 0x1532, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_64bit_indexing"], version: None, last: None } },
    Enumerant { value: 0x1535, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_cooperative_matrix_maintenance1"], version: None, last: None } },
    Enumerant { value: 0x1536, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_cooperative_matrix_maintenance1"], version: None, last: None } },
    Enumerant { value: 0x1538, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_cooperative_matrix_maintenance1"], version: None, last: None } },
    Enumerant { value: 0x153e, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_cooperative_matrix_maintenance1"], version: None, last: None } },
    Enumerant { value: 0x15ec, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_atomic_float_min_max"], version: None, last: None } },
    Enumerant { value: 0x15ed, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_atomic_float_min_max"], version: None, last: None } },
    Enumerant { value: 0x15f0, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_atomic_float_min_max"], version: None, last: None } },
    Enumerant { value: 0x15fd, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_expect_assume"], version: None, last: None } },
    Enumerant { value: 0x1780, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Enumerant { value: 0x1781, parameters: &[], gate: Gate { capabilities: &[Capability::INT8], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Enumerant { value: 0x1782, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Enumerant { value: 0x1783, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Enumerant { value: 0x1784, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_ray_cull_mask"], version: None, last: None } },
    Enumerant { value: 0x1786, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_cooperative_matrix"], version: None, last: None } },
    Enumerant { value: 0x1788, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_replicated_composites"], version: None, last: None } },
    Enumerant { value: 0x1789, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_bit_instructions"], version: None, last: None } },
    Enumerant { value: 0x178a, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &["SPV_KHR_subgroup_rotate"], version: None, last: None } },
    Enumerant { value: 0x178d, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_float_controls2"], version: None, last: None } },
    Enumerant { value: 0x178e, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_fma"], version: None, last: None } },
    Enumerant { value: 0x1790, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_opacity_micromap"], version: None, last: None } },
    Enumerant { value: 0x1791, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_atomic_float_add"], version: None, last: None } },
    Enumerant { value: 0x1792, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_atomic_float_add"], version: None, last: None } },
    Enumerant { value: 0x17ce, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_optnone"], version: None, last: None } },
    Enumerant { value: 0x17cf, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_shader_atomic_float16_add"], version: None, last: None } },
    Enumerant { value: 0x17fd, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_split_barrier"], version: None, last: None } },
    Enumerant { value: 0x1800, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_EXT_arithmetic_fence"], version: None, last: None } },
    Enumerant { value: 0x1900, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_uniform_group_instructions"], version: None, last: None } },
];

static COOPERATIVE_MATRIX_OPERANDS: [Enumerant; 6] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x10, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
];

static COOPERATIVE_MATRIX_REDUCE: [Enumerant; 3] = [
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: None, last: None } },
];

static DECORATION: [Enumerant; 62] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER, Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: V1_3 } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x7, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x9, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xa, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xb, parameters: &[Kind::BuiltIn], gate: CORE },
    Enumerant { value: 0xd, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xe, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xf, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x10, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x11, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLE_RATE_SHADING], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x12, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x13, parameters: &[], gate: CORE },
    Enumerant { value: 0x14, parameters: &[], gate: CORE },
    Enumerant { value: 0x15, parameters: &[], gate: CORE },
    Enumerant { value: 0x16, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x17, parameters: &[], gate: CORE },
    Enumerant { value: 0x18, parameters: &[], gate: CORE },
    Enumerant { value: 0x19, parameters: &[], gate: CORE },
    Enumerant { value: 0x1a, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER, Capability::UNIFORM_DECORATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1b, parameters: &[Kind::IdScope], gate: Gate { capabilities: &[Capability::SHADER, Capability::UNIFORM_DECORATION], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x1c, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1d, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::GEOMETRY_STREAMS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1e, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1f, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x20, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x21, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x22, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x23, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x24, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::TRANSFORM_FEEDBACK], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x25, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::TRANSFORM_FEEDBACK], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x26, parameters: &[Kind::FunctionParameterAttribute], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x27, parameters: &[Kind::FPRoundingMode], gate: CORE },
    Enumerant { value: 0x28, parameters: &[Kind::FPFastMathMode], gate: Gate { capabilities: &[Capability::KERNEL, Capability::FLOAT_CONTROLS2], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x29, parameters: &[Kind::LiteralString, Kind::LinkageType], gate: Gate { capabilities: &[Capability::LINKAGE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2a, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2b, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::INPUT_ATTACHMENT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2c, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2d, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x2e, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_2, last: None } },
    Enumerant { value: 0x2f, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_2, last: None } },
    Enumerant { value: 0x1078, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT8_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1175, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_no_integer_wrap_decoration"], version: V1_4, last: None } },
    Enumerant { value: 0x1176, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_no_integer_wrap_decoration"], version: V1_4, last: None } },
    Enumerant { value: 0x1404, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1405, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1419, parameters: &[], gate: Gate { capabilities: &[Capability::CONSTANT_DATA_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1497, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14a5, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_BARYCENTRIC_KHR], extensions: &["SPV_KHR_fragment_shader_barycentric"], version: None, last: None } },
    Enumerant { value: 0x14b4, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_NON_UNIFORM], extensions: &["SPV_EXT_descriptor_indexing"], version: V1_5, last: None } },
    Enumerant { value: 0x14eb, parameters: &[], gate: Gate { capabilities: &[Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &["SPV_EXT_physical_storage_buffer", "SPV_KHR_physical_storage_buffer"], version: V1_5, last: None } },
    Enumerant { value: 0x14ec, parameters: &[], gate: Gate { capabilities: &[Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &["SPV_EXT_physical_storage_buffer", "SPV_KHR_physical_storage_buffer"], version: V1_5, last: None } },
    Enumerant { value: 0x150d, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1540, parameters: &[], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_CONVERSIONS_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1602, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x1603, parameters: &[Kind::LiteralString], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
];

static DIM: [Enumerant; 8] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED1D], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_RECT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLED_BUFFER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::INPUT_ATTACHMENT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x104d, parameters: &[], gate: Gate { capabilities: &[Capability::TILE_IMAGE_COLOR_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
];

static EXECUTION_MODE: [Enumerant; 72] = [
    Enumerant { value: 0x0, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x7, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x9, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xa, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xb, parameters: &[], gate: Gate { capabilities: &[Capability::TRANSFORM_FEEDBACK], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xc, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xe, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xf, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x10, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x11, parameters: &[Kind::LiteralInteger, Kind::LiteralInteger, Kind::LiteralInteger], gate: CORE },
    Enumerant { value: 0x12, parameters: &[Kind::LiteralInteger, Kind::LiteralInteger, Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x13, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x14, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x15, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x16, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY, Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x17, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x18, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x19, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1a, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::GEOMETRY, Capability::TESSELLATION, Capability::MESH_SHADING_EXT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1b, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY, Capability::MESH_SHADING_EXT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1c, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1d, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1e, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1f, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x21, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x22, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x23, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SUBGROUP_DISPATCH], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x24, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SUBGROUP_DISPATCH], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x25, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::SUBGROUP_DISPATCH], extensions: &[], version: V1_2, last: None } },
    Enumerant { value: 0x26, parameters: &[Kind::IdRef, Kind::IdRef, Kind::IdRef], gate: Gate { capabilities: &[], extensions: &[], version: V1_2, last: None } },
    Enumerant { value: 0x27, parameters: &[Kind::IdRef, Kind::IdRef, Kind::IdRef], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_2, last: None } },
    Enumerant { value: 0x1049, parameters: &[], gate: Gate { capabilities: &[Capability::TILE_IMAGE_COLOR_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x104a, parameters: &[], gate: Gate { capabilities: &[Capability::TILE_IMAGE_DEPTH_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x104b, parameters: &[], gate: Gate { capabilities: &[Capability::TILE_IMAGE_STENCIL_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1145, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_subgroup_uniform_control_flow"], version: None, last: None } },
    Enumerant { value: 0x115e, parameters: &[], gate: Gate { capabilities: &[Capability::SAMPLE_MASK_POST_DEPTH_COVERAGE], extensions: &["SPV_KHR_post_depth_coverage"], version: None, last: None } },
    Enumerant { value: 0x116b, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::DENORM_PRESERVE], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x116c, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::DENORM_FLUSH_TO_ZERO], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x116d, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::SIGNED_ZERO_INF_NAN_PRESERVE], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x116e, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::ROUNDING_MODE_RTE], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x116f, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::ROUNDING_MODE_RTZ], extensions: &["SPV_KHR_float_controls"], version: V1_4, last: None } },
    Enumerant { value: 0x13a3, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13d7, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13d8, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13d9, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13da, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13db, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13dc, parameters: &[], gate: Gate { capabilities: &[Capability::STENCIL_EXPORT_EXT], extensions: &["SPV_EXT_shader_stencil_export"], version: None, last: None } },
    Enumerant { value: 0x13e0, parameters: &[], gate: Gate { capabilities: &[Capability::QUAD_CONTROL_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x13e1, parameters: &[], gate: Gate { capabilities: &[Capability::QUAD_CONTROL_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1495, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x1496, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14a9, parameters: &[], gate: Gate { capabilities: &[Capability::COMPUTE_DERIVATIVE_GROUP_QUADS_KHR], extensions: &["SPV_KHR_compute_shader_derivatives"], version: None, last: None } },
    Enumerant { value: 0x14aa, parameters: &[], gate: Gate { capabilities: &[Capability::COMPUTE_DERIVATIVE_GROUP_LINEAR_KHR], extensions: &["SPV_KHR_compute_shader_derivatives"], version: None, last: None } },
    Enumerant { value: 0x14b2, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: None, last: None } },
    Enumerant { value: 0x14f6, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_PIXEL_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x14f7, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_PIXEL_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x14f8, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_SAMPLE_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x14f9, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_SAMPLE_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x14fa, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_SHADING_RATE_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x14fb, parameters: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_SHADING_RATE_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Enumerant { value: 0x1533, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER64_BIT_INDEXING_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1787, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_maximal_reconvergence"], version: None, last: None } },
    Enumerant { value: 0x178c, parameters: &[Kind::IdRef, Kind::IdRef], gate: Gate { capabilities: &[Capability::FLOAT_CONTROLS2], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x178f, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::RAY_TRACING_OPACITY_MICROMAP_EXECUTION_MODE_KHR], extensions: &[], version: None, last: None } },
];

static EXECUTION_MODEL: [Enumerant; 15] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::TESSELLATION], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x14c1, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14c2, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14c3, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14c4, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14c5, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14c6, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14f4, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14f5, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &[], version: None, last: None } },
];

static FP_ENCODING: [Enumerant; 8] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::B_FLOAT16_TYPE_KHR], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1076, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT8_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1077, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT8_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x107f, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT6_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1080, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT6_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1081, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT4_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1082, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT8_UNSIGNED_E8_M0_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x1083, parameters: &[], gate: Gate { capabilities: &[Capability::MX_INT8_EXT], extensions: &[], version: None, last: None } },
];

static FP_FAST_MATH_MODE: [Enumerant; 9] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x8, parameters: &[], gate: CORE },
    Enumerant { value: 0x10, parameters: &[], gate: CORE },
    Enumerant { value: 0x1_0000, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT_CONTROLS2], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x2_0000, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT_CONTROLS2], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x4_0000, parameters: &[], gate: Gate { capabilities: &[Capability::FLOAT_CONTROLS2], extensions: &[], version: None, last: None } },
];

static FP_ROUNDING_MODE: [Enumerant; 4] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x3, parameters: &[], gate: CORE },
];

static FUNCTION_CONTROL: [Enumerant; 6] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x8, parameters: &[], gate: CORE },
    Enumerant { value: 0x1_0000, parameters: &[], gate: Gate { capabilities: &[Capability::OPT_NONE_EXT], extensions: &[], version: None, last: None } },
];

static FUNCTION_PARAMETER_ATTRIBUTE: [Enumerant; 8] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x7, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
];

static GROUP_OPERATION: [Enumerant; 7] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL, Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_CLUSTERED], extensions: &[], version: V1_3, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x7, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: None, last: None } },
];

static IMAGE_FORMAT: [Enumerant; 42] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x7, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x9, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xa, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xb, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xc, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xd, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xe, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xf, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x10, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x11, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x12, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x13, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x14, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x15, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x16, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x17, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x18, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x19, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1a, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1b, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1c, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1d, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1e, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1f, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x20, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x21, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x22, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x23, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x24, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x25, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x26, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x27, parameters: &[], gate: Gate { capabilities: &[Capability::STORAGE_IMAGE_EXTENDED_FORMATS], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x28, parameters: &[], gate: Gate { capabilities: &[Capability::INT64_IMAGE_EXT], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x29, parameters: &[], gate: Gate { capabilities: &[Capability::INT64_IMAGE_EXT], extensions: &[], version: V1_0, last: None } },
];

static IMAGE_OPERANDS: [Enumerant; 17] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[Kind::IdRef], gate: CORE },
    Enumerant { value: 0x4, parameters: &[Kind::IdRef, Kind::IdRef], gate: CORE },
    Enumerant { value: 0x8, parameters: &[Kind::IdRef], gate: CORE },
    Enumerant { value: 0x10, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::IMAGE_GATHER_EXTENDED], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x20, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::IMAGE_GATHER_EXTENDED], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x40, parameters: &[Kind::IdRef], gate: CORE },
    Enumerant { value: 0x80, parameters: &[Kind::IdRef], gate: Gate { capabilities: &[Capability::MIN_LOD], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x100, parameters: &[Kind::IdScope], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x200, parameters: &[Kind::IdScope], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x400, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x800, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x1000, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x2000, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x4000, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_6, last: None } },
    Enumerant { value: 0x1_0000, parameters: &[Kind::IdRef], gate: CORE },
];

static LINKAGE_TYPE: [Enumerant; 3] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::LINKAGE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::LINKAGE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::LINKAGE], extensions: &["SPV_KHR_linkonce_odr"], version: None, last: None } },
];

static LOOP_CONTROL: [Enumerant; 11] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: Gate { capabilities: &[], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x8, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[], extensions: &[], version: V1_1, last: None } },
    Enumerant { value: 0x10, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x20, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x40, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x80, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x100, parameters: &[Kind::LiteralInteger], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Enumerant { value: 0x1000_0000, parameters: &[Kind::LiteralInteger], gate: CORE },
];

static MEMORY_ACCESS: [Enumerant; 7] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[Kind::LiteralInteger], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x8, parameters: &[Kind::IdScope], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x10, parameters: &[Kind::IdScope], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x20, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
];

static MEMORY_MODEL: [Enumerant; 4] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x1, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
];

static MEMORY_SEMANTICS: [Enumerant; 15] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x8, parameters: &[], gate: CORE },
    Enumerant { value: 0x10, parameters: &[], gate: CORE },
    Enumerant { value: 0x40, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x80, parameters: &[], gate: CORE },
    Enumerant { value: 0x100, parameters: &[], gate: CORE },
    Enumerant { value: 0x200, parameters: &[], gate: CORE },
    Enumerant { value: 0x400, parameters: &[], gate: Gate { capabilities: &[Capability::ATOMIC_STORAGE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x800, parameters: &[], gate: CORE },
    Enumerant { value: 0x1000, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x2000, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x4000, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
    Enumerant { value: 0x8000, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &["SPV_KHR_vulkan_memory_model"], version: V1_5, last: None } },
];

static PACKED_VECTOR_FORMAT: [Enumerant; 1] = [
    Enumerant { value: 0x0, parameters: &[], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
];

static SAMPLER_ADDRESSING_MODE: [Enumerant; 5] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x3, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
];

static SAMPLER_FILTER_MODE: [Enumerant; 2] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
];

static SCOPE: [Enumerant; 7] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x3, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x5, parameters: &[], gate: Gate { capabilities: &[Capability::VULKAN_MEMORY_MODEL], extensions: &[], version: V1_5, last: None } },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &[], version: None, last: None } },
];

static SELECTION_CONTROL: [Enumerant; 3] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
];

static SOURCE_LANGUAGE: [Enumerant; 16] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: CORE },
    Enumerant { value: 0x3, parameters: &[], gate: CORE },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x5, parameters: &[], gate: CORE },
    Enumerant { value: 0x6, parameters: &[], gate: CORE },
    Enumerant { value: 0x7, parameters: &[], gate: CORE },
    Enumerant { value: 0x8, parameters: &[], gate: CORE },
    Enumerant { value: 0x9, parameters: &[], gate: CORE },
    Enumerant { value: 0xa, parameters: &[], gate: CORE },
    Enumerant { value: 0xb, parameters: &[], gate: CORE },
    Enumerant { value: 0xc, parameters: &[], gate: CORE },
    Enumerant { value: 0xd, parameters: &[], gate: CORE },
    Enumerant { value: 0xe, parameters: &[], gate: CORE },
    Enumerant { value: 0xf, parameters: &[], gate: CORE },
];

static STORAGE_CLASS: [Enumerant; 23] = [
    Enumerant { value: 0x0, parameters: &[], gate: CORE },
    Enumerant { value: 0x1, parameters: &[], gate: CORE },
    Enumerant { value: 0x2, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x3, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x4, parameters: &[], gate: CORE },
    Enumerant { value: 0x5, parameters: &[], gate: CORE },
    Enumerant { value: 0x6, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x7, parameters: &[], gate: CORE },
    Enumerant { value: 0x8, parameters: &[], gate: Gate { capabilities: &[Capability::GENERIC_POINTER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0x9, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xa, parameters: &[], gate: Gate { capabilities: &[Capability::ATOMIC_STORAGE], extensions: &[], version: V1_0, last: None } },
    Enumerant { value: 0xb, parameters: &[], gate: CORE },
    Enumerant { value: 0xc, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_storage_buffer_storage_class", "SPV_KHR_variable_pointers"], version: V1_3, last: None } },
    Enumerant { value: 0x104c, parameters: &[], gate: Gate { capabilities: &[Capability::TILE_IMAGE_COLOR_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Enumerant { value: 0x14d0, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14d1, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14da, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14db, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14de, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14df, parameters: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Enumerant { value: 0x14e5, parameters: &[], gate: Gate { capabilities: &[Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &["SPV_EXT_physical_storage_buffer", "SPV_KHR_physical_storage_buffer"], version: V1_5, last: None } },
    Enumerant { value: 0x151a, parameters: &[], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &["SPV_EXT_mesh_shader"], version: V1_4, last: None } },
    Enumerant { value: 0x1523, parameters: &[], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
];

/// Every instruction recognised, by ascending opcode.
pub(crate) static INSTRUCTIONS: [Spec; 490] = [
    Spec { op: Op(0), name: "OpNop", operands: &[], gate: CORE },
    Spec { op: Op(1), name: "OpUndef", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(2), name: "OpSourceContinued", operands: &[(Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(3), name: "OpSource", operands: &[(Kind::SourceLanguage, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::LiteralString, Quantity::Optional)], gate: CORE },
    Spec { op: Op(4), name: "OpSourceExtension", operands: &[(Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(5), name: "OpName", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(6), name: "OpMemberName", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(7), name: "OpString", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(8), name: "OpLine", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: CORE },
    Spec { op: Op(10), name: "OpExtension", operands: &[(Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(11), name: "OpExtInstImport", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralString, Quantity::One)], gate: CORE },
    Spec { op: Op(12), name: "OpExtInst", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralExtInstInteger, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(14), name: "OpMemoryModel", operands: &[(Kind::AddressingModel, Quantity::One), (Kind::MemoryModel, Quantity::One)], gate: CORE },
    Spec { op: Op(15), name: "OpEntryPoint", operands: &[(Kind::ExecutionModel, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralString, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(16), name: "OpExecutionMode", operands: &[(Kind::IdRef, Quantity::One), (Kind::ExecutionMode, Quantity::One)], gate: CORE },
    Spec { op: Op(17), name: "OpCapability", operands: &[(Kind::Capability, Quantity::One)], gate: CORE },
    Spec { op: Op(19), name: "OpTypeVoid", operands: &[(Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(20), name: "OpTypeBool", operands: &[(Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(21), name: "OpTypeInt", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: CORE },
    Spec { op: Op(22), name: "OpTypeFloat", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::FPEncoding, Quantity::Optional)], gate: CORE },
    Spec { op: Op(23), name: "OpTypeVector", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: CORE },
    Spec { op: Op(24), name: "OpTypeMatrix", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(25), name: "OpTypeImage", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::Dim, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::ImageFormat, Quantity::One), (Kind::AccessQualifier, Quantity::Optional)], gate: CORE },
    Spec { op: Op(26), name: "OpTypeSampler", operands: &[(Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(27), name: "OpTypeSampledImage", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(28), name: "OpTypeArray", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(29), name: "OpTypeRuntimeArray", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(30), name: "OpTypeStruct", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(31), name: "OpTypeOpaque", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralString, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(32), name: "OpTypePointer", operands: &[(Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(33), name: "OpTypeFunction", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(34), name: "OpTypeEvent", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(35), name: "OpTypeDeviceEvent", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(36), name: "OpTypeReserveId", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(37), name: "OpTypeQueue", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(38), name: "OpTypePipe", operands: &[(Kind::IdResult, Quantity::One), (Kind::AccessQualifier, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(39), name: "OpTypeForwardPointer", operands: &[(Kind::IdRef, Quantity::One), (Kind::StorageClass, Quantity::One)], gate: Gate { capabilities: &[Capability::ADDRESSES, Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(41), name: "OpConstantTrue", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(42), name: "OpConstantFalse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(43), name: "OpConstant", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralContextDependentNumber, Quantity::One)], gate: CORE },
    Spec { op: Op(44), name: "OpConstantComposite", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(45), name: "OpConstantSampler", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::SamplerAddressingMode, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::SamplerFilterMode, Quantity::One)], gate: Gate { capabilities: &[Capability::LITERAL_SAMPLER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(46), name: "OpConstantNull", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(48), name: "OpSpecConstantTrue", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(49), name: "OpSpecConstantFalse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(50), name: "OpSpecConstant", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralContextDependentNumber, Quantity::One)], gate: CORE },
    Spec { op: Op(51), name: "OpSpecConstantComposite", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(52), name: "OpSpecConstantOp", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralSpecConstantOpInteger, Quantity::One)], gate: CORE },
    Spec { op: Op(54), name: "OpFunction", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::FunctionControl, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(55), name: "OpFunctionParameter", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(56), name: "OpFunctionEnd", operands: &[], gate: CORE },
    Spec { op: Op(57), name: "OpFunctionCall", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(59), name: "OpVariable", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: CORE },
    Spec { op: Op(60), name: "OpImageTexelPointer", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(61), name: "OpLoad", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional)], gate: CORE },
    Spec { op: Op(62), name: "OpStore", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional)], gate: CORE },
    Spec { op: Op(63), name: "OpCopyMemory", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)], gate: CORE },
    Spec { op: Op(64), name: "OpCopyMemorySized", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)], gate: Gate { capabilities: &[Capability::ADDRESSES, Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(65), name: "OpAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(66), name: "OpInBoundsAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(67), name: "OpPtrAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::ADDRESSES, Capability::VARIABLE_POINTERS, Capability::VARIABLE_POINTERS_STORAGE_BUFFER, Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(68), name: "OpArrayLength", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(69), name: "OpGenericPtrMemSemantics", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(70), name: "OpInBoundsPtrAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(71), name: "OpDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::Decoration, Quantity::One)], gate: CORE },
    Spec { op: Op(72), name: "OpMemberDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::Decoration, Quantity::One)], gate: CORE },
    Spec { op: Op(73), name: "OpDecorationGroup", operands: &[(Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(74), name: "OpGroupDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(75), name: "OpGroupMemberDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::PairIdRefLiteralInteger, Quantity::Many)], gate: CORE },
    Spec { op: Op(77), name: "OpVectorExtractDynamic", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(78), name: "OpVectorInsertDynamic", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(79), name: "OpVectorShuffle", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)], gate: CORE },
    Spec { op: Op(80), name: "OpCompositeConstruct", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(81), name: "OpCompositeExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)], gate: CORE },
    Spec { op: Op(82), name: "OpCompositeInsert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)], gate: CORE },
    Spec { op: Op(83), name: "OpCopyObject", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(84), name: "OpTranspose", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(86), name: "OpSampledImage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(87), name: "OpImageSampleImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(88), name: "OpImageSampleExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: CORE },
    Spec { op: Op(89), name: "OpImageSampleDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(90), name: "OpImageSampleDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(91), name: "OpImageSampleProjImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(92), name: "OpImageSampleProjExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(93), name: "OpImageSampleProjDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(94), name: "OpImageSampleProjDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(95), name: "OpImageFetch", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: CORE },
    Spec { op: Op(96), name: "OpImageGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(97), name: "OpImageDrefGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(98), name: "OpImageRead", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: CORE },
    Spec { op: Op(99), name: "OpImageWrite", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: CORE },
    Spec { op: Op(100), name: "OpImage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(101), name: "OpImageQueryFormat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(102), name: "OpImageQueryOrder", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(103), name: "OpImageQuerySizeLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL, Capability::IMAGE_QUERY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(104), name: "OpImageQuerySize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL, Capability::IMAGE_QUERY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(105), name: "OpImageQueryLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::IMAGE_QUERY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(106), name: "OpImageQueryLevels", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL, Capability::IMAGE_QUERY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(107), name: "OpImageQuerySamples", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL, Capability::IMAGE_QUERY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(109), name: "OpConvertFToU", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(110), name: "OpConvertFToS", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(111), name: "OpConvertSToF", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(112), name: "OpConvertUToF", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(113), name: "OpUConvert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(114), name: "OpSConvert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(115), name: "OpFConvert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(116), name: "OpQuantizeToF16", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(117), name: "OpConvertPtrToU", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ADDRESSES, Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(118), name: "OpSatConvertSToU", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(119), name: "OpSatConvertUToS", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(120), name: "OpConvertUToPtr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ADDRESSES, Capability::PHYSICAL_STORAGE_BUFFER_ADDRESSES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(121), name: "OpPtrCastToGeneric", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(122), name: "OpGenericCastToPtr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(123), name: "OpGenericCastToPtrExplicit", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::StorageClass, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(124), name: "OpBitcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(126), name: "OpSNegate", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(127), name: "OpFNegate", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(128), name: "OpIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(129), name: "OpFAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(130), name: "OpISub", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(131), name: "OpFSub", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(132), name: "OpIMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(133), name: "OpFMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(134), name: "OpUDiv", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(135), name: "OpSDiv", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(136), name: "OpFDiv", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(137), name: "OpUMod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(138), name: "OpSRem", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(139), name: "OpSMod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(140), name: "OpFRem", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(141), name: "OpFMod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(142), name: "OpVectorTimesScalar", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(143), name: "OpMatrixTimesScalar", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(144), name: "OpVectorTimesMatrix", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(145), name: "OpMatrixTimesVector", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(146), name: "OpMatrixTimesMatrix", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(147), name: "OpOuterProduct", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MATRIX], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(148), name: "OpDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(149), name: "OpIAddCarry", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(150), name: "OpISubBorrow", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(151), name: "OpUMulExtended", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(152), name: "OpSMulExtended", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(154), name: "OpAny", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(155), name: "OpAll", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(156), name: "OpIsNan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(157), name: "OpIsInf", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(158), name: "OpIsFinite", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(159), name: "OpIsNormal", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(160), name: "OpSignBitSet", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(161), name: "OpLessOrGreater", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: V1_5 } },
    Spec { op: Op(162), name: "OpOrdered", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(163), name: "OpUnordered", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(164), name: "OpLogicalEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(165), name: "OpLogicalNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(166), name: "OpLogicalOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(167), name: "OpLogicalAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(168), name: "OpLogicalNot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(169), name: "OpSelect", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(170), name: "OpIEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(171), name: "OpINotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(172), name: "OpUGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(173), name: "OpSGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(174), name: "OpUGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(175), name: "OpSGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(176), name: "OpULessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(177), name: "OpSLessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(178), name: "OpULessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(179), name: "OpSLessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(180), name: "OpFOrdEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(181), name: "OpFUnordEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(182), name: "OpFOrdNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(183), name: "OpFUnordNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(184), name: "OpFOrdLessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(185), name: "OpFUnordLessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(186), name: "OpFOrdGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(187), name: "OpFUnordGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(188), name: "OpFOrdLessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(189), name: "OpFUnordLessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(190), name: "OpFOrdGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(191), name: "OpFUnordGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(194), name: "OpShiftRightLogical", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(195), name: "OpShiftRightArithmetic", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(196), name: "OpShiftLeftLogical", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(197), name: "OpBitwiseOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(198), name: "OpBitwiseXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(199), name: "OpBitwiseAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(200), name: "OpNot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(201), name: "OpBitFieldInsert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER, Capability::BIT_INSTRUCTIONS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(202), name: "OpBitFieldSExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER, Capability::BIT_INSTRUCTIONS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(203), name: "OpBitFieldUExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER, Capability::BIT_INSTRUCTIONS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(204), name: "OpBitReverse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER, Capability::BIT_INSTRUCTIONS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(205), name: "OpBitCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(207), name: "OpDPdx", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(208), name: "OpDPdy", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(209), name: "OpFwidth", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(210), name: "OpDPdxFine", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DERIVATIVE_CONTROL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(211), name: "OpDPdyFine", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DERIVATIVE_CONTROL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(212), name: "OpFwidthFine", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DERIVATIVE_CONTROL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(213), name: "OpDPdxCoarse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DERIVATIVE_CONTROL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(214), name: "OpDPdyCoarse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DERIVATIVE_CONTROL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(215), name: "OpFwidthCoarse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DERIVATIVE_CONTROL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(218), name: "OpEmitVertex", operands: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(219), name: "OpEndPrimitive", operands: &[], gate: Gate { capabilities: &[Capability::GEOMETRY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(220), name: "OpEmitStreamVertex", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GEOMETRY_STREAMS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(221), name: "OpEndStreamPrimitive", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GEOMETRY_STREAMS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(224), name: "OpControlBarrier", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: CORE },
    Spec { op: Op(225), name: "OpMemoryBarrier", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: CORE },
    Spec { op: Op(227), name: "OpAtomicLoad", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: CORE },
    Spec { op: Op(228), name: "OpAtomicStore", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(229), name: "OpAtomicExchange", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(230), name: "OpAtomicCompareExchange", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(231), name: "OpAtomicCompareExchangeWeak", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: V1_3 } },
    Spec { op: Op(232), name: "OpAtomicIIncrement", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: CORE },
    Spec { op: Op(233), name: "OpAtomicIDecrement", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: CORE },
    Spec { op: Op(234), name: "OpAtomicIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(235), name: "OpAtomicISub", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(236), name: "OpAtomicSMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(237), name: "OpAtomicUMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(238), name: "OpAtomicSMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(239), name: "OpAtomicUMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(240), name: "OpAtomicAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(241), name: "OpAtomicOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(242), name: "OpAtomicXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(245), name: "OpPhi", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::PairIdRefIdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(246), name: "OpLoopMerge", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LoopControl, Quantity::One)], gate: CORE },
    Spec { op: Op(247), name: "OpSelectionMerge", operands: &[(Kind::IdRef, Quantity::One), (Kind::SelectionControl, Quantity::One)], gate: CORE },
    Spec { op: Op(248), name: "OpLabel", operands: &[(Kind::IdResult, Quantity::One)], gate: CORE },
    Spec { op: Op(249), name: "OpBranch", operands: &[(Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(250), name: "OpBranchConditional", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)], gate: CORE },
    Spec { op: Op(251), name: "OpSwitch", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PairLiteralIntegerIdRef, Quantity::Many)], gate: CORE },
    Spec { op: Op(252), name: "OpKill", operands: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(253), name: "OpReturn", operands: &[], gate: CORE },
    Spec { op: Op(254), name: "OpReturnValue", operands: &[(Kind::IdRef, Quantity::One)], gate: CORE },
    Spec { op: Op(255), name: "OpUnreachable", operands: &[], gate: CORE },
    Spec { op: Op(256), name: "OpLifetimeStart", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(257), name: "OpLifetimeStop", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(259), name: "OpGroupAsyncCopy", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(260), name: "OpGroupWaitEvents", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(261), name: "OpGroupAll", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(262), name: "OpGroupAny", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(263), name: "OpGroupBroadcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(264), name: "OpGroupIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(265), name: "OpGroupFAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(266), name: "OpGroupFMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(267), name: "OpGroupUMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(268), name: "OpGroupSMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(269), name: "OpGroupFMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(270), name: "OpGroupUMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(271), name: "OpGroupSMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUPS], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(274), name: "OpReadPipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(275), name: "OpWritePipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(276), name: "OpReservedReadPipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(277), name: "OpReservedWritePipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(278), name: "OpReserveReadPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(279), name: "OpReserveWritePipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(280), name: "OpCommitReadPipe", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(281), name: "OpCommitWritePipe", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(282), name: "OpIsValidReserveId", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(283), name: "OpGetNumPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(284), name: "OpGetMaxPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(285), name: "OpGroupReserveReadPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(286), name: "OpGroupReserveWritePipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(287), name: "OpGroupCommitReadPipe", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(288), name: "OpGroupCommitWritePipe", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPES], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(291), name: "OpEnqueueMarker", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(292), name: "OpEnqueueKernel", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(293), name: "OpGetKernelNDrangeSubGroupCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(294), name: "OpGetKernelNDrangeMaxSubGroupSize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(295), name: "OpGetKernelWorkGroupSize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(296), name: "OpGetKernelPreferredWorkGroupSizeMultiple", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(297), name: "OpRetainEvent", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(298), name: "OpReleaseEvent", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(299), name: "OpCreateUserEvent", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(300), name: "OpIsValidEvent", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(301), name: "OpSetUserEventStatus", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(302), name: "OpCaptureEventProfilingInfo", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(303), name: "OpGetDefaultQueue", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(304), name: "OpBuildNDRange", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DEVICE_ENQUEUE], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(305), name: "OpImageSparseSampleImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(306), name: "OpImageSparseSampleExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(307), name: "OpImageSparseSampleDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(308), name: "OpImageSparseSampleDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(309), name: "OpImageSparseSampleProjImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: None, last: None } },
    Spec { op: Op(310), name: "OpImageSparseSampleProjExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: None, last: None } },
    Spec { op: Op(311), name: "OpImageSparseSampleProjDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: None, last: None } },
    Spec { op: Op(312), name: "OpImageSparseSampleProjDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: None, last: None } },
    Spec { op: Op(313), name: "OpImageSparseFetch", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(314), name: "OpImageSparseGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(315), name: "OpImageSparseDrefGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(316), name: "OpImageSparseTexelsResident", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(317), name: "OpNoLine", operands: &[], gate: CORE },
    Spec { op: Op(318), name: "OpAtomicFlagTestAndSet", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(319), name: "OpAtomicFlagClear", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: Gate { capabilities: &[Capability::KERNEL], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(320), name: "OpImageSparseRead", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SPARSE_RESIDENCY], extensions: &[], version: V1_0, last: None } },
    Spec { op: Op(321), name: "OpSizeOf", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ADDRESSES], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(322), name: "OpTypePipeStorage", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPE_STORAGE], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(323), name: "OpConstantPipeStorage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPE_STORAGE], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(324), name: "OpCreatePipeFromPipeStorage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::PIPE_STORAGE], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(325), name: "OpGetKernelLocalSizeForSubgroupCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_DISPATCH], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(326), name: "OpGetKernelMaxNumSubgroups", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_DISPATCH], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(327), name: "OpTypeNamedBarrier", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::NAMED_BARRIER], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(328), name: "OpNamedBarrierInitialize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::NAMED_BARRIER], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(329), name: "OpMemoryNamedBarrier", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: Gate { capabilities: &[Capability::NAMED_BARRIER], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(330), name: "OpModuleProcessed", operands: &[(Kind::LiteralString, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_1, last: None } },
    Spec { op: Op(331), name: "OpExecutionModeId", operands: &[(Kind::IdRef, Quantity::One), (Kind::ExecutionMode, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_2, last: None } },
    Spec { op: Op(332), name: "OpDecorateId", operands: &[(Kind::IdRef, Quantity::One), (Kind::Decoration, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_2, last: None } },
    Spec { op: Op(333), name: "OpGroupNonUniformElect", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(334), name: "OpGroupNonUniformAll", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_VOTE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(335), name: "OpGroupNonUniformAny", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_VOTE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(336), name: "OpGroupNonUniformAllEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_VOTE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(337), name: "OpGroupNonUniformBroadcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(338), name: "OpGroupNonUniformBroadcastFirst", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(339), name: "OpGroupNonUniformBallot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(340), name: "OpGroupNonUniformInverseBallot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(341), name: "OpGroupNonUniformBallotBitExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(342), name: "OpGroupNonUniformBallotBitCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(343), name: "OpGroupNonUniformBallotFindLSB", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(344), name: "OpGroupNonUniformBallotFindMSB", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_BALLOT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(345), name: "OpGroupNonUniformShuffle", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_SHUFFLE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(346), name: "OpGroupNonUniformShuffleXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_SHUFFLE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(347), name: "OpGroupNonUniformShuffleUp", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_SHUFFLE_RELATIVE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(348), name: "OpGroupNonUniformShuffleDown", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_SHUFFLE_RELATIVE], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(349), name: "OpGroupNonUniformIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(350), name: "OpGroupNonUniformFAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(351), name: "OpGroupNonUniformIMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(352), name: "OpGroupNonUniformFMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(353), name: "OpGroupNonUniformSMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(354), name: "OpGroupNonUniformUMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(355), name: "OpGroupNonUniformFMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(356), name: "OpGroupNonUniformSMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(357), name: "OpGroupNonUniformUMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(358), name: "OpGroupNonUniformFMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(359), name: "OpGroupNonUniformBitwiseAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(360), name: "OpGroupNonUniformBitwiseOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(361), name: "OpGroupNonUniformBitwiseXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(362), name: "OpGroupNonUniformLogicalAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(363), name: "OpGroupNonUniformLogicalOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(364), name: "OpGroupNonUniformLogicalXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ARITHMETIC, Capability::GROUP_NON_UNIFORM_CLUSTERED, Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(365), name: "OpGroupNonUniformQuadBroadcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_QUAD], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(366), name: "OpGroupNonUniformQuadSwap", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_QUAD], extensions: &[], version: V1_3, last: None } },
    Spec { op: Op(400), name: "OpCopyLogical", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Spec { op: Op(401), name: "OpPtrEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Spec { op: Op(402), name: "OpPtrNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Spec { op: Op(403), name: "OpPtrDiff", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ADDRESSES, Capability::VARIABLE_POINTERS, Capability::VARIABLE_POINTERS_STORAGE_BUFFER], extensions: &[], version: V1_4, last: None } },
    Spec { op: Op(4160), name: "OpColorAttachmentReadEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::TILE_IMAGE_COLOR_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4161), name: "OpDepthAttachmentReadEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::TILE_IMAGE_DEPTH_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4162), name: "OpStencilAttachmentReadEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::TILE_IMAGE_STENCIL_READ_ACCESS_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4195), name: "OpBitcastExtractEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::BITCAST_EXTRACT_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4416), name: "OpTerminateInvocation", operands: &[], gate: Gate { capabilities: &[Capability::SHADER], extensions: &["SPV_KHR_terminate_invocation"], version: V1_6, last: None } },
    Spec { op: Op(4417), name: "OpTypeUntypedPointerKHR", operands: &[(Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4418), name: "OpUntypedVariableKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4419), name: "OpUntypedAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4420), name: "OpUntypedInBoundsAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4421), name: "OpSubgroupBallotKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR], extensions: &["SPV_KHR_shader_ballot"], version: None, last: None } },
    Spec { op: Op(4422), name: "OpSubgroupFirstInvocationKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR], extensions: &["SPV_KHR_shader_ballot"], version: None, last: None } },
    Spec { op: Op(4423), name: "OpUntypedPtrAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4424), name: "OpUntypedInBoundsPtrAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4425), name: "OpUntypedArrayLengthKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4426), name: "OpUntypedPrefetchKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4427), name: "OpFmaKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::FMAKHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4428), name: "OpSubgroupAllKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_VOTE_KHR], extensions: &["SPV_KHR_subgroup_vote"], version: None, last: None } },
    Spec { op: Op(4429), name: "OpSubgroupAnyKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_VOTE_KHR], extensions: &["SPV_KHR_subgroup_vote"], version: None, last: None } },
    Spec { op: Op(4430), name: "OpSubgroupAllEqualKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_VOTE_KHR], extensions: &["SPV_KHR_subgroup_vote"], version: None, last: None } },
    Spec { op: Op(4431), name: "OpGroupNonUniformRotateKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_ROTATE_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4432), name: "OpSubgroupReadInvocationKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SUBGROUP_BALLOT_KHR], extensions: &["SPV_KHR_shader_ballot"], version: None, last: None } },
    Spec { op: Op(4433), name: "OpExtInstWithForwardRefsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralExtInstInteger, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[], extensions: &["SPV_KHR_relaxed_extended_instruction"], version: None, last: None } },
    Spec { op: Op(4434), name: "OpUntypedGroupAsyncCopyKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)], gate: Gate { capabilities: &[Capability::UNTYPED_POINTERS_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4445), name: "OpTraceRayKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Spec { op: Op(4446), name: "OpExecuteCallableKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Spec { op: Op(4447), name: "OpConvertUToAccelerationStructureKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR, Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_tracing", "SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4448), name: "OpIgnoreIntersectionKHR", operands: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Spec { op: Op(4449), name: "OpTerminateRayKHR", operands: &[], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Spec { op: Op(4450), name: "OpSDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)], gate: Gate { capabilities: &[Capability::DOT_PRODUCT], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Spec { op: Op(4451), name: "OpUDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)], gate: Gate { capabilities: &[Capability::DOT_PRODUCT], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Spec { op: Op(4452), name: "OpSUDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)], gate: Gate { capabilities: &[Capability::DOT_PRODUCT], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Spec { op: Op(4453), name: "OpSDotAccSat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)], gate: Gate { capabilities: &[Capability::DOT_PRODUCT], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Spec { op: Op(4454), name: "OpUDotAccSat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)], gate: Gate { capabilities: &[Capability::DOT_PRODUCT], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Spec { op: Op(4455), name: "OpSUDotAccSat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)], gate: Gate { capabilities: &[Capability::DOT_PRODUCT], extensions: &["SPV_KHR_integer_dot_product"], version: V1_6, last: None } },
    Spec { op: Op(4456), name: "OpTypeCooperativeMatrixKHR", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4457), name: "OpCooperativeMatrixLoadKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4458), name: "OpCooperativeMatrixStoreKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4459), name: "OpCooperativeMatrixMulAddKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::CooperativeMatrixOperands, Quantity::Optional)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4460), name: "OpCooperativeMatrixLengthKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(4461), name: "OpConstantCompositeReplicateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::REPLICATED_COMPOSITES_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4462), name: "OpSpecConstantCompositeReplicateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::REPLICATED_COMPOSITES_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4463), name: "OpCompositeConstructReplicateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::REPLICATED_COMPOSITES_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(4472), name: "OpTypeRayQueryKHR", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4473), name: "OpRayQueryInitializeKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4474), name: "OpRayQueryTerminateKHR", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4475), name: "OpRayQueryGenerateIntersectionKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4476), name: "OpRayQueryConfirmIntersectionKHR", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4477), name: "OpRayQueryProceedKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(4479), name: "OpRayQueryGetIntersectionTypeKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(5056), name: "OpReadClockKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_CLOCK_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5110), name: "OpGroupNonUniformQuadAllKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::QUAD_CONTROL_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5111), name: "OpGroupNonUniformQuadAnyKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::QUAD_CONTROL_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5115), name: "OpTypeBufferEXT", operands: &[(Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One)], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5119), name: "OpBufferPointerEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5121), name: "OpAbortKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ABORT_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5126), name: "OpUntypedImageTexelPointerEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5127), name: "OpMemberDecorateIdEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::Decoration, Quantity::One)], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5129), name: "OpConstantSizeOfEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::DESCRIPTOR_HEAP_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5147), name: "OpConstantDataKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::Many)], gate: Gate { capabilities: &[Capability::CONSTANT_DATA_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5148), name: "OpSpecConstantDataKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::Many)], gate: Gate { capabilities: &[Capability::CONSTANT_DATA_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5288), name: "OpTypeVectorIdEXT", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::LONG_VECTOR_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5293), name: "OpCooperativeMatrixConvertUseEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_CONVERSIONS_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5294), name: "OpEmitMeshTasksEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5295), name: "OpSetMeshOutputsEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::MESH_SHADING_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5296), name: "OpGroupNonUniformPartitionEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_NON_UNIFORM_PARTITIONED_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5304), name: "OpHitObjectRecordFromQueryEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5305), name: "OpHitObjectRecordMissEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5306), name: "OpHitObjectRecordMissMotionEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5307), name: "OpHitObjectGetIntersectionTriangleVertexPositionsEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5308), name: "OpHitObjectGetRayFlagsEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5309), name: "OpHitObjectSetShaderBindingTableRecordIndexEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5310), name: "OpHitObjectReorderExecuteShaderEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5311), name: "OpHitObjectTraceReorderExecuteEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5312), name: "OpHitObjectTraceMotionReorderExecuteEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5313), name: "OpTypeHitObjectEXT", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5314), name: "OpReorderThreadWithHintEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5315), name: "OpReorderThreadWithHitObjectEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5316), name: "OpHitObjectTraceRayEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5317), name: "OpHitObjectTraceRayMotionEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5318), name: "OpHitObjectRecordEmptyEXT", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5319), name: "OpHitObjectExecuteShaderEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5320), name: "OpHitObjectGetCurrentTimeEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5321), name: "OpHitObjectGetAttributesEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5322), name: "OpHitObjectGetHitKindEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5323), name: "OpHitObjectGetPrimitiveIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5324), name: "OpHitObjectGetGeometryIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5325), name: "OpHitObjectGetInstanceIdEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5326), name: "OpHitObjectGetInstanceCustomIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5327), name: "OpHitObjectGetObjectRayOriginEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5328), name: "OpHitObjectGetObjectRayDirectionEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5329), name: "OpHitObjectGetWorldRayDirectionEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5330), name: "OpHitObjectGetWorldRayOriginEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5331), name: "OpHitObjectGetObjectToWorldEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5332), name: "OpHitObjectGetWorldToObjectEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5333), name: "OpHitObjectGetRayTMaxEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5334), name: "OpReportIntersectionKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR], extensions: &["SPV_KHR_ray_tracing"], version: None, last: None } },
    Spec { op: Op(5340), name: "OpRayQueryGetIntersectionTriangleVertexPositionsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_POSITION_FETCH_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(5341), name: "OpTypeAccelerationStructureKHR", operands: &[(Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_TRACING_KHR, Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_tracing", "SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(5347), name: "OpHitObjectGetRayTMinEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5348), name: "OpHitObjectGetShaderBindingTableRecordIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5349), name: "OpHitObjectGetShaderRecordBufferHandleEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5350), name: "OpHitObjectIsEmptyEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5351), name: "OpHitObjectIsHitEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5352), name: "OpHitObjectIsMissEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::SHADER_INVOCATION_REORDER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5363), name: "OpCooperativeMatrixGetCoordinateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_GET_COORDINATE_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5364), name: "OpBeginInvocationInterlockEXT", operands: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_SAMPLE_INTERLOCK_EXT, Capability::FRAGMENT_SHADER_PIXEL_INTERLOCK_EXT, Capability::FRAGMENT_SHADER_SHADING_RATE_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Spec { op: Op(5365), name: "OpEndInvocationInterlockEXT", operands: &[], gate: Gate { capabilities: &[Capability::FRAGMENT_SHADER_SAMPLE_INTERLOCK_EXT, Capability::FRAGMENT_SHADER_PIXEL_INTERLOCK_EXT, Capability::FRAGMENT_SHADER_SHADING_RATE_INTERLOCK_EXT], extensions: &["SPV_EXT_fragment_shader_interlock"], version: None, last: None } },
    Spec { op: Op(5366), name: "OpCooperativeMatrixReduceEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::CooperativeMatrixReduce, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_REDUCTIONS_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5369), name: "OpCooperativeMatrixPerElementOpEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)], gate: Gate { capabilities: &[Capability::COOPERATIVE_MATRIX_PER_ELEMENT_OPERATIONS_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5380), name: "OpDemoteToHelperInvocation", operands: &[], gate: Gate { capabilities: &[Capability::DEMOTE_TO_HELPER_INVOCATION], extensions: &[], version: V1_6, last: None } },
    Spec { op: Op(5381), name: "OpIsHelperInvocationEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)], gate: Gate { capabilities: &[Capability::DEMOTE_TO_HELPER_INVOCATION], extensions: &["SPV_EXT_demote_to_helper_invocation"], version: None, last: None } },
    Spec { op: Op(5614), name: "OpAtomicFMinEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ATOMIC_FLOAT16_MIN_MAX_EXT, Capability::ATOMIC_FLOAT32_MIN_MAX_EXT, Capability::ATOMIC_FLOAT64_MIN_MAX_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5615), name: "OpAtomicFMaxEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ATOMIC_FLOAT16_MIN_MAX_EXT, Capability::ATOMIC_FLOAT32_MIN_MAX_EXT, Capability::ATOMIC_FLOAT64_MIN_MAX_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(5630), name: "OpAssumeTrueKHR", operands: &[(Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::EXPECT_ASSUME_KHR], extensions: &["SPV_KHR_expect_assume"], version: None, last: None } },
    Spec { op: Op(5631), name: "OpExpectKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::EXPECT_ASSUME_KHR], extensions: &["SPV_KHR_expect_assume"], version: None, last: None } },
    Spec { op: Op(5632), name: "OpDecorateString", operands: &[(Kind::IdRef, Quantity::One), (Kind::Decoration, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Spec { op: Op(5633), name: "OpMemberDecorateString", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::Decoration, Quantity::One)], gate: Gate { capabilities: &[], extensions: &[], version: V1_4, last: None } },
    Spec { op: Op(6016), name: "OpRayQueryGetRayTMinKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6017), name: "OpRayQueryGetRayFlagsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6018), name: "OpRayQueryGetIntersectionTKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6019), name: "OpRayQueryGetIntersectionInstanceCustomIndexKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6020), name: "OpRayQueryGetIntersectionInstanceIdKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6021), name: "OpRayQueryGetIntersectionInstanceShaderBindingTableRecordOffsetKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6022), name: "OpRayQueryGetIntersectionGeometryIndexKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6023), name: "OpRayQueryGetIntersectionPrimitiveIndexKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6024), name: "OpRayQueryGetIntersectionBarycentricsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6025), name: "OpRayQueryGetIntersectionFrontFaceKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6026), name: "OpRayQueryGetIntersectionCandidateAABBOpaqueKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6027), name: "OpRayQueryGetIntersectionObjectRayDirectionKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6028), name: "OpRayQueryGetIntersectionObjectRayOriginKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6029), name: "OpRayQueryGetWorldRayDirectionKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6030), name: "OpRayQueryGetWorldRayOriginKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6031), name: "OpRayQueryGetIntersectionObjectToWorldKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6032), name: "OpRayQueryGetIntersectionWorldToObjectKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::RAY_QUERY_KHR], extensions: &["SPV_KHR_ray_query"], version: None, last: None } },
    Spec { op: Op(6035), name: "OpAtomicFAddEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ATOMIC_FLOAT16_ADD_EXT, Capability::ATOMIC_FLOAT32_ADD_EXT, Capability::ATOMIC_FLOAT64_ADD_EXT], extensions: &["SPV_EXT_shader_atomic_float_add"], version: None, last: None } },
    Spec { op: Op(6142), name: "OpControlBarrierArriveEXT", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: Gate { capabilities: &[Capability::SPLIT_BARRIER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(6143), name: "OpControlBarrierWaitEXT", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)], gate: Gate { capabilities: &[Capability::SPLIT_BARRIER_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(6145), name: "OpArithmeticFenceEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::ARITHMETIC_FENCE_EXT], extensions: &[], version: None, last: None } },
    Spec { op: Op(6401), name: "OpGroupIMulKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6402), name: "OpGroupFMulKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6403), name: "OpGroupBitwiseAndKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6404), name: "OpGroupBitwiseOrKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6405), name: "OpGroupBitwiseXorKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6406), name: "OpGroupLogicalAndKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6407), name: "OpGroupLogicalOrKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
    Spec { op: Op(6408), name: "OpGroupLogicalXorKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)], gate: Gate { capabilities: &[Capability::GROUP_UNIFORM_ARITHMETIC_KHR], extensions: &[], version: None, last: None } },
];

/// The `GLSL.std.450` extended instructions, by ascending number.
pub(crate) static GLSL_STD_450: [ExtInst; 81] = [
    ExtInst { number: 1, operands: 1, capabilities: &[] },
    ExtInst { number: 2, operands: 1, capabilities: &[] },
    ExtInst { number: 3, operands: 1, capabilities: &[] },
    ExtInst { number: 4, operands: 1, capabilities: &[] },
    ExtInst { number: 5, operands: 1, capabilities: &[] },
    ExtInst { number: 6, operands: 1, capabilities: &[] },
    ExtInst { number: 7, operands: 1, capabilities: &[] },
    ExtInst { number: 8, operands: 1, capabilities: &[] },
    ExtInst { number: 9, operands: 1, capabilities: &[] },
    ExtInst { number: 10, operands: 1, capabilities: &[] },
    ExtInst { number: 11, operands: 1, capabilities: &[] },
    ExtInst { number: 12, operands: 1, capabilities: &[] },
    ExtInst { number: 13, operands: 1, capabilities: &[] },
    ExtInst { number: 14, operands: 1, capabilities: &[] },
    ExtInst { number: 15, operands: 1, capabilities: &[] },
    ExtInst { number: 16, operands: 1, capabilities: &[] },
    ExtInst { number: 17, operands: 1, capabilities: &[] },
    ExtInst { number: 18, operands: 1, capabilities: &[] },
    ExtInst { number: 19, operands: 1, capabilities: &[] },
    ExtInst { number: 20, operands: 1, capabilities: &[] },
    ExtInst { number: 21, operands: 1, capabilities: &[] },
    ExtInst { number: 22, operands: 1, capabilities: &[] },
    ExtInst { number: 23, operands: 1, capabilities: &[] },
    ExtInst { number: 24, operands: 1, capabilities: &[] },
    ExtInst { number: 25, operands: 2, capabilities: &[] },
    ExtInst { number: 26, operands: 2, capabilities: &[] },
    ExtInst { number: 27, operands: 1, capabilities: &[] },
    ExtInst { number: 28, operands: 1, capabilities: &[] },
    ExtInst { number: 29, operands: 1, capabilities: &[] },
    ExtInst { number: 30, operands: 1, capabilities: &[] },
    ExtInst { number: 31, operands: 1, capabilities: &[] },
    ExtInst { number: 32, operands: 1, capabilities: &[] },
    ExtInst { number: 33, operands: 1, capabilities: &[] },
    ExtInst { number: 34, operands: 1, capabilities: &[] },
    ExtInst { number: 35, operands: 2, capabilities: &[] },
    ExtInst { number: 36, operands: 1, capabilities: &[] },
    ExtInst { number: 37, operands: 2, capabilities: &[] },
    ExtInst { number: 38, operands: 2, capabilities: &[] },
    ExtInst { number: 39, operands: 2, capabilities: &[] },
    ExtInst { number: 40, operands: 2, capabilities: &[] },
    ExtInst { number: 41, operands: 2, capabilities: &[] },
    ExtInst { number: 42, operands: 2, capabilities: &[] },
    ExtInst { number: 43, operands: 3, capabilities: &[] },
    ExtInst { number: 44, operands: 3, capabilities: &[] },
    ExtInst { number: 45, operands: 3, capabilities: &[] },
    ExtInst { number: 46, operands: 3, capabilities: &[] },
    ExtInst { number: 47, operands: 3, capabilities: &[] },
    ExtInst { number: 48, operands: 2, capabilities: &[] },
    ExtInst { number: 49, operands: 3, capabilities: &[] },
    ExtInst { number: 50, operands: 3, capabilities: &[] },
    ExtInst { number: 51, operands: 2, capabilities: &[] },
    ExtInst { number: 52, operands: 1, capabilities: &[] },
    ExtInst { number: 53, operands: 2, capabilities: &[] },
    ExtInst { number: 54, operands: 1, capabilities: &[] },
    ExtInst { number: 55, operands: 1, capabilities: &[] },
    ExtInst { number: 56, operands: 1, capabilities: &[] },
    ExtInst { number: 57, operands: 1, capabilities: &[] },
    ExtInst { number: 58, operands: 1, capabilities: &[] },
    ExtInst { number: 59, operands: 1, capabilities: &[Capability::FLOAT64] },
    ExtInst { number: 60, operands: 1, capabilities: &[] },
    ExtInst { number: 61, operands: 1, capabilities: &[] },
    ExtInst { number: 62, operands: 1, capabilities: &[] },
    ExtInst { number: 63, operands: 1, capabilities: &[] },
    ExtInst { number: 64, operands: 1, capabilities: &[] },
    ExtInst { number: 65, operands: 1, capabilities: &[Capability::FLOAT64] },
    ExtInst { number: 66, operands: 1, capabilities: &[] },
    ExtInst { number: 67, operands: 2, capabilities: &[] },
    ExtInst { number: 68, operands: 2, capabilities: &[] },
    ExtInst { number: 69, operands: 1, capabilities: &[] },
    ExtInst { number: 70, operands: 3, capabilities: &[] },
    ExtInst { number: 71, operands: 2, capabilities: &[] },
    ExtInst { number: 72, operands: 3, capabilities: &[] },
    ExtInst { number: 73, operands: 1, capabilities: &[] },
    ExtInst { number: 74, operands: 1, capabilities: &[] },
    ExtInst { number: 75, operands: 1, capabilities: &[] },
    ExtInst { number: 76, operands: 1, capabilities: &[Capability::INTERPOLATION_FUNCTION] },
    ExtInst { number: 77, operands: 2, capabilities: &[Capability::INTERPOLATION_FUNCTION] },
    ExtInst { number: 78, operands: 2, capabilities: &[Capability::INTERPOLATION_FUNCTION] },
    ExtInst { number: 79, operands: 2, capabilities: &[] },
    ExtInst { number: 80, operands: 2, capabilities: &[] },
    ExtInst { number: 81, operands: 3, capabilities: &[] },
];
