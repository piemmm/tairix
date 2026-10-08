//! Build-time target classification shared by the freestanding QEMU
//! integration binaries.
//!
//! The integration binaries under `tests/integration/` compile two ways:
//! as freestanding `no_std`/`no_main` kernels for a bare-metal QEMU
//! target, and as inert host stubs for `cargo build --workspace`. They
//! must choose between those forms without naming the target instruction
//! set in their own source, which confines to the architecture
//! ports and the build glue.
//!
//! This crate is that build glue. Each binary's build script calls
//! [`emit_target_cfg`], which inspects the cargo-provided target
//! description and enables the matching conditional-compilation names:
//!
//! * `freestanding` — the crate is being built for a bare-metal
//!   (`os = "none"`) target and should compile its kernel body.
//! * `itest_x86_64` — freestanding on the 64-bit x86 port.
//! * `itest_riscv64` — freestanding on the 64-bit RISC-V port.
//! * `itest_aarch64` — freestanding on the 64-bit Arm port.
//! * `itest_wasm32` — the browser-sandbox wasm32 port
//!   (`wasm32-unknown-unknown`, `os = "unknown"`). Unlike the bare-metal
//!   ports this is a `cdylib`, not a `no_main` kernel, so it gets its
//!   own cfg *without* `freestanding`.
//!
//! Binaries gate on those names instead of a raw target predicate, so the
//! instruction-set choice lives in this one audited place.
//!
//! It also hosts the build-time [`elf2rxe`] converter, which turns a linked
//! PIE program ELF into the `rxe` load image the kernel spawn path consumes
//! (used by the CCOMPAT CC3 spawn round-trips).

pub mod elf2rxe;

pub use tairix_qemu::{Board, DmaTranslation, InterruptControllers};

/// The freestanding cross-compile target vocabulary (`PieArch`) and how
/// cargo is told to build for one: each Tier-1 target's name, its `--target`
/// value, and its `CARGO_TARGET_<name>_RUSTFLAGS` variable, shared by every
/// freestanding build so the selection cannot drift between them.
pub mod pie;

/// Dep-info-driven `cargo:rerun-if-changed` emission for build scripts that
/// run an inner `cargo build` and embed its output: freshness is derived
/// from the compiler's own dep-info record, never a hand-kept source list
/// that rots and ships a stale embedded binary.
pub mod dep_info;

/// The generated-source glue every freestanding fixture program's build
/// script shares: the one `program.ld` they link with, and the one emitter
/// of the `PROGRAM_RXE` + `USER_BIAS` source they `include!`.
pub mod program_fixture;

/// The M1 demand-paged file-mapping fixture: the single definition of the
/// fixture file the `file_map_qemu_*` verticals serve kernel-side and probe
/// from EL0 (geometry constants, content generator, `TAIRIX_FM_*` env
/// pinning, and the kernel-side constants emitter).
pub mod filemap_fixture;

/// The signed `.rxe` driver-bundle composer, shared by the build scripts
/// that lay a kernel-trusted driver into the system.
/// Enabled by the `driver-image` feature so the Ed25519 dependency is
/// pulled in only where a bundle is actually signed.
#[cfg(feature = "driver-image")]
pub mod driver_image;

/// The signed application-bundle composer and `AppInfo.toml` manifest
/// discovery, shared by every build that plants a program bundle onto the
/// read-only `/System` store (`plans/APPS.md` deliverable 8).
/// Enabled by the `app-image` feature so the signing/hashing dependencies
/// are pulled in only where a bundle is actually composed.
#[cfg(feature = "app-image")]
pub mod app_image;

/// Virtual base the production aarch64 spawn producer maps every spawned
/// user image at — the `SHELL_USER_BIAS` (64 GiB) the kernel's `build.rs`
/// bakes into `spawn_layout`.
///
/// A `.rxe` the kernel spawn path will load must have its `R_*_RELATIVE`
/// relocations baked for this exact bias ([`elf2rxe::elf_to_rxe`]'s
/// `load_bias`), so the converted image runs correctly once mapped at
/// `vaddr + USER_IMAGE_BIAS`. It is the one definition every build script
/// that bakes a spawnable `rxe` shares; the kernel spawn
/// path asserts the baked bias matches `SHELL_USER_BIAS` and fails closed on
/// a mismatch, so a drift between this constant and the
/// kernel is caught rather than miscompiled.
pub const USER_IMAGE_BIAS: u64 = 0x10_0000_0000;

