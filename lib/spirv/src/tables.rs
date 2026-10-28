// GENERATED FILE — DO NOT EDIT.
//
// Emitted by `cargo xtask spirv --write` from the Khronos SPIR-V grammar
// in `lib/spirv/assets/` (SPIRV-Headers vulkan-sdk-1.4.363.0; MIT, see `LICENSE`
// there). `cargo xtask spirv` (run by `ci`) fails closed if this file
// drifts from a fresh generation.

use crate::grammar::{Enumerant, ExtInst, Quantity, Shape, Spec};
use crate::Op;

/// The grammar's SPIR-V version (revision 7), the newest a module may
/// declare.
pub(crate) const VERSION: (u8, u8) = (1, 6);

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
            Self::CooperativeMatrixOperands | Self::CooperativeMatrixReduce | Self::FPFastMathMode | Self::FunctionControl | Self::ImageOperands | Self::LoopControl | Self::MemoryAccess | Self::SelectionControl => Shape::Bits,
            Self::AccessQualifier | Self::AddressingModel | Self::BuiltIn | Self::Capability | Self::Decoration | Self::Dim | Self::ExecutionMode | Self::ExecutionModel | Self::FPEncoding | Self::FPRoundingMode | Self::FunctionParameterAttribute | Self::GroupOperation | Self::ImageFormat | Self::LinkageType | Self::MemoryModel | Self::PackedVectorFormat | Self::SamplerAddressingMode | Self::SamplerFilterMode | Self::SourceLanguage | Self::StorageClass => Shape::Enum,
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
            Self::PackedVectorFormat => &PACKED_VECTOR_FORMAT,
            Self::SamplerAddressingMode => &SAMPLER_ADDRESSING_MODE,
            Self::SamplerFilterMode => &SAMPLER_FILTER_MODE,
            Self::SelectionControl => &SELECTION_CONTROL,
            Self::SourceLanguage => &SOURCE_LANGUAGE,
            Self::StorageClass => &STORAGE_CLASS,
            _ => &[],
        }
    }
}

static ACCESS_QUALIFIER: [Enumerant; 3] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
];

static ADDRESSING_MODEL: [Enumerant; 4] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x14e4, parameters: &[] },
];

static BUILT_IN: [Enumerant; 81] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[] },
    Enumerant { value: 0xc, parameters: &[] },
    Enumerant { value: 0xd, parameters: &[] },
    Enumerant { value: 0xe, parameters: &[] },
    Enumerant { value: 0xf, parameters: &[] },
    Enumerant { value: 0x10, parameters: &[] },
    Enumerant { value: 0x11, parameters: &[] },
    Enumerant { value: 0x12, parameters: &[] },
    Enumerant { value: 0x13, parameters: &[] },
    Enumerant { value: 0x14, parameters: &[] },
    Enumerant { value: 0x16, parameters: &[] },
    Enumerant { value: 0x17, parameters: &[] },
    Enumerant { value: 0x18, parameters: &[] },
    Enumerant { value: 0x19, parameters: &[] },
    Enumerant { value: 0x1a, parameters: &[] },
    Enumerant { value: 0x1b, parameters: &[] },
    Enumerant { value: 0x1c, parameters: &[] },
    Enumerant { value: 0x1d, parameters: &[] },
    Enumerant { value: 0x1e, parameters: &[] },
    Enumerant { value: 0x1f, parameters: &[] },
    Enumerant { value: 0x20, parameters: &[] },
    Enumerant { value: 0x21, parameters: &[] },
    Enumerant { value: 0x22, parameters: &[] },
    Enumerant { value: 0x24, parameters: &[] },
    Enumerant { value: 0x25, parameters: &[] },
    Enumerant { value: 0x26, parameters: &[] },
    Enumerant { value: 0x27, parameters: &[] },
    Enumerant { value: 0x28, parameters: &[] },
    Enumerant { value: 0x29, parameters: &[] },
    Enumerant { value: 0x2a, parameters: &[] },
    Enumerant { value: 0x2b, parameters: &[] },
    Enumerant { value: 0x1140, parameters: &[] },
    Enumerant { value: 0x1141, parameters: &[] },
    Enumerant { value: 0x1142, parameters: &[] },
    Enumerant { value: 0x1143, parameters: &[] },
    Enumerant { value: 0x1144, parameters: &[] },
    Enumerant { value: 0x1148, parameters: &[] },
    Enumerant { value: 0x1149, parameters: &[] },
    Enumerant { value: 0x114a, parameters: &[] },
    Enumerant { value: 0x1150, parameters: &[] },
    Enumerant { value: 0x1156, parameters: &[] },
    Enumerant { value: 0x1158, parameters: &[] },
    Enumerant { value: 0x115c, parameters: &[] },
    Enumerant { value: 0x1396, parameters: &[] },
    Enumerant { value: 0x1402, parameters: &[] },
    Enumerant { value: 0x1403, parameters: &[] },
    Enumerant { value: 0x1490, parameters: &[] },
    Enumerant { value: 0x14a6, parameters: &[] },
    Enumerant { value: 0x14a7, parameters: &[] },
    Enumerant { value: 0x14ac, parameters: &[] },
    Enumerant { value: 0x14ad, parameters: &[] },
    Enumerant { value: 0x14ae, parameters: &[] },
    Enumerant { value: 0x14af, parameters: &[] },
    Enumerant { value: 0x14b0, parameters: &[] },
    Enumerant { value: 0x14b3, parameters: &[] },
    Enumerant { value: 0x14c7, parameters: &[] },
    Enumerant { value: 0x14c8, parameters: &[] },
    Enumerant { value: 0x14c9, parameters: &[] },
    Enumerant { value: 0x14ca, parameters: &[] },
    Enumerant { value: 0x14cb, parameters: &[] },
    Enumerant { value: 0x14cc, parameters: &[] },
    Enumerant { value: 0x14cd, parameters: &[] },
    Enumerant { value: 0x14ce, parameters: &[] },
    Enumerant { value: 0x14cf, parameters: &[] },
    Enumerant { value: 0x14d2, parameters: &[] },
    Enumerant { value: 0x14d3, parameters: &[] },
    Enumerant { value: 0x14d5, parameters: &[] },
    Enumerant { value: 0x14d7, parameters: &[] },
    Enumerant { value: 0x14e7, parameters: &[] },
    Enumerant { value: 0x14e8, parameters: &[] },
    Enumerant { value: 0x1785, parameters: &[] },
];

