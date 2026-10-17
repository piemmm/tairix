//! xHCI device enumeration (xHCI 1.2 §4.3) and the transfer paths it serves.
//!
//! [`UsbDevice`] drives one controller through the bring-up of attached
//! devices — port reset, Enable Slot, Address Device, the device and
//! configuration descriptors, Configure Endpoint and `SET_CONFIGURATION` —
//! and then serves each interface's interrupt-IN, bulk and control transfers
//! through its [`DeviceEngine`] over the URB transport. It knows no device
//! class: a class driver reads its own descriptors and drives its own class
//! requests, inside the [`crate::transport::UrbScope`] of its interface.
//!
//! # Memory seam
//!
//! Every byte the controller shares with the driver lives in a growable,
//! caller-provided bank of DMA chunks behind the [`DmaBank`] trait — on
//! metal the [`crate::SlabBank`] over capability-granted slabs, in host
//! tests a plain shared buffer — so the enumeration state machine is
//! proven host-side against the register-level mock plus an in-memory
//! ring model. The controller's shared structures live in one chunk sized
//! exactly to the silicon's reported geometry; every device and hub gets
//! its own chunk on attach and returns it on detach, so concurrency is
//! bounded by the controller's slots and genuine memory exhaustion, never
//! a compile-time budget. The engine performs every ring read/write
//! through the seam; the ring state machines themselves hold no
//! memory ([`ProducerRing`], [`EventRingCursor`]).

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::driver::DmaReach;
use tairix_abi::{DriverError, HwDeviceClass, HwMatchKey, HwNode, HwProperty, HwResource};
use tairix_inline::{ArrayVec, BitSet256, RingBuf};

use crate::alternate::is_control_only;
use crate::descriptor::{
    address_dci, descriptors, endpoint_dci, ConfigurationHeader, Malformed,
    CONFIGURATION_HEADER_LEN, DESC_TYPE_ENDPOINT, DESC_TYPE_INTERFACE,
    DESC_TYPE_SS_ENDPOINT_COMPANION, ENDPOINT_ADDR_DIR_IN, ENDPOINT_ADDR_NUMBER_MASK,
    ENDPOINT_ATTR_BULK, ENDPOINT_ATTR_INTERRUPT, ENDPOINT_ATTR_TYPE_MASK, ENDPOINT_DESCRIPTOR_LEN,
    ENDPOINT_MAX_PACKET_MASK, ENDPOINT_TRANSACTIONS_SHIFT, INTERFACE_DESCRIPTOR_LEN,
    SS_ENDPOINT_COMPANION_LEN,
};
use crate::periodic::ServiceInterval;
use crate::ring::{EventRingCursor, ProducerRing, PushOutcome};
use crate::trb::{self, CompletionCode, Trb, TrbType};
use crate::{ControllerStatus, DmaProgram, PortStatus, Xhci};
use tairix_abi::usb_urb::UsbSpeed;
use tairix_abi::RegisterBlock;

#[path = "device_iso.rs"]
mod iso;
#[path = "device_ops.rs"]
pub(crate) mod ops;
#[path = "device_port.rs"]
pub(crate) mod port;
#[path = "device_recover.rs"]
pub(crate) mod recover;
#[path = "device_recovery.rs"]
pub(crate) mod recovery;
#[path = "device_route.rs"]
pub(crate) mod route;
#[path = "device_urb.rs"]
mod urb;

pub use ops::{Serviced, UsbHost};
pub use urb::DeviceEngine;

use recovery::{Escalation, Ladder, OnHalt, Rung};

use iso::{BusClock, Streaming};

/// The alignment of every [`DmaBank`] chunk, in its offset space and on the
/// device side: a page.
pub const DMA_CHUNK_ALIGN: usize = 4096;

/// Growable device-shared memory the engine and the controller both see:
/// a bank of independently allocated DMA chunks addressed through one
/// virtual offset space.
///
/// The engine sizes nothing up front beyond the controller's own shared
/// structures: each enumerated device's rings and buffers live in a chunk
/// [`Self::grow`]n on attach and [`Self::release`]d on detach, so the
/// number of concurrently served devices is bounded by the controller's
/// silicon (its device slots) and genuine memory exhaustion — never by a
/// compile-time constant.
///
/// Chunk base offsets are never reused: a stale offset kept past its
/// chunk's release maps to no chunk and every access through it fails
/// closed, rather than aliasing a later allocation. Reads and writes are
/// CPU-side and bounds-checked within a single chunk. The implementor
/// owns DMA publication ordering (cache cleaning/invalidation on a
/// non-coherent interconnect).
pub trait DmaBank {
    /// Allocate a fresh zeroed chunk of `len` bytes and return its base
    /// offset in the bank's virtual offset space.
    ///
    /// Both the base offset and the chunk's device-visible base are
    /// [`DMA_CHUNK_ALIGN`]-aligned, so in-chunk layout arithmetic preserves
    /// device alignment.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfMemory`] on genuine memory exhaustion (the DMA
    ///   pool, or the bank's bookkeeping heap).
    /// * [`DriverError::OutOfRange`] if the chunk would lie beyond the
    ///   device-visible aperture the controller can reach, or `len` is 0.
    fn grow(&mut self, len: usize) -> Result<usize, DriverError>;

    /// Release the chunk whose base offset `grow` returned, returning its
    /// memory to the allocator.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] if `base` names no live chunk (a double
    /// release or a forged offset — fail closed, never a panic).
    fn release(&mut self, base: usize) -> Result<(), DriverError>;

    /// Take the chunk whose base offset `grow` returned out of service
    /// without returning it: the controller may still reach it. No offset in
    /// it is served again, and its memory is returned only once the
    /// controller confirms it no longer reaches it
    /// ([`Self::release_withheld_chunk`]) or is proven reset
    /// ([`Self::release_withheld`]).
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] if `base` names no live chunk.
    fn withhold(&mut self, base: usize) -> Result<(), DriverError>;

    /// Return the chunk [`Self::withhold`] took out of service at `base`: the
    /// controller has since confirmed it no longer reaches it.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] if `base` names no withheld chunk.
    fn release_withheld_chunk(&mut self, base: usize) -> Result<(), DriverError>;

    /// Return every withheld chunk: the controller has been reset, so it
    /// reaches none of them.
    fn release_withheld(&mut self);

    /// Never return any chunk, live or withheld: the controller may still
    /// reach all of them, and nothing has proven otherwise.
    fn withhold_all(&mut self);

    /// Device-visible address of virtual offset `offset`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] if `offset` lies in no live chunk.
    fn device_addr_of(&self, offset: usize) -> Result<u64, DriverError>;

    /// Place every later chunk where a controller driving `reach` reaches
    /// it.
    ///
    /// # Errors
    ///
    /// The host's refusal to bound its regions so.
    fn narrow_reach(&mut self, reach: DmaReach) -> Result<(), DriverError>;

    /// Copy `buf.len()` bytes at `offset` into `buf`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] if `[offset, offset + buf.len())` does
    /// not lie wholly within one live chunk.
    fn read(&mut self, offset: usize, buf: &mut [u8]) -> Result<(), DriverError>;

    /// Publish `bytes` at `offset`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] if `[offset, offset + bytes.len())` does
    /// not lie wholly within one live chunk.
    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), DriverError>;

    /// The controller has been reset and can no longer reach memory an
    /// earlier instance of this driver gave it, as
    /// [`DmaHost::device_quiesced`](tairix_abi::driver::dma::DmaHost::device_quiesced)
    /// declares.
    fn device_quiesced(&self);
}

/// Wall-clock window, in microseconds, the bring-up walk
/// ([`UsbHost::bring_up`]) allows a powered root port to report a connect
/// before it walks the ports: the hub power-on-good ceiling
/// (`bPwrOn2PwrGood` ≤ ~200 ms, USB 2.0 §11.11) plus the 100 ms
/// attach-debounce interval (USB 2.0 §7.1.7.3) with headroom, so a device
/// present at boot is served before the walk settles. A protocol settle
/// window, not a scalable capacity.
pub(crate) const CONNECT_WINDOW_US: u64 = 500_000;

/// How many times enumeration drives one device through its port reset,
/// Enable Slot and Address Device when bringing its control pipe up faults
/// without the device answering wrong.
///
/// Two shapes qualify. A CRC/timeout/bad-PID or a hub transaction-translator
/// split failure is what a device that is *present but momentarily disturbed*
/// produces — a keyboard hammered with input while its control endpoint is
/// still being brought up (the on-metal defect: typing during boot killed the
/// whole controller). A command the controller *rejected* on its own state
/// ([`CompletionCode::indicates_state_disagreement`]) never reached the device
/// — the VL805 answers an Address Device issued into a port still settling out
/// of its reset that way. A device that answers with an error (STALL, babble)
/// or a forged descriptor re-fails deterministically and is not retried.
///
/// Every retry resets the port first, as Linux `hub_port_init` does on each
/// of its `PORT_INIT_TRIES`: a device whose Address Device succeeded holds
/// its address, so a fresh slot's `SET_ADDRESS`, sent to the default
/// address, reaches it only once a reset has returned it to Default state. A
/// recovery bound, not a scalable capacity.
pub(crate) const ENUM_ATTEMPTS: u32 = 4;

/// TRB slots in the command, EP0 and hub status-change rings. Protocol
/// working sets, not scalable capacities: an EP0 ring holds one control TD
/// and a hub's ring one status transfer, and commands past the command
/// ring's room wait their turn in software.
pub const RING_TRBS: usize = 16;

/// Interrupt-IN transfers the engine keeps armed on a report endpoint at
/// once, so the controller always has a landing TRB for the next report
/// even in the window between one report being delivered to the class
/// driver and that driver submitting its next URB.
///
/// A boot keyboard reports only on a state change, at its polling
/// interval. The class driver reads one report per blocking URB round
/// trip; between the reply and its next submit the endpoint has no URB
/// driving it. If nothing is armed in that window the controller has
/// nowhere to write the interval's report and the device's report is
/// dropped — so a keystroke pressed and released while the class driver
/// (or this host-controller driver) was slow to run, e.g. under heavy CPU
/// load, was silently lost. Keeping several transfers armed means the
/// controller captures each report into its own ring slot regardless of
/// how promptly software re-submits; the xHCI event ring then queues the
/// completions until the class driver drains them one URB at a time.
///
/// Bounded by the ring: the producer ring keeps one slot free to tell
/// full from empty, so at most `RING_TRBS - 2` transfers can be in flight.
/// This depth sits well under that ceiling and comfortably covers the
/// reports a human generates within one class-driver scheduling round
/// trip. A protocol working set, not a scalable capacity.
pub const INT_ARM_DEPTH: usize = 8;

/// TRB slots in an interrupt-IN transfer ring: [`INT_ARM_DEPTH`] in flight,
/// the slot that tells a full ring from an empty one, and the link.
const INT_RING_TRBS: usize = INT_ARM_DEPTH + 2;

/// The longest interrupt-IN transfer a class driver may ask for: the URB
/// data window its report is copied into. A bound on device-supplied data,
/// not a capacity.
pub const INT_TRANSFER_MAX: usize = BULK_BUF_LEN;

/// Minimum TRBs in an xHCI event-ring segment.
pub const EVENT_RING_SEGMENT_MIN_TRBS: usize = 16;

/// TRBs in one event ring segment: one page, so no segment crosses the
/// 64 KiB boundary none may (xHCI Table 6-1).
pub const EVENT_RING_SEGMENT_TRBS: usize = DMA_CHUNK_ALIGN / trb::TRB_LEN;

const _: () = assert!(EVENT_RING_SEGMENT_TRBS >= EVENT_RING_SEGMENT_MIN_TRBS);

/// Event ring segments laid out where the controller takes that many
/// (`HCSPARAMS2` ERST Max; QEMU takes one, the VL805 eight).
///
/// An isochronous stream posts an event for every service interval and
/// raises the interrupt once per slot, so a slot of
/// [`ISO_MAX_PACKETS`](tairix_abi::usb_urb::ISO_MAX_PACKETS) intervals lands
/// that many events before anything drains them. Four segments hold
/// sixteen such slots; a full ring holds the controller back rather than
/// losing an event, so this is headroom, not a capacity.
const EVENT_RING_SEGMENTS_MAX: usize = 4;

/// Byte length of the hub status-change endpoint report buffer (USB 2.0
/// §11.12.4): the port-change bitmap is one bit per port plus the hub bit,
/// so eight bytes covers up to 63 downstream ports — well beyond any hub
/// this engine descends. A fixed protocol working-set buffer, not a
/// scalable capacity.
const HUB_REPORT_LEN: usize = 8;

/// Byte length of the control-transfer data buffer, the longest data stage a
/// class driver may ask for: a composite device's whole configuration
/// descriptor, and a class driver's longest descriptor or report, in one data
/// stage. A bound on device-supplied data, not a capacity: a longer
/// configuration is served from its first bytes only.
pub const CTRL_DATA_LEN: usize = BULK_BUF_LEN;

/// Interfaces decoded from one configuration descriptor: the servable
/// working set of one device. A composite device (a wireless
/// keyboard+mouse receiver) carries two or three interfaces; further
/// interfaces are ignored rather than trusted. A validation bound on
/// device-supplied data, not a scalable capacity.
pub const MAX_INTERFACES: usize = 4;

/// TRB slots in each bulk transfer ring (one link + [`BULK_SLOTS`] data
/// slots). Sized so several bulk URBs can be outstanding per direction
/// while the whole staging area still fits the fixed controller DMA carve
/// beside the scratchpad pages — a protocol working set, not a scalable
/// capacity (the class driver chunks a large transfer through it at a
/// fixed per-device cost).
pub const BULK_RING_TRBS: usize = 9;

/// Data slots in each bulk transfer ring (the ring's last slot is its
/// permanent Link TRB). Each data slot owns one [`BULK_BUF_LEN`] staging
/// buffer, so a completed TRB maps back to its bytes by slot index alone.
pub const BULK_SLOTS: usize = BULK_RING_TRBS - 1;

/// Byte length of one bulk staging buffer — the largest single bulk URB.
/// One controller page: a class driver moves larger transfers as a
/// sequence of URBs through the shared-memory window (the `virtio_blk`
/// chunking precedent), so per-device DMA cost stays fixed.
pub const BULK_BUF_LEN: usize = 4096;

/// The xHCI protocol's ceiling on device slots one controller can expose:
/// `HCSPARAMS1.MaxSlots` is an 8-bit field (xHCI 1.2 §5.3.3), so no
/// controller addresses more than 255 devices concurrently. The engine
/// serves as many devices as the *controller actually reports* — each
/// enumerated device claims a demand-allocated DMA chunk and a table
/// entry, released on detach — so the only concurrency bounds are this
/// protocol ceiling, the controller's own reported slot count, and
/// genuine memory exhaustion (which fails closed as a typed error).
pub const XHCI_MAX_SLOTS: usize = 255;

/// Deepest hub chain a device may sit behind: the xHCI Route String has
/// five four-bit tiers (§8.9.1 / §6.2.2), matching USB 2.0 §4.1.1's
/// five-hub limit. A bound fixed by the protocol, never widened.
pub const MAX_HUB_DEPTH: u8 = 5;

/// Bits of a Route String: four per tier.
const ROUTE_STRING_BITS: u32 = 4 * MAX_HUB_DEPTH as u32;

/// Contexts in an input context: the input control context, the slot
/// context, and the 31 endpoint contexts (§6.2.5).
const INPUT_CONTEXTS: usize = 33;

/// Contexts in an output device context: slot + 31 endpoints (§6.2.1).
const OUTPUT_CONTEXTS: usize = 32;

/// Dwords of a context this driver writes (the defined fields all sit
/// in the first eight dwords; a 64-byte context's tail stays zero).
const CTX_DWORDS: usize = 8;

/// Endpoint context type field: Control (§6.2.3).
const EP_TYPE_CONTROL: u32 = 4;

/// Endpoint context type field: Interrupt IN (§6.2.3).
const EP_TYPE_INTERRUPT_IN: u32 = 7;

/// Endpoint context type field: Bulk OUT (§6.2.3).
const EP_TYPE_BULK_OUT: u32 = 2;

/// Endpoint context type field: Bulk IN (§6.2.3).
const EP_TYPE_BULK_IN: u32 = 6;

/// Endpoint context type field: Isoch OUT (§6.2.3).
pub(crate) const EP_TYPE_ISOCH_OUT: u32 = 1;

/// Endpoint context type field: Isoch IN (§6.2.3).
pub(crate) const EP_TYPE_ISOCH_IN: u32 = 5;

/// Device Context Index of the default control endpoint (§4.5.1). Also
/// the [`DeviceState::int_dci`] marker for an interface with no interrupt
/// endpoint (the value never names a real interrupt endpoint).
const DCI_CONTROL: u8 = 1;

/// Hub power-on-good settle, in microseconds, before reading a downstream
/// port's connect status. A USB 2.0 hub reports `bPwrOn2PwrGood` in 2 ms
/// units and is commonly ≤ 100 ms (USB 2.0 §11.11); this fixed budget
/// covers the typical worst case rather than decoding the field. A fixed
/// protocol settle, not a scalable capacity.
pub(crate) const HUB_POWER_ON_GOOD_US: u32 = 100_000;

/// Poll spacing, in microseconds, while awaiting a port's reset
/// completion — the same figure for a root-hub port and a downstream hub
/// port, which run the same protocol step. A reset signals for 10–20 ms
/// (USB 2.0 §11.5.1.5 `TDRST`), so the first poll usually observes it
/// complete; each interval is parked, never spun. A fixed protocol settle,
/// not a scalable capacity.
pub(crate) const PORT_RESET_POLL_US: u32 = 20_000;

/// Bound on the reset-completion polls: 40 polls of [`PORT_RESET_POLL_US`]
/// give a slow hub 800 ms to enable the port — the budget production
/// stacks allow — after which the attach fails closed. A single fixed
/// 50 ms wait was not enough for slow external hubs, which legitimately
/// take hundreds of milliseconds to complete a downstream reset.
pub(crate) const PORT_RESET_POLLS: u32 = 40;

/// Reset-recovery settle (`TRSTRCY`, USB 2.0 §7.1.7.5), in microseconds,
/// after a port reports reset complete and enabled, before the device
/// behind it is addressed.
///
/// Skipping it on the root-hub port is the metal defect this figure now
/// covers on both tiers: a full-speed device addressed inside its recovery
/// interval has the VL805 reject the Address Device with a Context State
/// Error, which killed the whole controller. A re-driven enumeration attempt
/// ([`ENUM_ATTEMPTS`]) resets the port, so it is settled by this same
/// interval.
pub(crate) const PORT_RESET_SETTLE_US: u32 = 10_000;

/// Bounded attempts for the hub-descriptor read
/// ([`port::read_hub_topology`]): production stacks retry this
/// exchange (Linux's hub driver issues it up to three times) because real
/// hubs occasionally answer it wrongly once and honestly on the retry. A
/// fixed protocol retry budget, not a scalable capacity.
const HUB_DESC_ATTEMPTS: u32 = 3;

/// Packs structures into a chunk of the [`DmaBank`]'s offset space at
/// 64-byte alignment — the strictest alignment any xHCI context or ring
/// requires — so every region constructor lays its slices out through the
/// one definition.
struct Packer {
    next: usize,
}

impl Packer {
    /// Start packing at `base` (a [`DmaBank::grow`] chunk base, or `0` to
    /// measure a region's packed length).
    const fn new(base: usize) -> Self {
        Self { next: base }
    }

    /// Claim `len` bytes starting on a page boundary, so a buffer of at most
    /// a page never spans a 64 KiB boundary, which no TRB's data may (xHCI
    /// §4.11.7.1); the chunk itself starts on one.
    const fn take_page(&mut self, len: usize) -> usize {
        self.next = self.next.next_multiple_of(DMA_CHUNK_ALIGN);
        self.take(len)
    }

    /// Claim `len` bytes, returning their offset and advancing to the
    /// next 64-byte boundary.
    const fn take(&mut self, len: usize) -> usize {
        let offset = self.next;
        self.next = (self.next + len).next_multiple_of(64);
        offset
    }
}

/// One tracked hub's status-change watch chunk: the transfer ring and
/// report buffer of its interrupt-IN status-change endpoint (USB 2.0
/// §11.12.3). Every hub the engine keeps addressed — the root-attached
/// hub and each downstream hub — owns exactly one, allocated from the
/// [`DmaBank`] when the hub installs and released when it detaches, so
/// every tier is watched at once. A hub's output context, EP0 ring, and
/// control data buffer are not here: it keeps the [`DeviceRegion`] it was
/// enumerated on ([`HubState::device_region`]).
#[derive(Copy, Clone, Debug, Default)]
struct HubRegion {
    /// The chunk base offset this region was laid out at — the
    /// [`DmaBank::release`] key.
    base: usize,
    /// The hub's interrupt-IN status-change endpoint transfer ring.
    int_ring: usize,
    /// One status-change report buffer for [`Self::int_ring`]: the hub's
    /// port-change bitmap (USB 2.0 §11.12.4, one bit per port plus the
    /// hub bit; [`HUB_REPORT_LEN`] covers up to 63 downstream ports).
    report: usize,
}

impl HubRegion {
    /// Lay the region out inside the chunk granted at `base`.
    const fn at(base: usize) -> Self {
        let mut packer = Packer::new(base);
        Self {
            base,
            int_ring: packer.take(RING_TRBS * trb::TRB_LEN),
            report: packer.take(HUB_REPORT_LEN),
        }
    }

    /// Packed byte length of one hub watch region — the [`DmaBank::grow`]
    /// request that backs [`Self::at`].
    const fn layout_len() -> usize {
        let mut packer = Packer::new(0);
        let _ = packer.take(RING_TRBS * trb::TRB_LEN);
        let _ = packer.take(HUB_REPORT_LEN);
        packer.next
    }
}

/// One served device's demand-allocated chunk: its output device context,
/// default-control-endpoint transfer ring, interrupt-IN transfer ring with
/// its per-slot report buffers, and bulk endpoint rings with their staging
/// buffers. Every enumerated device — the root-attached device, or each
/// device downstream of the addressed hub — owns exactly one, allocated
/// from the [`DmaBank`] when the device attaches and released when it
/// detaches, so all served devices stay live in the DCBAA at once and an
/// idle controller pays for none.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct DeviceRegion {
    /// The chunk base offset this region was laid out at — the
    /// [`DmaBank::release`] key.
    base: usize,
    /// The device slot's output device context.
    output_ctx: usize,
    /// The input context the slot's commands stage through, so no two
    /// slots' Address Device or Configure Endpoint share one.
    input_ctx: usize,
    /// The device's default-control-endpoint transfer ring.
    ep0_ring: usize,
    /// [`CTRL_DATA_LEN`] bytes the device's control data stages move
    /// through: a stale TD it completes late lands here, never in another
    /// device's transfer.
    ctrl_data: usize,
    /// Interrupt-IN transfer ring, live only for an interface with an
    /// interrupt-IN endpoint.
    int_ring: usize,
    /// One [`INT_TRANSFER_MAX`] buffer per data slot of [`Self::int_ring`]:
    /// slot `n`'s TRB points at buffer `n`, so a completion maps back to its
    /// bytes by slot index.
    report_bufs: usize,
    /// Bulk-IN transfer ring ([`BULK_RING_TRBS`] slots), live only for a
    /// device whose matched interface carries a bulk-IN endpoint (e.g. a
    /// mass-storage interface).
    bulk_in_ring: usize,
    /// Bulk-OUT transfer ring, as [`Self::bulk_in_ring`].
    bulk_out_ring: usize,
    /// Transfer ring of the interface's **second** bulk-IN endpoint (a UAS
    /// interface's two IN pipes). Its TRBs stage through
    /// [`Self::bulk_in_bufs`]: the URB service holds one URB — and so one
    /// bulk TD — in flight per interface, so the direction's pipes never
    /// race on the buffers.
    bulk_in2_ring: usize,
    /// Second bulk-OUT transfer ring, as [`Self::bulk_in2_ring`].
    bulk_out2_ring: usize,
    /// [`BULK_SLOTS`] staging buffers of [`BULK_BUF_LEN`] bytes for the
    /// bulk-IN rings: slot `n`'s TRB points at buffer `n`, so a completion
    /// maps back to its bytes by slot index.
    bulk_in_bufs: usize,
    /// Staging buffers for the bulk-OUT rings, as [`Self::bulk_in_bufs`].
    bulk_out_bufs: usize,
}

impl DeviceRegion {
    /// Lay the region out inside the chunk granted at `base`, for a
    /// controller with `ctx_size`-byte contexts.
    const fn at(base: usize, ctx_size: usize) -> Self {
        let mut packer = Packer::new(base);
        Self {
            base,
            output_ctx: packer.take(OUTPUT_CONTEXTS * ctx_size),
            input_ctx: packer.take(INPUT_CONTEXTS * ctx_size),
            ep0_ring: packer.take(RING_TRBS * trb::TRB_LEN),
            int_ring: packer.take(INT_RING_TRBS * trb::TRB_LEN),
            bulk_in_ring: packer.take(BULK_RING_TRBS * trb::TRB_LEN),
            bulk_out_ring: packer.take(BULK_RING_TRBS * trb::TRB_LEN),
            bulk_in2_ring: packer.take(BULK_RING_TRBS * trb::TRB_LEN),
            bulk_out2_ring: packer.take(BULK_RING_TRBS * trb::TRB_LEN),
            ctrl_data: packer.take_page(CTRL_DATA_LEN),
            report_bufs: packer.take_page((INT_RING_TRBS - 1) * INT_TRANSFER_MAX),
            bulk_in_bufs: packer.take_page(BULK_SLOTS * BULK_BUF_LEN),
            bulk_out_bufs: packer.take_page(BULK_SLOTS * BULK_BUF_LEN),
        }
    }

    /// Packed byte length of one device region — the [`DmaBank::grow`]
    /// request that backs [`Self::at`].
    const fn layout_len(ctx_size: usize) -> usize {
        let region = Self::at(0, ctx_size);
        (region.bulk_out_bufs + BULK_SLOTS * BULK_BUF_LEN).next_multiple_of(64)
    }

    /// Every TRB data buffer of a region laid out at offset `0`: its offset
    /// and length.
    #[cfg(test)]
    pub(crate) fn transfer_buffers(ctx_size: usize) -> Vec<(usize, usize)> {
        let region = Self::at(0, ctx_size);
        let mut buffers = alloc::vec![(region.ctrl_data, CTRL_DATA_LEN)];
        buffers.extend((0..INT_RING_TRBS - 1).map(|slot| {
            (
                region.report_bufs + slot * INT_TRANSFER_MAX,
                INT_TRANSFER_MAX,
            )
        }));
        for bufs in [region.bulk_in_bufs, region.bulk_out_bufs] {
            buffers.extend((0..BULK_SLOTS).map(|slot| (bufs + slot * BULK_BUF_LEN, BULK_BUF_LEN)));
        }
        buffers
    }

    /// Region offset of `pipe`'s transfer ring.
    fn bulk_ring_off(&self, pipe: BulkPipe) -> usize {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_ring,
            (BulkDirection::In, true) => self.bulk_in2_ring,
            (BulkDirection::Out, false) => self.bulk_out_ring,
            (BulkDirection::Out, true) => self.bulk_out2_ring,
        }
    }

    /// Region offset of `pipe`'s staging buffers (shared per direction).
    fn bulk_bufs_off(&self, pipe: BulkPipe) -> usize {
        match pipe.direction {
            BulkDirection::In => self.bulk_in_bufs,
            BulkDirection::Out => self.bulk_out_bufs,
        }
    }
}

