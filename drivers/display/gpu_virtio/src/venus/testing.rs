//! What the crate's Venus tests and its fuzz harness
//! (`tests/fuzz_venus_replies.rs`) build replies with, and the rounds the
//! harness runs over the reader. Built only for tests, behind the crate's
//! `test-support` feature.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Debug;

use tairix_abi::time::MonotonicClock;
use tairix_abi::DriverError;
use tairix_gpu::{Commands, Device, Error, Hardware, Job};

use super::info::narrow;
use super::reply::{
    HostMemory, HostMemoryHeap, HostMemoryType, HostQueueFamily, HostRequirements, Reader,
    MAX_PHYSICAL_DEVICES, MAX_QUEUE_FAMILIES,
};
use super::{
    Command, Structure, VkResult, API_VERSION, MAX_MEMORY_HEAPS, MAX_MEMORY_TYPES,
    PHYSICAL_DEVICE_NAME_LEN, UUID_LEN,
};

/// A stream written item by item.
#[derive(Default)]
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

    /// A structure's type and its empty chain.
    pub(crate) fn structure(self, structure: Structure) -> Self {
        self.i32(structure as i32).u64(0)
    }

    pub(crate) fn words(self, count: usize) -> Self {
        (0..count).fold(self, |grammar, _| grammar.u32(0))
    }
}

/// A reply of [`Command::GetPhysicalDeviceProperties2`]: the limits all
/// zero but those set by `limits`, given each member's offset in words from
/// the limits' start.
pub(crate) fn properties_reply(limits: impl FnOnce(Grammar) -> Grammar) -> Grammar {
    let mut name = [0u8; PHYSICAL_DEVICE_NAME_LEN];
    name[..8].copy_from_slice(b"llvmpipe");
    let header = Grammar::default()
        .i32(Command::GetPhysicalDeviceProperties2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceProperties2 as i32)
        .u64(1)
        .i32(Structure::PhysicalDeviceSubgroupProperties as i32)
        .u64(1)
        .structure(Structure::PhysicalDeviceMaintenance3Properties)
        .u32(1024)
        .u64(1 << 31)
        .u32(32)
        .u32(0x3f)
        .u32(0xff)
        .u32(1)
        .u32(API_VERSION)
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
/// test reads back.
pub(crate) fn limits(grammar: Grammar) -> Grammar {
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
        .words(14)
        .u32(8)
        .words(12)
        .u64(2)
        .f32(1.0)
        .f32(255.0)
        .u64(2)
        .f32(1.0)
        .f32(8.0)
        .words(4)
        .u64(1)
        .u64(1)
        .u64(64)
}

/// A backend that runs nothing, so a statement alone is judged.
pub(crate) struct Idle;

impl Hardware for Idle {
    type Context = ();
    type Memory = ();

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

    fn run(&self, job: Job<Self>) {
        job.finish(Ok(()));
    }

    fn run_bytes(_: Commands<'_, Self>) -> u64 {
        0
    }

    fn lost(&self, _: &Arc<()>) {}
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
    let _ = Reader::new(bytes).properties();
    let _ = Reader::new(bytes).queue_families(MAX_QUEUE_FAMILIES);
    let _ = Reader::new(bytes).memory_properties();
    let _ = Reader::new(bytes).memory_requirements();
    let _ = Reader::new(bytes).command_buffers(&ids);
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

/// Queue families a device might state: plausible half the time.
fn drawn_families(draw: &mut dyn FnMut() -> u64) -> Vec<HostQueueFamily> {
    let plausible = draw() & 1 == 0;
    (0..=below(draw, MAX_QUEUE_FAMILIES))
        .map(|_| {
            if plausible {
                HostQueueFamily {
                    flags: word(draw) & 0x7,
                    queues: 1 + u32::try_from(below(draw, 4)).unwrap_or(0),
                }
            } else {
                HostQueueFamily {
                    flags: word(draw),
                    queues: word(draw),
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
        Reader::new(bytes).command_buffers(&buffers)
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
                grammar
                    .structure(Structure::QueueFamilyProperties2)
                    .u32(family.flags)
                    .u32(family.queues)
                    .words(4)
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
        Reader::new(bytes).memory_requirements()
    });
    made.push(bytes);
    memory
}

/// A physical device's properties, a few of its limits' words changed: read
/// or refused, and narrowed with `memory` and `families` into a statement the
/// seam takes, or refused.
fn properties(
    draw: &mut dyn FnMut() -> u64,
    made: &mut Vec<Vec<u8>>,
    memory: &HostMemory,
    families: &[HostQueueFamily],
) {
    let mut properties = properties_reply(limits).0;
    let header = properties_reply(|grammar| grammar).0.len() - 20;
    let limits_words = (properties.len() - 20 - header) / 4;
    for _ in 0..below(draw, 4) {
        let at = header + 4 * below(draw, limits_words);
        properties[at..at + 4].copy_from_slice(&word(draw).to_le_bytes());
    }
    if let Ok(read) = Reader::new(&properties).properties() {
        if let Ok(statement) = narrow(&read, memory, families) {
            assert_eq!(
                statement.memory_types.len(),
                statement.info.memory_types.len()
            );
            assert!(statement
                .memory_types
                .iter()
                .all(|&host| usize::try_from(host).is_ok_and(|host| host < memory.types.len())));
            assert!(usize::try_from(statement.family).is_ok_and(|family| family < families.len()));
            assert!(
                Device::hardware(Idle, statement.info.clone(), &[1], Arc::new(Still)).is_ok(),
                "a statement the seam refuses: {statement:?}"
            );
        }
    }
    made.push(properties);
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
    made
}