/// Cargo environment key naming the target operating system.
const TARGET_OS_KEY: &str = "CARGO_CFG_TARGET_OS";
/// Cargo environment key naming the target instruction set.
const TARGET_ARCH_KEY: &str = "CARGO_CFG_TARGET_ARCH";

/// Every conditional-compilation name this crate may enable. Declared to
/// the compiler unconditionally so `--cfg`-aware lints accept the gates
/// even on host builds where none of them are active.
pub const KNOWN_CFGS: &[&str] = &[
    "freestanding",
    "itest_x86_64",
    "itest_riscv64",
    "itest_aarch64",
    "itest_wasm32",
];

/// Classify a target into the conditional-compilation names its
/// freestanding integration binary should enable.
///
/// Bare-metal targets (`os == "none"`) are freestanding; the matching
/// per-port name is added when the instruction set is one the QEMU
/// verticals cover. Hosted targets enable nothing, leaving the binary as
/// an inert stub.
#[must_use]
pub fn active_cfgs(os: &str, arch: &str) -> Vec<&'static str> {
    // The wasm32 browser target (`wasm32-unknown-unknown`, `os =
    // "unknown"`) is a `cdylib` the host loads, not a bare-metal
    // `no_main` kernel, so it enables its own cfg without
    // `freestanding`.
    if os == "unknown" && arch == "wasm32" {
        return vec!["itest_wasm32"];
    }
    if os != "none" {
        return Vec::new();
    }
    let mut cfgs = vec!["freestanding"];
    match arch {
        "x86_64" => cfgs.push("itest_x86_64"),
        "riscv64" => cfgs.push("itest_riscv64"),
        "aarch64" => cfgs.push("itest_aarch64"),
        _ => {}
    }
    cfgs
}

/// Emit the conditional-compilation flags for the current build.
///
/// Call this from a binary's build script. It declares every
/// [`KNOWN_CFGS`] name to the compiler and enables those returned by
/// [`active_cfgs`] for the target cargo is building.
pub fn emit_target_cfg() {
    for name in KNOWN_CFGS {
        println!("cargo:rustc-check-cfg=cfg({name})");
    }
    let os = std::env::var(TARGET_OS_KEY).unwrap_or_default();
    let arch = std::env::var(TARGET_ARCH_KEY).unwrap_or_default();
    for name in active_cfgs(&os, &arch) {
        println!("cargo:rustc-cfg={name}");
    }
}

/// Guest RAM the dumped tree describes when a caller does not say, in
/// mebibytes — `tools/qemu`'s own aarch64 default, so the tree's `/memory`
/// window and the RAM the runner actually gives the guest agree by
/// construction. A vertical that overrides one must override both.
pub const DEFAULT_DTB_RAM_MIB: u32 = tairix_qemu::aarch64::DEFAULT_RAM_MIB;

/// Build the `qemu-system-aarch64` argument vector that dumps, to
/// `dtb_path`, the device tree of the `virt` `board` a run boots, for `cpus`
/// CPUs and `ram_mib` mebibytes of guest RAM.
///
/// The machine, its `-global`s, the unit a run creates as a device and the
/// CPU are `tools/qemu`'s own, so the tree describes the board the runner
/// starts. The DTB layout the verticals
/// read from the blob (virtio-MMIO transport bases, GICv2 SPIs, the `/psci`
/// conduit) is the stable `virt`-board layout, independent of the memory size;
/// its `/cpus` names `cpus` CPUs, which are the only ones the boot starts.
#[must_use]
pub fn dump_virt_dtb_args(dtb_path: &str, cpus: u32, ram_mib: u32, board: Board) -> Vec<String> {
    let (machine, globals) = tairix_qemu::aarch64::machine(board);
    let mut args = vec!["-M".to_string(), format!("{machine},dumpdtb={dtb_path}")];
    for global in globals {
        args.extend(["-global".to_string(), (*global).to_string()]);
    }
    if let Some(unit) = board.translation.unit_device() {
        args.extend(["-device".to_string(), unit.to_string()]);
    }
    args.extend([
        "-cpu".to_string(),
        tairix_qemu::aarch64::CPU.to_string(),
        "-m".to_string(),
        format!("{ram_mib}M"),
        "-smp".to_string(),
        cpus.to_string(),
        "-display".to_string(),
        "none".to_string(),
        "-no-reboot".to_string(),
    ]);
    args
}