/// Where each **controller-shared** structure lives inside the engine's
/// dedicated shared chunk — the first [`DmaBank::grow`] the engine
/// performs: DCBAA (sized from the controller's reported `MaxSlots`),
/// ERST, command ring, event segment, and the scratchpad.
/// Per-device and per-hub regions are **not** here: each is its own
/// demand-allocated chunk ([`DeviceRegion::at`] / [`HubRegion::at`]), so the
/// shared chunk's size is exactly what the silicon's reported geometry
/// requires.
///
/// All offsets are 64-byte aligned, computed chunk-relative by
/// [`Self::new`] and made absolute in the bank's offset space by
/// [`Self::rebased`].
#[derive(Copy, Clone, Debug)]
struct Layout {
    dcbaa: usize,
    erst: usize,
    command_ring: usize,
    /// The first of [`Self::event_segments`] page-sized event segments, laid
    /// back to back so the event cursor reads them as one ring.
    event_segment: usize,
    event_segments: usize,
    /// Offset of the scratchpad buffer pointer array (xHCI §6.6): one
    /// 64-bit device-visible pointer per scratchpad buffer, the array
    /// `DCBAA[0]` points at. Meaningful only when
    /// [`Self::scratchpad_count`] is non-zero.
    scratchpad_array: usize,
    /// Offset of the first scratchpad buffer page. Each buffer is one
    /// controller page and page-aligned. Meaningful only when
    /// [`Self::scratchpad_count`] is non-zero.
    scratchpad_pages: usize,
    /// Number of scratchpad buffers reserved (`HCSPARAMS2` Max Scratchpad
    /// Buffers; the VL805 needs 31).
    scratchpad_count: usize,
    /// The controller page size each scratchpad buffer occupies.
    page_size: usize,
    ctx_size: usize,
    /// The shared chunk's base offset in the bank ([`Self::rebased`]).
    base: usize,
    /// Packed byte length of the shared chunk — the [`DmaBank::grow`]
    /// request that backs it.
    total: usize,
}

impl Layout {
    /// Compute the shared-structure layout, chunk-relative, for a
    /// controller with `max_slots` device slots, `csz` context size, the
    /// reported scratchpad geometry, and `erst_entries` event segments taken.
    /// The result's offsets are relative to a chunk base of `0`;
    /// [`Self::rebased`] moves them to the granted chunk.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if the controller demands scratchpad
    ///   buffers but reports no page size.
    /// * [`DriverError::LengthOutOfRange`] if the scratchpad arithmetic
    ///   overflows (a hostile or broken geometry report).
    fn new(
        max_slots: u8,
        csz: bool,
        scratchpad_count: u32,
        page_size: usize,
        erst_entries: u32,
    ) -> Result<Self, DriverError> {
        let scratchpad_count = scratchpad_count as usize;
        // A controller that needs scratchpad must report a page size so
        // each buffer can land on a page boundary in the device address
        // space (xHCI §4.20 / §6.6). Fail closed otherwise.
        if scratchpad_count > 0 && page_size == 0 {
            return Err(DriverError::OutOfRange);
        }
        let ctx_size = if csz { 64 } else { 32 };
        let event_segments = usize::try_from(erst_entries)
            .unwrap_or(usize::MAX)
            .clamp(1, EVENT_RING_SEGMENTS_MAX);
        let mut packer = Packer::new(0);
        // First, where the chunk's own page alignment keeps every segment on
        // a page.
        let event_segment = packer.take(event_segments * DMA_CHUNK_ALIGN);
        let dcbaa = packer.take((usize::from(max_slots) + 1) * 8);
        let erst = packer.take(event_segments * ERST_ENTRY_LEN);
        let command_ring = packer.take(RING_TRBS * trb::TRB_LEN);
        let (scratchpad_array, scratchpad_pages) = if scratchpad_count > 0 {
            let array = packer.take(scratchpad_count * 8);
            // The buffer pages must be page-aligned, not merely 64-aligned.
            let pages = packer.next.next_multiple_of(page_size);
            packer.next = pages
                .checked_add(
                    scratchpad_count
                        .checked_mul(page_size)
                        .ok_or(DriverError::LengthOutOfRange)?,
                )
                .ok_or(DriverError::LengthOutOfRange)?;
            (array, pages)
        } else {
            (0, 0)
        };
        Ok(Self {
            dcbaa,
            erst,
            command_ring,
            event_segment,
            event_segments,
            scratchpad_array,
            scratchpad_pages,
            scratchpad_count,
            page_size,
            ctx_size,
            base: 0,
            total: packer.next,
        })
    }

    /// The same layout moved to the shared chunk granted at `base` (a
    /// [`DmaBank::grow`] base offset): every offset becomes absolute in
    /// the bank's offset space. The scratchpad offsets are moved only when
    /// scratchpad is in use, preserving their "meaningful only when
    /// non-zero-count" contract.
    fn rebased(self, base: usize) -> Self {
        let (scratchpad_array, scratchpad_pages) = if self.scratchpad_count > 0 {
            (self.scratchpad_array + base, self.scratchpad_pages + base)
        } else {
            (0, 0)
        };
        Self {
            dcbaa: self.dcbaa + base,
            erst: self.erst + base,
            command_ring: self.command_ring + base,
            event_segment: self.event_segment + base,
            scratchpad_array,
            scratchpad_pages,
            base,
            ..self
        }
    }

    /// TRBs in the whole event ring.
    const fn event_trbs(&self) -> usize {
        self.event_segments * EVENT_RING_SEGMENT_TRBS
    }
}

/// Bytes of one event ring segment table entry (xHCI §6.5).
const ERST_ENTRY_LEN: usize = 16;

/// Default-control-endpoint max packet size *assumed* for a protocol
/// speed ID before the device descriptor reports the real
/// `bMaxPacketSize0` (USB2 §5.5.3, USB3 §9.6.6). Full speed's 64-byte
/// worst case holds only for the one-packet prefix read
/// ([`DEVICE_DESCRIPTOR_PREFIX_LEN`]); a full-speed device may legally
/// use 8/16/32, so any longer transfer must wait for the Evaluate
/// Context fix-up ([`ep0_max_packet_from_descriptor`]).
const fn ep0_max_packet(speed: u8) -> Result<u32, DriverError> {
    match speed {
        SPEED_LOW => Ok(8),
        SPEED_FULL | SPEED_HIGH => Ok(64),
        SPEED_SUPER => Ok(512),
        _ => Err(DriverError::DeviceFault),
    }
}

/// Byte length of the device-descriptor prefix read before the default
/// control endpoint's real max packet size is known: bytes 0..8 of the
/// descriptor, ending at `bMaxPacketSize0` (USB 2.0 §9.6.1). Eight bytes
/// is a single packet at the smallest legal EP0 size, so the read
/// completes identically whatever size the device actually uses.
const DEVICE_DESCRIPTOR_PREFIX_LEN: usize = 8;

/// Validate a device descriptor's `bMaxPacketSize0` against the protocol
/// speed and return the default control endpoint's max packet size in
/// bytes: low speed fixes 8, full speed allows 8/16/32/64, high speed
/// fixes 64 (USB 2.0 §5.5.3), and `SuperSpeed` encodes its fixed 512 as
/// the exponent 9 (USB 3.2 §9.6.1).
///
/// # Errors
///
/// * [`DriverError::BadMagic`] for a value the speed does not permit —
///   a forged or corrupt reply.
/// * [`DriverError::DeviceFault`] for a speed ID this driver does not
///   model.
pub(crate) fn ep0_max_packet_from_descriptor(
    speed: u8,
    b_max_packet0: u8,
) -> Result<u32, DriverError> {
    let valid = match speed {
        SPEED_LOW => b_max_packet0 == 8,
        SPEED_FULL => matches!(b_max_packet0, 8 | 16 | 32 | 64),
        SPEED_HIGH => b_max_packet0 == 64,
        SPEED_SUPER => {
            return if b_max_packet0 == 9 {
                Ok(512)
            } else {
                Err(DriverError::BadMagic)
            }
        }
        _ => return Err(DriverError::DeviceFault),
    };
    if valid {
        Ok(u32::from(b_max_packet0))
    } else {
        Err(DriverError::BadMagic)
    }
}

/// The 8-byte SETUP payload of `GET_DESCRIPTOR(device)` for `len`
/// descriptor bytes (USB 2.0 §9.4.3).
const fn setup_get_device_descriptor(len: u16) -> [u8; 8] {
    let l = len.to_le_bytes();
    [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, l[0], l[1]]
}

/// The 8-byte SETUP payload of `GET_DESCRIPTOR(configuration, 0)` for
/// `len` bytes (USB 2.0 §9.4.3): descriptor type `0x02` in the high
/// byte of `wValue`, configuration index `0` in the low byte. A class driver
/// reads its own interface's descriptors with it.
#[must_use]
pub const fn setup_get_configuration_descriptor(len: u16) -> [u8; 8] {
    let l = len.to_le_bytes();
    [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, l[0], l[1]]
}

/// The 8-byte SETUP payload of a standard `CLEAR_FEATURE(ENDPOINT_HALT)`
/// targeting the endpoint the controller knows by Device Context Index `dci`
/// (USB 2.0 §9.4.1): recipient endpoint, feature selector `ENDPOINT_HALT`
/// (0), resetting the endpoint's device-side halt and data toggle. An odd DCI
/// is an IN endpoint (xHCI §4.5.1).
const fn setup_clear_endpoint_halt(dci: u8) -> [u8; 8] {
    let direction = if dci % 2 == 1 {
        ENDPOINT_ADDR_DIR_IN
    } else {
        0
    };
    [
        0x02,
        0x01,
        0x00,
        0x00,
        (dci / 2) | direction,
        0x00,
        0x00,
        0x00,
    ]
}

/// The 8-byte SETUP payload of `GET_DESCRIPTOR(string)` for string `index`
/// in language `langid`, `len` bytes (USB 2.0 §9.4.3, §9.6.7): `wIndex`
/// carries the LANGID, `0` for the LANGID table (string `0`) itself.
const fn setup_get_string_descriptor(index: u8, langid: u16, len: u8) -> [u8; 8] {
    let id = langid.to_le_bytes();
    [0x80, 0x06, index, DESC_TYPE_STRING, id[0], id[1], len, 0x00]
}

/// `bDescriptorType` of a string descriptor (USB 2.0 §9.6.7).
const DESC_TYPE_STRING: u8 = 0x03;

/// The longest string descriptor a one-byte `bLength` can describe.
const STRING_DESCRIPTOR_MAX_LEN: usize = u8::MAX as usize;

/// UTF-16 code units the longest string descriptor carries past its header.
const MAX_STRING_UNITS: usize = (STRING_DESCRIPTOR_MAX_LEN - StringHeader::LEN) / 2;

/// `bInterfaceClass` of a Human Interface Device (USB HID 1.11 §4.1), held
/// as the top byte of the 24-bit class triple ([`InterfaceInfo`]).
const INTERFACE_CLASS_HID: u32 = 0x03;

/// USB interface class code for Audio (USB Audio 1.0 §A.1).
const INTERFACE_CLASS_AUDIO: u32 = 0x01;

/// `bInterfaceClass` of a mass-storage interface (the USB Mass Storage Class
/// Specification Overview), held as the top byte of the 24-bit class triple.
const INTERFACE_CLASS_MASS_STORAGE: u32 = 0x08;

/// `bDeviceClass` of a USB hub (USB 2.0 §11.23.1). The Pi 4B's onboard
/// `2109:3431` VIA Labs hub reports this, so the keyboard plugged into a
/// USB-A port is a device *downstream* of the hub, not on a root-hub
/// port — reaching it requires walking the hub (`plans/PI.md`).
const DEVICE_CLASS_HUB: u8 = 0x09;

/// `bDescriptorType` of a USB 2.0 hub class descriptor (USB 2.0
/// §11.23.2.1), requested with a class `GET_DESCRIPTOR`.
const DESC_TYPE_HUB: u8 = 0x29;

/// `bDescriptorType` of the **`SuperSpeed`** hub class descriptor (USB 3.2
/// §10.15.2.1). A `SuperSpeed` hub serves *only* this descriptor — it
/// STALLs a request for the USB 2.0 [`DESC_TYPE_HUB`] one — so the read
/// must select the type by the hub's own protocol speed.
const DESC_TYPE_SS_HUB: u8 = 0x2A;

/// Byte length of the `SuperSpeed` hub descriptor (USB 3.2 §10.15.2.1): a
/// fixed-size descriptor, unlike the USB 2.0 one whose tail varies with
/// the port count.
const SS_HUB_DESC_LEN: usize = 12;

/// The USB 2.0 hub descriptor's fixed head plus its two port bitmaps at
/// their smallest (USB 2.0 §11.23.2.1), the length Linux requests: a hub
/// with more ports answers it short with its honest byte count.
const HUB_DESC_REQUEST: usize = 15;

/// What a hub class descriptor (USB 2.0 §11.23.2.1 / USB 3.2 §10.15.2.1)
/// tells the engine. Both layouts carry `bNbrPorts` at byte 2 and
/// `wHubCharacteristics` at bytes 3:4.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HubDescriptor {
    /// `bNbrPorts`: the downstream port count.
    pub ports: u8,
    /// The TT Think Time, `wHubCharacteristics` bits 5:6. Zero for a
    /// `SuperSpeed` hub, which has no transaction translator: those bits are
    /// reserved there.
    pub tt_think_time: u8,
    /// The downstream ports whose device is built in, one bit per 1-based
    /// port (`DeviceRemovable`). A port past the 31st, or one the reply did
    /// not reach, is never reported built in.
    pub fixed_ports: u32,
}

/// Where a `SuperSpeed` hub descriptor's `DeviceRemovable` word sits.
const SS_HUB_DEVICE_REMOVABLE: core::ops::Range<usize> = 10..12;

/// Where a USB 2.0 hub descriptor's `DeviceRemovable` bitmap starts.
const HUB_DEVICE_REMOVABLE: usize = 7;

impl HubDescriptor {
    /// The descriptor type a hub of this speed serves — a `SuperSpeed` hub
    /// STALLs a request for the USB 2.0 one — and the length to request.
    const fn request(superspeed: bool) -> (u8, usize) {
        if superspeed {
            (DESC_TYPE_SS_HUB, SS_HUB_DESC_LEN)
        } else {
            (DESC_TYPE_HUB, HUB_DESC_REQUEST)
        }
    }

    /// A hub's reply to `GET_DESCRIPTOR(hub)`: `None` unless it carries the
    /// type a hub of its speed serves and every byte through
    /// `wHubCharacteristics`.
    #[must_use]
    pub fn decode(answer: &[u8], superspeed: bool) -> Option<Self> {
        let &[_, desc_type, ports, low, high, ..] = answer else {
            return None;
        };
        if desc_type != Self::request(superspeed).0 {
            return None;
        }
        let tt_think_time = if superspeed {
            0
        } else {
            ((u16::from_le_bytes([low, high]) >> 5) & 0b11) as u8
        };
        let fixed_ports = if superspeed {
            answer
                .get(SS_HUB_DEVICE_REMOVABLE)
                .map_or(0, |word| u32::from(u16::from_le_bytes([word[0], word[1]])))
        } else {
            // One bit per port and a reserved bit 0, as many bytes as that
            // takes; a hub with more ports than a word holds reports the rest
            // removable.
            let len = (usize::from(ports) + 1).div_ceil(8).min(4);
            answer
                .get(HUB_DEVICE_REMOVABLE..)
                .unwrap_or_default()
                .iter()
                .take(len)
                .enumerate()
                .fold(0, |bits, (byte, &value)| {
                    bits | u32::from(value) << (8 * byte)
                })
        };
        // Bit 0 is reserved and a bit past the hub's last port names nothing.
        let ports_mask = u32::MAX >> (31 - ports.min(31));
        Some(Self {
            ports,
            tt_think_time,
            fixed_ports: fixed_ports & ports_mask & !1,
        })
    }
}

/// Whether the device on downstream `port` of a hub reporting `fixed_ports`
/// built in can be unplugged.
const fn port_removable(fixed_ports: u32, port: u8) -> bool {
    port >= 32 || fixed_ports & (1 << port) == 0
}

/// Hub class request `SET_HUB_DEPTH` (USB 3.2 §10.16.2.7, Table 10-8):
/// a `SuperSpeed` hub must be told its tier depth (the number of hubs
/// between it and the root port) before it can decode the route string
/// in downstream packet headers; without it, downstream transactions
/// are misrouted.
const HUB_REQUEST_SET_HUB_DEPTH: u8 = 12;

/// Hub class port feature selector `PORT_POWER` (USB 2.0 §11.24.2,
/// Table 11-17): a port-power-controlled hub reports a downstream port
/// disconnected until software sets this.
const PORT_FEATURE_POWER: u8 = 8;

/// Hub class port feature selector `PORT_RESET` (USB 2.0 §11.24.2,
/// Table 11-17): resetting a downstream port enables it and lets the
/// hub establish the device's speed (and, for a full/low-speed device,
/// its transaction translator) before the device is addressed.
const PORT_FEATURE_RESET: u8 = 4;

/// Hub class port feature selectors for the latched port-change bits (USB
/// 2.0 §11.24.2, Table 11-17). A hub keeps its status-change endpoint
/// asserting a report for a port until **every** latched change on it is
/// cleared with a class `CLEAR_FEATURE`; clearing only `C_PORT_CONNECTION`
/// while a `C_PORT_RESET`/`C_PORT_ENABLE` latched by enumeration remains set
/// leaves the port flagged forever, so the watch re-fires endlessly on a
/// stale change. [`port::clear_hub_port_changes`] clears each set one.
const PORT_FEATURE_C_CONNECTION: u8 = 16;
const PORT_FEATURE_C_ENABLE: u8 = 17;
const PORT_FEATURE_C_SUSPEND: u8 = 18;
const PORT_FEATURE_C_OVER_CURRENT: u8 = 19;
const PORT_FEATURE_C_RESET: u8 = 20;

/// `wPortStatus` bit: Current Connect Status (USB 2.0 §11.24.2.7.1).
const PORT_STATUS_CONNECT: u16 = 1 << 0;

/// `wPortChange` bits the hub latches and reports in its status-change
/// endpoint bitmap until cleared (USB 2.0 §11.24.2.7.2): Connect Status,
/// Port Enable/Disable, Suspend, Over-Current, and Reset change. Every set
/// bit must be cleared (its [`PORT_FEATURE_C_CONNECTION`]-family selector)
/// or the hub keeps re-asserting the port's status-change report.
const PORT_CHANGE_CONNECT: u16 = 1 << 0;
const PORT_CHANGE_ENABLE: u16 = 1 << 1;
const PORT_CHANGE_SUSPEND: u16 = 1 << 2;
const PORT_CHANGE_OVER_CURRENT: u16 = 1 << 3;
const PORT_CHANGE_RESET: u16 = 1 << 4;

/// Each latched `wPortChange` bit paired with the `CLEAR_FEATURE` selector
/// that clears it, so a port's whole change set is drained in one pass.
const PORT_CHANGE_FEATURES: [(u16, u8); 5] = [
    (PORT_CHANGE_CONNECT, PORT_FEATURE_C_CONNECTION),
    (PORT_CHANGE_ENABLE, PORT_FEATURE_C_ENABLE),
    (PORT_CHANGE_SUSPEND, PORT_FEATURE_C_SUSPEND),
    (PORT_CHANGE_OVER_CURRENT, PORT_FEATURE_C_OVER_CURRENT),
    (PORT_CHANGE_RESET, PORT_FEATURE_C_RESET),
];

/// `SuperSpeed`-hub `wPortChange` bits and `CLEAR_FEATURE` selectors (USB
/// 3.2 §10.16.2.6, Table 10-12): the enable/suspend changes are reserved,
/// and three new latches exist — warm (BH) reset done, a port link-state
/// transition, and a link-configuration error. Every latched bit must be
/// cleared or the hub keeps re-asserting the port's status-change report,
/// exactly as on a USB 2.0 hub.
const PORT_FEATURE_C_LINK_STATE: u8 = 25;
const PORT_FEATURE_C_CONFIG_ERROR: u8 = 26;
const PORT_FEATURE_C_BH_RESET: u8 = 29;
const PORT_CHANGE_BH_RESET: u16 = 1 << 5;
const PORT_CHANGE_LINK_STATE: u16 = 1 << 6;
const PORT_CHANGE_CONFIG_ERROR: u16 = 1 << 7;
const SS_PORT_CHANGE_FEATURES: [(u16, u8); 6] = [
    (PORT_CHANGE_CONNECT, PORT_FEATURE_C_CONNECTION),
    (PORT_CHANGE_OVER_CURRENT, PORT_FEATURE_C_OVER_CURRENT),
    (PORT_CHANGE_RESET, PORT_FEATURE_C_RESET),
    (PORT_CHANGE_BH_RESET, PORT_FEATURE_C_BH_RESET),
    (PORT_CHANGE_LINK_STATE, PORT_FEATURE_C_LINK_STATE),
    (PORT_CHANGE_CONFIG_ERROR, PORT_FEATURE_C_CONFIG_ERROR),
];

/// `wPortStatus` bit: Port Enabled (USB 2.0 §11.24.2.7.1): set by the
/// hub once a port reset completes, the gate the downstream device must
/// pass before it can be addressed.
const PORT_STATUS_ENABLE: u16 = 1 << 1;

/// `wPortStatus` bit: Reset (USB 2.0 §11.24.2.7.1): set while the hub is
/// still driving the downstream port's reset signalling; cleared (with
/// `C_PORT_RESET` latched) once the reset completes.
const PORT_STATUS_RESET: u16 = 1 << 4;

/// `wPortStatus` bit: Low-Speed Device Attached (USB 2.0 §11.24.2.7.1).
const PORT_STATUS_LOW_SPEED: u16 = 1 << 9;

/// `wPortStatus` bit: High-Speed Device Attached (USB 2.0 §11.24.2.7.1).
const PORT_STATUS_HIGH_SPEED: u16 = 1 << 10;

/// xHCI protocol speed ID for a full-speed device (§7.2.1 default speed
/// IDs): the speed of the Pi 4B's keyboard behind the high-speed hub.
pub(crate) const SPEED_FULL: u8 = UsbSpeed::Full.as_u8();

/// xHCI protocol speed ID for a low-speed device (§7.2.1).
pub(crate) const SPEED_LOW: u8 = UsbSpeed::Low.as_u8();

/// xHCI protocol speed ID for a high-speed device (§7.2.1): the speed of
/// the Pi 4B's onboard hub.
pub(crate) const SPEED_HIGH: u8 = UsbSpeed::High.as_u8();

/// xHCI protocol speed ID for a `SuperSpeed` device (§7.2.1).
pub(crate) const SPEED_SUPER: u8 = UsbSpeed::Super.as_u8();

/// The fields of the 18-byte USB device descriptor this driver uses
/// (USB 2.0 §9.6.1), decoded fail-closed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    /// `idVendor`.
    pub vendor_id: u16,
    /// `idProduct`.
    pub product_id: u16,
    /// `bcdDevice`: the device's release number.
    pub device_release: u16,
    /// `bDeviceClass` (`0` defers the class to the interfaces — the
    /// usual shape for HID devices).
    pub device_class: u8,
    /// `bDeviceSubClass`.
    pub device_subclass: u8,
    /// `bDeviceProtocol`.
    pub device_protocol: u8,
    /// `iSerialNumber`: the string holding the serial number, `0` for none.
    pub serial_number_index: u8,
    /// `bNumConfigurations`.
    pub num_configurations: u8,
}

impl DeviceDescriptor {
    /// Byte length of the descriptor on the wire.
    pub const LEN: usize = 18;

    /// Decode the 18 descriptor bytes.
    ///
    /// # Errors
    ///
    /// * [`DriverError::BadMagic`] if `bLength` or `bDescriptorType`
    ///   does not describe a device descriptor, or the device reports
    ///   zero configurations — a forged or corrupt reply.
    pub fn decode(bytes: &[u8; Self::LEN]) -> Result<Self, DriverError> {
        if usize::from(bytes[0]) < Self::LEN || bytes[1] != 0x01 || bytes[17] == 0 {
            return Err(DriverError::BadMagic);
        }
        Ok(Self {
            vendor_id: u16::from_le_bytes([bytes[8], bytes[9]]),
            product_id: u16::from_le_bytes([bytes[10], bytes[11]]),
            device_release: u16::from_le_bytes([bytes[12], bytes[13]]),
            device_class: bytes[4],
            device_subclass: bytes[5],
            device_protocol: bytes[6],
            serial_number_index: bytes[16],
            num_configurations: bytes[17],
        })
    }

    /// Whether this device descriptor describes a USB hub (USB 2.0
    /// §11.23.1).
    ///
    /// The Pi 4B's onboard `2109:3431` hub reports `bDeviceClass = 0x09`;
    /// a keyboard plugged into a USB-A port enumerates *downstream* of
    /// it, so the bring-up must walk the hub's ports rather than treat
    /// the enumerated device as the keyboard.
    #[must_use]
    pub const fn is_hub(&self) -> bool {
        self.device_class == DEVICE_CLASS_HUB
    }
}

