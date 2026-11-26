//! What the front end's run-time tests share: a module compiled and
//! admitted again, and the software device it runs on.

use std::sync::Arc;

use tairix_abi::time::MonotonicClock;
use tairix_gpu::{
    Allocation, AllocationDesc, BindSetEntry, Buffer, BufferDesc, BufferUsage, CommandBuffer,
    ComputePipelineDesc, Context, ContextDesc, DescriptorType, Device, LayoutBinding, NoThreads,
    PushConstants, Resource, SoftwareDesc, Submission, DEFAULT_SUBGROUP_SIZE,
};
use tairix_spirv::{Access, Admitted, Module, Stages};
use tairix_wgsl::{compile, Options};

/// The words of each storage binding a test shader reads or writes.
pub const WORDS: u64 = 1024;

/// The two bindings every test shader declares: binding 0 written, binding 1
/// read, both `array<u32>`.
pub const BINDINGS: &str = "
    @group(0) @binding(0) var<storage, read_write> out: array<u32>;
    @group(0) @binding(1) var<storage, read> input: array<u32>;
";

/// A clock that never moves: no dispatch is stopped at a deadline.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// `body` with [`BINDINGS`] after its directives, which come first.
pub fn with_bindings(body: &str) -> String {
    let directive = |line: &str| {
        let line = line.trim_start();
        line.is_empty() || line.starts_with("enable ") || line.starts_with("requires ")
    };
    let split = body
        .split_inclusive('\n')
        .take_while(|line| directive(line))
        .map(str::len)
        .sum::<usize>();
    let (directives, rest) = body.split_at(split);
    format!("{directives}{BINDINGS}{rest}")
}

/// `source` compiled with `options` and admitted by the validator again.
///
/// # Panics
///
/// Where it does not compile, or its module is refused, naming the source.
pub fn admitted(source: &str, options: &Options<'_>) -> Admitted {
    let compiled =
        compile(source, options).unwrap_or_else(|failure| panic!("{failure:?}\n{source}"));
    let module = Module::decode(&compiled.module).expect("a module the decoder reads");
    Admitted::new(module).unwrap_or_else(|refusal| panic!("refused again: {refusal:?}\n{source}"))
}

/// The words of `bytes`, little-endian.
pub fn words_of(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&word| u32::from_le_bytes(word))
        .collect()
}

/// `words` little-endian in `len` bytes, zero past them.
pub fn bytes_of(words: &[u32], len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    for (word, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *word = value.to_le_bytes();
    }
    bytes
}

/// A software device and one context on it.
pub struct Gpu {
    pub device: Device,
    pub context: Context,
}

impl Gpu {
    pub fn new() -> Self {
        let device = Device::software(&SoftwareDesc {
            memory_bytes: 16 << 20,
            clock: Arc::new(Still),
            runner: Arc::new(tairix_parallel::Serial),
            threads: &NoThreads,
            subgroup_size: DEFAULT_SUBGROUP_SIZE,
        })
        .expect("a software device");
        let context = device
            .create_context(&ContextDesc::default())
            .expect("a context");
        Self { device, context }
    }

    /// A buffer over all of `size` bytes of fresh memory, and the memory.
    pub fn buffer(&self, size: u64, usage: BufferUsage) -> (Allocation, Buffer) {
        let memory = self
            .context
            .allocate(&AllocationDesc {
                size,
                memory_type: 0,
            })
            .expect("memory");
        let buffer = self
            .context
            .create_buffer(
                &memory,
                &BufferDesc {
                    offset: 0,
                    size,
                    usage,
                },
            )
            .expect("a buffer");
        (memory, buffer)
    }

    /// Run `commands` to their end, the context still answering.
    pub fn submit(&self, commands: &CommandBuffer) {
        let longest = self.device.info().limits.max_job_time;
        let fence = self
            .context
            .queue(0, 0)
            .expect("a queue")
            .submit(&Submission {
                command_buffers: &[commands],
                waits: &[],
                signals: &[],
                deadline: longest,
            })
            .unwrap_or_else(|refused| panic!("refused: {refused:?}"));
        fence.wait(longest).expect("finished");
        self.context
            .status()
            .unwrap_or_else(|error| panic!("lost: {error:?}"));
    }

    /// Run `admitted`'s `main` over `groups` workgroups, binding 1 holding
    /// `input`, and answer binding 0's first `count` words.
    pub fn compute(
        &self,
        admitted: Admitted,
        input: &[u32],
        groups: u32,
        count: usize,
    ) -> Vec<u32> {
        let context = &self.context;
        let shader = context.create_shader_module(admitted).expect("a module");
        let binding = |binding, access| LayoutBinding {
            binding,
            kind: DescriptorType::StorageBuffer,
            count: 1,
            access,
            stages: Stages::COMPUTE,
        };
        let set_layout = context
            .create_bind_set_layout(&[
                binding(0, Access::READ.with(Access::WRITE)),
                binding(1, Access::READ),
            ])
            .expect("a set layout");
        let layout = context
            .create_pipeline_layout(&[&set_layout], PushConstants::NONE)
            .expect("a pipeline layout");
        let pipeline = context
            .create_compute_pipeline(&ComputePipelineDesc {
                module: &shader,
                entry_point: "main",
                layout: &layout,
                specialization: &[],
            })
            .unwrap_or_else(|error| panic!("no pipeline: {error:?}"));
        let (output_memory, output) = self.buffer(WORDS * 4, BufferUsage::STORAGE);
        let (input_memory, input_buffer) = self.buffer(WORDS * 4, BufferUsage::STORAGE);
        let bytes = bytes_of(input, usize::try_from(WORDS * 4).expect("small"));
        input_memory.write(0, &bytes).expect("written");
        let whole = |buffer| {
            [Resource::Buffer {
                buffer,
                offset: 0,
                size: WORDS * 4,
            }]
        };
        let (written, read) = (whole(&output), whole(&input_buffer));
        let set = context
            .create_bind_set(
                &set_layout,
                &[
                    BindSetEntry {
                        binding: 0,
                        resources: &written,
                    },
                    BindSetEntry {
                        binding: 1,
                        resources: &read,
                    },
                ],
            )
            .expect("a set");
        let mut commands = context.command_buffer();
        commands
            .dispatch(&pipeline, &[&set], &[], [groups, 1, 1])
            .expect("recorded");
        self.submit(&commands);
        let mut out = vec![0u8; count * 4];
        output_memory.read(0, &mut out).expect("read back");
        words_of(&out)
    }
}
