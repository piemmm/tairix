//! A session's contexts (`plans/RISCOS-EMULATOR.md` §15.1): the Wimp's task
//! set, each `TaskWindow` and each screen run as one, side by side in one
//! world.
//!
//! A context's own are its processors, its application slot, its program
//! environment, its typing, keys and mouse buffer, its screen — the VDU
//! drivers' state, a screen area of its own and the zero page describing it
//! — the `OS_Byte` variables of its screen and keyboard, its escape condition
//! and redirection, the library words and domain zero page holds for the
//! task running, its GSTrans, its floating point contexts, the values other
//! crates hold for it, and a SWI suspended in a wait. Everything else is the
//! session's. The current context's own are in the world; each other
//! context's are set aside here, and a switch exchanges the two.
//!
//! A context is set aside only where RISC OS could take a callback: no SWI
//! or call in progress and its program in USR mode, so the supervisor stack
//! every context shares is empty and no module is left part way.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_riscos_arm::psr::Mode;

use crate::buffers::MouseBuffer;
use crate::byte::{self, var};
use crate::env::{Buffers, Environment};
use crate::extension::Values;
use crate::input::{Continuation, Typing};
use crate::keyboard::{Raw, HANDLER_WORKSPACE};
use crate::memory::layout::HIGH_ZERO_PAGE;
use crate::memory::{ParkedArea, Slot, SCREEN};
use crate::os::{Ended, Fail, GsState, Os, Parked, Transfer, Unwind};
use crate::output::workspace;
use crate::relay::Relays;
use crate::zeropage;

/// A context, as long as it lives: one made later never has the name of one
/// that ended.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContextId {
    index: u32,
    generation: u32,
}

/// Why a context could not be made, switched to or ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContextError {
    /// No such context lives.
    Unknown,
    /// It is the current context.
    Current,
    /// The current context cannot be set aside now: a SWI or a call is in
    /// progress, or its program runs privileged.
    Busy,
    /// The host or the guest had no room.
    NoMemory,
    /// The new context would not start: the error's number, none for an
    /// unwind. It has been ended again.
    Start(Option<u32>),
    /// The kernel faulted part way through a switch: the session cannot go
    /// on.
    Fault,
}

/// Each range of zero page a context has of its own, by offset and length:
/// its escape condition and redirection, the `OS_Byte` variables of its
/// screen, keyboard and streams, the VDU drivers' colours and changed box,
/// and the library words and domain RISC OS keeps there for the task
/// running.
const ZERO_PAGE: [(u32, u32); 18] = own_zero_page();

const fn own_zero_page() -> [(u32, u32); 18] {
    let mut ranges = [(0, 0); 18];
    ranges[0] = (zeropage::ESC_STATUS, 1);
    ranges[1] = (zeropage::REDIRECT_IN, 2);
    let mut n = 0;
    while n < byte::CONTEXT_VARIABLES.len() {
        let (first, last) = (
            *byte::CONTEXT_VARIABLES[n].start(),
            *byte::CONTEXT_VARIABLES[n].end(),
        );
        ranges[2 + n] = (byte::variable_offset(first), (last - first) as u32 + 1);
        n += 1;
    }
    ranges[13] = (workspace::ECF_SHIFT, 8);
    ranges[14] = (workspace::FOREGROUND, 3 * workspace::BLOCK);
    ranges[15] = (workspace::CHANGED_BOX, workspace::CHANGED_BOX_BYTES);
    ranges[16] = (zeropage::CLIB_COUNTER, 12);
    ranges[17] = (zeropage::DOMAIN_ID, 4);
    ranges
}

const _: () = {
    assert!(zeropage::REDIRECT_OUT == zeropage::REDIRECT_IN + 1);
    assert!(byte::CONTEXT_VARIABLES.len() == 11);
    assert!(workspace::ECF_Y_OFFSET == workspace::ECF_SHIFT + 4);
    assert!(workspace::BACKGROUND == workspace::FOREGROUND + workspace::BLOCK);
    assert!(workspace::BACKGROUND_STORE == workspace::BACKGROUND + workspace::BLOCK);
    assert!(zeropage::RISCOS_LIB_WORD == zeropage::CLIB_COUNTER + 4);
    assert!(zeropage::CLIB_WORD == zeropage::CLIB_COUNTER + 8);
};

const fn total(ranges: &[(u32, u32)]) -> usize {
    let mut bytes = 0;
    let mut n = 0;
    while n < ranges.len() {
        bytes += ranges[n].1 as usize;
        n += 1;
    }
    bytes
}

