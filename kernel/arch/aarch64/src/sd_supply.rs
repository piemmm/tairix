//! The SD card's supplies where the firmware switches them: the card's I/O
//! (signalling) rail and its power rail, each a regulator driven by one line
//! of the firmware's GPIO expander.
//!
//! On a Pi 4 the EMMC2 node names both: `vqmmc-supply` is a `regulator-gpio`
//! whose expander line selects 1.8 V or 3.3 V signalling, and `vmmc-supply` a
//! `regulator-fixed` one expander line enables. Each SD host's own properties
//! are read, never assumed, and the discovery walk states the pair on that
//! node ([`crate::platform`]) for its driver to switch through the firmware
//! channel the kernel lends it.

use tairix_abi::driver::supply::{IoRail, PowerRail};
use tairix_fdt::{gpio_enabled_regulator, gpio_selected_regulator, supply, Fdt, GpioLine, Node};
use tairix_vcmailbox::FIRMWARE_GPIO_COMPATIBLE;

/// The SD signalling voltages an I/O rail selects between, in microvolts.
const SIGNAL_1V8_UV: u32 = 1_800_000;
const SIGNAL_3V3_UV: u32 = 3_300_000;

/// The rails `host`'s `vqmmc-supply` and `vmmc-supply` name.
///
/// `None` unless both are regulators the firmware's lines drive — a
/// `regulator-gpio` and a switchable `regulator-fixed`, each on an enabled
/// `raspberrypi,firmware-gpio` controller — so a board that wires either rail
/// otherwise offers no switchable supply at all rather than half of one.
pub(crate) fn sd_rails(fdt: &Fdt<'_>, host: &Node<'_>) -> Option<(IoRail, PowerRail)> {
    let signalling = gpio_selected_regulator(fdt, &supply(fdt, host, "vqmmc-supply")?)?;
    let card = supply(fdt, host, "vmmc-supply")?;
    // A rail the board keeps on cannot be cycled, and a failed voltage
    // switch is recovered only by cycling it.
    if card.property("regulator-always-on").is_some() {
        return None;
    }
    let power = gpio_enabled_regulator(fdt, &card)?;
    Some((
        IoRail {
            line: firmware_line(fdt, signalling.line())?,
            level_1v8: signalling.level_for(SIGNAL_1V8_UV),
            level_3v3: signalling.level_for(SIGNAL_3V3_UV),
            settle_us: signalling.settle_us(),
        },
        PowerRail {
            line: firmware_line(fdt, power.line())?,
            active_low: power.line().active_low,
            startup_us: power.startup_us(),
            off_on_us: power.off_on_us(),
        },
    ))
}

/// The expander line `line` names, when its controller is the firmware's.
fn firmware_line(fdt: &Fdt<'_>, line: GpioLine) -> Option<u8> {
    let controller = fdt.node_by_phandle(line.controller)?;
    let firmware = controller.is_enabled() && controller.is_compatible(FIRMWARE_GPIO_COMPATIBLE);
    firmware.then(|| u8::try_from(line.line).ok()).flatten()
}
