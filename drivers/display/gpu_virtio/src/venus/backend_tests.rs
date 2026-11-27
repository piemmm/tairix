//! The Venus backend held to a host of the test's own: a display end that
//! carries out the desk's orders on a device it keeps, and a renderer that
//! decodes every stream the backend sends, keeps the objects it names, runs
//! its submissions on its memory, and writes its replies — failing a context
//! fatally, as the real renderer does, on any command off its grammar, and on
//! two commands reaching one buffer's bytes, either writing, with no barrier
//! between them to order them as the renderer runs them in turn. The backend
//! is driven only through the GPU seam.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar as StdCondvar, Mutex as StdMutex};
use std::thread::JoinHandle;

use tairix_abi::driver::RegionCaching;
use tairix_abi::hwtree::HwResource;
use tairix_abi::time::MonotonicClock;
use tairix_abi::{DriverError, Duration64, PAGE_SIZE};
use tairix_collections::SlotMap;
use tairix_gpu::{
    AllocationDesc, BindSet, BindSetEntry, BindSetLayout, BufferDesc, BufferUsage,
    ComputePipelineDesc, ContextDesc, CopyRegion, Device, Error, Granularity, LayoutBinding, Loss,
    MemoryProperties, NoThreads, PushConstants, QueueCapabilities, QueueFamily, Resource,
    SoftwareDesc, Spawn, Submission, DEFAULT_SUBGROUP_SIZE, MAX_UPDATE_BYTES,
};
use tairix_rt::sync::Mutex;
use tairix_shaders::compose::fill;
use tairix_spirv::{Admitted, Module, Op, Stage, Stages, Version};
use tairix_virtio::{MockRegion, Part, SharedMemory};

use self::host_images::{HostImage, HostTexels, ImageCommand, Mirrored, Reply};
use super::{
    bind_ids, bring_up, check_counts, device, finish_commands, lose, make_context, payload, ring,
    run_check, stream, submit_and_read, utility, Admits, Area, Channel, ContextState, Making, Then,
    DEVICE, EXCHANGE_LEN, FEEDBACK, FEEDBACK_SLOTS, FIRST_QUEUE, LONGEST_COMMAND, POOL,
    RECORDS_CHECK, SHARED_SPEC,
};
use crate::three_d::{Blob, Context};
use crate::venus::desk::{Answer, Desk, Order, Rouse};
use crate::venus::encode::{Encoder, LayoutEntry, SamplerInfo, ViewInfo, Written};
use crate::venus::reply::{
    chained_members, HostFeatures, HostFormatFeatures, RobustAlignments, CHAINED,
};
use crate::venus::testing::{features_reply, lined_limits, properties_reply, Grammar, Still};
use crate::venus::{
    api_version, descriptor, extension, queue, stage, Command, Structure, API_VERSION,
    BIND_POINT_COMPUTE, BIND_POINT_GRAPHICS, CORE_FEATURES, EXTENSION_NAME_LEN, MAX_MEMORY_HEAPS,
    MAX_MEMORY_TYPES,
};

#[path = "backend_tests/host_images.rs"]
mod host_images;
#[path = "backend_tests/host_render.rs"]
mod host_render;
#[path = "backend_tests/image_tests.rs"]
mod image_tests;
#[path = "backend_tests/render_tests.rs"]
mod render_tests;

const REGION_BYTES: usize = 16 << 20;
const SECOND: Duration64 = Duration64::from_secs(1);

/// What the renderer reads a stream with: `None` off the grammar.
struct Words<'a>(&'a [u8]);

