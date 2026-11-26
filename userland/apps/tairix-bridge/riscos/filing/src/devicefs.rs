//! DeviceFS 0.73 (`plans/RISCOS-EMULATOR.md` §10): `devices:`, where every
//! device driver's devices are catalogued, and the streams programs open on
//! them, a byte at a time or through the buffer manager, as the PRM's
//! DeviceFS chapters and RISC OS 5's module, as recorded, describe them.
//!
//! - `special`: a driver's validation string and the words a stream's
//!   special field gives it.
//! - `stream`: a stream's life — its open, its bytes each way, its waits and
//!   its close.
//! - `entries`: the filing system's entries, which FileSwitch calls.
//!
//! A driver's handle and a stream's are blocks of the RMA, as RISC OS 5's
//! are addresses there, so a guest that registers or opens more pays for
//! each in its own memory.

pub(crate) mod special;

mod entries;
mod stream;

use alloc::vec::Vec;

use tairix_collections::HashMap;
use tairix_hash::BuildSipHash13;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::fs::info;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::module::{Builtin, Resource, Services, Swis, SWI_OUT_OF_RANGE};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, quietly, Entry, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::{ADD_CALLBACK, CLAIM, RELEASE, REMOVE_CALLBACK, SET_VAR_VAL};
use tairix_riscos_kernel::swi::X;
use tairix_util::fallible;

use crate::declared::Declared;
use crate::names::NameIndex;
use special::Validation;
use stream::Stream;

/// The filing system's number and name.
const NUMBER: u32 = 0x35;
const NAME: &[u8] = b"devices";

/// Its information word: special fields, told of flushes, saved through its
/// open files, with an extra word, as RISC OS 5's DeviceFS declares itself.
const INFO: u32 =
    NUMBER | info::SPECIAL_FIELDS | info::FLUSH_NOTIFY | info::SAVE_BY_OPEN | info::EXTRA;

/// Its messages, in the emulator's own words.
const MESSAGES: &[u8] = include_bytes!("../resources/DeviceFS/Messages");

/// Its filing system, declared with I/O controls passed to its entries.
static DECLARED: Declared = Declared {
    module: &DEVICE_FS,
    messages: OwnFile::named(b"Resources:$.Resources.DeviceFS.Messages\0"),
    name: NAME,
    first_entry: ENTRIES,
    gbpb: true,
    info: INFO,
    extra: tairix_riscos_kernel::filing::fs::extra::IOCTL,
};

/// The routines it hands out: its filing system's entries from the first,
/// `FSEntry_GBPB` last of them; then the buffer manager's wake-up routine
/// for its transmit buffers, its claim on `UpCallV`, and the callback that
/// starts the drivers.
const ENTRIES: usize = 0;
const WAKE_TX: usize = 8;
const UPCALLS: usize = 9;
const STARTING: usize = 10;

/// Its errors — an operation devices cannot do, a name no device has, and a
/// block device registered — numbered as RISC OS 5's are.
pub(crate) const BAD_OPERATION: Error = Error {
    number: 0x1_3501,
    token: "BadOperation",
};
pub(crate) const NOT_FOUND: Error = Error {
    number: 0x1_3505,
    token: "DeviceNotFound",
};
const BLOCK_DEVICES: Error = Error {
    number: 0x1_3509,
    token: "BlockDevices",
};

