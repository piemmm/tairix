//! The pre-MMU `VideoCore` firmware channel, and the boot-time requests the
//! kernel makes over it.
//!
//! On a Raspberry Pi several things the kernel needs before it has drivers
//! belong to the firmware rather than to any register the ARM cores can
//! reach: the scan-out surface the boot console renders into, and the rate
//! the ARM cores run at. Both are property exchanges over the same mailbox
//! doorbell, so the doorbell, the DMA-visible property buffer, and the
//! transport built over them live here once and every early consumer borrows
//! them (`with_transport`).
//!
//! Each exchange is split into a pure half that takes a transport
//! (`raise_over`) and a wrapper that builds one, so the protocol sequence is
//! host-testable against the mock firmware — QEMU models no `VideoCore`, so
//! that is the only way to prove it before it runs on metal.
//!
//! # Boot CPU, before the MMU
//!
//! Every boot-time exchange runs from the boot CPU before
//! `enable_mmu_and_vectors`: with the data caches still off the
//! CPU↔firmware exchange is coherent by construction, so no cache maintenance
//! is needed, and there is no other thread to serialise against. That is also
//! why the property buffer is a plain cell rather than a lock — an atomic
//! read-modify-write on MMU-off Device-typed memory is architecturally
//! UNPREDICTABLE, the constraint that orders the whole aarch64 boot
//! (`plans/PI.md` P6c-2).
//!
//! # The storage floor's channel
//!
//! The kernel's one later exchange is its storage floor's: it lends the SD
//! host's driver a channel to the firmware
//! ([`with_floor_channel`](crate::firmware::with_floor_channel)) for that
//! driver's bring-up alone — over a cacheable buffer the floor carves,
//! cleaned and invalidated around each exchange — before the user-space
//! `vcmailbox` service can load from the card the bring-up reaches. Each
//! request is admitted for the driver of the SD host's node exactly as that
//! service would admit it (`tairix_vcmailbox::admit_request`), so the driver
//! is never lent the rest of the firmware's interface. From then on the
//! service alone owns the doorbell.
//!
//! # Fail closed
//!
//! A board whose tree carries no mailbox (QEMU `virt`), a doorbell window
//! shorter than the register block, an unreachable firmware, or a malformed
//! answer all yield `None`. The console then keeps the UART and the clock
//! keeps whatever rate the firmware had chosen; nothing is guessed at.

use tairix_abi::driver::mailbox::{MailboxChannel, MAILBOX_PROPERTY_WORDS};
use tairix_abi::{DriverError, HwNode, MmioMapper, RegisterWindow};
use tairix_fdt::Fdt;
use tairix_sync::SpinLock;

/// Compatible string of the BCM283x/BCM2711 firmware mailbox doorbell.
///
/// The single source of the match identity is the device's own client crate,
/// so this discovery and the `vcmailbox` service driver's `BIND_KEYS` cannot
/// diverge.
use tairix_vcmailbox::{
    admit_request, arm_physical_to_bus, query_clock_rate, set_clock_rate, BufferCoherency,
    ClockRateQuery, FirmwareClock, MailboxError, MailboxTransport, MmioMailbox, APERTURE_LIMIT,
    DEFAULT_BUS_ALIAS, DEFAULT_POLL_BUDGET, MAILBOX_COMPATIBLE, MAILBOX_REGS_LEN_BYTES,
    PROPERTY_LEN_BYTES,
};

/// Bytes of the property buffer the storage floor carves for its channel to
/// the firmware: one property message.
pub const FLOOR_BUFFER_LEN: usize = PROPERTY_LEN_BYTES;

/// The exclusive CPU-physical bound that buffer must lie below: the
/// firmware's aperture.
pub const FLOOR_BUFFER_LIMIT: u64 = APERTURE_LIMIT;

/// A firmware mailbox doorbell located in a flattened device tree.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DiscoveredMailbox {
    /// CPU-physical MMIO base of the doorbell register block (the node's
    /// first `reg` entry, decoded with its parent bus's cell counts and
    /// translated through the ancestor buses' `ranges`).
    pub base: u64,
    /// Length in bytes of the register window.
    pub len: u64,
}

