//! The event-id registry: the one place each component's identifiers come
//! from.
//!
//! A component owns one [`EventRange`] and takes every identifier it emits
//! from it, so no two components ever give one number two meanings. The
//! `cargo xtask event-ids` stage of `ci` reads [`OWNERS`] and fails when a
//! crate defines an identifier outside its range, when a crate defines
//! identifiers with no range here, or when two crates define one value.

use crate::EventId;

/// A half-open span of event identifiers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EventRange {
    start: u32,
    end: u32,
}

impl EventRange {
    /// The identifiers `start..end`.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// The first identifier in the range.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// One past the last identifier in the range.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }

    /// Whether `id` lies in the range.
    #[must_use]
    pub const fn contains(self, id: EventId) -> bool {
        id.0 >= self.start && id.0 < self.end
    }

    /// Whether the two ranges share an identifier.
    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// A range and the crates, by workspace path, that take identifiers from it.
#[derive(Copy, Clone, Debug)]
pub struct Owner {
    /// The identifiers the crates share.
    pub range: EventRange,
    /// The crates; between them each identifier means one event.
    pub crates: &'static [&'static str],
}

/// `kernel/sec`.
pub const KERNEL_SEC: EventRange = EventRange::new(1_000, 2_000);
/// `kernel/mem` and the reclaim model it applies.
pub const KERNEL_MEM: EventRange = EventRange::new(2_000, 3_000);
/// `kernel/ipc`.
pub const KERNEL_IPC: EventRange = EventRange::new(3_000, 4_000);
/// The kernel's audit catalogue, its boot path and its architecture ports.
pub const KERNEL: EventRange = EventRange::new(4_000, 5_000);
/// `kernel/syscall`.
pub const KERNEL_SYSCALL: EventRange = EventRange::new(5_000, 6_000);
/// The parser sandbox.
pub const SANDBOX: EventRange = EventRange::new(6_000, 7_000);
/// The driver host.
pub const DRVHOST: EventRange = EventRange::new(7_000, 8_000);
/// The System Information service.
pub const SYSINFOD: EventRange = EventRange::new(8_000, 9_000);
/// PID 1 and its service manager.
pub const INIT: EventRange = EventRange::new(9_000, 10_000);
/// The session authority.
pub const LOGIN: EventRange = EventRange::new(10_000, 11_000);
/// The application-bundle load gate.
pub const APPLOAD: EventRange = EventRange::new(11_000, 12_000);
/// ARXFS.
pub const ARXFS: EventRange = EventRange::new(12_000, 13_000);
/// The device manager.
pub const DEVMGR: EventRange = EventRange::new(13_000, 14_000);
/// The seat manager.
pub const SEATMGR: EventRange = EventRange::new(14_000, 15_000);
/// The display service.
pub const DISPLAY_SERVICE: EventRange = EventRange::new(15_000, 16_000);
/// The network stack.
pub const NETSTACK: EventRange = EventRange::new(16_000, 17_000);
/// The font service.
pub const FONTD: EventRange = EventRange::new(17_000, 18_000);
/// The kernel console-output engine.
pub const CONSOLE_OUT: EventRange = EventRange::new(18_000, 19_000);
/// The graphical login screen.
pub const GREETER: EventRange = EventRange::new(19_000, 20_000);
/// The desktop session.
pub const DESKTOP_SESSION: EventRange = EventRange::new(20_000, 21_000);
/// The app-data service.
pub const CONFD: EventRange = EventRange::new(21_000, 22_000);
/// The desktop's sound-cue authority.
pub const SOUNDD: EventRange = EventRange::new(22_000, 23_000);
/// The time service.
pub const TIMED: EventRange = EventRange::new(23_000, 24_000);
/// The driver runtime: a driver's ending, in every driver.
pub const DRIVER_RUNTIME: EventRange = EventRange::new(24_000, 24_100);
/// The I2C controller side.
pub const I2C_BUS: EventRange = EventRange::new(24_100, 24_200);
/// The DMA controller side.
pub const DMA_ENGINE: EventRange = EventRange::new(24_200, 24_300);
/// The clock controller side.
pub const CLOCK: EventRange = EventRange::new(24_300, 24_400);
/// The codec side.
pub const CODEC: EventRange = EventRange::new(24_400, 24_500);
/// The display encoder side.
pub const ENCODER: EventRange = EventRange::new(24_800, 24_900);
/// The virtio-gpu display driver.
pub const VIRTIO_GPU: EventRange = EventRange::new(24_920, 24_930);
/// Link-local service discovery.
pub const DISCOVERYD: EventRange = EventRange::new(25_000, 26_000);
/// The userland runtime.
pub const RT: EventRange = EventRange::new(26_000, 27_000);
/// The xHCI host-controller driver.
pub const XHCI: EventRange = EventRange::new(27_000, 27_200);
/// The USB HID class driver.
pub const USB_HID: EventRange = EventRange::new(27_200, 27_300);
/// The USB mass-storage class driver.
pub const USB_MSD: EventRange = EventRange::new(27_300, 27_400);
/// The volume manager.
pub const VOLMGR: EventRange = EventRange::new(28_000, 28_100);
/// The RAID member driver.
pub const RAID_MEMBER: EventRange = EventRange::new(28_100, 28_200);
/// The RAID array composer.
pub const RAID: EventRange = EventRange::new(28_200, 28_300);
/// The audio device-channel driver side.
pub const AUDIOCHAN: EventRange = EventRange::new(29_000, 29_100);
/// The NIC device-channel driver side.
pub const NETCHAN: EventRange = EventRange::new(29_100, 29_200);
/// The audio mixer and router.
pub const AUDIOD: EventRange = EventRange::new(30_000, 31_000);
/// The Switchboard system-overview service.
pub const SWITCHBOARD: EventRange = EventRange::new(31_000, 32_000);
/// The cooling service.
pub const THERMALD: EventRange = EventRange::new(32_000, 33_000);
/// The network map's active reachability probing.
pub const NETMAP: EventRange = EventRange::new(33_000, 34_000);
/// The machine-settings service.
pub const SETTINGSD: EventRange = EventRange::new(34_000, 35_000);
/// The network-share service.
pub const SMBFS: EventRange = EventRange::new(35_000, 36_000);

