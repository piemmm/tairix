//! virtio-gpu 3D: Venus contexts and their fence rings, blob resources the
//! host makes and maps into the host-visible region, and command streams.
//!
//! Every 3D command is answered in its own time, as a [`Completion`] carrying
//! the tag its sender gave it: nothing waits on one, and its fence holds back
//! neither the display path, whose descriptors and slots it cannot take, nor
//! another command. An object the device makes is named only once its making
//! is answered, so no command depends on one the device may not have run.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::driver::RegionCaching;
use tairix_abi::{DriverError, PAGE_SIZE};
use tairix_collections::{Key, SlotKey, SlotMap};
use tairix_display::{BufferMemory, DeviceBuffer};
use tairix_drvrt::Recorder;
use tairix_virtio::{ChainSegment, Direction, Ticket, Transport};

use crate::wire::{self, BlobSpec, Request};
use crate::{
    said, AnswerIn, ConsoleSource, Layout, Record, Slot, VirtioGpu, BATCH_MAX, DISPLAY_DESCRIPTORS,
    RESPONSE_SLOT,
};

/// The features Venus needs: 3D commands, blob resources, and contexts
/// initialised for a capset.
pub(crate) const FEATURES: u64 =
    wire::FEATURE_VIRGL | wire::FEATURE_RESOURCE_BLOB | wire::FEATURE_CONTEXT_INIT;

/// The first resource id a blob takes: a presenter's resources keep below it.
pub(crate) const BLOB_RESOURCES: u32 = 0x1_0000;

/// The most capsets of a device's list the driver reads: a bound on what it
/// states.
const CAPSETS_MAX: u32 = 16;

/// The longest command stream one submission carries: a bound on what the
/// host is asked to copy at once, past which work goes through a context's
/// own ring.
pub const SUBMIT_MAX: u32 = 1 << 20;

// The shallowest control queue the driver takes leaves 3D work room for a
// submission with its stream.
const _: () = {
    let shallowest = DISPLAY_DESCRIPTORS.next_power_of_two();
    assert!(shallowest as usize / 2 > BATCH_MAX && shallowest - DISPLAY_DESCRIPTORS >= 3);
};

/// A context the device runs Venus work in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Context(SlotKey);

impl Key for Context {
    fn from_slot(slot: SlotKey) -> Self {
        Self(slot)
    }

    fn slot(self) -> SlotKey {
        self.0
    }
}

impl Context {
    /// The device's id for it, which is never zero.
    fn device_id(self) -> Option<u32> {
        u32::try_from(self.0.index()).ok()?.checked_add(1)
    }
}

/// A blob resource the host made.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Blob(SlotKey);

impl Key for Blob {
    fn from_slot(slot: SlotKey) -> Self {
        Self(slot)
    }

    fn slot(self) -> SlotKey {
        self.0
    }
}

impl Blob {
    /// The resource id the device knows it by, which a context's own protocol
    /// names it with.
    #[must_use]
    pub fn resource_id(self) -> Option<u32> {
        BLOB_RESOURCES.checked_add(u32::try_from(self.0.index()).ok()?)
    }
}

/// A command stream in memory the device reads: `len` bytes, a whole number
/// of words, at device address `device_addr`. The memory is its sender's, and
/// the device's until the submission is answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stream {
    /// Where the device reads it.
    pub device_addr: u64,
    /// Its bytes.
    pub len: u32,
}

impl Stream {
    /// No stream: a submission that only fences its ring.
    pub const EMPTY: Self = Self {
        device_addr: 0,
        len: 0,
    };
}

/// What the device did of a 3D command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Done {
    /// What was asked.
    Done,
    /// It mapped the blob, whose part of the host-visible region takes this
    /// caching.
    Mapped(RegionCaching),
}

/// A 3D command the device answered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    /// The tag its sender gave it.
    pub tag: u64,
    /// What the device answered: [`DriverError::DeviceFault`] for an answer
    /// that does not say what it did, which leaves the object it named
    /// unusable.
    pub outcome: Result<Done, DriverError>,
}