impl Words<'_> {
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let (taken, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(u32::cast_signed)
    }

    fn u64(&mut self) -> Option<u64> {
        let bytes = self.take(8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }

    /// A word that must be `want`.
    fn is(&mut self, want: u64) -> Option<()> {
        (self.u64()? == want).then_some(())
    }

    fn structure(&mut self, structure: Structure) -> Option<()> {
        (self.i32()? == structure as i32).then_some(())?;
        self.is(0)
    }

    /// A word that must be `want`.
    fn word(&mut self, want: u32) -> Option<()> {
        (self.u32()? == want).then_some(())
    }

    /// A Boolean, which must be one or nought.
    fn boolean(&mut self) -> Option<bool> {
        match self.u32()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// A string: its length with its terminator, then its bytes and the
    /// terminator, padded to whole words.
    fn string(&mut self) -> Option<String> {
        let len = usize::try_from(self.u64()?).ok()?;
        let bytes = self.take(len.checked_next_multiple_of(4)?)?;
        let (&end, text) = bytes.get(..len)?.split_last()?;
        (end == 0 && !text.contains(&0)).then_some(())?;
        String::from_utf8(text.to_vec()).ok()
    }

    /// The types of a chain of structures, outermost first, up to its end.
    fn chain(&mut self) -> Option<Vec<i32>> {
        let mut chain = Vec::new();
        loop {
            match self.u64()? {
                0 => return Some(chain),
                1 if chain.len() < 8 => chain.push(self.i32()?),
                _ => return None,
            }
        }
    }

    /// An array of `count` handles: its size, then each.
    fn handles(&mut self, count: u32) -> Option<Vec<u64>> {
        self.is(u64::from(count))?;
        (0..count).map(|_| self.u64()).collect()
    }
}

/// A compute pipeline as the renderer keeps it: the module it runs, its
/// entry point, and the layout its sets and push constants are bound with.
#[derive(Clone)]
struct Program {
    module: Arc<Admitted>,
    entry: String,
    sets: Vec<Vec<LayoutEntry>>,
    /// Its push constants' stages and bytes.
    push: (u32, u32),
}

/// What a command buffer has bound as it runs.
#[derive(Default)]
struct Bound {
    program: Option<Program>,
    sets: Vec<u64>,
    push: Vec<u8>,
}

/// The extension structures of the features a device is asked for or made
/// with.
/// The granularity the host bounds robust accesses at.
const ROBUST: RobustAlignments = RobustAlignments {
    storage: 4,
    uniform: 16,
};

/// The most workgroups the host's statement makes along each axis.
const MAX_GROUPS: u32 = 65535;

/// What the host's physical device offers past what it always states.
struct Offered {
    extensions: Vec<String>,
    /// What each format offers, by the host's name: nothing for one absent.
    formats: BTreeMap<i32, HostFormatFeatures>,
    /// The widest image side it makes.
    image_extent: u32,
    /// The version of Vulkan it states.
    api_version: u32,
    /// What its first queue family runs.
    family_flags: u32,
    /// A second family running transfers alone, a copy engine: its queues
    /// and the least its image copies move.
    copy_engine: Option<(u32, [u32; 3])>,
    /// The formats, by the host's name, it makes no image of able to be seen
    /// as a cube.
    uncubed: Vec<i32>,
    /// Whether its own lines are rectangles, so no pipeline asks for them.
    strict_lines: bool,
    /// The core features it lacks, by their place.
    lacks: Vec<usize>,
    /// The members of the chained feature structures it lacks: each
    /// structure and the member's place.
    withheld: Vec<(Structure, usize)>,
}

impl Default for Offered {
    /// Every extension the driver asks for, among more than the reply area
    /// holds, so their names come back through a blob of their own.
    fn default() -> Self {
        let mut extensions: Vec<String> = [
            extension::ROBUSTNESS2_KHR,
            extension::ROBUSTNESS2_EXT,
            extension::ZERO_INITIALIZE_WORKGROUP_MEMORY,
            extension::SHADER_FLOAT16_INT8,
            extension::SAMPLER_MIRROR_CLAMP_TO_EDGE,
            extension::DEPTH_RANGE_UNRESTRICTED,
            extension::FORMAT_FEATURE_FLAGS2,
            extension::MAINTENANCE5,
            extension::LINE_RASTERIZATION_KHR,
        ]
        .into_iter()
        .map(String::from)
        .collect();
        extensions.extend((0..64).map(|n| format!("VK_TEST_padding_{n}")));
        Self {
            extensions,
            formats: host_images::offered_formats(),
            image_extent: 16384,
            api_version: API_VERSION,
            family_flags: queue::GRAPHICS | queue::COMPUTE | queue::TRANSFER,
            copy_engine: None,
            uncubed: Vec::new(),
            strict_lines: true,
            lacks: Vec::new(),
            withheld: Vec::new(),
        }
    }
}

impl Offered {
    fn offers(&self, name: &str) -> bool {
        self.extensions.iter().any(|offered| offered == name)
    }

    /// Whether a chained structure of type `raw` is one the device may be
    /// asked for or made with: an extension's only where the extension is
    /// offered, the rest being core at the host's version.
    fn chains(&self, raw: i32) -> Option<Structure> {
        let structure = CHAINED
            .into_iter()
            .find(|&structure| structure as i32 == raw)?;
        let offered = match structure {
            Structure::PhysicalDeviceRobustness2Features => {
                self.offers(extension::ROBUSTNESS2_KHR) || self.offers(extension::ROBUSTNESS2_EXT)
            }
            Structure::PhysicalDeviceDynamicRenderingFeatures => self.api_version >= api_version(3),
            Structure::PhysicalDeviceMaintenance5Features => self.offers(extension::MAINTENANCE5),
            Structure::PhysicalDeviceLineRasterizationFeatures => {
                self.offers(extension::LINE_RASTERIZATION_KHR)
                    || self.offers(extension::LINE_RASTERIZATION_EXT)
            }
            _ => true,
        };
        offered.then_some(structure)
    }

    /// Whether member `at` of the chained structure `structure` is offered.
    fn feature(&self, structure: Structure, at: usize) -> bool {
        !self.withheld.contains(&(structure, at))
    }
}

/// What a device was made with.
struct Enablement {
    extensions: Vec<String>,
    /// Each structure chained, by type, and its members: the core features'
    /// first.
    features: Vec<(i32, Vec<bool>)>,
}

/// A command a command buffer recorded.
enum Recorded {
    Copy(u64, u64, Vec<(u64, u64, u64)>),
    Fill(u64, u64, u64, u32),
    Update(u64, u64, Vec<u8>),
    BindPipeline(u64),
    BindGraphics(u64),
    /// Sets bound at the bind point named.
    BindSets(u32, Vec<u64>),
    /// Push constants for the stages named.
    Push(u32, Vec<u8>),
    Dispatch([u32; 3]),
    DispatchIndirect(u64, u64),
    Image(ImageCommand),
    BeginRendering(Box<host_render::Rendering>),
    EndRendering,
    VertexBuffers(Vec<[u64; 3]>),
    Index([u64; 3], bool),
    Viewport([f32; 6]),
    Scissor([i64; 4]),
    StencilReference(u32),
    BlendConstants([f32; 4]),
    Draw(host_render::DrawRecord),
    /// A memory barrier: what came before it ordered before what follows.
    Barrier,
}

/// Bytes `start..end` of memory `memory` a command reads, or writes.
#[derive(Copy, Clone)]
struct Access {
    memory: u64,
    start: u64,
    end: u64,
    write: bool,
}

impl Access {
    /// Whether it and `other` race where no barrier orders them: they share
    /// a byte, and one writes it.
    const fn races(self, other: Self) -> bool {
        self.memory == other.memory
            && self.start < other.end
            && other.start < self.end
            && (self.write || other.write)
    }
}

/// What the commands since the last barrier reached.
#[derive(Default)]
struct Unordered(Vec<Access>);

impl Unordered {
    /// Note what a command reached, refused where it races what one before
    /// it reached.
    fn note(&mut self, reached: Vec<Access>) -> Option<()> {
        let races = reached
            .iter()
            .any(|access| self.0.iter().any(|held| held.races(*access)));
        (!races).then_some(())?;
        self.0.extend(reached);
        Some(())
    }
}

impl Recorded {
    /// Whether it may lie within a rendering instance: a draw, the state
    /// draws read, or the instance's end.
    const fn within_rendering(&self) -> bool {
        matches!(
            self,
            Self::BindGraphics(_)
                | Self::BindSets(BIND_POINT_GRAPHICS, _)
                | Self::Push(..)
                | Self::VertexBuffers(_)
                | Self::Index(..)
                | Self::Viewport(_)
                | Self::Scissor(_)
                | Self::StencilReference(_)
                | Self::BlendConstants(_)
                | Self::Draw(_)
                | Self::EndRendering
        )
    }
}

enum Object {
    Instance,
    Physical,
    Device,
    Queue,
    Pool,
    Commands(Vec<Recorded>),
    Buffer {
        size: u64,
        memory: Option<u64>,
    },
    Memory {
        bytes: Vec<u8>,
    },
    Module(Arc<Admitted>),
    SetLayout(Vec<LayoutEntry>),
    PipelineLayout {
        sets: Vec<Vec<LayoutEntry>>,
        push: (u32, u32),
    },
    Pipeline(Program),
    Graphics(host_render::Graphics),
    DescriptorPool {
        /// Descriptors it has room for, by type.
        room: BTreeMap<u32, u32>,
        sets: Vec<u64>,
    },
    Set {
        layout: Vec<LayoutEntry>,
        /// What each binding was written with.
        written: BTreeMap<u32, Written>,
    },
    Image(HostImage),
    View(ViewInfo),
    Sampler(SamplerInfo),
    Texels(HostTexels),
}

/// The queues of the host's first family.
const FIRST_QUEUES: u32 = 2;

/// The sharing a buffer or an image is made with, whether concurrent: by one
/// family at a time, or by every one of a device's several `families`.
fn shared(words: &mut Words<'_>, families: &[u32]) -> Option<bool> {
    let mode = words.i32()?;
    let named = words.u32()?;
    if mode == 0 {
        (named == 0).then_some(())?;
        words.is(0)?;
        return Some(false);
    }
    (mode == 1 && families.len() > 1 && named as usize == families.len()).then_some(())?;
    words.is(u64::from(named))?;
    families.iter().try_for_each(|&family| words.word(family))?;
    Some(true)
}

struct HostContext {
    reply: Option<(u32, u64, u64)>,
    reply_at: u64,
    objects: BTreeMap<u64, Object>,
    dead: bool,
    /// The families the device was made with.
    families: Vec<u32>,
    /// The family each queue, command pool and command buffer is of.
    family_of: BTreeMap<u64, u32>,
    /// The rings its queues are bound to, one queue each.
    rings: Vec<u32>,
}

struct HostBlob {
    context: Context,
    blob_id: u64,
    size: u64,
    /// Where it is mapped in the region.
    at: Option<u64>,
    /// Its bytes while unmapped, a context's own memory's held by the memory.
    own: Vec<u8>,
}

/// The ways the host is made to misbehave.
#[derive(Copy, Clone, Default, Eq, PartialEq)]
enum Misbehaviour {
    #[default]
    Nothing,
    /// Submissions refused.
    RefuseSubmits,
    /// Submissions answered without being run, as a render server answers
    /// the fences of a worker that died.
    IgnoreWork,
    /// Unmaps refused once the driver's window is gone, the blob left mapped.
    RefuseUnmaps,
    /// Maps carried out but answered with what cannot be read, as an answer
    /// without its fence is: the blob may be mapped, and its part is kept.
    DoubtMaps,
    /// Pipelines refused for want of host memory.
    RefusePipelines,
    /// Sets refused for want of host memory.
    RefuseSets,
    /// Views made but answered with what cannot be read, as a renderer that
    /// faulted answers.
    GarbleViews,
    /// Samplers reading their border as integers refused for want of host
    /// memory.
    RefuseIntegerBorders,
}

tairix_util::flags! {
    /// What reached the host that the driver must never send it.
    struct Unsent(u8) {
        /// Memory freed while a blob still made it host memory of the device.
        const FREED_EXPORTED = 1;
        /// A record naming a first instance, on a device that draws none.
        const FIRST_INSTANCED = 1 << 1;
        /// A dispatch past the device's counts.
        const OVERDISPATCHED = 1 << 2;
    }
}

#[derive(Default)]
struct HostState {
    contexts: SlotMap<Context, HostContext>,
    blobs: SlotMap<Blob, HostBlob>,
    /// How the host departs from a well-behaved one, if it does.
    misbehaves: Misbehaviour,
    unmaps_refused: usize,
    offered: Offered,
    /// What the device was last made with.
    enabled: Option<Enablement>,
    /// Dispatches the device ran.
    dispatched: usize,
    /// Rendering instances the device ran.
    rendered: usize,
    /// What reached it that the driver must never send it.
    reached: Unsent,
    /// Submissions ordered.
    submits: usize,
    /// Images whose memory requirements were asked of their description.
    described: usize,
    /// Each image format query: its format, kind, uses, flags and listed
    /// formats.
    queried: Vec<(i32, i32, u32, u32, Vec<i32>)>,
    /// Orders are taken but not carried out until the host resumes.
    paused: bool,
    /// Orders taken while paused.
    held: usize,
    /// A ring whose submissions are carried out but answered only once it is
    /// let go, as a device's work runs before its fence is seen to retire.
    withheld: Option<u8>,
    /// The answers it keeps back.
    kept: usize,
    /// How many submissions go by before one is refused, unrun, for want of
    /// memory.
    refuse_submit_in: Option<usize>,
    /// Orders carried out while a thread held the shared memory the display
    /// end maps parts through.
    served_while_shared_held: usize,
}

/// The host: its device, its renderer, and the region its blobs map into.
struct Host {
    region: Arc<MockRegion>,
    shared: Arc<Mutex<SharedMemory>>,
    state: StdMutex<HostState>,
    /// Told each time an order has been carried out.
    changed: StdCondvar,
    /// What its dispatches run on.
    reference: Device,
}

impl Host {
    /// Carry out the orders `held` in order, answering each but a withheld
    /// ring's, whose answers join `kept` until the ring is let go.
    fn serve(
        &self,
        desk: &Desk<Then>,
        held: &mut VecDeque<(u64, Order)>,
        kept: &mut VecDeque<(u64, Answer)>,
    ) {
        while let Some((tag, order)) = held.pop_front() {
            let withheld = self.state.lock().expect("unpoisoned").withheld;
            let keep = matches!(order, Order::Submit { ring, .. } if Some(ring) == withheld);
            let answer = self.carry_out(order);
            if keep {
                kept.push_back((tag, answer));
                self.state.lock().expect("unpoisoned").kept = kept.len();
            } else {
                desk.answer(tag, answer);
            }
            self.changed.notify_all();
        }
        let mut state = self.state.lock().expect("unpoisoned");
        if state.withheld.is_none() {
            state.kept = 0;
            drop(state);
            while let Some((tag, answer)) = kept.pop_front() {
                desk.answer(tag, answer);
            }
        }
    }

    fn carry_out(&self, order: Order) -> Answer {
        let shared_held = self.shared.try_lock().is_none();
        let mut state = self.state.lock().expect("unpoisoned");
        state.served_while_shared_held += usize::from(shared_held);
        match order {
            Order::MakeContext => {
                let context = HostContext {
                    reply: None,
                    reply_at: 0,
                    objects: BTreeMap::new(),
                    dead: false,
                    families: Vec::new(),
                    family_of: BTreeMap::new(),
                    rings: Vec::new(),
                };
                Answer::Context(state.contexts.try_insert(context).expect("room"))
            }
            Order::EndContext(context) => match state.contexts.remove(context) {
                Some(_) => Answer::Done,
                None => Answer::Refused(tairix_abi::DriverError::NotFound, None),
            },
            Order::MakeBlob(context, spec) => {
                let fits = spec.blob_id == 0
                    || state.contexts.get(context).is_some_and(|host| {
                        matches!(host.objects.get(&spec.blob_id),
                            Some(Object::Memory { bytes }) if bytes.len() as u64 == spec.size)
                    });
                if !fits {
                    return Answer::Refused(tairix_abi::DriverError::OutOfRange, None);
                }
                let blob = HostBlob {
                    context,
                    blob_id: spec.blob_id,
                    size: spec.size,
                    at: None,
                    own: vec![0; usize::try_from(spec.size).expect("small")],
                };
                Answer::Blob(state.blobs.try_insert(blob).expect("room"))
            }
            Order::EndBlob(blob) => match state.blobs.get(blob) {
                Some(held) if held.at.is_none() => {
                    state.blobs.remove(blob);
                    Answer::Done
                }
                _ => Answer::Refused(tairix_abi::DriverError::Busy, None),
            },
            Order::MapBlob(blob, part) => self.map(&mut state, blob, part),
            Order::UnmapBlob(blob, part) => self.unmap(&mut state, blob, part),
            Order::Submit {
                context, payload, ..
            } => {
                state.submits += 1;
                if state.misbehaves == Misbehaviour::RefuseSubmits {
                    return Answer::Refused(tairix_abi::DriverError::DeviceFault, None);
                }
                match state.refuse_submit_in {
                    Some(0) => {
                        state.refuse_submit_in = None;
                        return Answer::Refused(tairix_abi::DriverError::OutOfMemory, None);
                    }
                    Some(left) => state.refuse_submit_in = Some(left - 1),
                    None => {}
                }
                if state.misbehaves != Misbehaviour::IgnoreWork {
                    self.run_payload(&mut state, context, &payload);
                }
                Answer::Done
            }
        }
    }

    /// Map `blob` at `part`, as the display end and the device together do.
    fn map(&self, state: &mut HostState, blob: Blob, mut part: Part) -> Answer {
        let at = part.offset();
        let Some(bytes) = Self::blob_bytes(state, blob) else {
            return Answer::Refused(tairix_abi::DriverError::NotFound, Some(part));
        };
        self.region
            .write(usize::try_from(at).expect("small"), &bytes)
            .expect("in the region");
        if let Some(held) = state.blobs.get_mut(blob) {
            held.at = Some(at);
        }
        if state.misbehaves == Misbehaviour::DoubtMaps {
            return Answer::Refused(tairix_abi::DriverError::DeviceFault, None);
        }
        let _ = self
            .shared
            .lock()
            .map(&mut part, self.region.as_ref(), RegionCaching::WriteBack);
        Answer::Mapped(part)
    }

    /// Unmap `blob` from `part`, the driver's window first.
    fn unmap(&self, state: &mut HostState, blob: Blob, mut part: Part) -> Answer {
        if self
            .shared
            .lock()
            .unmap(&mut part, self.region.as_ref())
            .is_err()
        {
            return Answer::Refused(tairix_abi::DriverError::DeviceFault, Some(part));
        }
        if state.misbehaves == Misbehaviour::RefuseUnmaps {
            // Still mapped, so the part is not lent back.
            state.unmaps_refused += 1;
            return Answer::Refused(tairix_abi::DriverError::DeviceFault, None);
        }
        let Some(held) = state.blobs.get(blob) else {
            return Answer::Refused(tairix_abi::DriverError::NotFound, Some(part));
        };
        let (Some(at), size) = (held.at, held.size) else {
            return Answer::Refused(tairix_abi::DriverError::NotFound, Some(part));
        };
        let mut bytes = vec![0; usize::try_from(size).expect("small")];
        self.region
            .read(usize::try_from(at).expect("small"), &mut bytes)
            .expect("in the region");
        Self::put_blob_bytes(state, blob, bytes);
        if let Some(held) = state.blobs.get_mut(blob) {
            held.at = None;
        }
        Answer::Unmapped(part)
    }

    /// A blob's bytes, wherever they are held.
    fn blob_bytes(state: &HostState, blob: Blob) -> Option<Vec<u8>> {
        let held = state.blobs.get(blob)?;
        if held.blob_id == 0 {
            return Some(held.own.clone());
        }
        match state
            .contexts
            .get(held.context)?
            .objects
            .get(&held.blob_id)?
        {
            Object::Memory { bytes } => Some(bytes.clone()),
            _ => None,
        }
    }

    fn put_blob_bytes(state: &mut HostState, blob: Blob, bytes: Vec<u8>) {
        let Some(held) = state.blobs.get_mut(blob) else {
            return;
        };
        if held.blob_id == 0 {
            held.own = bytes;
            return;
        }
        let (context, id) = (held.context, held.blob_id);
        if let Some(Object::Memory { bytes: held }) = state
            .contexts
            .get_mut(context)
            .and_then(|host| host.objects.get_mut(&id))
        {
            *held = bytes;
        }
    }

    /// Where memory `id` of `context` lies in the region, where its blob is
    /// mapped.
    fn mapped_at(state: &HostState, context: Context, id: u64) -> Option<u64> {
        state
            .blobs
            .iter()
            .find(|(_, held)| held.context == context && held.blob_id == id)
            .and_then(|(_, held)| held.at)
    }

    fn read_memory(&self, state: &HostState, context: Context, id: u64, at: u64, out: &mut [u8]) {
        if let Some(base) = Self::mapped_at(state, context, id) {
            self.region
                .read(usize::try_from(base + at).expect("small"), out)
                .expect("in the region");
        } else if let Some(Object::Memory { bytes }) = state
            .contexts
            .get(context)
            .and_then(|host| host.objects.get(&id))
        {
            let start = usize::try_from(at).expect("small");
            out.copy_from_slice(&bytes[start..start + out.len()]);
        }
    }

    fn write_memory(&self, state: &mut HostState, context: Context, id: u64, at: u64, data: &[u8]) {
        if let Some(base) = Self::mapped_at(state, context, id) {
            self.region
                .write(usize::try_from(base + at).expect("small"), data)
                .expect("in the region");
        } else if let Some(Object::Memory { bytes }) = state
            .contexts
            .get_mut(context)
            .and_then(|host| host.objects.get_mut(&id))
        {
            let start = usize::try_from(at).expect("small");
            bytes[start..start + data.len()].copy_from_slice(data);
        }
    }

    /// The commands an order carried: the reply stream set and sought, and
    /// streams run where they lie.
    fn run_payload(&self, state: &mut HostState, context: Context, payload: &[u8]) {
        let mut words = Words(payload);
        while !words.0.is_empty() {
            let ran = (|| {
                let command = words.i32()?;
                words.u32()?;
                match command {
                    c if c == Command::SetReplyCommandStreamMesa as i32 => {
                        words.is(1)?;
                        let span = (words.u32()?, words.u64()?, words.u64()?);
                        let host = state.contexts.get_mut(context)?;
                        host.reply = Some(span);
                        host.reply_at = 0;
                    }
                    c if c == Command::SeekReplyCommandStreamMesa as i32 => {
                        let position = words.u64()?;
                        state.contexts.get_mut(context)?.reply_at = position;
                    }
                    c if c == Command::ExecuteCommandStreamsMesa as i32 => {
                        let count = words.u32()?;
                        words.is(u64::from(count))?;
                        let mut spans = Vec::new();
                        for _ in 0..count {
                            spans.push((words.u32()?, words.u64()?, words.u64()?));
                        }
                        words.is(0)?;
                        (words.u32()? == 0).then_some(())?;
                        words.is(0)?;
                        (words.u32()? == 0).then_some(())?;
                        for (resource, offset, size) in spans {
                            let stream =
                                self.resource_bytes(state, context, (resource, offset, size))?;
                            self.run_stream(state, context, &stream);
                        }
                    }
                    _ => return None,
                }
                Some(())
            })();
            if ran.is_none() {
                if let Some(host) = state.contexts.get_mut(context) {
                    host.dead = true;
                }
                return;
            }
        }
    }

    /// The blob resource `resource` of `context`, as the renderer finds it.
    fn resource(state: &HostState, context: Context, resource: u32) -> Option<&HostBlob> {
        state
            .blobs
            .iter()
            .find(|(blob, held)| held.context == context && blob.resource_id() == Some(resource))
            .map(|(_, held)| held)
    }

    fn resource_bytes(
        &self,
        state: &HostState,
        context: Context,
        (resource, offset, size): (u32, u64, u64),
    ) -> Option<Vec<u8>> {
        let held = Self::resource(state, context, resource)?;
        (offset.checked_add(size)? <= held.size).then_some(())?;
        let at = held.at?;
        let mut bytes = vec![0; usize::try_from(size).ok()?];
        self.region
            .read(usize::try_from(at + offset).ok()?, &mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Write `reply` at the context's reply position.
    fn reply(&self, state: &mut HostState, context: Context, reply: &[u8]) -> Option<()> {
        let host = state.contexts.get_mut(context)?;
        let (resource, base, size) = host.reply?;
        let at = host.reply_at;
        if at + reply.len() as u64 > size {
            return None;
        }
        host.reply_at += reply.len() as u64;
        let held = Self::resource(state, context, resource)?;
        (base.checked_add(size)? <= held.size).then_some(())?;
        let mapped = held.at?;
        self.region
            .write(usize::try_from(mapped + base + at).ok()?, reply)
            .ok()
    }

    fn run_stream(&self, state: &mut HostState, context: Context, stream: &[u8]) {
        let mut words = Words(stream);
        while !words.0.is_empty() {
            if state.contexts.get(context).is_none_or(|host| host.dead) {
                return;
            }
            if self.run_command(state, context, &mut words).is_none() {
                if let Some(host) = state.contexts.get_mut(context) {
                    host.dead = true;
                }
                return;
            }
        }
    }

    fn insert(state: &mut HostState, context: Context, id: u64, object: Object) -> Option<()> {
        let host = state.contexts.get_mut(context)?;
        (id != 0 && !host.objects.contains_key(&id)).then_some(())?;
        host.objects.insert(id, object);
        Some(())
    }

    /// `family`, where the device was made with it.
    fn family(state: &HostState, context: Context, family: u32) -> Option<u32> {
        state
            .contexts
            .get(context)?
            .families
            .contains(&family)
            .then_some(family)
    }

    /// Note `id` as of `family`.
    fn of_family(state: &mut HostState, context: Context, id: u64, family: u32) -> Option<()> {
        state
            .contexts
            .get_mut(context)?
            .family_of
            .insert(id, family)
            .is_none()
            .then_some(())
    }

    fn has(state: &HostState, context: Context, id: u64) -> Option<()> {
        state
            .contexts
            .get(context)?
            .objects
            .contains_key(&id)
            .then_some(())
    }

    fn made(command: Command, id: u64) -> Vec<u8> {
        Grammar::default()
            .i32(command as i32)
            .i32(0)
            .u64(1)
            .u64(id)
            .0
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per command the backend sends"
    )]
    fn run_command(
        &self,
        state: &mut HostState,
        context: Context,
        words: &mut Words<'_>,
    ) -> Option<()> {
        let command = words.i32()?;
        let reply = words.u32()? & 1 == 1;
        let answer: Option<Vec<u8>> = match command {
            c if c == Command::CreateInstance as i32 => {
                words.is(1)?;
                words.structure(Structure::InstanceCreateInfo)?;
                words.u32()?;
                words.is(1)?;
                words.structure(Structure::ApplicationInfo)?;
                words.is(0)?;
                words.u32()?;
                words.is(0)?;
                words.u32()?;
                words.u32()?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Instance)?;
                Some(Self::made(Command::CreateInstance, id))
            }
            c if c == Command::EnumeratePhysicalDevices as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                let count = words.u32()?;
                let named = words.u64()?;
                (named == u64::from(count)).then_some(())?;
                let mut reply = Grammar::default()
                    .i32(Command::EnumeratePhysicalDevices as i32)
                    .i32(0)
                    .u64(1)
                    .u32(1);
                if named == 0 {
                    reply = reply.u64(0);
                } else {
                    let id = words.u64()?;
                    for _ in 1..named {
                        words.u64()?;
                    }
                    Self::insert(state, context, id, Object::Physical)?;
                    reply = reply.u64(1).u64(id);
                }
                Some(reply.0)
            }
            c if c == Command::GetPhysicalDeviceProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::PhysicalDeviceProperties2 as i32).then_some(())?;
                let robust = match words.chain()?.as_slice() {
                    [subgroup, maintenance] => {
                        (*subgroup, *maintenance)
                            == (
                                Structure::PhysicalDeviceSubgroupProperties as i32,
                                Structure::PhysicalDeviceMaintenance3Properties as i32,
                            )
                    }
                    .then_some(None)?,
                    [subgroup, maintenance, robust] => {
                        let offered = &state.offered;
                        ((*subgroup, *maintenance, *robust)
                            == (
                                Structure::PhysicalDeviceSubgroupProperties as i32,
                                Structure::PhysicalDeviceMaintenance3Properties as i32,
                                Structure::PhysicalDeviceRobustness2Properties as i32,
                            )
                            && (offered.offers(extension::ROBUSTNESS2_KHR)
                                || offered.offers(extension::ROBUSTNESS2_EXT)))
                        .then_some(Some(ROBUST))?
                    }
                    _ => return None,
                };
                let strict = state.offered.strict_lines;
                Some(
                    properties_reply(robust, state.offered.api_version, |grammar| {
                        lined_limits(grammar, strict)
                    })
                    .0,
                )
            }
            c if c == Command::EnumerateDeviceExtensionProperties as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(0)?;
                words.is(1)?;
                let capacity = words.u32()?;
                let names = &state.offered.extensions;
                let total = u32::try_from(names.len()).ok()?;
                if capacity == 0 {
                    words.is(0)?;
                    Some(
                        Grammar::default()
                            .i32(command)
                            .i32(0)
                            .u64(1)
                            .u32(total)
                            .u64(0)
                            .0,
                    )
                } else {
                    words.is(u64::from(capacity))?;
                    let written = capacity.min(total);
                    let result = if written < total { 5 } else { 0 };
                    let reply = names.iter().take(written as usize).fold(
                        Grammar::default()
                            .i32(command)
                            .i32(result)
                            .u64(1)
                            .u32(written)
                            .u64(u64::from(written)),
                        |grammar, name| {
                            let mut bytes = [0u8; EXTENSION_NAME_LEN];
                            bytes[..name.len()].copy_from_slice(name.as_bytes());
                            grammar.u64(EXTENSION_NAME_LEN as u64).bytes(&bytes).u32(1)
                        },
                    );
                    Some(reply.0)
                }
            }
            c if c == Command::GetPhysicalDeviceFeatures2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::PhysicalDeviceFeatures2 as i32).then_some(())?;
                let chain: Vec<Structure> = words
                    .chain()?
                    .into_iter()
                    .map(|raw| state.offered.chains(raw))
                    .collect::<Option<_>>()?;
                let mut chained = tairix_inline::ArrayVec::new();
                for &structure in &chain {
                    let mut members = tairix_inline::ArrayVec::new();
                    for at in 0..chained_members(structure)? {
                        members
                            .try_push(state.offered.feature(structure, at))
                            .ok()?;
                    }
                    chained.try_push(members).ok()?;
                }
                let features = HostFeatures {
                    core: core::array::from_fn(|at| !state.offered.lacks.contains(&at)),
                    chained,
                };
                Some(features_reply(&chain, &features).0)
            }
            c if c == Command::GetPhysicalDeviceQueueFamilyProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                let capacity = words.u32()?;
                words.is(u64::from(capacity))?;
                for _ in 0..capacity {
                    words.structure(Structure::QueueFamilyProperties2)?;
                }
                let mut families = alloc::vec![(state.offered.family_flags, FIRST_QUEUES, [1; 3])];
                if let Some((queues, granularity)) = state.offered.copy_engine {
                    families.push((queue::TRANSFER | queue::SPARSE_BINDING, queues, granularity));
                }
                let read = families.len().min(capacity as usize);
                let counted = u32::try_from(families.len()).ok()?;
                let mut reply = Grammar::default()
                    .i32(command)
                    .u64(1)
                    .u32(counted)
                    .u64(read as u64);
                for &(flags, queues, [width, height, depth]) in &families[..read] {
                    reply = reply
                        .structure(Structure::QueueFamilyProperties2)
                        .u32(flags)
                        .u32(queues)
                        .words(1)
                        .u32(width)
                        .u32(height)
                        .u32(depth);
                }
                Some(reply.0)
            }
            c if c == Command::GetPhysicalDeviceMemoryProperties2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::PhysicalDeviceMemoryProperties2)?;
                words.is(MAX_MEMORY_TYPES as u64)?;
                words.is(MAX_MEMORY_HEAPS as u64)?;
                let mut reply = Grammar::default()
                    .i32(command)
                    .u64(1)
                    .structure(Structure::PhysicalDeviceMemoryProperties2)
                    .u32(2)
                    .u64(MAX_MEMORY_TYPES as u64);
                for index in 0..MAX_MEMORY_TYPES {
                    reply = match index {
                        0 => reply.u32(1).u32(0),
                        1 => reply.u32(0xe).u32(0),
                        _ => reply.u32(0).u32(0),
                    };
                }
                reply = reply.u32(1).u64(MAX_MEMORY_HEAPS as u64);
                for index in 0..MAX_MEMORY_HEAPS {
                    reply = if index == 0 {
                        reply.u64(1 << 30).u32(1)
                    } else {
                        reply.u64(0).u32(0)
                    };
                }
                Some(reply.0)
            }
            c if c == Command::CreateDevice as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::DeviceCreateInfo as i32).then_some(())?;
                let chain = words.chain()?;
                let mut features = Vec::new();
                for &raw in chain.iter().rev() {
                    let structure = if raw == Structure::PhysicalDeviceFeatures2 as i32 {
                        None
                    } else {
                        Some(state.offered.chains(raw)?)
                    };
                    let len = structure.map_or(Some(CORE_FEATURES), chained_members)?;
                    let members: Vec<bool> =
                        (0..len).map(|_| words.boolean()).collect::<Option<_>>()?;
                    let offered = members.iter().enumerate().all(|(at, &enabled)| {
                        !enabled
                            || structure.map_or(!state.offered.lacks.contains(&at), |structure| {
                                state.offered.feature(structure, at)
                            })
                    });
                    offered.then_some(())?;
                    features.push((raw, members));
                }
                features.reverse();
                (features.first()?.0 == Structure::PhysicalDeviceFeatures2 as i32).then_some(())?;
                words.word(0)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut families = Vec::new();
                for _ in 0..count {
                    words.structure(Structure::DeviceQueueCreateInfo)?;
                    words.word(0)?;
                    let family = words.u32()?;
                    let queues = words.u32()?;
                    let offered = match family {
                        0 => FIRST_QUEUES,
                        1 => state.offered.copy_engine?.0,
                        _ => return None,
                    };
                    (!families.contains(&family) && (1..=offered).contains(&queues))
                        .then_some(())?;
                    words.is(u64::from(queues))?;
                    for _ in 0..queues {
                        words.u32()?;
                    }
                    families.push(family);
                }
                (!families.is_empty()).then_some(())?;
                words.word(0)?;
                words.is(0)?;
                let named = words.u32()?;
                let mut extensions = Vec::new();
                if named == 0 {
                    words.is(0)?;
                } else {
                    words.is(u64::from(named))?;
                    for _ in 0..named {
                        let name = words.string()?;
                        state.offered.offers(&name).then_some(())?;
                        extensions.push(name);
                    }
                }
                // No core features pointed at, and no allocator.
                words.is(0)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Device)?;
                state.contexts.get_mut(context)?.families = families;
                state.enabled = Some(Enablement {
                    extensions,
                    features,
                });
                Some(Self::made(Command::CreateDevice, id))
            }
            c if c == Command::GetDeviceQueue2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                (words.i32()? == Structure::DeviceQueueInfo2 as i32).then_some(())?;
                words.is(1)?;
                words.structure(Structure::DeviceQueueTimelineInfoMesa)?;
                let ring = words.u32()?;
                let rings = &mut state.contexts.get_mut(context)?.rings;
                ((1..=63).contains(&ring) && !rings.contains(&ring)).then_some(())?;
                rings.push(ring);
                words.u32()?;
                let family = Self::family(state, context, words.u32()?)?;
                words.u32()?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Queue)?;
                Self::of_family(state, context, id, family)?;
                None
            }
            c if c == Command::CreateCommandPool as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::CommandPoolCreateInfo)?;
                words.u32()?;
                let family = Self::family(state, context, words.u32()?)?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Pool)?;
                Self::of_family(state, context, id, family)?;
                Some(Self::made(Command::CreateCommandPool, id))
            }
            c if c == Command::AllocateCommandBuffers as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::CommandBufferAllocateInfo)?;
                let pool = words.u64()?;
                let family = *state.contexts.get(context)?.family_of.get(&pool)?;
                (words.i32()? == 0).then_some(())?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut reply = Grammar::default().i32(command).i32(0).u64(u64::from(count));
                for _ in 0..count {
                    let id = words.u64()?;
                    Self::insert(state, context, id, Object::Commands(Vec::new()))?;
                    Self::of_family(state, context, id, family)?;
                    reply = reply.u64(id);
                }
                Some(reply.0)
            }
            c if c == Command::CreateBuffer as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::BufferCreateInfo)?;
                words.u32()?;
                let size = words.u64()?;
                words.u32()?;
                // Every buffer is made for every use, a copy engine's among them.
                let families = &state.contexts.get(context)?.families;
                (shared(words, families)? == (families.len() > 1)).then_some(())?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::Buffer { size, memory: None })?;
                Some(Self::made(Command::CreateBuffer, id))
            }
            c if c == Command::GetBufferMemoryRequirements2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::BufferMemoryRequirementsInfo2)?;
                let id = words.u64()?;
                words.is(1)?;
                words.structure(Structure::MemoryRequirements2)?;
                let Some(Object::Buffer { size, .. }) =
                    state.contexts.get(context)?.objects.get(&id)
                else {
                    return None;
                };
                Some(
                    Grammar::default()
                        .i32(command)
                        .u64(1)
                        .structure(Structure::MemoryRequirements2)
                        .u64(size.next_multiple_of(256))
                        .u64(256)
                        .u32(0b11)
                        .0,
                )
            }
            c if c == Command::AllocateMemory as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::MemoryAllocateInfo)?;
                let size = words.u64()?;
                (words.u32()? < 2).then_some(())?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                // What the host held before: the backend zeroes it.
                let bytes = vec![0xa5; usize::try_from(size).ok()?];
                Self::insert(state, context, id, Object::Memory { bytes })?;
                Some(Self::made(Command::AllocateMemory, id))
            }
            c if c == Command::BindBufferMemory2 as i32 => {
                Self::has(state, context, words.u64()?)?;
                (words.u32()? == 1).then_some(())?;
                words.is(1)?;
                words.structure(Structure::BindBufferMemoryInfo)?;
                let (buffer, memory, offset) = (words.u64()?, words.u64()?, words.u64()?);
                (offset == 0).then_some(())?;
                Self::has(state, context, memory)?;
                let Some(Object::Buffer { memory: bound, .. }) =
                    state.contexts.get_mut(context)?.objects.get_mut(&buffer)
                else {
                    return None;
                };
                *bound = Some(memory);
                Some(Grammar::default().i32(command).i32(0).0)
            }
            c if c == Command::BeginCommandBuffer as i32 => {
                let id = words.u64()?;
                words.is(1)?;
                words.structure(Structure::CommandBufferBeginInfo)?;
                words.u32()?;
                words.is(0)?;
                let Some(Object::Commands(recorded)) =
                    state.contexts.get_mut(context)?.objects.get_mut(&id)
                else {
                    return None;
                };
                recorded.clear();
                None
            }
            c if c == Command::EndCommandBuffer as i32 => {
                Self::has(state, context, words.u64()?)?;
                None
            }
            c if c == Command::CmdCopyBuffer as i32 => {
                let id = words.u64()?;
                let (src, dst) = (words.u64()?, words.u64()?);
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut regions = Vec::new();
                for _ in 0..count {
                    regions.push((words.u64()?, words.u64()?, words.u64()?));
                }
                Self::record(state, context, id, Recorded::Copy(src, dst, regions))?;
                None
            }
            c if c == Command::CmdFillBuffer as i32 => {
                let id = words.u64()?;
                let fill = Recorded::Fill(words.u64()?, words.u64()?, words.u64()?, words.u32()?);
                Self::record(state, context, id, fill)?;
                None
            }
            c if c == Command::CmdUpdateBuffer as i32 => {
                let id = words.u64()?;
                let (dst, offset, size) = (words.u64()?, words.u64()?, words.u64()?);
                words.is(size)?;
                let data = words.take(usize::try_from(size).ok()?)?.to_vec();
                Self::record(state, context, id, Recorded::Update(dst, offset, data))?;
                None
            }
            c if c == Command::CmdPipelineBarrier as i32 => {
                let id = words.u64()?;
                Self::has(state, context, id)?;
                words.u32()?;
                words.u32()?;
                words.u32()?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                for _ in 0..count {
                    words.structure(Structure::MemoryBarrier)?;
                    words.u32()?;
                    words.u32()?;
                }
                if count > 0 {
                    Self::record(state, context, id, Recorded::Barrier)?;
                }
                // No buffer barrier.
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                for _ in 0..count {
                    let kept = Self::image_barrier(state, context, words)?;
                    Self::record(state, context, id, Recorded::Image(kept))?;
                }
                None
            }
            c if c == Command::QueueSubmit as i32 => {
                let queue = words.u64()?;
                Self::has(state, context, queue)?;
                (words.u32()? == 1).then_some(())?;
                words.is(1)?;
                words.structure(Structure::SubmitInfo)?;
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut buffers = Vec::new();
                for _ in 0..count {
                    buffers.push(words.u64()?);
                }
                (words.u32()? == 0).then_some(())?;
                words.is(0)?;
                words.is(0)?;
                // A command buffer runs only on a queue of its pool's family.
                let held = state.contexts.get(context)?;
                let family = held.family_of.get(&queue)?;
                buffers
                    .iter()
                    .all(|id| held.family_of.get(id) == Some(family))
                    .then_some(())?;
                for id in buffers {
                    self.execute(state, context, id)?;
                }
                None
            }
            c if c == Command::DestroyBuffer as i32 => {
                Self::has(state, context, words.u64()?)?;
                let id = words.u64()?;
                words.is(0)?;
                state.contexts.get_mut(context)?.objects.remove(&id)?;
                None
            }
            c if c == Command::FreeMemory as i32 => {
                Self::has(state, context, words.u64()?)?;
                let id = words.u64()?;
                words.is(0)?;
                if Self::mapped_at(state, context, id).is_some()
                    || state
                        .blobs
                        .iter()
                        .any(|(_, held)| held.context == context && held.blob_id == id)
                {
                    state.reached = state.reached.with(Unsent::FREED_EXPORTED);
                }
                state.contexts.get_mut(context)?.objects.remove(&id)?;
                None
            }
            c if c == Command::SetReplyCommandStreamMesa as i32 => {
                words.is(1)?;
                let span = (words.u32()?, words.u64()?, words.u64()?);
                let host = state.contexts.get_mut(context)?;
                host.reply = Some(span);
                host.reply_at = 0;
                None
            }
            c if c == Command::SeekReplyCommandStreamMesa as i32 => {
                let position = words.u64()?;
                state.contexts.get_mut(context)?.reply_at = position;
                None
            }
            c if c == Command::CreateShaderModule as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::ShaderModuleCreateInfo)?;
                words.word(0)?;
                let size = words.u64()?;
                (size % 4 == 0).then_some(())?;
                words.is(size / 4)?;
                let bytes = words.take(usize::try_from(size).ok()?)?.to_vec();
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                // The host compiles what it is given as it is: a module left
                // for it to specialize could mean something else there.
                let admitted = Admitted::new(Module::decode(&bytes).ok()?).ok()?;
                let specialized = admitted.module().instructions().iter().any(|instruction| {
                    matches!(
                        instruction.op(),
                        Op::SPEC_CONSTANT
                            | Op::SPEC_CONSTANT_TRUE
                            | Op::SPEC_CONSTANT_FALSE
                            | Op::SPEC_CONSTANT_COMPOSITE
                            | Op::SPEC_CONSTANT_OP
                    )
                });
                (!specialized).then_some(())?;
                Self::insert(state, context, id, Object::Module(Arc::new(admitted)))?;
                Some(Self::made(Command::CreateShaderModule, id))
            }
            c if c == Command::CreateDescriptorSetLayout as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::DescriptorSetLayoutCreateInfo)?;
                words.word(0)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut bindings = Vec::new();
                for _ in 0..count {
                    let binding = LayoutEntry {
                        binding: words.u32()?,
                        kind: words.u32()?,
                        count: words.u32()?,
                        stages: words.u32()?,
                    };
                    words.is(0)?;
                    bindings.push(binding);
                }
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::SetLayout(bindings))?;
                Some(Self::made(Command::CreateDescriptorSetLayout, id))
            }
            c if c == Command::CreatePipelineLayout as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::PipelineLayoutCreateInfo)?;
                words.word(0)?;
                let count = words.u32()?;
                let mut sets = Vec::new();
                for id in words.handles(count)? {
                    let Some(Object::SetLayout(bindings)) =
                        state.contexts.get(context)?.objects.get(&id)
                    else {
                        return None;
                    };
                    sets.push(bindings.clone());
                }
                let push = match words.u32()? {
                    0 => {
                        words.is(0)?;
                        (0, 0)
                    }
                    1 => {
                        words.is(1)?;
                        let stages = words.u32()?;
                        words.word(0)?;
                        (stages, words.u32()?)
                    }
                    _ => return None,
                };
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                Self::insert(state, context, id, Object::PipelineLayout { sets, push })?;
                Some(Self::made(Command::CreatePipelineLayout, id))
            }
            c if c == Command::CreateComputePipelines as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(0)?;
                words.word(1)?;
                words.is(1)?;
                words.structure(Structure::ComputePipelineCreateInfo)?;
                words.word(0)?;
                words.structure(Structure::PipelineShaderStageCreateInfo)?;
                words.word(0)?;
                words.word(stage::COMPUTE)?;
                let module = words.u64()?;
                let entry = words.string()?;
                words.is(0)?;
                let layout = words.u64()?;
                words.is(0)?;
                (words.i32()? == -1).then_some(())?;
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                let objects = &state.contexts.get(context)?.objects;
                let (Some(Object::Module(admitted)), Some(Object::PipelineLayout { sets, push })) =
                    (objects.get(&module), objects.get(&layout))
                else {
                    return None;
                };
                admitted.entry_point(&entry, Stage::Compute)?;
                let program = Program {
                    module: Arc::clone(admitted),
                    entry,
                    sets: sets.clone(),
                    push: *push,
                };
                let refused = state.misbehaves == Misbehaviour::RefusePipelines;
                if !refused {
                    Self::insert(state, context, id, Object::Pipeline(program))?;
                }
                Some(
                    Grammar::default()
                        .i32(command)
                        .i32(if refused { -1 } else { 0 })
                        .u64(1)
                        .u64(id)
                        .0,
                )
            }
            c if c == Command::CreateDescriptorPool as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::DescriptorPoolCreateInfo)?;
                words.word(0)?;
                words.word(1)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                let mut room = BTreeMap::new();
                for _ in 0..count {
                    let kind = words.u32()?;
                    *room.entry(kind).or_insert(0) += words.u32()?;
                }
                words.is(0)?;
                words.is(1)?;
                let id = words.u64()?;
                let pool = Object::DescriptorPool {
                    room,
                    sets: Vec::new(),
                };
                Self::insert(state, context, id, pool)?;
                Some(Self::made(Command::CreateDescriptorPool, id))
            }
            c if c == Command::AllocateDescriptorSets as i32 => {
                Self::has(state, context, words.u64()?)?;
                words.is(1)?;
                words.structure(Structure::DescriptorSetAllocateInfo)?;
                let pool = words.u64()?;
                words.word(1)?;
                let layout = words.handles(1)?[0];
                let set = words.handles(1)?[0];
                let objects = &mut state.contexts.get_mut(context)?.objects;
                let Some(Object::SetLayout(bindings)) = objects.get(&layout) else {
                    return None;
                };
                let bindings = bindings.clone();
                let refused = state.misbehaves == Misbehaviour::RefuseSets;
                let Some(Object::DescriptorPool { room, sets }) = objects.get_mut(&pool) else {
                    return None;
                };
                if !refused {
                    // One set a pool, and room in it for every descriptor.
                    sets.is_empty().then_some(())?;
                    for binding in &bindings {
                        let left = room.get_mut(&binding.kind)?;
                        *left = left.checked_sub(binding.count)?;
                    }
                    sets.push(set);
                    Self::insert(
                        state,
                        context,
                        set,
                        Object::Set {
                            layout: bindings,
                            written: BTreeMap::new(),
                        },
                    )?;
                }
                Some(
                    Grammar::default()
                        .i32(command)
                        .i32(if refused { -1 } else { 0 })
                        .u64(1)
                        .u64(set)
                        .0,
                )
            }
            c if c == Command::UpdateDescriptorSets as i32 => {
                Self::has(state, context, words.u64()?)?;
                let count = words.u32()?;
                words.is(u64::from(count))?;
                for _ in 0..count {
                    words.structure(Structure::WriteDescriptorSet)?;
                    let set = words.u64()?;
                    let binding = words.u32()?;
                    words.word(0)?;
                    words.word(1)?;
                    let kind = words.u32()?;
                    // One of an image's, a buffer's or a texel buffer's.
                    let image = match words.u64()? {
                        0 => None,
                        1 => {
                            let (sampler, view) = (words.u64()?, words.u64()?);
                            (words.i32()? == crate::venus::image::GENERAL).then_some(())?;
                            Some(Written::Image { sampler, view })
                        }
                        _ => return None,
                    };
                    let buffer = match words.u64()? {
                        0 => None,
                        1 => Some(Written::Buffer {
                            buffer: words.u64()?,
                            offset: words.u64()?,
                            range: words.u64()?,
                        }),
                        _ => return None,
                    };
                    let texels = match words.u64()? {
                        0 => None,
                        1 => Some(Written::Texels(words.u64()?)),
                        _ => return None,
                    };
                    let ((Some(one), None, None)
                    | (None, Some(one), None)
                    | (None, None, Some(one))) = (image, buffer, texels)
                    else {
                        return None;
                    };
                    Self::descriptor_valid(state, context, kind, one)?;
                    let Some(Object::Set { layout, written }) =
                        state.contexts.get_mut(context)?.objects.get_mut(&set)
                    else {
                        return None;
                    };
                    let laid = layout.iter().find(|laid| laid.binding == binding)?;
                    (laid.kind == kind).then_some(())?;
                    written.insert(binding, one);
                }
                words.word(0)?;
                words.is(0)?;
                None
            }
            c if c == Command::DestroyShaderModule as i32
                || c == Command::DestroyDescriptorSetLayout as i32
                || c == Command::DestroyPipelineLayout as i32
                || c == Command::DestroyPipeline as i32
                || c == Command::DestroyDescriptorPool as i32 =>
            {
                Self::has(state, context, words.u64()?)?;
                let id = words.u64()?;
                words.is(0)?;
                let objects = &mut state.contexts.get_mut(context)?.objects;
                let kind = match objects.get(&id)? {
                    Object::Module(_) => Command::DestroyShaderModule,
                    Object::SetLayout(_) => Command::DestroyDescriptorSetLayout,
                    Object::PipelineLayout { .. } => Command::DestroyPipelineLayout,
                    Object::Pipeline(_) | Object::Graphics(_) => Command::DestroyPipeline,
                    Object::DescriptorPool { .. } => Command::DestroyDescriptorPool,
                    _ => return None,
                };
                (kind as i32 == c).then_some(())?;
                if let Some(Object::DescriptorPool { sets, .. }) = objects.remove(&id) {
                    for set in sets {
                        objects.remove(&set);
                    }
                }
                None
            }
            c if c == Command::CmdBindPipeline as i32 => {
                let id = words.u64()?;
                let point = words.u32()?;
                let pipeline = words.u64()?;
                let recorded = match (point, state.contexts.get(context)?.objects.get(&pipeline)?) {
                    (BIND_POINT_COMPUTE, Object::Pipeline(_)) => Recorded::BindPipeline(pipeline),
                    (BIND_POINT_GRAPHICS, Object::Graphics(_)) => Recorded::BindGraphics(pipeline),
                    _ => return None,
                };
                Self::record(state, context, id, recorded)?;
                None
            }
            c if c == Command::CmdBindDescriptorSets as i32 => {
                let id = words.u64()?;
                let point = words.u32()?;
                (point == BIND_POINT_COMPUTE || point == BIND_POINT_GRAPHICS).then_some(())?;
                let layout = words.u64()?;
                words.word(0)?;
                let count = words.u32()?;
                let sets = words.handles(count)?;
                words.word(0)?;
                words.is(0)?;
                let objects = &state.contexts.get(context)?.objects;
                let laid_out = matches!(objects.get(&layout), Some(Object::PipelineLayout { .. }));
                let all_sets = sets
                    .iter()
                    .all(|set| matches!(objects.get(set), Some(Object::Set { .. })));
                (laid_out && all_sets).then_some(())?;
                Self::record(state, context, id, Recorded::BindSets(point, sets))?;
                None
            }
            c if c == Command::CmdPushConstants as i32 => {
                let id = words.u64()?;
                let layout = words.u64()?;
                let stages = words.u32()?;
                words.word(0)?;
                let size = words.u32()?;
                words.is(u64::from(size))?;
                let bytes = words.take(usize::try_from(size).ok()?)?.to_vec();
                let Some(Object::PipelineLayout { push, .. }) =
                    state.contexts.get(context)?.objects.get(&layout)
                else {
                    return None;
                };
                (size % 4 == 0 && size <= push.1 && stages == push.0).then_some(())?;
                Self::record(state, context, id, Recorded::Push(stages, bytes))?;
                None
            }
            c if c == Command::CreateGraphicsPipelines as i32 => {
                Self::has(state, context, words.u64()?)?;
                let rectangular = !state.offered.strict_lines;
                let (id, graphics) =
                    host_render::decode_graphics(state, context, words, rectangular)?;
                let refused = state.misbehaves == Misbehaviour::RefusePipelines;
                if !refused {
                    Self::insert(state, context, id, Object::Graphics(graphics))?;
                }
                Some(
                    Grammar::default()
                        .i32(command)
                        .i32(if refused { -1 } else { 0 })
                        .u64(1)
                        .u64(id)
                        .0,
                )
            }
            c if c == Command::CmdBeginRendering as i32 => {
                let id = words.u64()?;
                let rendering = host_render::decode_rendering(words)?;
                Self::record(
                    state,
                    context,
                    id,
                    Recorded::BeginRendering(Box::new(rendering)),
                )?;
                None
            }
            c if c == Command::CmdEndRendering as i32 => {
                let id = words.u64()?;
                Self::record(state, context, id, Recorded::EndRendering)?;
                None
            }
            c if c == Command::CmdBindVertexBuffers2 as i32 => {
                let id = words.u64()?;
                words.word(0)?;
                let count = words.u32()?;
                let buffers = words.handles(count)?;
                let mut members = [Vec::new(), Vec::new()];
                for member in &mut members {
                    words.is(u64::from(count))?;
                    for _ in 0..count {
                        member.push(words.u64()?);
                    }
                }
                // Each stride the pipeline's.
                words.is(0)?;
                let [offsets, sizes] = members;
                let mut bound = Vec::new();
                for ((&buffer, offset), size) in buffers.iter().zip(offsets).zip(sizes) {
                    let (_, held) = Self::bound(state, context, buffer)?;
                    (offset < held && offset.checked_add(size)? <= held).then_some(())?;
                    bound.push([buffer, offset, size]);
                }
                Self::record(state, context, id, Recorded::VertexBuffers(bound))?;
                None
            }
            c if c == Command::CmdBindIndexBuffer2 as i32 => {
                let id = words.u64()?;
                let range = [words.u64()?, words.u64()?, words.u64()?];
                let wide = match words.i32()? {
                    0 => false,
                    1 => true,
                    _ => return None,
                };
                let (_, held) = Self::bound(state, context, range[0])?;
                let index = if wide { 4 } else { 2 };
                let whole = range[1].is_multiple_of(index) && range[2].is_multiple_of(index);
                (whole && range[1] < held && range[1].checked_add(range[2])? <= held)
                    .then_some(())?;
                Self::record(state, context, id, Recorded::Index(range, wide))?;
                None
            }
            c if c == Command::CmdSetViewport as i32 => {
                let id = words.u64()?;
                words.word(0)?;
                words.word(1)?;
                words.is(1)?;
                let viewport = [
                    words.f32()?,
                    words.f32()?,
                    words.f32()?,
                    words.f32()?,
                    words.f32()?,
                    words.f32()?,
                ];
                Self::record(state, context, id, Recorded::Viewport(viewport))?;
                None
            }
            c if c == Command::CmdSetScissor as i32 => {
                let id = words.u64()?;
                words.word(0)?;
                words.word(1)?;
                words.is(1)?;
                let scissor = [
                    i64::from(words.i32()?),
                    i64::from(words.i32()?),
                    i64::from(words.u32()?),
                    i64::from(words.u32()?),
                ];
                let most = i64::from(i32::MAX);
                let [x, y, width, height] = scissor;
                (x >= 0 && y >= 0 && x + width <= most && y + height <= most).then_some(())?;
                Self::record(state, context, id, Recorded::Scissor(scissor))?;
                None
            }
            c if c == Command::CmdSetStencilReference as i32 => {
                let id = words.u64()?;
                words.word(3)?;
                let reference = words.u32()?;
                Self::record(state, context, id, Recorded::StencilReference(reference))?;
                None
            }
            c if c == Command::CmdSetBlendConstants as i32 => {
                let id = words.u64()?;
                words.is(4)?;
                let constants = [words.f32()?, words.f32()?, words.f32()?, words.f32()?];
                Self::record(state, context, id, Recorded::BlendConstants(constants))?;
                None
            }
            c if c == Command::CmdDraw as i32 => {
                let id = words.u64()?;
                let counts = [words.u32()?, words.u32()?, words.u32()?, words.u32()?];
                let record = host_render::DrawRecord::Vertices(counts);
                Self::record(state, context, id, Recorded::Draw(record))?;
                None
            }
            c if c == Command::CmdDrawIndexed as i32 => {
                let id = words.u64()?;
                let counts = [words.u32()?, words.u32()?, words.u32()?];
                let (base, first_instance) = (words.i32()?, words.u32()?);
                let record = host_render::DrawRecord::Indexed(counts, base, first_instance);
                Self::record(state, context, id, Recorded::Draw(record))?;
                None
            }
            c if c == Command::CmdDrawIndirect as i32
                || c == Command::CmdDrawIndexedIndirect as i32 =>
            {
                let indexed = c == Command::CmdDrawIndexedIndirect as i32;
                let id = words.u64()?;
                let (buffer, offset) = (words.u64()?, words.u64()?);
                let (count, stride) = (words.u32()?, words.u32()?);
                let record_bytes = if indexed { 20 } else { 16 };
                let (_, held) = Self::bound(state, context, buffer)?;
                let span = match count {
                    0 => 0,
                    _ => u64::from(count - 1) * u64::from(stride) + record_bytes,
                };
                let strided = count <= 1 || (stride % 4 == 0 && u64::from(stride) >= record_bytes);
                (offset % 4 == 0 && strided && offset.checked_add(span)? <= held).then_some(())?;
                let record =
                    host_render::DrawRecord::Indirect(indexed, buffer, offset, count, stride);
                Self::record(state, context, id, Recorded::Draw(record))?;
                None
            }
            c if c == Command::CmdDispatch as i32 => {
                let id = words.u64()?;
                let groups = [words.u32()?, words.u32()?, words.u32()?];
                Self::record(state, context, id, Recorded::Dispatch(groups))?;
                None
            }
            c if c == Command::CmdDispatchIndirect as i32 => {
                let id = words.u64()?;
                let (buffer, offset) = (words.u64()?, words.u64()?);
                let (_, size) = Self::bound(state, context, buffer)?;
                (offset % 4 == 0 && offset.checked_add(12)? <= size).then_some(())?;
                Self::record(
                    state,
                    context,
                    id,
                    Recorded::DispatchIndirect(buffer, offset),
                )?;
                None
            }
            c if host_images::COMMANDS.contains(&c) => {
                match Self::run_image_command(state, context, words, c)? {
                    Reply::Silent => None,
                    Reply::Written(reply) => Some(reply),
                }
            }
            _ => return None,
        };
        match (reply, answer) {
            (true, Some(answer)) => self.reply(state, context, &answer),
            (false, _) => Some(()),
            // A reply asked of a command that writes none.
            (true, None) => None,
        }
    }

    fn record(state: &mut HostState, context: Context, id: u64, command: Recorded) -> Option<()> {
        let Some(Object::Commands(recorded)) =
            state.contexts.get_mut(context)?.objects.get_mut(&id)
        else {
            return None;
        };
        recorded.push(command);
        Some(())
    }

    /// The memory a buffer is bound to, and its size.
    fn bound(state: &HostState, context: Context, buffer: u64) -> Option<(u64, u64)> {
        match state.contexts.get(context)?.objects.get(&buffer)? {
            Object::Buffer { size, memory } => Some(((*memory)?, *size)),
            _ => None,
        }
    }

    fn execute(&self, state: &mut HostState, context: Context, id: u64) -> Option<()> {
        let Some(Object::Commands(recorded)) = state.contexts.get_mut(context)?.objects.remove(&id)
        else {
            return None;
        };
        let mut bound = Bound::default();
        let mut drawing = host_render::DrawState::default();
        let mut pass: Option<host_render::PassRun> = None;
        let mut unordered = Unordered::default();
        for command in &recorded {
            (pass.is_none() || command.within_rendering()).then_some(())?;
            if matches!(command, Recorded::Barrier) {
                unordered.0.clear();
                continue;
            }
            unordered.note(Self::accesses(state, context, (&bound, &drawing), command)?)?;
            match command {
                Recorded::Copy(src, dst, regions) => {
                    let (from, _) = Self::bound(state, context, *src)?;
                    let (to, _) = Self::bound(state, context, *dst)?;
                    for &(at, into, len) in regions {
                        let mut bytes = vec![0; usize::try_from(len).ok()?];
                        self.read_memory(state, context, from, at, &mut bytes);
                        self.write_memory(state, context, to, into, &bytes);
                    }
                }
                Recorded::Fill(dst, offset, size, word) => {
                    let (to, buffer) = Self::bound(state, context, *dst)?;
                    let size = if *size == u64::MAX {
                        (buffer - offset) & !3
                    } else {
                        *size
                    };
                    let bytes = word.to_le_bytes().repeat(usize::try_from(size / 4).ok()?);
                    self.write_memory(state, context, to, *offset, &bytes);
                }
                Recorded::Update(dst, offset, data) => {
                    let (to, _) = Self::bound(state, context, *dst)?;
                    self.write_memory(state, context, to, *offset, data);
                }
                Recorded::BindPipeline(id) => {
                    let Some(Object::Pipeline(program)) =
                        state.contexts.get(context)?.objects.get(id)
                    else {
                        return None;
                    };
                    bound.program = Some(program.clone());
                }
                Recorded::BindSets(BIND_POINT_COMPUTE, sets) => bound.sets.clone_from(sets),
                Recorded::BindSets(_, sets) => drawing.sets.clone_from(sets),
                Recorded::Push(stages, bytes) if *stages == stage::COMPUTE => {
                    bound.push.clone_from(bytes);
                }
                Recorded::Push(_, bytes) => drawing.push.clone_from(bytes),
                Recorded::BindGraphics(id) => drawing.pipeline = Some(*id),
                Recorded::VertexBuffers(buffers) => drawing.vertex_buffers.clone_from(buffers),
                Recorded::Index(range, wide) => drawing.index = Some((*range, *wide)),
                Recorded::Viewport(viewport) => drawing.viewport = Some(*viewport),
                Recorded::Scissor(scissor) => drawing.scissor = Some(*scissor),
                Recorded::StencilReference(reference) => drawing.reference = Some(*reference),
                Recorded::BlendConstants(constants) => drawing.constants = Some(*constants),
                Recorded::BeginRendering(rendering) => {
                    (pass.is_none()).then_some(())?;
                    // A rendering instance takes its own copy of what it
                    // names, as each is recorded once.
                    pass = Some(host_render::PassRun::new((**rendering).clone()));
                }
                Recorded::Draw(record) => {
                    pass.as_mut()?.draw(state, context, (&drawing, *record))?;
                }
                Recorded::EndRendering => {
                    let run = pass.take()?;
                    self.render(state, context, &run)?;
                }
                Recorded::Dispatch(groups) => self.dispatch(state, context, &bound, *groups)?,
                Recorded::DispatchIndirect(buffer, offset) => {
                    let (memory, _) = Self::bound(state, context, *buffer)?;
                    let mut words = [0u8; 12];
                    self.read_memory(state, context, memory, *offset, &mut words);
                    let (counts, _) = words.as_chunks::<4>();
                    let groups = core::array::from_fn(|axis| u32::from_le_bytes(counts[axis]));
                    self.dispatch(state, context, &bound, groups)?;
                }
                Recorded::Image(command) => self.execute_image(state, context, command)?,
                Recorded::Barrier => {}
            }
        }
        // No rendering instance outlives its command buffer.
        pass.is_none().then_some(())?;
        state
            .contexts
            .get_mut(context)?
            .objects
            .insert(id, Object::Commands(recorded));
        Some(())
    }
}