const fn longest(ranges: &[(u32, u32)]) -> usize {
    let mut most = 0;
    let mut n = 0;
    while n < ranges.len() {
        if ranges[n].1 as usize > most {
            most = ranges[n].1 as usize;
        }
        n += 1;
    }
    most
}

const ZERO_PAGE_BYTES: usize = total(&ZERO_PAGE);
/// The longest run of guest bytes a context keeps.
const LONGEST: usize = longest(&ZERO_PAGE);

/// Where zero page's `offset` is among the bytes a context keeps, for one it
/// keeps.
const fn kept_at(offset: u32) -> Option<usize> {
    let mut at = 0;
    let mut n = 0;
    while n < ZERO_PAGE.len() {
        let (start, length) = ZERO_PAGE[n];
        if offset >= start && offset < start + length {
            return Some(at + (offset - start) as usize);
        }
        at += length as usize;
        n += 1;
    }
    None
}

/// Where a context set aside keeps its output redirection's handle.
const REDIRECT_OUT_KEPT: usize = match kept_at(zeropage::REDIRECT_OUT) {
    Some(at) => at,
    None => panic!("a context keeps its output redirection"),
};

const _: () = assert!(HANDLER_WORKSPACE as usize <= LONGEST);

/// A context's own while another runs.
pub(crate) struct Kept {
    processor: Parked,
    slot: Option<Slot>,
    screen: Option<ParkedArea>,
    env: Environment,
    typing: Typing,
    raw: Raw,
    held: crate::intkey::Held,
    mouse_buffer: MouseBuffer,
    zero_page: [u8; ZERO_PAGE_BYTES],
    key_workspace: [u8; HANDLER_WORKSPACE as usize],
    gs: Option<GsState>,
    transfer: Option<Transfer>,
    callbacks_first: bool,
    continuation: Option<Continuation>,
    resume: Option<(u32, u32)>,
    relays: Relays,
    running: bool,
    /// What it has of FPEmulator's and of VFPSupport's.
    pub(crate) fp: crate::fpemulator::Own,
    pub(crate) vfp_active: u32,
    extensions: Values,
}

/// Where a context stands.
enum Place {
    /// It runs: its own are the world's.
    Current,
    Parked(Box<Kept>),
    /// No context's: the next made takes it.
    Free,
}

struct Entry {
    generation: u32,
    place: Place,
}

/// The session's contexts.
pub(crate) struct Contexts {
    entries: Vec<Entry>,
    current: usize,
}

impl Contexts {
    /// The boot's context, current, alone.
    pub(crate) fn new() -> Option<Self> {
        let mut entries = Vec::new();
        entries.try_reserve_exact(1).ok()?;
        entries.push(Entry {
            generation: 0,
            place: Place::Current,
        });
        Some(Self {
            entries,
            current: 0,
        })
    }

    fn id(&self, index: usize) -> ContextId {
        ContextId {
            index: crate::os::len32(index),
            generation: self.entries.get(index).map_or(0, |entry| entry.generation),
        }
    }

    /// Every context set aside.
    pub(crate) fn parked(&mut self) -> impl Iterator<Item = &mut Kept> {
        self.entries
            .iter_mut()
            .filter_map(|entry| match &mut entry.place {
                Place::Parked(kept) => Some(&mut **kept),
                Place::Current | Place::Free => None,
            })
    }

    /// Whether a context set aside has its output redirected.
    pub(crate) fn redirect_output_parked(&self) -> bool {
        self.entries.iter().any(|entry| match &entry.place {
            Place::Parked(kept) => kept
                .zero_page
                .get(REDIRECT_OUT_KEPT)
                .is_some_and(|&handle| handle != 0),
            Place::Current | Place::Free => false,
        })
    }

    /// The values the other crates hold for each context set aside.
    pub(crate) fn parked_extensions(&mut self) -> impl Iterator<Item = &mut Values> {
        self.parked().map(|kept| &mut kept.extensions)
    }

    /// Where `id` is, while it is set aside.
    fn parked_at(&self, id: ContextId) -> Result<usize, ContextError> {
        let index = id.index as usize;
        let entry = self.entries.get(index).ok_or(ContextError::Unknown)?;
        match entry.place {
            _ if entry.generation != id.generation => Err(ContextError::Unknown),
            Place::Parked(_) => Ok(index),
            Place::Current => Err(ContextError::Current),
            Place::Free => Err(ContextError::Unknown),
        }
    }