/// Why a device takes no Venus work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NoVenus {
    /// It offers no 3D commands, blob resources, or contexts initialised for
    /// a capset.
    Features,
    /// Its node holds no host-visible region a blob could be mapped into.
    HostVisibleRegion,
    /// It lists no Venus capset, or one past what the driver reads.
    Capset,
    /// It would not describe its capsets.
    Refused(DriverError),
}

impl NoVenus {
    /// The reason in a word or two.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Features => "features",
            Self::HostVisibleRegion => "no host-visible region",
            Self::Capset => "no Venus capset",
            Self::Refused(_) => "capsets refused",
        }
    }
}

/// Where an object the device makes is in its life.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Life {
    /// Asked for, not yet answered.
    Making,
    /// Made: it may be named.
    Live,
    /// Its end asked for, not yet answered.
    Ending,
    /// An answer the driver could not read left it unknown: it is never named
    /// again, and its id never reused.
    Doubtful,
}

/// A blob's place in the host-visible region.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Placed {
    Unmapped,
    Mapping,
    Mapped(RegionCaching),
    Unmapping(RegionCaching),
}

struct BlobState {
    life: Life,
    size: u64,
    mappable: bool,
    placed: Placed,
}

/// What a 3D command with the device changes once it is answered.
#[derive(Copy, Clone, Debug)]
enum Effect {
    MakeContext(Context),
    EndContext(Context),
    MakeBlob(Blob),
    EndBlob(Blob),
    Map(Blob),
    Unmap(Blob),
    Run(Context),
}

/// A 3D command with the device.
#[derive(Copy, Clone, Debug)]
struct Pending {
    ticket: Ticket,
    slot: Slot,
    fence: u64,
    descriptors: u16,
    tag: u64,
    effect: Effect,
}

/// What a 3D command's answer says.
#[derive(Copy, Clone, Debug)]
enum Answer {
    Done(Done),
    /// Refused: nothing changed.
    Refused(DriverError),
    /// Without its fence, or stating what the driver cannot read: what it did
    /// is unknown.
    Unknown,
}

impl Answer {
    /// What its command's completion says.
    const fn outcome(self) -> Result<Done, DriverError> {
        match self {
            Self::Done(done) => Ok(done),
            Self::Refused(refused) => Err(refused),
            Self::Unknown => Err(DriverError::DeviceFault),
        }
    }
}

/// What an object is once `answer` came to the command that made it
/// (`making`) or ended it: `None` where it is gone.
fn life_after(making: bool, answer: Answer) -> Option<Life> {
    match answer {
        Answer::Done(_) if making => Some(Life::Live),
        Answer::Refused(_) if !making => Some(Life::Live),
        Answer::Done(_) | Answer::Refused(_) => None,
        Answer::Unknown => Some(Life::Doubtful),
    }
}

/// Read the answer to `pending` the device wrote into `response`.
fn read(pending: &Pending, response: &[u8]) -> Answer {
    if response.len() < wire::HEADER_LEN || wire::fenced(response, pending.fence).is_err() {
        return Answer::Unknown;
    }
    match pending.effect {
        Effect::Map(_) => match wire::map_info(response) {
            Ok(Some(caching)) => Answer::Done(Done::Mapped(caching)),
            Ok(None) => Answer::Unknown,
            Err(refused) => Answer::Refused(refused),
        },
        _ => match wire::ok_nodata(response) {
            Ok(()) => Answer::Done(Done::Done),
            Err(refused) => Answer::Refused(refused),
        },
    }
}

/// The objects the device made for 3D work, by the handles naming them.
struct Objects {
    contexts: SlotMap<Context, Life>,
    blobs: SlotMap<Blob, BlobState>,
}

