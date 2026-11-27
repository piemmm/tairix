//! What the crate's Venus tests and its fuzz harness
//! (`tests/fuzz_venus_replies.rs`) build replies with, and the rounds the
//! harness runs over the reader. Built only for tests, behind the crate's
//! `test-support` feature.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Debug;

use core::convert::Infallible;

use tairix_abi::time::MonotonicClock;
use tairix_abi::DriverError;
use tairix_gpu::{
    BufferViewDesc, Commands, Device, Error, Format, Hardware, ImageDesc, ImageViewDesc, Job,
    LayoutBinding, MemoryRequirements, PipelineStage, RenderStages, SamplerDesc, SetResource,
};

use super::info::{feature_chain, narrow, HostDevice};
use super::reply::{
    chained_members, HostExtensions, HostFeatures, HostFormatFeatures, HostImageLimits, HostMemory,
    HostMemoryHeap, HostMemoryType, HostQueueFamily, HostRequirements, Reader, RobustAlignments,
    ASKED, MAX_PHYSICAL_DEVICES, MAX_QUEUE_FAMILIES,
};
use super::{
    Command, Structure, VkResult, API_VERSION, CORE_FEATURES, EXTENSION_NAME_LEN, MAX_MEMORY_HEAPS,
    MAX_MEMORY_TYPES, PHYSICAL_DEVICE_NAME_LEN, UUID_LEN,
};

/// A stream written item by item.
#[derive(Clone, Default)]
pub(crate) struct Grammar(pub(crate) Vec<u8>);

