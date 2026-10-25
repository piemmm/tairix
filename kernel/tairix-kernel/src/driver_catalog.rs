//! The kernel's bootstrap floor: the drivers it hosts itself, because the
//! storage they read holds the signed driver store every other driver is
//! loaded from (PLAN Stage 4.HW item 5; `plans/PI.md` P10 5c).
//!
//! Each floor driver declares itself in a `Floor.toml` beside its crate's
//! `Cargo.toml` — how the kernel brings its device up, the instruction sets
//! whose floor holds it, the bind table the build signs, and why it sits
//! below the store — and `build.rs` discovers the declarations and generates
//! this target's table, so the floor is listed nowhere by hand. An entry
//! pairs the driver's logical `/System/Drivers/` path, its crate's own
//! `BIND_KEYS`, the build-signed manifest image the load gate verifies, its
//! statically linked `register()` and its bring-up, so the views of one
//! driver cannot drift apart.
//!
//! Matching is the shared [`tairix_devmatch`] policy the user-space `devmgr`
//! autoloader also resolves: the kernel binds a driver because its bind
//! table matched a discovered node, and leaves an unmatched node unbound.
//! The floor binds by the same discovery-match as the store's drivers, holds
//! only the storage a target boots from, and shrinks toward the store, never
//! grows.

use alloc::boxed::Box;

use tairix_abi::driver::block::Block;
use tairix_abi::driver::timing::CompletionWait;
use tairix_abi::{DriverBindKey, DriverError, DriverHost, HwMatchKey, HwNode};
use tairix_devmatch::{resolve, DriverCandidate, MatchResolution};
use tairix_drvhost::DriverEntry;
use tairix_kernel_mem::window_slots;
use tairix_log::Sink;

/// Whether `path` is a class directory and one leaf under the driver store:
/// a vendor names only the leaf holding its part's driver, never the
/// namespace above it.
const fn is_class_and_leaf(path: &str) -> bool {
    const STORE: &[u8] = b"/System/Drivers/";
    let bytes = path.as_bytes();
    if bytes.len() <= STORE.len() {
        return false;
    }
    let mut at = 0;
    while at < STORE.len() {
        if bytes[at] != STORE[at] {
            return false;
        }
        at += 1;
    }
    let (mut separators, mut segment) = (0, 0);
    while at < bytes.len() {
        if bytes[at] == b'/' {
            if segment == 0 {
                return false;
            }
            separators += 1;
            segment = 0;
        } else {
            segment += 1;
        }
        at += 1;
    }
    separators == 1 && segment != 0
}

/// The DMA pool pages `carves` take, each a window of its own, or [`None`]
/// for a carve no pool grants — which the floor table refuses in a `const`
/// block, so it fails the build rather than the boot.
#[must_use]
pub const fn pool_pages(carves: &[usize]) -> Option<usize> {
    let mut pages: usize = 0;
    let mut at = 0;
    while at < carves.len() {
        let Some(slots) = window_slots(carves[at]) else {
            return None;
        };
        let Some(sum) = pages.checked_add(slots) else {
            return None;
        };
        pages = sum;
        at += 1;
    }
    Some(pages)
}

/// How the kernel opens a host-attached floor device: the driver's own
/// `open`, handed the host the kernel lends it, the device's node, its
/// completion wait and the audit log the driver records its outcome on.
pub type FloorOpen = fn(
    &dyn DriverHost,
    &HwNode,
    &'static dyn CompletionWait,
    &dyn Sink,
) -> Result<Box<dyn Block>, DriverError>;

/// A floor driver the kernel lends a host to.
#[derive(Clone, Copy)]
pub struct HostFloor {
    /// The driver's bring-up.
    pub open: FloorOpen,
    /// The DMA pool pages its carves take.
    pub dma_pages: usize,
}

/// How the kernel brings a floor driver's device up.
#[derive(Clone, Copy)]
pub enum FloorBringUp {
    /// The port provisions the device's virtio transport, and the kernel
    /// opens it as a virtio block device.
    Virtio,
    /// The kernel lends the driver a host over the device's own resources.
    Host(HostFloor),
}