/// DeviceFS.
pub static DEVICE_FS: Builtin = Builtin {
    title: "DeviceFS",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: Some(Swis {
        chunk: tairix_riscos_kernel::swi::chunk::DEVICE_FS,
        prefix: "DeviceFS",
        names: &[
            "Register",
            "Deregister",
            "RegisterObjects",
            "DeregisterObjects",
            "CallDevice",
            "Threshold",
            "ReceivedCharacter",
            "TransmitCharacter",
        ],
        handler: swi,
    }),
    services: Some(Services {
        numbers: &[service::FS_REDECLARE],
        handler: service_call,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[
        entries::open,
        entries::get_bytes,
        entries::put_bytes,
        entries::args,
        entries::close,
        entries::file,
        entries::func,
        entries::gbpb,
        stream::transmit_woken,
        stream::upcalled,
        starting,
    ],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.DeviceFS.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// A driver's entry reasons.
pub(crate) mod reason {
    pub(crate) const INITIALISE: u32 = 0;
    pub(crate) const FINALISE: u32 = 1;
    pub(crate) const WAKE_TX: u32 = 2;
    pub(crate) const WAKE_RX: u32 = 3;
    pub(crate) const SLEEP_RX: u32 = 4;
    pub(crate) const ENUMERATE: u32 = 5;
    pub(crate) const CREATE_TX_BUFFER: u32 = 6;
    pub(crate) const CREATE_RX_BUFFER: u32 = 7;
    pub(crate) const HALT: u32 = 8;
    pub(crate) const RESUME: u32 = 9;
    pub(crate) const END_OF_DATA: u32 = 10;
    pub(crate) const STREAM_CREATED: u32 = 11;
}

/// `DeviceFS_Register`'s flags: a block device, which is not provided; a
/// full duplex one; and a driver that monitors its streams while they wait.
const BLOCK_DEVICE: u32 = 1;
const FULL_DUPLEX: u32 = 1 << 1;
const MONITORED: u32 = 1 << 2;

/// A device list entry's flags: buffered, and a path variable made for it.
const BUFFERED: u32 = 1;
const PATH_VARIABLE: u32 = 1 << 1;
/// The words of a device list entry.
const ENTRY_WORDS: usize = 7;

/// Any number of streams, as `DeviceFS_Register` says it.
const UNLIMITED: u32 = u32::MAX;

/// The longest device name taken: a containment bound on a name a driver
/// hands over.
const NAME_MOST: usize = 255;

/// The RMA a driver's record charges.
const DRIVER_RECORD: u32 = 16;

/// A stream's direction, as a driver's flags and the PRM number them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Way {
    Receive,
    Transmit,
}

impl Way {
    /// Its place in a driver's limits and a device's buffers.
    const fn index(self) -> usize {
        match self {
            Self::Receive => 0,
            Self::Transmit => 1,
        }
    }

    /// As an `UpCall`'s R2 says it.
    pub(crate) const fn upcall_word(self) -> u32 {
        match self {
            Self::Receive => 0,
            Self::Transmit => u32::MAX,
        }
    }
}

/// Where a driver's code is entered: its handle, entry, private word and
/// workspace, and its profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Target {
    pub(crate) handle: u32,
    entry: u32,
    private: u32,
    workspace: u32,
    bits: Bits,
}

impl Target {
    /// The driver's entry for `reason` with `regs`, R1 its handle and R8 and
    /// R12 its own, entered as `entry` says.
    pub(crate) fn call(
        self,
        os: &mut Os<'_>,
        reason: u32,
        regs: &mut Regs,
        entry: Entry,
    ) -> Result<(), Fail> {
        self.call_with(os, reason, self.handle, regs, entry)
    }

    /// As [`Self::call`], R1 `r1`.
    pub(crate) fn call_with(
        self,
        os: &mut Os<'_>,
        reason: u32,
        r1: u32,
        regs: &mut Regs,
        entry: Entry,
    ) -> Result<(), Fail> {
        regs.r[0] = reason;
        regs.r[1] = r1;
        regs.r[8] = self.private;
        regs.r[12] = self.workspace;
        os.call_routine(self.entry, self.bits, entry, regs)
    }
}

/// A registered driver.
#[derive(Clone, Debug)]
pub(crate) struct Driver {
    pub(crate) target: Target,
    pub(crate) duplex: bool,
    pub(crate) monitored: bool,
    pub(crate) validation: Option<Validation>,
    /// The streams it takes each way: none at nought.
    pub(crate) most: [u32; 2],
}

/// A device in the catalogue.
#[derive(Clone, Debug)]
pub(crate) struct Device {
    /// Its own number, which a stream on it names it by.
    pub(crate) id: u32,
    pub(crate) name: Vec<u8>,
    pub(crate) driver: u32,
    pub(crate) buffered: bool,
    path: bool,
    /// Each way's buffer: its flags and size.
    pub(crate) buffers: [(u32, u32); 2],
    pub(crate) load: u32,
    pub(crate) exec: u32,
}

/// What DeviceFS holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    /// The drivers registered, by their handles.
    drivers: HashMap<u32, Driver, BuildSipHash13>,
    /// Their handles, in the order they registered.
    registered: Vec<u32>,
    /// The devices, in their numbers' order, which is the order they were
    /// added in.
    devices: Vec<Device>,
    /// Each device's number, by its name.
    named: NameIndex<NAME_MOST>,
    /// Its streams, in their handles' order.
    pub(crate) streams: Vec<Stream>,
    /// The handles of streams closed under their holders, in order: each
    /// kept claimed until its holder closes it.
    kept: Vec<u32>,
    next_device: u32,
    starting: bool,
}