impl Host {
    /// The bytes of buffers `command` reads and writes, run with `bound` and
    /// `drawing` as they stand.
    fn accesses(
        state: &HostState,
        context: Context,
        (bound, drawing): (&Bound, &host_render::DrawState),
        command: &Recorded,
    ) -> Option<Vec<Access>> {
        let reach = |buffer: u64, (start, len): (u64, u64), write: bool| {
            let (memory, size) = Self::bound(state, context, buffer)?;
            let end = if len == u64::MAX {
                size
            } else {
                start.checked_add(len)?
            };
            Some(Access {
                memory,
                start,
                end,
                write,
            })
        };
        let mut accesses = Vec::new();
        match command {
            Recorded::Copy(src, dst, regions) => {
                for &(at, into, len) in regions {
                    accesses.push(reach(*src, (at, len), false)?);
                    accesses.push(reach(*dst, (into, len), true)?);
                }
            }
            Recorded::Fill(dst, offset, size, _) => {
                accesses.push(reach(*dst, (*offset, *size), true)?);
            }
            Recorded::Update(dst, offset, data) => {
                accesses.push(reach(
                    *dst,
                    (*offset, u64::try_from(data.len()).ok()?),
                    true,
                )?);
            }
            Recorded::Dispatch(_) | Recorded::DispatchIndirect(..) => {
                if let Recorded::DispatchIndirect(buffer, offset) = command {
                    accesses.push(reach(*buffer, (*offset, 12), false)?);
                }
                let program = bound.program.as_ref()?;
                let ranges = Self::ranges(state, context, &program.sets, &bound.sets)?;
                accesses.extend(Self::bound_accesses(state, context, &ranges)?);
            }
            Recorded::Draw(record) => {
                for &[buffer, offset, size] in &drawing.vertex_buffers {
                    accesses.push(reach(buffer, (offset, size), false)?);
                }
                if let Some(([buffer, offset, size], _)) = drawing.index {
                    accesses.push(reach(buffer, (offset, size), false)?);
                }
                if let host_render::DrawRecord::Indirect(_, buffer, offset, count, stride) = *record
                {
                    let len = u64::from(count) * u64::from(stride);
                    accesses.push(reach(buffer, (offset, len), false)?);
                }
                let objects = &state.contexts.get(context)?.objects;
                let graphics = host_render::graphics_of(objects, drawing.pipeline?)?;
                let ranges = Self::ranges(state, context, &graphics.sets, &drawing.sets)?;
                accesses.extend(Self::bound_accesses(state, context, &ranges)?);
            }
            Recorded::Image(image) => {
                for (buffer, start, len, write) in Self::image_buffers(state, context, image)? {
                    accesses.push(reach(buffer, (start, len), write)?);
                }
            }
            _ => {}
        }
        Some(accesses)
    }