    /// What `index` set aside, taken to be exchanged.
    fn take(&mut self, index: usize) -> Option<Box<Kept>> {
        let entry = self.entries.get_mut(index)?;
        match core::mem::replace(&mut entry.place, Place::Current) {
            Place::Parked(kept) => Some(kept),
            other => {
                entry.place = other;
                None
            }
        }
    }

    fn put(&mut self, index: usize, place: Place) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.place = place;
        }
    }

    fn free(&self) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| matches!(entry.place, Place::Free))
    }

    /// Room for one more context, which [`Self::add`] then takes.
    fn reserve(&mut self) -> Result<(), ContextError> {
        if self.free().is_some() {
            return Ok(());
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| ContextError::NoMemory)
    }

    /// `kept` made a context set aside, in the room [`Self::reserve`] made:
    /// its name.
    fn add(&mut self, kept: Box<Kept>) -> ContextId {
        let index = if let Some(index) = self.free() {
            let entry = &mut self.entries[index];
            entry.generation = entry.generation.wrapping_add(1);
            entry.place = Place::Parked(kept);
            index
        } else {
            self.entries.push(Entry {
                generation: 0,
                place: Place::Parked(kept),
            });
            self.entries.len() - 1
        };
        self.id(index)
    }
}

/// The bytes each range of [`ZERO_PAGE`] holds in a new context: the
/// variables as a reset leaves them, the rest nought.
fn fresh_zero_page(cmos: &crate::cmos::Cmos) -> [u8; ZERO_PAGE_BYTES] {
    let table = byte::reset_table(cmos);
    let table_at = byte::variable_offset(var::FIRST);
    let table_end = table_at + crate::os::len32(table.len());
    let mut own = [0; ZERO_PAGE_BYTES];
    let mut at = 0;
    for (offset, length) in ZERO_PAGE {
        let length = length as usize;
        if (table_at..table_end).contains(&offset) {
            let from = (offset - table_at) as usize;
            if let (Some(to), Some(from)) =
                (own.get_mut(at..at + length), table.get(from..from + length))
            {
                to.copy_from_slice(from);
            }
        }
        at += length;
    }
    own
}