/// The 8-byte SETUP payload of `SET_CONFIGURATION(value)` (USB 2.0
/// §9.4.7) — class requests like `SET_PROTOCOL` are only defined on a
/// configured device.
const fn setup_set_configuration(value: u8) -> [u8; 8] {
    [0x00, 0x09, value, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// The 8-byte SETUP payload of the class `GET_DESCRIPTOR(hub)` request
/// (USB 2.0 §11.24.2.5 / USB 3.2 §10.16.2.4): `bmRequestType = 0xA0`
/// (device-to-host, class, device), `desc_type` ([`DESC_TYPE_HUB`] or
/// [`DESC_TYPE_SS_HUB`], selected by the hub's own protocol speed) in the
/// high byte of `wValue`, for `len` bytes.
const fn setup_get_hub_descriptor(desc_type: u8, len: u16) -> [u8; 8] {
    let l = len.to_le_bytes();
    [0xA0, 0x06, 0x00, desc_type, 0x00, 0x00, l[0], l[1]]
}

/// The 8-byte SETUP payload of the hub class `SET_HUB_DEPTH(depth)`
/// request (USB 3.2 §10.16.2.7): `bmRequestType = 0x20` (host-to-device,
/// class, device), the hub's tier depth in `wValue`, no data stage.
/// Defined only for `SuperSpeed` hubs.
const fn setup_set_hub_depth(depth: u8) -> [u8; 8] {
    [
        0x20,
        HUB_REQUEST_SET_HUB_DEPTH,
        depth,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ]
}

/// The 8-byte SETUP payload of `SET_FEATURE(feature)` on a downstream
/// hub `port` (USB 2.0 §11.24.2.13): `bmRequestType = 0x23`
/// (host-to-device, class, other), `feature` in `wValue`, the 1-based
/// `port` in `wIndex`, no data stage.
pub(crate) const fn setup_set_port_feature(feature: u8, port: u8) -> [u8; 8] {
    [0x23, 0x03, feature, 0x00, port, 0x00, 0x00, 0x00]
}

/// The 8-byte SETUP payload of `GET_STATUS` on a downstream hub `port`
/// (USB 2.0 §11.24.2.7): `bmRequestType = 0xA3` (device-to-host, class,
/// other), the 1-based `port` in `wIndex`, a 4-byte
/// `wPortStatus`/`wPortChange` IN data stage.
pub(crate) const fn setup_get_port_status(port: u8) -> [u8; 8] {
    [0xA3, 0x00, 0x00, 0x00, port, 0x00, 0x04, 0x00]
}

/// The 8-byte SETUP payload of `CLEAR_FEATURE(feature)` on a downstream
/// hub `port` (USB 2.0 §11.24.2.2): `bmRequestType = 0x23` (host-to-device,
/// class, other), `feature` in `wValue`, the 1-based `port` in `wIndex`, no
/// data stage. Used to clear a latched port change (e.g.
/// [`PORT_FEATURE_C_CONNECTION`]) once consumed.
const fn setup_clear_port_feature(feature: u8, port: u8) -> [u8; 8] {
    [0x23, 0x01, feature, 0x00, port, 0x00, 0x00, 0x00]
}

/// Whether a hub port's 16-bit `wPortStatus` reports a connected
/// downstream device (USB 2.0 §11.24.2.7.1, Current Connect Status).
#[must_use]
pub const fn hub_port_connected(status: u16) -> bool {
    status & PORT_STATUS_CONNECT != 0
}

/// Whether a hub port's 16-bit `wPortStatus` reports the port enabled
/// (USB 2.0 §11.24.2.7.1) — set by the hub once a port reset completes.
#[must_use]
pub const fn hub_port_enabled(status: u16) -> bool {
    status & PORT_STATUS_ENABLE != 0
}

/// Whether a hub port's 16-bit `wPortStatus` reports a reset still in
/// progress (USB 2.0 §11.24.2.7.1) — the hub is still driving the reset
/// signalling, so the enable bit is not yet meaningful.
#[must_use]
pub const fn hub_port_resetting(status: u16) -> bool {
    status & PORT_STATUS_RESET != 0
}

/// Whether `speed` (an xHCI protocol speed ID, [`hub_port_speed`]) is a
/// full- or low-speed device, which behind a high-speed hub must route
/// through that hub's transaction translator (xHCI §6.2.2 TT fields).
const fn speed_needs_tt(speed: u8) -> bool {
    speed == SPEED_FULL || speed == SPEED_LOW
}

/// Map a hub port's `wPortStatus` speed bits to an xHCI protocol speed
/// ID (USB 2.0 §11.24.2.7.1): Low-Speed → 2, High-Speed → 3, neither →
/// 1 (full speed). Only meaningful when [`hub_port_connected`].
#[must_use]
pub const fn hub_port_speed(status: u16) -> u8 {
    if status & PORT_STATUS_LOW_SPEED != 0 {
        SPEED_LOW
    } else if status & PORT_STATUS_HIGH_SPEED != 0 {
        SPEED_HIGH
    } else {
        SPEED_FULL
    }
}

/// Extend `parent_route` — the Route String of a hub `parent_depth` tiers
/// below the root port — by one tier: the child on that hub's 1-based
/// downstream `port` (xHCI §8.9.1, four bits per tier, least-significant
/// nibble first).
///
/// Fails closed rather than aliasing topology: the Route String holds
/// exactly [`MAX_HUB_DEPTH`] tiers, and a port above 15 cannot be encoded
/// in a nibble (xHCI §8.9.1 caps routable ports at 15).
pub(crate) fn route_for_child(
    parent_route: u32,
    parent_depth: u8,
    port: u8,
) -> Result<u32, DriverError> {
    if parent_depth >= MAX_HUB_DEPTH || port == 0 || port > 15 {
        return Err(DriverError::OutOfRange);
    }
    Ok(parent_route | (u32::from(port) << (4 * u32::from(parent_depth))))
}

/// What a port carries, or an attach served: a leaf device at its
/// device-table index, or a hub tier installed, watched and descended at its
/// hub-table index.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Attachment {
    /// A leaf device served at the carried device-table index.
    Device(usize),
    /// A hub served at the carried hub-table index.
    Hub(usize),
}

/// The port an attachment hangs off.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Upstream {
    /// The 1-based root-hub port.
    Root(u8),
    /// The 1-based downstream port of the hub at the hub-table index.
    Hub(usize, u8),
}

/// A port's live connection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PortLink {
    /// Nothing is connected.
    Vacant,
    /// A device is connected but the port is disabled, so nothing reaches it
    /// until the port is reset.
    Disabled,
    /// A device is connected and the port enabled.
    Enabled,
}

/// A periodic endpoint's packet shape as its descriptors state it, before the
/// device's speed says what it means ([`PeriodicShape::payload`]).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PeriodicShape {
    /// `wMaxPacketSize` bits 0:10.
    pub max_packet: u16,
    /// `wMaxPacketSize` bits 11:12.
    pub transactions: u8,
    /// The `SuperSpeed` companion's `bMaxBurst` and `wBytesPerInterval`, when
    /// one follows the endpoint.
    pub companion: Option<(u8, u16)>,
}

/// The most packets a `SuperSpeed` interrupt endpoint bursts in one service
/// interval, less one (USB 3.2 §9.6.7).
const SS_INTERRUPT_MAX_BURST: u8 = 2;

/// The most additional transactions a high-speed periodic endpoint moves per
/// microframe (USB 2.0 §5.9.1).
const HS_MAX_ADDITIONAL_TRANSACTIONS: u8 = 2;

/// The largest packet an interrupt endpoint may move at `speed` (USB 2.0
/// §5.7.3, USB 3.2 §9.6.6).
const fn interrupt_max_packet(speed: u8) -> u16 {
    match speed {
        SPEED_LOW => 8,
        SPEED_HIGH | SPEED_SUPER => 1024,
        _ => 64,
    }
}

impl PeriodicShape {
    /// The endpoint context's Max Packet Size at `speed` (xHCI §6.2.3): the
    /// descriptor's, held to the most the speed allows, so the controller
    /// refuses a device sending more than its transfer buffers hold.
    #[must_use]
    pub fn max_packet_at(self, speed: u8) -> u16 {
        self.max_packet.min(interrupt_max_packet(speed))
    }

    /// The endpoint context's Max Burst Size and Max ESIT Payload at `speed`
    /// (xHCI §6.2.3.4, §6.2.3.8): the packets one service interval bursts,
    /// less one, and the bytes it moves.
    #[must_use]
    pub fn payload(self, speed: u8) -> (u32, u32) {
        let packet = u32::from(self.max_packet_at(speed));
        let (burst, stated) = match speed {
            SPEED_SUPER => {
                let (burst, bytes) = self.companion.unwrap_or((0, self.max_packet));
                (burst.min(SS_INTERRUPT_MAX_BURST), Some(bytes))
            }
            SPEED_HIGH => (self.transactions.min(HS_MAX_ADDITIONAL_TRANSACTIONS), None),
            _ => (0, None),
        };
        let burst = u32::from(burst);
        let most = packet * (burst + 1);
        let esit = stated.map_or(most, |bytes| u32::from(bytes).clamp(1, most.max(1)));
        (burst, esit)
    }
}

/// One bulk endpoint as its descriptors state it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BulkEndpoint {
    /// Device Context Index (§4.5.1), `0` when the interface carries no such
    /// pipe: a DCI of zero names no device endpoint.
    pub dci: u8,
    /// `wMaxPacketSize` bits 0:10.
    pub max_packet: u16,
    /// The `SuperSpeed` companion's `bMaxBurst`, `0` without one.
    pub max_burst: u8,
}

/// The most packets a `SuperSpeed` bulk endpoint bursts, less one (USB 3.2
/// §9.6.7).
const SS_BULK_MAX_BURST: u8 = 15;

impl BulkEndpoint {
    /// The endpoint context's Max Burst Size at `speed` (xHCI §6.2.3.4): the
    /// companion's burst at `SuperSpeed`, and none at any other speed, where
    /// a bulk endpoint moves one packet per transaction.
    #[must_use]
    pub fn burst(self, speed: u8) -> u32 {
        if speed == SPEED_SUPER {
            u32::from(self.max_burst.min(SS_BULK_MAX_BURST))
        } else {
            0
        }
    }
}

/// One interface's descriptor fields this driver needs (USB 2.0 §9.6.3 /
/// §9.6.5), decoded fail-closed from the `GET_DESCRIPTOR(configuration)`
/// bytes — one per default-alternate interface of the configuration
/// ([`Self::decode_all`]), so a composite device's functions are each
/// represented. The interface class is read from the device, never
/// assumed, so each emitted hardware-tree child node carries the honest
/// class.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InterfaceInfo {
    /// `bConfigurationValue` to select with `SET_CONFIGURATION`.
    pub configuration_value: u8,
    /// `bInterfaceNumber` of this interface.
    pub interface_number: u8,
    /// The 24-bit USB interface class code
    /// `(bInterfaceClass << 16) | (bInterfaceSubClass << 8) | bInterfaceProtocol`
    /// (e.g. an HID boot keyboard is `0x03_01_01`, a boot mouse
    /// `0x03_01_02`), as carried by [`HwMatchKey::usb`].
    pub class24: u32,
    /// Device Context Index of the interface's first interrupt-IN endpoint
    /// (§4.5.1: `2 * endpoint_number + 1`), read from its endpoint
    /// descriptor rather than assumed. The default control-endpoint DCI
    /// (`1`) when the interface has none.
    pub int_dci: u8,
    /// The interrupt-IN endpoint's packet shape, `0`s when it has none.
    pub int_shape: PeriodicShape,
    /// `bInterval` of the interrupt-IN endpoint as the device reported
    /// it (speed-dependent units, decoded by
    /// [`ServiceInterval::interrupt`]).
    /// `0` when the interface has none.
    pub int_b_interval: u8,
    /// The interface's first bulk-IN endpoint.
    pub bulk_in: BulkEndpoint,
    /// The interface's first bulk-OUT endpoint.
    pub bulk_out: BulkEndpoint,
    /// The interface's **second** bulk-IN endpoint: a UAS interface carries
    /// two IN pipes (status and data-in).
    pub bulk_in2: BulkEndpoint,
    /// The interface's **second** bulk-OUT endpoint (a UAS interface's
    /// command and data-out pipes).
    pub bulk_out2: BulkEndpoint,
}

/// The endpoint a `SuperSpeed` companion descriptor completes: the one whose
/// descriptor it follows (USB 3.2 §9.6.7).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Accompanied {
    Interrupt,
    BulkIn(usize),
    BulkOut(usize),
}

impl InterfaceInfo {
    /// Decode the `GET_DESCRIPTOR(configuration)` bytes into **every**
    /// default-alternate interface of the configuration (up to
    /// [`MAX_INTERFACES`], filled from index `0`): each interface's number
    /// and class triple, its first interrupt-IN endpoint (DCI, packet shape,
    /// `bInterval`), and its first two bulk endpoints each way, each with the
    /// `SuperSpeed` companion following its endpoint descriptor. Walks the
    /// concatenated descriptors by each `bLength` (every endpoint is read,
    /// never assumed). A composite device therefore decodes into one entry
    /// per interface, so each can be served and published separately.
    ///
    /// An interface descriptor with a non-zero `bAlternateSetting` is
    /// skipped along with its endpoints (only the default setting is
    /// selected, USB 2.0 §9.6.5), and an interface with nothing to serve is
    /// decoded but not served ([`Self::is_servable`]), so a servable sibling
    /// still is rather than the whole device rejected. A
    /// second default setting of an interface number already taken is
    /// skipped with its endpoints, and so is an endpoint descriptor naming
    /// endpoint zero or an endpoint the configuration already named: every
    /// decoded interface has a number of its own, and every endpoint a
    /// context of its own.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a non-configuration leading
    /// descriptor, a length running off the buffer or below its minimum,
    /// or no decodable interface at all — a forged or corrupt reply.
    pub fn decode_all(buf: &[u8]) -> Result<[Option<Self>; MAX_INTERFACES], DriverError> {
        let header = ConfigurationHeader::decode(buf).map_err(|Malformed| DriverError::BadMagic)?;
        let configuration_value = header.value;
        let mut out: [Option<Self>; MAX_INTERFACES] = [None; MAX_INTERFACES];
        let mut count = 0usize;
        // The default-alternate interface whose endpoints are being
        // collected; `None` before the first interface descriptor and
        // inside a skipped alternate setting.
        let mut interface: Option<(u8, u32)> = None;
        let mut int_endpoint: Option<(u8, PeriodicShape, u8)> = None;
        // The endpoint the descriptor just read captured, which a SuperSpeed
        // companion following it completes.
        let mut accompanied: Option<Accompanied> = None;
        let mut bulk_in = [BulkEndpoint::default(); 2];
        let mut bulk_out = [BulkEndpoint::default(); 2];
        // The interface numbers a default setting has taken, and the DCIs an
        // endpoint descriptor has named: every served interface of the device
        // shares its slot's endpoint contexts.
        let mut numbered = BitSet256::new();
        let mut claimed = BitSet256::new();
        let body = buf
            .get(usize::from(buf[0])..)
            .ok_or(DriverError::BadMagic)?;
        for descriptor in descriptors(body) {
            let descriptor = descriptor.map_err(|Malformed| DriverError::BadMagic)?;
            let length = descriptor.len();
            let descriptor_type = descriptor[1];
            let follows = accompanied.take();
            match descriptor_type {
                DESC_TYPE_INTERFACE => {
                    if length < INTERFACE_DESCRIPTOR_LEN {
                        return Err(DriverError::BadMagic);
                    }
                    Self::flush_interface(
                        configuration_value,
                        &mut interface,
                        &mut int_endpoint,
                        &mut bulk_in,
                        &mut bulk_out,
                        &mut out,
                        &mut count,
                    );
                    // Only the default alternate setting is served; an
                    // alternate setting's endpoints must never be mistaken
                    // for the default's (USB 2.0 §9.6.5). A second default
                    // setting of one interface is forged, and is skipped with
                    // its endpoints as an alternate setting is.
                    let number = u16::from(descriptor[2]);
                    if descriptor[3] == 0 && !numbered.contains(number) {
                        numbered.insert(number);
                        interface = Some((
                            descriptor[2],
                            (u32::from(descriptor[5]) << 16)
                                | (u32::from(descriptor[6]) << 8)
                                | u32::from(descriptor[7]),
                        ));
                    }
                }
                DESC_TYPE_SS_ENDPOINT_COMPANION if follows.is_some() => {
                    if length < SS_ENDPOINT_COMPANION_LEN {
                        return Err(DriverError::BadMagic);
                    }
                    let max_burst = descriptor[2];
                    match follows {
                        Some(Accompanied::Interrupt) => {
                            if let Some((_, shape, _)) = int_endpoint.as_mut() {
                                shape.companion = Some((
                                    max_burst,
                                    u16::from_le_bytes([descriptor[4], descriptor[5]]),
                                ));
                            }
                        }
                        Some(Accompanied::BulkIn(pipe)) => bulk_in[pipe].max_burst = max_burst,
                        Some(Accompanied::BulkOut(pipe)) => bulk_out[pipe].max_burst = max_burst,
                        None => {}
                    }
                }
                DESC_TYPE_ENDPOINT if interface.is_some() => {
                    accompanied = Self::capture_endpoint(
                        descriptor,
                        &mut claimed,
                        &mut int_endpoint,
                        &mut bulk_in,
                        &mut bulk_out,
                    )?;
                }
                _ => {}
            }
        }
        Self::flush_interface(
            configuration_value,
            &mut interface,
            &mut int_endpoint,
            &mut bulk_in,
            &mut bulk_out,
            &mut out,
            &mut count,
        );
        if out[0].is_none() {
            return Err(DriverError::BadMagic);
        }
        Ok(out)
    }

    /// Capture one endpoint descriptor of the default setting
    /// [`Self::decode_all`] is collecting: its first interrupt-IN endpoint,
    /// and the first two bulk endpoints each way — one pair serves BOT/CBI, a
    /// UAS interface's four pipes need both. Endpoint zero has no endpoint
    /// descriptor, and two endpoints never share a context: a descriptor
    /// claiming either is forged, and is skipped rather than configured over
    /// the context it names, as is a periodic endpoint that moves no bytes.
    /// Returns the endpoint it captured, which a companion following it
    /// completes.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] for a descriptor shorter than an endpoint
    /// descriptor.
    fn capture_endpoint(
        descriptor: &[u8],
        claimed: &mut BitSet256,
        int_endpoint: &mut Option<(u8, PeriodicShape, u8)>,
        bulk_in: &mut [BulkEndpoint; 2],
        bulk_out: &mut [BulkEndpoint; 2],
    ) -> Result<Option<Accompanied>, DriverError> {
        if descriptor.len() < ENDPOINT_DESCRIPTOR_LEN {
            return Err(DriverError::BadMagic);
        }
        let address = descriptor[2];
        let is_in = address & ENDPOINT_ADDR_DIR_IN != 0;
        let endpoint_number = address & ENDPOINT_ADDR_NUMBER_MASK;
        let dci = address_dci(address);
        let max_packet =
            u16::from_le_bytes([descriptor[4], descriptor[5]]) & ENDPOINT_MAX_PACKET_MASK;
        let forged = endpoint_number == 0 || claimed.contains(u16::from(dci));
        claimed.insert(u16::from(dci));
        match descriptor[3] & ENDPOINT_ATTR_TYPE_MASK {
            _ if forged => {}
            ENDPOINT_ATTR_INTERRUPT if is_in && int_endpoint.is_none() && max_packet != 0 => {
                let shape = PeriodicShape {
                    max_packet,
                    transactions: descriptor[5] >> ENDPOINT_TRANSACTIONS_SHIFT & 0b11,
                    companion: None,
                };
                *int_endpoint = Some((dci, shape, descriptor[6]));
                return Ok(Some(Accompanied::Interrupt));
            }
            ENDPOINT_ATTR_BULK => {
                let pipes = if is_in { bulk_in } else { bulk_out };
                if let Some(free) = pipes.iter().position(|pipe| pipe.dci == 0) {
                    pipes[free] = BulkEndpoint {
                        dci,
                        max_packet,
                        max_burst: 0,
                    };
                    return Ok(Some(if is_in {
                        Accompanied::BulkIn(free)
                    } else {
                        Accompanied::BulkOut(free)
                    }));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Complete the interface being collected by [`Self::decode_all`] into
    /// the output set, clearing the collection state for the next one. An
    /// interface beyond the [`MAX_INTERFACES`] bound is ignored rather than
    /// trusted.
    #[allow(clippy::too_many_arguments)] // One decoder's collection state, threaded by reference.
    fn flush_interface(
        configuration_value: u8,
        interface: &mut Option<(u8, u32)>,
        int_endpoint: &mut Option<(u8, PeriodicShape, u8)>,
        bulk_in: &mut [BulkEndpoint; 2],
        bulk_out: &mut [BulkEndpoint; 2],
        out: &mut [Option<Self>; MAX_INTERFACES],
        count: &mut usize,
    ) {
        let int = int_endpoint.take();
        let [bulk_in, bulk_in2] = core::mem::take(bulk_in);
        let [bulk_out, bulk_out2] = core::mem::take(bulk_out);
        let Some((interface_number, class24)) = interface.take() else {
            return;
        };
        let (int_dci, int_shape, int_b_interval) =
            int.unwrap_or((DCI_CONTROL, PeriodicShape::default(), 0));
        if *count >= MAX_INTERFACES {
            return;
        }
        out[*count] = Some(Self {
            configuration_value,
            interface_number,
            class24,
            int_dci,
            int_shape,
            int_b_interval,
            bulk_in,
            bulk_out,
            bulk_in2,
            bulk_out2,
        });
        *count += 1;
    }

    /// Whether this interface carries an endpoint this engine serves: an
    /// interrupt-IN endpoint, or the bulk endpoint pair. A served interface
    /// gets its own device-table entry and its own published hardware-tree
    /// node, whatever its class.
    #[must_use]
    pub const fn is_servable(&self) -> bool {
        self.int_dci != DCI_CONTROL || self.has_bulk_pair()
    }

    /// Whether the matched interface carries the bulk-IN **and** bulk-OUT
    /// endpoint pair a bulk protocol needs (a mass-storage Bulk-Only
    /// Transport interface carries exactly this pair, USB MSC BOT §4).
    /// Only such an interface gets its bulk endpoints configured; a lone
    /// bulk endpoint is left unserved rather than half-configured.
    #[must_use]
    pub const fn has_bulk_pair(&self) -> bool {
        self.bulk_in.dci != 0 && self.bulk_out.dci != 0
    }

    /// Whether this is a mass-storage interface, whose identity rests on its
    /// device's serial number ([`DeviceIdentity::recognises`]).
    #[must_use]
    pub const fn is_mass_storage(&self) -> bool {
        self.class24 >> 16 == INTERFACE_CLASS_MASS_STORAGE
    }
}

/// A device's serial number as its string descriptor spells it: the UTF-16
/// code units exactly as delivered, never decoded or normalised, so two are
/// equal only when every unit is.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct SerialNumber {
    /// Zero past `len`, so the derived equality compares the serial alone.
    units: [u16; MAX_STRING_UNITS],
    len: u8,
}

impl SerialNumber {
    /// The serial spelled by `units`, or `None` for an empty one, which tells
    /// no two devices apart, or one longer than a string descriptor carries
    /// (126 code units).
    #[must_use]
    pub fn new(units: &[u16]) -> Option<Self> {
        if units.is_empty() || units.len() > MAX_STRING_UNITS {
            return None;
        }
        let mut serial = Self {
            units: [0; MAX_STRING_UNITS],
            len: u8::try_from(units.len()).ok()?,
        };
        serial.units[..units.len()].copy_from_slice(units);
        Some(serial)
    }

    /// The serial a string descriptor's payload spells, its UTF-16LE code
    /// units exactly: `None` for a payload that is not whole code units, or
    /// whose units [`Self::new`] refuses.
    #[must_use]
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let (pairs, []) = payload.as_chunks::<2>() else {
            return None;
        };
        let mut units = [0u16; MAX_STRING_UNITS];
        for (unit, pair) in units.iter_mut().zip(pairs) {
            *unit = u16::from_le_bytes(*pair);
        }
        Self::new(units.get(..pairs.len())?)
    }

    fn units(&self) -> &[u16] {
        &self.units[..usize::from(self.len)]
    }
}

impl core::fmt::Debug for SerialNumber {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("SerialNumber").field(&self.units()).finish()
    }
}

/// A string descriptor's header (USB 2.0 §9.6.7), validated: it names a
/// string descriptor whose length is at least the header's, and even, since
/// what follows is UTF-16 code units.
///
/// A string descriptor is read in two requests, this header alone and then
/// the whole descriptor at [`Self::descriptor_len`], so the device is never
/// asked for more than it advertised.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StringHeader(u8);

impl StringHeader {
    /// `bLength` and `bDescriptorType`: what the first request asks for.
    pub const LEN: usize = 2;

    /// The header `answer` carries: `None` unless it is exactly
    /// [`Self::LEN`] bytes of a well-formed string descriptor header.
    #[must_use]
    pub fn decode(answer: &[u8]) -> Option<Self> {
        let &[len, DESC_TYPE_STRING] = answer else {
            return None;
        };
        (usize::from(len) >= Self::LEN && len.is_multiple_of(2)).then_some(Self(len))
    }

    /// The whole descriptor's length, header included: what the second
    /// request asks for.
    #[must_use]
    pub fn descriptor_len(self) -> usize {
        usize::from(self.0)
    }

    /// What follows the header in `answer`, the reply to the second request:
    /// `None` unless it is exactly [`Self::descriptor_len`] bytes opening
    /// with this same header.
    #[must_use]
    pub fn payload(self, answer: &[u8]) -> Option<&[u8]> {
        let (header, payload) = answer.split_first_chunk::<{ Self::LEN }>()?;
        (answer.len() == self.descriptor_len() && Self::decode(header) == Some(self))
            .then_some(payload)
    }
}

/// The language a LANGID table (string descriptor 0's payload) lists first,
/// which a device's strings are read in: `None` for a table listing none, or
/// one that is not whole LANGIDs.
#[must_use]
pub fn first_langid(table: &[u8]) -> Option<u16> {
    let (langids, []) = table.as_chunks::<2>() else {
        return None;
    };
    langids.first().map(|&langid| u16::from_le_bytes(langid))
}

/// Who a served interface is and where it sits on the bus, as its enumeration
/// read it: what the emitted hardware-tree node is built from
/// ([`UsbDevice::describe_device`]), and what a host driver compares
/// ([`Self::recognises`]) to tell a device that came back after a
/// controller reset from one that replaced it.
///
/// Two devices of one model are told apart only by serial number, which is
/// read for a storage device alone: its driver holds state about one medium,
/// so a storage device without a serial is never recognised after a
/// re-enumeration, while two devices of another class swapped between the
/// same two positions compare equal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceIdentity {
    /// The 1-based root-hub port the device hangs off: its own port when
    /// attached directly, its hub tier's otherwise.
    pub root_port: u8,
    /// The hub-port path from that root port to the device: its xHCI Route
    /// String, one downstream port per nibble, `0` when attached directly.
    pub route_string: u32,
    /// `idVendor`.
    pub vendor_id: u16,
    /// `idProduct`.
    pub product_id: u16,
    /// `bcdDevice`.
    pub device_release: u16,
    /// `bDeviceClass`.
    pub device_class: u8,
    /// `bDeviceSubClass`.
    pub device_subclass: u8,
    /// `bDeviceProtocol`.
    pub device_protocol: u8,
    /// `bInterfaceNumber` of the served interface.
    pub interface_number: u8,
    /// The served interface's 24-bit class triple, as [`HwMatchKey::usb`]
    /// carries it.
    pub interface_class: u32,
    /// The device's serial number, read only for a device serving a
    /// mass-storage interface: `None` for any other, for one naming none, or
    /// when its string could not be read whole and well-formed.
    pub serial_number: Option<SerialNumber>,
    /// Whether the device can be unplugged while the machine runs, as its
    /// port reports: a root port through `PORTSC`, a hub's port through the
    /// hub's descriptor.
    pub removable: bool,
}

impl DeviceIdentity {
    /// Whether `reenumerated`, an identity a later enumeration read, is
    /// recognisably the device this one was read from: every fact equal, and
    /// for a storage interface a serial number to prove it, since binding its
    /// driver to another medium corrupts it. Within one enumeration an
    /// identity is the same device's exactly when it is equal.
    #[must_use]
    pub fn recognises(&self, reenumerated: &Self) -> bool {
        self == reenumerated
            && (self.serial_number.is_some()
                || self.interface_class >> 16 != INTERFACE_CLASS_MASS_STORAGE)
    }

    /// The device's position on the bus as one non-zero number, its root
    /// port above its Route String: a device that comes back after a
    /// controller reset keeps it, where the slot it is served on is
    /// reassigned.
    fn bus_position(&self) -> u32 {
        (u32::from(self.root_port) << ROUTE_STRING_BITS) | self.route_string
    }
}

/// Direction of a bulk transfer on the enumerated interface's configured
/// bulk endpoints.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum BulkDirection {
    /// Device → host, on a bulk-IN endpoint.
    In,
    /// Host → device, on a bulk-OUT endpoint.
    Out,
}

/// One of an interface's configured bulk endpoints: its direction and
/// whether it is the second endpoint in that direction (a UAS interface
/// carries two per direction; BOT/CBI use only the primaries).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BulkPipe {
    /// The pipe's data direction.
    pub direction: BulkDirection,
    /// Whether this is the interface's second pipe in that direction.
    pub secondary: bool,
}

impl BulkPipe {
    /// The interface's primary pipe in `direction`.
    pub(crate) const fn primary(direction: BulkDirection) -> Self {
        Self {
            direction,
            secondary: false,
        }
    }

    /// The interface's second pipe in `direction`.
    pub(crate) const fn secondary(direction: BulkDirection) -> Self {
        Self {
            direction,
            secondary: true,
        }
    }

    /// The pipe's place among an interface's four.
    const fn index(self) -> usize {
        (matches!(self.direction, BulkDirection::Out) as usize) << 1 | self.secondary as usize
    }
}

/// A bulk pipe's recovery, while one runs.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum BulkRecovery {
    /// None runs: the pipe serves.
    #[default]
    Idle,
    /// One runs; the TD whose failure started it, answered once it ends.
    Running(Option<(usize, DriverError)>),
}