    /// The bytes of buffers `ranges` bind: written through a storage
    /// binding, read through any other.
    fn bound_accesses(
        state: &HostState,
        context: Context,
        ranges: &[Range],
    ) -> Option<Vec<Access>> {
        let objects = &state.contexts.get(context)?.objects;
        let mut accesses = Vec::new();
        for range in ranges {
            let write = matches!(
                range.entry.kind,
                descriptor::STORAGE_BUFFER | descriptor::STORAGE_TEXEL_BUFFER
            );
            let (buffer, start, len) = match range.written {
                Written::Buffer {
                    buffer,
                    offset,
                    range,
                } => (buffer, offset, range),
                Written::Texels(id) => {
                    let Some(Object::Texels(texels)) = objects.get(&id) else {
                        return None;
                    };
                    (texels.buffer, texels.offset, texels.range)
                }
                Written::Image { .. } => continue,
            };
            let (memory, size) = Self::bound(state, context, buffer)?;
            let end = if len == u64::MAX {
                size
            } else {
                start.checked_add(len)?
            };
            accesses.push(Access {
                memory,
                start,
                end,
                write,
            });
        }
        Some(accesses)
    }

    /// The bytes memory `id` of `context` holds.
    fn memory_len(state: &HostState, context: Context, id: u64) -> Option<usize> {
        match state.contexts.get(context)?.objects.get(&id)? {
            Object::Memory { bytes } => Some(bytes.len()),
            _ => None,
        }
    }