static CAPABILITY: [Enumerant; 180] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[] },
    Enumerant { value: 0xc, parameters: &[] },
    Enumerant { value: 0xd, parameters: &[] },
    Enumerant { value: 0xe, parameters: &[] },
    Enumerant { value: 0xf, parameters: &[] },
    Enumerant { value: 0x11, parameters: &[] },
    Enumerant { value: 0x13, parameters: &[] },
    Enumerant { value: 0x14, parameters: &[] },
    Enumerant { value: 0x15, parameters: &[] },
    Enumerant { value: 0x16, parameters: &[] },
    Enumerant { value: 0x17, parameters: &[] },
    Enumerant { value: 0x18, parameters: &[] },
    Enumerant { value: 0x19, parameters: &[] },
    Enumerant { value: 0x1b, parameters: &[] },
    Enumerant { value: 0x1c, parameters: &[] },
    Enumerant { value: 0x1d, parameters: &[] },
    Enumerant { value: 0x1e, parameters: &[] },
    Enumerant { value: 0x1f, parameters: &[] },
    Enumerant { value: 0x20, parameters: &[] },
    Enumerant { value: 0x21, parameters: &[] },
    Enumerant { value: 0x22, parameters: &[] },
    Enumerant { value: 0x23, parameters: &[] },
    Enumerant { value: 0x24, parameters: &[] },
    Enumerant { value: 0x25, parameters: &[] },
    Enumerant { value: 0x26, parameters: &[] },
    Enumerant { value: 0x27, parameters: &[] },
    Enumerant { value: 0x28, parameters: &[] },
    Enumerant { value: 0x29, parameters: &[] },
    Enumerant { value: 0x2a, parameters: &[] },
    Enumerant { value: 0x2b, parameters: &[] },
    Enumerant { value: 0x2c, parameters: &[] },
    Enumerant { value: 0x2d, parameters: &[] },
    Enumerant { value: 0x2e, parameters: &[] },
    Enumerant { value: 0x2f, parameters: &[] },
    Enumerant { value: 0x30, parameters: &[] },
    Enumerant { value: 0x31, parameters: &[] },
    Enumerant { value: 0x32, parameters: &[] },
    Enumerant { value: 0x33, parameters: &[] },
    Enumerant { value: 0x34, parameters: &[] },
    Enumerant { value: 0x35, parameters: &[] },
    Enumerant { value: 0x36, parameters: &[] },
    Enumerant { value: 0x37, parameters: &[] },
    Enumerant { value: 0x38, parameters: &[] },
    Enumerant { value: 0x39, parameters: &[] },
    Enumerant { value: 0x3a, parameters: &[] },
    Enumerant { value: 0x3b, parameters: &[] },
    Enumerant { value: 0x3c, parameters: &[] },
    Enumerant { value: 0x3d, parameters: &[] },
    Enumerant { value: 0x3e, parameters: &[] },
    Enumerant { value: 0x3f, parameters: &[] },
    Enumerant { value: 0x40, parameters: &[] },
    Enumerant { value: 0x41, parameters: &[] },
    Enumerant { value: 0x42, parameters: &[] },
    Enumerant { value: 0x43, parameters: &[] },
    Enumerant { value: 0x44, parameters: &[] },
    Enumerant { value: 0x45, parameters: &[] },
    Enumerant { value: 0x46, parameters: &[] },
    Enumerant { value: 0x47, parameters: &[] },
    Enumerant { value: 0x1046, parameters: &[] },
    Enumerant { value: 0x1047, parameters: &[] },
    Enumerant { value: 0x1048, parameters: &[] },
    Enumerant { value: 0x1074, parameters: &[] },
    Enumerant { value: 0x1075, parameters: &[] },
    Enumerant { value: 0x1084, parameters: &[] },
    Enumerant { value: 0x1085, parameters: &[] },
    Enumerant { value: 0x1086, parameters: &[] },
    Enumerant { value: 0x1087, parameters: &[] },
    Enumerant { value: 0x1088, parameters: &[] },
    Enumerant { value: 0x1146, parameters: &[] },
    Enumerant { value: 0x1147, parameters: &[] },
    Enumerant { value: 0x114b, parameters: &[] },
    Enumerant { value: 0x114c, parameters: &[] },
    Enumerant { value: 0x114d, parameters: &[] },
    Enumerant { value: 0x114e, parameters: &[] },
    Enumerant { value: 0x114f, parameters: &[] },
    Enumerant { value: 0x1151, parameters: &[] },
    Enumerant { value: 0x1152, parameters: &[] },
    Enumerant { value: 0x1153, parameters: &[] },
    Enumerant { value: 0x1154, parameters: &[] },
    Enumerant { value: 0x1155, parameters: &[] },
    Enumerant { value: 0x1157, parameters: &[] },
    Enumerant { value: 0x1159, parameters: &[] },
    Enumerant { value: 0x115a, parameters: &[] },
    Enumerant { value: 0x115d, parameters: &[] },
    Enumerant { value: 0x115f, parameters: &[] },
    Enumerant { value: 0x1160, parameters: &[] },
    Enumerant { value: 0x1161, parameters: &[] },
    Enumerant { value: 0x1162, parameters: &[] },
    Enumerant { value: 0x1170, parameters: &[] },
    Enumerant { value: 0x1171, parameters: &[] },
    Enumerant { value: 0x1172, parameters: &[] },
    Enumerant { value: 0x1173, parameters: &[] },
    Enumerant { value: 0x1174, parameters: &[] },
    Enumerant { value: 0x1177, parameters: &[] },
    Enumerant { value: 0x1178, parameters: &[] },
    Enumerant { value: 0x1179, parameters: &[] },
    Enumerant { value: 0x117e, parameters: &[] },
    Enumerant { value: 0x117f, parameters: &[] },
    Enumerant { value: 0x1395, parameters: &[] },
    Enumerant { value: 0x1398, parameters: &[] },
    Enumerant { value: 0x13bf, parameters: &[] },
    Enumerant { value: 0x13df, parameters: &[] },
    Enumerant { value: 0x13fc, parameters: &[] },
    Enumerant { value: 0x13fd, parameters: &[] },
    Enumerant { value: 0x13fe, parameters: &[] },
    Enumerant { value: 0x1400, parameters: &[] },
    Enumerant { value: 0x1408, parameters: &[] },
    Enumerant { value: 0x141a, parameters: &[] },
    Enumerant { value: 0x1486, parameters: &[] },
    Enumerant { value: 0x1491, parameters: &[] },
    Enumerant { value: 0x14a3, parameters: &[] },
    Enumerant { value: 0x14a4, parameters: &[] },
    Enumerant { value: 0x14a8, parameters: &[] },
    Enumerant { value: 0x14ab, parameters: &[] },
    Enumerant { value: 0x14b1, parameters: &[] },
    Enumerant { value: 0x14b5, parameters: &[] },
    Enumerant { value: 0x14b6, parameters: &[] },
    Enumerant { value: 0x14b7, parameters: &[] },
    Enumerant { value: 0x14b8, parameters: &[] },
    Enumerant { value: 0x14b9, parameters: &[] },
    Enumerant { value: 0x14ba, parameters: &[] },
    Enumerant { value: 0x14bb, parameters: &[] },
    Enumerant { value: 0x14bc, parameters: &[] },
    Enumerant { value: 0x14bd, parameters: &[] },
    Enumerant { value: 0x14be, parameters: &[] },
    Enumerant { value: 0x14bf, parameters: &[] },
    Enumerant { value: 0x14c0, parameters: &[] },
    Enumerant { value: 0x14d8, parameters: &[] },
    Enumerant { value: 0x14e1, parameters: &[] },
    Enumerant { value: 0x14e2, parameters: &[] },
    Enumerant { value: 0x14e3, parameters: &[] },
    Enumerant { value: 0x14e6, parameters: &[] },
    Enumerant { value: 0x14e9, parameters: &[] },
    Enumerant { value: 0x14f3, parameters: &[] },
    Enumerant { value: 0x14fc, parameters: &[] },
    Enumerant { value: 0x1502, parameters: &[] },
    Enumerant { value: 0x1503, parameters: &[] },
    Enumerant { value: 0x1505, parameters: &[] },
    Enumerant { value: 0x150c, parameters: &[] },
    Enumerant { value: 0x150f, parameters: &[] },
    Enumerant { value: 0x1531, parameters: &[] },
    Enumerant { value: 0x1532, parameters: &[] },
    Enumerant { value: 0x1535, parameters: &[] },
    Enumerant { value: 0x1536, parameters: &[] },
    Enumerant { value: 0x1538, parameters: &[] },
    Enumerant { value: 0x153e, parameters: &[] },
    Enumerant { value: 0x15ec, parameters: &[] },
    Enumerant { value: 0x15ed, parameters: &[] },
    Enumerant { value: 0x15f0, parameters: &[] },
    Enumerant { value: 0x15fd, parameters: &[] },
    Enumerant { value: 0x1780, parameters: &[] },
    Enumerant { value: 0x1781, parameters: &[] },
    Enumerant { value: 0x1782, parameters: &[] },
    Enumerant { value: 0x1783, parameters: &[] },
    Enumerant { value: 0x1784, parameters: &[] },
    Enumerant { value: 0x1786, parameters: &[] },
    Enumerant { value: 0x1788, parameters: &[] },
    Enumerant { value: 0x1789, parameters: &[] },
    Enumerant { value: 0x178a, parameters: &[] },
    Enumerant { value: 0x178d, parameters: &[] },
    Enumerant { value: 0x178e, parameters: &[] },
    Enumerant { value: 0x1790, parameters: &[] },
    Enumerant { value: 0x1791, parameters: &[] },
    Enumerant { value: 0x1792, parameters: &[] },
    Enumerant { value: 0x17ce, parameters: &[] },
    Enumerant { value: 0x17cf, parameters: &[] },
    Enumerant { value: 0x17fd, parameters: &[] },
    Enumerant { value: 0x1800, parameters: &[] },
    Enumerant { value: 0x1900, parameters: &[] },
];

