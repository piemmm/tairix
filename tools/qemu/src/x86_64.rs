//! x86_64-specific QEMU defaults and argv assembly (Stage 3a (d1)).
//!
//! The generic [`crate::Spec`] is architecture-neutral; everything that
//! is *only* meaningful when targeting `qemu-system-x86_64` lives here:
//!
//! * the default guest RAM size,
//! * the CPU model,
//! * the `isa-debug-exit` I/O-port constants,
//! * the exact QEMU argv the runner emits.
//!
//! # Boot protocol: PVH direct boot
//!
//! The kernel ELF is booted **directly** via `-kernel`: QEMU's ELF
//! loader honours the `XEN_ELFNOTE_PHYS32_ENTRY` note the kernel
//! carries (`kernel/arch/x86_64/src/boot.s`) and enters `pvh_start` in
//! 32-bit protected mode with the `hvm_start_info` record — no
//! firmware boot chain, no bootloader, no boot media in the loop. This
//! keeps every x86_64 guest boot deterministic and fast; the OVMF/GRUB
//! ISO path this replaced put OVMF's (nondeterministically crashing)
//! video path between the runner and the kernel's first instruction.
//! The multiboot2 header stays in the kernel for real bootloaders; the
//! test runner simply does not need one.
//!
//! Splitting this surface out keeps [`crate::Spec`] honest as a
//! per-arch tagged union and lines the codebase up with the sibling
//! modules (`aarch64.rs`, `riscv64.rs`) without duplicating any glue
//! (no duplication, no interface creep).
//!
//! # No `unwrap` / `expect` / `panic!`
//!
//! The only `expect`s in this file live inside `#[cfg(test)]` blocks —
//! the charter's tests carve-out.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::{net_device_arg, netdev_arg, rtc_base_args, SessionKind, Spec};

/// Default guest RAM size in mebibytes for an x86_64 QEMU integration
/// test.
///
/// 256 MiB is comfortable headroom for the test kernels (the SMP
/// scheduler-stress vertical sizes its 64 MiB bump heap against this
/// figure). Callers cannot override this today; if a future test needs
/// more RAM the right move is to add a `with_ram_mib(n)` builder on
/// [`Spec`] rather than smuggling it through `extra_args`.
pub const DEFAULT_RAM_MIB: u32 = 256;

/// I/O port the QEMU `isa-debug-exit` device listens on for x86_64
/// tests.
///
/// Re-exported from [`crate`] for callers that want the value without
/// pulling in the rest of the runner.
pub const ISA_DEBUG_EXIT_IOPORT: u16 = 0xf4;

/// I/O port size the QEMU `isa-debug-exit` device is configured with.
///
/// Re-exported from [`crate`] for callers that want the value without
/// pulling in the rest of the runner.
pub const ISA_DEBUG_EXIT_IOSIZE: u8 = 0x04;

/// Name of the `qemu-system-*` binary for x86_64.
pub const QEMU_BINARY: &str = "qemu-system-x86_64";

/// The instructions the port's entropy source draws from, plus `enforce`.
/// Without `RDRAND`/`RDSEED` the kernel's random reserve never seeds and every
/// CSPRNG consumer fails closed; `enforce` refuses to boot rather than
/// silently dropping a feature the accelerator cannot supply. Appended to
/// every `-cpu` model so a capability override ([`Spec::with_x86_64_cpu`])
/// still seeds.
const ENTROPY_FEATURES: &str = "+rdrand,+rdseed,enforce";

/// The default CPU model: QEMU's baseline `qemu64` plus the entropy
/// features every model carries (`RDRAND`/`RDSEED`, `enforce`).
pub const CPU: &str = "qemu64,+rdrand,+rdseed,enforce";

/// How the board lays its CPUs out.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Topology {
    /// `Spec::cpus` CPUs at APIC ids from 0, the board's default.
    #[default]
    Dense,
    /// The boot CPU, and one more at the first core of a second socket of
    /// 256, so its APIC id is 256: past the eight bits xAPIC names.
    ApPastXapic,
}

/// [`Topology::ApPastXapic`]'s `-smp`: room for two sockets of 256 cores,
/// the boot CPU alone present at the start.
const AP_PAST_XAPIC_SMP: &str = "1,maxcpus=512,sockets=2,cores=256,threads=1";