    /// Run `groups` workgroups of what `bound` binds on the reference
    /// device, through the memory and images its sets name: what a device
    /// past its counts would run is noted and not run, and a dispatch of no
    /// workgroups runs nothing.
    fn dispatch(
        &self,
        state: &mut HostState,
        context: Context,
        bound: &Bound,
        groups: [u32; 3],
    ) -> Option<()> {
        if groups.iter().any(|&count| count > MAX_GROUPS) {
            state.reached = state.reached.with(Unsent::OVERDISPATCHED);
            return Some(());
        }
        if groups.contains(&0) {
            return Some(());
        }
        let program = bound.program.as_ref()?;
        let ranges = Self::ranges(state, context, &program.sets, &bound.sets)?;
        let gpu = self
            .reference
            .create_context(&ContextDesc::default())
            .ok()?;
        let mut uploaded = Uploaded::default();
        self.upload(state, context, &gpu, (&ranges, &mut uploaded))?;
        let info = self.reference.info();
        let entries = [(&*program.module, program.entry.as_str(), Stage::Compute)];
        let (layouts, sets) = bind(
            state,
            context,
            (&gpu, info, (&program.sets, &entries)),
            &ranges,
            &uploaded,
        )?;
        let layout_refs: Vec<&BindSetLayout> = layouts.iter().collect();
        let (push_stages, push_bytes) = program.push;
        let pipeline_layout = gpu
            .create_pipeline_layout(
                &layout_refs,
                PushConstants {
                    bytes: push_bytes,
                    stages: stages_of(push_stages)?,
                },
            )
            .ok()?;
        // The reference takes a module of its own.
        let module = Admitted::new(Module::decode(program.module.module().bytes()).ok()?).ok()?;
        let shader = gpu.create_shader_module(module).ok()?;
        let pipeline = gpu
            .create_compute_pipeline(&ComputePipelineDesc {
                module: &shader,
                entry_point: &program.entry,
                layout: &pipeline_layout,
                specialization: &[],
            })
            .ok()?;
        let set_refs: Vec<&BindSet> = sets.iter().collect();
        let mut commands = gpu.command_buffer();
        for (_, mirrored) in &uploaded.images {
            let (_, staging) = &mirrored.staging;
            commands
                .copy_buffer_to_image(staging, &mirrored.image, &mirrored.regions)
                .ok()?;
        }
        commands
            .dispatch(&pipeline, &set_refs, &bound.push, groups)
            .ok()?;
        for (_, mirrored) in &uploaded.images {
            let (_, staging) = &mirrored.staging;
            commands
                .copy_image_to_buffer(&mirrored.image, staging, &mirrored.regions)
                .ok()?;
        }
        host_images::run(&gpu, &commands)?;
        for (memory, allocation, len) in &uploaded.memories {
            let mut bytes = vec![0; *len];
            allocation.read(0, &mut bytes).ok()?;
            self.write_memory(state, context, *memory, 0, &bytes);
        }
        // After the memories, as an image may lie in one a buffer spans.
        for (_, mirrored) in &uploaded.images {
            let (staging, _) = &mirrored.staging;
            let mut texels = vec![0; mirrored.len];
            staging.read(0, &mut texels).ok()?;
            let (memory, offset) = mirrored.place;
            self.write_memory(state, context, memory, offset, &texels);
        }
        state.dispatched += 1;
        Some(())
    }

    /// Each binding of a pipeline's sets, laid out as `layouts`, with what
    /// was written to it in `sets`, as bound: its set's place, its layout
    /// entry and the descriptor.
    fn ranges(
        state: &HostState,
        context: Context,
        layouts: &[Vec<LayoutEntry>],
        sets: &[u64],
    ) -> Option<Vec<Range>> {
        let mut ranges = Vec::new();
        for (index, layout) in layouts.iter().enumerate() {
            let Some(Object::Set { written, .. }) =
                state.contexts.get(context)?.objects.get(sets.get(index)?)
            else {
                return None;
            };
            for entry in layout {
                ranges.push(Range {
                    set: index,
                    entry: *entry,
                    written: *written.get(&entry.binding)?,
                });
            }
        }
        Some(ranges)
    }

    /// The reference's copy of everything `ranges` reach: each buffer and
    /// the memory it lies in, each image a view sees, and every view,
    /// sampler and texel buffer.
    fn upload(
        &self,
        state: &HostState,
        context: Context,
        gpu: &tairix_gpu::Context,
        (ranges, uploaded): (&[Range], &mut Uploaded),
    ) -> Option<()> {
        let objects = &state.contexts.get(context)?.objects;
        for range in ranges {
            match range.written {
                Written::Buffer { buffer, .. } => {
                    self.upload_buffer(state, context, gpu, uploaded, buffer)?;
                }
                Written::Texels(id) => {
                    if held(&uploaded.texels, id).is_some() {
                        continue;
                    }
                    let Some(Object::Texels(texels)) = objects.get(&id) else {
                        return None;
                    };
                    self.upload_buffer(state, context, gpu, uploaded, texels.buffer)?;
                    let buffer = held(&uploaded.buffers, texels.buffer)?;
                    let made = host_images::mirrored_texels(gpu, buffer, texels)?;
                    uploaded.texels.push((id, made));
                }
                Written::Image { sampler, view } => {
                    if sampler != 0 && held(&uploaded.samplers, sampler).is_none() {
                        let Some(Object::Sampler(info)) = objects.get(&sampler) else {
                            return None;
                        };
                        let made = host_images::mirrored_sampler(gpu, info)?;
                        uploaded.samplers.push((sampler, made));
                    }
                    if view != 0 && held(&uploaded.views, view).is_none() {
                        let Some(Object::View(info)) = objects.get(&view) else {
                            return None;
                        };
                        if held(&uploaded.images, info.image).is_none() {
                            let mirrored = self.mirror(state, context, gpu, info.image)?;
                            uploaded.images.push((info.image, mirrored));
                        }
                        let image = &held(&uploaded.images, info.image)?.image;
                        let made = Self::mirrored_view(gpu, info, image)?;
                        uploaded.views.push((view, made));
                    }
                }
            }
        }
        Some(())
    }

    /// The reference's copy of buffer `id` and the memory it lies in, held
    /// in `uploaded`.
    fn upload_buffer(
        &self,
        state: &HostState,
        context: Context,
        gpu: &tairix_gpu::Context,
        uploaded: &mut Uploaded,
        id: u64,
    ) -> Option<()> {
        if held(&uploaded.buffers, id).is_some() {
            return Some(());
        }
        let (memory, size) = Self::bound(state, context, id)?;
        if !uploaded.memories.iter().any(|(held, _, _)| *held == memory) {
            let len = Self::memory_len(state, context, memory)?;
            let mut bytes = vec![0; len];
            self.read_memory(state, context, memory, 0, &mut bytes);
            let allocation = gpu
                .allocate(&AllocationDesc {
                    size: len as u64,
                    memory_type: 0,
                })
                .ok()?;
            allocation.write(0, &bytes).ok()?;
            uploaded.memories.push((memory, allocation, len));
        }
        let (_, allocation, _) = uploaded
            .memories
            .iter()
            .find(|(held, _, _)| *held == memory)?;
        let made = gpu
            .create_buffer(
                allocation,
                &BufferDesc {
                    offset: 0,
                    size,
                    usage: BufferUsage::ALL,
                },
            )
            .ok()?;
        uploaded.buffers.push((id, made));
        Some(())
    }
}

/// The reference's copy of what a dispatch reaches, each by the host's id:
/// each memory's allocation holding its bytes, with its length; each buffer
/// over one; each image, view, sampler and texel buffer.
#[derive(Default)]
struct Uploaded {
    memories: Vec<(u64, tairix_gpu::Allocation, usize)>,
    buffers: Vec<(u64, tairix_gpu::Buffer)>,
    images: Vec<(u64, Mirrored)>,
    views: Vec<(u64, tairix_gpu::ImageView)>,
    samplers: Vec<(u64, tairix_gpu::Sampler)>,
    texels: Vec<(u64, tairix_gpu::BufferView)>,
}

/// What `uploaded` holds of the host's object `id`.
fn held<T>(uploaded: &[(u64, T)], id: u64) -> Option<&T> {
    uploaded
        .iter()
        .find(|(held, _)| *held == id)
        .map(|(_, made)| made)
}

/// A binding a dispatch reads, and what was written to it.
struct Range {
    set: usize,
    entry: LayoutEntry,
    written: Written,
}

/// The reference's layout of each of `program`'s sets, each binding of the
/// type what was written to it reads as, and each set binding what
/// `uploaded` holds of `ranges`.
/// The entry points a pipeline runs: each module, its entry point's name and
/// its stage.
type Entries<'a> = [(&'a Admitted, &'a str, Stage)];