static COOPERATIVE_MATRIX_OPERANDS: [Enumerant; 6] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x10, parameters: &[] },
];

static COOPERATIVE_MATRIX_REDUCE: [Enumerant; 3] = [
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
];

static DECORATION: [Enumerant; 60] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x7, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[Kind::BuiltIn] },
    Enumerant { value: 0xd, parameters: &[] },
    Enumerant { value: 0xe, parameters: &[] },
    Enumerant { value: 0xf, parameters: &[] },
    Enumerant { value: 0x10, parameters: &[] },
    Enumerant { value: 0x11, parameters: &[] },
    Enumerant { value: 0x12, parameters: &[] },
    Enumerant { value: 0x13, parameters: &[] },
    Enumerant { value: 0x14, parameters: &[] },
    Enumerant { value: 0x15, parameters: &[] },
    Enumerant { value: 0x16, parameters: &[] },
    Enumerant { value: 0x17, parameters: &[] },
    Enumerant { value: 0x18, parameters: &[] },
    Enumerant { value: 0x19, parameters: &[] },
    Enumerant { value: 0x1a, parameters: &[] },
    Enumerant { value: 0x1b, parameters: &[Kind::IdScope] },
    Enumerant { value: 0x1c, parameters: &[] },
    Enumerant { value: 0x1d, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x1e, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x1f, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x20, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x21, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x22, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x23, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x24, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x25, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x26, parameters: &[Kind::FunctionParameterAttribute] },
    Enumerant { value: 0x27, parameters: &[Kind::FPRoundingMode] },
    Enumerant { value: 0x28, parameters: &[Kind::FPFastMathMode] },
    Enumerant { value: 0x29, parameters: &[Kind::LiteralString, Kind::LinkageType] },
    Enumerant { value: 0x2a, parameters: &[] },
    Enumerant { value: 0x2b, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x2c, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x2d, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x2e, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x2f, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x1078, parameters: &[] },
    Enumerant { value: 0x1175, parameters: &[] },
    Enumerant { value: 0x1176, parameters: &[] },
    Enumerant { value: 0x1404, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x1405, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x1419, parameters: &[] },
    Enumerant { value: 0x1497, parameters: &[] },
    Enumerant { value: 0x14a5, parameters: &[] },
    Enumerant { value: 0x14b4, parameters: &[] },
    Enumerant { value: 0x14eb, parameters: &[] },
    Enumerant { value: 0x14ec, parameters: &[] },
    Enumerant { value: 0x150d, parameters: &[] },
    Enumerant { value: 0x1540, parameters: &[] },
];

static DIM: [Enumerant; 8] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x104d, parameters: &[] },
];

static EXECUTION_MODE: [Enumerant; 72] = [
    Enumerant { value: 0x0, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[] },
    Enumerant { value: 0xc, parameters: &[] },
    Enumerant { value: 0xe, parameters: &[] },
    Enumerant { value: 0xf, parameters: &[] },
    Enumerant { value: 0x10, parameters: &[] },
    Enumerant { value: 0x11, parameters: &[Kind::LiteralInteger, Kind::LiteralInteger, Kind::LiteralInteger] },
    Enumerant { value: 0x12, parameters: &[Kind::LiteralInteger, Kind::LiteralInteger, Kind::LiteralInteger] },
    Enumerant { value: 0x13, parameters: &[] },
    Enumerant { value: 0x14, parameters: &[] },
    Enumerant { value: 0x15, parameters: &[] },
    Enumerant { value: 0x16, parameters: &[] },
    Enumerant { value: 0x17, parameters: &[] },
    Enumerant { value: 0x18, parameters: &[] },
    Enumerant { value: 0x19, parameters: &[] },
    Enumerant { value: 0x1a, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x1b, parameters: &[] },
    Enumerant { value: 0x1c, parameters: &[] },
    Enumerant { value: 0x1d, parameters: &[] },
    Enumerant { value: 0x1e, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x1f, parameters: &[] },
    Enumerant { value: 0x21, parameters: &[] },
    Enumerant { value: 0x22, parameters: &[] },
    Enumerant { value: 0x23, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x24, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x25, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x26, parameters: &[Kind::IdRef, Kind::IdRef, Kind::IdRef] },
    Enumerant { value: 0x27, parameters: &[Kind::IdRef, Kind::IdRef, Kind::IdRef] },
    Enumerant { value: 0x1049, parameters: &[] },
    Enumerant { value: 0x104a, parameters: &[] },
    Enumerant { value: 0x104b, parameters: &[] },
    Enumerant { value: 0x1145, parameters: &[] },
    Enumerant { value: 0x115e, parameters: &[] },
    Enumerant { value: 0x116b, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x116c, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x116d, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x116e, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x116f, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x13a3, parameters: &[] },
    Enumerant { value: 0x13d7, parameters: &[] },
    Enumerant { value: 0x13d8, parameters: &[] },
    Enumerant { value: 0x13d9, parameters: &[] },
    Enumerant { value: 0x13da, parameters: &[] },
    Enumerant { value: 0x13db, parameters: &[] },
    Enumerant { value: 0x13dc, parameters: &[] },
    Enumerant { value: 0x13e0, parameters: &[] },
    Enumerant { value: 0x13e1, parameters: &[] },
    Enumerant { value: 0x1495, parameters: &[] },
    Enumerant { value: 0x1496, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x14a9, parameters: &[] },
    Enumerant { value: 0x14aa, parameters: &[] },
    Enumerant { value: 0x14b2, parameters: &[] },
    Enumerant { value: 0x14f6, parameters: &[] },
    Enumerant { value: 0x14f7, parameters: &[] },
    Enumerant { value: 0x14f8, parameters: &[] },
    Enumerant { value: 0x14f9, parameters: &[] },
    Enumerant { value: 0x14fa, parameters: &[] },
    Enumerant { value: 0x14fb, parameters: &[] },
    Enumerant { value: 0x1533, parameters: &[] },
    Enumerant { value: 0x1787, parameters: &[] },
    Enumerant { value: 0x178c, parameters: &[Kind::IdRef, Kind::IdRef] },
    Enumerant { value: 0x178f, parameters: &[Kind::IdRef] },
];

