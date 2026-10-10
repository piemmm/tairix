//! One isochronous stream the engine runs: its slots, the completions the
//! host controller has reported and the engine not yet read, and how a slot
//! is filled from — or emptied into — the mixer's ring.
//!
//! A stream's slots are queued in turn and the controller finishes them in
//! the order they were queued, so the slots form a ring and the next one to
//! fill is always the one after the last queued. Slots filled before the
//! stream clocks are held, and queued in the same order once it does.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use tairix_abi::driver::audio_ring::PcmRing;
use tairix_abi::usb_urb::{IsoLayout, IsoNotify, IsoPacket, IsoPacketStatus, ISO_MAX_PACKETS};
use tairix_abi::{DriverError, Errno, ProcId};
use tairix_usb::periodic::PacketPacer;

/// Where one slot stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    /// The engine holds it.
    Free,
    /// Filled, and held by the engine until the stream clocks.
    Staged {
        /// Frames the slot carries toward the device.
        frames: u32,
    },
    /// The controller holds it, carrying `frames` (playback) or set out to
    /// receive (capture).
    Queued {
        /// Frames the slot carries toward the device.
        frames: u32,
    },
}

/// A slot the controller finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Done {
    /// The slot.
    pub slot: u16,
    /// Frames it carried toward the device.
    pub frames: u32,
    /// Intervals that passed carrying nothing before it began.
    pub skipped: u32,
    /// Monotonic nanoseconds at which the controller finished it.
    pub completed_at: u64,
}

/// One running isochronous stream.
#[derive(Debug)]
pub struct HwStream {
    /// The endpoint's `bEndpointAddress`.
    pub endpoint: u8,
    /// Its region's geometry.
    pub layout: IsoLayout,
    /// The process whose notices about it are believed: the one that
    /// delegated its region.
    pub grantor: ProcId,
    /// The number the host controller gave it, which its notices carry.
    pub number: NonZeroU32,
    slots: Vec<Slot>,
    next: u16,
    staged: u16,
    queued: u16,
    done: VecDeque<Done>,
    halted: Option<Errno>,
}

impl HwStream {
    /// Stream `number` on `endpoint` over `layout`, its notices believed
    /// from `grantor`, every slot free.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfMemory`].
    pub fn new(
        endpoint: u8,
        layout: IsoLayout,
        grantor: ProcId,
        number: NonZeroU32,
    ) -> Result<Self, DriverError> {
        let count = usize::from(layout.slots);
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| DriverError::OutOfMemory)?;
        slots.resize(count, Slot::Free);
        let mut done = VecDeque::new();
        done.try_reserve_exact(count)
            .map_err(|_| DriverError::OutOfMemory)?;
        Ok(Self {
            endpoint,
            layout,
            grantor,
            number,
            slots,
            next: 0,
            staged: 0,
            queued: 0,
            done,
            halted: None,
        })
    }

    /// Slots the controller holds.
    #[must_use]
    pub const fn queued(&self) -> u16 {
        self.queued
    }

    /// Slots filled and held for the stream's first clock.
    #[must_use]
    pub const fn staged(&self) -> u16 {
        self.staged
    }

    /// The next slot to fill, while it is free.
    #[must_use]
    pub fn next_free(&self) -> Option<u16> {
        (self.halted.is_none() && self.slots.get(usize::from(self.next)) == Some(&Slot::Free))
            .then_some(self.next)
    }

    /// Record that `slot` — the next one — was queued carrying `frames`.
    pub fn mark_queued(&mut self, slot: u16, frames: u32) {
        if self.take_next(slot, Slot::Queued { frames }) {
            self.queued += 1;
        }
    }

    /// Record that `slot` — the next one — was filled carrying `frames`, to
    /// be held until the stream clocks.
    pub fn mark_staged(&mut self, slot: u16, frames: u32) {
        if self.take_next(slot, Slot::Staged { frames }) {
            self.staged += 1;
        }
    }

    /// Put `slot` — the next one, while free — in `state`.
    fn take_next(&mut self, slot: u16, state: Slot) -> bool {
        if slot != self.next || self.slots.get(usize::from(slot)) != Some(&Slot::Free) {
            return false;
        }
        self.slots[usize::from(slot)] = state;
        self.next = (slot + 1) % self.layout.slots;
        true
    }

    /// The held slot filled first, which is the first to queue: held slots
    /// are filled in turn, so they are the ones just before the next.
    #[must_use]
    pub const fn oldest_staged(&self) -> Option<u16> {
        if self.staged == 0 {
            return None;
        }
        Some((self.next + self.layout.slots - self.staged) % self.layout.slots)
    }

    /// Record that held `slot` was queued.
    pub fn queue_staged(&mut self, slot: u16) {
        if let Some(entry) = self.slots.get_mut(usize::from(slot)) {
            if let Slot::Staged { frames } = *entry {
                *entry = Slot::Queued { frames };
                self.staged -= 1;
                self.queued += 1;
            }
        }
    }

    /// The controller finished `slot`. A slot it does not hold is a report
    /// about nothing and is dropped.
    pub fn complete(&mut self, slot: u16, skipped: u32, completed_at: u64) {
        let Some(&Slot::Queued { frames }) = self.slots.get(usize::from(slot)) else {
            return;
        };
        if self.done.iter().any(|done| done.slot == slot) {
            return;
        }
        self.done.push_back(Done {
            slot,
            frames,
            skipped,
            completed_at,
        });
    }

    /// The oldest finished slot, which is the engine's again.
    pub fn take_done(&mut self) -> Option<Done> {
        let done = self.done.pop_front()?;
        if let Some(entry) = self.slots.get_mut(usize::from(done.slot)) {
            *entry = Slot::Free;
            self.queued = self.queued.saturating_sub(1);
        }
        Some(done)
    }

    /// The slot most recently reported finished and not yet taken.
    #[must_use]
    pub fn latest_done(&self) -> Option<u16> {
        self.done.back().map(|done| done.slot)
    }

    /// Frames the slots the controller holds carry.
    #[must_use]
    pub fn queued_frames(&self) -> u32 {
        self.slots
            .iter()
            .map(|slot| match slot {
                Slot::Queued { frames } => *frames,
                Slot::Free | Slot::Staged { .. } => 0,
            })
            .sum()
    }

    /// Whether `notice`, sent by `origin`, is about this stream: from its
    /// grantor, and not one a stream since stopped on the same endpoint left
    /// behind.
    #[must_use]
    pub fn hears(&self, notice: &IsoNotify, origin: ProcId) -> bool {
        notice.endpoint() == self.endpoint
            && notice.stream() == self.number
            && origin == self.grantor
    }

    /// The stream ended on its own, for `reason`.
    pub fn halt(&mut self, reason: Errno) {
        self.halted.get_or_insert(reason);
    }

    /// Why the stream ended, if it has.
    #[must_use]
    pub const fn halted(&self) -> Option<Errno> {
        self.halted
    }
}