/// Where [`Topology::ApPastXapic`] plugs its second CPU in.
const AP_PAST_XAPIC_SLOT: &str = "socket-id=1,core-id=0,thread-id=0";

/// The `-cpu` model for `spec`: its capability override with the entropy
/// features appended, or the default [`CPU`].
fn cpu_model(spec: &Spec) -> String {
    match spec.x86_64_cpu {
        Some(model) => alloc_format(model),
        None => CPU.into(),
    }
}

/// Compose `{model},{ENTROPY_FEATURES}`.
fn alloc_format(model: &str) -> String {
    let mut out = String::with_capacity(model.len() + 1 + ENTROPY_FEATURES.len());
    out.push_str(model);
    out.push(',');
    out.push_str(ENTROPY_FEATURES);
    out
}

/// Push the x86_64 QEMU argv onto `cmd`.
///
/// Emits the canonical x86_64 invocation: the [`CPU`] model, headless
/// display, serial over stdio, `isa-debug-exit` device on
/// [`ISA_DEBUG_EXIT_IOPORT`], `-m {DEFAULT_RAM_MIB}M`, `-smp {spec.cpus}`,
/// `-no-reboot`, and the kernel ELF PVH-direct-booted via `-kernel` (see
/// the module docs).
pub(crate) fn push_argv(cmd: &mut Command, spec: &Spec, kernel: &Path) {
    for arg in build_argv(spec, kernel) {
        cmd.arg(arg);
    }
}

/// Push `spec`'s board, its translation unit, and its CPUs: their model, with
/// x2APIC wherever remapping or their layout needs it, and their layout.
fn push_board(argv: &mut Vec<OsString>, spec: &Spec) {
    // Another board's unit is refused before any argv is built. The default
    // `pc` board holds no translation unit, and no more than 255 CPUs.
    let unit = spec.dma_translation.unit_device();
    let past_xapic = spec.x86_64_topology == Topology::ApPastXapic;
    if unit.is_some() || past_xapic {
        argv.push("-machine".into());
        argv.push("q35".into());
    }
    if let Some(unit) = unit {
        argv.push("-device".into());
        argv.push(unit.into());
    }
    let mut cpu = cpu_model(spec);
    if spec.dma_translation.remaps() || past_xapic {
        cpu.push_str(",+x2apic");
    }
    // The type a plugged-in CPU is of: the model, without its features.
    let cpu_type = format!("{}-x86_64-cpu", cpu.split(',').next().unwrap_or_default());
    argv.push("-cpu".into());
    argv.push(cpu.into());
    argv.push("-smp".into());
    match spec.x86_64_topology {
        Topology::Dense => argv.push(spec.cpus.to_string().into()),
        Topology::ApPastXapic => {
            argv.push(AP_PAST_XAPIC_SMP.into());
            argv.push("-device".into());
            argv.push(format!("{cpu_type},{AP_PAST_XAPIC_SLOT}").into());
        }
    }
}