/// Find the `VideoCore` firmware mailbox doorbell in `fdt`.
///
/// The walk early-returns at the matched node
/// ([`tairix_fdt::scan_translated`]), so it stays safe with the MMU off.
/// Returns `None` when the tree carries no mailbox (e.g. QEMU `virt`) or the
/// node's `reg` cannot be decoded or translated.
#[must_use]
pub fn find_mailbox(fdt: &Fdt<'_>) -> Option<DiscoveredMailbox> {
    tairix_fdt::scan_translated(fdt, |node, levels, depth| {
        let compatible = node.property("compatible")?;
        if !compatible.iter_strings().any(|s| s == MAILBOX_COMPATIBLE) {
            return None;
        }
        let (base, len) = tairix_fdt::translated_reg(node, depth, levels, 0)?;
        Some(DiscoveredMailbox { base, len })
    })
}

/// Raise the ARM core clock to the highest rate the firmware behind
/// `transport` will deliver, returning that rate in Hz.
///
/// The firmware leaves the ARM clock wherever it last put it, which after the
/// boot window is `arm_freq_min` — 600 MHz on a Pi 4B whose parts are rated
/// at 1.5 GHz. Everything between here and the autoloaded frequency driver
/// (mounting the root volume, unlocking it, reaching a login) would otherwise
/// run at 40% of the machine's speed, so the kernel asks for full speed while
/// it is still the only thing that can.
///
/// This is a one-shot floor, not a policy: it takes no view of utilisation
/// and never lowers the clock. Once `devmgr` autoloads the frequency driver,
/// the kernel governor's targets take over.
///
/// The ceiling is read from the firmware rather than assumed, so a board
/// whose `config.txt` raises `arm_freq` is raised to *its* ceiling. Returns
/// `None` when the firmware is unreachable or unhelpful, leaving the clock as
/// it was.
///
/// Host-testable against the protocol-faithful mock firmware — QEMU models no
/// `VideoCore`, so this is the only way to prove the sequence before it runs
/// on metal.
///
/// A zero ceiling is how the property interface spells "no such clock", so it
/// yields `None` rather than a request for a stopped clock.
#[must_use]
pub fn raise_over(transport: &mut dyn MailboxTransport) -> Option<u32> {
    let ceiling = query_clock_rate(transport, FirmwareClock::Arm, ClockRateQuery::Max).ok()?;
    if ceiling == 0 {
        return None;
    }
    set_clock_rate(transport, FirmwareClock::Arm, ceiling).ok()
}

/// The doorbell transport over the mapped `regs` and property `buffer`, the
/// buffer addressed as the firmware's bus sees it: the one construction the
/// boot path and the storage floor share.
fn mailbox_over(
    regs: RegisterWindow,
    buffer: RegisterWindow,
    coherency: BufferCoherency,
) -> Option<MmioMailbox> {
    let bus = arm_physical_to_bus(buffer.phys_base(), DEFAULT_BUS_ALIAS).ok()?;
    MmioMailbox::with_coherency(regs, buffer, bus, DEFAULT_POLL_BUDGET, coherency).ok()
}

/// Run `body` with the storage floor's channel to the firmware: over the
/// doorbell `fdt` names, mapped through `map`, and the cacheable property
/// `buffer` the floor carved below [`FLOOR_BUFFER_LIMIT`], admitting each
/// request for the driver of `node`.
///
/// `body` is handed `None` when there is no `buffer`, the tree names no
/// doorbell as long as the register block, or the doorbell cannot be mapped.
/// The channel lives for `body` alone.
pub fn with_floor_channel<R>(
    fdt: &Fdt<'_>,
    map: &dyn MmioMapper,
    buffer: Option<RegisterWindow>,
    node: &HwNode,
    body: impl FnOnce(Option<&dyn MailboxChannel>) -> R,
) -> R {
    let mailbox = buffer
        .zip(find_mailbox(fdt))
        .and_then(|(buffer, doorbell)| {
            let whole =
                usize::try_from(doorbell.len).is_ok_and(|len| len >= MAILBOX_REGS_LEN_BYTES);
            let regs = whole
                .then(|| map.map_window(doorbell.base, MAILBOX_REGS_LEN_BYTES).ok())
                .flatten()?;
            let coherency = BufferCoherency::new(clean_invalidate_buffer, clean_invalidate_buffer);
            mailbox_over(regs, buffer, coherency)
        });
    let channel = mailbox.map(|mailbox| FloorChannel::new(mailbox, node));
    body(
        channel
            .as_ref()
            .map(|channel| channel as &dyn MailboxChannel),
    )
}