/// One retired bulk TD, as reported by [`UsbDevice::poll_bulk`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BulkComplete {
    /// Which bulk endpoint the TD ran on.
    pub pipe: BulkPipe,
    /// The transfer-ring data slot the TD occupied (the ticket
    /// [`UsbDevice::queue_bulk_in`] / [`UsbDevice::queue_bulk_out`]
    /// returned), pairing the completion with its submission.
    pub slot: usize,
    /// Bytes actually moved, or the per-transfer failure:
    /// [`DriverError::EndpointStalled`] for a TD the device answered with
    /// STALL (or one
    /// the halt recovery dropped — the endpoint is already recovered when
    /// this is delivered), [`DriverError::DeviceFault`] for a hard
    /// controller/device error on this TD.
    pub result: Result<u32, DriverError>,
}

/// The bulk transfer rings configured for one interface: the primary
/// IN/OUT pair every bulk interface carries, plus the second pair a UAS
/// interface's four pipes need.
#[allow(clippy::struct_field_names)] // Every field *is* a ring — the struct
                                     // is exactly the set of an interface's bulk rings.
struct BulkRings {
    in_ring: ProducerRing,
    out_ring: ProducerRing,
    in2_ring: Option<ProducerRing>,
    out2_ring: Option<ProducerRing>,
}

/// The endpoint rings configured for one planned interface, held until
/// the device-table entries are installed after the EP0 transfers of
/// enumeration complete.
struct ConfiguredRings {
    /// The interrupt-IN transfer ring: a HID interface's report endpoint,
    /// or a bulk interface's CBI completion endpoint.
    int_ring: Option<ProducerRing>,
    /// The bulk transfer rings, for a bulk interface.
    bulk_rings: Option<BulkRings>,
}

/// Parked-completion capacity: every data slot of all four bulk rings
/// could complete while a synchronous EP0 transfer or command is awaiting
/// its own event, so the FIFO holds the worst case. A protocol working
/// set, not a scalable capacity.
const BULK_QUEUE_CAP: usize = 4 * BULK_SLOTS;

/// Buffered interrupt-IN reports held per device between the controller
/// capturing them and the class driver collecting them.
///
/// The controller keeps [`INT_ARM_DEPTH`] transfers armed, so one drain can
/// capture that many completions; this buffer additionally covers a class
/// driver that stays several report intervals behind (a full ring's worth of
/// backlog) before the oldest report is dropped. A bounded protocol working
/// set, not a scalable capacity: a genuinely dead consumer cannot make the
/// engine hold unbounded memory, and a live one catching up sees the most
/// recent device state. Sized at least as deep as the armed depth so a single
/// drain never overflows.
pub const REPORT_QUEUE_CAP: usize = 16;

const _: () = assert!(REPORT_QUEUE_CAP >= INT_ARM_DEPTH);

/// One device's captured interrupt-IN reports, [`REPORT_QUEUE_CAP`] deep,
/// each up to the endpoint's transfer length.
///
/// Copied out of the DMA slot at capture time so the slot can be re-armed at
/// once, decoupling device polling from how promptly the class driver runs.
/// Its memory is sized once, from the transfer length, at the class driver's
/// first report request.
#[derive(Default)]
struct ReportQueue {
    bytes: Vec<u8>,
    lens: [u16; REPORT_QUEUE_CAP],
    slot: usize,
    head: usize,
    len: usize,
}

impl ReportQueue {
    /// Hold reports of up to `slot` bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] when memory for them runs out.
    fn size_for(&mut self, slot: usize) -> Result<(), DriverError> {
        let total = slot
            .checked_mul(REPORT_QUEUE_CAP)
            .ok_or(DriverError::LengthOutOfRange)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| DriverError::OutOfMemory)?;
        bytes.resize(total, 0);
        *self = Self {
            bytes,
            slot,
            ..Self::default()
        };
        Ok(())
    }

    /// Buffer a report of `len` bytes that `fill` writes, dropping the
    /// oldest when the queue is full; returns whether a report was lost, the
    /// oldest or this one when it cannot fit. Nothing is kept if `fill` fails.
    ///
    /// # Errors
    ///
    /// What `fill` returns.
    fn push_with<E>(
        &mut self,
        len: usize,
        fill: impl FnOnce(&mut [u8]) -> Result<(), E>,
    ) -> Result<bool, E> {
        let Some(stored) = u16::try_from(len).ok().filter(|_| len <= self.slot) else {
            return Ok(true);
        };
        let lost = self.len == REPORT_QUEUE_CAP;
        if lost {
            self.head = (self.head + 1) % REPORT_QUEUE_CAP;
            self.len -= 1;
        }
        let at = (self.head + self.len) % REPORT_QUEUE_CAP;
        fill(&mut self.bytes[at * self.slot..][..len])?;
        self.lens[at] = stored;
        self.len += 1;
        Ok(lost)
    }

    /// Move the oldest report into `buf`, returning its length.
    ///
    /// # Errors
    ///
    /// [`DriverError::BufferTooSmall`] when `buf` cannot hold it; it stays
    /// queued.
    fn pop_into(&mut self, buf: &mut [u8]) -> Result<Option<usize>, DriverError> {
        if self.len == 0 {
            return Ok(None);
        }
        let len = usize::from(self.lens[self.head]);
        buf.get_mut(..len)
            .ok_or(DriverError::BufferTooSmall)?
            .copy_from_slice(&self.bytes[self.head * self.slot..][..len]);
        self.head = (self.head + 1) % REPORT_QUEUE_CAP;
        self.len -= 1;
        Ok(Some(len))
    }

    /// Drop every queued report.
    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Upper bound on the events one service step routes before yielding.
///
/// The loop exits as soon as the shared event ring is empty; this is only a
/// safety cap against a controller that never stops presenting events. It
/// covers the worst case of every served slot having its full interrupt and
/// bulk depth armed at once, so a legitimate backlog always drains in a single
/// pass.
const EVENT_DRAIN_BOUND: usize = (XHCI_MAX_SLOTS + 1) * (INT_ARM_DEPTH + BULK_QUEUE_CAP);

/// The bus a protocol speed ID names. An ID past the four defaults is a
/// `SuperSpeedPlus` rate the port's protocol capability defines, whose
/// intervals are 125 µs like every speed above full.
pub(crate) fn bus_speed(speed: u8) -> UsbSpeed {
    UsbSpeed::from_u8(speed).unwrap_or(UsbSpeed::Super)
}

/// The xHCI endpoint-context Interval (§6.2.3.6) for an interrupt endpoint
/// reporting `b_interval` at protocol `speed`.
pub(crate) fn interrupt_interval(speed: u8, b_interval: u8) -> u32 {
    u32::from(ServiceInterval::interrupt(bus_speed(speed), b_interval).exponent())
}

/// Input control context dwords (§6.2.5.1): dword 0 carries the Drop
/// Context flags (`D(dci)` = drop that endpoint), dword 1 the Add Context
/// flags (`A0` = slot context, `A(dci)` = that endpoint).
fn input_control_dwords(drop_flags: u32, add_flags: u32) -> [u32; CTX_DWORDS] {
    let mut dwords = [0; CTX_DWORDS];
    dwords[0] = drop_flags;
    dwords[1] = add_flags;
    dwords
}

/// The topology fields of a device's slot context (xHCI §6.2.2) that
/// stay constant across both Address Device and Configure Endpoint for
/// one device: its protocol speed ID, the root-hub port it is reached
/// through, and — for a device *downstream* of a hub — the Route String
/// and transaction-translator (TT) coordinates.
///
/// A device directly on a root-hub port carries route string `0` and no
/// TT. A full/low-speed device behind a high-speed hub additionally
/// names that hub's slot and downstream port as its TT, so the
/// controller splits its transactions (`speed_needs_tt`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct SlotCtxBase {
    /// xHCI protocol speed ID ([`hub_port_speed`] / [`ep0_max_packet`]).
    speed: u8,
    /// The 1-based root-hub port the device is reached through (the hub's
    /// own root port for a downstream device).
    root_port: u8,
    /// Route String: the chain of downstream hub ports from the
    /// root to the device, four bits per tier. `0` for a root-port
    /// device.
    route_string: u32,
    /// TT Hub Slot ID (§6.2.2): the slot of the high-speed hub providing
    /// the transaction translator, or `0` when the device needs none.
    tt_hub_slot: u8,
    /// TT Port Number (§6.2.2): the hub's 1-based downstream port the
    /// device is attached to, or `0` when the device needs no TT.
    tt_port: u8,
}

/// Endpoint context dword 0 **EP State** field (§6.2.3): the controller's to
/// report, zero in an input context.
const EP_CTX_STATE_MASK: u32 = 0b111;

/// The flag bits below the TR Dequeue Pointer in endpoint context dwords
/// 2–3 (§6.2.3): the Dequeue Cycle State and the reserved bits.
const EP_CTX_DEQUEUE_FLAGS: u64 = 0xF;

/// Slot context dword 0 **Hub** bit (§6.2.2): the device on this slot is
/// a USB hub. The controller routes packets to — and, with the TT
/// fields, splits the transactions of — devices addressed downstream of
/// it only when this is set, so a keyboard behind the hub never receives
/// its interrupt transfers otherwise.
const SLOT_CTX_HUB: u32 = 1 << 26;
/// Slot context dword 0 **Multi-TT** bit (§6.2.2): the hub exposes one
/// transaction translator per port. The Pi 4B's onboard VIA hub is
/// single-TT, so this stays clear.
const SLOT_CTX_MTT: u32 = 1 << 25;
/// Slot context dword 0 **Context Entries** field shift (§6.2.2): the index
/// of the last valid endpoint context in the device context. Raised when an
/// endpoint at a higher DCI (e.g. the hub's status-change endpoint) is added.
const SLOT_CTX_CONTEXT_ENTRIES_SHIFT: u32 = 27;
/// Slot context dword 0 **Context Entries** field mask (five bits).
const SLOT_CTX_CONTEXT_ENTRIES_MASK: u32 = 0x1F << SLOT_CTX_CONTEXT_ENTRIES_SHIFT;
/// Slot context dword 1 **Number of Ports** field shift (§6.2.2): a
/// hub's downstream port count, used by the controller for periodic
/// transfer scheduling.
const SLOT_CTX_NUM_PORTS_SHIFT: u32 = 24;
/// Slot context dword 2 **TT Think Time** field shift and mask
/// (§6.2.2): the inter-transaction gap the hub's TT needs, in FS bit
/// times, copied from the hub descriptor's `wHubCharacteristics`.
const SLOT_CTX_TTT_SHIFT: u32 = 16;
const SLOT_CTX_TTT_MASK: u32 = 0b11 << SLOT_CTX_TTT_SHIFT;

/// Slot context dwords (§6.2.2): the Route String and protocol speed ID
/// (dword 0), context entries (the highest DCI in use) and the root-hub
/// port number (dword 1), and the transaction-translator coordinates
/// (dword 2) for a full/low-speed device behind a high-speed hub.
fn slot_ctx_dwords(base: SlotCtxBase, context_entries: u32) -> [u32; CTX_DWORDS] {
    let mut dwords = [0; CTX_DWORDS];
    dwords[0] =
        (base.route_string & 0x000F_FFFF) | (u32::from(base.speed) << 20) | (context_entries << 27);
    dwords[1] = u32::from(base.root_port) << 16;
    dwords[2] = u32::from(base.tt_hub_slot) | (u32::from(base.tt_port) << 8);
    dwords
}

/// A periodic endpoint's service: its Interval (§6.2.3.6), Max ESIT
/// Payload ([`PeriodicShape::payload`]) and, for a `SuperSpeed` isochronous
/// endpoint, its Mult.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Periodic {
    pub(crate) interval: u32,
    pub(crate) payload: u32,
    pub(crate) mult: u32,
}

/// Endpoint context dwords (§6.2.3): the error count, endpoint type, max
/// packet size, Max Burst Size, the transfer-ring dequeue pointer with
/// Dequeue Cycle State 1, and the average TRB length; for a `periodic`
/// endpoint also its Interval, Mult and Max ESIT Payload, split across
/// dwords 0 and 4 (§6.2.3.8). A periodic endpoint **must** carry a non-zero
/// Max ESIT Payload or the scheduler reserves no bandwidth and no transfer
/// runs (§4.14.2); its average TRB length is that payload. An isochronous
/// endpoint's error count is zero: a late packet is not retried.
pub(crate) fn ep_ctx_dwords(
    ep_type: u32,
    max_packet: u32,
    max_burst: u32,
    ring: u64,
    periodic: Option<Periodic>,
) -> [u32; CTX_DWORDS] {
    let mut dwords = [0; CTX_DWORDS];
    let Periodic {
        interval,
        payload,
        mult,
    } = periodic.unwrap_or(Periodic {
        interval: 0,
        payload: 0,
        mult: 0,
    });
    let error_count = if matches!(ep_type, EP_TYPE_ISOCH_OUT | EP_TYPE_ISOCH_IN) {
        0
    } else {
        3
    };
    dwords[0] = (mult << 8) | (interval << 16) | ((payload >> 16) << 24);
    dwords[1] = (error_count << 1) | (ep_type << 3) | (max_burst << 8) | (max_packet << 16);
    let dequeue = ring | 1;
    dwords[2] = crate::low_dword(dequeue);
    dwords[3] = crate::high_dword(dequeue);
    let average = if periodic.is_some() {
        payload
    } else {
        max_packet
    };
    dwords[4] = (average & 0xFFFF) | ((payload & 0xFFFF) << 16);
    dwords
}

/// Publish one [`PushOutcome`] into the ring at `ring_offset`: the
/// data TRB first, then — when the push wrapped — the re-cycled Link
/// TRB (§4.9.2.1 ordering).
fn publish<M: DmaBank>(
    dma: &mut M,
    ring_offset: usize,
    link_slot: usize,
    outcome: &PushOutcome,
) -> Result<(), DriverError> {
    dma.write(
        ring_offset + outcome.slot * trb::TRB_LEN,
        &outcome.trb.to_bytes(),
    )?;
    if let Some(link) = outcome.link {
        dma.write(ring_offset + link_slot * trb::TRB_LEN, &link.to_bytes())?;
    }
    Ok(())
}

/// The step the most recent enumeration last entered, a breadcrumb so a
/// capture can localise which xHCI operation a coarse
/// [`DriverError::DeviceFault`] came from. Stays at [`EnumStage::Scan`]
/// until a connected port enters enumeration, so an empty-hub
/// [`DriverError::NotFound`] stays distinguishable. Variants follow the
/// enumeration sequence.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EnumStage {
    /// Before (or between) any per-device step: scanning the root hub.
    Scan = 0,
    /// Resetting a connected-but-not-yet-enabled port.
    PortReset = 1,
    /// Enable Slot command (§6.4.3.2).
    EnableSlot = 2,
    /// Address Device command (§6.4.3.4).
    AddressDevice = 3,
    /// `GET_DESCRIPTOR(device)` control transfer (§9.4.3).
    GetDeviceDescriptor = 4,
    /// `GET_DESCRIPTOR(configuration)` control transfer (§9.4.3).
    GetConfigDescriptor = 5,
    /// `GET_DESCRIPTOR(string)` control transfers (§9.4.3): the LANGID table,
    /// then the serial number.
    GetStringDescriptor = 13,
    /// Configure Endpoint command (§6.4.3.5).
    ConfigureEndpoint = 6,
    /// `SET_CONFIGURATION` control transfer (§9.4.7).
    SetConfiguration = 7,
    /// Enumeration completed: the device is configured and ready for a class URB.
    Configured = 11,
}

impl EnumStage {
    /// Raw discriminant, for an allocation-free diagnostic log.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Whether this is a step of *establishing the device's control pipe* —
    /// assigning its address and reading the descriptors enumeration cannot
    /// go on without — as opposed to the optional string reads and the later
    /// configuration.
    ///
    /// A transaction fault in this phase is a device disturbed while it is
    /// still being brought up, which a port reset and a fresh slot re-drive
    /// (see [`ENUM_ATTEMPTS`]). One *after* it is surfaced as the fault it
    /// is, or, on a read enumeration can do without, costs only that read.
    const fn is_pipe_bringup(self) -> bool {
        matches!(
            self,
            Self::EnableSlot
                | Self::AddressDevice
                | Self::GetDeviceDescriptor
                | Self::GetConfigDescriptor
        )
    }
}

/// The diagnostics of a failed attach, as the port's own operation saw
/// them: each operation keeps its own, so ports attaching side by side never
/// overwrite each other's.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AttachFault {
    /// The 1-based root or downstream hub port whose attach failed.
    pub port: u8,
    /// The surfaced [`DriverError`].
    pub error: DriverError,
    /// The enumeration step the attach failed in ([`EnumStage::PortReset`]
    /// = the port never reported reset-complete + enabled).
    pub stage: EnumStage,
    /// Raw completion code of the event that ended the failing command or
    /// transfer (`0` = none).
    pub completion: u8,
    /// Raw TRB-type of that event (`0` = none).
    pub event_type: u8,
    /// Whether no event ended the failing command or transfer before its
    /// deadline.
    pub timed_out: bool,
    /// The raw `wPortStatus` the attach's reset-completion wait last
    /// observed (`0` = none read): at a "port never enabled" fault, the
    /// port's final connect/enable/reset/speed state as the hub reported
    /// it.
    pub port_status: u16,
}

/// What the controller said of a transfer abandoned at its deadline: the
/// endpoint's state then, and where its dequeue pointer stood once the
/// endpoint had stopped — the only state in which that pointer is the
/// controller's own (xHCI 1.2 §4.6.9).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AbandonedTransfer {
    /// The endpoint's Device Context Index.
    pub dci: u8,
    /// Its EP State at the deadline (xHCI 1.2 §6.2.3): `1` running, `2`
    /// halted, `3` stopped, `4` error.
    pub ep_state: u8,
    /// Where the stopped controller stood against the transfer.
    pub progress: TdProgress,
}

/// How far the controller had got through a transfer's TRBs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TdProgress {
    /// On its first TRB: nothing of it had completed.
    NotStarted,
    /// On a later TRB of it: a control transfer's data or status stage.
    Underway,
    /// On the TRB after its last: the controller had finished it, and its
    /// completion was lost.
    Completed,
    /// On none of its TRBs nor the one after: the ring was not where the
    /// engine had left it.
    Elsewhere,
}

impl TdProgress {
    /// Stable diagnostic name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Underway => "underway",
            Self::Completed => "completed",
            Self::Elsewhere => "elsewhere",
        }
    }
}

/// Where a transfer lay on its ring, for a stopped controller's dequeue
/// pointer to be read against.
#[derive(Clone, Debug)]
pub(crate) struct TdSpan {
    /// Its TRBs, the first first.
    trbs: ArrayVec<u64, 3>,
    /// The TRB after its last, the wrap link followed.
    next: u64,
    /// The ring's wrap link, which a dequeue pointer may rest on in place of
    /// the base it leads to.
    link: u64,
    base: u64,
}

impl TdSpan {
    /// The transfer whose TRBs on `ring` are `trbs`, the first first.
    pub(crate) fn on(ring: &ProducerRing, trbs: ArrayVec<u64, 3>) -> Option<Self> {
        Some(Self {
            next: ring.following(*trbs.last()?)?,
            link: ring.link_address()?,
            base: ring.base(),
            trbs,
        })
    }

    /// Where a stopped controller whose dequeue pointer is `dequeue` stood
    /// against the transfer.
    pub(crate) fn progress(&self, dequeue: u64) -> TdProgress {
        let dequeue = if dequeue == self.link {
            self.base
        } else {
            dequeue
        };
        match self.trbs.iter().position(|&trb| trb == dequeue) {
            Some(0) => TdProgress::NotStarted,
            Some(_) => TdProgress::Underway,
            None if dequeue == self.next => TdProgress::Completed,
            None => TdProgress::Elsewhere,
        }
    }
}

/// A transfer abandoned at its deadline whose endpoint is not yet stopped.
#[derive(Clone, Debug)]
struct PendingAbandon {
    slot: u8,
    dci: u8,
    ep_state: u8,
    span: TdSpan,
}

/// What a port's operation did to the topology, as the service step reports
/// it ([`Serviced::topology`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HubEvent {
    /// A device connected on a downstream or root port and was enumerated
    /// as a fresh device at the carried device index; the HCD emits a new
    /// interface node for it. Re-attach is always a brand-new
    /// enumeration — no prior state is reused.
    Attached(usize),
    /// The device at the carried index disconnected; its slot has been
    /// freed. The HCD retracts the interface node it published.
    Detached(usize),
    /// A **hub** connected on a downstream or root port and was installed,
    /// descended, and watched at the carried hub-table index; any devices
    /// found behind it are already served, so the HCD reconciles its
    /// published nodes against the live device table.
    HubAttached(usize),
    /// The hub at the carried hub-table index disconnected; it and every
    /// device and deeper hub tier behind it have been freed. The HCD
    /// reconciles its published nodes against the live device table.
    HubDetached(usize),
    /// The hub at the carried hub-table index failed a status-change transfer
    /// and its endpoint was recovered in place, every downstream port then
    /// reconciled: whatever the lost report carried, and a device freed while
    /// its port went on carrying it, is acted on. The HCD reconciles its
    /// published nodes against the live device table.
    HubRecovered(usize),
}

/// One addressed, watched USB hub: its slot, its place in the hub tree
/// (parent hub and port, route string, depth), the topology fields its
/// downstream devices inherit (speed, TT coordinates), its layout region,
/// and its status-change endpoint state.
///
/// The engine keeps every hub addressed concurrently — the root-attached
/// hub and each hub plugged into a hub — so each tier's status-change
/// endpoint is watched event-driven and its per-port class requests can be
/// issued at any time.
struct HubState {
    /// The hub's xHCI slot (never `0` while the entry is live).
    slot: u8,
    /// The attachment serial telling this hub from a later one at the same
    /// table index.
    serial: u64,
    /// Hub-table index of the parent hub, `None` for the root-attached hub.
    parent: Option<usize>,
    /// The parent hub's 1-based downstream port this hub hangs off, `0`
    /// for a root-attached hub.
    parent_port: u8,
    /// The 1-based root-hub port this tier ultimately hangs off: its own
    /// port for a root-attached hub, the parent's inherited value for a
    /// deeper tier. Carried into every downstream slot context (xHCI
    /// §6.2.2), and the root port its tier's operations reconcile.
    root_port: u8,
    /// The hub's own Route String (xHCI §8.9.1): `0` for the root-attached
    /// hub; a downstream hub extends its parent's by one nibble.
    route_string: u32,
    /// Hub tiers above this hub (`0` = root-attached), i.e. the number of
    /// route-string nibbles already in use. Bounded by [`MAX_HUB_DEPTH`].
    depth: u8,
    /// The hub's xHCI protocol speed ID, deciding whether a full/low-speed
    /// device below it uses this hub's transaction translator (high-speed
    /// hub) or inherits this hub's own TT coordinates.
    speed: u8,
    /// Downstream port count from the hub class descriptor.
    num_ports: u8,
    /// The downstream ports whose device is built in, from the same
    /// descriptor ([`HubDescriptor::fixed_ports`]).
    fixed_ports: u32,
    /// TT coordinates carried in this hub's own slot context (§6.2.2):
    /// the nearest high-speed ancestor's `(slot, port)` when this hub is
    /// full/low-speed behind one, else `(0, 0)`. A full/low-speed device
    /// below a non-high-speed hub inherits these.
    tt_hub_slot: u8,
    tt_port: u8,
    /// The [`HubRegion`] holding this hub's status-change ring and report
    /// buffer.
    region: HubRegion,
    /// The device-region table index this hub's contexts live on: a hub is
    /// enumerated on a freshly claimed device region before it is known to
    /// be a hub and keeps it for its lifetime (the entry is excluded from
    /// [`UsbDevice::claim_device_entry`]'s reuse while claimed, and retired
    /// on detach).
    device_region: usize,
    /// The hub's interrupt-IN status-change endpoint as
    /// `(dci, max_packet, interval)`, captured from its configuration
    /// descriptor during enumeration so the watch can be configured once
    /// the slot is marked a hub. `None` when the hub reported none.
    int_endpoint: Option<(u8, u32, u32)>,
    /// Device Context Index of the armed status-change endpoint. Valid
    /// only while [`Self::int_ring`] is live.
    int_dci: u8,
    /// The status-change endpoint's interrupt-IN producer ring (over the
    /// region's `int_ring`). `None` until the watch is configured and
    /// armed.
    int_ring: Option<ProducerRing>,
    /// A status-change completion observed while another transfer was
    /// awaiting its event, parked for the hub watcher to consume. Only one
    /// status-change transfer is ever armed, so a single slot suffices —
    /// unlike a device's report queue, which buffers several in-flight
    /// interrupt-IN completions.
    pending: Option<Trb>,
    /// The status-change endpoint's recovery ladder.
    watch: Ladder,
}

impl HubState {
    /// The port the hub hangs off.
    const fn upstream(&self) -> Upstream {
        match self.parent {
            None => Upstream::Root(self.root_port),
            Some(parent) => Upstream::Hub(parent, self.parent_port),
        }
    }
}