/// One bootstrap-floor driver: every view of it the kernel needs, in a
/// single entry so they cannot drift.
pub struct InKernelDriver {
    /// Logical `/System/Drivers/` image path — the driver's stable id.
    pub path: &'static str,
    /// The driver crate's published bind table, borrowed `'static` from the
    /// crate so the matcher never re-types it.
    pub bind_keys: &'static [DriverBindKey],
    /// The build-signed `.rxe` manifest image the load gate verifies.
    pub image: &'static [u8],
    /// The statically-linked `register()` entry the load gate invokes once
    /// the manifest is admitted.
    pub register: DriverEntry,
    /// How the kernel brings the driver's device up. Every floor driver is
    /// a block driver the root volume can be read through.
    pub bring_up: FloorBringUp,
}

/// Whether `signed` is `keys` on the wire, compared in a `const` block by
/// the floor table so a manifest signing any other bind table than its
/// crate's fails the build.
const fn is_bind_table(keys: &[DriverBindKey], signed: &[u8]) -> bool {
    if signed.len() != keys.len() * DriverBindKey::WIRE_LEN {
        return false;
    }
    let mut key = 0;
    while key < keys.len() {
        let wire = keys[key].to_le_bytes();
        let mut byte = 0;
        while byte < wire.len() {
            if wire[byte] != signed[key * DriverBindKey::WIRE_LEN + byte] {
                return false;
            }
            byte += 1;
        }
        key += 1;
    }
    true
}

/// One floor table entry for the crate `$krate`, from its own
/// `FLOOR_PATH`, `BIND_KEYS` and `register`, and for a host-opened driver its
/// `floor::open` and `floor::DMA_CARVES`; `$image` is its signed manifest and
/// `$bind` the bind table that manifest is signed with.
macro_rules! floor_driver {
    (virtio $krate:ident, $image:expr, $bind:expr) => {
        floor_driver!(@entry $krate, $image, $bind, FloorBringUp::Virtio)
    };
    (host $krate:ident, $image:expr, $bind:expr) => {
        floor_driver!(
            @entry $krate,
            $image,
            $bind,
            FloorBringUp::Host(HostFloor {
                open: $krate::floor::open,
                dma_pages: const {
                    match pool_pages($krate::floor::DMA_CARVES) {
                        Some(pages) => pages,
                        None => panic!("a floor driver's DMA carves fit a pool"),
                    }
                },
            })
        )
    };
    (@entry $krate:ident, $image:expr, $bind:expr, $bring_up:expr) => {
        InKernelDriver {
            path: const {
                assert!(
                    is_class_and_leaf($krate::FLOOR_PATH),
                    "a floor driver's path is a class and a leaf"
                );
                $krate::FLOOR_PATH
            },
            bind_keys: const {
                assert!(
                    is_bind_table($krate::BIND_KEYS, $bind),
                    "a floor driver's manifest signs its crate's bind table"
                );
                $krate::BIND_KEYS
            },
            image: $image,
            register: $krate::register,
            bring_up: $bring_up,
        }
    };
}

// `KERNEL_DRIVER_SIGNER_PUBKEY`, `IN_KERNEL_DRIVER_COUNT` and
// `IN_KERNEL_DRIVERS`, which `build.rs` generates from the floor's manifests.
include!(concat!(env!("OUT_DIR"), "/floor_drivers.rs"));

/// The registry entry whose `path` is `path`, or [`None`] if no in-kernel
/// driver owns it (the caller leaves an unknown node unbound rather than
/// guessing).
#[must_use]
pub fn driver_for(path: &str) -> Option<&'static InKernelDriver> {
    IN_KERNEL_DRIVERS.iter().find(|driver| driver.path == path)
}

/// The registry as match candidates (`path` + `bind_keys`), index-aligned
/// with [`IN_KERNEL_DRIVERS`] so a [`MatchResolution::Winner`] index names
/// the same driver in both. Derived from the one registry, never re-typed.
#[must_use]
pub fn driver_candidates() -> [DriverCandidate<'static>; IN_KERNEL_DRIVER_COUNT] {
    core::array::from_fn(|i| DriverCandidate {
        path: IN_KERNEL_DRIVERS[i].path,
        bind_keys: IN_KERNEL_DRIVERS[i].bind_keys,
    })
}