/// What a stream's slots carry: their geometry, and how wide and how quiet
/// one frame is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotShape {
    /// The region's geometry.
    pub layout: IsoLayout,
    /// Bytes of one frame.
    pub frame_bytes: usize,
    /// The byte a silent sample is filled with.
    pub silence: u8,
}

/// How a playback slot may be filled when the ring holds less than it needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shortfall {
    /// Wait for the mixer: frames still in flight cover the gap.
    Wait,
    /// The device would run dry: carry silence for what is missing, counted
    /// as lost.
    Pad,
    /// The stream is draining: carry what is left, the rest of the slot
    /// empty.
    Short,
}

/// What filling one playback slot did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Filled {
    /// Frames read out of the ring.
    pub taken: u32,
    /// Frames of silence carried for frames the ring lacked.
    pub padded: u32,
}

impl Filled {
    /// Frames the slot carries toward the device.
    #[must_use]
    pub const fn carried(&self) -> u32 {
        self.taken + self.padded
    }
}

/// The frames each interval of the next slot carries, paced by `pacer` and
/// held to what one interval's budget holds; `pacer` is advanced only if the
/// slot is then queued, so the caller paces a copy.
#[must_use]
pub fn plan_slot(
    shape: SlotShape,
    pacer: &mut PacketPacer,
) -> ([u32; ISO_MAX_PACKETS as usize], u32) {
    let capacity =
        u32::try_from(shape.layout.packet_bytes as usize / shape.frame_bytes.max(1)).unwrap_or(0);
    let mut counts = [0u32; ISO_MAX_PACKETS as usize];
    let mut total = 0u32;
    for count in counts.iter_mut().take(usize::from(shape.layout.packets)) {
        *count = pacer.next_frames().min(capacity);
        total += *count;
    }
    (counts, total)
}