impl Grammar {
    pub(crate) fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn i32(mut self, value: i32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn u64(mut self, value: u64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn f32(self, value: f32) -> Self {
        self.u32(value.to_bits())
    }

    pub(crate) fn bytes(mut self, bytes: &[u8]) -> Self {
        self.0.extend_from_slice(bytes);
        self
    }

    /// A command and its flags: `1` where it asks for a reply.
    #[cfg(test)]
    pub(crate) fn command(self, command: Command, reply: bool) -> Self {
        self.i32(command as i32).u32(u32::from(reply))
    }

    /// A string: its length with its terminator, then its bytes and the
    /// terminator, padded to whole words.
    #[cfg(test)]
    pub(crate) fn string(self, text: &str) -> Self {
        let len = text.len() + 1;
        self.u64(len as u64)
            .bytes(text.as_bytes())
            .bytes(&[0; 4][..len.next_multiple_of(4) - text.len()])
    }

    /// Booleans, a word each.
    #[cfg(test)]
    pub(crate) fn booleans(self, members: &[bool]) -> Self {
        members
            .iter()
            .fold(self, |grammar, &member| grammar.u32(u32::from(member)))
    }

    /// A structure's type and its empty chain.
    pub(crate) fn structure(self, structure: Structure) -> Self {
        self.i32(structure as i32).u64(0)
    }

    pub(crate) fn words(self, count: usize) -> Self {
        (0..count).fold(self, |grammar, _| grammar.u32(0))
    }
}

/// A reply of [`Command::GetPhysicalDeviceProperties2`], asked with the
/// robust accesses' properties where `robust` gives the granularity they are
/// bounded at, storage then uniform: the limits all zero but those set by
/// `limits`, given each member's offset in words from the limits' start.
pub(crate) fn properties_reply(
    robust: Option<RobustAlignments>,
    api_version: u32,
    limits: impl FnOnce(Grammar) -> Grammar,
) -> Grammar {
    let mut name = [0u8; PHYSICAL_DEVICE_NAME_LEN];
    name[..8].copy_from_slice(b"llvmpipe");
    let chained = Grammar::default()
        .i32(Command::GetPhysicalDeviceProperties2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceProperties2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceSubgroupProperties as i32)
        .u64(1);
    let chained = match robust {
        Some(alignments) => chained
            .i32(Structure::PhysicalDeviceMaintenance3Properties as i32)
            .u64(1)
            .structure(Structure::PhysicalDeviceRobustness2Properties)
            .u64(alignments.storage)
            .u64(alignments.uniform),
        None => chained.structure(Structure::PhysicalDeviceMaintenance3Properties),
    };
    let header = chained
        .u32(1024)
        .u64(1 << 31)
        .u32(32)
        .u32(0xff)
        .u32(0xf)
        .u32(1)
        .u32(api_version)
        .u32(1)
        .u32(0x10005)
        .u32(0)
        .i32(4)
        .u64(PHYSICAL_DEVICE_NAME_LEN as u64)
        .bytes(&name)
        .u64(UUID_LEN as u64)
        .bytes(&[0; UUID_LEN]);
    limits(header).words(5)
}

/// The limits, member by member in the renderer's order, with the values a
/// test reads back, the lines strict.
pub(crate) fn limits(grammar: Grammar) -> Grammar {
    lined_limits(grammar, true)
}

/// [`limits`], the lines strict where `strict`.
pub(crate) fn lined_limits(grammar: Grammar, strict: bool) -> Grammar {
    grammar
        .u32(16384)
        .u32(16384)
        .u32(2048)
        .u32(16384)
        .u32(2048)
        .u32(1 << 27)
        .u32(65536)
        .u32(1 << 30)
        .u32(256)
        .words(2)
        .u64(1)
        .u64(0)
        .u32(8)
        .words(7)
        .u32(1)
        .u32(2)
        .u32(3)
        .u32(4)
        .u32(5)
        .u32(6)
        .u32(7)
        .u32(8)
        .u32(32)
        .u32(32)
        .u32(2047)
        .u32(2048)
        .u32(128)
        .words(13)
        .u32(128)
        .words(3)
        .u32(32768)
        .u64(3)
        .u32(65535)
        .u32(65535)
        .u32(65535)
        .u32(1024)
        .u64(3)
        .u32(1024)
        .u32(1024)
        .u32(64)
        .words(4)
        .u32(u32::MAX)
        .f32(16.0)
        .f32(16.0)
        .u32(16)
        .u64(2)
        .u32(16384)
        .u32(16384)
        .u64(2)
        .f32(-32768.0)
        .f32(32767.0)
        .u32(0)
        .u64(64)
        .u64(16)
        .u64(64)
        .u64(16)
        .words(7)
        .u32(16384)
        .u32(16384)
        .words(5)
        .u32(8)
        .words(12)
        .u64(2)
        .f32(1.0)
        .f32(255.0)
        .u64(2)
        .f32(1.0)
        .f32(8.0)
        .words(2)
        .u32(u32::from(strict))
        .u32(1)
        .u64(1)
        .u64(1)
        .u64(64)
}

/// A reply of [`Command::EnumerateDeviceExtensionProperties`] naming
/// `names`, each at version one.
pub(crate) fn extensions_reply(names: &[&str]) -> Grammar {
    names.iter().fold(
        Grammar::default()
            .i32(Command::EnumerateDeviceExtensionProperties as i32)
            .i32(0)
            .u64(1)
            .u32(u32::try_from(names.len()).unwrap_or(u32::MAX))
            .u64(count(names.len())),
        |grammar, name| {
            let mut bytes = [0u8; EXTENSION_NAME_LEN];
            bytes[..name.len()].copy_from_slice(name.as_bytes());
            grammar.u64(count(EXTENSION_NAME_LEN)).bytes(&bytes).u32(1)
        },
    )
}

/// A reply of [`Command::GetPhysicalDeviceFeatures2`] asked with `chain`,
/// stating `features`: each chained structure's members in the chain's order,
/// then the core features.
pub(crate) fn features_reply(chain: &[Structure], features: &HostFeatures) -> Grammar {
    let mut grammar = Grammar::default()
        .i32(Command::GetPhysicalDeviceFeatures2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceFeatures2 as i32);
    for &structure in chain {
        grammar = grammar.u64(1).i32(structure as i32);
    }
    grammar = grammar.u64(0);
    for members in features.chained.iter().rev() {
        grammar = members
            .iter()
            .fold(grammar, |grammar, &member| grammar.u32(u32::from(member)));
    }
    features
        .core
        .iter()
        .fold(grammar, |grammar, &member| grammar.u32(u32::from(member)))
}

/// Every draw feature the seam names.
#[cfg(test)]
pub(crate) const DRAW_FEATURES: tairix_gpu::Features = tairix_gpu::Features::DUAL_SOURCE_BLEND
    .with(tairix_gpu::Features::LOGIC_OP)
    .with(tairix_gpu::Features::DEPTH_CLAMP)
    .with(tairix_gpu::Features::WIDE_LINES)
    .with(tairix_gpu::Features::LARGE_POINTS)
    .with(tairix_gpu::Features::VERTEX_STORES)
    .with(tairix_gpu::Features::FRAGMENT_STORES)
    .with(tairix_gpu::Features::DRAW_INDIRECT_FIRST_INSTANCE);

/// The uses a pass makes of a format, which only a device that draws states.
#[cfg(test)]
pub(crate) const DRAWN: tairix_gpu::FormatFeatures = tairix_gpu::FormatFeatures::COLOR_ATTACHMENT
    .with(tairix_gpu::FormatFeatures::DEPTH_STENCIL)
    .with(tairix_gpu::FormatFeatures::MULTISAMPLED)
    .with(tairix_gpu::FormatFeatures::BLENDED)
    .with(tairix_gpu::FormatFeatures::RESOLVED)
    .with(tairix_gpu::FormatFeatures::VERTEX_INPUT);

/// The uses a pass makes of `format` that a device that draws states: all
/// of [`DRAWN`] but an sRGB format's resolve, which a host may average
/// encoded.
#[cfg(test)]
pub(crate) fn drawn(format: tairix_gpu::Format) -> tairix_gpu::FormatFeatures {
    if format.is_srgb() {
        DRAWN.without(tairix_gpu::FormatFeatures::RESOLVED)
    } else {
        DRAWN
    }
}

/// What a device that draws states of `format` where its host serves every
/// use: every use of its encoding but those [`drawn`] leaves out.
#[cfg(test)]
pub(crate) fn every_use(format: tairix_gpu::Format) -> tairix_gpu::FormatFeatures {
    format.features().without(DRAWN.without(drawn(format)))
}

/// A backend that runs nothing, so a statement alone is judged.
pub(crate) struct Idle;

impl Hardware for Idle {
    type Context = ();
    type Memory = ();
    type Image = Infallible;
    type View = Infallible;
    type BufferView = Infallible;
    type Sampler = Infallible;
    type Set = Infallible;
    type Compute = Infallible;
    type Render = Infallible;

    fn create_context(&self) -> Result<(), Error> {
        Ok(())
    }

    fn allocate(&self, _: &Arc<()>, _: u32, _: u64) -> Result<(), Error> {
        Ok(())
    }

    fn write(&self, (): &(), _: u64, _: &[u8]) -> Result<(), Error> {
        Ok(())
    }

    fn read(&self, (): &(), _: u64, _: &mut [u8]) -> Result<(), Error> {
        Ok(())
    }

    fn image(&self, _: &Arc<()>, _: &ImageDesc) -> Result<(Infallible, MemoryRequirements), Error> {
        Err(Error::Unsupported)
    }

    fn place(&self, image: &Infallible, (): &(), _: u64) -> Result<(), Error> {
        match *image {}
    }

    fn view(
        &self,
        image: &Infallible,
        _: &ImageDesc,
        _: &ImageViewDesc,
    ) -> Result<Infallible, Error> {
        match *image {}
    }

    fn buffer_view(&self, (): &(), _: u64, _: &BufferViewDesc) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn sampler(&self, _: &Arc<()>, _: &SamplerDesc) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn set(
        &self,
        _: &Arc<()>,
        _: &[LayoutBinding],
        _: &[SetResource<'_, Self>],
    ) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn compute(&self, _: &Arc<()>, _: PipelineStage<'_>) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn render(&self, _: &Arc<()>, _: RenderStages<'_>) -> Result<Infallible, Error> {
        Err(Error::Unsupported)
    }

    fn run(&self, job: Job<Self>) {
        job.finish(Ok(()));
    }

    fn run_bytes(_: Commands<'_, Self>) -> u64 {
        0
    }

    fn lost(&self, _: &Arc<()>) {}
}

/// A reply of [`Command::GetPhysicalDeviceFormatProperties2`], asked with
/// Vulkan 1.3's format features chained where `stated`, offering `offered`:
/// in the older features, only what their 31 bits name.
pub(crate) fn format_reply(offered: HostFormatFeatures, stated: bool) -> Grammar {
    let older = |bits: u64| u32::try_from(bits & OLDER_FORMAT_FEATURES).unwrap_or(0);
    let reply = Grammar::default()
        .i32(Command::GetPhysicalDeviceFormatProperties2 as i32)
        .u64(1);
    let reply = if stated {
        reply
            .i32(Structure::FormatProperties2 as i32)
            .u64(1)
            .structure(Structure::FormatProperties3)
            .u64(0)
            .u64(offered.optimal)
            .u64(offered.buffer)
    } else {
        reply.structure(Structure::FormatProperties2)
    };
    reply
        .u32(0)
        .u32(older(offered.optimal))
        .u32(older(offered.buffer))
}

/// The format features older Vulkan names.
pub(crate) const OLDER_FORMAT_FEATURES: u64 = (1 << 31) - 1;

/// A reply of [`Command::GetPhysicalDeviceImageFormatProperties2`], asked
/// with no extension structure, answering `result` and `most`.
pub(crate) fn image_format_reply(result: i32, most: HostImageLimits) -> Grammar {
    Grammar::default()
        .i32(Command::GetPhysicalDeviceImageFormatProperties2 as i32)
        .i32(result)
        .u64(1)
        .structure(Structure::ImageFormatProperties2)
        .u32(most.extent[0])
        .u32(most.extent[1])
        .u32(most.extent[2])
        .u32(most.levels)
        .u32(most.layers)
        .u32(most.samples)
        .u64(u64::MAX)
}

/// A clock that never moves.
pub(crate) struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// A draw below `n`, which is at least one.
fn below(draw: &mut dyn FnMut() -> u64, n: usize) -> usize {
    let n = u64::try_from(n.max(1)).unwrap_or(u64::MAX);
    usize::try_from(draw() % n).unwrap_or(0)
}

fn word(draw: &mut dyn FnMut() -> u64) -> u32 {
    u32::try_from(draw() >> 32).unwrap_or(0)
}

fn signed(draw: &mut dyn FnMut() -> u64) -> i32 {
    i32::from_le_bytes(word(draw).to_le_bytes())
}

fn count(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// `bytes` read by `read` as `expected`, and refused cut short at a drawn
/// point.
fn held<T: Debug + PartialEq>(
    draw: &mut dyn FnMut() -> u64,
    bytes: &[u8],
    expected: &T,
    read: impl Fn(&[u8]) -> Result<T, DriverError>,
) {
    assert_eq!(read(bytes).as_ref(), Ok(expected));
    let cut = below(draw, bytes.len());
    assert!(
        read(&bytes[..cut]).is_err(),
        "a reply cut at {cut} of {} is read",
        bytes.len()
    );
}

/// Every reply reader over `bytes`, alone and one after another: each
/// answers or refuses, and none panics.
pub fn read_every_reply(bytes: &[u8]) {
    // The id or ids a reply names lie past its command, result and pointer or
    // count, so a reply made by [`replies`] and changed may still name them.
    let ids: Vec<u64> = bytes
        .get(16..)
        .unwrap_or(&[])
        .as_chunks::<8>()
        .0
        .iter()
        .take(4)
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect();
    let id = ids.first().copied().unwrap_or(0);
    let _ = Reader::new(bytes).result_of(Command::BindBufferMemory2);
    let _ = Reader::new(bytes).made(Command::CreateInstance, id);
    let _ = Reader::new(bytes).physical_devices(MAX_PHYSICAL_DEVICES);
    let _ = Reader::new(bytes).properties(false);
    let _ = Reader::new(bytes).properties(true);
    let _ = Reader::new(bytes).queue_families(MAX_QUEUE_FAMILIES);
    let _ = Reader::new(bytes).memory_properties();
    let _ = Reader::new(bytes).memory_requirements(Command::GetBufferMemoryRequirements2);
    let _ = Reader::new(bytes).memory_requirements(Command::GetImageMemoryRequirements2);
    let _ = Reader::new(bytes).format_properties(false);
    let _ = Reader::new(bytes).format_properties(true);
    let _ = Reader::new(bytes).image_format_properties();
    let _ = Reader::new(bytes).allocated(Command::AllocateCommandBuffers, &ids);
    let _ = Reader::new(bytes).extensions(ids.len());
    let chain = feature_chain(API_VERSION, every_extension());
    let _ = Reader::new(bytes).features(&chain);
    let mut reader = Reader::new(bytes);
    let _ = reader
        .made(Command::CreateInstance, id)
        .and_then(|_| reader.physical_devices(MAX_PHYSICAL_DEVICES))
        .and_then(|_| reader.result_of(Command::BindBufferMemory2));
}

/// A device's memory as the renderer replies it: `types` and `heaps` stated,
/// every slot past them written too.
fn memory_reply(draw: &mut dyn FnMut() -> u64, memory: &HostMemory) -> Vec<u8> {
    let typed = (0..MAX_MEMORY_TYPES).fold(
        Grammar::default()
            .i32(Command::GetPhysicalDeviceMemoryProperties2 as i32)
            .u64(1)
            .structure(Structure::PhysicalDeviceMemoryProperties2)
            .u32(u32::try_from(memory.types.len()).unwrap_or(u32::MAX))
            .u64(count(MAX_MEMORY_TYPES)),
        |grammar, index| match memory.types.get(index) {
            Some(kind) => grammar.u32(kind.flags).u32(kind.heap),
            None => grammar.u32(word(draw)).u32(word(draw)),
        },
    );
    (0..MAX_MEMORY_HEAPS)
        .fold(
            typed
                .u32(u32::try_from(memory.heaps.len()).unwrap_or(u32::MAX))
                .u64(count(MAX_MEMORY_HEAPS)),
            |grammar, index| match memory.heaps.get(index) {
                Some(heap) => grammar.u64(heap.size).u32(heap.flags),
                None => grammar.u64(draw()).u32(word(draw)),
            },
        )
        .0
}

/// Memory a device might state: plausible half the time, so a statement is
/// often narrowed, and anything at all otherwise.
fn drawn_memory(draw: &mut dyn FnMut() -> u64) -> HostMemory {
    let plausible = draw() & 1 == 0;
    let mut memory = HostMemory {
        types: tairix_inline::ArrayVec::new(),
        heaps: tairix_inline::ArrayVec::new(),
    };
    let heaps = 1 + below(draw, MAX_MEMORY_HEAPS);
    for _ in 0..heaps {
        let heap = if plausible {
            HostMemoryHeap {
                size: 1 << (20 + below(draw, 24)),
                flags: word(draw) & 1,
            }
        } else {
            HostMemoryHeap {
                size: draw(),
                flags: word(draw),
            }
        };
        assert!(memory.heaps.try_push(heap).is_ok());
    }
    for _ in 0..=below(draw, MAX_MEMORY_TYPES) {
        let kind = if plausible {
            HostMemoryType {
                flags: word(draw) & 0xf,
                heap: u32::try_from(below(draw, heaps)).unwrap_or(0),
            }
        } else {
            HostMemoryType {
                flags: word(draw),
                heap: word(draw),
            }
        };
        assert!(memory.types.try_push(kind).is_ok());
    }
    memory
}

/// Queue families a device might state: plausible half the time, a copy
/// engine's granularity among them whole levels, single texels, or blocks a
/// power of two along each axis, seldom anything else.
fn drawn_families(draw: &mut dyn FnMut() -> u64) -> Vec<HostQueueFamily> {
    let plausible = draw() & 1 == 0;
    (0..=below(draw, MAX_QUEUE_FAMILIES))
        .map(|_| {
            if plausible {
                let side = |draw: &mut dyn FnMut() -> u64| 1 << below(draw, 4);
                let granularity = match below(draw, 8) {
                    0 => [0; 3],
                    1 => [side(draw), side(draw), side(draw)],
                    2 => [3, 1, 1],
                    _ => [1; 3],
                };
                HostQueueFamily {
                    flags: word(draw) & 0xf,
                    queues: 1 + u32::try_from(below(draw, 4)).unwrap_or(0),
                    granularity,
                }
            } else {
                HostQueueFamily {
                    flags: word(draw),
                    queues: word(draw),
                    granularity: [word(draw), word(draw), word(draw)],
                }
            }
        })
        .collect()
}

/// A result alone, an object made, and command buffers allocated, each
/// answering `result`.
fn results(draw: &mut dyn FnMut() -> u64, made: &mut Vec<Vec<u8>>) {
    let result = signed(draw);
    let answered = VkResult::of(result);
    let id = draw();
    let creation = [
        Command::CreateInstance,
        Command::CreateDevice,
        Command::CreateCommandPool,
        Command::CreateBuffer,
        Command::AllocateMemory,
    ][below(draw, 5)];
    let bytes = Grammar::default()
        .i32(creation as i32)
        .i32(result)
        .u64(1)
        .u64(id)
        .0;
    held(draw, &bytes, &answered, |bytes| {
        Reader::new(bytes).made(creation, id)
    });
    made.push(bytes);

    let bytes = Grammar::default()
        .i32(Command::BindBufferMemory2 as i32)
        .i32(result)
        .0;
    held(draw, &bytes, &answered, |bytes| {
        Reader::new(bytes).result_of(Command::BindBufferMemory2)
    });
    made.push(bytes);

    let buffers: Vec<u64> = (0..=below(draw, 4)).map(|_| draw()).collect();
    let bytes = buffers
        .iter()
        .fold(
            Grammar::default()
                .i32(Command::AllocateCommandBuffers as i32)
                .i32(result)
                .u64(count(buffers.len())),
            |grammar, &buffer| grammar.u64(buffer),
        )
        .0;
    held(draw, &bytes, &answered, |bytes| {
        Reader::new(bytes).allocated(Command::AllocateCommandBuffers, &buffers)
    });
    made.push(bytes);
}

/// The physical devices an instance names, fewer than asked or counted.
fn physical_devices(draw: &mut dyn FnMut() -> u64, made: &mut Vec<Vec<u8>>) {
    let result = signed(draw);
    let named = below(draw, MAX_PHYSICAL_DEVICES + 1);
    let asked = named + below(draw, MAX_PHYSICAL_DEVICES - named + 1);
    let counted = u32::try_from(named)
        .unwrap_or(0)
        .saturating_add(word(draw) % 4);
    let devices: Vec<u64> = (0..named).map(|_| draw()).collect();
    let bytes = devices
        .iter()
        .fold(
            Grammar::default()
                .i32(Command::EnumeratePhysicalDevices as i32)
                .i32(result)
                .u64(1)
                .u32(counted)
                .u64(count(named)),
            |grammar, &device| grammar.u64(device),
        )
        .0;
    let expected = (VkResult::of(result), counted, devices);
    held(draw, &bytes, &expected, |bytes| {
        Reader::new(bytes)
            .physical_devices(asked)
            .map(|(result, counted, ids)| (result, counted, ids.to_vec()))
    });
    made.push(bytes);
}

/// A physical device's queue families, read back as drawn.
fn queue_families(draw: &mut dyn FnMut() -> u64, made: &mut Vec<Vec<u8>>) -> Vec<HostQueueFamily> {
    let families = drawn_families(draw);
    let counted = u32::try_from(families.len())
        .unwrap_or(0)
        .saturating_add(word(draw) % 4);
    let asked = families.len() + below(draw, MAX_QUEUE_FAMILIES - families.len() + 1);
    let bytes = families
        .iter()
        .fold(
            Grammar::default()
                .i32(Command::GetPhysicalDeviceQueueFamilyProperties2 as i32)
                .u64(1)
                .u32(counted)
                .u64(count(families.len())),
            |grammar, family| {
                let [width, height, depth] = family.granularity;
                grammar
                    .structure(Structure::QueueFamilyProperties2)
                    .u32(family.flags)
                    .u32(family.queues)
                    .words(1)
                    .u32(width)
                    .u32(height)
                    .u32(depth)
            },
        )
        .0;
    held(draw, &bytes, &(counted, families.clone()), |bytes| {
        Reader::new(bytes)
            .queue_families(asked)
            .map(|(counted, read)| (counted, read.to_vec()))
    });
    made.push(bytes);
    families
}

/// A physical device's memory and a buffer's requirements, read back as
/// drawn.
fn memory(draw: &mut dyn FnMut() -> u64, made: &mut Vec<Vec<u8>>) -> HostMemory {
    let memory = drawn_memory(draw);
    let bytes = memory_reply(draw, &memory);
    held(draw, &bytes, &memory, |bytes| {
        Reader::new(bytes).memory_properties()
    });
    made.push(bytes);

    let requirements = HostRequirements {
        size: draw(),
        alignment: draw(),
        type_bits: word(draw),
    };
    let bytes = Grammar::default()
        .i32(Command::GetBufferMemoryRequirements2 as i32)
        .u64(1)
        .structure(Structure::MemoryRequirements2)
        .u64(requirements.size)
        .u64(requirements.alignment)
        .u32(requirements.type_bits)
        .0;
    held(draw, &bytes, &requirements, |bytes| {
        Reader::new(bytes).memory_requirements(Command::GetBufferMemoryRequirements2)
    });
    made.push(bytes);
    memory
}

/// Every extension the driver asks for.
pub(crate) fn every_extension() -> HostExtensions {
    ASKED
        .iter()
        .fold(HostExtensions::NONE, |every, &(_, flag)| every.with(flag))
}

/// Extensions a device might offer: every one the driver asks for, three
/// times in four, so a statement is often narrowed.
fn drawn_extensions(draw: &mut dyn FnMut() -> u64) -> HostExtensions {
    if !draw().is_multiple_of(4) {
        return every_extension();
    }
    HostExtensions::from_bits(u16::try_from(draw() % (1 << ASKED.len())).unwrap_or(0))
}

/// The names of the extensions `offered` says a device offers, among one the
/// driver never asks for.
fn extension_names(offered: HostExtensions) -> Vec<&'static str> {
    core::iter::once("VK_KHR_swapchain")
        .chain(
            ASKED
                .iter()
                .filter(|&&(_, flag)| offered.contains(flag))
                .map(|&(name, _)| name),
        )
        .collect()
}

/// The granularity a device bounds robust accesses at: a power of two Vulkan
/// allows half the time, anything at all otherwise.
fn drawn_alignments(draw: &mut dyn FnMut() -> u64) -> RobustAlignments {
    if draw() & 1 == 0 {
        RobustAlignments {
            storage: 1 << below(draw, 9),
            uniform: 1 << below(draw, 9),
        }
    } else {
        RobustAlignments {
            storage: draw(),
            uniform: draw(),
        }
    }
}

/// A physical device's extensions, features and properties, a few of its
/// limits' words changed: each read back as written and refused cut short,
/// and narrowed with `memory` and `families` into a statement the seam
/// takes, or refused.
fn properties(
    draw: &mut dyn FnMut() -> u64,
    made: &mut Vec<Vec<u8>>,
    memory: &HostMemory,
    families: &[HostQueueFamily],
) {
    let extensions = drawn_extensions(draw);
    let names = extension_names(extensions);
    let bytes = extensions_reply(&names).0;
    let counted = u32::try_from(names.len()).unwrap_or(u32::MAX);
    held(
        draw,
        &bytes,
        &(VkResult::Success, counted, extensions),
        |bytes| Reader::new(bytes).extensions(names.len()),
    );
    made.push(bytes);

    let chain = feature_chain(API_VERSION, extensions);
    let plausible = draw() & 1 == 0;
    let mut bit = || plausible || draw() & 1 == 0;
    let mut chained = tairix_inline::ArrayVec::new();
    for &structure in &chain {
        let mut members = tairix_inline::ArrayVec::new();
        for _ in 0..chained_members(structure).unwrap_or(0) {
            assert!(members.try_push(bit()).is_ok());
        }
        assert!(chained.try_push(members).is_ok());
    }
    let mut core = [false; CORE_FEATURES];
    for member in &mut core {
        *member = bit();
    }
    let features = HostFeatures { core, chained };
    let bytes = features_reply(&chain, &features).0;
    held(draw, &bytes, &features, |bytes| {
        Reader::new(bytes).features(&chain)
    });
    made.push(bytes);

    let mut formats = [HostFormatFeatures::default(); Format::ALL.len()];
    for offered in &mut formats {
        let stated = draw() & 1 == 0;
        let mut features = || {
            let bits = draw();
            if stated {
                bits
            } else {
                bits & OLDER_FORMAT_FEATURES
            }
        };
        *offered = HostFormatFeatures {
            optimal: features(),
            buffer: features(),
        };
        let bytes = format_reply(*offered, stated).0;
        held(draw, &bytes, offered, |bytes| {
            Reader::new(bytes).format_properties(stated)
        });
        made.push(bytes);
    }

    // What the host makes of each format's widest image of four samples,
    // where it was asked.
    let multisampled: [Option<HostImageLimits>; Format::ALL.len()] = core::array::from_fn(|_| {
        draw().is_multiple_of(2).then(|| HostImageLimits {
            extent: [word(draw), word(draw), 1],
            levels: 1,
            layers: word(draw),
            samples: u32::try_from(draw() & 0x7f).unwrap_or(0),
        })
    });

    let robustness = extensions.intersects(HostExtensions::ROBUSTNESS2);
    let alignments = drawn_alignments(draw);
    let robust = robustness.then_some(alignments);
    let mut properties = properties_reply(robust, API_VERSION, limits).0;
    // The limits lie between the header and the five words that end it.
    let header = properties_reply(robust, API_VERSION, |grammar| grammar)
        .0
        .len()
        - 20;
    let limits_words = (properties.len() - 20 - header) / 4;
    for _ in 0..below(draw, 4) {
        let at = header + 4 * below(draw, limits_words);
        properties[at..at + 4].copy_from_slice(&word(draw).to_le_bytes());
    }
    if let Ok(read) = Reader::new(&properties).properties(robustness) {
        assert_eq!(read.robust_alignments, robust);
        let host = HostDevice {
            properties: &read,
            extensions,
            features: &features,
            chain: &chain,
            memory,
            families,
            formats: &formats,
            multisampled: &multisampled,
        };
        if let Ok(statement) = narrow(&host) {
            check_statement(&statement, memory, families);
        }
    }
    made.push(properties);
}

/// A narrowed statement names the host's own memory types and families, each
/// family's queues within the context's rings, and the seam takes it.
fn check_statement(
    statement: &super::info::Statement,
    memory: &HostMemory,
    families: &[HostQueueFamily],
) {
    assert_eq!(
        statement.memory_types.len(),
        statement.info.memory_types.len()
    );
    assert!(statement
        .memory_types
        .iter()
        .all(|&host| usize::try_from(host).is_ok_and(|host| host < memory.types.len())));
    assert_eq!(
        statement.families.len(),
        statement.info.queue_families.len()
    );
    assert!(statement
        .families
        .iter()
        .all(|&host| usize::try_from(host).is_ok_and(|host| host < families.len())));
    let queues: u32 = statement
        .info
        .queue_families
        .iter()
        .map(|family| family.queues)
        .sum();
    assert!(queues <= super::MAX_QUEUE_RINGS, "{statement:?}");
    assert!(
        Device::hardware(
            Idle,
            statement.info.clone(),
            &statement.info.engines(1),
            Arc::new(Still)
        )
        .is_ok(),
        "a statement the seam refuses: {statement:?}"
    );
}

/// Replies of every kind the driver reads, their values drawn from `draw`:
/// each read back as written and refused cut short, and the statement a
/// physical device's replies make, where narrowed, one the seam takes. The
/// replies are answered for the harness to change.
///
/// # Panics
///
/// Where a reply is not read as written, a reply cut short is read, or a
/// statement narrowed is one the seam refuses.
pub fn replies(draw: &mut dyn FnMut() -> u64) -> Vec<Vec<u8>> {
    let mut made = Vec::new();
    results(draw, &mut made);
    physical_devices(draw, &mut made);
    let families = queue_families(draw, &mut made);
    let memory = memory(draw, &mut made);
    properties(draw, &mut made, &memory, &families);
    image_formats(draw, &mut made);
    made
}

/// The most an image may be, made or refused: read back as written, and
/// refused cut short or as done in part.
fn image_formats(draw: &mut dyn FnMut() -> u64, made: &mut Vec<Vec<u8>>) {
    let most = HostImageLimits {
        extent: [word(draw), word(draw), word(draw)],
        levels: word(draw),
        layers: word(draw),
        samples: word(draw),
    };
    let result = match below(draw, 4) {
        0 => 0,
        // Host or device memory exhausted.
        1 => -1 - i32::try_from(below(draw, 2)).unwrap_or(0),
        2 => -3 - i32::try_from(below(draw, 1 << 10)).unwrap_or(0),
        _ => 1 + i32::try_from(below(draw, 1 << 10)).unwrap_or(0),
    };
    let bytes = image_format_reply(result, most).0;
    let read = Reader::new(&bytes).image_format_properties();
    match result {
        0 => held(draw, &bytes, &Some(most), |bytes| {
            Reader::new(bytes).image_format_properties()
        }),
        -2 | -1 => assert_eq!(
            read,
            Err(DriverError::OutOfMemory),
            "out of memory, the image perhaps made later"
        ),
        failed if failed < 0 => assert_eq!(read, Ok(None), "refused, its properties unread"),
        _ => assert!(read.is_err(), "done in part, which no query answers"),
    }
    made.push(bytes);
}