impl State {
    pub(crate) fn driver(&self, handle: u32) -> Option<&Driver> {
        self.drivers.get(&handle)
    }

    pub(crate) fn device_named(&self, name: &[u8]) -> Option<&Device> {
        self.device(self.named.get(name)?)
    }

    pub(crate) fn device(&self, id: u32) -> Option<&Device> {
        let at = self.devices.binary_search_by_key(&id, |d| d.id).ok()?;
        Some(&self.devices[at])
    }

    /// The device `id` taken from the catalogue.
    fn take_device(&mut self, id: u32) {
        if let Ok(at) = self.devices.binary_search_by_key(&id, |d| d.id) {
            let device = self.devices.remove(at);
            self.named.remove(&device.name);
        }
    }

    /// The driver `handle` taken from those registered.
    fn take_driver(&mut self, handle: u32) {
        self.drivers.remove(&handle);
        self.registered.retain(|&registered| registered != handle);
    }

    /// Where the stream `handle` names is, else where it would go.
    pub(crate) fn place(&self, handle: u32) -> Result<usize, usize> {
        self.streams
            .binary_search_by_key(&handle, |stream| stream.handle)
    }

    pub(crate) fn stream(&self, handle: u32) -> Option<&Stream> {
        self.place(handle).ok().map(|at| &self.streams[at])
    }

    pub(crate) fn stream_mut(&mut self, handle: u32) -> Option<&mut Stream> {
        self.place(handle).ok().map(|at| &mut self.streams[at])
    }

    /// `handle` kept claimed for its holder, in the room the removal that
    /// closed its stream reserved.
    pub(crate) fn keep(&mut self, handle: u32) {
        if let Err(at) = self.kept.binary_search(&handle) {
            self.kept.insert(at, handle);
        }
    }

    /// Whether `handle` was kept for its holder, which no longer is.
    pub(crate) fn forget_kept(&mut self, handle: u32) -> bool {
        match self.kept.binary_search(&handle) {
            Ok(at) => {
                self.kept.remove(at);
                true
            }
            Err(_) => false,
        }
    }
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// DeviceFS's error `error`.
pub(crate) fn fail(os: &mut Os<'_>, error: Error) -> Fail {
    os.own_error(own, error, &[])
}

/// The text a driver or caller hands over at `at`, to its first control
/// character as a RISC OS string ends; "buffer overflow" for one longer
/// than `most`.
fn text_at(os: &mut Os<'_>, at: u32, most: usize) -> Result<Vec<u8>, Fail> {
    let (text, ended) = os.read_until(at, most, |b| b < b' ')?;
    if !ended {
        return Err(os.error(tairix_riscos_kernel::error::BUFFER_OVERFLOW));
    }
    Ok(text)
}

/// `work` on DeviceFS's state; "not present" while it has none.
pub(crate) fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// Its initialisation: its workspace, the filing system declared, its claim
/// on `UpCallV`, and `Service_DeviceFSStarting` on a callback, as RISC OS 5
/// issues it once the module is in the chain.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, messages) = DECLARED.made(os, regs.r[12])?;
    os.hold_extension(State {
        own: messages,
        workspace,
        drivers: HashMap::with_hasher(BuildSipHash13::with_seed(os.cache_key())),
        registered: Vec::new(),
        devices: Vec::new(),
        named: NameIndex::new(os.cache_key()),
        streams: Vec::new(),
        kept: Vec::new(),
        next_device: 0,
        starting: false,
    })?;
    DECLARED.declare(os, workspace)?;
    vector_claim(os, CLAIM, workspace)?;
    let Some(callback) = os.routine_of(&DEVICE_FS, STARTING) else {
        return Ok(());
    };
    let mut add = Regs::default();
    add.r[..2].copy_from_slice(&[callback, workspace]);
    os.perform(X | ADD_CALLBACK, &mut add)?;
    with(os, |state| state.starting = true)
}

/// DeviceFS's claim on `UpCallV` made or let go, by `swi`.
fn vector_claim(os: &mut Os<'_>, swi: u32, workspace: u32) -> Result<(), Fail> {
    let Some(routine) = os.routine_of(&DEVICE_FS, UPCALLS) else {
        return Ok(());
    };
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[tairix_riscos_kernel::vectors::UPCALL.0, routine, workspace]);
    os.perform(X | swi, &mut regs)
}

