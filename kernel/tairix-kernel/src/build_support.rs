//! Pure build-support logic shared by the `tairix-kernel` build script.
//!
//! This file is pulled in by `build.rs` as a `#[path]` module (so the
//! build script has no dependency to pull in) and is *also* compiled
//! into the crate's host test build as a module, so the same rules are
//! unit tested rather than only exercised implicitly by a cross-compile
//! (tests are part of the change).
//!
//! It holds the target-selection logic: which instruction set and boot linker
//! script a build is for. The keys a build signs with are no part of the
//! source; they come from the build's secret key set (`tools/signkeys`).
//!
//! Every decision the build script makes about *which* instruction set
//! the production kernel is being built for, and *which* per-board boot
//! linker script to hand `rustc`, lives here as a pure function of the
//! Cargo-provided target strings. Keeping it target-string-driven — not
//! a target-conditional `cfg` predicate — is what lets the build script
//! (the build-glue carve-out) make the instruction-set
//! choice in one audited place instead of leaking that predicate into
//! the crate body, which `cargo xtask cfg-check` forbids outside the
//! architecture ports and the build glue.
//!
//! It also discovers the kernel's bootstrap floor: every driver crate whose
//! [`FLOOR_MANIFEST`] declares it part of the floor, read with the program
//! manifests' own grammar, and which of them each instruction set's floor
//! holds. No list of floor drivers is kept anywhere. That half reads files
//! and allocates, which both places this module compiles into — the build
//! script and the host test build — can.

/// The freestanding bare-metal target triples the production kernel
/// builds for, paired with the per-board boot linker script (a path
/// relative to `CARGO_MANIFEST_DIR`) the boot-stub carve-out pins for
/// that board.
///
/// The aarch64 production image targets the Raspberry Pi 4 (load address
/// `0x8_0000`, `aarch64-rpi4.ld`); the QEMU `virt` board's
/// `aarch64-virt.ld` is used only by the per-test bins under
/// `tests/integration/*`, which supply their own build scripts.
const FREESTANDING_TARGETS: &[(&str, &str)] = &[
    ("x86_64-tairix-none", "../arch/x86_64/linker.ld"),
    (
        "aarch64-unknown-none",
        "../arch/aarch64/link/aarch64-rpi4.ld",
    ),
    // The riscv64 production image targets the QEMU `virt` / SiFive
    // board (its only Tier-1 board), so it reuses the arch port's
    // `riscv64-virt.ld` (load `0x8020_0000` above the OpenSBI firmware
    // region) rather than a separate per-board script (there is no Pi-equivalent second riscv64 board yet).
    (
        "riscv64gc-unknown-none-elf",
        "../arch/riscv64/link/riscv64-virt.ld",
    ),
];

/// The boot linker script (relative to `CARGO_MANIFEST_DIR`) for a
/// freestanding target triple and board request, or `None` for any
/// other triple (the host build, which links no kernel image).
///
/// `board` is the `TAIRIX_KERNEL_BOARD` build-glue request: `None` (or
/// `Some("rpi4")`) selects each target's default board script;
/// `Some("virt")` selects the aarch64 QEMU `virt` script
/// (`aarch64-virt.ld`, load `0x4020_0000`) the interactive `cargo xtask
/// run` kernel links — the other targets have a single Tier-1 board, so
/// any non-default board request for them fails closed as `Err`.
///
/// # Errors
///
/// An unknown board name (or a board request the target has no script
/// for) is a build defect reported as [`UnknownBoard`], never a silent
/// default.
pub fn linker_script_for<'a>(
    target: &'a str,
    board: Option<&'a str>,
) -> Result<Option<&'static str>, UnknownBoard<'a>> {
    let default = FREESTANDING_TARGETS
        .iter()
        .find(|(triple, _)| *triple == target)
        .map(|(_, script)| *script);
    match board {
        None | Some("rpi4") => Ok(default),
        Some("virt") if target == "aarch64-unknown-none" => {
            Ok(Some("../arch/aarch64/link/aarch64-virt.ld"))
        }
        Some(other) => Err(UnknownBoard {
            board: other,
            target,
        }),
    }
}