/// The whole build script of an x86_64 vertical that links the port alone:
/// hand the kernel's shared linker script to `rustc` on the freestanding
/// target only, so the crate still checks on the host.
///
/// # Panics
///
/// When cargo set no manifest directory: a build script cannot go on without
/// it.
pub fn x86_64_guest_build() {
    emit_target_cfg();
    link_x86_64_kernel_layout();
}

/// Hand the kernel's shared x86_64 linker script to `rustc` when cargo is
/// building for the freestanding x86_64 target, and do nothing on any other,
/// so a vertical's crate still checks on the host.
///
/// # Panics
///
/// When cargo set no manifest directory: a build script cannot go on without
/// it.
pub fn link_x86_64_kernel_layout() {
    if let Some(layout) = x86_64_layout() {
        println!("cargo:rustc-link-arg=-T{layout}");
    }
}

/// [`x86_64_guest_build`] for a vertical whose workload runs on the boot stack
/// at the depth the aarch64 and riscv64 `virt` layouts give every image, rather
/// than the shared layout's own 64 KiB: it links a script of its own that
/// states that size and includes the shared layout.
///
/// # Panics
///
/// When cargo set no manifest or output directory, or the script cannot be
/// written: a build script cannot go on without them.
pub fn x86_64_guest_build_with_virt_boot_stack() {
    emit_target_cfg();
    if let Some(layout) = x86_64_layout() {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
        let script = format!("{out_dir}/virt-boot-stack.ld");
        std::fs::write(&script, virt_boot_stack_script(&layout))
            .expect("the image's linker script is written");
        println!("cargo:rustc-link-arg=-T{script}");
    }
}

/// The boot stack the aarch64 and riscv64 `virt` layouts give every image.
const VIRT_BOOT_STACK: &str = "256K";

/// A linker script sizing the boot stack at [`VIRT_BOOT_STACK`] and then
/// including `layout`. The size is stated ahead of the layout rather than by
/// `--defsym`, which the linker applies only after the layout it would have
/// sized.
fn virt_boot_stack_script(layout: &str) -> String {
    format!("BOOT_STACK_BYTES = {VIRT_BOOT_STACK};\nINCLUDE \"{layout}\"\n")
}