/// Resolve a discovered node's `match_keys` against the registry.
///
/// Returns the deterministic [`MatchResolution`]: the
/// strictly highest-priority matching driver wins, an unbroken tie across
/// distinct drivers is a packaging defect, and a node matching nothing is
/// [`MatchResolution::Unmatched`] — left unbound by the caller.
#[must_use]
pub fn resolve_driver(node_keys: &[HwMatchKey]) -> MatchResolution {
    resolve(node_keys, &driver_candidates())
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec::Vec;

    use tairix_abi::{DriverImage, DriverKind};

    #[test]
    fn every_entry_binds_a_node_carrying_its_own_key() {
        for (index, driver) in IN_KERNEL_DRIVERS.iter().enumerate() {
            for bind in driver.bind_keys {
                assert_eq!(
                    resolve_driver(&[bind.key]),
                    MatchResolution::Winner {
                        candidate: index,
                        priority: bind.priority,
                    },
                    "{}",
                    driver.path
                );
            }
        }
    }

    /// The build signs the bind table a driver's manifest restates; matching
    /// takes its crate's, so the two must be one table.
    #[test]
    fn every_entrys_signed_bind_table_is_its_crates() {
        for driver in &IN_KERNEL_DRIVERS {
            let image = DriverImage::parse(driver.image).expect("a manifest image");
            let crates: Vec<u8> = driver
                .bind_keys
                .iter()
                .flat_map(DriverBindKey::to_le_bytes)
                .collect();
            assert_eq!(image.bind_table, crates.as_slice(), "{}", driver.path);
            assert_eq!(image.manifest.kind, DriverKind::InKernel, "{}", driver.path);
            assert!(image.payload.is_empty(), "{}", driver.path);
        }
    }

    #[test]
    fn a_bind_table_is_its_keys_on_the_wire_and_nothing_else() {
        let keys = [
            DriverBindKey::new(10, HwMatchKey::virtio(2)),
            DriverBindKey::new(5, HwMatchKey::pci(0x1af4, 0x1042, 0x01_00_00)),
        ];
        let wire: Vec<u8> = keys.iter().flat_map(DriverBindKey::to_le_bytes).collect();
        assert!(is_bind_table(&keys, &wire));
        assert!(is_bind_table(&[], &[]));
        assert!(!is_bind_table(&keys[..1], &wire), "a key the crate lacks");
        let one = &wire[..DriverBindKey::WIRE_LEN];
        assert!(!is_bind_table(&keys, one), "a key the manifest lacks");
        let mut other = wire.clone();
        other[0] ^= 1;
        assert!(!is_bind_table(&keys, &other), "another priority");
    }

    #[test]
    fn an_unrelated_device_is_left_unmatched() {
        // An AHCI storage controller matches no in-kernel driver: the caller
        // leaves it unbound and logged, never a guess.
        let ahci = [HwMatchKey::pci(0x8086, 0x2922, 0x01_06_01)];
        assert_eq!(resolve_driver(&ahci), MatchResolution::Unmatched);
        // A different-protocol HID device (a non-boot interface) also fails
        // the boot-class wildcard and is unmatched.
        let non_boot_hid = [HwMatchKey::usb(0x3434, 0x0E21, 0x03_00_00)];
        assert_eq!(resolve_driver(&non_boot_hid), MatchResolution::Unmatched);
    }

    #[test]
    fn driver_for_resolves_exactly_the_registry_paths() {
        for driver in &IN_KERNEL_DRIVERS {
            let found = driver_for(driver.path).expect("a registered path resolves");
            assert_eq!(found.path, driver.path);
        }
        assert!(driver_for("/System/Drivers/not_in_kernel").is_none());
    }

    #[test]
    fn a_floor_path_is_a_class_and_a_leaf_and_a_vendor_namespace_is_refused() {
        for driver in &IN_KERNEL_DRIVERS {
            assert!(is_class_and_leaf(driver.path), "{}", driver.path);
        }
        for refused in [
            "/System/Drivers/storage/vendor/part",
            "/System/Drivers/storage",
            "/System/Drivers/storage/",
            "/System/Drivers//part",
            "/System/Drivers/",
            "/System/Applications/storage/part",
        ] {
            assert!(!is_class_and_leaf(refused), "{refused}");
        }
    }

    #[test]
    fn a_host_floor_drivers_pool_holds_each_carve_in_a_window_of_its_own() {
        let page = tairix_kernel_mem::PAGE_SIZE;
        assert_eq!(pool_pages(&[]), Some(0));
        assert_eq!(
            pool_pages(&[1, page, page + 1]),
            [1, page, page + 1]
                .into_iter()
                .map(window_slots)
                .sum::<Option<usize>>()
        );
        assert_eq!(pool_pages(&[page, 0]), None, "a carve no pool grants");
    }
}