/// The callback: `Service_DeviceFSStarting`, the drivers asked to register.
fn starting(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    with(os, |state| state.starting = false)?;
    let mut regs = Regs::default();
    regs.r[1] = service::DEVICE_FS_STARTING;
    os.service(&mut regs)?;
    Ok(())
}

/// Its finalisation: every stream closed and every driver let go, as each
/// deregistration does, `Service_DeviceFSDying`, then the filing system
/// removed and all it holds freed but its workspace, which the kernel frees
/// from its private word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    if with(os, |state| core::mem::take(&mut state.starting))? {
        if let Some(callback) = os.routine_of(&DEVICE_FS, STARTING) {
            let mut regs = Regs::default();
            regs.r[..2].copy_from_slice(&[callback, workspace]);
            os.perform_quietly(X | REMOVE_CALLBACK, &mut regs)?;
        }
    }
    while let Some(handle) = with(os, |state| state.registered.first().copied())? {
        deregister(os, handle)?;
    }
    let mut dying = Regs::default();
    dying.r[1] = service::DEVICE_FS_DYING;
    os.service(&mut dying)?;
    DECLARED.remove(os, workspace)?;
    // The filing system's removal closed every file on it, so no holder is
    // left for a handle still kept.
    for handle in with(os, |state| core::mem::take(&mut state.kept))? {
        os.free_rma_quietly(handle);
    }
    quietly(vector_claim(os, RELEASE, workspace))?;
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

/// Its service handler: `Service_FSRedeclare`, the filing system declared
/// again.
fn service_call(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if regs.r[1] != service::FS_REDECLARE {
        return Ok(());
    }
    quietly(with(os, |state| state.workspace).and_then(|at| DECLARED.declare(os, at)))
}

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 => register(os, regs),
        1 => deregister(os, regs.r[0]),
        2 => register_objects(os, regs.r[0], regs.r[1]),
        3 => deregister_object(os, regs.r[0], regs.r[1]),
        4 => call_device(os, regs),
        5 => stream::threshold(os, regs),
        6 => stream::received(os, regs),
        7 => stream::transmit(os, regs),
        _ => Err(os.error_with(SWI_OUT_OF_RANGE, &[DEVICE_FS.title.as_bytes()])),
    }
}

/// `DeviceFS_Register`: R1's devices for the driver at R2, R3 its private
/// word and R4 its workspace, R5 its validation string, R6 and R7 the
/// streams it takes each way; R0 its handle.
fn register(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [flags, list, entry, private, workspace, validation, rx, tx, ..] = regs.r;
    if flags & BLOCK_DEVICE != 0 {
        return Err(fail(os, BLOCK_DEVICES));
    }
    let validation = match validation {
        0 => None,
        at => {
            let text = text_at(os, at, special::MOST)?;
            Some(Validation::parse(&text).ok_or_else(|| os.no_memory())?)
        }
    };
    let handle = os.rma_claim(DRIVER_RECORD)?;
    let driver = Driver {
        target: Target {
            handle,
            entry,
            private,
            workspace,
            bits: os.caller_bits(),
        },
        duplex: flags & FULL_DUPLEX != 0,
        monitored: flags & MONITORED != 0,
        validation,
        most: [rx, tx],
    };
    let kept = with(os, |state| {
        if state.registered.try_reserve(1).is_err()
            || state.drivers.try_insert(handle, driver).is_err()
        {
            return false;
        }
        state.registered.push(handle);
        true
    })?;
    if !kept {
        os.free_rma_quietly(handle);
        return Err(os.no_memory());
    }
    if let Err(fail) = register_objects(os, handle, list) {
        with(os, |state| state.take_driver(handle))?;
        os.free_rma_quietly(handle);
        return Err(fail);
    }
    regs.r[0] = handle;
    Ok(())
}

/// `DeviceFS_RegisterObjects`: the devices of the list at `list` added for
/// the driver `handle`, each replacing any of its name; any that cannot be
/// added removing all of them.
fn register_objects(os: &mut Os<'_>, handle: u32, list: u32) -> Result<(), Fail> {
    if with(os, |state| state.driver(handle).is_none())? {
        return Err(fail(os, NOT_FOUND));
    }
    let mut added: Vec<u32> = Vec::new();
    let mut at = list;
    loop {
        let mut words = [0; ENTRY_WORDS];
        os.read_words(at, &mut words[..1])?;
        if words[0] == 0 {
            return Ok(());
        }
        let outcome = os
            .read_words(at, &mut words)
            .and_then(|()| add_device(os, handle, at, &words));
        match outcome {
            Ok(id) if fallible::push(&mut added, id) => {}
            Ok(id) => {
                remove_devices(os, &[id])?;
                remove_devices(os, &added)?;
                return Err(os.no_memory());
            }
            Err(fail) => {
                remove_devices(os, &added)?;
                return Err(fail);
            }
        }
        at = at.wrapping_add(4 * len32(ENTRY_WORDS));
    }
}