impl Os<'_> {
    /// The current context.
    #[must_use]
    pub fn context(&self) -> ContextId {
        let contexts = &self.world.contexts;
        contexts.id(contexts.current)
    }

    /// No SWI, call, interrupt or unwinding in progress.
    fn at_rest(&self) -> bool {
        let world = &*self.world;
        world.depth == 0
            && world.routines_nested == 0
            && world.kernel_calls == 0
            && world.frame.is_none()
            && world.leaving.is_none()
            && world.unwinding.is_none()
            && !world.in_interrupt
            && !world.swi_passing
    }

    /// Whether the current context can be set aside now: nothing in
    /// progress, and its program in USR mode, about to be replaced by the
    /// transfer pending, or not yet begun or over.
    pub fn switchable(&mut self) -> bool {
        if !self.at_rest() {
            return false;
        }
        let world = &mut *self.world;
        world.transfer.is_some()
            || !world.running
            || matches!(world.processor.cpu().mode(), Mode::User | Mode::User26)
    }

    /// Makes `id` current, the current context set aside.
    ///
    /// # Errors
    ///
    /// [`ContextError::Unknown`] or [`ContextError::Current`] for `id`,
    /// [`ContextError::Busy`] where the current context cannot be set aside
    /// now, [`ContextError::NoMemory`] with nothing changed, and
    /// [`ContextError::Fault`].
    pub fn switch_context(&mut self, id: ContextId) -> Result<(), ContextError> {
        let index = self.world.contexts.parked_at(id)?;
        if !self.switchable() {
            return Err(ContextError::Busy);
        }
        let Some(mut kept) = self.world.contexts.take(index) else {
            return Err(ContextError::Unknown);
        };
        match self.exchange(&mut kept) {
            Ok(()) => {
                let contexts = &mut self.world.contexts;
                let left = contexts.current;
                contexts.put(left, Place::Parked(kept));
                contexts.current = index;
                Ok(())
            }
            Err(error) => {
                self.world.contexts.put(index, Place::Parked(kept));
                Err(error)
            }
        }
    }

    /// A new context, made current and started as a reset starts the first:
    /// its stacks empty, its mouse and floating point fresh, the keyboard
    /// handler started for its keys and its screen made in the configured
    /// mode. Nothing runs in it until a program is started there.
    ///
    /// # Errors
    ///
    /// [`ContextError::Busy`] where the current context cannot be set aside
    /// now, [`ContextError::NoMemory`], each with nothing made;
    /// [`ContextError::Start`] once the new context has been ended again; and
    /// [`ContextError::Fault`], also where it could not be.
    pub fn launch_context(&mut self) -> Result<ContextId, ContextError> {
        if !self.switchable() {
            return Err(ContextError::Busy);
        }
        let previous = self.context();
        self.world.contexts.reserve()?;
        let kept = self.fresh_context()?;
        let id = self.world.contexts.add(kept);
        if let Err(error) = self.switch_context(id) {
            self.discard(id);
            return Err(error);
        }
        let Err(fail) = self.start_context() else {
            return Ok(id);
        };
        let number = match fail {
            Fail::Error(block) => self.peek_word(block),
            Fail::Unwind(_) => None,
        };
        // Undoing the launch: refused, the session's contexts are not what
        // its caller was told.
        self.end_context(previous)
            .map_err(|_| ContextError::Fault)?;
        Err(ContextError::Start(number))
    }

    /// Ends the current context, giving back what it has of its own, and
    /// makes `next` current. Ended part way by a refusal, it ends the rest
    /// when asked again.
    ///
    /// # Errors
    ///
    /// [`ContextError::Unknown`] or [`ContextError::Current`] for `next`,
    /// [`ContextError::Busy`] while a SWI or call is in progress,
    /// [`ContextError::NoMemory`] with the current context still current,
    /// and [`ContextError::Fault`].
    pub fn end_context(&mut self, next: ContextId) -> Result<(), ContextError> {
        let index = self.world.contexts.parked_at(next)?;
        if !self.at_rest() {
            return Err(ContextError::Busy);
        }
        self.give_back_own()?;
        let Some(mut kept) = self.world.contexts.take(index) else {
            return Err(ContextError::Unknown);
        };
        match self.exchange(&mut kept) {
            Ok(()) => {
                let contexts = &mut self.world.contexts;
                let ended = contexts.current;
                contexts.put(ended, Place::Free);
                contexts.current = index;
                Ok(())
            }
            Err(error) => {
                self.world.contexts.put(index, Place::Parked(kept));
                Err(error)
            }
        }
    }

    /// What the current context has of its own given back, each taken from
    /// it as it goes so that asking again gives back only the rest: the
    /// files its streams hold, its unit, the values other crates hold for
    /// it, its screen area, its buffers and its slot.
    fn give_back_own(&mut self) -> Result<(), ContextError> {
        self.close_streams();
        crate::vfpsupport::release_unit(self);
        let mut values = Values::new();
        self.world.extensions.exchange_context(&mut values);
        values.end(self);
        if self.memory.area_info(SCREEN).is_ok() {
            self.remove_kernel_area(SCREEN)
                .map_err(|_| ContextError::Fault)?;
        }
        let buffers = core::mem::replace(&mut self.world.env.buffers, Buffers::NONE);
        buffers.release(self);
        if let Some(slot) = self.memory.current_slot() {
            self.memory
                .leave_slot()
                .and_then(|()| self.memory.destroy_slot(slot))
                .map_err(|_| ContextError::Fault)?;
        }
        Ok(())
    }

    /// A context `add` made and never ran, ended without a switch.
    fn discard(&mut self, id: ContextId) {
        let Ok(index) = self.world.contexts.parked_at(id) else {
            return;
        };
        if let Some(kept) = self.world.contexts.take(index) {
            kept.env.buffers.release(self);
        }
        self.world.contexts.put(index, Place::Free);
    }

    /// A new context's own, as a reset leaves them.
    fn fresh_context(&mut self) -> Result<Box<Kept>, ContextError> {
        let typing = Typing::new().ok_or(ContextError::NoMemory)?;
        let kept = Kept {
            processor: Parked::fresh(self.memory.layout().kernel_bits()),
            slot: None,
            screen: None,
            env: self.environment_at_reset(Buffers::NONE),
            typing,
            raw: Raw::new(),
            held: crate::intkey::Held::new(),
            mouse_buffer: MouseBuffer::EMPTY,
            zero_page: fresh_zero_page(&self.world.cmos),
            key_workspace: [0; HANDLER_WORKSPACE as usize],
            gs: None,
            transfer: None,
            callbacks_first: false,
            continuation: None,
            resume: None,
            relays: Relays::new(),
            running: false,
            fp: crate::fpemulator::Own::NONE,
            vfp_active: crate::vfpsupport::NO_CONTEXT,
            extensions: Values::new(),
        };
        let mut kept = tairix_collections::try_box(kept).map_err(|_| ContextError::NoMemory)?;
        let buffers = Buffers::claim(self).map_err(|_| ContextError::NoMemory)?;
        kept.env = self.environment_at_reset(buffers);
        Ok(kept)
    }

    /// The new current context started: its stacks emptied, its mouse and
    /// floating point fresh, its keys and its screen started.
    fn start_context(&mut self) -> Result<(), Fail> {
        let state = self.world.processor.state();
        let state = self.emptied_stacks(state);
        let bits = self.world.processor.bits();
        if self.world.processor.load(bits, &state).is_err() {
            return Err(Fail::Unwind(Unwind::Ended(Ended::Fault)));
        }
        self.reset_mouse();
        crate::fpemulator::start_context(self)?;
        if self.world.keyboard.id.is_some() {
            self.enable_keyboard()?;
        }
        if let Some(vdu) = self.world.vdu {
            (vdu.screen)(self)?;
        }
        Ok(())
    }

    /// The world's own of the current context exchanged for `kept`'s: the
    /// memory first, the only part that can be refused, put back as it was
    /// where it is.
    fn exchange(&mut self, kept: &mut Kept) -> Result<(), ContextError> {
        crate::vfpsupport::release_unit(self);
        let screen = self.memory.park_area(SCREEN).ok();
        if let Some(theirs) = &kept.screen {
            if self.memory.unpark_area(theirs).is_err() {
                if let Some(ours) = &screen {
                    self.memory
                        .unpark_area(ours)
                        .map_err(|_| ContextError::Fault)?;
                }
                return Err(ContextError::NoMemory);
            }
        }
        let slot = self.memory.current_slot();
        let switched = match kept.slot {
            Some(theirs) => self.memory.switch_slot(theirs),
            None => self.memory.leave_slot(),
        };
        if switched.is_err() {
            return Err(ContextError::Fault);
        }
        let processor = self.world.processor.park();
        self.world
            .processor
            .resume(&kept.processor)
            .map_err(|_| ContextError::Fault)?;
        kept.processor = processor;
        kept.slot = slot;
        kept.screen = screen;
        let world = &mut *self.world;
        core::mem::swap(&mut world.env, &mut kept.env);
        core::mem::swap(&mut world.input.typing, &mut kept.typing);
        core::mem::swap(&mut world.keyboard.raw, &mut kept.raw);
        core::mem::swap(&mut world.intkey.held, &mut kept.held);
        core::mem::swap(&mut world.gs, &mut kept.gs);
        core::mem::swap(&mut world.transfer, &mut kept.transfer);
        core::mem::swap(&mut world.callbacks_first, &mut kept.callbacks_first);
        core::mem::swap(&mut world.continuation, &mut kept.continuation);
        core::mem::swap(&mut world.resume, &mut kept.resume);
        core::mem::swap(&mut world.relays, &mut kept.relays);
        core::mem::swap(&mut world.running, &mut kept.running);
        core::mem::swap(&mut world.fp_emulator.own, &mut kept.fp);
        core::mem::swap(&mut world.vfp_support.active, &mut kept.vfp_active);
        world.extensions.exchange_context(&mut kept.extensions);
        let fpa_active = world.fp_emulator.own.active();
        world.processor.cpu().set_fpa_active(fpa_active);
        self.exchange_guest_bytes(kept)
            .map_err(|_| ContextError::Fault)
    }

    /// The context's bytes in the guest exchanged for `kept`'s: its zero
    /// page, the key handler's workspace and the mouse buffer.
    fn exchange_guest_bytes(&mut self, kept: &mut Kept) -> Result<(), Fail> {
        let mut at = 0;
        for (offset, length) in ZERO_PAGE {
            let length = length as usize;
            if let Some(own) = kept.zero_page.get_mut(at..at + length) {
                exchange_bytes(self, HIGH_ZERO_PAGE + offset, own)?;
            }
            at += length;
        }
        let key_workspace = self.world.keyboard.workspace;
        if key_workspace != 0 {
            exchange_bytes(self, key_workspace, &mut kept.key_workspace)?;
        }
        self.exchange_mouse_buffer(&mut kept.mouse_buffer)
    }
}

/// The guest's bytes at `address` exchanged for `own`, [`LONGEST`] at most.
fn exchange_bytes(os: &mut Os<'_>, address: u32, own: &mut [u8]) -> Result<(), Fail> {
    let mut buffer = [0; LONGEST];
    let Some(live) = buffer.get_mut(..own.len()) else {
        return Err(os.no_memory());
    };
    os.read_bytes(address, live)?;
    for (live, own) in live.iter_mut().zip(own.iter_mut()) {
        core::mem::swap(live, own);
    }
    os.write_bytes(address, live)
}

#[cfg(test)]
mod tests;