fn bind(
    state: &HostState,
    context: Context,
    (gpu, info, (set_layouts, entries)): (
        &tairix_gpu::Context,
        &tairix_gpu::DeviceInfo,
        (&[Vec<LayoutEntry>], &Entries<'_>),
    ),
    ranges: &[Range],
    uploaded: &Uploaded,
) -> Option<(Vec<BindSetLayout>, Vec<BindSet>)> {
    let written: BTreeMap<(u32, u32), Written> = ranges
        .iter()
        .map(|range| {
            Some((
                (u32::try_from(range.set).ok()?, range.entry.binding),
                range.written,
            ))
        })
        .collect::<Option<_>>()?;
    let mut layouts = Vec::new();
    for (index, layout) in set_layouts.iter().enumerate() {
        let set = u32::try_from(index).ok()?;
        let bindings: Vec<LayoutBinding> = layout
            .iter()
            .map(|entry| {
                host_images::reference_binding(
                    state,
                    context,
                    (info, entries),
                    ((set, entry.binding), entry),
                    &written,
                )
            })
            .collect::<Option<_>>()?;
        layouts.push(gpu.create_bind_set_layout(&bindings).ok()?);
    }
    let mut sets = Vec::new();
    for (index, layout) in layouts.iter().enumerate() {
        let of_set: Vec<&Range> = ranges.iter().filter(|range| range.set == index).collect();
        let resources: Vec<Resource<'_>> = of_set
            .iter()
            .map(|range| {
                let (sampler, view) = match range.written {
                    Written::Image { sampler, view } => (sampler, view),
                    _ => (0, 0),
                };
                Some(match (range.entry.kind, range.written) {
                    (descriptor::SAMPLER, _) => {
                        Resource::Sampler(held(&uploaded.samplers, sampler)?)
                    }
                    (descriptor::COMBINED_IMAGE_SAMPLER, _) => Resource::ImageSampler(
                        held(&uploaded.views, view)?,
                        held(&uploaded.samplers, sampler)?,
                    ),
                    (descriptor::SAMPLED_IMAGE | descriptor::STORAGE_IMAGE, _) => {
                        Resource::Image(held(&uploaded.views, view)?)
                    }
                    (_, Written::Texels(id)) => Resource::TexelBuffer(held(&uploaded.texels, id)?),
                    (
                        _,
                        Written::Buffer {
                            buffer,
                            offset,
                            range,
                        },
                    ) => Resource::Buffer {
                        buffer: held(&uploaded.buffers, buffer)?,
                        offset,
                        size: range,
                    },
                    _ => return None,
                })
            })
            .collect::<Option<_>>()?;
        let entries: Vec<BindSetEntry<'_>> = of_set
            .iter()
            .zip(&resources)
            .map(|(range, resource)| BindSetEntry {
                binding: range.entry.binding,
                resources: core::slice::from_ref(resource),
            })
            .collect();
        sets.push(gpu.create_bind_set(layout, &entries).ok()?);
    }
    Some((layouts, sets))
}

/// The stages shader stage `flags` name.
fn stages_of(flags: u32) -> Option<Stages> {
    let named = stage::VERTEX | stage::FRAGMENT | stage::COMPUTE;
    (flags & !named == 0).then_some(())?;
    Some(
        [
            (stage::VERTEX, Stages::VERTEX),
            (stage::FRAGMENT, Stages::FRAGMENT),
            (stage::COMPUTE, Stages::COMPUTE),
        ]
        .into_iter()
        .filter(|&(flag, _)| flags & flag != 0)
        .fold(Stages::NONE, |stages, (_, stage)| stages.with(stage)),
    )
}

/// What a rouse runs first, on the thread ordering: a test's look at the
/// order's moment.
type Probe = Arc<StdMutex<Option<Box<dyn Fn() + Send>>>>;

/// Rouses the test's display end.
struct Roused(Arc<(StdMutex<bool>, StdCondvar)>, Probe);

impl Rouse for Roused {
    fn rouse(&self) -> bool {
        if let Some(probe) = self.1.lock().expect("unpoisoned").as_ref() {
            probe();
        }
        *self.0 .0.lock().expect("unpoisoned") = true;
        self.0 .1.notify_all();
        true
    }
}

/// Threads of the test's own.
#[derive(Default)]
struct Threads(StdMutex<Vec<JoinHandle<()>>>);

impl Spawn for Threads {
    fn spawn(&self, body: Box<dyn FnOnce() + Send>) -> bool {
        self.0
            .lock()
            .expect("unpoisoned")
            .push(std::thread::spawn(body));
        true
    }
}

/// Real time, so a job never answered times out instead of holding the test.
struct Wall(std::time::Instant);

impl MonotonicClock for Wall {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Longer than any answer a correct run waits for.
const ANSWERED: Duration64 = Duration64::from_secs(10);

/// A host, the desk to it, and the display end serving it on a thread.
struct Rig {
    host: Arc<Host>,
    desk: Arc<Desk<Then>>,
    threads: Threads,
    stop: Arc<AtomicBool>,
    roused: Arc<(StdMutex<bool>, StdCondvar)>,
    probe: Probe,
    display: Option<JoinHandle<()>>,
}

impl Rig {
    fn new() -> Self {
        let whole = HwResource::memory_region(0x10_0000_0000, REGION_BYTES as u64, 1);
        let reference = Device::software(&SoftwareDesc {
            memory_bytes: 64 << 20,
            clock: Arc::new(Still),
            runner: Arc::new(tairix_parallel::Serial),
            threads: &NoThreads,
            subgroup_size: DEFAULT_SUBGROUP_SIZE,
        })
        .expect("the reference");
        let host = Arc::new(Host {
            region: Arc::new(MockRegion::new(REGION_BYTES)),
            shared: Arc::new(Mutex::new(SharedMemory::new(whole).expect("a region"))),
            state: StdMutex::new(HostState::default()),
            changed: StdCondvar::new(),
            reference,
        });
        let roused = Arc::new((StdMutex::new(false), StdCondvar::new()));
        let probe = Probe::default();
        let desk = Arc::new(Desk::new(Box::new(Roused(
            Arc::clone(&roused),
            Arc::clone(&probe),
        ))));
        let stop = Arc::new(AtomicBool::new(false));
        let display = {
            let (host, desk, stop, roused) = (
                Arc::clone(&host),
                Arc::clone(&desk),
                Arc::clone(&stop),
                Arc::clone(&roused),
            );
            std::thread::spawn(move || {
                // Orders taken while the host is paused, carried out in order
                // once it resumes; and the answers a ring keeps back.
                let mut held = VecDeque::new();
                let mut kept = VecDeque::new();
                while !stop.load(Ordering::SeqCst) {
                    {
                        let (flag, wake) = &*roused;
                        let mut set = flag.lock().expect("unpoisoned");
                        while !*set && !stop.load(Ordering::SeqCst) {
                            set = wake.wait(set).expect("unpoisoned");
                        }
                        *set = false;
                    }
                    desk.begin_turn();
                    while let Some(taken) = desk.take() {
                        held.push_back(taken);
                        let mut state = host.state.lock().expect("unpoisoned");
                        if state.paused {
                            state.held = held.len();
                            drop(state);
                            host.changed.notify_all();
                            continue;
                        }
                        drop(state);
                        host.serve(&desk, &mut held, &mut kept);
                    }
                    if !host.state.lock().expect("unpoisoned").paused {
                        host.serve(&desk, &mut held, &mut kept);
                    }
                }
            })
        };
        Self {
            host,
            desk,
            threads: Threads::default(),
            stop,
            roused,
            probe,
            display: Some(display),
        }
    }

    /// Run `probe` as each order rouses the display end, or nothing.
    fn on_rouse(&self, probe: Option<Box<dyn Fn() + Send>>) {
        *self.probe.lock().expect("unpoisoned") = probe;
    }

    fn device(&self) -> Device {
        self.device_on(Arc::new(Still))
    }

    fn device_on(&self, clock: Arc<dyn MonotonicClock + Send + Sync>) -> Device {
        self.try_device_on(clock).expect("a device")
    }

    fn try_device_on(
        &self,
        clock: Arc<dyn MonotonicClock + Send + Sync>,
    ) -> Result<Device, DriverError> {
        device(
            Arc::clone(&self.desk),
            Arc::clone(&self.host.shared),
            &self.threads,
            clock,
        )
    }

    fn misbehave(&self, how: Misbehaviour) {
        self.host.state.lock().expect("unpoisoned").misbehaves = how;
    }

    /// Take orders without carrying them out until [`Self::resume`].
    fn pause(&self) {
        self.host.state.lock().expect("unpoisoned").paused = true;
    }

    /// Carry out the orders held while paused, in order, and go on.
    fn resume(&self) {
        self.host.state.lock().expect("unpoisoned").paused = false;
        self.rouse();
    }

    /// Carry out what is submitted on `ring` but answer it only once
    /// [`Self::let_ring_go`].
    fn withhold(&self, ring: u8) {
        self.host.state.lock().expect("unpoisoned").withheld = Some(ring);
    }

    /// Give the answers the withheld ring kept back, in order, and keep none
    /// back again.
    fn let_ring_go(&self) {
        self.host.state.lock().expect("unpoisoned").withheld = None;
        self.rouse();
    }

    fn rouse(&self) {
        let (flag, wake) = &*self.roused;
        *flag.lock().expect("unpoisoned") = true;
        wake.notify_all();
    }

    /// Change what the host's physical device offers.
    fn offer(&self, change: impl FnOnce(&mut Offered)) {
        change(&mut self.host.state.lock().expect("unpoisoned").offered);
    }

    /// Wait until the host's state is as `until` says, failing past a budget
    /// no correct run comes near.
    fn wait_for(&self, until: impl Fn(&HostState) -> bool) {
        let state = self.host.state.lock().expect("unpoisoned");
        let (state, _) = self
            .host
            .changed
            .wait_timeout_while(state, std::time::Duration::from_secs(10), |state| {
                !until(state)
            })
            .expect("unpoisoned");
        // Let go before judging: a failed assertion must not poison the lock
        // the display end serves the desk under.
        let met = until(&state);
        drop(state);
        assert!(met, "the host never came to the state awaited");
    }

    /// Answer everything still with the desk, then stop.
    fn finish(mut self) -> Arc<Host> {
        self.desk.close();
        for thread in self.threads.0.lock().expect("unpoisoned").drain(..) {
            thread.join().expect("the answering thread ended");
        }
        {
            // Under the flag's lock, so the display end cannot miss it between
            // its check and its wait.
            let (flag, wake) = &*self.roused;
            let _held = flag.lock().expect("unpoisoned");
            self.stop.store(true, Ordering::SeqCst);
            wake.notify_all();
        }
        if let Some(display) = self.display.take() {
            display.join().expect("the display end ended");
        }
        Arc::clone(&self.host)
    }
}

fn context(device: &Device) -> tairix_gpu::Context {
    device
        .create_context(&ContextDesc::default())
        .expect("a context")
}

fn buffer(
    context: &tairix_gpu::Context,
    memory_type: u32,
    size: u64,
) -> (tairix_gpu::Allocation, tairix_gpu::Buffer) {
    buffer_for(
        context,
        (memory_type, size),
        BufferUsage::TRANSFER_SRC.with(BufferUsage::TRANSFER_DST),
    )
}

/// A buffer for every use a dispatch makes of one.
fn storage(
    context: &tairix_gpu::Context,
    memory_type: u32,
    size: u64,
) -> (tairix_gpu::Allocation, tairix_gpu::Buffer) {
    let usage = BufferUsage::STORAGE
        .with(BufferUsage::INDIRECT)
        .with(BufferUsage::TRANSFER_DST);
    buffer_for(context, (memory_type, size), usage)
}

fn buffer_for(
    context: &tairix_gpu::Context,
    (memory_type, size): (u32, u64),
    usage: BufferUsage,
) -> (tairix_gpu::Allocation, tairix_gpu::Buffer) {
    let memory = context
        .allocate(&AllocationDesc { size, memory_type })
        .expect("memory");
    let buffer = context
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

fn submit(
    context: &tairix_gpu::Context,
    commands: &tairix_gpu::CommandBuffer,
) -> Result<(), Error> {
    submit_by(context, commands, SECOND)
}

fn submit_by(
    context: &tairix_gpu::Context,
    commands: &tairix_gpu::CommandBuffer,
    deadline: Duration64,
) -> Result<(), Error> {
    queued(context, (0, 0), commands, deadline)?.wait(deadline)
}

/// `commands` submitted to queue `index` of the context's family `family`,
/// to end by `deadline`.
fn queued(
    context: &tairix_gpu::Context,
    (family, index): (u32, u32),
    commands: &tairix_gpu::CommandBuffer,
    deadline: Duration64,
) -> Result<tairix_gpu::Fence, Error> {
    let queue = context.queue(family, index).expect("a queue");
    queue
        .submit(&Submission {
            command_buffers: &[commands],
            waits: &[],
            signals: &[],
            deadline,
        })
        .map_err(|refused| refused.error)
}

/// That `kept` still runs work once another context of its device is lost.
fn goes_on(kept: &tairix_gpu::Context) {
    let (memory, target) = buffer(kept, 1, 64);
    let mut fill = kept.command_buffer();
    fill.fill_buffer(&target, 0, 64, 0x0303_0303)
        .expect("recorded");
    submit_by(kept, &fill, ANSWERED).expect("another context's work goes on");
    let mut seen = [0u8; 4];
    memory.read(0, &mut seen).expect("read");
    assert_eq!(seen, [3; 4]);
}

/// That everything the host made for the backend was let go.
fn let_go(host: &Host) {
    let state = host.state.lock().expect("unpoisoned");
    assert_eq!(state.contexts.iter().count(), 0, "every context ended");
    assert_eq!(state.blobs.iter().count(), 0, "every blob ended");
    assert!(
        !state.reached.contains(Unsent::FREED_EXPORTED),
        "no memory freed while a blob made it the device's"
    );
    assert_eq!(host.region.mapped(), 0, "no part left mapped");
    assert_eq!(
        host.shared
            .lock()
            .reserve(REGION_BYTES as u64)
            .map(|part| part.size())
            .ok(),
        Some(REGION_BYTES as u64),
        "every part given back"
    );
}

/// The pipeline of the pinned pass `spirv`'s entry point and its one set's
/// layout, laid out as the module states.
fn pass(
    context: &tairix_gpu::Context,
    spirv: &[u8],
) -> Result<(tairix_gpu::ComputePipeline, BindSetLayout), Error> {
    let admitted = Admitted::new(Module::decode(spirv).expect("decoded")).expect("admitted");
    let entry = admitted
        .entry_point("main", Stage::Compute)
        .expect("an entry point");
    let bindings: Vec<LayoutBinding> = entry
        .bindings()
        .iter()
        .map(|binding| LayoutBinding::unsampled(binding, Stages::COMPUTE).expect("unsampled"))
        .collect();
    let push = entry.push_constants().unwrap_or(0);
    let layout = context.create_bind_set_layout(&bindings)?;
    let laid_out = context.create_pipeline_layout(
        &[&layout],
        PushConstants {
            bytes: push,
            stages: Stages::COMPUTE,
        },
    )?;
    let module = context.create_shader_module(admitted)?;
    let pipeline = context.create_compute_pipeline(&ComputePipelineDesc {
        module: &module,
        entry_point: "main",
        layout: &laid_out,
        specialization: &[],
    })?;
    Ok((pipeline, layout))
}

/// The set binding `size` bytes of `surface` as a pass's target.
fn target(
    context: &tairix_gpu::Context,
    layout: &BindSetLayout,
    (surface, size): (&tairix_gpu::Buffer, u64),
) -> Result<BindSet, Error> {
    context.create_bind_set(
        layout,
        &[BindSetEntry {
            binding: fill::TARGET,
            resources: &[Resource::Buffer {
                buffer: surface,
                offset: 0,
                size,
            }],
        }],
    )
}

/// `spirv` lengthened past `len` bytes by a source extension, which a
/// module may carry whatever its text and which changes nothing it means.
fn lengthened(spirv: &[u8], len: usize) -> Vec<u8> {
    let module = Module::decode(spirv).expect("decoded");
    let text = "x".repeat(len);
    let mut out = tairix_spirv::Encoder::rewriting(&module).expect("an encoder");
    let mut extended = false;
    for instruction in module.instructions() {
        let before_debug = matches!(
            instruction.op(),
            Op::CAPABILITY
                | Op::EXTENSION
                | Op::EXT_INST_IMPORT
                | Op::MEMORY_MODEL
                | Op::ENTRY_POINT
                | Op::EXECUTION_MODE
                | Op::EXECUTION_MODE_ID
        );
        if !before_debug && !extended {
            out.emit(Op::SOURCE_EXTENSION, &[tairix_spirv::Arg::Text(&text)])
                .expect("emitted");
            extended = true;
        }
        out.copy(&module, instruction).expect("copied");
    }
    out.finish()
}

fn bytes_of(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// A surface `stride` words wide and `rows` high, `pixel` laid over the
/// rectangle at `left` and `top`, `width` by `height`, and nought elsewhere.
fn filled(
    (stride, rows): (u32, u32),
    (left, top, width, height): (u32, u32, u32, u32),
    pixel: u32,
) -> Vec<u8> {
    let words: Vec<u32> = (0..rows)
        .flat_map(|y| (0..stride).map(move |x| (x, y)))
        .map(|(x, y)| {
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            if inside {
                pixel
            } else {
                0
            }
        })
        .collect();
    bytes_of(&words)
}

/// Whether a pipeline just made has let its modules and its sets' layouts go,
/// as it does behind the context's work once made: no module is left.
fn pipeline_settled(state: &HostState) -> bool {
    state
        .contexts
        .iter()
        .flat_map(|(_, host)| host.objects.values())
        .all(|object| !matches!(object, Object::Module(_)))
}

/// The objects pipelines and sets are made of, across every context.
fn compute_objects(state: &HostState) -> usize {
    state
        .contexts
        .iter()
        .flat_map(|(_, host)| host.objects.values())
        .filter(|object| {
            matches!(
                object,
                Object::Module(_)
                    | Object::SetLayout(_)
                    | Object::PipelineLayout { .. }
                    | Object::Pipeline(_)
                    | Object::DescriptorPool { .. }
                    | Object::Set { .. }
            )
        })
        .count()
}

#[test]
fn the_device_states_coherent_memory_and_dispatches_and_is_made_robust() {
    let rig = Rig::new();
    let device = rig.device();
    let info = device.info();
    assert_eq!(info.queue_families.len(), 1);
    assert_eq!(info.queue_families[0].capabilities, QueueCapabilities::ALL);
    assert_eq!(info.queue_families[0].queues, 2);
    assert_eq!(info.spirv_version, Version { major: 1, minor: 6 });
    assert_eq!(
        (
            info.limits.storage_buffer_size_alignment,
            info.limits.uniform_buffer_size_alignment
        ),
        (ROBUST.storage, ROBUST.uniform)
    );
    let stated: Vec<MemoryProperties> = info
        .memory_types
        .iter()
        .map(|kind| kind.properties)
        .collect();
    assert_eq!(
        stated,
        [
            MemoryProperties::DEVICE_LOCAL,
            MemoryProperties::HOST_VISIBLE
                .with(MemoryProperties::HOST_COHERENT)
                .with(MemoryProperties::HOST_CACHED)
        ]
    );
    let context = context(&device);
    {
        let state = rig.host.state.lock().expect("unpoisoned");
        let enabled = state.enabled.as_ref().expect("a device made");
        assert_eq!(
            enabled.extensions,
            [
                extension::ROBUSTNESS2_KHR,
                extension::SAMPLER_MIRROR_CLAMP_TO_EDGE,
                extension::MAINTENANCE5,
                extension::DEPTH_RANGE_UNRESTRICTED
            ],
            "its lines strict and the rest core at Vulkan 1.3"
        );
        let members = |structure: Structure| {
            enabled
                .features
                .iter()
                .find(|(raw, _)| *raw == structure as i32)
                .map(|(_, members)| members.clone())
        };
        let core = members(Structure::PhysicalDeviceFeatures2).expect("chained");
        assert!(core[0], "robust buffer access");
        assert_eq!(
            members(Structure::PhysicalDeviceRobustness2Features),
            Some(vec![true, true, false]),
            "both robust accesses, no null descriptor"
        );
        assert_eq!(
            members(Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures),
            Some(vec![true])
        );
    }
    drop((context, device));
    let_go(&rig.finish());
}

#[test]
fn a_host_short_of_the_robustness_the_seam_s_semantics_need_serves_nothing() {
    let shorts: [fn(&mut Offered); 2] = [
        |offered| {
            offered
                .extensions
                .retain(|name| !name.contains("robustness2"));
        },
        |offered| {
            offered.withheld.push((
                Structure::PhysicalDeviceZeroInitializeWorkgroupMemoryFeatures,
                0,
            ));
        },
    ];
    for short in shorts {
        let rig = Rig::new();
        rig.offer(short);
        assert_eq!(
            rig.try_device_on(Arc::new(Still)).err(),
            Some(DriverError::Unsupported)
        );
        let_go(&rig.finish());
    }
}

#[test]
fn a_dispatch_runs_on_the_host_through_the_memory_its_set_binds() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let baseline = compute_objects(&rig.host.state.lock().expect("unpoisoned"));
    let (stride, rows) = (64, 48);
    let size = u64::from(stride * rows * 4);
    let (memory, surface) = storage(&context, 1, size);
    let (pipeline, layout) = pass(&context, fill::SPIRV).expect("a pipeline");
    let set = target(&context, &layout, (&surface, size)).expect("a set");
    let push = fill::Push {
        stride,
        left: 5,
        top: 3,
        width: 40,
        height: 30,
        pixel: 0x8040_20ff,
    };
    let mut commands = context.command_buffer();
    commands
        .dispatch(
            &pipeline,
            &[&set],
            &bytes_of(&push.words()),
            tairix_shaders::groups(40, 30),
        )
        .expect("recorded");
    submit(&context, &commands).expect("ran");
    let mut seen = vec![0u8; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    assert_eq!(seen, filled((stride, rows), (5, 3, 40, 30), push.pixel));
    let dispatched = rig.host.state.lock().expect("unpoisoned").dispatched;
    assert_eq!(dispatched, 1);
    drop((commands, set, layout, pipeline));
    rig.wait_for(|state| compute_objects(state) == baseline);
    drop((surface, memory, context, device));
    let_go(&rig.finish());
}

#[test]
fn an_indirect_dispatch_runs_the_counts_it_reads_and_loses_its_context_past_the_device_s() {
    let rig = Rig::new();
    let device = rig.device();
    let kept = context(&device);
    let context = context(&device);
    let (stride, rows) = (64, 48);
    let size = u64::from(stride * rows * 4);
    let (memory, surface) = storage(&context, 1, size);
    let (counts_memory, counts) = storage(&context, 1, 64);
    let (pipeline, layout) = pass(&context, fill::SPIRV).expect("a pipeline");
    let set = target(&context, &layout, (&surface, size)).expect("a set");
    let push = bytes_of(
        &fill::Push {
            stride,
            left: 0,
            top: 0,
            width: stride,
            height: rows,
            pixel: 0x0102_0304,
        }
        .words(),
    );
    counts_memory
        .write(16, &bytes_of(&tairix_shaders::groups(stride, rows)))
        .expect("written");
    let mut commands = context.command_buffer();
    commands
        .dispatch_indirect(&pipeline, &[&set], &push, (&counts, 16))
        .expect("recorded");
    submit(&context, &commands).expect("ran");
    let mut seen = vec![0u8; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    assert_eq!(
        seen,
        filled((stride, rows), (0, 0, stride, rows), 0x0102_0304)
    );

    counts_memory
        .write(16, &bytes_of(&[MAX_GROUPS + 1, 1, 1]))
        .expect("written");
    let mut past = context.command_buffer();
    past.dispatch_indirect(&pipeline, &[&set], &push, (&counts, 16))
        .expect("recorded");
    assert!(matches!(
        submit(&context, &past),
        Err(Error::Lost(Loss::Fault))
    ));
    assert!(matches!(context.status(), Err(Error::Lost(_))));
    let overdispatched = rig
        .host
        .state
        .lock()
        .expect("unpoisoned")
        .reached
        .contains(Unsent::OVERDISPATCHED);
    assert!(!overdispatched, "the device never ran past its counts");
    rig.wait_for(|state| state.contexts.iter().count() == 1);
    goes_on(&kept);
    drop((
        commands,
        past,
        set,
        layout,
        pipeline,
        counts,
        counts_memory,
        surface,
        memory,
        context,
        kept,
        device,
    ));
    let_go(&rig.finish());
}

#[test]
fn a_module_longer_than_the_exchange_area_travels_whole_in_a_blob_of_its_own() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let spirv = lengthened(fill::SPIRV, EXCHANGE_LEN);
    // What the host is sent: the module folded, every value it could read
    // unwritten defined, and guarded.
    let admitted = Admitted::new(Module::decode(&spirv).expect("decoded")).expect("admitted");
    let specialized = admitted.specialize(&[]).expect("specialized");
    let folded = admitted.fold(&specialized).expect("folded");
    let sent = folded.guarded().expect("guarded");
    let sent = sent.admitted().module().bytes();
    assert!(sent.len() > EXCHANGE_LEN, "longer than the exchange area");
    let held = |state: &HostState| {
        state
            .contexts
            .iter()
            .flat_map(|(_, host)| host.objects.values())
            .filter(|object| {
                matches!(object, Object::Pipeline(program) if program.module.module().bytes() == sent)
            })
            .count()
    };
    let (pipeline, layout) = pass(&context, &spirv).expect("a pipeline");
    let made = held(&rig.host.state.lock().expect("unpoisoned"));
    assert_eq!(made, 1);
    drop((pipeline, layout));
    rig.wait_for(|state| held(state) == 0);
    drop((context, device));
    let_go(&rig.finish());
}

/// A kernel reading a 16-bit push constant through a dynamic 16-bit index, a
/// module declaring only that it stores 16-bit values there.
const NARROW: &str = r#"OpCapability Shader
OpCapability StoragePushConstant16
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %pc %out
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %push Block
OpMemberDecorate %push 0 Offset 0
OpMemberDecorate %push 1 Offset 8
OpMemberDecorate %push 2 Offset 10
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
%push = OpTypeStruct %halves %ushort %ushort
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
"#;

#[test]
fn the_host_is_sent_its_module_guarded_and_a_guard_it_cannot_run_is_refused() {
    let spirv = tairix_spirv::assemble::assemble(NARROW, Version::NEWEST);
    let admitted = Admitted::new(Module::decode(&spirv).expect("decoded")).expect("admitted");
    let specialized = admitted.specialize(&[]).expect("specialized");
    let folded = admitted.fold(&specialized).expect("folded");
    let unguarded = folded.module().bytes().to_vec();
    let guarded = folded.guarded().expect("guarded");
    let sent = guarded.admitted().module().bytes();
    assert_ne!(
        sent,
        unguarded.as_slice(),
        "its push constant indexed through a clamp"
    );
    let selects_halves = guarded
        .admitted()
        .capabilities()
        .binary_search(&tairix_spirv::enumerant::Capability::FLOAT16)
        .is_ok();
    assert!(selects_halves, "the guard's own capability declared");
    let rig = Rig::new();
    let device = rig.device();
    let made = context(&device);
    let (pipeline, layout) = pass(&made, &spirv).expect("a pipeline");
    let held = rig
        .host
        .state
        .lock()
        .expect("unpoisoned")
        .contexts
        .iter()
        .flat_map(|(_, host)| host.objects.values())
        .filter(|object| {
            matches!(object, Object::Pipeline(program) if program.module.module().bytes() == sent)
        })
        .count();
    assert_eq!(held, 1, "the guarded module is the one the host made");
    drop((pipeline, layout, made, device));
    let_go(&rig.finish());

    let rig = Rig::new();
    rig.offer(|offered| {
        offered
            .withheld
            .push((Structure::PhysicalDeviceShaderFloat16Int8Features, 0));
    });
    let device = rig.device();
    let context = context(&device);
    assert!(matches!(pass(&context, &spirv), Err(Error::Unsupported)));
    drop((context, device));
    let_go(&rig.finish());
}

#[test]
fn a_pipeline_or_set_the_host_refuses_is_answered_so_and_leaves_nothing_behind() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let baseline = compute_objects(&rig.host.state.lock().expect("unpoisoned"));
    let size = 64 * 64 * 4;
    let (memory, surface) = storage(&context, 1, size);
    rig.misbehave(Misbehaviour::RefusePipelines);
    assert!(matches!(
        pass(&context, fill::SPIRV),
        Err(Error::OutOfMemory)
    ));
    rig.misbehave(Misbehaviour::Nothing);
    rig.wait_for(|state| compute_objects(state) == baseline);
    let (pipeline, layout) = pass(&context, fill::SPIRV).expect("a pipeline");
    rig.wait_for(pipeline_settled);
    let made = compute_objects(&rig.host.state.lock().expect("unpoisoned"));
    rig.misbehave(Misbehaviour::RefuseSets);
    assert!(matches!(
        target(&context, &layout, (&surface, size)),
        Err(Error::OutOfMemory)
    ));
    rig.misbehave(Misbehaviour::Nothing);
    rig.wait_for(|state| compute_objects(state) == made);
    let set = target(&context, &layout, (&surface, size)).expect("a set");
    let push = fill::Push {
        stride: 64,
        left: 0,
        top: 0,
        width: 8,
        height: 8,
        pixel: 9,
    };
    let mut commands = context.command_buffer();
    commands
        .dispatch(
            &pipeline,
            &[&set],
            &bytes_of(&push.words()),
            tairix_shaders::groups(8, 8),
        )
        .expect("recorded");
    submit(&context, &commands).expect("the context goes on");
    let mut seen = vec![0u8; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    assert_eq!(seen, filled((64, 64), (0, 0, 8, 8), 9));
    drop((
        commands, set, layout, pipeline, surface, memory, context, device,
    ));
    let_go(&rig.finish());
}

#[test]
fn host_visible_memory_is_zeroed_written_and_read_through_the_driver() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (memory, whole) = buffer(&context, 1, 4096 + 12);
    let mut seen = vec![0xffu8; 4096 + 12];
    memory.read(0, &mut seen).expect("read");
    assert!(
        seen.iter().all(|&byte| byte == 0),
        "zeroed: the host's bytes never reach the client"
    );
    memory.write(100, &[1, 2, 3, 4]).expect("written");
    let mut back = [0u8; 4];
    memory.read(100, &mut back).expect("read");
    assert_eq!(back, [1, 2, 3, 4]);
    drop((memory, whole, context, device));
    let_go(&rig.finish());
}

#[test]
fn copies_fills_and_updates_land_through_device_local_memory() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (visible, staging) = buffer(&context, 1, 256);
    let (local, deep) = buffer(&context, 0, 256);
    visible.write(0, &[7; 64]).expect("written");
    let mut commands = context.command_buffer();
    commands
        .copy_buffer(
            &staging,
            &deep,
            &[CopyRegion {
                src_offset: 0,
                dst_offset: 0,
                size: 64,
            }],
        )
        .expect("recorded");
    commands
        .fill_buffer(&deep, 64, 64, 0x0101_0101)
        .expect("recorded");
    commands
        .update_buffer(&deep, 128, &[9; 16])
        .expect("recorded");
    commands
        .copy_buffer(
            &deep,
            &staging,
            &[CopyRegion {
                src_offset: 0,
                dst_offset: 0,
                size: 256,
            }],
        )
        .expect("recorded");
    submit(&context, &commands).expect("ran");
    let mut seen = vec![0u8; 256];
    visible.read(0, &mut seen).expect("read");
    assert_eq!(&seen[..64], &[7; 64]);
    assert_eq!(&seen[64..128], &[1; 64]);
    assert_eq!(&seen[128..144], &[9; 16]);
    assert!(
        seen[144..].iter().all(|&byte| byte == 0),
        "the device-local memory was zeroed"
    );
    drop((commands, visible, staging, local, deep, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_host_s_copy_engine_runs_the_transfers_queued_on_it_beside_the_first_family() {
    let rig = Rig::new();
    rig.offer(|offered| offered.copy_engine = Some((2, [1; 3])));
    let device = rig.device();
    assert_eq!(
        device.info().queue_families.get(1),
        Some(&QueueFamily {
            capabilities: QueueCapabilities::TRANSFER,
            queues: 2,
            granularity: Granularity::TEXEL,
        })
    );
    let context = context(&device);
    let (visible, staging) = buffer(&context, 1, 256);
    let (local, deep) = buffer(&context, 0, 256);
    visible.write(0, &[7; 64]).expect("written");
    let mut moved = context.command_buffer();
    let copy = |src_offset, dst_offset, size| CopyRegion {
        src_offset,
        dst_offset,
        size,
    };
    moved
        .copy_buffer(&staging, &deep, &[copy(0, 0, 64)])
        .expect("recorded");
    moved
        .fill_buffer(&deep, 64, 64, 0x0101_0101)
        .expect("recorded");
    moved
        .copy_buffer(&deep, &staging, &[copy(0, 128, 128)])
        .expect("recorded");
    for index in 0..2 {
        queued(&context, (1, index), &moved, SECOND)
            .and_then(|fence| fence.wait(SECOND))
            .expect("run on the copy engine's queue, from its pool");
    }
    let mut seen = vec![0u8; 256];
    visible.read(0, &mut seen).expect("read");
    assert_eq!(&seen[128..192], &[7; 64]);
    assert_eq!(&seen[192..], &[1; 64]);
    let mut cleared = context.command_buffer();
    cleared.fill_buffer(&staging, 0, 256, 0).expect("recorded");
    queued(&context, (0, 0), &cleared, SECOND)
        .and_then(|fence| fence.wait(SECOND))
        .expect("the first family's queue still runs its own");
    drop((moved, cleared, visible, staging, local, deep, context));
    drop(device);
    let_go(&rig.finish());
}

#[test]
fn a_job_longer_than_a_part_travels_in_parts_and_lands_whole() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let size = 4 * MAX_UPDATE_BYTES as u64;
    let (memory, target) = buffer(&context, 1, size);
    let mut commands = context.command_buffer();
    for part in 0..4u8 {
        let offset = u64::from(part) * MAX_UPDATE_BYTES as u64;
        commands
            .update_buffer(&target, offset, &vec![part + 1; MAX_UPDATE_BYTES])
            .expect("recorded");
    }
    submit(&context, &commands).expect("ran");
    let mut seen = vec![0u8; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    for (part, chunk) in seen.chunks(MAX_UPDATE_BYTES).enumerate() {
        assert!(
            chunk.iter().all(|&byte| usize::from(byte) == part + 1),
            "part {part} landed"
        );
    }
    drop((commands, memory, target, context, device));
    let_go(&rig.finish());
}

#[test]
fn a_job_the_host_refuses_loses_its_context_alone_and_everything_is_let_go() {
    let rig = Rig::new();
    let device = rig.device();
    let refused = context(&device);
    let kept = context(&device);
    let (memory, target) = buffer(&refused, 1, 64);
    let mut commands = refused.command_buffer();
    commands.fill_buffer(&target, 0, 64, 1).expect("recorded");
    rig.misbehave(Misbehaviour::RefuseSubmits);
    assert!(matches!(submit(&refused, &commands), Err(Error::Lost(_))));
    assert!(matches!(refused.status(), Err(Error::Lost(_))));
    rig.misbehave(Misbehaviour::Nothing);
    let (other, aim) = buffer(&kept, 1, 64);
    let mut fill = kept.command_buffer();
    fill.fill_buffer(&aim, 0, 64, 0x0202_0202)
        .expect("recorded");
    submit(&kept, &fill).expect("another context's work goes on");
    let mut seen = [0u8; 4];
    other.read(0, &mut seen).expect("read");
    assert_eq!(seen, [2; 4]);
    drop((
        commands, memory, target, refused, fill, other, aim, kept, device,
    ));
    let_go(&rig.finish());
}

#[test]
fn a_lost_context_answers_the_work_it_still_holds_and_ends_while_held() {
    let rig = Rig::new();
    let device = rig.device_on(Arc::new(Wall(std::time::Instant::now())));
    let kept = context(&device);
    let refused = context(&device);
    // More parts than the job area holds, so the last still waits when the
    // first is refused.
    let size = 4 * MAX_UPDATE_BYTES as u64;
    let (memory, target) = buffer(&refused, 1, size);
    let mut commands = refused.command_buffer();
    for part in 0..4u8 {
        let offset = u64::from(part) * MAX_UPDATE_BYTES as u64;
        commands
            .update_buffer(&target, offset, &vec![part; MAX_UPDATE_BYTES])
            .expect("recorded");
    }
    rig.misbehave(Misbehaviour::RefuseSubmits);
    let answered = submit_by(&refused, &commands, ANSWERED);
    rig.misbehave(Misbehaviour::Nothing);
    assert!(matches!(answered, Err(Error::Lost(_))), "{answered:?}");
    rig.wait_for(|state| state.contexts.iter().count() == 1);
    goes_on(&kept);
    drop((commands, memory, target, refused, kept, device));
    let_go(&rig.finish());
}

#[test]
fn a_job_whose_number_never_lands_loses_its_context_alone() {
    let rig = Rig::new();
    let device = rig.device_on(Arc::new(Wall(std::time::Instant::now())));
    let kept = context(&device);
    let silent = context(&device);
    let (memory, target) = buffer(&silent, 1, 64);
    let mut commands = silent.command_buffer();
    commands.fill_buffer(&target, 0, 64, 1).expect("recorded");
    rig.misbehave(Misbehaviour::IgnoreWork);
    let answered = submit_by(&silent, &commands, ANSWERED);
    rig.misbehave(Misbehaviour::Nothing);
    assert!(matches!(answered, Err(Error::Lost(_))), "{answered:?}");
    rig.wait_for(|state| state.contexts.iter().count() == 1);
    goes_on(&kept);
    drop((commands, memory, target, silent, kept, device));
    let_go(&rig.finish());
}

#[test]
fn a_job_answered_late_keeps_its_slot_while_more_jobs_than_slots_end_on_another_queue() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (late_memory, late_target) = buffer(&context, 1, 64);
    let (_, quick_target) = buffer(&context, 1, 64);
    let mut late = context.command_buffer();
    late.fill_buffer(&late_target, 0, 64, 0x0505_0505)
        .expect("recorded");
    let mut quick = context.command_buffer();
    quick
        .fill_buffer(&quick_target, 0, 64, 0x0606_0606)
        .expect("recorded");
    rig.withhold(super::ring(0).expect("a ring"));
    let answered_late = queued(&context, (0, 0), &late, SECOND).expect("submitted");
    for _ in 0..FEEDBACK_SLOTS {
        queued(&context, (0, 1), &quick, SECOND)
            .and_then(|fence| fence.wait(SECOND))
            .expect("another queue's job ran");
    }
    rig.let_ring_go();
    answered_late
        .wait(SECOND)
        .expect("judged by the slot it holds, not one another job took");
    let mut seen = [0u8; 4];
    late_memory.read(0, &mut seen).expect("read");
    assert_eq!(seen, [5; 4]);
    drop((late, quick, late_memory, late_target, quick_target, context));
    drop(device);
    let_go(&rig.finish());
}

#[test]
fn a_zero_fill_answered_late_holds_its_slot_until_its_number_is_read() {
    let rig = Rig::new();
    let core = super::bring_up(
        Arc::clone(&rig.desk),
        Arc::clone(&rig.host.shared),
        &rig.threads,
    )
    .expect("the host served");
    let context = Arc::new(super::make_context(&core).expect("a context"));
    let first = super::allocate(&context, 0, 4096).expect("device-local memory");
    let second = super::allocate(&context, 0, 4096).expect("device-local memory");
    let zero = |memory: &super::VenusMemory| {
        let (context, buffer) = (Arc::clone(&context), memory.buffer);
        std::thread::spawn(move || super::zero_device(&context, buffer))
    };
    rig.withhold(super::ring(0).expect("a ring"));
    let late = zero(&first);
    rig.wait_for(|state| state.kept == 1);
    assert!(
        context.utility.try_lock().is_none(),
        "no other zero-fill marks or reads the slot before its number is read"
    );
    let next = zero(&second);
    rig.let_ring_go();
    late.join()
        .expect("joined")
        .expect("judged by its own number");
    next.join().expect("joined").expect("zeroed in its turn");
    drop((first, second, context, core));
    let_go(&rig.finish());
}

#[test]
fn a_blob_the_device_would_not_unmap_keeps_its_part_and_its_memory() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let (memory, whole) = buffer(&context, 1, 4096);
    rig.misbehave(Misbehaviour::RefuseUnmaps);
    drop((memory, whole));
    rig.wait_for(|state| state.unmaps_refused == 1);
    rig.misbehave(Misbehaviour::Nothing);
    drop((context, device));
    let host = rig.finish();
    let state = host.state.lock().expect("unpoisoned");
    assert_eq!(state.contexts.iter().count(), 0, "every context ended");
    assert_eq!(state.blobs.iter().count(), 1, "the blob still mapped stays");
    assert!(
        !state.reached.contains(Unsent::FREED_EXPORTED),
        "its memory is never freed under it"
    );
    assert_eq!(host.region.mapped(), 0, "no window left mapped");
    assert!(
        host.shared.lock().reserve(REGION_BYTES as u64).is_err(),
        "the part it may still hold is kept from every other"
    );
}

#[test]
fn memory_whose_blob_may_be_mapped_is_kept_with_it() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    rig.misbehave(Misbehaviour::DoubtMaps);
    let refused = context.allocate(&AllocationDesc {
        size: 4096,
        memory_type: 1,
    });
    rig.misbehave(Misbehaviour::Nothing);
    assert!(refused.is_err(), "memory not mapped is not answered");
    drop((context, device));
    let host = rig.finish();
    let state = host.state.lock().expect("unpoisoned");
    assert_eq!(state.contexts.iter().count(), 0, "every context ended");
    assert_eq!(state.blobs.iter().count(), 1, "the blob it may map stays");
    assert!(
        !state.reached.contains(Unsent::FREED_EXPORTED),
        "its memory is never freed under it"
    );
}

#[test]
fn nothing_joins_the_queue_of_a_context_lost_or_ended() {
    let mut state = ContextState {
        next_id: 1,
        spans: tairix_collections::RangeMap::new(),
        free_commands: Default::default(),
        pending: VecDeque::new(),
        in_flight: 0,
        next_seq: 0,
        lost: false,
        ended: false,
        waiting: None,
    };
    assert_eq!(state.admits(), Admits::Room);
    state.lost = true;
    assert_eq!(state.admits(), Admits::Gone);
    state.lost = false;
    state.ended = true;
    assert_eq!(state.admits(), Admits::Gone);
}

#[test]
fn a_set_whose_making_is_refused_part_way_lets_go_what_was_made() {
    // The set's layout and pool are made by the first exchange; the second
    // allocates the set and the third writes it.
    for refused_at in [1, 2] {
        let rig = Rig::new();
        let device = rig.device();
        let context = context(&device);
        let size = 64 * 4;
        let (memory, surface) = storage(&context, 1, size);
        let (pipeline, layout) = pass(&context, fill::SPIRV).expect("a pipeline");
        rig.wait_for(pipeline_settled);
        let made = compute_objects(&rig.host.state.lock().expect("unpoisoned"));
        rig.host.state.lock().expect("unpoisoned").refuse_submit_in = Some(refused_at);
        assert!(matches!(
            target(&context, &layout, (&surface, size)),
            Err(Error::OutOfMemory)
        ));
        rig.wait_for(|state| compute_objects(state) == made);
        let set = target(&context, &layout, (&surface, size)).expect("the context goes on");
        let push = fill::Push {
            stride: 8,
            left: 0,
            top: 0,
            width: 8,
            height: 8,
            pixel: 5,
        };
        let mut commands = context.command_buffer();
        commands
            .dispatch(
                &pipeline,
                &[&set],
                &bytes_of(&push.words()),
                tairix_shaders::groups(8, 8),
            )
            .expect("recorded");
        submit(&context, &commands).expect("ran");
        let mut seen = vec![0u8; usize::try_from(size).expect("small")];
        memory.read(0, &mut seen).expect("read");
        assert_eq!(
            seen,
            filled((8, 8), (0, 0, 8, 8), 5),
            "refused at {refused_at}"
        );
        drop((
            commands, set, layout, pipeline, surface, memory, context, device,
        ));
        let_go(&rig.finish());
    }
}

#[test]
fn a_blob_no_part_is_left_for_is_ended_with_the_shared_memory_let_go() {
    let rig = Rig::new();
    let device = rig.device();
    // Runs halving down to a page: a page at a time would cost a placement
    // per page, each walking every part already held.
    let mut taken = Vec::new();
    let mut run = REGION_BYTES as u64;
    while run >= PAGE_SIZE as u64 {
        match rig.host.shared.lock().reserve(run) {
            Ok(part) => taken.push(part),
            Err(_) => run /= 2,
        }
    }
    {
        let mut making = Making::new(&rig.desk, &rig.host.shared);
        let context = making.context().expect("a context");
        assert!(making.mapped(context, SHARED_SPEC).is_err());
        assert!(making.let_go(), "every blob ended");
    }
    let held = rig
        .host
        .state
        .lock()
        .expect("unpoisoned")
        .served_while_shared_held;
    assert_eq!(held, 0, "the display end maps parts through the memory");
    for part in taken {
        assert!(rig.host.shared.lock().release(part).is_ok());
    }
    drop(device);
    let_go(&rig.finish());
}

#[test]
fn a_refusal_is_answered_as_the_seam_names_its_kind() {
    use crate::venus::backend::device_error;
    assert_eq!(device_error(DriverError::Unsupported), Error::Unsupported);
    assert_eq!(device_error(DriverError::NotFound), Error::Unsupported);
    for fault in [DriverError::DeviceFault, DriverError::DeviceOffline] {
        assert_eq!(device_error(fault), Error::Lost(Loss::Fault));
    }
    for past in [DriverError::OutOfRange, DriverError::LengthOutOfRange] {
        assert_eq!(device_error(past), Error::TooLarge, "{past:?}");
    }
    for short in [DriverError::OutOfMemory, DriverError::NoSpace] {
        assert_eq!(device_error(short), Error::OutOfMemory, "{short:?}");
    }
}

#[test]
fn a_waited_exchange_is_released_by_its_context_s_loss_and_a_lost_one_asks_nothing() {
    let rig = Rig::new();
    let core = bring_up(
        Arc::clone(&rig.desk),
        Arc::clone(&rig.host.shared),
        &rig.threads,
    )
    .expect("brought up");
    let context = Arc::new(make_context(&core).expect("a context"));
    // Queue 0's ring answers nothing, as a queue behind a dispatch the host
    // cannot stop.
    rig.withhold(ring(0).expect("a ring"));
    let (sent, answered) = std::sync::mpsc::channel();
    let waiting = {
        let context = Arc::clone(&context);
        std::thread::spawn(move || {
            let _ = sent.send(utility(&context, |_| Ok(())));
        })
    };
    rig.wait_for(|state| state.kept == 1);
    lose(&context);
    let released = answered.recv_timeout(std::time::Duration::from_secs(10));
    rig.let_ring_go();
    waiting.join().expect("the waiting thread ended");
    assert_eq!(
        released.expect("answered once its context was lost"),
        Err(DriverError::DeviceOffline)
    );
    let submits = rig.host.state.lock().expect("unpoisoned").submits;
    assert_eq!(
        utility(&context, |_| Ok(())),
        Err(DriverError::DeviceOffline)
    );
    let sent = rig.host.state.lock().expect("unpoisoned").submits;
    assert_eq!(sent, submits, "a lost context asks nothing");
    drop((context, core));
    let_go(&rig.finish());
}

#[test]
fn an_exchange_is_ordered_under_the_lock_its_context_s_loss_is_marked_under() {
    let rig = Rig::new();
    let core = bring_up(
        Arc::clone(&rig.desk),
        Arc::clone(&rig.host.shared),
        &rig.threads,
    )
    .expect("brought up");
    let context = Arc::new(make_context(&core).expect("a context"));
    let held = Arc::new(StdMutex::new(Vec::new()));
    {
        let (context, held) = (Arc::downgrade(&context), Arc::clone(&held));
        let asking = std::thread::current().id();
        // The answering thread gives an owed rouse again, ordering nothing.
        rig.on_rouse(Some(Box::new(move || {
            if std::thread::current().id() != asking {
                return;
            }
            if let Some(context) = context.upgrade() {
                let locked = context.state.try_lock().is_none();
                held.lock().expect("unpoisoned").push(locked);
            }
        })));
    }
    // So the turn's one order, its exchange's, rouses the display end.
    rig.desk.begin_turn();
    assert_eq!(utility(&context, |_| Ok(())), Ok(()));
    rig.on_rouse(None);
    let held = held.lock().expect("unpoisoned").clone();
    assert_eq!(
        held,
        [true],
        "so no loss comes between finding the context live and ordering"
    );
    drop((context, core));
    let_go(&rig.finish());
}

#[test]
fn a_waiter_keeps_the_first_answer_it_is_given() {
    let waiter = super::Waiter::default();
    waiter.give(Answer::Refused(DriverError::DeviceOffline, None));
    waiter.give(Answer::Done);
    assert!(matches!(
        waiter.wait(),
        Answer::Refused(DriverError::DeviceOffline, None)
    ));
    waiter.give(Answer::Done);
    assert!(
        matches!(waiter.wait(), Answer::Done),
        "once taken, the next"
    );
}

#[test]
fn an_exchange_that_cannot_hold_its_replies_sends_nothing() {
    let rig = Rig::new();
    let device = rig.device();
    {
        let mut making = Making::new(&rig.desk, &rig.host.shared);
        let context = making.context().expect("a context");
        let mapped = making.mapped(context, SHARED_SPEC).expect("mapped");
        let at = Channel {
            desk: &rig.desk,
            shared: &rig.host.shared,
            context,
            resource: mapped.blob.resource_id().expect("a resource"),
            area: Area::Owned(mapped),
            owner: None,
        };
        let submits = rig.host.state.lock().expect("unpoisoned").submits;
        let seek = payload(|encoder| encoder.seek_reply_command_stream(0)).expect("a payload");
        assert_eq!(
            submit_and_read(at, (0, seek), (at.area, 0, usize::MAX)),
            Err(DriverError::OutOfMemory)
        );
        let sent = rig.host.state.lock().expect("unpoisoned").submits;
        assert_eq!(sent, submits, "nothing was sent");
        assert!(making.let_go(), "every blob ended");
    }
    drop(device);
    let_go(&rig.finish());
}

/// The bytes `encode` writes.
fn stream_len(encode: impl FnOnce(&mut Encoder) -> Result<(), DriverError>) -> u64 {
    let mut encoder = Encoder::default();
    encode(&mut encoder).expect("encoded");
    encoder.as_bytes().len() as u64
}

#[test]
fn a_job_s_stream_is_no_longer_than_its_bound() {
    let commands = 99;
    let job = stream_len(|e| {
        e.allocate_command_buffers(DEVICE, POOL, &[commands])?;
        e.begin_command_buffer(commands, crate::venus::ONE_TIME_SUBMIT)?;
        finish_commands(e, commands, (1, 1))?;
        e.queue_submit(FIRST_QUEUE, &[commands])
    });
    assert!(job <= stream::JOB, "{job}");
    let region = crate::venus::encode::CopyRegion {
        src: 0,
        dst: 0,
        size: 4,
    };
    for count in [0, 1, 8] {
        let regions = vec![region; count];
        let copy = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_copy_buffer(commands, (1, 2), regions.iter().copied())
        });
        assert!(
            copy <= stream::COMMAND + count as u64 * stream::REGION,
            "{count} regions"
        );
    }
    for len in [0, 4, 1024] {
        let update = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_update_buffer(commands, 1, 0, &vec![0; len])
        });
        assert!(update <= stream::COMMAND + len as u64, "{len} bytes");
    }
    let fill = stream_len(|e| {
        e.cmd_barrier(commands)?;
        e.cmd_fill_buffer(commands, 1, (0, 4), 0)
    });
    assert!(fill <= stream::COMMAND);
    for (sets, words) in [(0, 0), (1, 6), (4, 1024)] {
        let bound: Vec<u64> = (0..sets).collect();
        let push = vec![0u32; words];
        let dispatched = stream_len(|e| {
            e.cmd_barrier(commands)?;
            bind_ids(e, commands, (1, 2, stage::COMPUTE), (&bound, &push))?;
            e.cmd_dispatch(commands, [1, 1, 1])
        });
        let own = stream::COMMAND
            + stream::DISPATCH
            + sets * stream::HANDLE
            + words as u64 * stream::WORD;
        assert!(dispatched <= own, "{sets} sets and {words} words");
        let checked = stream_len(|e| {
            e.cmd_barrier(commands)?;
            check_counts(e, commands, (1, 0), (1, [65535; 3]))?;
            bind_ids(e, commands, (1, 2, stage::COMPUTE), (&bound, &push))?;
            e.cmd_dispatch_indirect(commands, 3, 0)
        });
        assert!(
            checked <= own + stream::CHECK,
            "indirect, {sets} sets and {words} words"
        );
    }
}