/// The device the list entry at `at` holds, `words` its words, added for
/// the driver `handle`: its variables made, one of its name killed first.
fn add_device(
    os: &mut Os<'_>,
    handle: u32,
    at: u32,
    words: &[u32; ENTRY_WORDS],
) -> Result<u32, Fail> {
    let [name_offset, flags, rx_flags, rx_size, tx_flags, tx_size, _] = *words;
    let name = text_at(os, at.wrapping_add(name_offset), NAME_MOST)?;
    if let Some(old) = with(os, |state| state.device_named(&name).map(|d| d.id))? {
        remove_devices(os, &[old])?;
    }
    let options = option_variable(os, &name)?;
    if os.variable_text(&options)?.is_none() {
        os.set_string_variable(&options, b"")?;
    }
    let path = flags & PATH_VARIABLE != 0;
    if path {
        let value =
            fallible::joined(&[b"devices#:$.", &name, b"."]).ok_or_else(|| os.no_memory())?;
        let variable = path_variable(os, &name)?;
        set_variable(os, &variable, Some(&value), MACRO)?;
    }
    let (load, exec) =
        crate::fileswitch::file::stamp_now(os, tairix_riscos_kernel::filing::filetype::DEVICE);
    let added = with(os, |state| {
        // Numbers are never reused, so a stream of a device that has gone
        // never names another; once they run out, no more devices are made.
        let id = state.next_device;
        let next = id.checked_add(1)?;
        if state.devices.try_reserve(1).is_err() || !state.named.insert(&name, id) {
            return None;
        }
        state.next_device = next;
        let device = Device {
            id,
            name,
            driver: handle,
            buffered: flags & BUFFERED != 0,
            path,
            buffers: [(rx_flags, rx_size), (tx_flags, tx_size)],
            load,
            exec,
        };
        state.devices.push(device);
        Some(id)
    })?;
    added.ok_or_else(|| os.no_memory())
}

/// `OS_SetVarVal`'s type of a macro.
const MACRO: u32 = 2;

/// `DeviceFS$<name>$Options`.
fn option_variable(os: &mut Os<'_>, name: &[u8]) -> Result<Vec<u8>, Fail> {
    fallible::joined(&[b"DeviceFS$", name, b"$Options"]).ok_or_else(|| os.no_memory())
}

/// `<name>$Path`.
fn path_variable(os: &mut Os<'_>, name: &[u8]) -> Result<Vec<u8>, Fail> {
    fallible::joined(&[name, b"$Path"]).ok_or_else(|| os.no_memory())
}

/// The variable `name` set to `value` as `kind` says, or deleted for none,
/// by `OS_SetVarVal`.
fn set_variable(os: &mut Os<'_>, name: &[u8], value: Option<&[u8]>, kind: u32) -> Result<(), Fail> {
    let text = value.unwrap_or_default();
    let room = len32(name.len() + text.len() + 2).next_multiple_of(4);
    os.below_supervisor_stack(room, |os, at| {
        let value_at = at + len32(name.len()) + 1;
        os.write_terminated(at, name, 0, name.len() + 1)?;
        os.write_terminated(value_at, text, 0, text.len() + 1)?;
        let mut regs = Regs::default();
        let length = value.map_or(u32::MAX, |text| len32(text.len()));
        regs.r[..5].copy_from_slice(&[at, value_at, length, 0, kind]);
        match os.perform(X | SET_VAR_VAL, &mut regs) {
            Err(Fail::Error(_)) if value.is_none() => Ok(()),
            done => done,
        }
    })
}

/// The devices of `ids` removed: their streams closed, `Service_DeviceDead`
/// for each, and their path variables unset.
fn remove_devices(os: &mut Os<'_>, ids: &[u32]) -> Result<(), Fail> {
    for &id in ids {
        close_streams(os, |stream| stream.device == id)?;
        let Some(device) = with(os, |state| state.device(id).cloned())? else {
            continue;
        };
        dead(os, device.driver, Some(&device.name))?;
        if device.path {
            let variable = path_variable(os, &device.name)?;
            set_variable(os, &variable, None, 0)?;
        }
        with(os, |state| state.take_device(id))?;
    }
    Ok(())
}