/// A board request [`linker_script_for`] rejected: an unknown
/// `TAIRIX_KERNEL_BOARD` name, or a board the target has no boot script
/// for. Alloc-free so the shared source compiles in both the `no_std`
/// crate's host test build and the `std` build script (which formats
/// the loud build error from it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownBoard<'a> {
    /// The rejected board name.
    pub board: &'a str,
    /// The build's target triple.
    pub target: &'a str,
}

/// The `kernel_isa` conditional-compilation value for a target
/// instruction set, or `None` if the crate ships no production kernel
/// for it.
///
/// Emitted by the build script for *every* build (host included) so the
/// crate body can gate each architecture's modules on the chosen
/// instruction set without naming `target_arch` inline.
#[must_use]
pub fn kernel_isa(target_arch: &str) -> Option<&'static str> {
    tairix_buildcfg::NATIVE_ARCHES
        .into_iter()
        .find(|&isa| isa == target_arch)
}

/// True when the crate is being built as the bare-metal production
/// kernel: a supported instruction set with no host operating system.
#[must_use]
pub fn is_freestanding(target_os: &str, target_arch: &str) -> bool {
    tairix_buildcfg::native_arch_of(target_os, target_arch).is_some()
}

/// Parse a `SOURCE_DATE_EPOCH` value into whole seconds, or `None` when it is
/// absent/malformed so the build script falls back to the current wall-clock
/// second.
///
/// `SOURCE_DATE_EPOCH` is the standard reproducible-build input: when a pinned build sets it, the build provenance id's epoch is
/// this fixed value, so two reproducible builds stamp an identical id (and
/// hence a byte-identical image); otherwise the id carries the real build
/// second, the freshness signal an operator reads off the UART to confirm a
/// reflash actually changed. Surrounding whitespace is tolerated; a negative,
/// empty, or non-numeric value is rejected (`None`) rather than guessed
/// (fail closed to the wall-clock fallback). Alloc-free so
/// it compiles both into the `no_std` crate's host test build and into the
/// `std` build script.
#[must_use]
pub fn parse_source_date_epoch(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

/// A short, stable content fingerprint of arbitrary build-provenance bytes,
/// rendered as 12 lowercase hex digits.
///
/// The build provenance id ([`crate`]'s `build.rs`) appends this over a
/// working tree's uncommitted content so two *different* dirty trees no
/// longer collapse to the same `+dirty` marker — the recurring "is my change
/// actually in this image?" ambiguity, where a bare `<hash>+dirty` was
/// identical whether or not a given edit was present. A distinct fingerprint
/// proves a distinct tree; an identical one proves the same content.
///
/// This is a build-provenance fingerprint, not a security digest: the fast,
/// non-cryptographic shared hash is the right tool because it only has to
/// distinguish developer working trees, never resist an adversary — the
/// image's real integrity guarantee is the reproducible build + source-hash
/// pinning + signed SBOM, not this id. Rolling the 8 bytes out
/// as 16 hex and keeping the low 12 (the low 48 bits) gives a marker short
/// enough to read off a UART line while collision-free across any realistic
/// set of dev trees. Alloc-free — it renders into the caller's `out` buffer
/// and returns a borrow of it — so it compiles into the `no_std` crate's host
/// test build as well as the `std` build script (like
/// [`parse_source_date_epoch`]).
pub fn short_content_hash<'a>(bytes: &[u8], out: &'a mut [u8; 12]) -> &'a str {
    let hash = tairix_hash::FastHash::hash_bytes(0, bytes);
    // The low 48 bits as 12 lowercase hex nibbles, most-significant first —
    // identical to taking the last 12 chars of the full 16-hex rendering.
    for (i, slot) in out.iter_mut().enumerate() {
        let nibble = ((hash >> ((11 - i) * 4)) & 0xf) as u8;
        *slot = match nibble {
            0..=9 => b'0' + nibble,
            _ => b'a' + (nibble - 10),
        };
    }
    // Every byte written above is an ASCII hex digit, so the buffer is valid
    // UTF-8; fall back to the empty string rather than panicking if that
    // invariant were ever broken.
    core::str::from_utf8(out).unwrap_or("")
}