/// One concurrently served, enumerated device: its slot, topology, layout
/// region, endpoint state, and the completions parked for it while another
/// transfer owned the shared event ring.
struct DeviceState {
    /// The device's xHCI slot (never `0` while the entry is live).
    slot: u8,
    /// The attachment serial telling this interface from a later one at the
    /// same table index.
    serial: u64,
    /// The control transfer the interface's URB has queued, reaped on a
    /// later drive.
    urb_control: Option<route::ControlId>,
    /// The request the interface holds while its operation runs.
    request: Option<urb::Request>,
    /// Each bulk pipe's recovery, by [`BulkPipe::index`]: a pipe being
    /// recovered arms nothing, and the TD whose failure started it is
    /// answered once it ends.
    bulk_recovery: [BulkRecovery; 4],
    /// The device's xHCI protocol speed ID.
    speed: u8,
    /// The configuration descriptor the device was configured with, as it
    /// answered: what its alternate settings and claimable interfaces are
    /// read from.
    config: Vec<u8>,
    /// The interfaces the node claimed and the settings selected on them.
    streaming: Streaming,
    /// The hub downstream port the device hangs off (1-based), `0` for a
    /// directly-attached root device.
    hub_port: u8,
    /// Hub-table index of the hub the device hangs off. Meaningful only
    /// while [`Self::hub_port`] is non-zero; a directly-attached root
    /// device has no parent hub.
    parent_hub: usize,
    /// The [`Layout`] region holding this device's endpoint rings and
    /// buffers.
    region: DeviceRegion,
    /// The served interface's identity and position: a directly-attached
    /// device's fate is decided by its root port's operation.
    identity: DeviceIdentity,
    /// Device Context Index of the device's interrupt-IN endpoint, read
    /// from its endpoint descriptor during enumeration (§4.5.1).
    /// [`DCI_CONTROL`] when the interface carries none (a bulk interface).
    int_dci: u8,
    /// The interrupt-IN endpoint's Max ESIT Payload: the bytes one service
    /// interval moves, the least a transfer is armed to. `0` when the
    /// interface carries no interrupt-IN endpoint.
    int_payload: u16,
    /// The length every interrupt-IN transfer is armed to, fixed by the class
    /// driver's first report request; `None` until it makes one, and nothing
    /// is armed before. No longer than a report needs: a full/low-speed
    /// endpoint behind a high-speed hub faults with a Split Transaction Error
    /// when a transfer outruns the interval budget its transaction translator
    /// scheduled (the Pi 4 keyboard behind its hub).
    int_transfer: Option<u16>,
    /// The request [`Self::int_transfer`] was fixed from.
    int_request: u16,
    /// Interrupt-IN transfer ring over the region's `int_ring`, live only
    /// for an interface with an interrupt-IN endpoint.
    int_ring: Option<ProducerRing>,
    /// Interrupt-IN reports captured off the controller's ring, buffered
    /// until a class-driver URB collects them. Filled by
    /// [`UsbDevice::capture_report_event`] on every controller interrupt
    /// (independent of whether a URB is outstanding), drained by
    /// [`UsbDevice::next_report`]. Several transfers are armed at once
    /// ([`INT_ARM_DEPTH`]), so more than one completion can be pending —
    /// a queue, not a single slot.
    reports: ReportQueue,
    /// A fail-closed report completion — a successful one whose buffer mapping
    /// the decode refused — recorded for the next [`UsbDevice::next_report`] to
    /// surface once. A capture never propagates such a fault synchronously, so
    /// it can neither fault an unrelated EP0/command wait that happened to
    /// observe the report nor the shared drain.
    report_fault: Option<DriverError>,
    /// Count of buffered reports dropped because the class driver fell more
    /// than [`REPORT_QUEUE_CAP`] reports behind (a genuinely stalled
    /// consumer). Surfaced for diagnostics; never silently ignored.
    dropped_reports: u64,
    /// A completion arrived for the interrupt-IN endpoint while a recovery of
    /// it ran, so the recovery acts on it once it ends: it halted the
    /// endpoint again, or the recovery's own rebuild discarded it.
    int_recovery_pending: bool,
    /// A recovery of the interrupt-IN endpoint runs, and its ring is the
    /// recovery's: a capture leaves it alone and flags
    /// [`Self::int_recovery_pending`] instead.
    int_recovering: bool,
    /// The interrupt-IN endpoint's recovery ladder. Nothing is armed on the
    /// endpoint while a retry is owed, and the verdict on the device is the
    /// retry's ([`UsbHost::retry_owed_recoveries`]).
    recovery: Ladder,
    /// Bulk-IN transfer ring over the region's `bulk_in_ring`, built when
    /// the interface's bulk endpoint pair is configured. `None` otherwise.
    bulk_in_ring: Option<ProducerRing>,
    /// Bulk-OUT transfer ring, as [`Self::bulk_in_ring`].
    bulk_out_ring: Option<ProducerRing>,
    /// The second bulk-IN ring (a UAS interface's second IN pipe), `None`
    /// when the interface declares fewer than two.
    bulk_in2_ring: Option<ProducerRing>,
    /// The second bulk-OUT ring, as [`Self::bulk_in2_ring`].
    bulk_out2_ring: Option<ProducerRing>,
    /// Device Context Index of the configured bulk-IN endpoint (`0` = none).
    bulk_in_dci: u8,
    /// Device Context Index of the configured bulk-OUT endpoint (`0` = none).
    bulk_out_dci: u8,
    /// DCI of the second configured bulk-IN endpoint (`0` = none).
    bulk_in2_dci: u8,
    /// DCI of the second configured bulk-OUT endpoint (`0` = none).
    bulk_out2_dci: u8,
    /// Requested byte length of each in-flight bulk-IN TD, by ring data
    /// slot, so a completion's residual decodes into bytes transferred.
    bulk_in_len: [u32; BULK_SLOTS],
    /// As [`Self::bulk_in_len`], for the bulk-OUT ring.
    bulk_out_len: [u32; BULK_SLOTS],
    /// As [`Self::bulk_in_len`], for the second bulk-IN ring.
    bulk_in2_len: [u32; BULK_SLOTS],
    /// As [`Self::bulk_in_len`], for the second bulk-OUT ring.
    bulk_out2_len: [u32; BULK_SLOTS],
    /// Bulk completions parked while a synchronous EP0 transfer or command
    /// was awaiting its own event. Several bulk TDs can be outstanding at
    /// once, so this is a FIFO — as is the interrupt-IN report buffer
    /// ([`Self::reports`]).
    pending_bulk: RingBuf<Trb, BULK_QUEUE_CAP>,
    /// TDs a bulk pipe recovery dropped, each with why, so every queued
    /// transfer is answered, never silently lost.
    aborted_bulk: RingBuf<(BulkPipe, usize, DriverError), BULK_QUEUE_CAP>,
    /// Raw completion code of the most recent bulk transfer event that did
    /// not succeed, `0` when none has failed or the last failure was a
    /// transfer abandoned at its deadline, which no completion ended.
    last_bulk_fault_code: u8,
    /// Raw completion code of the most recent interrupt-IN transfer event
    /// this device's report path *rejected* (a non-`Success`/`ShortPacket`
    /// code). Unlike the engine-wide diagnostics this is not reset by a
    /// later control transfer, so it survives the port reads a failed
    /// recovery's retry issues — the only place the controller's verdict on
    /// the device's own endpoint can still be read. `0` until a report has
    /// been rejected.
    last_report_fault_code: u8,
    /// The transfer last abandoned at its deadline, until its endpoint stops.
    abandoning: Option<PendingAbandon>,
}

impl DeviceState {
    /// The port the device hangs off.
    const fn upstream(&self) -> Upstream {
        if self.hub_port == 0 {
            Upstream::Root(self.identity.root_port)
        } else {
            Upstream::Hub(self.parent_hub, self.hub_port)
        }
    }

    /// Whether the node governs `interface`: its own, or one it claimed.
    fn governs(&self, interface: u8) -> bool {
        self.identity.interface_number == interface || self.streaming.claimed(interface)
    }

    /// The engine's own pipes on the interface's default setting — its
    /// interrupt-IN endpoint and bulk endpoints — as a mask of Device Context
    /// Indices.
    fn pipe_dci_mask(&self) -> u32 {
        let int = if self.int_ring.is_some() {
            self.int_dci
        } else {
            0
        };
        [
            int,
            self.bulk_in_dci,
            self.bulk_out_dci,
            self.bulk_in2_dci,
            self.bulk_out2_dci,
        ]
        .into_iter()
        .filter(|&dci| dci > DCI_CONTROL && u32::from(dci) < u32::BITS)
        .fold(0, |mask, dci| mask | 1 << dci)
    }

    /// Fix the length interrupt-IN transfers are armed to from the class
    /// driver's `request`, the longest report it expects: that, or one
    /// service interval's payload when it is longer. A later request must
    /// name the same length.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for a `request` of zero or past
    /// [`INT_TRANSFER_MAX`], for a payload no transfer buffer holds, or when
    /// memory for the report queue runs out; [`DriverError::OutOfRange`] for
    /// a request other than the first.
    fn fix_int_transfer(&mut self, request: usize) -> Result<usize, DriverError> {
        let wanted = u16::try_from(request)
            .ok()
            .filter(|&len| len != 0 && usize::from(len) <= INT_TRANSFER_MAX)
            .ok_or(DriverError::LengthOutOfRange)?;
        if let Some(fixed) = self.int_transfer {
            return if self.int_request == wanted {
                Ok(usize::from(fixed))
            } else {
                Err(DriverError::OutOfRange)
            };
        }
        let transfer = wanted.max(self.int_payload);
        if usize::from(transfer) > INT_TRANSFER_MAX {
            return Err(DriverError::LengthOutOfRange);
        }
        self.reports.size_for(usize::from(transfer))?;
        self.int_request = wanted;
        self.int_transfer = Some(transfer);
        Ok(usize::from(transfer))
    }

    /// The configured DCI of `pipe` (`0` = the pipe does not exist).
    fn bulk_dci(&self, pipe: BulkPipe) -> u8 {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_dci,
            (BulkDirection::In, true) => self.bulk_in2_dci,
            (BulkDirection::Out, false) => self.bulk_out_dci,
            (BulkDirection::Out, true) => self.bulk_out2_dci,
        }
    }

    /// The configured pipe whose endpoint is `dci`, `None` when no bulk
    /// pipe of this device uses it.
    fn bulk_pipe_of_dci(&self, dci: u8) -> Option<BulkPipe> {
        if dci == 0 {
            return None;
        }
        [
            BulkPipe::primary(BulkDirection::In),
            BulkPipe::primary(BulkDirection::Out),
            BulkPipe::secondary(BulkDirection::In),
            BulkPipe::secondary(BulkDirection::Out),
        ]
        .into_iter()
        .find(|&pipe| self.bulk_dci(pipe) == dci)
    }

    /// Borrow `pipe`'s transfer ring, `None` when unconfigured.
    fn bulk_ring(&self, pipe: BulkPipe) -> Option<&ProducerRing> {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_ring.as_ref(),
            (BulkDirection::In, true) => self.bulk_in2_ring.as_ref(),
            (BulkDirection::Out, false) => self.bulk_out_ring.as_ref(),
            (BulkDirection::Out, true) => self.bulk_out2_ring.as_ref(),
        }
    }

    /// Mutably borrow `pipe`'s transfer ring.
    fn bulk_ring_mut(&mut self, pipe: BulkPipe) -> Option<&mut ProducerRing> {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_ring.as_mut(),
            (BulkDirection::In, true) => self.bulk_in2_ring.as_mut(),
            (BulkDirection::Out, false) => self.bulk_out_ring.as_mut(),
            (BulkDirection::Out, true) => self.bulk_out2_ring.as_mut(),
        }
    }

    /// Replace `pipe`'s transfer ring (the halt recovery's rebuild).
    fn set_bulk_ring(&mut self, pipe: BulkPipe, ring: ProducerRing) {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_ring = Some(ring),
            (BulkDirection::In, true) => self.bulk_in2_ring = Some(ring),
            (BulkDirection::Out, false) => self.bulk_out_ring = Some(ring),
            (BulkDirection::Out, true) => self.bulk_out2_ring = Some(ring),
        }
    }

    /// The requested length recorded for `pipe`'s ring data `slot`.
    fn bulk_len(&self, pipe: BulkPipe, slot: usize) -> u32 {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_len[slot],
            (BulkDirection::In, true) => self.bulk_in2_len[slot],
            (BulkDirection::Out, false) => self.bulk_out_len[slot],
            (BulkDirection::Out, true) => self.bulk_out2_len[slot],
        }
    }

    /// Record the requested length of the TD in `pipe`'s ring data `slot`.
    fn set_bulk_len(&mut self, pipe: BulkPipe, slot: usize, len: u32) {
        match (pipe.direction, pipe.secondary) {
            (BulkDirection::In, false) => self.bulk_in_len[slot] = len,
            (BulkDirection::In, true) => self.bulk_in2_len[slot] = len,
            (BulkDirection::Out, false) => self.bulk_out_len[slot] = len,
            (BulkDirection::Out, true) => self.bulk_out2_len[slot] = len,
        }
    }
}

/// One addressed slot's default control pipe: its EP0 ring and the region
/// its contexts and data stages live in, the region the slot was enumerated
/// on. A composite device's interfaces share their slot's pipe.
struct ControlPipe {
    /// The endpoint's transfer ring. Nothing is in flight on it between
    /// transfers unless a TD could not be taken back, after which it runs
    /// nothing more.
    ring: ProducerRing,
    /// Region offset of [`Self::ring`].
    ring_off: usize,
    /// Region offset of the slot's output device context.
    output_ctx: usize,
    /// Region offset of the input context the slot's commands stage through.
    input_ctx: usize,
    /// Region offset of the buffer its data stages move through.
    ctrl_data: usize,
    /// Its transfers: waiting, running, and the one a recovery settles.
    queue: route::PipeQueue,
    /// Whether it serves, is being taken back, or is dead.
    state: route::PipeState,
    /// Its slot's input context, which one command at a time stages into.
    input: ops::TicketLock,
}

/// The TRBs of one control TD, which its completions name.
#[derive(Copy, Clone)]
struct ControlTd {
    setup: u64,
    data: Option<u64>,
    status: u64,
    data_len: u32,
}

/// A control transfer that did not complete.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ControlFault {
    /// What the caller is told: [`DriverError::EndpointStalled`] for a
    /// refusal the endpoint was recovered from.
    pub(crate) error: DriverError,
    /// Whether the endpoint serves the next transfer: the failure left it
    /// untouched, or it was taken back from the TD.
    pub(crate) endpoint_serves: bool,
    /// The raw completion code of the event that ended the TD, `0` when none
    /// did and its deadline ended it.
    pub(crate) completion: u8,
}

impl ControlFault {
    /// A failure that leaves the endpoint serving: nothing reached its ring,
    /// or the TD completed.
    const fn serving(error: DriverError) -> Self {
        Self {
            error,
            endpoint_serves: true,
            completion: 0,
        }
    }

    /// No pipe to run on: none is open, or its last TD could not be taken
    /// back.
    const fn dead() -> Self {
        Self {
            error: DriverError::DeviceFault,
            endpoint_serves: false,
            completion: 0,
        }
    }
}

/// Where the end of the TD being abandoned left its endpoint, which decides
/// the command tried first to stop it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Abandoned {
    /// An error completion ended the TD and halted the endpoint.
    Halted,
    /// Nothing ended it: the endpoint may still be running it.
    Running,
}

/// Whether the controller may still reach what it was handed for a slot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SlotHold {
    /// It may not: the slot was confirmed disabled, or never enabled.
    Released,
    /// Its Disable Slot, `command`, is posted and unanswered: the
    /// confirmation releases it.
    Pending { slot: u8, command: route::CommandId },
    /// It may, until the controller is reset.
    Held,
}

/// A chunk withheld for a slot whose Disable Slot is unanswered, returned
/// once that command confirms the slot disabled.
#[derive(Copy, Clone)]
struct AwaitedDisable {
    command: route::CommandId,
    slot: u8,
    base: usize,
}

/// Push one `None` entry onto a growable engine table, fallibly:
/// exhaustion of the bookkeeping heap surfaces as a typed error, never a
/// panic (deterministic OOM).
fn push_free_entry<T>(table: &mut Vec<Option<T>>) -> Result<usize, DriverError> {
    table.try_reserve(1).map_err(|_| DriverError::OutOfMemory)?;
    table.push(None);
    Ok(table.len() - 1)
}

/// A command's completion `event`, when it succeeded.
///
/// # Errors
///
/// [`DriverError::NoBandwidth`] when the controller cannot schedule the
/// periodic endpoints a Configure Endpoint adds, else
/// [`DriverError::DeviceFault`] for any other refusal.
fn command_outcome(event: Trb) -> Result<Trb, DriverError> {
    match event.completion_code() {
        Ok(CompletionCode::Success) => Ok(event),
        Ok(CompletionCode::BandwidthError | CompletionCode::SecondaryBandwidthError) => {
            Err(DriverError::NoBandwidth)
        }
        _ => Err(DriverError::DeviceFault),
    }
}

/// The controller engine serving every enumerated device on one started
/// xHCI controller: its rings, contexts and device tables, and the routing of
/// every event the controller posts.
///
/// It never waits: everything that does is an operation its
/// [`UsbHost`] runs, borrowing it only between waits.
pub struct UsbDevice<H: RegisterBlock, M: DmaBank> {
    xhci: Xhci<H>,
    dma: M,
    layout: Layout,
    command_ring: ProducerRing,
    /// The commands posted, in ring order, and the ring's abort state.
    commands: route::Commands,
    /// Each addressed slot's control pipe, indexed by slot id: every slot's
    /// EP0 stays live at once, so a control transfer names its slot and no
    /// other is disturbed. Opened when a slot is enumerated
    /// ([`Self::open_pipe`]) and closed as it is disabled.
    pipes: Vec<Option<ControlPipe>>,
    /// The control transfers queued or running on the pipes, by ticket.
    controls: Vec<Option<route::ControlRequest>>,
    /// The serial the next control transfer's ticket carries.
    control_serial: u64,
    /// Chunks withheld behind Disable Slots the controller has not answered
    /// yet, returned by their confirmations
    /// ([`Self::settle_awaited_disable`]).
    awaited_disables: Vec<AwaitedDisable>,
    event_cursor: EventRingCursor,
    /// Whether some root port's change bits may be latched, so the service
    /// step must scan `PORTSC`.
    ///
    /// Armed by the two independent latched sources the controller offers — a
    /// drained Port Status Change Event ([`Self::poll_event`]) and the
    /// `USBSTS.PCD` summary ([`Self::acknowledge_interrupt`]) — and starts set
    /// so bring-up, and a re-enumeration after a controller reset, scan the
    /// ports the firmware may already have left connected. Without it the scan
    /// costs one `PORTSC` read per port on *every* interrupt, which on a PCIe
    /// controller is the most expensive thing on the report path.
    root_change_pending: bool,
    /// Bound on the *register-handshake* polls (`Xhci` open/start/reset
    /// readiness waits) only — the brief, bounded MMIO waits the silicon
    /// dictates.
    budget: u32,
    /// The instant the service step is running at, which deadlines posted
    /// inside it count from.
    now_ns: u64,
    /// The readiness the host's executor works from.
    wakes: route::Wakes,
    /// The bus's default address, which one port at a time holds.
    address0: ops::TicketLock,
    /// Set while a controller reset drops every operation: what they held
    /// goes with the reset, so their guards give nothing back.
    resetting: bool,
    /// The concurrently served devices, indexed by the device index the
    /// [`HubEvent`]s carry and the per-device transfer paths take. `None`
    /// entries are free. Grows as devices attach ([`Self::claim_device_entry`])
    /// and is bounded only by the controller's reported slot count times the
    /// servable interfaces per slot — a silicon-derived ceiling, never a
    /// hand-picked constant.
    devices: Vec<Option<DeviceState>>,
    /// Each table entry's demand-allocated DMA chunk, index-aligned with
    /// [`Self::devices`]. `Some` from the entry's claim
    /// ([`Self::claim_device_entry`]) until its release — a downstream
    /// hub's contexts keep their entry's region claimed for the hub's
    /// lifetime ([`HubState::device_region`]) even though no served device
    /// occupies the entry.
    regions: Vec<Option<DeviceRegion>>,
    /// The addressed hubs, indexed by the hub-table index [`HubState::
    /// parent`] and [`DeviceState::parent_hub`] refer to. Every hub is kept
    /// addressed concurrently with the served devices so each tier's
    /// status-change endpoint is watched and its per-port class requests can
    /// be issued. Grows as hub tiers install ([`Self::claim_hub_entry`]);
    /// each entry's status-change watch lives in its own demand-allocated
    /// chunk, released on detach.
    hubs: Vec<Option<HubState>>,
    /// The serial the next attachment takes, telling a device or hub from a
    /// later one at the same table index.
    attach_serial: u64,
    /// Ports whose attachment a recovery escalation freed, each owed a reset
    /// and a fresh enumeration in the next retry round, so the caller sees the
    /// teardown before the device that answers there: a device enumerated
    /// afresh is never taken for the one its node was built from.
    /// Deduplicated, so it holds at most one entry per port.
    reenumerations: Vec<Upstream>,
    /// Ports a walk found a device on but could not serve, since the last
    /// walk began, so the driver can surface "present but unserved" rather
    /// than an empty port.
    skipped_ports: u32,
    /// The first failed attach since the driver last took one
    /// ([`Self::take_attach_fault`]).
    attach_fault: Option<AttachFault>,
    /// The controller's microframe count past `MFINDEX`'s wrap, from the
    /// first isochronous schedule on; restarted with the controller.
    bus_clock: Option<BusClock>,
    /// What each abandoned transfer's stopped endpoint said of it, by device
    /// index, until the driver's log takes it.
    abandon_reports: VecDeque<(usize, AbandonedTransfer)>,
}

impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Grow the controller's shared chunk out of `dma`, lay the shared
    /// structures out inside it, program them, and start the controller.
    ///
    /// The controller is first declared quiesced to `dma`: an [`Xhci`]
    /// exists only once [`Xhci::open`] has halted and reset it.
    ///
    /// The chunk is sized **exactly** to the geometry the silicon reports
    /// (`MaxSlots`, context size, scratchpad count and page size); no
    /// per-device memory is reserved here — each device's region is grown
    /// on attach and released on detach, so the served-device count is
    /// bounded by the controller's slots and genuine memory exhaustion,
    /// never a compile-time budget.
    ///
    /// `budget` bounds the register-handshake polls (the brief MMIO
    /// readiness waits the silicon dictates), failing closed on a stuck
    /// controller.
    ///
    /// The controller's completion interrupter is enabled here (and on
    /// every re-program after a reset), because the event loop parks on that
    /// interrupt: the caller must have routed and bound the controller's
    /// interrupt line **before** calling this.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if the granted chunk's device-visible
    ///   base is zero, not 64-byte aligned, or (when the controller needs
    ///   scratchpad) leaves the scratchpad pages off a controller-page
    ///   boundary.
    /// * [`DriverError::OutOfMemory`] if the bank cannot supply the shared
    ///   chunk (deterministic OOM — the [`DmaHost`] exhaustion convention).
    /// * [`DriverError::DeviceFault`] if the controller does not
    ///   start within `budget` polls.
    /// * The bank's refusal to place chunks where a controller without AC64
    ///   reaches them.
    ///
    /// [`DmaHost`]: tairix_abi::driver::dma::DmaHost
    pub(crate) fn start(xhci: Xhci<H>, dma: M, budget: u32) -> Result<Self, DriverError> {
        let mut xhci = xhci;
        let mut dma = dma;
        dma.device_quiesced();
        dma.narrow_reach(xhci.dma_reach())?;
        let layout = Layout::new(
            xhci.max_slots(),
            xhci.csz(),
            xhci.max_scratchpad_buffers(),
            xhci.page_size(),
            xhci.event_ring_segments(),
        )?;
        let base = dma.grow(layout.total)?;
        let layout = layout.rebased(base);
        let device = dma.device_addr_of(base)?;
        if device == 0 || device % 64 != 0 {
            return Err(DriverError::OutOfRange);
        }
        // Each scratchpad buffer must land on a controller-page boundary
        // in the device address space (xHCI §4.20 / §6.6); fail closed on
        // a chunk the bank could not place page-aligned.
        if layout.scratchpad_count > 0
            && dma.device_addr_of(layout.scratchpad_pages)? % layout.page_size as u64 != 0
        {
            return Err(DriverError::OutOfRange);
        }

        let (command_ring, event_cursor) =
            match Self::program_and_start(&mut xhci, &mut dma, &layout, budget) {
                Ok(started) => started,
                Err(err) => {
                    // The controller may be running over the chunk: it goes
                    // only once the controller is reset, and never if it will
                    // not reset.
                    if xhci.reset_to_ready(budget).is_err() {
                        dma.withhold_all();
                    }
                    return Err(err);
                }
            };

        Ok(Self {
            xhci,
            dma,
            layout,
            command_ring,
            commands: route::Commands::default(),
            pipes: Vec::new(),
            controls: Vec::new(),
            control_serial: 0,
            awaited_disables: Vec::new(),
            event_cursor,
            root_change_pending: true,
            budget,
            now_ns: 0,
            wakes: route::Wakes::default(),
            address0: ops::TicketLock::default(),
            resetting: false,
            devices: Vec::new(),
            regions: Vec::new(),
            hubs: Vec::new(),
            attach_serial: 0,
            reenumerations: Vec::new(),
            skipped_ports: 0,
            attach_fault: None,
            bus_clock: None,
            abandon_reports: VecDeque::new(),
        })
    }

    /// Zero the DMA region, build the command producer ring and the
    /// event-ring cursor, reserve the controller's scratchpad buffers, and
    /// start the controller.
    ///
    /// Factored out of [`Self::start`] so the controller re-bring-up after a
    /// fault ([`UsbHost::reset_and_reenumerate`]) re-programs the
    /// *same* held DMA region and register window identically, rather than
    /// duplicating the sequence. Every EP0, hub status-change and endpoint
    /// ring is built in its device's own region when that device is
    /// addressed or configured, not here.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    fn program_and_start(
        xhci: &mut Xhci<H>,
        dma: &mut M,
        layout: &Layout,
        budget: u32,
    ) -> Result<(ProducerRing, EventRingCursor), DriverError> {
        let zeros = [0u8; 64];
        let mut offset = 0;
        while offset < layout.total {
            let chunk = (layout.total - offset).min(zeros.len());
            dma.write(layout.base + offset, &zeros[..chunk])?;
            offset += chunk;
        }

        // One segment table entry per page-sized segment: its base and size
        // in TRBs.
        let event_device = dma.device_addr_of(layout.event_segment)?;
        let segment_trbs =
            u32::try_from(EVENT_RING_SEGMENT_TRBS).map_err(|_| DriverError::LengthOutOfRange)?;
        for segment in 0..layout.event_segments {
            let base = dma.device_addr_of(layout.event_segment + segment * DMA_CHUNK_ALIGN)?;
            let mut entry = [0u8; ERST_ENTRY_LEN];
            entry[..8].copy_from_slice(&base.to_le_bytes());
            entry[8..12].copy_from_slice(&segment_trbs.to_le_bytes());
            dma.write(layout.erst + segment * ERST_ENTRY_LEN, &entry)?;
        }

        let (command_ring, link) =
            ProducerRing::new(RING_TRBS, dma.device_addr_of(layout.command_ring)?)?;
        dma.write(
            layout.command_ring + command_ring.link_slot() * trb::TRB_LEN,
            &link.to_bytes(),
        )?;
        let event_cursor = EventRingCursor::new(layout.event_trbs())?;

        // Reserve the controller's scratchpad buffers (xHCI §4.20): fill
        // the scratchpad pointer array with the device-visible base of
        // each page-aligned buffer, then point `DCBAA[0]` at that array.
        // The VL805 reports 31 buffers and cannot execute a single command
        // without them — the very first Enable Slot produces no completion
        // event (the Pi 4 `stage=2 completion=0` metal symptom). A
        // controller reporting `0` skips this entirely.
        if layout.scratchpad_count > 0 {
            for index in 0..layout.scratchpad_count {
                let page =
                    dma.device_addr_of(layout.scratchpad_pages + index * layout.page_size)?;
                dma.write(layout.scratchpad_array + index * 8, &page.to_le_bytes())?;
            }
            let array = dma.device_addr_of(layout.scratchpad_array)?;
            dma.write(layout.dcbaa, &array.to_le_bytes())?;
        }

        xhci.start(
            &DmaProgram {
                dcbaap: dma.device_addr_of(layout.dcbaa)?,
                command_ring: dma.device_addr_of(layout.command_ring)?,
                erst: dma.device_addr_of(layout.erst)?,
                erst_entries: u32::try_from(layout.event_segments)
                    .map_err(|_| DriverError::LengthOutOfRange)?,
                event_segment: event_device,
            },
            budget,
        )?;

        // The event loop parks on the controller's completion interrupt, so
        // interrupt generation is part of starting the controller — enabled
        // here so a cold boot and a post-reset re-program share the one
        // definition.
        xhci.enable_interrupter()?;

        Ok((command_ring, event_cursor))
    }

    /// The device table entry at `index`, when live.
    fn device(&self, index: usize) -> Option<&DeviceState> {
        self.devices.get(index).and_then(Option::as_ref)
    }

    /// The mutable device table entry at `index`, when live.
    fn device_mut(&mut self, index: usize) -> Option<&mut DeviceState> {
        self.devices.get_mut(index).and_then(Option::as_mut)
    }

    /// The hub table entry at `hub_index`, when live.
    fn hub(&self, hub_index: usize) -> Option<&HubState> {
        self.hubs.get(hub_index).and_then(Option::as_ref)
    }

    /// The mutable hub table entry at `hub_index`, when live.
    fn hub_mut(&mut self, hub_index: usize) -> Option<&mut HubState> {
        self.hubs.get_mut(hub_index).and_then(Option::as_mut)
    }

    /// Claim a hub-table entry: reuse a free one or grow the table. The
    /// table is bounded by the controller's own slot count — every tracked
    /// hub holds an xHCI slot — so growth is silicon-derived, never a
    /// hand-picked ceiling.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NoSpace`] if every entry is live and the table
    ///   already covers the controller's slot count.
    /// * [`DriverError::LengthOutOfRange`] on bookkeeping-heap exhaustion
    ///   (deterministic OOM).
    fn claim_hub_entry(&mut self) -> Result<usize, DriverError> {
        if let Some(index) = self.hubs.iter().position(Option::is_none) {
            return Ok(index);
        }
        if self.hubs.len() >= usize::from(self.xhci.max_slots()) {
            return Err(DriverError::NoSpace);
        }
        push_free_entry(&mut self.hubs)
    }

    /// Index of the live device the hub at `hub_index` serves on its
    /// downstream `port`, when one is.
    fn device_index_for_hub_and_port(&self, hub_index: usize, port: u8) -> Option<usize> {
        self.devices.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|device| device.hub_port == port && device.parent_hub == hub_index)
        })
    }

    /// Index of the live child *hub* the hub at `hub_index` carries on its
    /// downstream `port`, when one is.
    fn hub_index_for_hub_and_port(&self, hub_index: usize, port: u8) -> Option<usize> {
        self.hubs.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|hub| hub.parent == Some(hub_index) && hub.parent_port == port)
        })
    }

    /// Claim a device-table entry and allocate its DMA region: reuse a
    /// free entry (one with no live device *and* no claimed region — a
    /// region kept by a downstream hub's contexts,
    /// [`HubState::device_region`], is not free even though no served
    /// device occupies its entry) or grow the table, then grow a fresh
    /// region chunk for it. The table is bounded by the controller's
    /// reported slot count times the servable interfaces per slot (a
    /// composite device's sibling interfaces share one slot) — a
    /// silicon-derived ceiling, never a hand-picked one.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NoSpace`] if every entry is claimed and the table
    ///   already covers the silicon-derived ceiling.
    /// * [`DriverError::LengthOutOfRange`] on DMA or bookkeeping-heap
    ///   exhaustion (deterministic OOM — the attach fails closed and every
    ///   already-served device keeps its service).
    fn claim_device_entry(&mut self) -> Result<usize, DriverError> {
        let free = (0..self.devices.len())
            .find(|&index| self.devices[index].is_none() && self.regions[index].is_none());
        let index = if let Some(index) = free {
            index
        } else {
            let ceiling = usize::from(self.xhci.max_slots()) * MAX_INTERFACES;
            if self.devices.len() >= ceiling {
                return Err(DriverError::NoSpace);
            }
            let index = push_free_entry(&mut self.devices)?;
            match push_free_entry(&mut self.regions) {
                Ok(region_index) => debug_assert_eq!(region_index, index),
                Err(err) => {
                    // Keep the tables index-aligned on the failed path.
                    self.devices.pop();
                    return Err(err);
                }
            }
            index
        };
        let base = self
            .dma
            .grow(DeviceRegion::layout_len(self.layout.ctx_size))?;
        self.regions[index] = Some(DeviceRegion::at(base, self.layout.ctx_size));
        Ok(index)
    }

    /// The claimed DMA region backing device-table entry `index`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] if the entry has no claimed region (a
    /// stale or forged index — fail closed).
    fn device_region(&self, index: usize) -> Result<DeviceRegion, DriverError> {
        self.regions
            .get(index)
            .copied()
            .flatten()
            .ok_or(DriverError::OutOfRange)
    }

    /// Drop every tracked device and hub and release their
    /// demand-allocated chunks, leaving only the shared chunk live — the
    /// teardown of a full controller reset that rebuilds the tree from
    /// scratch, and sound only after one: the reset is what lets the chunks
    /// go.
    fn reset_device_tracking(&mut self) {
        self.pipes.clear();
        self.controls.clear();
        self.commands = route::Commands::default();
        self.address0 = ops::TicketLock::default();
        self.clear_wakes();
        self.bus_clock = None;
        for index in 0..self.devices.len() {
            if let Some(device) = self.devices[index].take() {
                for chunk in device.streaming.chunks() {
                    let _ = self.dma.release(chunk);
                }
            }
            self.retire_device_region(index, SlotHold::Released);
        }
        self.devices.clear();
        self.regions.clear();
        let hubs = core::mem::take(&mut self.hubs);
        for hub in hubs.into_iter().flatten() {
            let _ = self.dma.release(hub.region.base);
        }
        self.reenumerations.clear();
    }

    /// Whether a served device is live at `index`.
    #[must_use]
    pub fn device_live(&self, index: usize) -> bool {
        self.device(index).is_some()
    }

    /// Number of device-table entries (live or free) — the index bound a
    /// consumer reconciles its per-index state against
    /// ([`Self::device_live`] indices lie below it). Grows as devices
    /// attach and shrinks only on a full re-enumeration.
    #[must_use]
    pub fn device_table_len(&self) -> usize {
        self.devices.len()
    }

    /// Whether any served device is live.
    #[must_use]
    pub fn any_device_live(&self) -> bool {
        self.devices.iter().any(Option::is_some)
    }

    /// Acknowledge the controller interrupter's pending interrupt
    /// (`IMAN.IP`), keeping it armed (xHCI §4.17.5).
    ///
    /// Called at the **start** of servicing a delivered interrupt — before
    /// the reports are drained through [`Self::next_report`] — so a
    /// completion the controller posts during the drain re-asserts `IMAN.IP`
    /// and is not lost. Delegates to [`Xhci::acknowledge_interrupt`], whose
    /// single `USBSTS` read also carries the port-change summary — folded into
    /// the root-port scan's arming here — and the fault latch, returned
    /// so the caller recovers without a second read of the same register.
    ///
    /// This clears only `IMAN.IP`, never `ERDP`. Event Handler Busy
    /// (`ERDP.EHB`) is released solely by the per-event dequeue advance the
    /// drain performs (`ack_event`, one write per event actually consumed),
    /// so `ERDP` is only ever written with EHB once the controller's event is
    /// genuinely caught up. A standalone `ERDP` write on an empty or
    /// not-yet-consumed ring would tell the controller the ring is drained to
    /// a point behind its own enqueue and re-assert the interrupt
    /// immediately — a self-sustaining storm (the metal symptom: the loop
    /// wakes continuously the moment a key is pressed). So the drain, not a
    /// separate write, owns EHB.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if the register window rejects the access.
    pub fn acknowledge_interrupt(&mut self) -> Result<ControllerStatus, DriverError> {
        let status = self.xhci.acknowledge_interrupt()?;
        if status.port_change {
            self.root_change_pending = true;
        }
        Ok(status)
    }

    /// Device-visible address of the bank's virtual offset `offset`.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] if `offset` lies in no live chunk (a
    /// stale offset kept past its chunk's release — fail closed).
    fn device_addr_of(&self, offset: usize) -> Result<u64, DriverError> {
        self.dma.device_addr_of(offset)
    }

    /// Consume the next controller event, advancing `ERDP` when one
    /// was taken.
    fn poll_event(&mut self) -> Result<Option<Trb>, DriverError> {
        // First snapshot: decide *whether* the controller has produced the
        // event at the dequeue point, by its cycle bit alone.
        let trb = self.read_event_slot()?;
        if !self.event_cursor.owned(trb) {
            return Ok(None);
        }
        // An event is owned. The controller writes the entry body before it
        // sets the cycle bit; on the device-shared Normal-Non-Cacheable DMA
        // region those two writes are not ordered for this PE without a
        // barrier, so the first snapshot's body bytes may predate the cycle
        // bit (a torn read pairing a fresh cycle with a stale TRB pointer —
        // the metal `REJECT_ADDRESS_MISMATCH` this fixes). Order the body read
        // after the cycle observation, then re-read and consume (see `tairix_dma_barrier`).
        tairix_dma_barrier::dma_rmb();
        let trb = self.read_event_slot()?;
        // Re-confirm ownership on the post-barrier snapshot, then verify the
        // entry has actually landed before consuming it. The read barrier
        // orders *this PE's* reads (body after cycle), but it cannot order the
        // *controller's* writes into RAM: on the BCM2711 PCIe path the VL805's
        // 16-byte TRB write is not guaranteed to reach RAM atomically, so the
        // announcing cycle bit can become visible while the body is still the
        // zeroed initial state. A real event TRB never has type 0, so a
        // cycle-owned entry whose type is still 0 has not fully landed: leave it
        // un-consumed (do not advance the cursor, do not write `ERDP`) and
        // re-read it on the next wake once the body is visible. Consuming such a
        // phantom would advance the dequeue past the controller's enqueue and
        // permanently desynchronise the consumer cycle, wedging the interrupter
        // with Event Handler Busy stuck set so no further completion interrupts
        // — the metal "first key then silent" fault.
        if !self.event_cursor.owned(trb) {
            return Ok(None);
        }
        if trb.trb_type_raw() == 0 {
            return Ok(None);
        }
        let Some(event) = self.event_cursor.pop(trb) else {
            return Ok(None);
        };
        // A Port Status Change Event is the controller's notification that some
        // root port's change bits latched. Recording it here — at the one point
        // every ring consumer funnels through — is what lets the root-port scan
        // be event-driven rather than run on every interrupt: no consumer can
        // drain such an event without arming the scan.
        if event.trb_type() == Ok(TrbType::PortStatusChange) {
            self.root_change_pending = true;
        }
        let index = self.event_cursor.dequeue_index();
        let dequeue = index
            .checked_mul(trb::TRB_LEN)
            .and_then(|at| self.layout.event_segment.checked_add(at))
            .ok_or(DriverError::OutOfRange)?;
        let erdp = self.device_addr_of(dequeue)?;
        let segment =
            u32::try_from(index / EVENT_RING_SEGMENT_TRBS).map_err(|_| DriverError::OutOfRange)?;
        self.xhci.ack_event(erdp, segment)?;
        Ok(Some(event))
    }

    /// Read the one event-ring entry at the cursor's dequeue point out of DMA.
    ///
    /// Only that entry decides whether an event is pending, so reading it alone
    /// keeps a poll at 16 bytes of non-cacheable traffic instead of the whole
    /// segment.
    fn read_event_slot(&mut self) -> Result<Trb, DriverError> {
        let mut image = [0u8; trb::TRB_LEN];
        self.dma.read(
            self.layout.event_segment + self.event_cursor.dequeue_offset(),
            &mut image,
        )?;
        Ok(Trb::from_bytes(image))
    }

    /// Index of the served device whose interrupt-IN report completion
    /// `event` is, routed by its stable slot and endpoint.
    fn report_async_index(&self, event: Trb) -> Option<usize> {
        self.devices.iter().position(|entry| {
            entry.as_ref().is_some_and(|device| {
                device.int_dci != DCI_CONTROL
                    && event.slot_id() == device.slot
                    && event.endpoint_id() == device.int_dci
            })
        })
    }

    /// Index of the addressed hub whose status-change endpoint completion
    /// `event` is, routed by its stable slot and endpoint.
    fn hub_async_index(&self, event: Trb) -> Option<usize> {
        self.hubs.iter().position(|entry| {
            entry.as_ref().is_some_and(|hub| {
                hub.int_dci != 0
                    && event.slot_id() == hub.slot
                    && event.endpoint_id() == hub.int_dci
            })
        })
    }

    /// Index of the served device on whose configured bulk endpoint `event`
    /// completed, routed by its stable slot and endpoint.
    fn bulk_async_index(&self, event: Trb) -> Option<usize> {
        self.devices.iter().position(|entry| {
            entry.as_ref().is_some_and(|device| {
                event.slot_id() == device.slot
                    && device.bulk_pipe_of_dci(event.endpoint_id()).is_some()
            })
        })
    }

    /// Settle command `command`, completing as `event`, when it is a Disable
    /// Slot chunks were withheld behind: a confirmation clears the slot's
    /// DCBAA entry and returns every such chunk, a refusal leaves them
    /// withheld until a controller reset. Whether it was one.
    fn settle_awaited_disable(&mut self, command: route::CommandId, event: Trb) -> bool {
        let Some(slot) = self
            .awaited_disables
            .iter()
            .find(|awaited| awaited.command == command)
            .map(|awaited| awaited.slot)
        else {
            return false;
        };
        let confirmed = event.completion_code() == Ok(CompletionCode::Success);
        if confirmed {
            // The entry goes before the memory it names. A failed write
            // leaves a pointer the controller no longer reads: the slot is
            // disabled, and re-enabling it rewrites the entry first.
            let _ = self.dma.write(
                self.layout.dcbaa + usize::from(slot) * 8,
                &0u64.to_le_bytes(),
            );
        }
        let dma = &mut self.dma;
        self.awaited_disables.retain(|awaited| {
            if awaited.command != command {
                return true;
            }
            if confirmed {
                // Recorded only when withheld, so it is still withheld.
                let _ = dma.release_withheld_chunk(awaited.base);
            }
            false
        });
        true
    }

    /// The control pipe of `slot`.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when `slot` has none open.
    fn pipe(&self, slot: u8) -> Result<&ControlPipe, DriverError> {
        self.pipes
            .get(usize::from(slot))
            .and_then(Option::as_ref)
            .ok_or(DriverError::DeviceFault)
    }

    /// As [`Self::pipe`], mutably.
    fn pipe_mut(&mut self, slot: u8) -> Result<&mut ControlPipe, DriverError> {
        self.pipes
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
            .ok_or(DriverError::DeviceFault)
    }

    /// Open `slot`'s control pipe on device-table entry `index`'s region,
    /// with an empty EP0 ring and its output context zeroed, as Address
    /// Device requires (§4.5.2): a reused region still holds the previous
    /// device's contexts.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for an entry with no region,
    /// [`DriverError::OutOfMemory`] when the pipe table cannot grow, or a
    /// DMA fault.
    fn open_pipe(&mut self, index: usize, slot: u8) -> Result<(), DriverError> {
        let region = self.device_region(index)?;
        let at = usize::from(slot);
        if self.pipes.len() <= at {
            self.pipes
                .try_reserve(at + 1 - self.pipes.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            self.pipes.resize_with(at + 1, || None);
        }
        let ring = self.build_ring(region.ep0_ring, RING_TRBS)?;
        let ctx_zeros = [0u8; CTX_DWORDS * 4];
        for offset in (0..OUTPUT_CONTEXTS * self.layout.ctx_size).step_by(ctx_zeros.len()) {
            self.dma.write(region.output_ctx + offset, &ctx_zeros)?;
        }
        self.pipes[at] = Some(ControlPipe {
            ring,
            ring_off: region.ep0_ring,
            output_ctx: region.output_ctx,
            input_ctx: region.input_ctx,
            ctrl_data: region.ctrl_data,
            queue: route::PipeQueue::default(),
            state: route::PipeState::Serving,
            input: ops::TicketLock::default(),
        });
        Ok(())
    }

    /// Close `slot`'s control pipe, answering every transfer on it: nothing
    /// more runs on it.
    fn close_pipe(&mut self, slot: u8) {
        self.fail_pipe_queue(slot);
        if let Some(pipe) = self.pipes.get_mut(usize::from(slot)) {
            *pipe = None;
        }
    }

    /// Clear `slot`'s whole input context (§6.2.5.1), so a command writes
    /// its contexts over zeroes rather than over an earlier command's.
    fn zero_input_ctx(&mut self, slot: u8) -> Result<(), DriverError> {
        let input_ctx = self.pipe(slot)?.input_ctx;
        // Walk the contiguous span, not one write per context: with 64-byte
        // contexts a context-wide write would leave the padding between them
        // carrying the previous command's bytes.
        let zeros = [0u8; CTX_DWORDS * 4];
        for offset in (0..INPUT_CONTEXTS * self.layout.ctx_size).step_by(zeros.len()) {
            self.dma.write(input_ctx + offset, &zeros)?;
        }
        Ok(())
    }

    /// Write context `index` of `slot`'s input context (§6.2.5: index 0 is
    /// the input control context, 1 the slot context, and `1 + dci` the
    /// endpoint contexts).
    fn write_input_ctx(
        &mut self,
        slot: u8,
        index: usize,
        dwords: &[u32; CTX_DWORDS],
    ) -> Result<(), DriverError> {
        let entry = self.pipe(slot)?.input_ctx + index * self.layout.ctx_size;
        let mut bytes = [0u8; CTX_DWORDS * 4];
        for (dword_index, dword) in dwords.iter().enumerate() {
            bytes[dword_index * 4..dword_index * 4 + 4].copy_from_slice(&dword.to_le_bytes());
        }
        self.dma.write(entry, &bytes)
    }

    /// Device-visible address of `slot`'s input context, which its commands
    /// name.
    fn input_ctx_addr(&self, slot: u8) -> Result<u64, DriverError> {
        self.device_addr_of(self.pipe(slot)?.input_ctx)
    }

    /// Read one device-context block (the [`CTX_DWORDS`] dwords at
    /// `offset`) back out of DMA, for copying a controller-maintained
    /// output context into the input context before re-issuing a command
    /// over it (xHCI §4.6.6: a Configure Endpoint preserves the fields it
    /// does not touch, so the input copy must start from the live output
    /// context).
    fn read_ctx(&mut self, offset: usize) -> Result<[u32; CTX_DWORDS], DriverError> {
        let mut bytes = [0u8; CTX_DWORDS * 4];
        self.dma.read(offset, &mut bytes)?;
        let mut dwords = [0u32; CTX_DWORDS];
        for (index, dword) in dwords.iter_mut().enumerate() {
            *dword = u32::from_le_bytes([
                bytes[index * 4],
                bytes[index * 4 + 1],
                bytes[index * 4 + 2],
                bytes[index * 4 + 3],
            ]);
        }
        Ok(dwords)
    }

    /// Publish one control TD's stages on `slot`'s EP0 ring, its data stage
    /// (of `data_len` bytes, `out` naming the direction) through
    /// `ctrl_data`.
    fn arm_control_td(
        &mut self,
        slot: u8,
        setup: [u8; 8],
        data_len: u32,
        out: bool,
        ctrl_data: usize,
    ) -> Result<ControlTd, DriverError> {
        let transfer_type = if data_len == 0 {
            trb::SETUP_TRT_NO_DATA
        } else if out {
            trb::SETUP_TRT_OUT
        } else {
            trb::SETUP_TRT_IN
        };
        let buffer = self.device_addr_of(ctrl_data)?;
        let setup = self.push_control_trb(
            slot,
            Trb::new(
                TrbType::SetupStage,
                u64::from_le_bytes(setup),
                8,
                trb::CONTROL_IDT | transfer_type,
            ),
        )?;
        let data = if data_len > 0 {
            // An IN data stage interrupts on a short packet so the honest
            // byte count is read from the residual; an OUT stage moves
            // host bytes and carries no direction flag.
            let data_flags = if out {
                0
            } else {
                trb::CONTROL_DIR_IN | trb::CONTROL_ISP
            };
            Some(self.push_control_trb(
                slot,
                Trb::new(TrbType::DataStage, buffer, data_len, data_flags),
            )?)
        } else {
            None
        };
        // The status stage runs opposite to the data direction; with
        // no data stage it is always IN (§4.11.2.2).
        let status_direction = if data_len > 0 && !out {
            0
        } else {
            trb::CONTROL_DIR_IN
        };
        let status = self.push_control_trb(
            slot,
            Trb::new(
                TrbType::StatusStage,
                0,
                0,
                status_direction | trb::CONTROL_IOC,
            ),
        )?;
        Ok(ControlTd {
            setup,
            data,
            status,
            data_len,
        })
    }

    /// Push `trb` onto `slot`'s EP0 ring and publish it, returning the
    /// address its completions name.
    fn push_control_trb(&mut self, slot: u8, trb: Trb) -> Result<u64, DriverError> {
        let pipe = self.pipe_mut(slot)?;
        let outcome = pipe.ring.push(trb)?;
        let (ring_off, link_slot) = (pipe.ring_off, pipe.ring.link_slot());
        publish(&mut self.dma, ring_off, link_slot, &outcome)?;
        Ok(outcome.address)
    }

    /// Prime one interrupt-IN transfer on device `index`'s endpoint: a
    /// Normal TRB pointing at the report buffer paired with the ring slot
    /// it lands in.
    fn arm_report(&mut self, index: usize) -> Result<(), DriverError> {
        let region = self.device(index).ok_or(DriverError::NotFound)?.region;
        let bufs_device = self.device_addr_of(region.report_bufs)?;
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        let report_len = u32::from(device.int_transfer.ok_or(DriverError::DeviceFault)?);
        let ring = device.int_ring.as_mut().ok_or(DriverError::DeviceFault)?;
        let slot = ring.enqueue_slot();
        let buffer = slot
            .checked_mul(INT_TRANSFER_MAX)
            .and_then(|at| u64::try_from(at).ok())
            .and_then(|at| bufs_device.checked_add(at))
            .ok_or(DriverError::OutOfRange)?;
        let normal = Trb::new(
            TrbType::Normal,
            buffer,
            report_len,
            trb::CONTROL_IOC | trb::CONTROL_ISP,
        );
        let outcome = ring.push(normal)?;
        let link_slot = ring.link_slot();
        publish(&mut self.dma, region.int_ring, link_slot, &outcome)
    }

    /// Keep device `index`'s interrupt-IN endpoint armed with up to
    /// [`INT_ARM_DEPTH`] outstanding transfers, arming as many fresh ones as
    /// the current in-flight count is short and ringing the doorbell once if
    /// any were armed.
    ///
    /// This is what keeps the report endpoint continuously live: with a
    /// transfer always armed the controller has somewhere to write each
    /// interval's report, so a report is not dropped merely because software
    /// was slow to run. Combined with the service step's capture into the
    /// report FIFO ([`UsbHost::service`]), device polling is decoupled from the
    /// class driver's consume rate (the metal "missed keypresses under load"
    /// defect). In-flight transfers the controller has already consumed but
    /// whose completion this engine has not yet retired still count against
    /// the depth, so this never over-fills the ring; the count is topped back
    /// up as [`Self::capture_report_event`] retires each completion.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] / [`DriverError::DeviceFault`] if the device
    /// or its interrupt ring is gone, or a controller fault while arming.
    fn ensure_reports_armed(&mut self, index: usize) -> Result<(), DriverError> {
        let (device_slot, int_dci) = {
            let device = self.device(index).ok_or(DriverError::NotFound)?;
            if device.int_transfer.is_none() || device.recovery.owed() || device.int_recovering {
                return Ok(());
            }
            (device.slot, device.int_dci)
        };
        let mut armed_any = false;
        loop {
            let in_flight = self
                .device(index)
                .and_then(|device| device.int_ring.as_ref().map(ProducerRing::in_flight))
                .ok_or(DriverError::DeviceFault)?;
            if in_flight >= INT_ARM_DEPTH {
                break;
            }
            match self.arm_report(index) {
                Ok(()) => armed_any = true,
                // The ring cannot hold another transfer: the depth already
                // armed is the most this ring supports, which is enough.
                Err(DriverError::Busy) => break,
                Err(err) => return Err(err),
            }
        }
        if armed_any {
            self.xhci.ring_doorbell(device_slot, u32::from(int_dci))?;
        }
        Ok(())
    }

    /// Plan which interfaces of the decoded set are served and at which
    /// device-table index: the first `published` one takes the caller's
    /// `index` (whose entry and region the caller already claimed); each
    /// further one — a composite device's sibling function, e.g. the mouse
    /// interface of a wireless keyboard+mouse receiver — claims its own
    /// table entry and region ([`Self::claim_device_entry`]), sharing the
    /// device's slot and EP0. An interface the claim cannot supply memory
    /// for is left unserved rather than displacing a live device (the
    /// failed-enumeration sweep releases anything a later fault strands).
    /// A hub is not planned. An interface not published — a streaming
    /// interface with alternate settings — stays for a published sibling to
    /// claim.
    fn plan_interfaces(
        &mut self,
        index: usize,
        interfaces: &[Option<InterfaceInfo>; MAX_INTERFACES],
        published: impl Fn(&InterfaceInfo) -> bool,
    ) -> [Option<(usize, InterfaceInfo)>; MAX_INTERFACES] {
        let mut plan: [Option<(usize, InterfaceInfo)>; MAX_INTERFACES] = [None; MAX_INTERFACES];
        let mut planned = 0usize;
        for iface in interfaces.iter().flatten() {
            if !published(iface) {
                continue;
            }
            let target = if planned == 0 {
                index
            } else {
                let Ok(claimed) = self.claim_device_entry() else {
                    break;
                };
                claimed
            };
            plan[planned] = Some((target, *iface));
            planned += 1;
        }
        plan
    }

    /// Install one freshly enumerated, served interface into the table at
    /// `index`. A composite sibling entry shares its slot, and so its
    /// control pipe, with the primary entry.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] if `slot` has no open pipe, before
    /// anything is installed.
    #[allow(clippy::too_many_arguments)] // The one construction site's facts.
    #[allow(clippy::similar_names)] // The `*2` names are the second pipes'
                                    // own names beside their primaries — deliberate siblings.
    fn install_device_entry(
        &mut self,
        index: usize,
        slot: u8,
        hub_port: u8,
        base: SlotCtxBase,
        parent_hub: usize,
        region: DeviceRegion,
        descriptor: DeviceDescriptor,
        serial_number: Option<SerialNumber>,
        removable: bool,
        config: Vec<u8>,
        interface: &InterfaceInfo,
        int_dci: u8,
        int_ring: Option<ProducerRing>,
        bulk_rings: Option<BulkRings>,
    ) -> Result<(), DriverError> {
        self.pipe(slot)?;
        let (bulk_in_ring, bulk_out_ring, bulk_in2_ring, bulk_out2_ring) = match bulk_rings {
            Some(rings) => (
                Some(rings.in_ring),
                Some(rings.out_ring),
                rings.in2_ring,
                rings.out2_ring,
            ),
            None => (None, None, None, None),
        };
        // A DCI is recorded exactly when its ring went live, so the
        // transfer paths and event attribution agree on which endpoints
        // exist.
        let bulk_in_dci = bulk_in_ring.as_ref().map_or(0, |_| interface.bulk_in.dci);
        let bulk_out_dci = bulk_out_ring.as_ref().map_or(0, |_| interface.bulk_out.dci);
        let bulk_in2_dci = bulk_in2_ring.as_ref().map_or(0, |_| interface.bulk_in2.dci);
        let bulk_out2_dci = bulk_out2_ring
            .as_ref()
            .map_or(0, |_| interface.bulk_out2.dci);
        let serial = self.next_attach_serial();
        self.devices[index] = Some(DeviceState {
            slot,
            serial,
            urb_control: None,
            request: None,
            bulk_recovery: [BulkRecovery::Idle; 4],
            speed: base.speed,
            config,
            streaming: Streaming::default(),
            hub_port,
            parent_hub,
            region,
            identity: DeviceIdentity {
                root_port: base.root_port,
                route_string: base.route_string,
                vendor_id: descriptor.vendor_id,
                product_id: descriptor.product_id,
                device_release: descriptor.device_release,
                device_class: descriptor.device_class,
                device_subclass: descriptor.device_subclass,
                device_protocol: descriptor.device_protocol,
                interface_number: interface.interface_number,
                interface_class: interface.class24,
                serial_number,
                removable,
            },
            int_dci,
            int_payload: if int_dci == DCI_CONTROL {
                0
            } else {
                u16::try_from(interface.int_shape.payload(base.speed).1)
                    .map_err(|_| DriverError::BadMagic)?
            },
            int_transfer: None,
            int_request: 0,
            int_ring,
            reports: ReportQueue::default(),
            report_fault: None,
            dropped_reports: 0,
            int_recovery_pending: false,
            int_recovering: false,
            recovery: Ladder::default(),
            bulk_in_ring,
            bulk_out_ring,
            bulk_in2_ring,
            bulk_out2_ring,
            bulk_in_dci,
            bulk_out_dci,
            bulk_in2_dci,
            bulk_out2_dci,
            bulk_in_len: [0; BULK_SLOTS],
            bulk_out_len: [0; BULK_SLOTS],
            bulk_in2_len: [0; BULK_SLOTS],
            bulk_out2_len: [0; BULK_SLOTS],
            pending_bulk: RingBuf::new(),
            aborted_bulk: RingBuf::new(),
            last_bulk_fault_code: 0,
            last_report_fault_code: 0,
            abandoning: None,
        });
        Ok(())
    }

    /// Build an empty transfer ring of `trbs` TRBs at region offset
    /// `ring_off`, zeroing it first: reused memory may hold TRBs at the
    /// producer cycle, which the controller would consume past the new
    /// enqueue pointer.
    fn build_ring(&mut self, ring_off: usize, trbs: usize) -> Result<ProducerRing, DriverError> {
        let zeros = [0u8; trb::TRB_LEN];
        for slot_index in 0..trbs {
            self.dma
                .write(ring_off + slot_index * trb::TRB_LEN, &zeros)?;
        }
        let (ring, link) = ProducerRing::new(trbs, self.device_addr_of(ring_off)?)?;
        self.dma
            .write(ring_off + ring.link_slot() * trb::TRB_LEN, &link.to_bytes())?;
        Ok(ring)
    }

    /// The attachment served on root-hub `port`: the root-attached hub
    /// whose tier sits there, or the directly-attached device, or `None`
    /// while the port is unserved. Composite sibling entries share the
    /// port; the first is returned (detaching it detaches its siblings).
    fn root_attachment_on(&self, port: u8) -> Option<Attachment> {
        if let Some(hub_index) = self.hubs.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|hub| hub.parent.is_none() && hub.root_port == port)
        }) {
            return Some(Attachment::Hub(hub_index));
        }
        if let Some(index) = self.devices.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|device| device.hub_port == 0 && device.identity.root_port == port)
        }) {
            return Some(Attachment::Device(index));
        }
        None
    }

    /// Number of root-hub ports the controller reports
    /// (`HCSPARAMS1` `MaxPorts`).
    ///
    /// For a one-shot diagnostic that walks every root-hub port's
    /// `PORTSC` ([`Self::root_port_status_raw`]).
    #[must_use]
    pub fn root_port_count(&self) -> u8 {
        self.xhci.max_ports()
    }

    /// Raw `PORTSC` dword of root-hub `port` (1-based), for a one-shot
    /// diagnostic capture of every port's connect/power/enable/speed state.
    ///
    /// # Errors
    ///
    /// * [`DriverError::OutOfRange`] if `port` is zero or above
    ///   [`Self::root_port_count`].
    /// * [`DriverError::DeviceFault`] if the register window rejects the read.
    pub fn root_port_status_raw(&mut self, port: u8) -> Result<u32, DriverError> {
        Ok(self.xhci.port_status(port)?.raw())
    }

    /// Whether the device attached at `hub_port` of hub `parent_hub` — or,
    /// with `hub_port` zero, at root port `root_port` — can be unplugged.
    ///
    /// A root port answers through its own register, a hub's port through the
    /// hub's descriptor; an answer that cannot be read leaves the device what
    /// USB devices are, unpluggable.
    fn attached_removable(&mut self, root_port: u8, hub_port: u8, parent_hub: usize) -> bool {
        if hub_port == 0 {
            self.xhci
                .port_status(root_port)
                .map_or(true, PortStatus::removable)
        } else {
            self.hub(parent_hub)
                .is_none_or(|hub| port_removable(hub.fixed_ports, hub_port))
        }
    }

    /// The slot-context topology of a device at `speed` on the port `parent`
    /// names — a downstream port of a hub, else root-hub `root_port` — and
    /// that hub's slot (`0` on a root port).
    ///
    /// # Errors
    ///
    /// * [`DriverError::DeviceFault`] if `parent` names no live hub.
    /// * [`DriverError::OutOfRange`] if the tier would exceed
    ///   [`MAX_HUB_DEPTH`] or the port cannot be route-encoded.
    fn slot_topology(
        &self,
        parent: Option<(usize, u8)>,
        root_port: u8,
        speed: u8,
    ) -> Result<(SlotCtxBase, u8), DriverError> {
        let (route_string, tt_hub_slot, tt_port, parent_slot) = match parent {
            Some((hub_index, down_port)) => {
                let parent = self.hub(hub_index).ok_or(DriverError::DeviceFault)?;
                // The child extends its parent's Route String by one tier,
                // and a full/low-speed child routes through a transaction
                // translator: the parent's own when the parent is a
                // high-speed hub, else the one the parent itself inherited
                // (the nearest high-speed ancestor, §6.2.2 / §8.9). A
                // high-speed (or faster) child needs none.
                let route_string = route_for_child(parent.route_string, parent.depth, down_port)?;
                let (tt_hub_slot, tt_port) = if speed_needs_tt(speed) {
                    if parent.speed == SPEED_HIGH {
                        (parent.slot, down_port)
                    } else {
                        (parent.tt_hub_slot, parent.tt_port)
                    }
                } else {
                    (0, 0)
                };
                (route_string, tt_hub_slot, tt_port, parent.slot)
            }
            // A root attach: route string 0, no transaction translator.
            None => (0, 0, 0, 0),
        };
        Ok((
            SlotCtxBase {
                speed,
                root_port,
                route_string,
                tt_hub_slot,
                tt_port,
            },
            parent_slot,
        ))
    }

    /// Prime one interrupt-IN transfer on the status-change endpoint of
    /// the hub at `hub_index` (a Normal TRB pointing at that hub's report
    /// buffer).
    fn arm_hub_report(&mut self, hub_index: usize) -> Result<(), DriverError> {
        let region = self.hub(hub_index).ok_or(DriverError::DeviceFault)?.region;
        let buffer = self.device_addr_of(region.report)?;
        let report_len =
            u32::try_from(HUB_REPORT_LEN).map_err(|_| DriverError::LengthOutOfRange)?;
        let normal = Trb::new(
            TrbType::Normal,
            buffer,
            report_len,
            trb::CONTROL_IOC | trb::CONTROL_ISP,
        );
        let ring = self
            .hub_mut(hub_index)
            .and_then(|hub| hub.int_ring.as_mut())
            .ok_or(DriverError::DeviceFault)?;
        let outcome = ring.push(normal)?;
        let link_slot = ring.link_slot();
        publish(&mut self.dma, region.int_ring, link_slot, &outcome)
    }

    /// Give up the chunk at `base` the controller was handed for a slot:
    /// returned once the controller has confirmed the slot disabled, and
    /// withheld otherwise, since it may still reach the contexts and rings in
    /// it — recorded against a Disable Slot still unanswered, so its late
    /// confirmation returns the chunk.
    fn retire_slot_chunk(&mut self, base: usize, hold: SlotHold) {
        // The base was minted by `grow`; a refusal would mean corrupted
        // bookkeeping, and a stale offset maps to no chunk either way.
        match hold {
            SlotHold::Released => {
                let _ = self.dma.release(base);
            }
            SlotHold::Pending { slot, command } => {
                // With no room to record it, only a controller reset returns it.
                if self.dma.withhold(base).is_ok() && self.awaited_disables.try_reserve(1).is_ok() {
                    self.awaited_disables.push(AwaitedDisable {
                        command,
                        slot,
                        base,
                    });
                }
            }
            SlotHold::Held => {
                let _ = self.dma.withhold(base);
            }
        }
    }

    /// [`Self::retire_slot_chunk`] for device-table entry `index`'s region.
    fn retire_device_region(&mut self, index: usize, hold: SlotHold) {
        if let Some(region) = self.regions.get_mut(index).and_then(Option::take) {
            self.retire_slot_chunk(region.base, hold);
        }
    }

    /// Tear down the served device at `index` after it has disconnected:
    /// drop its table entry — **and every sibling entry sharing its slot**,
    /// since a composite device's interfaces vanish together with the
    /// physical device — end every operation acting on them, and post its
    /// slot's Disable Slot, so a re-attach is a brand-new enumeration on a
    /// brand-new chunk. Every other served device is untouched.
    ///
    /// The entries go before Disable Slot is posted, so nothing arms or rings
    /// the slot being disabled: a completion landing meanwhile is dropped as
    /// stale. Their chunks go back to the bank only once the controller
    /// confirms the slot disabled ([`Self::settle_awaited_disable`]); until
    /// then it may still reach them.
    ///
    /// A Disable Slot that cannot be posted withholds the chunks until a
    /// controller reset.
    fn detach_device(&mut self, index: usize) {
        if self.device(index).is_some() {
            self.tear_down_device(index);
            self.note_topology(HubEvent::Detached(index));
        }
    }

    /// [`Self::detach_device`], reporting nothing: the caller reports what
    /// the teardown it is part of did.
    fn tear_down_device(&mut self, index: usize) {
        let Some(slot) = self.device(index).map(|device| device.slot) else {
            return;
        };
        let mut chunks: Vec<usize> = Vec::new();
        for entry_index in 0..self.devices.len() {
            let Some(device) = self.devices[entry_index].take_if(|device| device.slot == slot)
            else {
                continue;
            };
            // No drive comes back for its URB's answer.
            if let Some(id) = device.urb_control {
                self.orphan_control(id);
            }
            self.cancel(route::Owner::Device {
                index: entry_index,
                serial: device.serial,
                task: route::Task::Reports,
            });
            let region = self.regions.get_mut(entry_index).and_then(Option::take);
            for base in region
                .map(|region| region.base)
                .into_iter()
                .chain(device.streaming.chunks())
            {
                if chunks.try_reserve(1).is_ok() {
                    chunks.push(base);
                } else {
                    // With nowhere to await its outcome, it is kept.
                    self.retire_slot_chunk(base, SlotHold::Held);
                }
            }
        }
        let hold = match self.post_disable_slot(slot) {
            Ok(command) => SlotHold::Pending { slot, command },
            Err(_) => SlotHold::Held,
        };
        for base in chunks {
            self.retire_slot_chunk(base, hold);
        }
    }

    /// Tear down the hub at `hub_index` and **everything behind it** after
    /// it has disconnected: every served device on its downstream ports,
    /// every child hub (recursively — unplugging a hub takes each deeper
    /// tier with it), every operation on its ports, and finally the hub's
    /// own slot, watch ring and claimed device region, as
    /// [`Self::detach_device`] tears a device down. Every other hub and
    /// served device is untouched, and the depth is bounded by
    /// [`MAX_HUB_DEPTH`].
    ///
    fn detach_hub(&mut self, hub_index: usize) {
        if self.hub(hub_index).is_some() {
            self.tear_down_hub(hub_index);
            self.note_topology(HubEvent::HubDetached(hub_index));
        }
    }

    /// [`Self::detach_hub`], reporting nothing.
    fn tear_down_hub(&mut self, hub_index: usize) {
        if self.hub(hub_index).is_none() {
            return;
        }
        for index in 0..self.devices.len() {
            let behind = self.devices[index]
                .as_ref()
                .is_some_and(|device| device.hub_port != 0 && device.parent_hub == hub_index);
            if behind {
                self.tear_down_device(index);
            }
        }
        for child in 0..self.hubs.len() {
            let behind = self
                .hub(child)
                .is_some_and(|hub| hub.parent == Some(hub_index));
            if behind {
                self.tear_down_hub(child);
            }
        }
        let Some(hub) = self.hubs.get_mut(hub_index).and_then(Option::take) else {
            return;
        };
        self.cancel(route::Owner::Hub {
            index: hub_index,
            serial: hub.serial,
        });
        // A port of a hub that is gone is no port: its index can be another
        // hub's by the next round.
        self.reenumerations.retain(
            |upstream| !matches!(upstream, Upstream::Hub(parent, _) if *parent == hub_index),
        );
        let hold = match self.post_disable_slot(hub.slot) {
            Ok(command) => SlotHold::Pending {
                slot: hub.slot,
                command,
            },
            Err(_) => SlotHold::Held,
        };
        // The hub's status-change watch chunk, and the device-region chunk its
        // contexts were enumerated on.
        self.retire_slot_chunk(hub.region.base, hold);
        self.retire_device_region(hub.device_region, hold);
    }

    /// Whether this engine is watching at least one hub's status-change
    /// endpoint event-driven (a hub is addressed and its endpoint armed).
    #[must_use]
    pub fn hub_watch_active(&self) -> bool {
        self.hubs
            .iter()
            .any(|entry| entry.as_ref().is_some_and(|hub| hub.int_ring.is_some()))
    }

    /// Hubs whose status-change endpoint is watched — the most distinct reports
    /// one ring drain can have parked, since each hub keeps exactly one status
    /// transfer outstanding.
    ///
    /// This bounds the caller's service loop: draining every parked report in
    /// one pass is what stops a second reporting hub being stranded with no
    /// outstanding transfer to raise the next interrupt, while the bound stops
    /// a *flapping* hub — whose re-armed endpoint completes again during each
    /// service — from holding the loop and starving the other devices' URBs.
    #[must_use]
    pub fn watched_hub_count(&self) -> usize {
        self.hubs
            .iter()
            .filter(|entry| entry.as_ref().is_some_and(|hub| hub.int_ring.is_some()))
            .count()
    }

    /// Whether a failed endpoint recovery — a served device's interrupt-IN
    /// endpoint, or a hub's status-change endpoint — is owed a deferred retry
    /// ([`UsbHost::retry_owed_recoveries`]), or a port a re-enumeration.
    #[must_use]
    pub fn recovery_owed(&self) -> bool {
        self.devices
            .iter()
            .flatten()
            .any(|device| device.recovery.owed())
            || self.hubs.iter().flatten().any(|hub| hub.watch.owed())
            || !self.reenumerations.is_empty()
    }

    /// Free `attachment` — a hub with everything behind it — owing its port,
    /// for an [`Escalation::Reset`], a reset and a fresh enumeration in the
    /// next round.
    ///
    /// # Errors
    ///
    /// [`DriverError`] from the teardown, or when no memory holds the owed
    /// re-enumeration; [`DriverError::DeviceFault`] for an
    /// [`Escalation::GiveUp`], which leaves the port unserved until a
    /// re-plug.
    fn escalate(
        &mut self,
        attachment: Attachment,
        escalation: Escalation,
    ) -> Result<(), DriverError> {
        self.reenumerations
            .try_reserve(1)
            .map_err(|_| DriverError::NoSpace)?;
        let upstream = match attachment {
            Attachment::Hub(hub_index) => {
                let upstream = self.hub(hub_index).ok_or(DriverError::NotFound)?.upstream();
                self.detach_hub(hub_index);
                upstream
            }
            Attachment::Device(index) => {
                let upstream = self.device(index).ok_or(DriverError::NotFound)?.upstream();
                self.detach_device(index);
                upstream
            }
        };
        if escalation == Escalation::GiveUp {
            return Err(DriverError::DeviceFault);
        }
        if !self.reenumerations.contains(&upstream) {
            self.reenumerations.push(upstream);
        }
        Ok(())
    }

    /// What is owed a paced retry: the ports owed a re-enumeration, and the
    /// hubs and interfaces whose endpoint recovery failed, each with its
    /// attachment serial.
    #[allow(clippy::type_complexity)] // Three plain lists, named by the doc.
    fn owed_recoveries(&mut self) -> (Vec<Upstream>, Vec<(usize, u64)>, Vec<(usize, u64)>) {
        self.attach_fault = None;
        let ports = self.reenumerations.clone();
        let hubs = self
            .hubs
            .iter()
            .enumerate()
            .filter_map(|(index, hub)| {
                hub.as_ref()
                    .filter(|hub| hub.watch.owed())
                    .map(|hub| (index, hub.serial))
            })
            .collect();
        let devices = self
            .devices
            .iter()
            .enumerate()
            .filter_map(|(index, device)| {
                device
                    .as_ref()
                    .filter(|device| device.recovery.owed())
                    .map(|device| (index, device.serial))
            })
            .collect();
        (ports, hubs, devices)
    }

    /// Every port no attachment serves: each root port, and each downstream
    /// port of every watched hub.
    fn unserved_ports(&self) -> Vec<Upstream> {
        let mut ports = Vec::new();
        for port in 1..=self.xhci.max_ports() {
            if self.root_attachment_on(port).is_none() && ports.try_reserve(1).is_ok() {
                ports.push(Upstream::Root(port));
            }
        }
        for (hub_index, hub) in self.hubs.iter().enumerate() {
            let Some(hub) = hub else {
                continue;
            };
            for port in 1..=hub.num_ports {
                let served = self
                    .device_index_for_hub_and_port(hub_index, port)
                    .is_some()
                    || self.hub_index_for_hub_and_port(hub_index, port).is_some();
                if !served && ports.try_reserve(1).is_ok() {
                    ports.push(Upstream::Hub(hub_index, port));
                }
            }
        }
        ports
    }

    /// Hand every port of the hub at `hub_index` to its operation: whatever
    /// a lost status report carried is acted on.
    fn reconcile_hub_ports(&mut self, hub_index: usize) {
        let ports = self.hub(hub_index).map_or(0, |hub| hub.num_ports);
        for port in 1..=ports {
            self.port_changed(Upstream::Hub(hub_index, port));
        }
    }

    /// The serial the next attachment takes.
    fn next_attach_serial(&mut self) -> u64 {
        let serial = self.attach_serial;
        self.attach_serial = self.attach_serial.wrapping_add(1);
        serial
    }

    /// Reset the controller and program it afresh, dropping every device and
    /// hub: the reset releases everything the controller held.
    ///
    /// # Errors
    ///
    /// The reset or the re-programming failed.
    fn reset_controller(&mut self) -> Result<(), DriverError> {
        self.xhci
            .reset_to_ready(self.budget)
            .map_err(|err| err.error)?;
        self.dma.release_withheld();
        self.awaited_disables.clear();
        let layout = self.layout;
        let (command_ring, event_cursor) =
            Self::program_and_start(&mut self.xhci, &mut self.dma, &layout, self.budget)?;
        self.command_ring = command_ring;
        self.event_cursor = event_cursor;
        // The reset cleared every `PORTSC` latch and the fresh ring has posted
        // no Port Status Change Event yet, so nothing would arm the scan for
        // ports the controller comes back with already connected.
        self.root_change_pending = true;
        self.reset_device_tracking();
        Ok(())
    }

    /// Ports — root-hub or downstream — whose connected device failed
    /// enumeration and was skipped fail-soft by the most recent walk
    /// ([`UsbHost::bring_up`], [`UsbHost::reset_and_reenumerate`], or
    /// [`UsbHost::retry_skipped_ports`]), so the driver can log "a device was
    /// present but never served" rather than the port silently looking
    /// empty.
    #[must_use]
    pub const fn skipped_port_count(&self) -> u32 {
        self.skipped_ports
    }

    /// The EP State the endpoint of the transfer device `index` last
    /// abandoned at its deadline had then, while that endpoint is stopping.
    #[must_use]
    pub fn abandoned_ep_state(&self, index: usize) -> Option<u8> {
        self.device(index)?
            .abandoning
            .as_ref()
            .map(|pending| pending.ep_state)
    }

    /// The next report of where a stopped endpoint stood against the
    /// transfer abandoned on it, with the device index, taken.
    pub fn take_abandon_report(&mut self) -> Option<(usize, AbandonedTransfer)> {
        self.abandon_reports.pop_front()
    }

    /// Endpoint `dci` of `slot`'s output context, as the controller last
    /// wrote it.
    fn endpoint_ctx(&mut self, slot: u8, dci: u8) -> Option<[u32; CTX_DWORDS]> {
        let at = self.pipe(slot).ok()?.output_ctx + usize::from(dci) * self.layout.ctx_size;
        self.read_ctx(at).ok()
    }

    /// Record that device `index` abandons the transfer `span` on `slot`'s
    /// endpoint `dci`, with the EP State it has now. Where the controller
    /// stood against it is read once the endpoint stops
    /// ([`Self::settle_abandoned`]): until then the context's dequeue pointer
    /// is not the controller's to report.
    pub(crate) fn note_abandoned(&mut self, index: usize, slot: u8, dci: u8, span: TdSpan) {
        let Some(ctx) = self.endpoint_ctx(slot, dci) else {
            return;
        };
        if let Some(device) = self.device_mut(index) {
            device.abandoning = Some(PendingAbandon {
                slot,
                dci,
                ep_state: (ctx[0] & EP_CTX_STATE_MASK) as u8,
                span,
            });
        }
    }

    /// `slot`'s endpoint `dci` has `stopped`, or could not be: report where
    /// the controller stood against the transfer abandoned on it, if one
    /// was. One that could not be stopped reports nothing, since its context
    /// still says nothing the controller vouches for.
    pub(crate) fn settle_abandoned(&mut self, slot: u8, dci: u8, stopped: bool) {
        let Some(index) = self.devices.iter().position(|device| {
            device.as_ref().is_some_and(|device| {
                device
                    .abandoning
                    .as_ref()
                    .is_some_and(|pending| pending.slot == slot && pending.dci == dci)
            })
        }) else {
            return;
        };
        let Some(pending) = self
            .device_mut(index)
            .and_then(|device| device.abandoning.take())
        else {
            return;
        };
        if !stopped {
            return;
        }
        let Some(ctx) = self.endpoint_ctx(slot, dci) else {
            return;
        };
        let dequeue = (u64::from(ctx[3]) << 32 | u64::from(ctx[2])) & !EP_CTX_DEQUEUE_FLAGS;
        // A report the queue cannot grow for is a diagnostic lost, never a
        // recovery held up.
        if self.abandon_reports.try_reserve(1).is_ok() {
            self.abandon_reports.push_back((
                index,
                AbandonedTransfer {
                    dci,
                    ep_state: pending.ep_state,
                    progress: pending.span.progress(dequeue),
                },
            ));
        }
    }

    /// The first failed attach since the last call — the port, stage, and
    /// controller/hub state its own operation saw — taken, so the next
    /// failure is reported afresh. `None` when every attach since succeeded
    /// (or none ran).
    pub fn take_attach_fault(&mut self) -> Option<AttachFault> {
        self.attach_fault.take()
    }

    /// Raw completion code of the most recent interrupt-IN report the engine
    /// rejected for the device at `index` (`0` = none rejected since its
    /// attach, or no device live there).
    ///
    /// This is the controller's verdict on the device's *own* endpoint,
    /// captured when an interrupt-IN report is rejected. It tells a metal
    /// capture whether a fault surfaced as a transaction error, a split
    /// through a hub's translator, or a stall.
    #[must_use]
    pub fn last_report_fault_code(&self, index: usize) -> u8 {
        self.device(index)
            .map_or(0, |device| device.last_report_fault_code)
    }

    /// Read the controller's `USBCMD` for a one-shot bring-up diagnostic
    /// (delegates to [`Xhci::read_usbcmd`]), or `None` if the read faults.
    pub fn read_usbcmd(&mut self) -> Option<u32> {
        self.xhci.read_usbcmd()
    }

    /// Read the controller's `USBSTS` for a one-shot bring-up diagnostic
    /// (delegates to [`Xhci::read_usbsts`]), or `None` if the read faults.
    pub fn read_usbsts(&mut self) -> Option<u32> {
        self.xhci.read_usbsts()
    }

    /// Whether the controller has latched a fatal error or halted
    /// (delegates to [`Xhci::controller_faulted`]).
    ///
    /// A faulted controller raises no further interrupts until it is reset, so
    /// a downstream device's hot-plug and transfers go silent. The Pi 4 VL805
    /// latches a Host System Error during a downstream-device hot-removal
    /// teardown (after its Disable Slot completes), so the HCD checks this
    /// after servicing a wake and recovers with
    /// [`UsbHost::reset_and_reenumerate`]. A controller that would not stop a
    /// hung command ring is as faulted: no command it is given will run.
    #[must_use]
    pub fn controller_faulted(&mut self) -> bool {
        self.command_ring_dead() || self.xhci.controller_faulted()
    }

    /// Raw `PORTSC` of root-hub `port` (1-based) for a bring-up diagnostic,
    /// or `None` if the port is out of range or the read faults. A capture
    /// of the connect/power/enable/speed bits when enumeration stalls on a
    /// root port.
    pub fn port_status_raw(&mut self, port: u8) -> Option<u32> {
        self.xhci.port_status(port).ok().map(crate::PortStatus::raw)
    }

    /// The identity and position of the device served at `index`, or `None`
    /// when no device is live there.
    #[must_use]
    pub fn device_identity(&self, index: usize) -> Option<DeviceIdentity> {
        self.device(index).map(|device| device.identity)
    }

    /// Describe the served device at `index` as a discovered child
    /// [`HwNode`] parented at `parent_id` and assigned `node_id`.
    ///
    /// The node carries one [`HwMatchKey::usb`] of the device's
    /// `vid:pid` and the 24-bit class of the interface this driver
    /// brought up — both read from the device during enumeration, never
    /// assumed — so `devmgr` resolves a class driver's signed bind table
    /// against it. Its [`HwDeviceClass`] is derived from the interface
    /// class, the match key mirroring the PCI child node
    /// [`PciBus::describe_function`](tairix_abi::driver::pci::PciBus::describe_function)
    /// emits for the controller above it. The node's device address is the
    /// device's position on the bus — its root port above its Route String,
    /// never `0` — so every interface node of one composite device carries
    /// the same address and an inventory consumer can attribute sibling
    /// interfaces to their one physical device. A controller reset
    /// reassigns slots, but a device that comes back is where it was: a node
    /// kept across the reset still agrees with a sibling published after it,
    /// and no device published beside it can carry its address.
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] if no device is live at `index` (the
    ///   identity is captured only on a successful enumeration) — fail
    ///   closed, never a fabricated node.
    /// * [`DriverError::DeviceFault`] if the match key cannot be pushed.
    ///
    /// # Capabilities
    ///
    /// None — describing a node mints no resources; they are minted at the
    /// load gate.
    pub fn describe_device(
        &self,
        index: usize,
        parent_id: u32,
        node_id: u32,
    ) -> Result<HwNode, DriverError> {
        let device = self.device(index).ok_or(DriverError::NotFound)?;
        let identity = device.identity;
        // Derive the node's device class from the interface's own class
        // byte, never assumed: a HID interface is an input device, a
        // mass-storage interface a storage device. An unmapped class is
        // honestly `Other` — the match keys still carry the exact triple.
        let device_class = match identity.interface_class >> 16 {
            INTERFACE_CLASS_AUDIO => HwDeviceClass::Audio,
            INTERFACE_CLASS_HID => HwDeviceClass::Input,
            INTERFACE_CLASS_MASS_STORAGE => HwDeviceClass::Storage,
            _ => HwDeviceClass::Other,
        };
        let mut node = HwNode::new(node_id, parent_id, device_class);
        node.set_address(identity.bus_position());
        node.set_removable(identity.removable);
        node.push_match_key(HwMatchKey::usb(
            identity.vendor_id,
            identity.product_id,
            identity.interface_class,
        ))
        .map_err(|_| DriverError::DeviceFault)?;
        node.push_resource(HwResource::property(
            HwProperty::UsbInterface,
            u64::from(identity.interface_number),
        ))
        .map_err(|_| DriverError::DeviceFault)?;
        node.push_resource(HwResource::property(
            HwProperty::UsbSpeed,
            u64::from(bus_speed(device.speed).as_u8()),
        ))
        .map_err(|_| DriverError::DeviceFault)?;
        Ok(node)
    }
}