/// The shared x86_64 kernel layout, when cargo is building for the
/// freestanding x86_64 target; `None` on every other, so the crate still
/// checks on the host.
fn x86_64_layout() -> Option<String> {
    if !std::env::var("TARGET").is_ok_and(|target| target == pie::PieArch::X86_64.target_triple()) {
        return None;
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let layout = format!(
        "{}/../../../kernel/arch/x86_64/linker.ld",
        manifest_dir.trim_end_matches('/')
    );
    println!("cargo:rerun-if-changed={layout}");
    Some(layout)
}

/// The whole build script of an aarch64 `virt` production-boot vertical: link
/// the board's linker script and embed the `virt` device tree for `cpus` CPUs
/// as `DTB_BLOB` in `OUT_DIR/dtb_fixture.rs` — empty on a host build, whose
/// bin is a no-op `main`.
///
/// # Panics
///
/// As [`aarch64_virt_guest_build_trees`].
pub fn aarch64_virt_guest_build(cpus: u32) {
    aarch64_virt_guest_build_trees(&[("dtb_fixture", Board::default(), cpus)]);
}

/// [`aarch64_virt_guest_build`] for a crate whose binaries boot different
/// machines: for each `(stem, board, cpus)`, the tree of `board` with `cpus`
/// CPUs as `DTB_BLOB` in `OUT_DIR/{stem}.rs`, so each binary includes the
/// tree of the machine it runs on and no other. A machine given more CPUs
/// than its tree describes starts only those the tree names.
///
/// # Panics
///
/// As [`aarch64_virt_guest_build_with_ram`].
pub fn aarch64_virt_guest_build_trees(trees: &[(&str, Board, u32)]) {
    embed_aarch64_virt_trees(None, trees);
}

/// [`aarch64_virt_guest_build`] for a guest given `ram_mib` mebibytes of RAM,
/// which its QEMU enrolment must declare too: the boot sizes the direct map
/// from the tree's `/memory` window, so a default tree would leave the rest
/// unmapped. The fixture also carries the figure as `GUEST_RAM_MIB`.
///
/// # Panics
///
/// When `qemu-system-aarch64` cannot dump the tree, cargo set no `OUT_DIR`
/// or manifest directory, or a fixture cannot be written: a build script
/// cannot go on without them.
pub fn aarch64_virt_guest_build_with_ram(cpus: u32, ram_mib: u32) {
    embed_aarch64_virt_trees(Some(ram_mib), &[("dtb_fixture", Board::default(), cpus)]);
}

/// The whole build script of an aarch64 `virt` vertical whose kernel reads no
/// device tree: link the board's linker script on the freestanding target.
///
/// # Panics
///
/// When cargo set no manifest directory: a build script cannot go on without
/// it.
pub fn aarch64_virt_guest_build_without_tree() {
    emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");
    link_virt_layout(pie::PieArch::Aarch64, "aarch64/link/aarch64-virt.ld");
}

/// Link the `virt` layout and write, for each `(stem, board)`, the tree of
/// `board` for `cpus` CPUs as `OUT_DIR/{stem}.rs`, empty on a host build: of
/// the default RAM, or of `ram_mib` mebibytes, which the fixture then states.
fn embed_aarch64_virt_trees(ram_mib: Option<u32>, trees: &[(&str, Board, u32)]) {
    use std::fmt::Write as _;

    emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR set by cargo");
    let freestanding = link_virt_layout(pie::PieArch::Aarch64, "aarch64/link/aarch64-virt.ld");
    for &(stem, board, cpus) in trees {
        let dtb = if freestanding {
            dump_aarch64_dtb(
                &out_dir,
                stem,
                cpus,
                ram_mib.unwrap_or(DEFAULT_DTB_RAM_MIB),
                board,
            )
        } else {
            Vec::new()
        };
        let (machine, _) = tairix_qemu::aarch64::machine(board);
        let mut out = format!(
            "// Auto-generated by build.rs. DO NOT EDIT.\n\
             /// The QEMU `{machine}` flattened device tree, dumped at build\n\
             /// time for the aarch64-none target (empty on host builds).\n\
             pub const DTB_BLOB: &[u8] = &["
        );
        for (i, byte) in dtb.iter().enumerate() {
            if i % 16 == 0 {
                out.push_str("\n    ");
            }
            write!(out, "0x{byte:02x}, ").expect("write to String never fails");
        }
        out.push_str("\n];\n");
        if let Some(ram_mib) = ram_mib {
            write!(
                out,
                "/// Guest RAM the tree describes, in mebibytes.\n\
                 pub const GUEST_RAM_MIB: u64 = {ram_mib};\n"
            )
            .expect("write to String never fails");
        }
        std::fs::write(
            std::path::PathBuf::from(&out_dir).join(format!("{stem}.rs")),
            out,
        )
        .expect("write the device-tree fixture");
    }
}

/// The whole build script of a riscv64 `virt` vertical: link the board's
/// linker script on the freestanding target. The firmware hands the kernel
/// the live device tree, so none is embedded.
///
/// # Panics
///
/// When cargo set no manifest directory: a build script cannot go on without
/// it.
pub fn riscv64_virt_guest_build() {
    emit_target_cfg();
    println!("cargo:rerun-if-changed=build.rs");
    link_virt_layout(pie::PieArch::Riscv64, "riscv64/link/riscv64-virt.ld");
}

/// Hand `rustc` the `virt` linker script at `script`, under
/// `kernel/arch/`, when cargo builds for `arch`; whether it does.
fn link_virt_layout(arch: pie::PieArch, script: &str) -> bool {
    if !std::env::var("TARGET").is_ok_and(|building| building == arch.target_triple()) {
        return false;
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let linker_script = format!(
        "{}/../../../kernel/arch/{script}",
        manifest_dir.trim_end_matches('/')
    );
    println!("cargo:rerun-if-changed={linker_script}");
    println!("cargo:rustc-link-arg=-T{linker_script}");
    true
}

/// Dump the tree of `board` into `OUT_DIR/{stem}.dtb` and return its
/// trimmed bytes.
fn dump_aarch64_dtb(
    out_dir: &std::ffi::OsStr,
    stem: &str,
    cpus: u32,
    ram_mib: u32,
    board: Board,
) -> Vec<u8> {
    let dtb_path = std::path::PathBuf::from(out_dir).join(format!("{stem}.dtb"));
    let dtb_str = dtb_path.display().to_string();
    let status = std::process::Command::new(tairix_qemu::aarch64::QEMU_BINARY)
        .args(dump_virt_dtb_args(&dtb_str, cpus, ram_mib, board))
        .status()
        .expect("run qemu-system-aarch64 to dump the virt DTB");
    assert!(status.success(), "qemu dumpdtb failed: {status}");
    trim_fdt_to_extent(&std::fs::read(&dtb_path).expect("read the dumped DTB"))
}

/// Trim a flattened device tree to the extent its header describes,
/// dropping any trailing padding, and rewrite the `totalsize` field to
/// match.
///
/// QEMU's `dumpdtb` emits the blob padded out to the machine's 1 MiB
/// device-tree region. A reader only needs the memory-reservation,
/// structure, and strings blocks, so this returns the prefix up to the
/// furthest block end (`off_dt_struct + size_dt_struct` /
/// `off_dt_strings + size_dt_strings`) and patches `totalsize` so the
/// trimmed copy stays self-consistent for a `totalsize`-driven reader.
///
/// A blob too short for the 40-byte header, with the wrong magic, or
/// whose header offsets escape the buffer is returned unchanged — trimming
/// is an optimisation, never a parser, so callers still validate the result
/// through `tairix_fdt::Fdt::new`.
#[must_use]
pub fn trim_fdt_to_extent(bytes: &[u8]) -> Vec<u8> {
    const FDT_MAGIC: u32 = 0xd00d_feed;
    let be_u32 = |off: usize| -> Option<u32> {
        let s = bytes.get(off..off + 4)?;
        Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    let header_ok = bytes.len() >= 40 && be_u32(0) == Some(FDT_MAGIC);
    let extent = header_ok.then(|| {
        let struct_off = be_u32(8)? as usize;
        let strings_off = be_u32(12)? as usize;
        let strings_size = be_u32(32)? as usize;
        let struct_size = be_u32(36)? as usize;
        let struct_end = struct_off.checked_add(struct_size)?;
        let strings_end = strings_off.checked_add(strings_size)?;
        let end = struct_end.max(strings_end);
        (end <= bytes.len()).then_some(end)
    });
    match extent.flatten() {
        Some(end) if end < bytes.len() => {
            let mut trimmed = bytes[..end].to_vec();
            let total = u32::try_from(end).unwrap_or(u32::MAX).to_be_bytes();
            trimmed[4..8].copy_from_slice(&total);
            trimmed
        }
        _ => bytes.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size is stated before the layout is included, because the shared
    /// layout reads `BOOT_STACK_BYTES` as it lays the stack out.
    #[test]
    fn the_virt_boot_stack_script_sizes_the_stack_before_including_the_layout() {
        assert_eq!(
            virt_boot_stack_script("/k/linker.ld"),
            "BOOT_STACK_BYTES = 256K;\nINCLUDE \"/k/linker.ld\"\n"
        );
    }

    #[test]
    fn hosted_targets_are_inert() {
        assert!(active_cfgs("linux", "x86_64").is_empty());
        assert!(active_cfgs("macos", "aarch64").is_empty());
    }

    #[test]
    fn bare_metal_x86_64_is_freestanding() {
        assert_eq!(
            active_cfgs("none", "x86_64"),
            ["freestanding", "itest_x86_64"]
        );
    }

    #[test]
    fn bare_metal_riscv64_is_freestanding() {
        assert_eq!(
            active_cfgs("none", "riscv64"),
            ["freestanding", "itest_riscv64"]
        );
    }

    #[test]
    fn bare_metal_aarch64_is_freestanding() {
        assert_eq!(
            active_cfgs("none", "aarch64"),
            ["freestanding", "itest_aarch64"]
        );
    }

    #[test]
    fn unknown_bare_metal_arch_is_freestanding_only() {
        assert_eq!(active_cfgs("none", "wasm32"), ["freestanding"]);
    }

    #[test]
    fn wasm32_browser_target_is_a_cdylib_not_freestanding() {
        assert_eq!(active_cfgs("unknown", "wasm32"), ["itest_wasm32"]);
    }

    #[test]
    fn every_active_cfg_is_declared() {
        for (os, arch) in [("none", "x86_64"), ("none", "riscv64"), ("none", "wasm32")] {
            for name in active_cfgs(os, arch) {
                assert!(KNOWN_CFGS.contains(&name), "{name} not declared");
            }
        }
    }

    #[test]
    fn dump_virt_dtb_args_match_the_runner_machine() {
        let args = dump_virt_dtb_args(
            "/tmp/out/virt.dtb",
            2,
            DEFAULT_DTB_RAM_MIB,
            Board::default(),
        );
        assert_eq!(
            args,
            [
                "-M",
                "virt,dumpdtb=/tmp/out/virt.dtb",
                "-cpu",
                tairix_qemu::aarch64::CPU,
                "-m",
                "256M",
                "-smp",
                "2",
                "-display",
                "none",
                "-no-reboot",
            ]
        );
    }

    /// A translated run's tree comes from the machine the run boots, unit and
    /// `-global`s alike.
    #[test]
    fn dump_virt_dtb_args_name_the_unit_the_run_attaches() {
        for (translation, board) in [
            (
                DmaTranslation::Smmuv3Stage1,
                "virt-9.1,iommu=smmuv3,dumpdtb=/t.dtb",
            ),
            (
                DmaTranslation::Smmuv3Stage2,
                "virt,iommu=smmuv3,dumpdtb=/t.dtb",
            ),
        ] {
            let args = dump_virt_dtb_args(
                "/t.dtb",
                1,
                DEFAULT_DTB_RAM_MIB,
                Board::translated(translation),
            );
            assert_eq!(args[..3], ["-M", board, "-cpu"], "{translation:?}");
        }
        let args = dump_virt_dtb_args(
            "/t.dtb",
            1,
            DEFAULT_DTB_RAM_MIB,
            Board::translated(DmaTranslation::VirtioIommu),
        );
        let unit = DmaTranslation::VirtioIommu
            .unit_device()
            .expect("the run creates the virtio-iommu");
        assert_eq!(
            args[..5],
            ["-M", "virt,dumpdtb=/t.dtb", "-device", unit, "-cpu"]
        );
    }

    #[test]
    fn dump_virt_dtb_args_name_the_interrupt_controllers_the_run_builds() {
        let args = dump_virt_dtb_args(
            "/t.dtb",
            1,
            DEFAULT_DTB_RAM_MIB,
            Board::translated(DmaTranslation::Smmuv3Stage2)
                .with_interrupts(InterruptControllers::Gicv3),
        );
        assert_eq!(
            args[..3],
            [
                "-M",
                "virt,iommu=smmuv3,gic-version=3,dumpdtb=/t.dtb",
                "-cpu"
            ]
        );
    }

    #[test]
    fn dump_virt_dtb_args_render_the_cpu_count() {
        let one = dump_virt_dtb_args("d", 1, DEFAULT_DTB_RAM_MIB, Board::default());
        let four = dump_virt_dtb_args("d", 4, DEFAULT_DTB_RAM_MIB, Board::default());
        let smp = |a: &[String]| a[a.iter().position(|s| s == "-smp").unwrap() + 1].clone();
        assert_eq!(smp(&one), "1");
        assert_eq!(smp(&four), "4");
    }

    /// The direct-map vertical runs with more RAM than the default, so the
    /// tree it embeds has to describe that RAM or the kernel maps the
    /// default and the extra memory is invisible.
    #[test]
    fn dump_virt_dtb_args_render_the_declared_memory() {
        let args = dump_virt_dtb_args("d", 1, 3072, Board::default());
        let mem = args[args.iter().position(|a| a == "-m").unwrap() + 1].clone();
        assert_eq!(mem, "3072M");
        // The default tracks the runner's own figure, so the two cannot
        // drift into a tree that describes RAM the guest does not have.
        assert_eq!(DEFAULT_DTB_RAM_MIB, tairix_qemu::aarch64::DEFAULT_RAM_MIB);
    }

    #[test]
    fn trimming_drops_padding_and_keeps_the_tree_parseable() {
        let blob = tairix_fdt::fixture::virt_like_arm(0x4000_0000, 0x2000_0000, "hvc", 14);
        // Simulate QEMU `dumpdtb` padding the blob out to its 1 MiB region.
        let mut padded = blob.clone();
        padded.resize(blob.len() + 4096, 0);

        let trimmed = trim_fdt_to_extent(&padded);
        assert!(trimmed.len() < padded.len(), "padding was not dropped");
        assert!(trimmed.len() <= blob.len());

        let fdt = tairix_fdt::Fdt::new(&trimmed).expect("trimmed fdt parses");
        assert_eq!(fdt.first_memory_region(), Some((0x4000_0000, 0x2000_0000)));
        let method = fdt
            .property(&[b"psci"], b"method")
            .expect("psci method present after trim");
        assert!(method.starts_with(b"hvc"), "psci method survived trim");
    }

    #[test]
    fn trimming_rewrites_totalsize_to_the_trimmed_length() {
        let blob = tairix_fdt::fixture::virt_like_arm(0x4000_0000, 0x2000_0000, "smc", 30);
        let mut padded = blob.clone();
        padded.resize(blob.len() + 8192, 0);
        let trimmed = trim_fdt_to_extent(&padded);
        let total = u32::from_be_bytes([trimmed[4], trimmed[5], trimmed[6], trimmed[7]]) as usize;
        assert_eq!(total, trimmed.len(), "totalsize must match trimmed length");
    }

    #[test]
    fn trimming_leaves_a_short_or_non_fdt_blob_unchanged() {
        assert_eq!(trim_fdt_to_extent(&[1, 2, 3]), vec![1, 2, 3]);
        let mut not_fdt = vec![0u8; 64];
        not_fdt[0] = 0xab;
        assert_eq!(trim_fdt_to_extent(&not_fdt), not_fdt);
    }

    /// A vertical names neither `virt` linker script: its whole build is
    /// one of the harness's, so the layout glue has one home.
    #[test]
    fn no_vertical_names_a_virt_linker_script() {
        let verticals = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut naming = Vec::new();
        for entry in std::fs::read_dir(&verticals).expect("tests/integration lists") {
            let script = entry.expect("an entry").path().join("build.rs");
            let Ok(source) = std::fs::read_to_string(&script) else {
                continue;
            };
            let code = source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            if ["aarch64-virt.ld", "riscv64-virt.ld"]
                .iter()
                .any(|layout| code.contains(layout))
            {
                naming.push(script);
            }
        }
        assert!(naming.is_empty(), "re-rolled layout glue: {naming:?}");
    }
}