/// The program manifests' grammar and the walk of where crates live, shared
/// with the bundle composer so a floor manifest reads as an `AppInfo.toml`
/// does.
#[expect(
    dead_code,
    reason = "the bundle composer's own helpers it carries have no use here"
)]
#[path = "../../../tools/syshelp/src/bundles.rs"]
mod bundles;

use std::path::Path;
use std::string::{String, ToString};
use std::vec::Vec;

use tairix_abi::{DriverBindKey, HwMatchKey, DRIVER_MANIFEST_MAX_BIND_KEYS};

/// The file whose presence beside a driver crate's `Cargo.toml` makes the
/// driver part of the kernel's bootstrap floor.
pub const FLOOR_MANIFEST: &str = "Floor.toml";

/// How the kernel brings a floor driver's device up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloorKind {
    /// A virtio device, on the transport the port provisions.
    Virtio,
    /// A device the kernel lends the driver a host for.
    Host,
}

impl FloorKind {
    /// The arm of the catalogue's `floor_driver!` macro the kind is entered
    /// by.
    #[must_use]
    pub fn arm(self) -> &'static str {
        match self {
            Self::Virtio => "virtio",
            Self::Host => "host",
        }
    }
}

/// One floor driver's declaration, as its manifest states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloorDeclaration {
    /// How the kernel brings its device up.
    pub kind: FloorKind,
    /// The instruction sets whose floor holds it.
    pub isas: Vec<&'static str>,
    /// The bind table the build signs: its crate's `BIND_KEYS`, restated.
    pub bind_keys: Vec<DriverBindKey>,
    /// Why it sits below the driver store.
    pub reason: String,
}

/// A floor driver the walk found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloorDriver {
    /// Its crate's package name.
    pub package: String,
    /// What its manifest declares.
    pub declaration: FloorDeclaration,
}

impl FloorDriver {
    /// The name generated code names its crate by.
    #[must_use]
    pub fn crate_name(&self) -> String {
        self.package.replace('-', "_")
    }
}

/// Every floor driver under `drivers`, in the walk's order.
///
/// # Errors
///
/// An unreadable tree or file, a crate whose `Cargo.toml` names no package,
/// or a manifest [`parse_floor_manifest`] refuses, each naming its file: the
/// build fails rather than leave a declared driver out of the floor.
pub fn discover_floor_drivers(drivers: &Path) -> Result<Vec<FloorDriver>, String> {
    let crates = bundles::manifest_crates(drivers, FLOOR_MANIFEST, |_| {})
        .map_err(|e| std::format!("{}: {e}", drivers.display()))?;
    crates
        .iter()
        .map(|dir| {
            let read = |name: &str| {
                let path = dir.join(name);
                std::fs::read_to_string(&path)
                    .map_err(|e| std::format!("{}: {e}", path.display()))
                    .map(|text| (path, text))
            };
            let (manifest, text) = read(FLOOR_MANIFEST)?;
            let declaration = parse_floor_manifest(&text)
                .map_err(|e| std::format!("{}: {e}", manifest.display()))?;
            let (cargo, text) = read("Cargo.toml")?;
            let package = bundles::package_name(&text)
                .ok_or_else(|| std::format!("{}: no `[package] name`", cargo.display()))?;
            Ok(FloorDriver {
                package: package.to_string(),
                declaration,
            })
        })
        .collect()
}

/// The floor drivers of `floor` that `isa`'s floor holds.
#[must_use]
pub fn floor_for_isa<'a>(floor: &'a [FloorDriver], isa: &str) -> Vec<&'a FloorDriver> {
    floor
        .iter()
        .filter(|driver| driver.declaration.isas.contains(&isa))
        .collect()
}