#[test]
fn a_pass_s_stream_is_no_longer_than_its_bound() {
    use crate::venus::encode::{AttachmentInfo, DrawInfo, HostClear};
    use tairix_gpu::{
        MAX_DISPATCH_SETS, MAX_DRAW_VERTEX_BUFFERS, MAX_PASS_ATTACHMENTS, MAX_PUSH_BYTES,
    };

    let commands = 99;
    let cleared = AttachmentInfo {
        view: 7,
        resolve: Some(8),
        clear: Some(HostClear::Colour(2, [u32::MAX; 4])),
    };
    let depth = AttachmentInfo {
        view: 9,
        resolve: None,
        clear: Some(HostClear::DepthStencil(1.0, 255)),
    };
    let colour = vec![cleared; MAX_PASS_ATTACHMENTS];
    let instance = |e: &mut Encoder| {
        e.cmd_barrier(commands)?;
        e.cmd_begin_rendering(
            commands,
            [0, 0, 16384, 16384],
            (&colour, Some(depth), Some(depth)),
        )?;
        e.cmd_end_rendering(commands)
    };
    let attachments = MAX_PASS_ATTACHMENTS as u64 + 2;
    let instance_bound = stream::PASS + attachments * stream::ATTACHMENT;
    let instance_len = stream_len(instance);
    assert!(instance_len <= instance_bound, "{instance_len}");

    let sets: Vec<u64> = (0..MAX_DISPATCH_SETS as u64).collect();
    let push = vec![u32::MAX; MAX_PUSH_BYTES / 4];
    let buffers = vec![[u64::MAX; 3]; MAX_DRAW_VERTEX_BUFFERS];
    for indexed in [false, true] {
        let draw = stream_len(|e| {
            e.cmd_bind_pipeline(commands, crate::venus::BIND_POINT_GRAPHICS, 1)?;
            e.cmd_bind_sets(commands, (crate::venus::BIND_POINT_GRAPHICS, 2), &sets)?;
            e.cmd_push_constants(commands, (2, stage::VERTEX | stage::FRAGMENT), &push)?;
            e.cmd_bind_vertex_buffers(commands, &buffers)?;
            e.cmd_bind_index_buffer(commands, [u64::MAX; 3], true)?;
            e.cmd_set_viewport(commands, [0.0; 6])?;
            e.cmd_set_scissor(commands, [0, 0, 16384, 16384])?;
            e.cmd_set_stencil_reference(commands, 255)?;
            e.cmd_set_blend_constants(commands, [1.0; 4])?;
            e.cmd_draw(commands, DrawInfo::Indexed([u32::MAX; 3], -1, u32::MAX))?;
            e.cmd_draw_indirect(commands, indexed, (u64::MAX, u64::MAX), (u32::MAX, 20))
        });
        let bound = stream::DRAW
            + sets.len() as u64 * stream::HANDLE
            + push.len() as u64 * stream::WORD
            + buffers.len() as u64 * stream::VERTEX_BUFFER;
        assert!(draw <= bound, "{draw}, indexed: {indexed}");
    }

    let records = crate::venus::info::CHECKED_RECORDS;
    let regions = vec![
        crate::venus::encode::CopyRegion {
            src: u64::MAX,
            dst: u64::MAX,
            size: 20,
        };
        records as usize
    ];
    let checked = stream_len(|e| {
        e.cmd_copy_buffer(commands, (1, FEEDBACK), regions.iter().copied())?;
        e.cmd_barrier(commands)?;
        let push = tairix_shaders::indirect::records::Push {
            slot: FEEDBACK_SLOTS - 1,
            first: u32::MAX,
            count: records,
            indexed: true,
        };
        run_check(e, commands, &RECORDS_CHECK, &push.words())?;
        e.cmd_barrier(commands)?;
        instance(e)
    });
    let bound = stream::RECORDS + instance_bound + u64::from(records) * stream::REGION;
    assert!(checked <= bound, "{checked}");
    assert!(
        checked <= LONGEST_COMMAND,
        "a pass's longest piece fits a part"
    );
}