static EXECUTION_MODEL: [Enumerant; 15] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x14c1, parameters: &[] },
    Enumerant { value: 0x14c2, parameters: &[] },
    Enumerant { value: 0x14c3, parameters: &[] },
    Enumerant { value: 0x14c4, parameters: &[] },
    Enumerant { value: 0x14c5, parameters: &[] },
    Enumerant { value: 0x14c6, parameters: &[] },
    Enumerant { value: 0x14f4, parameters: &[] },
    Enumerant { value: 0x14f5, parameters: &[] },
];

static FP_ENCODING: [Enumerant; 8] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1076, parameters: &[] },
    Enumerant { value: 0x1077, parameters: &[] },
    Enumerant { value: 0x107f, parameters: &[] },
    Enumerant { value: 0x1080, parameters: &[] },
    Enumerant { value: 0x1081, parameters: &[] },
    Enumerant { value: 0x1082, parameters: &[] },
    Enumerant { value: 0x1083, parameters: &[] },
];

static FP_FAST_MATH_MODE: [Enumerant; 9] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x10, parameters: &[] },
    Enumerant { value: 0x1_0000, parameters: &[] },
    Enumerant { value: 0x2_0000, parameters: &[] },
    Enumerant { value: 0x4_0000, parameters: &[] },
];

static FP_ROUNDING_MODE: [Enumerant; 4] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
];

static FUNCTION_CONTROL: [Enumerant; 6] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x1_0000, parameters: &[] },
];

static FUNCTION_PARAMETER_ATTRIBUTE: [Enumerant; 8] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
];

static GROUP_OPERATION: [Enumerant; 7] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
];

static IMAGE_FORMAT: [Enumerant; 42] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[] },
    Enumerant { value: 0xc, parameters: &[] },
    Enumerant { value: 0xd, parameters: &[] },
    Enumerant { value: 0xe, parameters: &[] },
    Enumerant { value: 0xf, parameters: &[] },
    Enumerant { value: 0x10, parameters: &[] },
    Enumerant { value: 0x11, parameters: &[] },
    Enumerant { value: 0x12, parameters: &[] },
    Enumerant { value: 0x13, parameters: &[] },
    Enumerant { value: 0x14, parameters: &[] },
    Enumerant { value: 0x15, parameters: &[] },
    Enumerant { value: 0x16, parameters: &[] },
    Enumerant { value: 0x17, parameters: &[] },
    Enumerant { value: 0x18, parameters: &[] },
    Enumerant { value: 0x19, parameters: &[] },
    Enumerant { value: 0x1a, parameters: &[] },
    Enumerant { value: 0x1b, parameters: &[] },
    Enumerant { value: 0x1c, parameters: &[] },
    Enumerant { value: 0x1d, parameters: &[] },
    Enumerant { value: 0x1e, parameters: &[] },
    Enumerant { value: 0x1f, parameters: &[] },
    Enumerant { value: 0x20, parameters: &[] },
    Enumerant { value: 0x21, parameters: &[] },
    Enumerant { value: 0x22, parameters: &[] },
    Enumerant { value: 0x23, parameters: &[] },
    Enumerant { value: 0x24, parameters: &[] },
    Enumerant { value: 0x25, parameters: &[] },
    Enumerant { value: 0x26, parameters: &[] },
    Enumerant { value: 0x27, parameters: &[] },
    Enumerant { value: 0x28, parameters: &[] },
    Enumerant { value: 0x29, parameters: &[] },
];

static IMAGE_OPERANDS: [Enumerant; 17] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x2, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x4, parameters: &[Kind::IdRef, Kind::IdRef] },
    Enumerant { value: 0x8, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x10, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x20, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x40, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x80, parameters: &[Kind::IdRef] },
    Enumerant { value: 0x100, parameters: &[Kind::IdScope] },
    Enumerant { value: 0x200, parameters: &[Kind::IdScope] },
    Enumerant { value: 0x400, parameters: &[] },
    Enumerant { value: 0x800, parameters: &[] },
    Enumerant { value: 0x1000, parameters: &[] },
    Enumerant { value: 0x2000, parameters: &[] },
    Enumerant { value: 0x4000, parameters: &[] },
    Enumerant { value: 0x1_0000, parameters: &[Kind::IdRef] },
];

static LINKAGE_TYPE: [Enumerant; 3] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
];

static LOOP_CONTROL: [Enumerant; 11] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x10, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x20, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x40, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x80, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x100, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x1000_0000, parameters: &[Kind::LiteralInteger] },
];

static MEMORY_ACCESS: [Enumerant; 7] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[Kind::LiteralInteger] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[Kind::IdScope] },
    Enumerant { value: 0x10, parameters: &[Kind::IdScope] },
    Enumerant { value: 0x20, parameters: &[] },
];

static MEMORY_MODEL: [Enumerant; 4] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
];

static PACKED_VECTOR_FORMAT: [Enumerant; 1] = [
    Enumerant { value: 0x0, parameters: &[] },
];

static SAMPLER_ADDRESSING_MODE: [Enumerant; 5] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
];

static SAMPLER_FILTER_MODE: [Enumerant; 2] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
];

static SELECTION_CONTROL: [Enumerant; 3] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
];

static SOURCE_LANGUAGE: [Enumerant; 16] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[] },
    Enumerant { value: 0xc, parameters: &[] },
    Enumerant { value: 0xd, parameters: &[] },
    Enumerant { value: 0xe, parameters: &[] },
    Enumerant { value: 0xf, parameters: &[] },
];

static STORAGE_CLASS: [Enumerant; 23] = [
    Enumerant { value: 0x0, parameters: &[] },
    Enumerant { value: 0x1, parameters: &[] },
    Enumerant { value: 0x2, parameters: &[] },
    Enumerant { value: 0x3, parameters: &[] },
    Enumerant { value: 0x4, parameters: &[] },
    Enumerant { value: 0x5, parameters: &[] },
    Enumerant { value: 0x6, parameters: &[] },
    Enumerant { value: 0x7, parameters: &[] },
    Enumerant { value: 0x8, parameters: &[] },
    Enumerant { value: 0x9, parameters: &[] },
    Enumerant { value: 0xa, parameters: &[] },
    Enumerant { value: 0xb, parameters: &[] },
    Enumerant { value: 0xc, parameters: &[] },
    Enumerant { value: 0x104c, parameters: &[] },
    Enumerant { value: 0x14d0, parameters: &[] },
    Enumerant { value: 0x14d1, parameters: &[] },
    Enumerant { value: 0x14da, parameters: &[] },
    Enumerant { value: 0x14db, parameters: &[] },
    Enumerant { value: 0x14de, parameters: &[] },
    Enumerant { value: 0x14df, parameters: &[] },
    Enumerant { value: 0x14e5, parameters: &[] },
    Enumerant { value: 0x151a, parameters: &[] },
    Enumerant { value: 0x1523, parameters: &[] },
];