/// Pure argv builder used by [`push_argv`] and the host unit tests.
///
/// Splitting the pure builder out keeps the argv-assembly contract
/// unit-testable. The list is intentionally returned as
/// `Vec<OsString>` so callers can inspect it before spawning QEMU.
fn build_argv(spec: &Spec, kernel: &Path) -> Vec<OsString> {
    // Note: `-nographic` is *not* used because it implicitly attaches the
    // monitor and serial 0 to stdio, which collides with our explicit
    // `-serial stdio`. `-display none` gives the headless behaviour we
    // want without that implicit muxing.
    let mut argv: Vec<OsString> = Vec::with_capacity(22 + spec.extra_args.len());
    push_board(&mut argv, spec);
    argv.push("-no-reboot".into());
    // Pin the board's emulated real-time clock when the vertical asked for
    // a deterministic one, so a clock-chip driver's reading is a value the
    // run can assert rather than whatever the host clock says.
    if let Some(args) = rtc_base_args(spec) {
        argv.extend(args);
    }
    // Headless by default (the test runner captures serial only); an
    // interactive windowed run omits `-display` here so the runner can append
    // the windowing backend it selected at spawn time.
    if spec.session == SessionKind::HeadlessTest {
        argv.push("-display".into());
        argv.push("none".into());
    }
    argv.push("-serial".into());
    argv.push("stdio".into());
    argv.push("-m".into());
    argv.push(format!("{}M", spec.ram_mib()).into());
    argv.push("-device".into());
    argv.push(
        format!(
            "isa-debug-exit,iobase=0x{ISA_DEBUG_EXIT_IOPORT:x},\
             iosize=0x{ISA_DEBUG_EXIT_IOSIZE:x}"
        )
        .into(),
    );
    // Attach QEMU's `ramfb` display device when requested. `ramfb` is a
    // firmware-programmed linear framebuffer whose scan-out surface lives
    // in guest RAM; the guest programs its geometry over the `fw_cfg`
    // device the `pc`/`q35` machine already carries (here over the x86
    // IOport DMA interface). This is what the vesa-display vertical
    // drives.
    if spec.devices.ramfb {
        argv.push("-device".into());
        argv.push("ramfb".into());
    }
    // PVH direct boot: QEMU's ELF loader reads the kernel's
    // XEN_ELFNOTE_PHYS32_ENTRY note and enters `pvh_start` with the
    // start-info record — no firmware boot chain in the path. SeaBIOS
    // (the machine default) performs the PCI BAR assignment and keeps
    // every BAR inside the 32-bit MMIO hole below 4 GiB, which the boot
    // trampoline's 0..4 GiB identity map and the Stage 4.D
    // `DirectPhysMap` require.
    argv.push("-kernel".into());
    argv.push(kernel.into());

    // Attach each backing image as a modern virtio-blk-pci function.
    // `if=none` detaches the drive from any automatic controller so the
    // explicit `-device virtio-blk-pci,drive=blkN` is the only thing that
    // surfaces it to the guest — that is the PCI function the Stage 4.D
    // boot walk discovers and `PciTransport` drives.
    //
    // `disable-legacy=on` forces the function to be a *non-transitional*
    // (modern, virtio-1.0+) device: it reports PCI device id 0x1042
    // (`0x1040 + virtio-blk`) and exposes its registers exclusively
    // through the virtio-1.x PCI capability layout the boot walk decodes
    // (`tairix_kernel::provision_virtio_pci`). Without it QEMU's default
    // `pc`/`q35` machine presents a *transitional* device (id 0x1001) on
    // the legacy PCI bus, which the modern-only walk would not match.
    for (i, dev) in spec.block_devices.iter().enumerate() {
        argv.push("-drive".into());
        let mut drive = OsString::from(format!("if=none,format=raw,id=blk{i},file="));
        drive.push(dev.image.as_os_str());
        argv.push(drive);
        argv.push("-device".into());
        argv.push(
            format!(
                "virtio-blk-pci,drive=blk{i},{}",
                crate::virtio_pci_options(spec)
            )
            .into(),
        );
    }

    // Suppress QEMU's implicit default NIC for a network-free vertical (see
    // the aarch64 builder): the `q35`/`pc` machine auto-creates a default
    // network device when no networking option is given, a phantom
    // interface a guest's discovery would enumerate. Added only when no
    // explicit `-netdev` is attached below (which already overrides it).
    if spec.net_devices.is_empty() {
        argv.push("-net".into());
        argv.push("none".into());
    }

    // Attach each network interface as a modern virtio-net-pci function
    // behind the backend the spec chose (`netdev_arg`).
    // `disable-legacy=on` pins the function to the modern virtio-1.x PCI
    // layout the Stage 4.D boot walk decodes (device id 0x1041 =
    // `0x1040 + virtio-net`), exactly as for virtio-blk above. An optional
    // `filter-dump` mirrors every frame on the interface to a host pcap so
    // the harness can verify the exchange after the run.
    for (i, dev) in spec.net_devices.iter().enumerate() {
        argv.push("-netdev".into());
        argv.push(netdev_arg(i, dev));
        argv.push("-device".into());
        argv.push(net_device_arg(
            "virtio-net-pci",
            i,
            dev,
            &format!(",{}", crate::virtio_pci_options(spec)),
        ));
        if let Some(pcap) = &dev.pcap {
            argv.push("-object".into());
            let mut filter = OsString::from(format!("filter-dump,id=dump{i},netdev=net{i},file="));
            filter.push(pcap.as_os_str());
            argv.push(filter);
        }
    }

    // The input devices as PCI functions, pinned to the modern layout the
    // virtio-input discovery probe matches, as virtio-blk and virtio-net are.
    // Behind the bridge each takes a slot past 0, so its own requester id
    // differs from the bridge's alias for it.
    let input =
        crate::input_device_args(spec, &format!("-pci,{}", crate::virtio_pci_options(spec)));
    if spec.devices.input == crate::InputPlacement::Bridged {
        argv.push("-device".into());
        argv.push(format!("pcie-pci-bridge,id={INPUT_BRIDGE},bus=pcie.0").into());
        // `input` is `-device <arg>` pairs.
        for (slot, mut arg) in (1u8..).zip(input.into_iter().skip(1).step_by(2)) {
            arg.push(format!(",bus={INPUT_BRIDGE},addr=0x{slot:x}"));
            argv.push("-device".into());
            argv.push(arg);
        }
    } else {
        argv.extend(input);
    }

    // Attach a virtio sound device behind QEMU's `wav` backend, which writes
    // what the emulated card received to a host file the vertical then checks
    // sample for sample.
    let sound = if spec.dma_translation == crate::DmaTranslation::Absent {
        "virtio-sound-pci"
    } else {
        "virtio-sound-pci,iommu_platform=on"
    };
    argv.extend(crate::audio_wav_args(spec, sound));

    argv
}