/// The device's 3D state, where it takes Venus work.
pub(crate) struct ThreeD {
    capset: Vec<u8>,
    capset_version: u32,
    /// Bytes of the host-visible region a blob is mapped into.
    region_len: u64,
    objects: Objects,
    /// What is with the device, in the order it was sent.
    pending: Vec<Pending>,
    completions: VecDeque<Completion>,
    /// Descriptors what is with the device holds.
    descriptors: u16,
    /// Commands out and answers unread it may hold: the slots the display
    /// path leaves it.
    room: usize,
    /// Descriptors it may hold: those the display path leaves it.
    descriptor_room: u16,
}

impl ThreeD {
    /// How many 3D commands may be out and unread at once.
    pub(crate) const fn commands_room(&self) -> usize {
        self.room
    }

    /// Whether a command of `descriptors` fits beside what is out and unread.
    fn room(&self, descriptors: u16) -> Result<(), DriverError> {
        let held = self.pending.len() + self.completions.len();
        if held >= self.room || self.descriptors.saturating_add(descriptors) > self.descriptor_room
        {
            return Err(DriverError::Busy);
        }
        Ok(())
    }
}

impl Objects {
    fn live_context(&self, context: Context) -> Result<(), DriverError> {
        match self.contexts.get(context) {
            Some(Life::Live) => Ok(()),
            Some(Life::Making | Life::Ending) => Err(DriverError::Busy),
            Some(Life::Doubtful) | None => Err(DriverError::NotFound),
        }
    }

    fn set_context(&mut self, context: Context, life: Life) {
        if let Some(held) = self.contexts.get_mut(context) {
            *held = life;
        }
    }

    fn live_blob(&mut self, blob: Blob) -> Result<&mut BlobState, DriverError> {
        match self.blobs.get_mut(blob) {
            Some(state) if state.life == Life::Live => Ok(state),
            Some(state) if matches!(state.life, Life::Making | Life::Ending) => {
                Err(DriverError::Busy)
            }
            _ => Err(DriverError::NotFound),
        }
    }

    /// Change the objects `effect` names as `answer` says the device left
    /// them.
    fn apply(&mut self, effect: Effect, answer: Answer) {
        match effect {
            Effect::MakeContext(context) | Effect::EndContext(context) => {
                match life_after(matches!(effect, Effect::MakeContext(_)), answer) {
                    Some(life) => self.set_context(context, life),
                    None => {
                        self.contexts.remove(context);
                    }
                }
            }
            Effect::MakeBlob(blob) | Effect::EndBlob(blob) => {
                match life_after(matches!(effect, Effect::MakeBlob(_)), answer) {
                    Some(life) => {
                        if let Some(state) = self.blobs.get_mut(blob) {
                            state.life = life;
                        }
                    }
                    None => {
                        self.blobs.remove(blob);
                    }
                }
            }
            Effect::Map(blob) | Effect::Unmap(blob) => {
                let Some(state) = self.blobs.get_mut(blob) else {
                    return;
                };
                state.placed = match (answer, state.placed) {
                    (Answer::Done(Done::Mapped(caching)), _)
                    | (Answer::Refused(_), Placed::Unmapping(caching)) => Placed::Mapped(caching),
                    (Answer::Done(Done::Done), _) | (Answer::Refused(_), Placed::Mapping) => {
                        Placed::Unmapped
                    }
                    (Answer::Refused(_), placed) => placed,
                    (Answer::Unknown, placed) => {
                        state.life = Life::Doubtful;
                        placed
                    }
                };
            }
            Effect::Run(_) => {}
        }
    }
}