/// Fill playback `slot` of `region` from `ring`: each interval its share of
/// `counts`, as far as the ring reaches and `shortfall` allows. With no ring
/// nothing is read, so a padded slot is silence throughout.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a ring whose counters fail validation;
/// [`DriverError::DeviceFault`] for a region shorter than its layout.
pub fn fill_playback(
    shape: SlotShape,
    region: &mut [u8],
    slot: u16,
    counts: &[u32],
    mut ring: Option<&mut PcmRing<'_>>,
    shortfall: Shortfall,
) -> Result<Filled, DriverError> {
    let SlotShape {
        layout,
        frame_bytes,
        silence,
    } = shape;
    let mut filled = Filled::default();
    for (packet, &count) in (0u16..).zip(counts.iter().take(usize::from(layout.packets))) {
        let data = layout
            .data_mut(region, slot, packet)
            .map_err(|_| DriverError::DeviceFault)?;
        let wanted = count as usize * frame_bytes;
        let area = data.get_mut(..wanted).ok_or(DriverError::DeviceFault)?;
        let taken = match ring.as_deref_mut() {
            Some(ring) => ring.read(area).map_err(|_| DriverError::BadMagic)?,
            None => 0,
        };
        let mut carried = taken;
        if taken < count && shortfall == Shortfall::Pad {
            area[taken as usize * frame_bytes..].fill(silence);
            filled.padded += count - taken;
            carried = count;
        }
        filled.taken += taken;
        let length =
            u32::try_from(carried as usize * frame_bytes).map_err(|_| DriverError::DeviceFault)?;
        layout
            .set_record(
                region,
                slot,
                packet,
                IsoPacket {
                    length,
                    status: IsoPacketStatus::Moved,
                },
            )
            .map_err(|_| DriverError::DeviceFault)?;
    }
    Ok(filled)
}

/// Frames of a finished playback slot the device never played: the slot
/// carried `carried`, and only its moved intervals reached the device. The
/// host controller rewrites a failed interval's length to zero, so the loss
/// is what was sent less what arrived rather than what the failed records
/// still say.
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for a record the layout cannot hold.
pub fn playback_losses(
    shape: SlotShape,
    region: &[u8],
    slot: u16,
    carried: u32,
) -> Result<u32, DriverError> {
    let SlotShape {
        layout,
        frame_bytes,
        ..
    } = shape;
    let mut played = 0u32;
    for packet in 0..layout.packets {
        let record = layout
            .record(region, slot, packet)
            .map_err(|_| DriverError::DeviceFault)?;
        if record.status == IsoPacketStatus::Moved {
            played = played.saturating_add(
                u32::try_from(record.length as usize / frame_bytes.max(1)).unwrap_or(u32::MAX),
            );
        }
    }
    Ok(carried.saturating_sub(played))
}

/// What emptying one capture slot did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Delivered {
    /// Frames the device sent that reached the ring.
    pub written: u32,
    /// Frames the device sent that the ring had no room for.
    pub overrun: u32,
    /// Frames the intervals that moved nothing would have carried.
    pub missed: u32,
    /// Frames the device sent, whether or not the ring took them.
    pub received: u32,
    /// Microframes the intervals that moved spanned.
    pub moved_microframes: u64,
}

/// Empty finished capture `slot` of `region` into `ring`, if there is one:
/// each moved interval's whole frames, a missed or failed interval's nominal
/// frames from `timeline` counted lost. With no ring the frames are only
/// counted — what an implicit-feedback source with no listener needs.
///
/// # Errors
///
/// [`DriverError::BadMagic`] for a ring whose counters fail validation;
/// [`DriverError::DeviceFault`] for a record the layout cannot hold.
pub fn deliver_capture(
    shape: SlotShape,
    region: &[u8],
    slot: u16,
    mut ring: Option<&mut PcmRing<'_>>,
    timeline: &mut PacketPacer,
    interval_microframes: u32,
) -> Result<Delivered, DriverError> {
    let SlotShape {
        layout,
        frame_bytes,
        ..
    } = shape;
    let mut delivered = Delivered::default();
    for packet in 0..layout.packets {
        let record = layout
            .record(region, slot, packet)
            .map_err(|_| DriverError::DeviceFault)?;
        let nominal = timeline.next_frames();
        if record.status != IsoPacketStatus::Moved {
            delivered.missed += nominal;
            continue;
        }
        let data = layout
            .data(region, slot, packet)
            .map_err(|_| DriverError::DeviceFault)?;
        let frames = (record.length as usize).min(data.len()) / frame_bytes.max(1);
        let frames_u32 = u32::try_from(frames).unwrap_or(u32::MAX);
        delivered.received += frames_u32;
        delivered.moved_microframes += u64::from(interval_microframes);
        if let Some(ring) = ring.as_deref_mut() {
            let written = ring
                .write(&data[..frames * frame_bytes])
                .map_err(|_| DriverError::BadMagic)?;
            delivered.written += written;
            delivered.overrun += frames_u32 - written;
        }
    }
    Ok(delivered)
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