/// Every stream `which` picks closed on DeviceFS's side, whatever its driver
/// answers. Its holder keeps both FileSwitch's handle and DeviceFS's, which
/// refuse everything until it closes them, so neither number is given to
/// another stream meanwhile.
fn close_streams(os: &mut Os<'_>, which: impl Fn(&Stream) -> bool) -> Result<(), Fail> {
    loop {
        let next = with(os, |state| {
            let handle = state.streams.iter().find(|s| which(s))?.handle;
            Some(state.kept.try_reserve(1).map(|()| handle))
        })?;
        match next {
            None => return Ok(()),
            Some(Ok(handle)) => stream::shut(os, handle, stream::Closer::Removal)?,
            Some(Err(_)) => return Err(os.no_memory()),
        }
    }
}

/// `Service_DeviceDead` for the device `name` of the driver `handle`, or
/// for the driver itself.
fn dead(os: &mut Os<'_>, handle: u32, name: Option<&[u8]>) -> Result<(), Fail> {
    let issue = |os: &mut Os<'_>, at: u32| {
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[0, service::DEVICE_DEAD, handle, at]);
        os.service(&mut regs).map_err(Fail::Unwind)
    };
    match name {
        Some(name) => os.with_text(name, 0, issue),
        None => issue(os, 0),
    }
}

/// `DeviceFS_Deregister`: the driver `handle` let go: its streams closed and
/// its devices removed, its entry finalised for every stream, and
/// `Service_DeviceDead` for it.
fn deregister(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    let Some(target) = with(os, |state| state.driver(handle).map(|d| d.target))? else {
        return Err(fail(os, NOT_FOUND));
    };
    let ids = with(os, |state| {
        let mine = |device: &&Device| device.driver == handle;
        let count = state.devices.iter().filter(mine).count();
        fallible::collected(count, state.devices.iter().filter(mine).map(|d| d.id))
    })?
    .ok_or_else(|| os.no_memory())?;
    remove_devices(os, &ids)?;
    let mut finalise = Regs::default();
    quietly(target.call(os, reason::FINALISE, &mut finalise, Entry::Supervisor))?;
    dead(os, handle, None)?;
    with(os, |state| state.take_driver(handle))?;
    os.free_rma_quietly(handle);
    Ok(())
}

/// `DeviceFS_DeregisterObjects`: the driver `handle`'s device named at
/// `name` removed.
fn deregister_object(os: &mut Os<'_>, handle: u32, name: u32) -> Result<(), Fail> {
    let name = text_at(os, name, NAME_MOST)?;
    let id = with(os, |state| {
        state
            .device_named(&name)
            .filter(|device| device.driver == handle)
            .map(|device| device.id)
    })?;
    match id {
        Some(id) => remove_devices(os, &[id]),
        None => Err(fail(os, NOT_FOUND)),
    }
}

/// Every driver's entry, in the order the drivers registered.
fn targets(os: &mut Os<'_>) -> Result<Vec<Target>, Fail> {
    with(os, |state| {
        let entries = state
            .registered
            .iter()
            .filter_map(|handle| state.driver(*handle).map(|d| d.target));
        fallible::collected(state.registered.len(), entries)
    })?
    .ok_or_else(|| os.no_memory())
}

/// `DeviceFS_CallDevice`: R0's reason with R2 to R7 to the driver R1 names
/// — by its handle, by a device's path within `devices:`, which the driver
/// is given in R1, or every driver for nought — and what it answers.
fn call_device(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let reason = regs.r[0];
    if regs.r[1] == 0 {
        for target in targets(os)? {
            target.call(os, reason, regs, Entry::Supervisor)?;
        }
        return Ok(());
    }
    if let Some(target) = with(os, |state| state.driver(regs.r[1]).map(|d| d.target))? {
        return target.call(os, reason, regs, Entry::Supervisor);
    }
    let path_at = regs.r[1];
    let path = text_at(os, path_at, NAME_MOST)?;
    let name = path.strip_prefix(b"$.").unwrap_or(&path);
    let skipped = len32(path.len() - name.len());
    let Some(target) = with(os, |state| {
        let device = state.device_named(name)?;
        state.driver(device.driver).map(|driver| driver.target)
    })?
    else {
        return Err(fail(os, NOT_FOUND));
    };
    target.call_with(
        os,
        reason,
        path_at.wrapping_add(skipped),
        regs,
        Entry::Supervisor,
    )
}