impl<T, M, C, L> VirtioGpu<'_, T, M, C, L>
where
    T: Transport,
    M: BufferMemory,
    M::Region: DeviceBuffer,
    C: ConsoleSource,
    L: Recorder<Record>,
{
    /// Learn whether the device takes Venus work, given the `features`
    /// negotiated and the bytes of the host-visible region its node holds,
    /// recording what was found.
    pub(crate) fn offer_venus(&mut self, features: u64, host_visible: Option<u64>) {
        match self.read_venus(features, host_visible) {
            Ok(three_d) => {
                self.log.record(Record::Venus {
                    version: three_d.capset_version,
                });
                self.three_d = Some(three_d);
            }
            Err(why) => self.log.record(Record::NoVenus { why }),
        }
    }

    fn read_venus(&mut self, features: u64, host_visible: Option<u64>) -> Result<ThreeD, NoVenus> {
        if features & FEATURES != FEATURES {
            return Err(NoVenus::Features);
        }
        let region_len = host_visible
            .filter(|&len| len != 0)
            .ok_or(NoVenus::HostVisibleRegion)?;
        let room = usize::from(self.layout.slots).saturating_sub(BATCH_MAX);
        let descriptor_room = self.control.size().saturating_sub(DISPLAY_DESCRIPTORS);
        let listed = self
            .transport
            .read_config_u32(wire::CONFIG_NUM_CAPSETS)
            .min(CAPSETS_MAX);
        let mut venus = None;
        for index in 0..listed {
            let info = self
                .ask(&wire::get_capset_info(index), wire::CAPSET_INFO_LEN)
                .and_then(wire::capset_info)
                .map_err(NoVenus::Refused)?;
            if info.id == wire::CAPSET_VENUS {
                venus = Some(info);
                break;
            }
        }
        let info = venus.ok_or(NoVenus::Capset)?;
        let len = usize::try_from(info.max_size)
            .ok()
            .filter(|len| (1..=wire::CAPSET_MAX).contains(len))
            .ok_or(NoVenus::Capset)?;
        let out_of_memory = |_| NoVenus::Refused(DriverError::OutOfMemory);
        let mut capset = Vec::new();
        capset.try_reserve_exact(len).map_err(out_of_memory)?;
        let mut pending = Vec::new();
        pending.try_reserve_exact(room).map_err(out_of_memory)?;
        let mut completions = VecDeque::new();
        completions.try_reserve_exact(room).map_err(out_of_memory)?;
        let bytes = self
            .ask(
                &wire::get_capset(wire::CAPSET_VENUS, info.max_version),
                wire::HEADER_LEN + len,
            )
            .and_then(|answer| wire::capset(answer, len))
            .map_err(NoVenus::Refused)?;
        capset.extend_from_slice(bytes);
        Ok(ThreeD {
            capset,
            capset_version: info.max_version,
            region_len,
            objects: Objects {
                contexts: SlotMap::new(),
                blobs: SlotMap::new(),
            },
            pending,
            completions,
            descriptors: 0,
            room,
            descriptor_room,
        })
    }

    /// The Venus capset the device states and its version, where it takes
    /// Venus work.
    #[must_use]
    pub fn venus(&self) -> Option<(u32, &[u8])> {
        self.three_d
            .as_ref()
            .map(|three_d| (three_d.capset_version, three_d.capset.as_slice()))
    }

    /// Have the device make a Venus context, named `name` in its diagnostics.
    /// The context may be named once `tag`'s completion says it was made.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] where the device takes no Venus work,
    /// [`DriverError::Busy`] while as much 3D work as the queue leaves room
    /// for is out or unread, [`DriverError::OutOfMemory`] for a record that
    /// cannot be had, else the queue's refusal.
    pub fn create_context(&mut self, name: &[u8], tag: u64) -> Result<Context, DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        three_d.room(2)?;
        let context = three_d
            .objects
            .contexts
            .try_insert(Life::Making)
            .map_err(|_| DriverError::OutOfMemory)?;
        let made = match context.device_id() {
            Some(id) => self.send_3d(
                &wire::ctx_create(id, wire::CAPSET_VENUS, name),
                None,
                Stream::EMPTY,
                tag,
                Effect::MakeContext(context),
            ),
            None => Err(DriverError::NoSpace),
        };
        if made.is_err() {
            if let Some(three_d) = self.three_d.as_mut() {
                three_d.objects.contexts.remove(context);
            }
        }
        made.map(|()| context)
    }

    /// Have the device destroy `context`, which is gone once `tag`'s
    /// completion says so.
    ///
    /// # Errors
    ///
    /// As [`Self::create_context`]; [`DriverError::NotFound`] for a context
    /// that is not live, [`DriverError::Busy`] for one still being made or
    /// ended, or while a submission to it is out.
    pub fn destroy_context(&mut self, context: Context, tag: u64) -> Result<(), DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        three_d.room(2)?;
        three_d.objects.live_context(context)?;
        // A device need never answer a fence on a context it destroyed (QEMU
        // does not): a submission out would keep its slot and stream for good.
        if three_d
            .pending
            .iter()
            .any(|pending| matches!(pending.effect, Effect::Run(on) if on == context))
        {
            return Err(DriverError::Busy);
        }
        let id = context.device_id().ok_or(DriverError::NotFound)?;
        three_d.objects.set_context(context, Life::Ending);
        let sent = self.send_3d(
            &wire::ctx_destroy(id),
            None,
            Stream::EMPTY,
            tag,
            Effect::EndContext(context),
        );
        if sent.is_err() {
            if let Some(three_d) = self.three_d.as_mut() {
                three_d.objects.set_context(context, Life::Live);
            }
        }
        sent
    }

    /// Have `context` make the blob resource `spec` describes, of host
    /// memory. The blob may be named once `tag`'s completion says it was
    /// made.
    ///
    /// # Errors
    ///
    /// As [`Self::create_context`]; [`DriverError::NotFound`] for a `context`
    /// that is not live, [`DriverError::Busy`] for one still being made or
    /// ended; [`DriverError::LengthOutOfRange`] for an empty blob.
    pub fn create_blob(
        &mut self,
        context: Context,
        spec: BlobSpec,
        tag: u64,
    ) -> Result<Blob, DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        three_d.room(2)?;
        three_d.objects.live_context(context)?;
        if spec.size == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        let blob = three_d
            .objects
            .blobs
            .try_insert(BlobState {
                life: Life::Making,
                size: spec.size,
                mappable: spec.mappable,
                placed: Placed::Unmapped,
            })
            .map_err(|_| DriverError::OutOfMemory)?;
        let made = match (context.device_id(), blob.resource_id()) {
            (Some(id), Some(resource)) => self.send_3d(
                &wire::resource_create_blob(id, resource, &spec),
                None,
                Stream::EMPTY,
                tag,
                Effect::MakeBlob(blob),
            ),
            _ => Err(DriverError::NoSpace),
        };
        if made.is_err() {
            if let Some(three_d) = self.three_d.as_mut() {
                three_d.objects.blobs.remove(blob);
            }
        }
        made.map(|()| blob)
    }

    /// Have the device destroy `blob`, which is gone once `tag`'s completion
    /// says so.
    ///
    /// # Errors
    ///
    /// As [`Self::create_context`]; [`DriverError::NotFound`] for a blob that
    /// is not live, [`DriverError::Busy`] for one still being made or ended,
    /// or not yet unmapped.
    pub fn destroy_blob(&mut self, blob: Blob, tag: u64) -> Result<(), DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        three_d.room(2)?;
        let resource = blob.resource_id().ok_or(DriverError::NotFound)?;
        let state = three_d.objects.live_blob(blob)?;
        if state.placed != Placed::Unmapped {
            return Err(DriverError::Busy);
        }
        state.life = Life::Ending;
        let sent = self.send_3d(
            &wire::resource_unref(resource),
            None,
            Stream::EMPTY,
            tag,
            Effect::EndBlob(blob),
        );
        if sent.is_err() {
            if let Some(state) = self
                .three_d
                .as_mut()
                .and_then(|t| t.objects.blobs.get_mut(blob))
            {
                state.life = Life::Live;
            }
        }
        sent
    }

    /// Have the device map `blob` into the host-visible region at `offset`;
    /// `tag`'s completion states the caching its part is mapped with.
    ///
    /// # Errors
    ///
    /// As [`Self::destroy_blob`]; [`DriverError::PermissionDenied`] for a blob
    /// made unmappable, [`DriverError::OutOfRange`] for an offset off a page
    /// or a part past the region.
    pub fn map_blob(&mut self, blob: Blob, offset: u64, tag: u64) -> Result<(), DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        three_d.room(2)?;
        let resource = blob.resource_id().ok_or(DriverError::NotFound)?;
        let region_len = three_d.region_len;
        let state = three_d.objects.live_blob(blob)?;
        if !state.mappable {
            return Err(DriverError::PermissionDenied);
        }
        if state.placed != Placed::Unmapped {
            return Err(DriverError::Busy);
        }
        let fits = offset.is_multiple_of(PAGE_SIZE as u64)
            && offset
                .checked_add(state.size)
                .is_some_and(|end| end <= region_len);
        if !fits {
            return Err(DriverError::OutOfRange);
        }
        state.placed = Placed::Mapping;
        let sent = self.send_3d(
            &wire::resource_map_blob(resource, offset),
            None,
            Stream::EMPTY,
            tag,
            Effect::Map(blob),
        );
        if sent.is_err() {
            if let Some(state) = self
                .three_d
                .as_mut()
                .and_then(|t| t.objects.blobs.get_mut(blob))
            {
                state.placed = Placed::Unmapped;
            }
        }
        sent
    }

    /// Whether the device is known to map `blob` into no part of the
    /// host-visible region: an answer that did not say what it did leaves it
    /// unknown, and the part it was given is free again only once known.
    pub(crate) fn blob_unmapped(&self, blob: Blob) -> bool {
        self.three_d
            .as_ref()
            .and_then(|three_d| three_d.objects.blobs.get(blob))
            .is_some_and(|state| state.placed == Placed::Unmapped && state.life != Life::Doubtful)
    }

    /// Have the device take `blob` out of the host-visible region, its part
    /// free for another once `tag`'s completion says so. The driver unmaps
    /// its own mapping of the part first.
    ///
    /// # Errors
    ///
    /// As [`Self::create_context`]; [`DriverError::NotFound`] for a blob that
    /// is not live, [`DriverError::Busy`] for one still being made or ended,
    /// or not mapped.
    pub fn unmap_blob(&mut self, blob: Blob, tag: u64) -> Result<(), DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        three_d.room(2)?;
        let resource = blob.resource_id().ok_or(DriverError::NotFound)?;
        let state = three_d.objects.live_blob(blob)?;
        let Placed::Mapped(caching) = state.placed else {
            return Err(DriverError::Busy);
        };
        state.placed = Placed::Unmapping(caching);
        let sent = self.send_3d(
            &wire::resource_unmap_blob(resource),
            None,
            Stream::EMPTY,
            tag,
            Effect::Unmap(blob),
        );
        if sent.is_err() {
            if let Some(state) = self
                .three_d
                .as_mut()
                .and_then(|t| t.objects.blobs.get_mut(blob))
            {
                state.placed = Placed::Mapped(caching);
            }
        }
        sent
    }

    /// Submit `stream` to `context`, fenced on its ring `ring`: `tag`'s
    /// completion comes once the ring's work up to it is done.
    ///
    /// # Errors
    ///
    /// As [`Self::create_context`]; [`DriverError::NotFound`] for a `context`
    /// that is not live, [`DriverError::Busy`] for one still being made or
    /// ended; [`DriverError::OutOfRange`] for a ring past [`wire::RINGS`], and
    /// [`DriverError::LengthOutOfRange`] for a stream that is not a whole
    /// number of words or is longer than [`SUBMIT_MAX`].
    pub fn submit_3d(
        &mut self,
        context: Context,
        ring: u8,
        stream: Stream,
        tag: u64,
    ) -> Result<(), DriverError> {
        let three_d = self.three_d.as_mut().ok_or(DriverError::Unsupported)?;
        if ring >= wire::RINGS {
            return Err(DriverError::OutOfRange);
        }
        if !stream.len.is_multiple_of(4) || stream.len > SUBMIT_MAX {
            return Err(DriverError::LengthOutOfRange);
        }
        three_d.room(if stream.len == 0 { 2 } else { 3 })?;
        three_d.objects.live_context(context)?;
        let id = context.device_id().ok_or(DriverError::NotFound)?;
        self.send_3d(
            &wire::submit_3d(id, stream.len),
            Some(ring),
            stream,
            tag,
            Effect::Run(context),
        )
    }

    /// The next 3D command answered, in the order they were sent among those
    /// answered together.
    pub fn completion(&mut self) -> Option<Completion> {
        self.three_d.as_mut()?.completions.pop_front()
    }

    /// Take the answers the device has posted, without waiting.
    ///
    /// # Errors
    ///
    /// The queue's refusal of an answer it had no right to post.
    pub fn poll(&mut self) -> Result<(), DriverError> {
        self.control.drain()?;
        self.take_3d_answers();
        Ok(())
    }

    /// Wait until the 3D command tagged `tag` is answered, for at most
    /// `budget_ns`; one already answered, or none so tagged, costs no wait.
    ///
    /// # Errors
    ///
    /// As [`tairix_virtio::TicketQueue::wait`].
    pub fn wait_for(&mut self, tag: u64, budget_ns: u64) -> Result<(), DriverError> {
        let ticket = self
            .three_d
            .as_ref()
            .and_then(|three_d| three_d.pending.iter().find(|pending| pending.tag == tag))
            .map(|pending| pending.ticket);
        let Some(ticket) = ticket else {
            return Ok(());
        };
        let waited = self
            .control
            .wait(&mut self.transport, self.host, &[ticket], budget_ns);
        self.take_3d_answers();
        waited
    }

    /// Send the 3D command `request`, fenced on `ring` of its context or else
    /// on the device's own timeline, with `stream` following it. Its room was
    /// checked.
    fn send_3d(
        &mut self,
        request: &Request,
        ring: Option<u8>,
        stream: Stream,
        tag: u64,
        effect: Effect,
    ) -> Result<(), DriverError> {
        let fence = self.take_fence();
        let (stream, descriptors) = if stream.len == 0 {
            (None, 2)
        } else {
            let segment = ChainSegment {
                device_addr: stream.device_addr,
                len: stream.len,
                direction: Direction::DeviceRead,
            };
            (Some(segment), 3)
        };
        let slot = self.slots.take()?;
        let published = self.publish(
            slot,
            request,
            |header| match ring {
                Some(ring) => wire::fence_on_ring(header, fence, ring),
                None => wire::fence(header, fence),
            },
            stream,
            AnswerIn::Slot,
        );
        let ticket = match published {
            Ok(ticket) => ticket,
            Err(refused) => {
                self.slots.put(slot);
                return Err(refused);
            }
        };
        self.control.kick(&mut self.transport);
        if let Some(three_d) = self.three_d.as_mut() {
            three_d.descriptors += descriptors;
            three_d.pending.push(Pending {
                ticket,
                slot,
                fence,
                descriptors,
                tag,
                effect,
            });
        }
        Ok(())
    }

    /// Take back each 3D command the device has answered: its slot freed, its
    /// effect applied, and its completion queued.
    pub(crate) fn take_3d_answers(&mut self) {
        let Some(three_d) = self.three_d.as_mut() else {
            return;
        };
        let ThreeD {
            objects,
            pending,
            completions,
            descriptors,
            ..
        } = three_d;
        let (control, commands, slots) = (&mut self.control, &self.commands, &mut self.slots);
        pending.retain(|pending| {
            let written = match control.retire(pending.ticket) {
                Ok(written) => written,
                Err(DriverError::Busy) => return true,
                // A ticket the queue no longer carries: its slot is never
                // reused, since what the device does with it is unknown.
                Err(_) => return false,
            };
            let start = Layout::response_at(pending.slot);
            let answer = read(
                pending,
                &commands.as_bytes()[start..start + said(written, RESPONSE_SLOT)],
            );
            objects.apply(pending.effect, answer);
            completions.push_back(Completion {
                tag: pending.tag,
                outcome: answer.outcome(),
            });
            slots.put(pending.slot);
            *descriptors = descriptors.saturating_sub(pending.descriptors);
            false
        });
    }
}