impl<H: RegisterBlock, M: DmaBank> Drop for UsbDevice<H, M> {
    /// Reset the controller before its memory goes: one that will not reset
    /// may still master every ring, context and buffer it was handed, which
    /// are then held for the kernel to quarantine when the driver exits.
    fn drop(&mut self) {
        if self.xhci.reset_to_ready(self.budget).is_err() {
            self.dma.withhold_all();
        }
    }
}

#[cfg(test)]
impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Test-only access to the register seam, so the crate's unit
    /// tests can drive and assert the mock controller's state.
    pub(crate) fn host_mut(&mut self) -> &mut H {
        &mut self.xhci.host
    }

    /// Test-only read of a served device's raw slot, so a hot-removal test
    /// can capture which slot a later trailing transfer event names.
    pub(crate) fn raw_device_slot(&self, index: usize) -> u8 {
        self.device(index).map_or(0, |device| device.slot)
    }

    /// Test-only read of a tracked hub's raw slot, so a hub-descent test can
    /// assert which slot the hub occupies.
    pub(crate) fn raw_hub_slot(&self, hub_index: usize) -> u8 {
        self.hub(hub_index).map_or(0, |hub| hub.slot)
    }

    /// Test-only read of a tracked hub's root port.
    pub(crate) fn raw_hub_root_port(&self, hub_index: usize) -> u8 {
        self.hub(hub_index).map_or(0, |hub| hub.root_port)
    }

    /// Test-only read of a tracked hub's slot and whether it is `SuperSpeed`.
    pub(crate) fn raw_hub_slot_and_speed(&self, hub_index: usize) -> (u8, bool) {
        self.hub(hub_index)
            .map_or((0, false), |hub| (hub.slot, hub.speed == SPEED_SUPER))
    }

    /// Test-only lookup of the bulk pipe at endpoint `address` of the
    /// interface at `index`.
    pub(crate) fn raw_bulk_pipe(&self, index: usize, address: u8) -> Option<BulkPipe> {
        self.device(index)?.bulk_pipe_of_dci(address_dci(address))
    }

    /// Test-only read of whether the root ports are owed a scan.
    pub(crate) const fn raw_root_change_pending(&self) -> bool {
        self.root_change_pending
    }

    /// Test-only count of the control transfers recorded, answered or not.
    pub(crate) fn recorded_controls(&self) -> usize {
        self.controls.iter().flatten().count()
    }

    /// Test-only count of the open control pipes.
    pub(crate) fn open_pipes(&self) -> usize {
        self.pipes.iter().flatten().count()
    }

    /// Test-only view of the DMA bank, so a test can observe its chunk
    /// accounting (a region allocated on attach, released on detach).
    pub(crate) fn dma_ref(&self) -> &M {
        &self.dma
    }

    /// Test-only command-ring occupancy, so a regression can prove a
    /// *rejected* command still retires its slot rather than leaking it.
    pub(crate) fn command_ring_in_flight(&self) -> usize {
        self.command_ring.in_flight()
    }
}

impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Decode one completed interrupt-IN [`TrbType::TransferEvent`] (already
    /// confirmed to target device `index`'s slot and interrupt endpoint)
    /// into a report length, copying the report bytes into `buf`.
    ///
    /// This performs only the *validation and copy* of one transfer; it does
    /// **not** touch the transfer ring. Re-arming the endpoint is the caller's
    /// (`next_report`) unconditional responsibility, so that a transfer whose
    /// completion code or buffer mapping this method rejects still leaves the
    /// endpoint re-armed for the next report (a single odd transfer must never
    /// silence the keyboard).
    ///
    /// `Ok(Some(len))` is a delivered report of `len` bytes; `Ok(None)` is a
    /// *successful* transfer that carried **zero** bytes (a zero-length
    /// packet — a residual equal to the whole request). A ZLP is not a report
    /// and not a fault: a composite/idle HID interface (a wireless MMO mouse's
    /// extra collection) legitimately completes an interrupt-IN transfer with
    /// no data. The caller re-arms and keeps the URB parked rather than
    /// replying, so an idle or ZLP-streaming device neither spins the class
    /// driver on empty completions nor is killed by a false fault.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] for an unexpected completion code, a
    /// completed-TRB address outside the interrupt ring, a misaligned or
    /// out-of-range ring slot, or a residual larger than the report.
    fn decode_transfer_report(
        &mut self,
        index: usize,
        event: Trb,
    ) -> Result<Option<(usize, usize)>, DriverError> {
        let region = self.device(index).ok_or(DriverError::NotFound)?.region;
        let ring_base = self.device_addr_of(region.int_ring)?;
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        if !matches!(
            event.completion_code(),
            Ok(CompletionCode::Success | CompletionCode::ShortPacket)
        ) {
            // Preserve the controller's verdict on the device's own
            // interrupt-IN endpoint before failing closed: a later hub
            // disconnect-confirmation control transfer resets the shared event
            // diagnostics, so this is the only surviving record of why the
            // report faulted (a transient transaction error vs. a device-gone /
            // stall code).
            device.last_report_fault_code = event.completion_code_raw();
            return Err(DriverError::DeviceFault);
        }
        // Map the completed TRB back to its slot's report buffer,
        // validating every step of the controller's claim.
        let offset = event
            .parameter
            .checked_sub(ring_base)
            .ok_or(DriverError::DeviceFault)?;
        let trb_len = trb::TRB_LEN as u64;
        if offset % trb_len != 0 {
            return Err(DriverError::DeviceFault);
        }
        let slot = usize::try_from(offset / trb_len).map_err(|_| DriverError::DeviceFault)?;
        if slot >= INT_RING_TRBS - 1 {
            return Err(DriverError::DeviceFault);
        }
        let residual =
            usize::try_from(event.transfer_residual()).map_err(|_| DriverError::DeviceFault)?;
        // The residual is the tail of the armed transfer the device left
        // untransferred, so the delivered report is the difference.
        let len = device
            .int_transfer
            .map(usize::from)
            .and_then(|armed| armed.checked_sub(residual))
            .ok_or(DriverError::DeviceFault)?;
        // A zero-length completion is a successful transfer that carried no
        // report; it is neither delivered nor a fault (the caller re-arms and
        // parks).
        if len == 0 {
            return Ok(None);
        }
        Ok(Some((region.report_bufs + slot * INT_TRANSFER_MAX, len)))
    }

    /// Retire device `index`'s just-completed interrupt-IN transfer.
    ///
    /// Called by [`Self::next_report`] for **every** completed transfer
    /// event addressed to that endpoint — including one whose report was
    /// rejected by [`Self::decode_transfer_report`] — so the transfer-ring
    /// software dequeue always matches what the controller has consumed.
    /// Retiring frees one ring slot, which [`Self::ensure_reports_armed`]
    /// immediately re-arms, so the endpoint is kept armed to
    /// [`INT_ARM_DEPTH`] and the controller always has a landing TRB for the
    /// next report rather than going idle between class-driver URBs.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] if the controller reported a completion
    /// when no transfer was in flight.
    fn retire_interrupt_transfer(&mut self, index: usize) -> Result<(), DriverError> {
        self.device_mut(index)
            .ok_or(DriverError::NotFound)?
            .int_ring
            .as_mut()
            .ok_or(DriverError::DeviceFault)?
            .retire_one()
    }
}

/// Bulk transfer serving: several TDs may be queued per direction (each
/// ring data slot pairs with its own staging buffer), completions are
/// decoded asynchronously off the shared event ring, and a device STALL is
/// recovered in place (Reset Endpoint → Set TR Dequeue Pointer →
/// `CLEAR_FEATURE(ENDPOINT_HALT)`) with every abandoned TD answered.
impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Region ring offset, staging-buffer offset, endpoint DCI, and xHCI
    /// slot of device `index`'s configured bulk endpoint for `direction`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] when no device is live at `index` or its
    /// interface carries no configured bulk endpoint in that direction.
    fn bulk_params(
        &self,
        index: usize,
        pipe: BulkPipe,
    ) -> Result<(usize, usize, u8, u8), DriverError> {
        let device = self.device(index).ok_or(DriverError::NotFound)?;
        let ring_off = device.region.bulk_ring_off(pipe);
        let bufs_off = device.region.bulk_bufs_off(pipe);
        let dci = device.bulk_dci(pipe);
        if dci == 0 {
            return Err(DriverError::NotFound);
        }
        Ok((ring_off, bufs_off, dci, device.slot))
    }

    /// TDs in flight on device `index`'s bulk ring for `pipe` (`0`
    /// when unconfigured or no device is live there).
    pub(crate) fn bulk_in_flight(&self, index: usize, pipe: BulkPipe) -> usize {
        self.device(index).map_or(0, |device| {
            device.bulk_ring(pipe).map_or(0, ProducerRing::in_flight)
        })
    }

    /// Queue one bulk-IN TD reading up to `len` bytes from device `index`,
    /// returning the ring data slot it occupies (the ticket its
    /// [`BulkComplete`] echoes). Several TDs may be queued; they complete
    /// in order through [`Self::poll_bulk`].
    ///
    /// # Errors
    ///
    /// * [`DriverError::NotFound`] — no configured bulk-IN endpoint.
    /// * [`DriverError::LengthOutOfRange`] — `len` exceeds
    ///   [`BULK_BUF_LEN`] (the caller chunks larger transfers).
    /// * [`DriverError::Busy`] — the ring is full; poll completions first.
    pub(crate) fn queue_bulk_in(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        len: usize,
    ) -> Result<usize, DriverError> {
        if pipe.direction != BulkDirection::In {
            return Err(DriverError::OutOfRange);
        }
        self.queue_bulk(index, pipe, len, None)
    }

    /// Queue one bulk-OUT TD writing `data` to device `index`, returning
    /// its ring data slot. As [`Self::queue_bulk_in`] otherwise.
    ///
    /// # Errors
    ///
    /// As [`Self::queue_bulk_in`].
    pub(crate) fn queue_bulk_out(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        data: &[u8],
    ) -> Result<usize, DriverError> {
        if pipe.direction != BulkDirection::Out {
            return Err(DriverError::OutOfRange);
        }
        self.queue_bulk(index, pipe, data.len(), Some(data))
    }

    /// Shared body of the bulk queue paths: stage the OUT bytes (when
    /// given), push one Normal TRB pointing at the slot's staging buffer,
    /// publish it, record the requested length, and ring the endpoint's
    /// doorbell.
    fn queue_bulk(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        len: usize,
        data: Option<&[u8]>,
    ) -> Result<usize, DriverError> {
        let (ring_off, bufs_off, dci, device_slot) = self.bulk_params(index, pipe)?;
        if len > BULK_BUF_LEN {
            return Err(DriverError::LengthOutOfRange);
        }
        let len_u32 = u32::try_from(len).map_err(|_| DriverError::LengthOutOfRange)?;
        let slot = {
            let device = self.device(index).ok_or(DriverError::NotFound)?;
            device
                .bulk_ring(pipe)
                .ok_or(DriverError::NotFound)?
                .enqueue_slot()
        };
        // Refuse a full ring before staging, so a rejected queue leaves no
        // half-written buffer.
        if self.bulk_in_flight(index, pipe) >= BULK_SLOTS - 1 {
            return Err(DriverError::Busy);
        }
        if let Some(bytes) = data {
            self.dma.write(bufs_off + slot * BULK_BUF_LEN, bytes)?;
        }
        let buffer = self.device_addr_of(bufs_off + slot * BULK_BUF_LEN)?;
        let normal = Trb::new(
            TrbType::Normal,
            buffer,
            len_u32,
            trb::CONTROL_IOC | trb::CONTROL_ISP,
        );
        let (outcome, link_slot) = {
            let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
            let ring = device.bulk_ring_mut(pipe).ok_or(DriverError::NotFound)?;
            (ring.push(normal)?, ring.link_slot())
        };
        publish(&mut self.dma, ring_off, link_slot, &outcome)?;
        {
            let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
            device.set_bulk_len(pipe, slot, len_u32);
        }
        self.xhci.ring_doorbell(device_slot, u32::from(dci))?;
        Ok(slot)
    }

    /// Reap the next completed bulk TD, if any: one a pipe recovery
    /// answered first, then the completions the service step routed here. A
    /// completed bulk-IN TD's bytes are copied into `in_buf`. Never blocks:
    /// `Ok(None)` when nothing has completed yet, or while the TD's pipe is
    /// being recovered.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no interface at `index`. A per-TD
    /// transfer failure is **not** an error here — it is reported in the
    /// returned [`BulkComplete::result`].
    pub(crate) fn poll_bulk(
        &mut self,
        index: usize,
        in_buf: &mut [u8],
    ) -> Result<Option<BulkComplete>, DriverError> {
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        if let Some((pipe, slot, error)) = device.aborted_bulk.pop_front() {
            return Ok(Some(BulkComplete {
                pipe,
                slot,
                result: Err(error),
            }));
        }
        while let Some(event) = self
            .device_mut(index)
            .and_then(|device| device.pending_bulk.pop_front())
        {
            if let Some(complete) = self.decode_bulk_event(index, event, in_buf)? {
                return Ok(Some(complete));
            }
        }
        Ok(None)
    }

    /// Decode one bulk [`TrbType::TransferEvent`], already known to target a
    /// configured bulk endpoint of device `index`.
    ///
    /// `None` for what a Stop Endpoint gives the TD it interrupted, which was
    /// answered when it was abandoned, and for a completion on a pipe being
    /// recovered, whose TDs the recovery answers. Any other failure halted
    /// the endpoint, or may have: its TD is retired and the pipe's recovery
    /// started, which answers it once the pipe serves again — a STALL as
    /// [`DriverError::EndpointStalled`], anything else as
    /// [`DriverError::DeviceFault`]. A completion the ring cannot account for
    /// leaves the controller and this engine out of step, so the pipe is
    /// restarted empty and its oldest TD fails.
    fn decode_bulk_event(
        &mut self,
        index: usize,
        event: Trb,
        in_buf: &mut [u8],
    ) -> Result<Option<BulkComplete>, DriverError> {
        let device = self.device(index).ok_or(DriverError::NotFound)?;
        let pipe = device
            .bulk_pipe_of_dci(event.endpoint_id())
            .ok_or(DriverError::DeviceFault)?;
        let serial = device.serial;
        if self.bulk_recovering(index, pipe) {
            return Ok(None);
        }
        let code = event.completion_code();
        if code.is_ok_and(CompletionCode::is_stopped) {
            return Ok(None);
        }
        let succeeded = matches!(
            code,
            Ok(CompletionCode::Success | CompletionCode::ShortPacket)
        );
        if !succeeded {
            if let Some(device) = self.device_mut(index) {
                device.last_bulk_fault_code = event.completion_code_raw();
            }
        }
        let Some(slot) = self.retire_completed_bulk_td(index, pipe, event)? else {
            self.begin_bulk_recovery(index, pipe, None)?;
            self.ask(route::Work::ResyncBulk {
                index,
                serial,
                pipe,
            });
            return Ok(None);
        };
        if succeeded {
            let result = self.bulk_transferred(index, pipe, slot, event, in_buf);
            return Ok(Some(BulkComplete { pipe, slot, result }));
        }
        let failure = if code == Ok(CompletionCode::StallError) {
            DriverError::EndpointStalled
        } else {
            DriverError::DeviceFault
        };
        self.begin_bulk_recovery(index, pipe, Some((slot, failure)))?;
        self.ask(route::Work::RecoverBulk {
            index,
            serial,
            pipe,
            failure,
        });
        Ok(None)
    }

    /// Whether device `index`'s bulk `pipe` is being recovered.
    pub(crate) fn bulk_recovering(&self, index: usize, pipe: BulkPipe) -> bool {
        self.device(index)
            .is_some_and(|device| device.bulk_recovery[pipe.index()] != BulkRecovery::Idle)
    }

    /// Whether an answer a recovery left for device `index`'s bulk `pipe`
    /// waits to be reaped.
    pub(crate) fn bulk_answer_waiting(&self, index: usize, pipe: BulkPipe) -> bool {
        self.device(index).is_some_and(|device| {
            device
                .aborted_bulk
                .iter()
                .any(|&(answered, _, _)| answered == pipe)
        })
    }

    /// Mark device `index`'s bulk `pipe` recovering, `failed` the TD whose
    /// failure started it, answered once the recovery ends.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] for no interface at `index`.
    pub(crate) fn begin_bulk_recovery(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        failed: Option<(usize, DriverError)>,
    ) -> Result<(), DriverError> {
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        device.bulk_recovery[pipe.index()] = BulkRecovery::Running(failed);
        Ok(())
    }

    /// Device `index`'s bulk `pipe` recovery ended with `recovered`: the TD
    /// that started it is answered — with its own failure when the pipe
    /// serves again, as a device fault when it does not — and the pipe
    /// serves.
    pub(crate) fn bulk_recovered(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        recovered: Result<(), DriverError>,
    ) {
        let Some(device) = self.device_mut(index) else {
            return;
        };
        let BulkRecovery::Running(failed) =
            core::mem::take(&mut device.bulk_recovery[pipe.index()])
        else {
            return;
        };
        if let Some((slot, failure)) = failed {
            let answer = if recovered.is_ok() {
                failure
            } else {
                DriverError::DeviceFault
            };
            // The front: it failed before every TD the recovery abandoned.
            let _ = device.aborted_bulk.try_push_front((pipe, slot, answer));
        }
    }

    /// Retire the TD `event` completes on device `index`'s bulk `pipe`,
    /// returning its ring slot: `None` when the ring cannot account for it —
    /// an address off the ring or misaligned, or a TD other than the oldest in
    /// flight.
    fn retire_completed_bulk_td(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        event: Trb,
    ) -> Result<Option<usize>, DriverError> {
        let (ring_off, _bufs_off, _dci, _device_slot) = self.bulk_params(index, pipe)?;
        let trb_len = trb::TRB_LEN as u64;
        let claimed = event
            .parameter
            .checked_sub(self.device_addr_of(ring_off)?)
            .filter(|offset| offset % trb_len == 0)
            .and_then(|offset| usize::try_from(offset / trb_len).ok());
        let ring = self
            .device_mut(index)
            .ok_or(DriverError::NotFound)?
            .bulk_ring_mut(pipe)
            .ok_or(DriverError::NotFound)?;
        let Some(slot) =
            claimed.filter(|&slot| ring.in_flight() != 0 && ring.dequeue_slot() == slot)
        else {
            return Ok(None);
        };
        ring.retire_one()?;
        Ok(Some(slot))
    }

    /// The bytes a successful TD in ring `slot` of `pipe` moved, a bulk-IN
    /// TD's copied into `in_buf`: a residual past the request, or more bytes
    /// than `in_buf` holds, is a claim no honest controller makes and fails
    /// the TD.
    fn bulk_transferred(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        slot: usize,
        event: Trb,
        in_buf: &mut [u8],
    ) -> Result<u32, DriverError> {
        let (_ring_off, bufs_off, _dci, _device_slot) = self.bulk_params(index, pipe)?;
        let requested = self
            .device(index)
            .ok_or(DriverError::NotFound)?
            .bulk_len(pipe, slot);
        let transferred = requested
            .checked_sub(event.transfer_residual())
            .ok_or(DriverError::DeviceFault)?;
        if pipe.direction == BulkDirection::In {
            let count = usize::try_from(transferred).map_err(|_| DriverError::DeviceFault)?;
            let landed = in_buf.get_mut(..count).ok_or(DriverError::DeviceFault)?;
            self.dma.read(bufs_off + slot * BULK_BUF_LEN, landed)?;
        }
        Ok(transferred)
    }

    /// Retire every TD still queued on device `index`'s stopped bulk `pipe`
    /// and drop what is parked for it, answering each TD with `dropped` when
    /// given; without it the caller answers the transfer itself. Called only
    /// once the controller has stopped the endpoint: every completion of a TD
    /// on it precedes that command's own, so all are parked by then, and none
    /// can arrive later naming a slot of the ring that replaces this one.
    fn abandon_bulk_tds(
        &mut self,
        index: usize,
        pipe: BulkPipe,
        dropped: Option<DriverError>,
    ) -> Result<(), DriverError> {
        let device = self.device_mut(index).ok_or(DriverError::NotFound)?;
        let dci = device.bulk_dci(pipe);
        device
            .pending_bulk
            .retain(|event| event.endpoint_id() != dci);
        device.aborted_bulk.retain(|&(owner, _, _)| owner != pipe);
        loop {
            let slot = {
                let ring = device.bulk_ring_mut(pipe).ok_or(DriverError::NotFound)?;
                if ring.in_flight() == 0 {
                    return Ok(());
                }
                let slot = ring.dequeue_slot();
                ring.retire_one()?;
                slot
            };
            if let Some(error) = dropped {
                device
                    .aborted_bulk
                    .try_push_back((pipe, slot, error))
                    .map_err(|_| DriverError::Busy)?;
            }
        }
    }

    /// The raw completion code of device `index`'s most recent failed bulk
    /// transfer, `0` when none has failed or the last was abandoned at its
    /// deadline.
    #[must_use]
    pub fn last_bulk_fault_code(&self, index: usize) -> u8 {
        self.device(index)
            .map_or(0, |device| device.last_bulk_fault_code)
    }
}

impl<H: RegisterBlock, M: DmaBank> UsbDevice<H, M> {
    /// Deliver device `index`'s next interrupt-IN report into `buf` from the
    /// engine's report buffer, keeping the endpoint armed to depth. Never
    /// blocks: `Ok(None)` when no report is buffered (the class driver's URB
    /// stays parked).
    ///
    /// `request` is the longest report the class driver expects; the first
    /// fixes the length the endpoint is armed to — that, or one service
    /// interval's payload when it is longer — and nothing is armed before it.
    ///
    /// The service step captures every report off the controller's ring as
    /// it arrives, so a starved class driver collects buffered reports
    /// rather than losing them.
    ///
    /// # Errors
    ///
    /// [`DriverError::DeviceFault`] when no device is live at `index` or the
    /// device's interface carries no interrupt endpoint;
    /// [`DriverError::LengthOutOfRange`] for a `request` of zero or past
    /// [`INT_TRANSFER_MAX`], for a payload no transfer buffer holds, or when
    /// memory for the report queue runs out; [`DriverError::OutOfRange`] for
    /// a `request` other than the first; a report longer than `buf`
    /// ([`DriverError::BufferTooSmall`]); a recorded fail-closed report
    /// completion, surfaced once; or a fault reading the event ring.
    pub fn next_report(
        &mut self,
        index: usize,
        request: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, DriverError> {
        {
            let Some(device) = self.device_mut(index) else {
                // Not enumerated: there is no endpoint to drain.
                return Err(DriverError::DeviceFault);
            };
            if device.int_dci == DCI_CONTROL || device.int_ring.is_none() {
                // A bulk-only interface has no interrupt endpoint to read.
                return Err(DriverError::DeviceFault);
            }
            device.fix_int_transfer(request)?;
        }
        self.ensure_reports_armed(index)?;
        let device = self.device_mut(index).ok_or(DriverError::DeviceFault)?;
        if let Some(len) = device.reports.pop_into(buf)? {
            return Ok(Some(len));
        }
        if let Some(err) = self
            .device_mut(index)
            .and_then(|device| device.report_fault.take())
        {
            return Err(err);
        }
        Ok(None)
    }

    /// Drop every report device `index` has delivered to the controller and
    /// [`Self::next_report`] has not yet handed out, those still on the event
    /// ring included, so the next report is one the device sends after this.
    /// A recorded fault is the endpoint's, not a report's, and is kept.
    ///
    /// Draining the ring routes what else it held too; the operations that
    /// wakes run at the next service step.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] when no device is live at `index`, or a
    /// fault reading the event ring.
    pub(crate) fn discard_reports(&mut self, index: usize) -> Result<(), DriverError> {
        self.advance(self.now_ns)?;
        self.device_mut(index)
            .ok_or(DriverError::NotFound)?
            .reports
            .clear();
        Ok(())
    }

    /// Total interrupt-IN reports dropped across all served devices because a
    /// class driver fell more than [`REPORT_QUEUE_CAP`] reports behind.
    ///
    /// A genuinely stalled consumer (one that has stopped reading for many
    /// report intervals) cannot make the engine hold unbounded memory: the
    /// oldest buffered report is dropped so the newest device state is kept.
    /// This count lets the HCD surface that loss for diagnostics rather than
    /// hiding it.
    #[must_use]
    pub fn dropped_report_total(&self) -> u64 {
        self.devices
            .iter()
            .flatten()
            .map(|device| device.dropped_reports)
            .sum()
    }

    /// Capture one completion on device `index`'s interrupt-IN endpoint,
    /// retiring its TRB:
    ///
    /// * a real report is copied out of its DMA slot and **enqueued** into the
    ///   device's report FIFO ([`Self::enqueue_report`]), and the endpoint is
    ///   re-armed to depth — so its bytes survive the slot being reused;
    /// * a *successful* zero-length completion (a ZLP) carries no report: the
    ///   endpoint is simply re-armed;
    /// * a *successful* completion whose claimed buffer mapping is malformed
    ///   (a forged residual or TRB pointer) leaves the endpoint un-halted, so
    ///   there is nothing to recover: it **records** a fail-closed fault on
    ///   the device for [`Self::next_report`] to surface once;
    /// * any halting code — a STALL, babble, data-buffer or TRB error, and a
    ///   USB or split *transaction* error alike — leaves the endpoint halted.
    ///   Its recovery is started as an operation while the endpoint's ladder
    ///   allows, and owed to the driver's paced retry otherwise. A halting
    ///   code is not, on its own, a hot-removal: a transaction error is
    ///   exactly what a present-but-disturbed device produces.
    ///
    /// While a recovery of the endpoint runs, its ring is the recovery's: a
    /// completion is left alone and flagged, for the recovery to act on once
    /// it ends.
    fn capture_report_event(&mut self, index: usize, event: Trb) {
        let Some(device) = self.device_mut(index) else {
            return;
        };
        if device.int_recovering {
            device.int_recovery_pending = true;
            return;
        }
        let serial = device.serial;
        let captured = self
            .decode_transfer_report(index, event)
            .and_then(|report| match report {
                Some((at, len)) => self.enqueue_report(index, at, len),
                None => Ok(()),
            });
        let _ = self.retire_interrupt_transfer(index);
        match captured {
            Ok(()) => {
                if let Some(device) = self.device_mut(index) {
                    device.recovery.completed();
                }
                let _ = self.ensure_reports_armed(index);
            }
            Err(err)
                if matches!(
                    event.completion_code(),
                    Ok(CompletionCode::Success | CompletionCode::ShortPacket)
                ) =>
            {
                self.record_report_fault(index, err);
            }
            Err(_) => {
                let recover = self
                    .device_mut(index)
                    .is_some_and(|device| device.recovery.halted() == OnHalt::Recover);
                if recover {
                    self.ask(route::Work::RecoverReports { index, serial });
                }
            }
        }
    }

    /// Buffer device `index`'s report of `len` bytes at DMA offset `at` as
    /// delivered, before its slot is re-armed, dropping the **oldest**
    /// buffered report (and counting the loss) when the queue is full, so a
    /// consumer that catches up sees the newest device state.
    ///
    /// # Errors
    ///
    /// [`DriverError::NotFound`] when no device is live at `index`, or a
    /// fault reading the slot.
    fn enqueue_report(&mut self, index: usize, at: usize, len: usize) -> Result<(), DriverError> {
        let Self { devices, dma, .. } = self;
        let device = devices
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or(DriverError::NotFound)?;
        if device.reports.push_with(len, |slot| dma.read(at, slot))? {
            device.dropped_reports = device.dropped_reports.saturating_add(1);
        }
        Ok(())
    }

    /// Record a fail-closed report completion on device `index` for the next
    /// [`Self::next_report`] to surface, keeping the first observed fault.
    fn record_report_fault(&mut self, index: usize, err: DriverError) {
        if let Some(device) = self.device_mut(index) {
            device.report_fault.get_or_insert(err);
        }
    }
}
