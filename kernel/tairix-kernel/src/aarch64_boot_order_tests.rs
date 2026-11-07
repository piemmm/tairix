//! The aarch64 boot's order, which no QEMU run can show: QEMU's UART is
//! powered up from reset, and a fault before the reports are installed parks
//! the CPU without a word.

const BOOT: &str = include_str!("aarch64/boot.rs");

/// The boot entry, to the brace that closes it.
fn boot_body() -> &'static str {
    let entry = BOOT.find("pub fn boot(").expect("the boot entry");
    let end = BOOT[entry..].find("\n}\n").expect("it ends");
    &BOOT[entry..entry + end]
}

/// Where `needle` first appears in the boot entry.
fn position(needle: &str) -> usize {
    boot_body()
        .find(needle)
        .unwrap_or_else(|| panic!("the boot entry names `{needle}`"))
}

/// A fault in the console, mailbox or display bring-up is reported, since the
/// reports are installed before the boot touches the board.
#[test]
fn the_fatal_reports_are_installed_before_the_boot_touches_the_board() {
    let reports = position("install_fatal_reports();");
    assert!(reports < position("type_direct_map(tree)"));
    assert!(reports < position("configure_mmio_from_dtb(tree)"));
}

/// The direct map's record is written once the console's line is up: a Pi's
/// UART is silent until it is programmed.
#[test]
fn the_direct_map_is_recorded_once_the_console_is_up() {
    assert!(position("configure_mmio_from_dtb(tree)") < position("log_direct_map("));
}
