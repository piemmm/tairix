//! What lowering holds before it refuses a program past its instructions:
//! each kind of thing it builds is counted against the program's own bound as
//! it is built, so a refused program holds no more than one could.

use std::fmt::Write as _;
use std::sync::Arc;

use tairix_abi::time::MonotonicClock;
use tairix_fuzzseed::meter::{metered, Metered};
use tairix_gpu::{
    ComputePipelineDesc, ContextDesc, Device, Error, NoThreads, PushConstants, SoftwareDesc,
    DEFAULT_SUBGROUP_SIZE,
};
use tairix_spirv::assemble::admit;

#[global_allocator]
static ALLOCATOR: Metered = Metered;

struct Stopped;

impl MonotonicClock for Stopped {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// A compute module whose entry point calls, `calls` times, a function of
/// one instruction making `copies` copies.
fn module(calls: usize, copies: usize) -> String {
    let parts = " %one".repeat(copies);
    let mut text = format!(
        r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main"
OpExecutionMode %main LocalSize 1 1 1
%void = OpTypeVoid
%fn = OpTypeFunction %void
%uint = OpTypeInt 32 0
%one = OpConstant %uint 1
%length = OpConstant %uint {copies}
%row = OpTypeArray %uint %length
%main = OpFunction %void None %fn
%entry = OpLabel
"#
    );
    for k in 0..calls {
        let _ = writeln!(text, "%r{k} = OpFunctionCall %void %leaf");
    }
    let _ = write!(
        text,
        "OpReturn\nOpFunctionEnd\n%leaf = OpFunction %void None %fn\n%l = OpLabel\n%built = OpCompositeConstruct %row{parts}\nOpReturn\nOpFunctionEnd\n"
    );
    text
}

#[test]
fn a_program_past_its_instructions_holds_no_more_than_one_could_before_it_is_refused() {
    let device = Device::software(&SoftwareDesc {
        memory_bytes: 1 << 30,
        clock: Arc::new(Stopped),
        runner: Arc::new(tairix_parallel::Serial),
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a device");
    let context = device
        .create_context(&ContextDesc::default())
        .expect("a context");
    let layout = context
        .create_pipeline_layout(&[], PushConstants::NONE)
        .expect("a layout");
    let lower = |calls: usize| {
        let module = context
            .create_shader_module(admit(&module(calls, 1024)).expect("admitted"))
            .expect("a module");
        metered(|| {
            context
                .create_compute_pipeline(&ComputePipelineDesc {
                    module: &module,
                    entry_point: "main",
                    layout: &layout,
                    specialization: &[],
                })
                .map(drop)
        })
    };
    // Half the instructions a program holds, to measure what each costs.
    let (made, half) = lower(512);
    assert_eq!(made, Ok(()));
    // Twice them: refused once the program's bound is passed, not after.
    let (refused, twice) = lower(2048);
    assert_eq!(refused, Err(Error::TooLarge));
    assert!(
        twice.peak < half.peak * 3,
        "{} MiB held refusing twice the bound, {} MiB lowering half of it",
        twice.peak >> 20,
        half.peak >> 20
    );
}