#[test]
fn an_image_command_s_stream_is_no_longer_than_its_bound() {
    let commands = 99;
    let layers = crate::venus::encode::Layers {
        aspect: 1,
        level: 0,
        base_layer: 0,
        count: 1,
    };
    let between = crate::venus::encode::BufferImageRegion {
        offset: 0,
        row_length: 0,
        image_height: 0,
        layers,
        at: [0; 3],
        extent: [1; 3],
    };
    let across = crate::venus::encode::ImageRegion {
        src: (layers, [0; 3]),
        dst: (layers, [0; 3]),
        extent: [1; 3],
    };
    for count in [0, 1, 8, tairix_gpu::MAX_COPY_REGIONS] {
        let regions = vec![between; count];
        let own = stream::COMMAND + count as u64 * stream::IMAGE_REGION;
        assert!(own <= super::LONGEST_COMMAND && own <= super::PART_MAX as u64);
        let into = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_copy_buffer_to_image(commands, (1, 2), &regions)
        });
        let out = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_copy_image_to_buffer(commands, (2, 1), &regions)
        });
        let copies = vec![across; count];
        let image = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_copy_image(commands, (2, 3), &copies)
        });
        assert!(
            into <= own && out <= own && image <= own,
            "{count} image regions"
        );
    }
    let range = crate::venus::encode::Subresources {
        aspects: 1,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
    };
    for count in [0, 1, 8] {
        let ranges = vec![range; count];
        let own = stream::COMMAND + (count as u64 + 1) * stream::RANGE;
        let colour = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_clear_color_image(commands, 2, (0, [0; 4]), &ranges)
        });
        let depth = stream_len(|e| {
            e.cmd_barrier(commands)?;
            e.cmd_clear_depth_stencil_image(commands, 2, (0.0, 0), &ranges)
        });
        assert!(colour <= own && depth <= own, "{count} ranges cleared");
    }
}

#[test]
fn an_exchange_waiting_on_the_host_holds_back_no_part_of_a_job() {
    let rig = Rig::new();
    let device = rig.device();
    let context = context(&device);
    let size = 4 * MAX_UPDATE_BYTES as u64;
    let (memory, written) = buffer(&context, 1, size);
    let (pipeline, layout) = pass(&context, fill::SPIRV).expect("a pipeline");
    let (_bound, surface) = storage(&context, 1, 256);
    let mut commands = context.command_buffer();
    for part in 0..4u8 {
        let offset = u64::from(part) * MAX_UPDATE_BYTES as u64;
        commands
            .update_buffer(&written, offset, &vec![part + 1; MAX_UPDATE_BYTES])
            .expect("recorded");
    }
    rig.pause();
    let fence = context
        .queue(0, 0)
        .expect("a queue")
        .submit(&Submission {
            command_buffers: &[&commands],
            waits: &[],
            signals: &[],
            deadline: SECOND,
        })
        .map_err(|refused| refused.error)
        .expect("submitted");
    // Three parts with the host and the last waiting for room among them.
    rig.wait_for(|state| state.held == 3);
    let (made, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let set = target(&context, &layout, (&surface, 256));
        let _ = made.send((set.is_ok(), context, layout, surface, set));
    });
    // The exchange's own order waits behind the parts.
    rig.wait_for(|state| state.held == 4);
    rig.resume();
    let Ok((ok, context, layout, surface, set)) =
        answered.recv_timeout(std::time::Duration::from_secs(10))
    else {
        // A deadlocked context can let nothing go, so fail without dropping.
        core::mem::forget((fence, commands, pipeline, written, memory, device, rig));
        panic!("the exchange was never answered while the job's last part waited");
    };
    assert!(ok);
    fence.wait(SECOND).expect("the job ran whole");
    let mut seen = vec![0u8; usize::try_from(size).expect("small")];
    memory.read(0, &mut seen).expect("read");
    for (part, chunk) in seen.chunks(MAX_UPDATE_BYTES).enumerate() {
        assert!(chunk.iter().all(|&byte| usize::from(byte) == part + 1));
    }
    drop((
        set, surface, layout, pipeline, commands, written, memory, context, device,
    ));
    let_go(&rig.finish());
}