/// Clean and invalidate the property buffer at `base` around the firmware's
/// DMA of it: the mailbox names the buffer by its physical address, which the
/// kernel's identity direct map makes its CPU address too.
fn clean_invalidate_buffer(base: u64, len: usize) {
    if let Ok(start) = usize::try_from(base) {
        crate::kernel_arch::clean_invalidate_dcache_range(start, len);
    }
}

/// The storage floor's channel to the firmware: each request admitted for the
/// driver of `node`, as the mailbox service admits it, before it reaches the
/// doorbell.
struct FloorChannel<'n, T> {
    transport: SpinLock<T>,
    node: &'n HwNode,
}

impl<'n, T: MailboxTransport> FloorChannel<'n, T> {
    const fn new(transport: T, node: &'n HwNode) -> Self {
        Self {
            transport: SpinLock::new(transport),
            node,
        }
    }
}

impl<T: MailboxTransport> MailboxChannel for FloorChannel<'_, T> {
    fn exchange(&self, message: &mut [u32; MAILBOX_PROPERTY_WORDS]) -> Result<(), DriverError> {
        admit_request(message, Some(self.node)).map_err(|_| DriverError::PermissionDenied)?;
        self.transport
            .lock()
            .exchange(message)
            .map_err(MailboxError::as_driver_error)
    }
}

/// Run `body` against a transport over `mailbox`'s doorbell and the shared
/// property buffer.
///
/// The boot path holds its whole conversation with the firmware inside one
/// call: a request the firmware leaves unanswered stays owed on that
/// transport, so the next exchange waits for its reply rather than taking it
/// as its own, which a second transport over the same buffer could not know
/// to do.
///
/// `None` when the doorbell window is shorter than the register block, its
/// base cannot be addressed, or the buffer's bus address cannot be formed —
/// in which case `body` never runs and no exchange is attempted.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub fn with_transport<R>(
    mailbox: DiscoveredMailbox,
    body: impl FnOnce(&mut dyn MailboxTransport) -> R,
) -> Option<R> {
    metal::with_transport(mailbox, body)
}

/// The freestanding half: the doorbell window, the DMA-visible property
/// buffer, and the transport over them. Target-only — it addresses MMIO and
/// SDRAM through boot-identity addresses.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
mod metal {
    use core::cell::UnsafeCell;
    use core::ptr::NonNull;

    use tairix_abi::RegisterWindow;
    use tairix_vcmailbox::{
        BufferCoherency, MailboxTransport, MAILBOX_REGS_LEN_BYTES, PROPERTY_LEN_BYTES,
    };

    use super::DiscoveredMailbox;

    /// The DMA-visible mailbox property message, 16-byte aligned as the
    /// doorbell protocol requires.
    #[repr(align(16))]
    struct PropertyBuffer(UnsafeCell<[u8; PROPERTY_LEN_BYTES]>);

    // SAFETY: written only by the single-threaded boot CPU inside the
    // pre-MMU discovery sequence, before SMP bring-up and before any other
    // user of the doorbell exists; never touched again afterwards, since the
    // user-space `vcmailbox` service owns the doorbell from then on.
    unsafe impl Sync for PropertyBuffer {}

    static PROPERTY_BUFFER: PropertyBuffer =
        PropertyBuffer(UnsafeCell::new([0; PROPERTY_LEN_BYTES]));