/// Parse a floor manifest: `bring-up`, `isas`, `bind` and `reason`, each
/// once and each a plain quoted string, and nothing else.
///
/// # Errors
///
/// The first line the grammar refuses, by number, or the key it lacks.
pub fn parse_floor_manifest(text: &str) -> Result<FloorDeclaration, String> {
    let (mut kind, mut isas, mut bind_keys, mut reason) = (None, None, None, None);
    for (line, entry) in bundles::manifest_entries(text) {
        let at = |detail: String| std::format!("line {line}: {detail}");
        let (key, value) = entry.ok_or_else(|| at("not `key = \"value\"`".to_string()))?;
        let value = bundles::quoted_string(value)
            .ok_or_else(|| at(std::format!("`{key}` is not a plain quoted string")))?;
        let fresh = match key {
            "bring-up" => kind.replace(parse_kind(value).map_err(at)?).is_none(),
            "isas" => isas.replace(parse_isas(value).map_err(at)?).is_none(),
            "bind" => bind_keys
                .replace(parse_bind_keys(value).map_err(at)?)
                .is_none(),
            "reason" if value.trim().is_empty() => {
                return Err(at("an empty `reason`".to_string()));
            }
            "reason" => reason.replace(value.to_string()).is_none(),
            other => return Err(at(std::format!("unknown key `{other}`"))),
        };
        if !fresh {
            return Err(at(std::format!("`{key}` given twice")));
        }
    }
    let missing = |key: &str| std::format!("no `{key}`");
    Ok(FloorDeclaration {
        kind: kind.ok_or_else(|| missing("bring-up"))?,
        isas: isas.ok_or_else(|| missing("isas"))?,
        bind_keys: bind_keys.ok_or_else(|| missing("bind"))?,
        reason: reason.ok_or_else(|| missing("reason"))?,
    })
}

fn parse_kind(value: &str) -> Result<FloorKind, String> {
    match value {
        "virtio" => Ok(FloorKind::Virtio),
        "host" => Ok(FloorKind::Host),
        other => Err(std::format!("unknown bring-up `{other}`")),
    }
}

/// Whitespace-separated instruction set names, each a native Tier-1 one,
/// each once.
fn parse_isas(value: &str) -> Result<Vec<&'static str>, String> {
    let mut isas = Vec::new();
    for name in value.split_whitespace() {
        let isa = tairix_buildcfg::NATIVE_ARCHES
            .into_iter()
            .find(|&isa| isa == name)
            .ok_or_else(|| std::format!("unknown instruction set `{name}`"))?;
        if isas.contains(&isa) {
            return Err(std::format!("`{name}` named twice"));
        }
        isas.push(isa);
    }
    if isas.is_empty() {
        return Err("no instruction set".to_string());
    }
    Ok(isas)
}

/// Whitespace-separated `<kind>:<identity>@<priority>` entries, at least one
/// and no more than a manifest carries.
fn parse_bind_keys(value: &str) -> Result<Vec<DriverBindKey>, String> {
    let keys = value
        .split_whitespace()
        .map(parse_bind_key)
        .collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() || keys.len() > usize::from(DRIVER_MANIFEST_MAX_BIND_KEYS) {
        return Err(std::format!(
            "a bind table holds 1..={DRIVER_MANIFEST_MAX_BIND_KEYS} keys, not {}",
            keys.len()
        ));
    }
    Ok(keys)
}

