//! `cargo xtask board-check` implementation.
//!
//! The charter gives platform specifics one home — the architecture ports —
//! and lets a device's own driver or support crate name its device. Every
//! other Rust source and crate README under `lib/`, `kernel/`, `drivers/` and
//! `userland/` names no board, `SoC` or board-specific part: generic code learns
//! the hardware by discovery. This walks them and fails on any word
//! [`names_a_board`] recognises outside [`ALLOWED`].
//!
//! Words are read as identifiers are written: each run of letters and digits
//! whole, and again split where a lower-case letter meets an upper-case one,
//! so `bcm2711_hvs`, `BrcmMsiAlloc`, `VideoCore` and "the Pi 4" are all
//! caught, while a digit never splits a word (`bcm2711` stays whole).

use std::path::Path;

use super::source_tree;

/// The roots the rule binds.
const SCANNED: &[&str] = &["lib/", "kernel/", "drivers/", "userland/"];

/// Workspace-relative prefixes where naming the hardware is the code's job.
const ALLOWED: &[&str] = &[
    // The architecture ports.
    "kernel/arch/aarch64/",
    "kernel/arch/riscv64/",
    "kernel/arch/x86_64/",
    "kernel/arch/wasm32/",
    // The kernel's linker-script and load-address selection.
    "kernel/tairix-kernel/build.rs",
    "kernel/tairix-kernel/src/build_support.rs",
    // Device support crates and device drivers, each of one part.
    "lib/vcmailbox/",
    "drivers/audio/bcm2711_i2s/",
    "drivers/audio/bcm2711_pwm/",
    "drivers/bus/i2c/bcm2835/",
    "drivers/bus/i2c/brcmstb/",
    "drivers/bus/mailbox/vcmailbox/",
    "drivers/bus/pcie_brcm/",
    "drivers/bus/usb/vl805/",
    "drivers/clock/bcm2711_cprman/",
    "drivers/clock/bcm2711_dvp/",
    "drivers/clock/rpi_firmware/",
    "drivers/display/bcm2711_hdmi/",
    "drivers/display/bcm2711_hvs/",
    "drivers/dma/bcm2835/",
    "drivers/gpio/bcm2835/",
    "drivers/irq/brcmstb_l2/",
    "drivers/network/genet/",
    "drivers/rtc/rpi/",
    "drivers/sensor/bcm2711_thermal/",
    "drivers/storage/emmc2/",
    // The event-id registry names each owner by its crate path.
    "lib/log/src/registry.rs",
    // The Settings application's Raspberry Pi category.
    "userland/apps/settings/",
    // Remote peers classified by the vendor their address belongs to.
    "userland/apps/netmap/src/infer",
];

/// Prefixes that name hardware today and may only be removed: each is a
/// tracked defect, never a sanctioned pattern.
const GRANDFATHERED: &[&str] = &[];

/// Words that are a board, `SoC` or board-specific part however they are cased.
const DENIED_WORDS: &[&str] = &[
    "raspberry",
    "raspberrypi",
    "rpi",
    "rpi4",
    "rpi5",
    "rp1",
    "brcm",
    "broadcom",
    "bcmstb",
    "videocore",
    "vl805",
    "genet",
    "emmc2",
    "cprman",
    "hvs",
    "hifive",
    "sifive",
    "pi3",
    "pi3b",
    "pi4",
    "pi4b",
    "pi5",
];

/// What may follow `Pi` to name a Raspberry Pi model. The board is spelt
/// `Pi`; `PI` and `pi` are the constant, as in `FRAC_PI_4` and `pi/4`.
const PI_MODELS: &[&str] = &["3", "3b", "4", "4b", "5", "400", "500"];

/// A single offending occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Scan the workspace rooted at `root` and return every violation outside
/// the allow-list and grandfather list.
pub fn scan(root: &Path) -> Result<Vec<Violation>, String> {
    let mut out = Vec::new();
    source_tree::for_each_file(root, "board-check", |path, rel, name| {
        if !SCANNED.iter().any(|p| rel.starts_with(p))
            || is_listed(ALLOWED, rel)
            || is_listed(GRANDFATHERED, rel)
            || !(source_tree::is_rust(name) || name == "README.md")
        {
            return Ok(());
        }
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("board-check: cannot read {}: {e}", path.display()))?;
        for (idx, line) in src.lines().enumerate() {
            if names_a_board(line) {
                out.push(Violation {
                    path: rel.to_string(),
                    line: idx + 1,
                    text: line.trim().to_string(),
                });
            }
        }
        Ok(())
    })?;
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    Ok(out)
}

fn is_listed(list: &[&str], rel: &str) -> bool {
    list.iter().any(|p| rel.starts_with(p))
}

/// Whether `line` holds a word that names a board, `SoC` or board part.
fn names_a_board(line: &str) -> bool {
    let mut previous = "";
    for word in words(line) {
        let lower = word.to_ascii_lowercase();
        let denied = DENIED_WORDS.contains(&lower.as_str())
            || lower.starts_with("raspi")
            || is_bcm2_part(&lower)
            || (previous == "Pi" && PI_MODELS.contains(&lower.as_str()))
            || (previous.eq_ignore_ascii_case("surface") && lower == "go");
        if denied {
            return true;
        }
        previous = word;
    }
    false
}

/// `bcm2` followed by three digits: the Broadcom `SoC` and peripheral numbers.
fn is_bcm2_part(word: &str) -> bool {
    word.strip_prefix("bcm2")
        .is_some_and(|rest| rest.len() >= 3 && rest.bytes().take(3).all(|b| b.is_ascii_digit()))
}