/// The id of the PCIe-to-PCI bridge the input devices sit behind
/// ([`crate::InputPlacement::Bridged`]).
const INPUT_BRIDGE: &str = "inputbridge";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Arch, AttachedDevices, SessionKind};
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixture_spec(cpus: u32) -> Spec {
        Spec {
            arch: Arch::X86_64,
            kernel: PathBuf::from("/tmp/k.elf"),
            cpus,
            timeout: Duration::from_secs(60),
            declared_runtime_ceiling: None,
            declared_ram_mib: None,
            x86_64_cpu: None,
            x86_64_topology: crate::x86_64::Topology::Dense,
            block_devices: Vec::new(),
            net_devices: Vec::new(),
            devices: AttachedDevices::NONE,
            dma_translation: crate::DmaTranslation::Absent,
            interrupts: crate::InterruptControllers::Default,
            rtc_base_unix_secs: None,
            audio_wav_path: None,
            extra_args: Vec::new(),
            input_keyboard: None,
            input_typing: Vec::new(),
            pointer_script: Vec::new(),
            bounded_pointer_script: false,
            serial_input: Vec::new(),
            screendumps: Vec::new(),
            monitor_commands: Vec::new(),
            session: SessionKind::HeadlessTest,
            reset_success_marker: None,
            completion_gate: None,
        }
    }

    fn render(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn default_ram_is_two_hundred_fifty_six_mebibytes() {
        // Pinned at 256 MiB — the SMP scheduler-stress vertical sizes
        // its bump heap against this figure (see the const's docs).
        assert_eq!(DEFAULT_RAM_MIB, 256);
    }

    #[test]
    fn isa_debug_exit_constants_match_qemu_documentation() {
        // QEMU's `isa-debug-exit` defaults are iobase=0x501,iosize=2;
        // the runner explicitly overrides both. The kernel side
        // (`kernel/arch/x86_64::qemu_exit`) hard-codes the same values
        // — a mismatch here is a silent test-protocol break.
        assert_eq!(ISA_DEBUG_EXIT_IOPORT, 0xf4);
        assert_eq!(ISA_DEBUG_EXIT_IOSIZE, 0x04);
    }

    #[test]
    fn qemu_binary_name_is_arch_specific() {
        assert_eq!(QEMU_BINARY, "qemu-system-x86_64");
    }

    #[test]
    fn argv_pins_the_rtc_only_when_the_spec_asked_for_it() {
        let plain = fixture_spec(1);
        let argv = render(&build_argv(&plain, Path::new("/tmp/k.elf")));
        assert!(
            !argv.iter().any(|a| a == "-rtc"),
            "a spec that pinned no instant leaves the host clock"
        );

        // 2027-03-05T12:00:00Z, spelled through the workspace calendar.
        let pinned = fixture_spec(1).with_rtc_base(1_804_248_000);
        let argv = render(&build_argv(&pinned, Path::new("/tmp/k.elf")));
        let pos = argv
            .iter()
            .position(|a| a == "-rtc")
            .expect("argv pins the clock chip");
        assert_eq!(argv[pos + 1], "base=2027-03-05T12:00:00");
    }

    #[test]
    fn a_translated_run_builds_its_unit_first_and_routes_every_virtio_function_through_it() {
        for (unit, device, remaps) in [
            (
                crate::DmaTranslation::Vtd,
                "intel-iommu,intremap=on,eim=on",
                true,
            ),
            (
                crate::DmaTranslation::AmdVi,
                "amd-iommu,dma-remap=on,intremap=on,xtsup=on",
                true,
            ),
            (
                crate::DmaTranslation::VirtioIommu,
                "virtio-iommu-pci,addr=0x2",
                false,
            ),
        ] {
            let mut spec = fixture_spec(1).with_dma_translation(unit);
            spec.block_devices.push(crate::BlockDevice {
                image: PathBuf::from("/tmp/root.img"),
            });
            spec = spec.with_virtio_keyboard("ready", "a");
            let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
            assert_eq!(
                argv[..4],
                ["-machine", "q35", "-device", device],
                "the unit precedes every device it translates"
            );
            let cpu = argv
                .iter()
                .position(|a| a == "-cpu")
                .map(|at| &argv[at + 1])
                .expect("a CPU model");
            assert_eq!(
                cpu.ends_with(",+x2apic"),
                remaps,
                "extended remapping wants x2APIC: {cpu}"
            );
            for device in ["virtio-blk-pci", "virtio-keyboard-pci"] {
                let arg = argv
                    .iter()
                    .find(|a| a.starts_with(device))
                    .expect("the device is attached");
                assert!(arg.contains("iommu_platform=on"), "{arg}");
            }
        }

        let plain = render(&build_argv(&fixture_spec(1), Path::new("/tmp/k.elf")));
        assert!(!plain.iter().any(|a| a.contains("iommu") || a == "q35"));
    }

    #[test]
    fn bridged_input_hangs_behind_a_pcie_to_pci_bridge_past_slot_zero() {
        let mut spec = fixture_spec(1)
            .with_dma_translation(crate::DmaTranslation::Vtd)
            .with_input_bridge();
        spec = spec.with_virtio_keyboard("ready", "a");
        spec.devices.pointing.mouse = true;
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let bridge = argv
            .iter()
            .position(|a| a == "pcie-pci-bridge,id=inputbridge,bus=pcie.0")
            .expect("the bridge is attached");
        let keyboard = argv
            .iter()
            .position(|a| a.starts_with("virtio-keyboard-pci"))
            .expect("the keyboard is attached");
        assert!(bridge < keyboard, "the bridge precedes what hangs from it");
        assert!(
            argv[keyboard].ends_with(",bus=inputbridge,addr=0x1"),
            "{}",
            argv[keyboard]
        );
        assert!(argv[keyboard].contains("iommu_platform=on"));
        let mouse = argv
            .iter()
            .find(|a| a.starts_with("virtio-mouse-pci"))
            .expect("the mouse is attached");
        assert!(mouse.ends_with(",bus=inputbridge,addr=0x2"), "{mouse}");

        let plain = render(&build_argv(
            &fixture_spec(1)
                .with_dma_translation(crate::DmaTranslation::Vtd)
                .with_virtio_keyboard("ready", "a"),
            Path::new("/tmp/k.elf"),
        ));
        assert!(!plain.iter().any(|a| a.contains("pcie-pci-bridge")));
    }

    #[test]
    fn argv_contains_documented_invariant_flags() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        // Headless boot — see the comment on `build_argv` for why
        // `-nographic` is forbidden.
        assert!(argv.iter().any(|a| a == "-no-reboot"));
        assert!(argv.iter().any(|a| a == "-display"));
        assert!(argv.iter().any(|a| a == "none"));
        assert!(argv.iter().any(|a| a == "-serial"));
        assert!(argv.iter().any(|a| a == "stdio"));
    }

    #[test]
    fn argv_presents_the_instructions_the_entropy_source_draws_from() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let at = argv
            .iter()
            .position(|a| a == "-cpu")
            .expect("argv selects a CPU model");
        let model = &argv[at + 1];
        for feature in ["+rdrand", "+rdseed", "enforce"] {
            assert!(
                model.split(',').any(|f| f == feature),
                "{model} lacks {feature}"
            );
        }
    }

    /// The AP-past-xAPIC layout plugs a second CPU of the run's own model in
    /// at the first core of a second 256-core socket, x2APIC on, beside the
    /// boot CPU alone present at the start; the default stays dense.
    #[test]
    fn the_ap_past_xapic_layout_plugs_its_cpu_in_at_apic_id_256() {
        let spec = fixture_spec(2)
            .with_x86_64_cpu("max")
            .with_x86_64_topology(Topology::ApPastXapic);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let after = |argv: &[String], flag: &str| {
            argv.iter()
                .position(|a| a == flag)
                .map(|at| argv[at + 1].clone())
        };
        assert_eq!(
            after(&argv, "-smp").as_deref(),
            Some("1,maxcpus=512,sockets=2,cores=256,threads=1")
        );
        assert_eq!(
            after(&argv, "-machine").as_deref(),
            Some("q35"),
            "past pc's 255"
        );
        let cpu = after(&argv, "-cpu").expect("a CPU model");
        assert!(cpu.split(',').any(|f| f == "+x2apic"), "{cpu}");
        assert!(
            argv.iter()
                .any(|a| a == "max-x86_64-cpu,socket-id=1,core-id=0,thread-id=0"),
            "{argv:?}"
        );
        let dense = render(&build_argv(&fixture_spec(2), Path::new("/tmp/k.elf")));
        assert_eq!(after(&dense, "-smp").as_deref(), Some("2"));
        assert!(!dense.iter().any(|a| a.contains("socket-id")));
    }

    #[test]
    fn a_cpu_override_replaces_the_model_and_keeps_the_entropy_features() {
        let spec = fixture_spec(1).with_x86_64_cpu("max,-xsaveopt");
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let at = argv.iter().position(|a| a == "-cpu").expect("a CPU model");
        let model = &argv[at + 1];
        assert!(model.starts_with("max,-xsaveopt,"), "{model}");
        for feature in ["+rdrand", "+rdseed", "enforce"] {
            assert!(
                model.split(',').any(|f| f == feature),
                "{model} lacks {feature}"
            );
        }
        // The default is unchanged when no override is set.
        let plain = render(&build_argv(&fixture_spec(1), Path::new("/tmp/k.elf")));
        let at = plain.iter().position(|a| a == "-cpu").unwrap();
        assert_eq!(plain[at + 1], CPU);
    }

    #[test]
    fn argv_boots_the_kernel_elf_directly_with_no_boot_media() {
        // PVH direct boot: the kernel ELF is passed straight to QEMU's
        // `-kernel` loader. No boot media, no firmware images: the
        // OVMF pflash pair, the GRUB ISO, and the `-cdrom` flag this
        // replaced must never reappear (they put nondeterministic
        // firmware between the runner and the kernel's entry point).
        let spec = fixture_spec(1);
        let kernel = Path::new("/tmp/k.elf");
        let argv = render(&build_argv(&spec, kernel));
        let pos = argv
            .iter()
            .position(|a| a == "-kernel")
            .expect("argv contains -kernel");
        assert_eq!(argv[pos + 1], kernel.to_string_lossy().into_owned());
        assert!(!argv.iter().any(|a| a == "-cdrom"));
        assert!(!argv.iter().any(|a| a.contains("if=pflash")));
        assert!(!argv.iter().any(|a| a.contains("X-PciMmio64Mb")));
    }

    #[test]
    fn argv_encodes_ram_size_in_mebibytes() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let mem_pos = argv
            .iter()
            .position(|a| a == "-m")
            .expect("argv contains -m");
        assert_eq!(argv[mem_pos + 1], format!("{DEFAULT_RAM_MIB}M"));
    }

    #[test]
    fn argv_encodes_cpu_count() {
        for n in [1u32, 4, 8] {
            let spec = fixture_spec(n);
            let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
            let pos = argv
                .iter()
                .position(|a| a == "-smp")
                .expect("argv contains -smp");
            assert_eq!(argv[pos + 1], n.to_string());
        }
    }

    #[test]
    fn argv_defaults_to_the_per_arch_ram_size() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let pos = argv
            .iter()
            .position(|a| a == "-m")
            .expect("argv contains -m");
        assert_eq!(argv[pos + 1], format!("{DEFAULT_RAM_MIB}M"));
    }

    #[test]
    fn argv_encodes_a_declared_ram_size() {
        // The direct-map vertical needs more RAM than the boot trampoline's
        // own identity window, so the declared size must reach the argv.
        let spec = fixture_spec(1).with_ram_mib(5120);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let pos = argv
            .iter()
            .position(|a| a == "-m")
            .expect("argv contains -m");
        assert_eq!(argv[pos + 1], "5120M");
    }

    #[test]
    fn argv_programs_isa_debug_exit_with_runner_constants() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let pos = argv
            .iter()
            .position(|a| a.starts_with("isa-debug-exit"))
            .expect("argv contains the isa-debug-exit device");
        assert_eq!(
            argv[pos],
            format!(
                "isa-debug-exit,iobase=0x{ISA_DEBUG_EXIT_IOPORT:x},\
                 iosize=0x{ISA_DEBUG_EXIT_IOSIZE:x}"
            )
        );
        assert_eq!(argv[pos - 1], "-device");
    }

    #[test]
    fn argv_without_block_devices_attaches_no_virtio_blk() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        assert!(
            !argv.iter().any(|a| a.starts_with("virtio-blk-pci")),
            "a storage-free spec must not attach a virtio-blk device"
        );
    }

    #[test]
    fn argv_attaches_each_block_device_as_virtio_blk_pci() {
        let mut spec = fixture_spec(1);
        spec.block_devices = vec![
            crate::BlockDevice {
                image: PathBuf::from("/tmp/disk0.img"),
            },
            crate::BlockDevice {
                image: PathBuf::from("/tmp/disk1.img"),
            },
        ];
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));

        // Each device is a detached drive (`if=none`) bound to its own
        // virtio-blk-pci function by a matching id.
        assert!(argv.iter().any(|a| a.contains("if=none")
            && a.contains("id=blk0")
            && a.contains("/tmp/disk0.img")));
        assert!(argv.iter().any(|a| a.contains("if=none")
            && a.contains("id=blk1")
            && a.contains("/tmp/disk1.img")));
        // `disable-legacy=on` pins the function to the modern
        // (non-transitional) virtio-1.x layout the boot walk decodes.
        assert!(argv
            .iter()
            .any(|a| a == "virtio-blk-pci,drive=blk0,disable-legacy=on"));
        assert!(argv
            .iter()
            .any(|a| a == "virtio-blk-pci,drive=blk1,disable-legacy=on"));
    }

    #[test]
    fn argv_without_ramfb_attaches_no_display_device() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        assert!(
            !argv.iter().any(|a| a == "ramfb"),
            "a display-free spec must not attach a ramfb device"
        );
    }

    #[test]
    fn argv_attaches_ramfb_when_requested() {
        let mut spec = fixture_spec(1);
        spec.devices.ramfb = true;
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let pos = argv
            .iter()
            .position(|a| a == "ramfb")
            .expect("argv contains the ramfb device");
        assert_eq!(argv[pos - 1], "-device");
    }

    #[test]
    fn argv_without_net_devices_attaches_no_virtio_net() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        assert!(
            !argv.iter().any(|a| a.starts_with("virtio-net-pci")),
            "a network-free spec must not attach a virtio-net device"
        );
        assert!(
            !argv.iter().any(|a| a.starts_with("dgram,id=net")),
            "a network-free spec must not attach a dgram netdev"
        );
    }

    #[test]
    fn argv_attaches_each_net_device_as_virtio_net_pci() {
        let mut spec = fixture_spec(1);
        spec.net_devices = vec![
            crate::NetDevice {
                backend: crate::NetBackend::Dgram {
                    qemu_sock: PathBuf::from("/tmp/net0.qemu.sock"),
                    peer_sock: PathBuf::from("/tmp/net0.peer.sock"),
                },
                pcap: None,
                mac: None,
            },
            crate::NetDevice {
                backend: crate::NetBackend::Dgram {
                    qemu_sock: PathBuf::from("/tmp/net1.qemu.sock"),
                    peer_sock: PathBuf::from("/tmp/net1.peer.sock"),
                },
                pcap: Some(PathBuf::from("/tmp/cap1.pcap")),
                mac: None,
            },
        ];
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));

        // Each interface is a dgram netdev bound to its own modern
        // virtio-net-pci function by a matching id.
        assert!(argv.iter().any(|a| a
            == "dgram,id=net0,local.type=unix,local.path=/tmp/net0.qemu.sock,\
                remote.type=unix,remote.path=/tmp/net0.peer.sock"));
        assert!(argv.iter().any(|a| a
            == "dgram,id=net1,local.type=unix,local.path=/tmp/net1.qemu.sock,\
                remote.type=unix,remote.path=/tmp/net1.peer.sock"));
        assert!(argv
            .iter()
            .any(|a| a == "virtio-net-pci,netdev=net0,disable-legacy=on"));
        assert!(argv
            .iter()
            .any(|a| a == "virtio-net-pci,netdev=net1,disable-legacy=on"));
        // Only the interface with a capture path gets a filter-dump.
        assert!(
            !argv
                .iter()
                .any(|a| a.contains("filter-dump") && a.contains("dump0")),
            "capture-free interface must not attach a filter-dump"
        );
        assert!(argv.iter().any(|a| a.contains("filter-dump")
            && a.contains("netdev=net1")
            && a.contains("/tmp/cap1.pcap")));
    }

    #[test]
    fn headless_default_argv_attaches_no_input_devices() {
        let spec = fixture_spec(1);
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        assert!(
            !argv.iter().any(|a| a.starts_with("virtio-keyboard-pci")),
            "a headless spec with no injection must not attach a keyboard"
        );
        assert!(
            !argv.iter().any(|a| a.starts_with("virtio-mouse-pci")),
            "a headless spec with no injection must not attach a mouse"
        );
    }

    #[test]
    fn argv_attaches_a_modern_virtio_keyboard_pci_for_key_injection() {
        let mut spec = fixture_spec(1);
        spec.input_keyboard = Some(crate::KeyInjection {
            ready_marker: "sc=irq_bind".into(),
            key: "a".into(),
            ready_occurrences: 1,
        });
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        let pos = argv
            .iter()
            .position(|a| a == "virtio-keyboard-pci,disable-legacy=on")
            .expect("argv contains the modern virtio-keyboard-pci device");
        assert_eq!(argv[pos - 1], "-device");
        // Key injection alone attaches no pointer device.
        assert!(!argv.iter().any(|a| a.starts_with("virtio-mouse-pci")));
    }

    #[test]
    fn windowed_interactive_argv_attaches_keyboard_mouse_and_touchscreen() {
        let mut spec = fixture_spec(1);
        spec.session = SessionKind::WindowedInteractive;
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        for device in [
            "virtio-keyboard-pci,disable-legacy=on",
            "virtio-mouse-pci,disable-legacy=on",
            "virtio-multitouch-pci,disable-legacy=on",
        ] {
            assert!(argv.iter().any(|a| a == device), "{device}: {argv:?}");
        }
    }

    #[test]
    fn windowed_interactive_argv_omits_display_for_the_runner_to_choose() {
        // The interactive display backend is selected and appended by the
        // runner at spawn time; the builder must not pin `-display none`, or
        // the window would never open.
        let mut spec = fixture_spec(1);
        spec.session = SessionKind::WindowedInteractive;
        let argv = render(&build_argv(&spec, Path::new("/tmp/k.elf")));
        assert!(
            !argv.iter().any(|a| a == "-display"),
            "windowed run leaves the display for the runner to select"
        );
    }
}