/// One bind entry: `compatible:<string>`, `virtio:<device id>`, or
/// `pci:`/`usb:` with a vendor, a device and a class in hexadecimal, each
/// `@` the decimal priority it binds at.
fn parse_bind_key(entry: &str) -> Result<DriverBindKey, String> {
    let refused = || std::format!("bind entry `{entry}` is not `<kind>:<identity>@<priority>`");
    let (key, priority) = entry.rsplit_once('@').ok_or_else(refused)?;
    let priority: u16 = priority.parse().map_err(|_| refused())?;
    let (kind, identity) = key.split_once(':').ok_or_else(refused)?;
    let key = match kind {
        "compatible" if !identity.is_empty() => {
            HwMatchKey::compatible(identity.as_bytes()).map_err(|_| refused())?
        }
        "virtio" => HwMatchKey::virtio(identity.parse().map_err(|_| refused())?),
        "pci" | "usb" => {
            let mut parts = identity.split(':');
            let (Some(vendor), Some(device), Some(class), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(refused());
            };
            let vendor = u16::from_str_radix(vendor, 16).map_err(|_| refused())?;
            let device = u16::from_str_radix(device, 16).map_err(|_| refused())?;
            let class = u32::from_str_radix(class, 16)
                .ok()
                .filter(|&class| class <= 0x00ff_ffff)
                .ok_or_else(refused)?;
            if kind == "pci" {
                HwMatchKey::pci(vendor, device, class)
            } else {
                HwMatchKey::usb(vendor, device, class)
            }
        }
        _ => return Err(refused()),
    };
    Ok(DriverBindKey::new(priority, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest stating every key once.
    const MANIFEST: &str = "# a floor driver\n\
                            bring-up = \"host\"\n\
                            isas = \"aarch64 riscv64\"\n\
                            bind = \"compatible:vendor,part@10 virtio:2@5\"\n\
                            reason = \"it reads the root\"\n";

    #[test]
    fn a_floor_manifest_states_its_bring_up_isas_bind_table_and_reason() {
        let declaration = parse_floor_manifest(MANIFEST).expect("parses");
        assert_eq!(declaration.kind, FloorKind::Host);
        assert_eq!(declaration.isas, ["aarch64", "riscv64"]);
        assert_eq!(
            declaration.bind_keys,
            [
                DriverBindKey::new(10, HwMatchKey::compatible(b"vendor,part").expect("fits")),
                DriverBindKey::new(5, HwMatchKey::virtio(2)),
            ]
        );
        assert_eq!(declaration.reason, "it reads the root");
    }

    #[test]
    fn a_floor_manifest_missing_a_key_or_stating_one_twice_or_another_is_refused() {
        let without = |key: &str| {
            MANIFEST
                .lines()
                .filter(|line| !line.starts_with(key))
                .collect::<Vec<_>>()
                .join("\n")
        };
        for key in ["bring-up", "isas", "bind", "reason"] {
            assert!(parse_floor_manifest(&without(key)).is_err(), "{key}");
        }
        for extra in [
            "reason = \"again\"",
            "path = \"/System/Drivers/storage/part\"",
            "[floor]",
            "isas = aarch64",
        ] {
            let text = std::format!("{MANIFEST}{extra}\n");
            assert!(parse_floor_manifest(&text).is_err(), "{extra}");
        }
    }

    #[test]
    fn a_floor_manifest_refuses_each_value_it_cannot_mean() {
        for (key, value) in [
            ("bring-up", "pci"),
            ("isas", ""),
            ("isas", "wasm32"),
            ("isas", "aarch64 aarch64"),
            ("bind", ""),
            ("bind", "compatible:@10"),
            ("bind", "virtio:2"),
            ("bind", "virtio:two@10"),
            ("bind", "pci:8086:2922@10"),
            ("bind", "usb:3434:0e21:1000000@10"),
            ("bind", "serial:0@10"),
            ("reason", " "),
        ] {
            let text = MANIFEST
                .lines()
                .map(|line| {
                    if line.starts_with(key) {
                        std::format!("{key} = \"{value}\"")
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(parse_floor_manifest(&text).is_err(), "{key} = {value:?}");
        }
    }

    #[test]
    fn a_bind_table_names_each_kind_of_key() {
        assert_eq!(
            parse_bind_keys("pci:8086:2922:010601@3 usb:3434:0e21:030101@4"),
            Ok(std::vec![
                DriverBindKey::new(3, HwMatchKey::pci(0x8086, 0x2922, 0x01_06_01)),
                DriverBindKey::new(4, HwMatchKey::usb(0x3434, 0x0e21, 0x03_01_01)),
            ])
        );
        let crowded = std::vec!["virtio:2@1"; usize::from(DRIVER_MANIFEST_MAX_BIND_KEYS) + 1];
        assert!(parse_bind_keys(&crowded.join(" ")).is_err());
    }

    #[test]
    fn each_instruction_set_holds_the_floor_drivers_that_name_it() {
        let declared = |isas: &[&'static str]| FloorDriver {
            package: "tairix-drv-x".to_string(),
            declaration: FloorDeclaration {
                isas: isas.to_vec(),
                ..parse_floor_manifest(MANIFEST).expect("parses")
            },
        };
        let floor = [declared(&["aarch64"]), declared(&["aarch64", "x86_64"])];
        assert_eq!(floor_for_isa(&floor, "aarch64").len(), 2);
        assert_eq!(floor_for_isa(&floor, "x86_64"), [&floor[1]]);
        assert!(floor_for_isa(&floor, "riscv64").is_empty());
        assert_eq!(floor[0].crate_name(), "tairix_drv_x");
        assert_eq!(
            (FloorKind::Virtio.arm(), FloorKind::Host.arm()),
            ("virtio", "host")
        );
    }

    /// The tree's own manifests: every target boots from a virtio disk, the
    /// aarch64 one from a host-attached disk too, and no target's floor binds
    /// one key twice, which would leave the disk to a tie.
    #[test]
    fn every_target_boots_from_the_floor_its_drivers_declare() {
        let drivers = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../drivers");
        let floor = discover_floor_drivers(&drivers).expect("every floor manifest parses");
        for isa in tairix_buildcfg::NATIVE_ARCHES {
            let held = floor_for_isa(&floor, isa);
            assert!(
                held.iter()
                    .any(|driver| driver.declaration.kind == FloorKind::Virtio),
                "{isa} boots from a virtio disk"
            );
            let keys: Vec<_> = held
                .iter()
                .flat_map(|driver| driver.declaration.bind_keys.iter().map(|bind| bind.key))
                .collect();
            for (at, key) in keys.iter().enumerate() {
                assert!(!keys[at + 1..].contains(key), "{isa} binds {key:?} twice");
            }
        }
        assert!(floor_for_isa(&floor, "aarch64")
            .iter()
            .any(|driver| driver.declaration.kind == FloorKind::Host));
    }

    #[test]
    fn x86_64_freestanding_selects_the_x86_64_linker_script() {
        assert_eq!(
            linker_script_for("x86_64-tairix-none", None),
            Ok(Some("../arch/x86_64/linker.ld"))
        );
        assert!(is_freestanding("none", "x86_64"));
        assert_eq!(kernel_isa("x86_64"), Some("x86_64"));
    }

    #[test]
    fn aarch64_freestanding_selects_the_rpi4_linker_script() {
        assert_eq!(
            linker_script_for("aarch64-unknown-none", None),
            Ok(Some("../arch/aarch64/link/aarch64-rpi4.ld"))
        );
        assert!(is_freestanding("none", "aarch64"));
        assert_eq!(kernel_isa("aarch64"), Some("aarch64"));
    }

    #[test]
    fn aarch64_virt_board_selects_the_virt_linker_script() {
        // The interactive `cargo xtask run` form: same production crate,
        // QEMU `virt` load layout.
        assert_eq!(
            linker_script_for("aarch64-unknown-none", Some("virt")),
            Ok(Some("../arch/aarch64/link/aarch64-virt.ld"))
        );
        // The explicit default board spelling matches the default.
        assert_eq!(
            linker_script_for("aarch64-unknown-none", Some("rpi4")),
            Ok(Some("../arch/aarch64/link/aarch64-rpi4.ld"))
        );
    }

    #[test]
    fn unknown_or_mistargeted_boards_fail_closed() {
        // A typo'd board, or `virt` on a target with a single board, is a
        // build defect reported loudly — never a silently defaulted script.
        assert!(linker_script_for("aarch64-unknown-none", Some("virt2")).is_err());
        assert!(linker_script_for("x86_64-tairix-none", Some("virt")).is_err());
        assert!(linker_script_for("riscv64gc-unknown-none-elf", Some("virt")).is_err());
    }

    #[test]
    fn host_targets_link_no_kernel_script_but_still_pick_an_isa() {
        // A host build (an OS present) is never freestanding, links no
        // kernel image, yet still reports its instruction set so the
        // crate's host test build compiles the right architecture's
        // modules.
        assert_eq!(
            linker_script_for("x86_64-unknown-linux-gnu", None),
            Ok(None)
        );
        assert!(!is_freestanding("linux", "x86_64"));
        assert_eq!(kernel_isa("x86_64"), Some("x86_64"));
    }

    #[test]
    fn riscv64_freestanding_selects_the_virt_linker_script() {
        assert_eq!(
            linker_script_for("riscv64gc-unknown-none-elf", None),
            Ok(Some("../arch/riscv64/link/riscv64-virt.ld"))
        );
        assert!(is_freestanding("none", "riscv64"));
        assert_eq!(kernel_isa("riscv64"), Some("riscv64"));
    }

    #[test]
    fn unsupported_instruction_sets_have_no_kernel() {
        assert_eq!(kernel_isa("wasm32"), None);
        assert!(!is_freestanding("none", "wasm32"));
        assert_eq!(linker_script_for("wasm32-unknown-unknown", None), Ok(None));
    }

    #[test]
    fn source_date_epoch_is_honoured_when_well_formed_and_rejected_otherwise() {
        // A pinned reproducible build: the exact second is
        // used, surrounding whitespace tolerated.
        assert_eq!(parse_source_date_epoch("1782181959"), Some(1_782_181_959));
        assert_eq!(
            parse_source_date_epoch("  1782181959\n"),
            Some(1_782_181_959)
        );
        assert_eq!(parse_source_date_epoch("0"), Some(0));
        // Malformed / empty / negative values fall back (None) rather than
        // being guessed, so the id carries the real wall-clock second instead
        // of a wrong fixed one (fail closed).
        assert_eq!(parse_source_date_epoch(""), None);
        assert_eq!(parse_source_date_epoch("not-a-number"), None);
        assert_eq!(parse_source_date_epoch("-1"), None);
        assert_eq!(parse_source_date_epoch("12.5"), None);
    }

    #[test]
    fn short_content_hash_is_stable_distinct_and_well_formed() {
        let mut a = [0u8; 12];
        let mut b = [0u8; 12];
        // Deterministic: the same bytes always fingerprint identically, so a
        // rebuild of the same dirty tree reports the same id.
        assert_eq!(
            short_content_hash(b"the same tree", &mut a),
            short_content_hash(b"the same tree", &mut b)
        );
        // Distinct trees fingerprint differently — the whole point: a metal
        // reflash of a *changed* tree reports a different `+dirty.<fp>`.
        assert_ne!(
            short_content_hash(b"tree A", &mut a),
            short_content_hash(b"tree B", &mut b)
        );
        // A single-byte change moves the fingerprint (avalanche), so a tiny
        // edit under test is never conflated with the tree it replaced.
        assert_ne!(
            short_content_hash(b"switch_return crumb present", &mut a),
            short_content_hash(b"switch_return crumb present.", &mut b)
        );
        // Always exactly 12 lowercase hex digits — short enough to read off a
        // UART line, fixed-width so two ids compare by eye.
        for sample in [&b""[..], b"x", b"a longer body of build material"] {
            let mut buf = [0u8; 12];
            let fp = short_content_hash(sample, &mut buf);
            assert_eq!(fp.len(), 12);
            assert!(fp
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        }
        // The empty input's low 12 hex digits pin the exact bytes so a
        // refactor cannot silently shift the id.
        let mut buf = [0u8; 12];
        assert_eq!(short_content_hash(b"", &mut buf), "db3751d8e999");
    }
}