/// Every range and the crates that take identifiers from it.
pub const OWNERS: &[Owner] = &[
    Owner {
        range: KERNEL_SEC,
        crates: &["kernel/sec"],
    },
    Owner {
        range: KERNEL_MEM,
        crates: &["kernel/mem", "lib/reclaim"],
    },
    Owner {
        range: KERNEL_IPC,
        crates: &["kernel/ipc"],
    },
    Owner {
        range: KERNEL,
        crates: &["kernel/core", "kernel/tairix-kernel", "kernel/arch/api"],
    },
    Owner {
        range: KERNEL_SYSCALL,
        crates: &["kernel/syscall"],
    },
    Owner {
        range: SANDBOX,
        crates: &["lib/sandbox"],
    },
    Owner {
        range: DRVHOST,
        crates: &["userland/system/drvhost"],
    },
    Owner {
        range: SYSINFOD,
        crates: &["userland/system/sysinfod"],
    },
    Owner {
        range: INIT,
        crates: &["userland/system/init"],
    },
    Owner {
        range: LOGIN,
        crates: &["userland/session/login"],
    },
    Owner {
        range: APPLOAD,
        crates: &["lib/appload"],
    },
    Owner {
        range: ARXFS,
        crates: &["drivers/filesystem/arxfs"],
    },
    Owner {
        range: DEVMGR,
        crates: &["userland/system/devmgr"],
    },
    Owner {
        range: SEATMGR,
        crates: &["userland/system/seatmgr"],
    },
    Owner {
        range: DISPLAY_SERVICE,
        crates: &["lib/display"],
    },
    Owner {
        range: NETSTACK,
        crates: &["userland/net/netstack"],
    },
    Owner {
        range: FONTD,
        crates: &["userland/system/fontd"],
    },
    Owner {
        range: CONSOLE_OUT,
        crates: &["lib/conout"],
    },
    Owner {
        range: GREETER,
        crates: &["userland/session/greeter"],
    },
    Owner {
        range: DESKTOP_SESSION,
        crates: &["userland/gui/session"],
    },
    Owner {
        range: CONFD,
        crates: &["userland/system/confd"],
    },
    Owner {
        range: SOUNDD,
        crates: &["userland/system/soundd"],
    },
    Owner {
        range: TIMED,
        crates: &["lib/timesync"],
    },
    Owner {
        range: DRIVER_RUNTIME,
        crates: &["lib/drvrt"],
    },
    Owner {
        range: I2C_BUS,
        crates: &["lib/i2cbus"],
    },
    Owner {
        range: DMA_ENGINE,
        crates: &["lib/dmaengine"],
    },
    Owner {
        range: CLOCK,
        crates: &["lib/clock"],
    },
    Owner {
        range: CODEC,
        crates: &["lib/codec"],
    },
    Owner {
        range: EventRange::new(24_500, 24_600),
        crates: &["drivers/bus/mailbox/vcmailbox"],
    },
    Owner {
        range: EventRange::new(24_600, 24_700),
        crates: &["drivers/irq/brcmstb_l2"],
    },
    Owner {
        range: EventRange::new(24_700, 24_800),
        crates: &["drivers/clock/rpi_firmware"],
    },
    Owner {
        range: ENCODER,
        crates: &["lib/encoder"],
    },
    Owner {
        range: EventRange::new(24_900, 24_910),
        crates: &["drivers/display/bcm2711_hvs"],
    },
    Owner {
        range: EventRange::new(24_910, 24_920),
        crates: &["drivers/display/bcm2711_hdmi"],
    },
    Owner {
        range: VIRTIO_GPU,
        crates: &["drivers/display/gpu_virtio"],
    },
    Owner {
        range: DISCOVERYD,
        crates: &["userland/net/discoveryd"],
    },
    Owner {
        range: RT,
        crates: &["lib/rt"],
    },
    Owner {
        range: XHCI,
        crates: &["drivers/bus/usb/xhci"],
    },
    Owner {
        range: USB_HID,
        crates: &["drivers/input/usb_hid"],
    },
    Owner {
        range: USB_MSD,
        crates: &["drivers/storage/usb_msd"],
    },
    Owner {
        range: VOLMGR,
        crates: &["drivers/storage/volmgr"],
    },
    Owner {
        range: RAID_MEMBER,
        crates: &["drivers/storage/raid_member"],
    },
    Owner {
        range: RAID,
        crates: &["drivers/storage/raid"],
    },
    Owner {
        range: AUDIOCHAN,
        crates: &["lib/audiochan"],
    },
    Owner {
        range: NETCHAN,
        crates: &["lib/netchan"],
    },
    Owner {
        range: AUDIOD,
        crates: &["userland/system/audiod"],
    },
    Owner {
        range: SWITCHBOARD,
        crates: &["userland/gui/switchboard"],
    },
    Owner {
        range: NETMAP,
        crates: &["userland/apps/netmap"],
    },
    Owner {
        range: SETTINGSD,
        crates: &["userland/system/settingsd"],
    },
    Owner {
        range: EventRange::new(24_930, 24_940),
        crates: &["drivers/gpio/bcm2835"],
    },
    Owner {
        range: EventRange::new(24_940, 24_950),
        crates: &["drivers/storage/emmc2"],
    },
    Owner {
        range: THERMALD,
        crates: &["userland/system/thermald"],
    },
    Owner {
        range: SMBFS,
        crates: &["userland/net/smbfs"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_ranges_overlap_and_no_crate_owns_two() {
        for (at, one) in OWNERS.iter().enumerate() {
            assert!(one.range.start() < one.range.end());
            for other in &OWNERS[at + 1..] {
                assert!(
                    !one.range.overlaps(other.range),
                    "{:?} overlaps {:?}",
                    one.crates,
                    other.crates
                );
                assert!(one.crates.iter().all(|c| !other.crates.contains(c)));
            }
        }
    }

    #[test]
    fn a_range_is_half_open() {
        let range = EventRange::new(10, 20);
        assert!(range.contains(EventId(10)));
        assert!(range.contains(EventId(19)));
        assert!(!range.contains(EventId(20)));
        assert!(!range.contains(EventId(9)));
        assert!(range.overlaps(EventRange::new(19, 30)));
        assert!(!range.overlaps(EventRange::new(20, 30)));
    }
}