/// Every instruction recognised, by ascending opcode.
pub(crate) static INSTRUCTIONS: [Spec; 476] = [
    Spec { op: Op(0), name: "OpNop", operands: &[] },
    Spec { op: Op(1), name: "OpUndef", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(2), name: "OpSourceContinued", operands: &[(Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(3), name: "OpSource", operands: &[(Kind::SourceLanguage, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::LiteralString, Quantity::Optional)] },
    Spec { op: Op(4), name: "OpSourceExtension", operands: &[(Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(5), name: "OpName", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(6), name: "OpMemberName", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(7), name: "OpString", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(8), name: "OpLine", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(10), name: "OpExtension", operands: &[(Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(11), name: "OpExtInstImport", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(12), name: "OpExtInst", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralExtInstInteger, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(14), name: "OpMemoryModel", operands: &[(Kind::AddressingModel, Quantity::One), (Kind::MemoryModel, Quantity::One)] },
    Spec { op: Op(15), name: "OpEntryPoint", operands: &[(Kind::ExecutionModel, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralString, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(16), name: "OpExecutionMode", operands: &[(Kind::IdRef, Quantity::One), (Kind::ExecutionMode, Quantity::One)] },
    Spec { op: Op(17), name: "OpCapability", operands: &[(Kind::Capability, Quantity::One)] },
    Spec { op: Op(19), name: "OpTypeVoid", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(20), name: "OpTypeBool", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(21), name: "OpTypeInt", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(22), name: "OpTypeFloat", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::FPEncoding, Quantity::Optional)] },
    Spec { op: Op(23), name: "OpTypeVector", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(24), name: "OpTypeMatrix", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(25), name: "OpTypeImage", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::Dim, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::ImageFormat, Quantity::One), (Kind::AccessQualifier, Quantity::Optional)] },
    Spec { op: Op(26), name: "OpTypeSampler", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(27), name: "OpTypeSampledImage", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(28), name: "OpTypeArray", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(29), name: "OpTypeRuntimeArray", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(30), name: "OpTypeStruct", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(31), name: "OpTypeOpaque", operands: &[(Kind::IdResult, Quantity::One), (Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(32), name: "OpTypePointer", operands: &[(Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(33), name: "OpTypeFunction", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(34), name: "OpTypeEvent", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(35), name: "OpTypeDeviceEvent", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(36), name: "OpTypeReserveId", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(37), name: "OpTypeQueue", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(38), name: "OpTypePipe", operands: &[(Kind::IdResult, Quantity::One), (Kind::AccessQualifier, Quantity::One)] },
    Spec { op: Op(39), name: "OpTypeForwardPointer", operands: &[(Kind::IdRef, Quantity::One), (Kind::StorageClass, Quantity::One)] },
    Spec { op: Op(41), name: "OpConstantTrue", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(42), name: "OpConstantFalse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(43), name: "OpConstant", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralContextDependentNumber, Quantity::One)] },
    Spec { op: Op(44), name: "OpConstantComposite", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(45), name: "OpConstantSampler", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::SamplerAddressingMode, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::SamplerFilterMode, Quantity::One)] },
    Spec { op: Op(46), name: "OpConstantNull", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(48), name: "OpSpecConstantTrue", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(49), name: "OpSpecConstantFalse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(50), name: "OpSpecConstant", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralContextDependentNumber, Quantity::One)] },
    Spec { op: Op(51), name: "OpSpecConstantComposite", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(52), name: "OpSpecConstantOp", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralSpecConstantOpInteger, Quantity::One)] },
    Spec { op: Op(54), name: "OpFunction", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::FunctionControl, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(55), name: "OpFunctionParameter", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(56), name: "OpFunctionEnd", operands: &[] },
    Spec { op: Op(57), name: "OpFunctionCall", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(59), name: "OpVariable", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(60), name: "OpImageTexelPointer", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(61), name: "OpLoad", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(62), name: "OpStore", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(63), name: "OpCopyMemory", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(64), name: "OpCopyMemorySized", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(65), name: "OpAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(66), name: "OpInBoundsAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(67), name: "OpPtrAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(68), name: "OpArrayLength", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(69), name: "OpGenericPtrMemSemantics", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(70), name: "OpInBoundsPtrAccessChain", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(71), name: "OpDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::Decoration, Quantity::One)] },
    Spec { op: Op(72), name: "OpMemberDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::Decoration, Quantity::One)] },
    Spec { op: Op(73), name: "OpDecorationGroup", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(74), name: "OpGroupDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(75), name: "OpGroupMemberDecorate", operands: &[(Kind::IdRef, Quantity::One), (Kind::PairIdRefLiteralInteger, Quantity::Many)] },
    Spec { op: Op(77), name: "OpVectorExtractDynamic", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(78), name: "OpVectorInsertDynamic", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(79), name: "OpVectorShuffle", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)] },
    Spec { op: Op(80), name: "OpCompositeConstruct", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(81), name: "OpCompositeExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)] },
    Spec { op: Op(82), name: "OpCompositeInsert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)] },
    Spec { op: Op(83), name: "OpCopyObject", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(84), name: "OpTranspose", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(86), name: "OpSampledImage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(87), name: "OpImageSampleImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(88), name: "OpImageSampleExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(89), name: "OpImageSampleDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(90), name: "OpImageSampleDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(91), name: "OpImageSampleProjImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(92), name: "OpImageSampleProjExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(93), name: "OpImageSampleProjDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(94), name: "OpImageSampleProjDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(95), name: "OpImageFetch", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(96), name: "OpImageGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(97), name: "OpImageDrefGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(98), name: "OpImageRead", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(99), name: "OpImageWrite", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(100), name: "OpImage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(101), name: "OpImageQueryFormat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(102), name: "OpImageQueryOrder", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(103), name: "OpImageQuerySizeLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(104), name: "OpImageQuerySize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(105), name: "OpImageQueryLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(106), name: "OpImageQueryLevels", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(107), name: "OpImageQuerySamples", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(109), name: "OpConvertFToU", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(110), name: "OpConvertFToS", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(111), name: "OpConvertSToF", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(112), name: "OpConvertUToF", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(113), name: "OpUConvert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(114), name: "OpSConvert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(115), name: "OpFConvert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(116), name: "OpQuantizeToF16", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(117), name: "OpConvertPtrToU", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(118), name: "OpSatConvertSToU", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(119), name: "OpSatConvertUToS", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(120), name: "OpConvertUToPtr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(121), name: "OpPtrCastToGeneric", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(122), name: "OpGenericCastToPtr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(123), name: "OpGenericCastToPtrExplicit", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::StorageClass, Quantity::One)] },
    Spec { op: Op(124), name: "OpBitcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(126), name: "OpSNegate", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(127), name: "OpFNegate", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(128), name: "OpIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(129), name: "OpFAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(130), name: "OpISub", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(131), name: "OpFSub", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(132), name: "OpIMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(133), name: "OpFMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(134), name: "OpUDiv", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(135), name: "OpSDiv", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(136), name: "OpFDiv", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(137), name: "OpUMod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(138), name: "OpSRem", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(139), name: "OpSMod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(140), name: "OpFRem", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(141), name: "OpFMod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(142), name: "OpVectorTimesScalar", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(143), name: "OpMatrixTimesScalar", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(144), name: "OpVectorTimesMatrix", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(145), name: "OpMatrixTimesVector", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(146), name: "OpMatrixTimesMatrix", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(147), name: "OpOuterProduct", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(148), name: "OpDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(149), name: "OpIAddCarry", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(150), name: "OpISubBorrow", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(151), name: "OpUMulExtended", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(152), name: "OpSMulExtended", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(154), name: "OpAny", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(155), name: "OpAll", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(156), name: "OpIsNan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(157), name: "OpIsInf", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(158), name: "OpIsFinite", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(159), name: "OpIsNormal", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(160), name: "OpSignBitSet", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(161), name: "OpLessOrGreater", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(162), name: "OpOrdered", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(163), name: "OpUnordered", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(164), name: "OpLogicalEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(165), name: "OpLogicalNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(166), name: "OpLogicalOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(167), name: "OpLogicalAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(168), name: "OpLogicalNot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(169), name: "OpSelect", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(170), name: "OpIEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(171), name: "OpINotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(172), name: "OpUGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(173), name: "OpSGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(174), name: "OpUGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(175), name: "OpSGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(176), name: "OpULessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(177), name: "OpSLessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(178), name: "OpULessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(179), name: "OpSLessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(180), name: "OpFOrdEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(181), name: "OpFUnordEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(182), name: "OpFOrdNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(183), name: "OpFUnordNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(184), name: "OpFOrdLessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(185), name: "OpFUnordLessThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(186), name: "OpFOrdGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(187), name: "OpFUnordGreaterThan", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(188), name: "OpFOrdLessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(189), name: "OpFUnordLessThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(190), name: "OpFOrdGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(191), name: "OpFUnordGreaterThanEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(194), name: "OpShiftRightLogical", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(195), name: "OpShiftRightArithmetic", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(196), name: "OpShiftLeftLogical", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(197), name: "OpBitwiseOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(198), name: "OpBitwiseXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(199), name: "OpBitwiseAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(200), name: "OpNot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(201), name: "OpBitFieldInsert", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(202), name: "OpBitFieldSExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(203), name: "OpBitFieldUExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(204), name: "OpBitReverse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(205), name: "OpBitCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(207), name: "OpDPdx", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(208), name: "OpDPdy", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(209), name: "OpFwidth", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(210), name: "OpDPdxFine", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(211), name: "OpDPdyFine", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(212), name: "OpFwidthFine", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(213), name: "OpDPdxCoarse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(214), name: "OpDPdyCoarse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(215), name: "OpFwidthCoarse", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(218), name: "OpEmitVertex", operands: &[] },
    Spec { op: Op(219), name: "OpEndPrimitive", operands: &[] },
    Spec { op: Op(220), name: "OpEmitStreamVertex", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(221), name: "OpEndStreamPrimitive", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(224), name: "OpControlBarrier", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(225), name: "OpMemoryBarrier", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(227), name: "OpAtomicLoad", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(228), name: "OpAtomicStore", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(229), name: "OpAtomicExchange", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(230), name: "OpAtomicCompareExchange", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(231), name: "OpAtomicCompareExchangeWeak", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(232), name: "OpAtomicIIncrement", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(233), name: "OpAtomicIDecrement", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(234), name: "OpAtomicIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(235), name: "OpAtomicISub", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(236), name: "OpAtomicSMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(237), name: "OpAtomicUMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(238), name: "OpAtomicSMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(239), name: "OpAtomicUMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(240), name: "OpAtomicAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(241), name: "OpAtomicOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(242), name: "OpAtomicXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(245), name: "OpPhi", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::PairIdRefIdRef, Quantity::Many)] },
    Spec { op: Op(246), name: "OpLoopMerge", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LoopControl, Quantity::One)] },
    Spec { op: Op(247), name: "OpSelectionMerge", operands: &[(Kind::IdRef, Quantity::One), (Kind::SelectionControl, Quantity::One)] },
    Spec { op: Op(248), name: "OpLabel", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(249), name: "OpBranch", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(250), name: "OpBranchConditional", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::Many)] },
    Spec { op: Op(251), name: "OpSwitch", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PairLiteralIntegerIdRef, Quantity::Many)] },
    Spec { op: Op(252), name: "OpKill", operands: &[] },
    Spec { op: Op(253), name: "OpReturn", operands: &[] },
    Spec { op: Op(254), name: "OpReturnValue", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(255), name: "OpUnreachable", operands: &[] },
    Spec { op: Op(256), name: "OpLifetimeStart", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(257), name: "OpLifetimeStop", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(259), name: "OpGroupAsyncCopy", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(260), name: "OpGroupWaitEvents", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(274), name: "OpReadPipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(275), name: "OpWritePipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(276), name: "OpReservedReadPipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(277), name: "OpReservedWritePipe", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(278), name: "OpReserveReadPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(279), name: "OpReserveWritePipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(280), name: "OpCommitReadPipe", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(281), name: "OpCommitWritePipe", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(282), name: "OpIsValidReserveId", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(283), name: "OpGetNumPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(284), name: "OpGetMaxPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(285), name: "OpGroupReserveReadPipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(286), name: "OpGroupReserveWritePipePackets", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(287), name: "OpGroupCommitReadPipe", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(288), name: "OpGroupCommitWritePipe", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(291), name: "OpEnqueueMarker", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(292), name: "OpEnqueueKernel", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(293), name: "OpGetKernelNDrangeSubGroupCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(294), name: "OpGetKernelNDrangeMaxSubGroupSize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(295), name: "OpGetKernelWorkGroupSize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(296), name: "OpGetKernelPreferredWorkGroupSizeMultiple", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(297), name: "OpRetainEvent", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(298), name: "OpReleaseEvent", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(299), name: "OpCreateUserEvent", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(300), name: "OpIsValidEvent", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(301), name: "OpSetUserEventStatus", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(302), name: "OpCaptureEventProfilingInfo", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(303), name: "OpGetDefaultQueue", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(304), name: "OpBuildNDRange", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(305), name: "OpImageSparseSampleImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(306), name: "OpImageSparseSampleExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(307), name: "OpImageSparseSampleDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(308), name: "OpImageSparseSampleDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(309), name: "OpImageSparseSampleProjImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(310), name: "OpImageSparseSampleProjExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(311), name: "OpImageSparseSampleProjDrefImplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(312), name: "OpImageSparseSampleProjDrefExplicitLod", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::One)] },
    Spec { op: Op(313), name: "OpImageSparseFetch", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(314), name: "OpImageSparseGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(315), name: "OpImageSparseDrefGather", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(316), name: "OpImageSparseTexelsResident", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(317), name: "OpNoLine", operands: &[] },
    Spec { op: Op(318), name: "OpAtomicFlagTestAndSet", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(319), name: "OpAtomicFlagClear", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(320), name: "OpImageSparseRead", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::ImageOperands, Quantity::Optional)] },
    Spec { op: Op(321), name: "OpSizeOf", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(322), name: "OpTypePipeStorage", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(323), name: "OpConstantPipeStorage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(324), name: "OpCreatePipeFromPipeStorage", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(325), name: "OpGetKernelLocalSizeForSubgroupCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(326), name: "OpGetKernelMaxNumSubgroups", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(327), name: "OpTypeNamedBarrier", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(328), name: "OpNamedBarrierInitialize", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(329), name: "OpMemoryNamedBarrier", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(330), name: "OpModuleProcessed", operands: &[(Kind::LiteralString, Quantity::One)] },
    Spec { op: Op(331), name: "OpExecutionModeId", operands: &[(Kind::IdRef, Quantity::One), (Kind::ExecutionMode, Quantity::One)] },
    Spec { op: Op(333), name: "OpGroupNonUniformElect", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One)] },
    Spec { op: Op(334), name: "OpGroupNonUniformAll", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(335), name: "OpGroupNonUniformAny", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(336), name: "OpGroupNonUniformAllEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(337), name: "OpGroupNonUniformBroadcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(338), name: "OpGroupNonUniformBroadcastFirst", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(339), name: "OpGroupNonUniformBallot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(340), name: "OpGroupNonUniformInverseBallot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(341), name: "OpGroupNonUniformBallotBitExtract", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(342), name: "OpGroupNonUniformBallotBitCount", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(343), name: "OpGroupNonUniformBallotFindLSB", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(344), name: "OpGroupNonUniformBallotFindMSB", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(345), name: "OpGroupNonUniformShuffle", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(346), name: "OpGroupNonUniformShuffleXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(347), name: "OpGroupNonUniformShuffleUp", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(348), name: "OpGroupNonUniformShuffleDown", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(349), name: "OpGroupNonUniformIAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(350), name: "OpGroupNonUniformFAdd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(351), name: "OpGroupNonUniformIMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(352), name: "OpGroupNonUniformFMul", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(353), name: "OpGroupNonUniformSMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(354), name: "OpGroupNonUniformUMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(355), name: "OpGroupNonUniformFMin", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(356), name: "OpGroupNonUniformSMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(357), name: "OpGroupNonUniformUMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(358), name: "OpGroupNonUniformFMax", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(359), name: "OpGroupNonUniformBitwiseAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(360), name: "OpGroupNonUniformBitwiseOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(361), name: "OpGroupNonUniformBitwiseXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(362), name: "OpGroupNonUniformLogicalAnd", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(363), name: "OpGroupNonUniformLogicalOr", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(364), name: "OpGroupNonUniformLogicalXor", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(365), name: "OpGroupNonUniformQuadBroadcast", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(366), name: "OpGroupNonUniformQuadSwap", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(400), name: "OpCopyLogical", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(401), name: "OpPtrEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(402), name: "OpPtrNotEqual", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(403), name: "OpPtrDiff", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4160), name: "OpColorAttachmentReadEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(4161), name: "OpDepthAttachmentReadEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(4162), name: "OpStencilAttachmentReadEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(4195), name: "OpBitcastExtractEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4416), name: "OpTerminateInvocation", operands: &[] },
    Spec { op: Op(4417), name: "OpTypeUntypedPointerKHR", operands: &[(Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One)] },
    Spec { op: Op(4418), name: "OpUntypedVariableKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(4419), name: "OpUntypedAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(4420), name: "OpUntypedInBoundsAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(4421), name: "OpSubgroupBallotKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4422), name: "OpSubgroupFirstInvocationKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4423), name: "OpUntypedPtrAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(4424), name: "OpUntypedInBoundsPtrAccessChainKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(4425), name: "OpUntypedArrayLengthKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One)] },
    Spec { op: Op(4426), name: "OpUntypedPrefetchKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(4427), name: "OpFmaKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4428), name: "OpSubgroupAllKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4429), name: "OpSubgroupAnyKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4430), name: "OpSubgroupAllEqualKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4431), name: "OpGroupNonUniformRotateKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(4432), name: "OpSubgroupReadInvocationKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4433), name: "OpExtInstWithForwardRefsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::LiteralExtInstInteger, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(4434), name: "OpUntypedGroupAsyncCopyKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::MemoryAccess, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(4445), name: "OpTraceRayKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4446), name: "OpExecuteCallableKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4447), name: "OpConvertUToAccelerationStructureKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4448), name: "OpIgnoreIntersectionKHR", operands: &[] },
    Spec { op: Op(4449), name: "OpTerminateRayKHR", operands: &[] },
    Spec { op: Op(4450), name: "OpSDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)] },
    Spec { op: Op(4451), name: "OpUDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)] },
    Spec { op: Op(4452), name: "OpSUDot", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)] },
    Spec { op: Op(4453), name: "OpSDotAccSat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)] },
    Spec { op: Op(4454), name: "OpUDotAccSat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)] },
    Spec { op: Op(4455), name: "OpSUDotAccSat", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::PackedVectorFormat, Quantity::Optional)] },
    Spec { op: Op(4456), name: "OpTypeCooperativeMatrixKHR", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4457), name: "OpCooperativeMatrixLoadKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(4458), name: "OpCooperativeMatrixStoreKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::MemoryAccess, Quantity::Optional)] },
    Spec { op: Op(4459), name: "OpCooperativeMatrixMulAddKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::CooperativeMatrixOperands, Quantity::Optional)] },
    Spec { op: Op(4460), name: "OpCooperativeMatrixLengthKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4461), name: "OpConstantCompositeReplicateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4462), name: "OpSpecConstantCompositeReplicateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4463), name: "OpCompositeConstructReplicateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4472), name: "OpTypeRayQueryKHR", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(4473), name: "OpRayQueryInitializeKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4474), name: "OpRayQueryTerminateKHR", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4475), name: "OpRayQueryGenerateIntersectionKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4476), name: "OpRayQueryConfirmIntersectionKHR", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4477), name: "OpRayQueryProceedKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(4479), name: "OpRayQueryGetIntersectionTypeKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5056), name: "OpReadClockKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One)] },
    Spec { op: Op(5110), name: "OpGroupNonUniformQuadAllKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5111), name: "OpGroupNonUniformQuadAnyKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5115), name: "OpTypeBufferEXT", operands: &[(Kind::IdResult, Quantity::One), (Kind::StorageClass, Quantity::One)] },
    Spec { op: Op(5119), name: "OpBufferPointerEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5121), name: "OpAbortKHR", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5126), name: "OpUntypedImageTexelPointerEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5127), name: "OpMemberDecorateIdEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::LiteralInteger, Quantity::One), (Kind::Decoration, Quantity::One)] },
    Spec { op: Op(5129), name: "OpConstantSizeOfEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5147), name: "OpConstantDataKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::Many)] },
    Spec { op: Op(5148), name: "OpSpecConstantDataKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::LiteralInteger, Quantity::Many)] },
    Spec { op: Op(5288), name: "OpTypeVectorIdEXT", operands: &[(Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5293), name: "OpCooperativeMatrixConvertUseEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5294), name: "OpEmitMeshTasksEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(5295), name: "OpSetMeshOutputsEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5296), name: "OpGroupNonUniformPartitionEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5304), name: "OpHitObjectRecordFromQueryEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(5305), name: "OpHitObjectRecordMissEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5306), name: "OpHitObjectRecordMissMotionEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5307), name: "OpHitObjectGetIntersectionTriangleVertexPositionsEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5308), name: "OpHitObjectGetRayFlagsEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5309), name: "OpHitObjectSetShaderBindingTableRecordIndexEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5310), name: "OpHitObjectReorderExecuteShaderEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(5311), name: "OpHitObjectTraceReorderExecuteEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(5312), name: "OpHitObjectTraceMotionReorderExecuteEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(5313), name: "OpTypeHitObjectEXT", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(5314), name: "OpReorderThreadWithHintEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5315), name: "OpReorderThreadWithHitObjectEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Optional), (Kind::IdRef, Quantity::Optional)] },
    Spec { op: Op(5316), name: "OpHitObjectTraceRayEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5317), name: "OpHitObjectTraceRayMotionEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5318), name: "OpHitObjectRecordEmptyEXT", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5319), name: "OpHitObjectExecuteShaderEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5320), name: "OpHitObjectGetCurrentTimeEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5321), name: "OpHitObjectGetAttributesEXT", operands: &[(Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5322), name: "OpHitObjectGetHitKindEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5323), name: "OpHitObjectGetPrimitiveIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5324), name: "OpHitObjectGetGeometryIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5325), name: "OpHitObjectGetInstanceIdEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5326), name: "OpHitObjectGetInstanceCustomIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5327), name: "OpHitObjectGetObjectRayOriginEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5328), name: "OpHitObjectGetObjectRayDirectionEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5329), name: "OpHitObjectGetWorldRayDirectionEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5330), name: "OpHitObjectGetWorldRayOriginEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5331), name: "OpHitObjectGetObjectToWorldEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5332), name: "OpHitObjectGetWorldToObjectEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5333), name: "OpHitObjectGetRayTMaxEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5334), name: "OpReportIntersectionKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5340), name: "OpRayQueryGetIntersectionTriangleVertexPositionsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5341), name: "OpTypeAccelerationStructureKHR", operands: &[(Kind::IdResult, Quantity::One)] },
    Spec { op: Op(5347), name: "OpHitObjectGetRayTMinEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5348), name: "OpHitObjectGetShaderBindingTableRecordIndexEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5349), name: "OpHitObjectGetShaderRecordBufferHandleEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5350), name: "OpHitObjectIsEmptyEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5351), name: "OpHitObjectIsHitEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5352), name: "OpHitObjectIsMissEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5363), name: "OpCooperativeMatrixGetCoordinateEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5364), name: "OpBeginInvocationInterlockEXT", operands: &[] },
    Spec { op: Op(5365), name: "OpEndInvocationInterlockEXT", operands: &[] },
    Spec { op: Op(5366), name: "OpCooperativeMatrixReduceEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::CooperativeMatrixReduce, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5369), name: "OpCooperativeMatrixPerElementOpEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::Many)] },
    Spec { op: Op(5380), name: "OpDemoteToHelperInvocation", operands: &[] },
    Spec { op: Op(5381), name: "OpIsHelperInvocationEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One)] },
    Spec { op: Op(5614), name: "OpAtomicFMinEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5615), name: "OpAtomicFMaxEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5630), name: "OpAssumeTrueKHR", operands: &[(Kind::IdRef, Quantity::One)] },
    Spec { op: Op(5631), name: "OpExpectKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6016), name: "OpRayQueryGetRayTMinKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6017), name: "OpRayQueryGetRayFlagsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6018), name: "OpRayQueryGetIntersectionTKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6019), name: "OpRayQueryGetIntersectionInstanceCustomIndexKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6020), name: "OpRayQueryGetIntersectionInstanceIdKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6021), name: "OpRayQueryGetIntersectionInstanceShaderBindingTableRecordOffsetKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6022), name: "OpRayQueryGetIntersectionGeometryIndexKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6023), name: "OpRayQueryGetIntersectionPrimitiveIndexKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6024), name: "OpRayQueryGetIntersectionBarycentricsKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6025), name: "OpRayQueryGetIntersectionFrontFaceKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6026), name: "OpRayQueryGetIntersectionCandidateAABBOpaqueKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6027), name: "OpRayQueryGetIntersectionObjectRayDirectionKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6028), name: "OpRayQueryGetIntersectionObjectRayOriginKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6029), name: "OpRayQueryGetWorldRayDirectionKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6030), name: "OpRayQueryGetWorldRayOriginKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6031), name: "OpRayQueryGetIntersectionObjectToWorldKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6032), name: "OpRayQueryGetIntersectionWorldToObjectKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6035), name: "OpAtomicFAddEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6142), name: "OpControlBarrierArriveEXT", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(6143), name: "OpControlBarrierWaitEXT", operands: &[(Kind::IdScope, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::IdMemorySemantics, Quantity::One)] },
    Spec { op: Op(6145), name: "OpArithmeticFenceEXT", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6401), name: "OpGroupIMulKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6402), name: "OpGroupFMulKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6403), name: "OpGroupBitwiseAndKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6404), name: "OpGroupBitwiseOrKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6405), name: "OpGroupBitwiseXorKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6406), name: "OpGroupLogicalAndKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6407), name: "OpGroupLogicalOrKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
    Spec { op: Op(6408), name: "OpGroupLogicalXorKHR", operands: &[(Kind::IdResultType, Quantity::One), (Kind::IdResult, Quantity::One), (Kind::IdScope, Quantity::One), (Kind::GroupOperation, Quantity::One), (Kind::IdRef, Quantity::One)] },
];

