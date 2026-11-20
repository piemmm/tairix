//! The modules the work sweep's pipelines run.

/// Four invocations a workgroup, each storing into word `place` of set 0's
/// binding 0 its place among the dispatch's invocations, counted along x,
/// then y, then z, exclusive-or'd with the push constants' one word.
pub(super) const KEYED: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %gid %groups %buf %push
OpExecutionMode %main LocalSize 4 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %groups BuiltIn NumWorkgroups
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %words ArrayStride 4
OpDecorate %pushed Block
OpMemberDecorate %pushed 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%v3uint = OpTypeVector %uint 3
%zero = OpConstant %uint 0
%four = OpConstant %uint 4
%words = OpTypeRuntimeArray %uint
%block = OpTypeStruct %words
%block_ptr = OpTypePointer StorageBuffer %block
%word_ptr = OpTypePointer StorageBuffer %uint
%v3_in = OpTypePointer Input %v3uint
%gid = OpVariable %v3_in Input
%groups = OpVariable %v3_in Input
%buf = OpVariable %block_ptr StorageBuffer
%pushed = OpTypeStruct %uint
%pushed_ptr = OpTypePointer PushConstant %pushed
%push_word = OpTypePointer PushConstant %uint
%push = OpVariable %pushed_ptr PushConstant
%main = OpFunction %void None %fn
%entry = OpLabel
%g = OpLoad %v3uint %gid
%n = OpLoad %v3uint %groups
%gx = OpCompositeExtract %uint %g 0
%gy = OpCompositeExtract %uint %g 1
%gz = OpCompositeExtract %uint %g 2
%nx = OpCompositeExtract %uint %n 0
%ny = OpCompositeExtract %uint %n 1
%across = OpIMul %uint %nx %four
%plane = OpIMul %uint %across %ny
%down = OpIMul %uint %gy %across
%deep = OpIMul %uint %gz %plane
%row = OpIAdd %uint %gx %down
%place = OpIAdd %uint %row %deep
%key_ptr = OpAccessChain %push_word %push %zero
%key = OpLoad %uint %key_ptr
%value = OpBitwiseXor %uint %place %key
%at = OpAccessChain %word_ptr %buf %zero %place
OpStore %at %value
OpReturn
OpFunctionEnd
"#;

/// A vertex stage passing its position, at location 0, and its colour, at
/// location 1, through to location 0.
pub(super) const PLAIN_VERTEX: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %in_position %in_colour %out_position %out_colour
OpDecorate %in_position Location 0
OpDecorate %in_colour Location 1
OpDecorate %out_position BuiltIn Position
OpDecorate %out_colour Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%in_position = OpVariable %in_v4 Input
%in_colour = OpVariable %in_v4 Input
%out_position = OpVariable %out_v4 Output
%out_colour = OpVariable %out_v4 Output
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpLoad %v4 %in_position
%c = OpLoad %v4 %in_colour
OpStore %out_position %p
OpStore %out_colour %c
OpReturn
OpFunctionEnd
"#;

/// A fragment stage writing its colour at location 0.
pub(super) const PLAIN_FRAGMENT: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_colour %out_colour
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_colour Location 0
OpDecorate %out_colour Location 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%in_colour = OpVariable %in_v4 Input
%out_colour = OpVariable %out_v4 Output
%main = OpFunction %void None %fn
%entry = OpLabel
%c = OpLoad %v4 %in_colour
OpStore %out_colour %c
OpReturn
OpFunctionEnd
"#;

/// [`PLAIN_VERTEX`], its position moved by the vector set 0's uniform buffer
/// at binding 1 holds.
pub(super) const MOVED_VERTEX: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Vertex %main "main" %in_position %in_colour %out_position %out_colour %moves
OpDecorate %in_position Location 0
OpDecorate %in_colour Location 1
OpDecorate %out_position BuiltIn Position
OpDecorate %out_colour Location 0
OpDecorate %moves DescriptorSet 0
OpDecorate %moves Binding 1
OpDecorate %move Block
OpMemberDecorate %move 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%float = OpTypeFloat 32
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%move = OpTypeStruct %v4
%move_ptr = OpTypePointer Uniform %move
%v4_uniform = OpTypePointer Uniform %v4
%in_position = OpVariable %in_v4 Input
%in_colour = OpVariable %in_v4 Input
%out_position = OpVariable %out_v4 Output
%out_colour = OpVariable %out_v4 Output
%moves = OpVariable %move_ptr Uniform
%main = OpFunction %void None %fn
%entry = OpLabel
%p = OpLoad %v4 %in_position
%c = OpLoad %v4 %in_colour
%by_ptr = OpAccessChain %v4_uniform %moves %zero
%by = OpLoad %v4 %by_ptr
%moved = OpFAdd %v4 %p %by
OpStore %out_position %moved
OpStore %out_colour %c
OpReturn
OpFunctionEnd
"#;

/// A fragment stage writing set 0's image at binding 0, sampled at its
/// colour's first two channels, scaled by the push constants' vector.
pub(super) const TEXTURED_FRAGMENT: &str = r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main" %in_colour %out_colour %picture %tint
OpExecutionMode %main OriginUpperLeft
OpDecorate %in_colour Location 0
OpDecorate %out_colour Location 0
OpDecorate %picture DescriptorSet 0
OpDecorate %picture Binding 0
OpDecorate %tinted Block
OpMemberDecorate %tinted 0 Offset 0
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%zero = OpConstant %uint 0
%float = OpTypeFloat 32
%v2 = OpTypeVector %float 2
%v4 = OpTypeVector %float 4
%in_v4 = OpTypePointer Input %v4
%out_v4 = OpTypePointer Output %v4
%image_t = OpTypeImage %float 2D 0 0 0 1 Unknown
%si_t = OpTypeSampledImage %image_t
%si_ptr = OpTypePointer UniformConstant %si_t
%tinted = OpTypeStruct %v4
%tinted_ptr = OpTypePointer PushConstant %tinted
%v4_push = OpTypePointer PushConstant %v4
%in_colour = OpVariable %in_v4 Input
%out_colour = OpVariable %out_v4 Output
%picture = OpVariable %si_ptr UniformConstant
%tint = OpVariable %tinted_ptr PushConstant
%main = OpFunction %void None %fn
%entry = OpLabel
%c = OpLoad %v4 %in_colour
%uv = OpVectorShuffle %v2 %c %c 0 1
%sampled = OpLoad %si_t %picture
%texel = OpImageSampleImplicitLod %v4 %sampled %uv
%scale_ptr = OpAccessChain %v4_push %tint %zero
%scale = OpLoad %v4 %scale_ptr
%out = OpFMul %v4 %texel %scale
OpStore %out_colour %out
OpReturn
OpFunctionEnd
"#;