    /// Build the transport and hand it to `body`. See
    /// [`super::with_transport`].
    pub(super) fn with_transport<R>(
        mailbox: DiscoveredMailbox,
        body: impl FnOnce(&mut dyn MailboxTransport) -> R,
    ) -> Option<R> {
        if mailbox.len < MAILBOX_REGS_LEN_BYTES as u64 {
            return None;
        }
        // SAFETY: single-threaded boot CPU, pre-publication (see
        // `PropertyBuffer`): no other reference to the buffer exists.
        let buffer_ptr = unsafe { NonNull::new_unchecked(PROPERTY_BUFFER.0.get().cast::<u8>()) };
        let buffer_phys = buffer_ptr.as_ptr() as u64;
        let doorbell_ptr = NonNull::new(usize::try_from(mailbox.base).ok()? as *mut u8)?;
        // SAFETY: `mailbox.base` is the FDT-discovered, `ranges`-translated
        // CPU-physical doorbell window (boot runs identity-addressed), at
        // least `MAILBOX_REGS_LEN_BYTES` long (checked above); the buffer
        // pointer covers exactly `PROPERTY_LEN_BYTES` of the static above.
        // Both windows are accessed only through `RegisterWindow`'s checked
        // 32-bit accessors, and neither outlives this call.
        let regs = unsafe {
            RegisterWindow::from_mapping(mailbox.base, doorbell_ptr, MAILBOX_REGS_LEN_BYTES)
        };
        let buffer =
            unsafe { RegisterWindow::from_mapping(buffer_phys, buffer_ptr, PROPERTY_LEN_BYTES) };
        // The caches are off, so the buffer agrees with the firmware as it is.
        let mut transport = super::mailbox_over(regs, buffer, BufferCoherency::none())?;
        Some(body(&mut transport))
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::{find_mailbox, raise_over, with_floor_channel, FloorChannel};
    use crate::raspi_fixture::raspi_like_arm;
    use tairix_abi::driver::mailbox::MailboxChannel;
    use tairix_abi::driver::supply::{IoRail, PowerRail};
    use tairix_abi::{
        DriverError, HwDeviceClass, HwNode, HwResource, MmioMapError, MmioMapper, RegisterWindow,
    };
    use tairix_fdt::Fdt;
    use tairix_vcmailbox::mock::MockFirmware;
    use tairix_vcmailbox::{
        encode_clock_rate_query, encode_clock_rate_write, encode_gpio_state_write, ClockRateQuery,
        FirmwareClock, PROPERTY_LEN_BYTES, SD_HOST_KEY,
    };

    /// The SD host's node, its rails on expander lines 4 and 6.
    fn sd_host() -> HwNode {
        let mut node = HwNode::new(3, 0, HwDeviceClass::Storage);
        node.push_match_key(SD_HOST_KEY).expect("room");
        node.push_resource(HwResource::io_rail(IoRail {
            line: 4,
            level_1v8: Some(true),
            level_3v3: Some(false),
            settle_us: 5000,
        }))
        .expect("room");
        let power = PowerRail {
            line: 6,
            active_low: false,
            startup_us: 0,
            off_on_us: 0,
        };
        node.push_resource(HwResource::power_rail(power).expect("fits"))
            .expect("room");
        node
    }

    /// Regression: the floor drove the doorbell with no admission, so the SD
    /// bring-up could send the firmware any request at all.
    #[test]
    fn the_floor_channel_forwards_for_its_node_only_what_the_service_would() {
        let node = sd_host();
        let channel = FloorChannel::new(MockFirmware::healthy(), &node);
        let mut own_line = encode_gpio_state_write(4, true);
        assert_eq!(channel.exchange(&mut own_line), Ok(()));
        assert!(channel.transport.lock().gpio_high(4));
        let mut clock = encode_clock_rate_query(FirmwareClock::Emmc2, ClockRateQuery::Current);
        assert_eq!(channel.exchange(&mut clock), Ok(()));
        let mut other_line = encode_gpio_state_write(5, true);
        let mut arm = encode_clock_rate_write(FirmwareClock::Arm, 1_500_000_000);
        for message in [&mut other_line, &mut arm] {
            assert_eq!(
                channel.exchange(message),
                Err(DriverError::PermissionDenied)
            );
        }
        let firmware = channel.transport.lock();
        assert!(
            !firmware.gpio_high(5),
            "a refused request never reaches the firmware"
        );
        assert_eq!(firmware.arm.hz, MockFirmware::healthy().arm.hz);
    }

    /// A mapper that maps nothing.
    struct Unmapped;

    impl MmioMapper for Unmapped {
        fn map_window(&self, _phys: u64, _len: usize) -> Result<RegisterWindow, MmioMapError> {
            Err(MmioMapError::Unsupported)
        }
    }

    #[test]
    fn no_buffer_no_doorbell_or_an_unmapped_doorbell_lends_no_channel() {
        #[repr(align(16))]
        struct Buffer([u8; PROPERTY_LEN_BYTES]);

        let node = sd_host();
        let lends_none = |fdt: &Fdt<'_>, buffer: Option<RegisterWindow>| {
            with_floor_channel(fdt, &Unmapped, buffer, &node, |channel| channel.is_none())
        };
        let pi = raspi_like_arm(0x7e20_1000, 0x7e21_5040);
        let pi = Fdt::new(&pi).expect("valid fdt");
        assert!(lends_none(&pi, None));

        let mut backing = Buffer([0; PROPERTY_LEN_BYTES]);
        let base = NonNull::from(&mut backing.0).cast::<u8>();
        let window = || {
            // SAFETY: `base` is `backing`, 16-byte aligned and
            // `PROPERTY_LEN_BYTES` long, which outlives every window taken
            // over it; no exchange is ever made over one, as no doorbell maps.
            unsafe { RegisterWindow::from_mapping(0x1000, base, PROPERTY_LEN_BYTES) }
        };
        assert!(lends_none(&pi, Some(window())), "the doorbell does not map");

        let mut builder = tairix_fdt::write::FdtWriter::new();
        builder.begin_node("");
        builder.end_node();
        let empty_tree = builder.build();
        let empty = Fdt::new(&empty_tree).expect("valid fdt");
        assert!(lends_none(&empty, Some(window())), "no doorbell named");
    }