/// The words of `line`: each run of letters and digits, then its parts where
/// a lower-case letter meets an upper-case one. A name written as one word
/// (`VideoCore`, `SiFive`) is caught whole; one written into an identifier
/// (`BrcmMsiAlloc`) by its part.
fn words(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|run| !run.is_empty())
        .flat_map(|run| {
            let parts = camel_parts(run).filter(move |part| part.len() < run.len());
            core::iter::once(run).chain(parts)
        })
}

fn camel_parts(word: &str) -> impl Iterator<Item = &str> {
    let bytes = word.as_bytes();
    let mut starts = vec![0];
    starts.extend(
        (1..bytes.len())
            .filter(|&i| bytes[i - 1].is_ascii_lowercase() && bytes[i].is_ascii_uppercase()),
    );
    starts.push(bytes.len());
    (0..starts.len() - 1).map(move |i| &word[starts[i]..starts[i + 1]])
}

/// Run the check, printing a report and returning an error if any
/// non-grandfathered violation remains.
pub fn run(root: &Path) -> Result<(), String> {
    use std::fmt::Write as _;
    let violations = scan(root)?;
    if violations.is_empty() {
        return Ok(());
    }
    let mut msg = String::from(
        "board-check: generic code names no board, SoC or board part; the \
         hardware is learnt by discovery (AGENTS.md §2.20):\n",
    );
    for v in &violations {
        let _ = writeln!(msg, "  {}:{}: {}", v.path, v.line, v.text);
    }
    Err(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_root() -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop();
        p.pop();
        p
    }

    #[test]
    fn workspace_is_clean_modulo_grandfathered() {
        let violations = scan(&workspace_root()).expect("scan");
        assert!(
            violations.is_empty(),
            "unexpected board-check violations: {violations:#?}"
        );
    }

    /// Regression: the kernel binary named the SD host and its board across
    /// the floor's wiring, which the floor drivers' manifests and discovery
    /// now carry instead.
    #[test]
    fn the_floor_wiring_names_no_board() {
        const WIRING: &[&str] = &[
            "driver_catalog.rs",
            "driver_loader.rs",
            "root_storage.rs",
            "unlock_orchestrate.rs",
            "unlock_service.rs",
            "aarch64/root_unlock.rs",
            "riscv64/root_unlock.rs",
            "x86_64/root_unlock.rs",
        ];
        let in_wiring = |path: &str| {
            path.strip_prefix("kernel/tairix-kernel/src/")
                .is_some_and(|file| WIRING.contains(&file) || file.starts_with("floor_"))
        };
        let violations: Vec<Violation> = scan(&workspace_root())
            .expect("scan")
            .into_iter()
            .filter(|violation| in_wiring(&violation.path))
            .collect();
        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn a_board_is_caught_however_it_is_spelled() {
        for line in [
            "// on the Raspberry Pi 4 the bus is wider",
            "let bus = BrcmMsiAlloc::new();",
            "const BCM2711_HVS: u32 = 1;",
            "fn bcm2711_hvs() {}",
            "/// the Pi 4B's onboard hub",
            "/// a Pi 400",
            "/// the VL805 xHCI",
            "let x = raspi4b();",
            "/// the `VideoCore` mailbox",
            "/// a Surface Go 2",
            "/// QEMU `virt` / SiFive",
        ] {
            assert!(names_a_board(line), "{line}");
        }
    }

    #[test]
    fn neutral_words_pass() {
        for line in [
            "/// the API 4 bytes wide",
            "let pipeline = Pipeline::new();",
            "/// a genetic algorithm, an RPC, a spi bus",
            "/// pi is 3.14159",
            "/// the surface goes dark",
            "/// a bcm-style register",
            "let quarter = FRAC_PI_4 * PI / 4.0;",
            "/// `alpha(0)`, equivalently `cos(pi/4)`",
        ] {
            assert!(!names_a_board(line), "{line}");
        }
    }

    #[test]
    fn the_ports_and_device_crates_may_name_their_hardware() {
        assert!(is_listed(ALLOWED, "kernel/arch/aarch64/src/platform.rs"));
        assert!(is_listed(ALLOWED, "drivers/network/genet/src/regs.rs"));
        assert!(!is_listed(ALLOWED, "drivers/network/virtio_net/src/lib.rs"));
        assert!(!is_listed(ALLOWED, "kernel/tairix-kernel/src/boot.rs"));
    }

    #[test]
    fn the_msi_wiring_names_no_board() {
        const NEUTRAL: &[&str] = &[
            "kernel/core/src/msimux.rs",
            "kernel/tairix-kernel/src/aarch64/arch_wrapper.rs",
            "kernel/tairix-kernel/src/aarch64/boot.rs",
            "kernel/tairix-kernel/src/aarch64/gic_irq.rs",
            "kernel/tairix-kernel/src/aarch64/init_spawn.rs",
            "kernel/tairix-kernel/src/aarch64_messages.rs",
            "kernel/tairix-kernel/src/riscv64/boot.rs",
        ];
        let violations: Vec<Violation> = scan(&workspace_root())
            .expect("scan")
            .into_iter()
            .filter(|violation| NEUTRAL.contains(&violation.path.as_str()))
            .collect();
        assert!(
            violations.is_empty(),
            "the message-interrupt wiring names a board: {violations:#?}"
        );
    }
}