/// The `GLSL.std.450` extended instructions, by ascending number.
pub(crate) static GLSL_STD_450: [ExtInst; 81] = [
    ExtInst { number: 1, operands: 1 },
    ExtInst { number: 2, operands: 1 },
    ExtInst { number: 3, operands: 1 },
    ExtInst { number: 4, operands: 1 },
    ExtInst { number: 5, operands: 1 },
    ExtInst { number: 6, operands: 1 },
    ExtInst { number: 7, operands: 1 },
    ExtInst { number: 8, operands: 1 },
    ExtInst { number: 9, operands: 1 },
    ExtInst { number: 10, operands: 1 },
    ExtInst { number: 11, operands: 1 },
    ExtInst { number: 12, operands: 1 },
    ExtInst { number: 13, operands: 1 },
    ExtInst { number: 14, operands: 1 },
    ExtInst { number: 15, operands: 1 },
    ExtInst { number: 16, operands: 1 },
    ExtInst { number: 17, operands: 1 },
    ExtInst { number: 18, operands: 1 },
    ExtInst { number: 19, operands: 1 },
    ExtInst { number: 20, operands: 1 },
    ExtInst { number: 21, operands: 1 },
    ExtInst { number: 22, operands: 1 },
    ExtInst { number: 23, operands: 1 },
    ExtInst { number: 24, operands: 1 },
    ExtInst { number: 25, operands: 2 },
    ExtInst { number: 26, operands: 2 },
    ExtInst { number: 27, operands: 1 },
    ExtInst { number: 28, operands: 1 },
    ExtInst { number: 29, operands: 1 },
    ExtInst { number: 30, operands: 1 },
    ExtInst { number: 31, operands: 1 },
    ExtInst { number: 32, operands: 1 },
    ExtInst { number: 33, operands: 1 },
    ExtInst { number: 34, operands: 1 },
    ExtInst { number: 35, operands: 2 },
    ExtInst { number: 36, operands: 1 },
    ExtInst { number: 37, operands: 2 },
    ExtInst { number: 38, operands: 2 },
    ExtInst { number: 39, operands: 2 },
    ExtInst { number: 40, operands: 2 },
    ExtInst { number: 41, operands: 2 },
    ExtInst { number: 42, operands: 2 },
    ExtInst { number: 43, operands: 3 },
    ExtInst { number: 44, operands: 3 },
    ExtInst { number: 45, operands: 3 },
    ExtInst { number: 46, operands: 3 },
    ExtInst { number: 47, operands: 3 },
    ExtInst { number: 48, operands: 2 },
    ExtInst { number: 49, operands: 3 },
    ExtInst { number: 50, operands: 3 },
    ExtInst { number: 51, operands: 2 },
    ExtInst { number: 52, operands: 1 },
    ExtInst { number: 53, operands: 2 },
    ExtInst { number: 54, operands: 1 },
    ExtInst { number: 55, operands: 1 },
    ExtInst { number: 56, operands: 1 },
    ExtInst { number: 57, operands: 1 },
    ExtInst { number: 58, operands: 1 },
    ExtInst { number: 59, operands: 1 },
    ExtInst { number: 60, operands: 1 },
    ExtInst { number: 61, operands: 1 },
    ExtInst { number: 62, operands: 1 },
    ExtInst { number: 63, operands: 1 },
    ExtInst { number: 64, operands: 1 },
    ExtInst { number: 65, operands: 1 },
    ExtInst { number: 66, operands: 1 },
    ExtInst { number: 67, operands: 2 },
    ExtInst { number: 68, operands: 2 },
    ExtInst { number: 69, operands: 1 },
    ExtInst { number: 70, operands: 3 },
    ExtInst { number: 71, operands: 2 },
    ExtInst { number: 72, operands: 3 },
    ExtInst { number: 73, operands: 1 },
    ExtInst { number: 74, operands: 1 },
    ExtInst { number: 75, operands: 1 },
    ExtInst { number: 76, operands: 1 },
    ExtInst { number: 77, operands: 2 },
    ExtInst { number: 78, operands: 2 },
    ExtInst { number: 79, operands: 2 },
    ExtInst { number: 80, operands: 2 },
    ExtInst { number: 81, operands: 3 },
];