    #[test]
    fn finds_the_mailbox_in_a_raspi_tree() {
        // The fixture mirrors the real Pi 4 tree: the mailbox sits under
        // `/soc` at bus address `0x7E00_B880`, remapped by `ranges` to
        // CPU-physical `0xFE00_B880`.
        let blob = raspi_like_arm(0x7e20_1000, 0x7e21_5040);
        let fdt = Fdt::new(&blob).expect("valid fdt");
        let mailbox = find_mailbox(&fdt).expect("mailbox present");
        assert_eq!(mailbox.base, 0xfe00_b880);
        assert_eq!(mailbox.len, 0x40);
    }

    #[test]
    fn the_clock_is_raised_to_the_ceiling_the_firmware_reports() {
        // The reported defect's boot half: the board is found at its minimum
        // and must be asked for its own ceiling, not a board constant.
        let mut firmware = MockFirmware::healthy();
        firmware.arm.min_hz = 600_000_000;
        firmware.arm.max_hz = 1_500_000_000;
        firmware.arm.hz = 600_000_000;
        assert_eq!(raise_over(&mut firmware), Some(1_500_000_000));
        assert_eq!(firmware.arm.hz, 1_500_000_000);
    }

    #[test]
    fn an_overclocked_board_is_raised_to_its_own_ceiling() {
        let mut firmware = MockFirmware::healthy();
        firmware.arm.max_hz = 2_000_000_000;
        assert_eq!(raise_over(&mut firmware), Some(2_000_000_000));
    }

    #[test]
    fn a_firmware_that_does_not_know_the_clock_is_left_alone() {
        // Zero means "no such clock". Setting the clock to it would ask for a
        // stopped one, so nothing is asked at all.
        let mut firmware = MockFirmware::healthy();
        firmware.arm.max_hz = 0;
        let before = firmware.arm.hz;
        assert_eq!(raise_over(&mut firmware), None);
        assert_eq!(firmware.arm.hz, before, "the clock is left as it was");
    }

    #[test]
    fn no_mailbox_in_a_mailboxless_tree_is_none() {
        // A virt-like tree carries no `brcm,bcm2835-mbox` node, so neither
        // the video console nor the clock raise attempts an exchange.
        let mut builder = tairix_fdt::write::FdtWriter::new();
        builder.begin_node("");
        builder.begin_node("pl011@9000000");
        builder.prop_str("compatible", "arm,pl011");
        builder.end_node();
        builder.end_node();
        let blob = builder.build();
        let fdt = Fdt::new(&blob).expect("valid fdt");
        assert!(find_mailbox(&fdt).is_none());
    }
}
