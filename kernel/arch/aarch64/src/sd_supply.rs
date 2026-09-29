//! The SD card's supplies where the firmware switches them: the card's power
//! rail and its I/O (signalling) rail, each a regulator driven by one line of
//! the firmware's GPIO expander.
//!
//! On a Pi 4 the EMMC2 node names both: `vmmc-supply` is a `regulator-fixed`
//! enabled by one expander line, `vqmmc-supply` a `regulator-gpio` whose
//! expander line selects 1.8 V or 3.3 V signalling. Both are read from the
//! tree ([`find_sd_supplies`]), never assumed, and driven over the firmware's
//! GPIO property tag ([`FirmwareSdSupply`]).
//!
//! The rails are reached only through the kernel's own mailbox transport, so
//! a consumer uses this while it is the doorbell's only user: the storage
//! floor's bring-up, which runs before the driver store that the `vcmailbox`
//! service is loaded from can be read.

use tairix_abi::driver::timing::Delay;
use tairix_abi::DriverError;
use tairix_fdt::{
    gpio_enabled_regulator, gpio_selected_regulator, supply, Fdt, GpioEnabledRegulator, GpioLine,
    GpioSelectedRegulator,
};
use tairix_vcmailbox::{set_gpio_state, MailboxTransport, FIRMWARE_GPIO_COMPATIBLE};

use crate::platform::EMMC2_COMPATIBLE;

/// The EMMC2 card's two firmware-switched rails, as the tree describes them.
#[derive(Copy, Clone, Debug)]
pub struct SdSupplies<'a> {
    signalling: GpioSelectedRegulator<'a>,
    signalling_line: u8,
    power: GpioEnabledRegulator,
    power_line: u8,
}

impl SdSupplies<'_> {
    /// The expander line that selects the card's I/O voltage.
    #[must_use]
    pub const fn signalling_line(&self) -> u8 {
        self.signalling_line
    }

    /// The expander line that switches the card's power.
    #[must_use]
    pub const fn power_line(&self) -> u8 {
        self.power_line
    }
}

/// Resolve the EMMC2 node's `vqmmc-supply` and `vmmc-supply` into
/// firmware-expander rails.
///
/// `None` unless both are enabled regulators this module can drive — a
/// `regulator-gpio` and a switchable `regulator-fixed`, each on an enabled
/// `raspberrypi,firmware-gpio` controller — so a board that wires either rail
/// otherwise offers no switchable supply at all rather than half of one.
#[must_use]
pub fn find_sd_supplies<'a>(fdt: &Fdt<'a>) -> Option<SdSupplies<'a>> {
    let host = fdt.find_compatible(EMMC2_COMPATIBLE)?;
    let signalling = gpio_selected_regulator(fdt, &supply(fdt, &host, "vqmmc-supply")?)?;
    let card = supply(fdt, &host, "vmmc-supply")?;
    // A rail the board keeps on cannot be cycled, and a failed voltage
    // switch is recovered only by cycling it.
    if card.property("regulator-always-on").is_some() {
        return None;
    }
    let power = gpio_enabled_regulator(fdt, &card)?;
    Some(SdSupplies {
        signalling_line: firmware_line(fdt, signalling.line())?,
        signalling,
        power_line: firmware_line(fdt, power.line())?,
        power,
    })
}

/// The expander line `line` names, when its controller is the firmware's.
fn firmware_line(fdt: &Fdt<'_>, line: GpioLine) -> Option<u8> {
    let controller = fdt.node_by_phandle(line.controller)?;
    let firmware = controller.is_enabled() && controller.is_compatible(FIRMWARE_GPIO_COMPATIBLE);
    firmware.then(|| u8::try_from(line.line).ok()).flatten()
}

/// The two rails, driven through the firmware over `firmware`.
///
/// Each switch waits out the regulator's own declared timing before it
/// returns: the signalling rail's settling time, the power rail's start-up
/// delay, and — on the way back on — whatever remains of its off-on delay.
pub struct FirmwareSdSupply<'s, 't> {
    firmware: &'t mut dyn MailboxTransport,
    supplies: SdSupplies<'s>,
    delay: &'t dyn Delay,
    /// When the power rail last went off, on `delay`'s clock.
    off_since_us: Option<u64>,
}

impl<'s, 't> FirmwareSdSupply<'s, 't> {
    /// Drive `supplies` over `firmware`, timing each switch with `delay`.
    #[must_use]
    pub fn new(
        firmware: &'t mut dyn MailboxTransport,
        supplies: SdSupplies<'s>,
        delay: &'t dyn Delay,
    ) -> Self {
        Self {
            firmware,
            supplies,
            delay,
            off_since_us: None,
        }
    }

    /// Select `microvolts` signalling on the I/O rail and wait for it to
    /// settle.
    ///
    /// # Errors
    ///
    /// [`DriverError::Unsupported`] when the regulator has no state for
    /// `microvolts`; the firmware's refusal otherwise, as its
    /// [`DriverError`].
    pub fn set_signalling(&mut self, microvolts: u32) -> Result<(), DriverError> {
        let level = self
            .supplies
            .signalling
            .level_for(microvolts)
            .ok_or(DriverError::Unsupported)?;
        set_gpio_state(self.firmware, self.supplies.signalling_line, level)
            .map_err(tairix_vcmailbox::MailboxError::as_driver_error)?;
        self.delay.delay_us(self.supplies.signalling.settle_us());
        Ok(())
    }

    /// Switch the card's power rail, waiting out the regulator's declared
    /// off-on and start-up delays on the way on.
    ///
    /// # Errors
    ///
    /// The firmware's refusal, as its [`DriverError`].
    pub fn set_power(&mut self, on: bool) -> Result<(), DriverError> {
        if on {
            if let Some(off_since) = self.off_since_us {
                let held = self.delay.now_us().saturating_sub(off_since);
                let owed = u64::from(self.supplies.power.off_on_us()).saturating_sub(held);
                self.delay.delay_us(u32::try_from(owed).unwrap_or(u32::MAX));
            }
        }
        let level = self.supplies.power.line().level(on);
        set_gpio_state(self.firmware, self.supplies.power_line, level)
            .map_err(tairix_vcmailbox::MailboxError::as_driver_error)?;
        if on {
            self.off_since_us = None;
            self.delay.delay_us(self.supplies.power.startup_us());
        } else {
            self.off_since_us = Some(self.delay.now_us());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use std::vec::Vec;
    use tairix_fdt::fixture::DtbBuilder;
    use tairix_vcmailbox::mock::MockFirmware;

    const EXPANDER: u32 = 0xb;

    fn cells(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// The Pi 4's SD supply wiring, with the power rail's `extra` property.
    fn wiring(controller: &str, card_extra: Option<(&str, u32)>) -> Vec<u8> {
        let mut b = DtbBuilder::new();
        b.begin_node("");
        b.begin_node("gpio");
        b.prop_str("compatible", controller);
        b.prop_u32("#gpio-cells", 2);
        b.prop_u32("phandle", EXPANDER);
        b.end_node();
        b.begin_node("regulator-sd-io-1v8");
        b.prop_str("compatible", "regulator-gpio");
        b.prop("gpios", &cells(&[EXPANDER, 4, 0]));
        b.prop("states", &cells(&[1_800_000, 1, 3_300_000, 0]));
        b.prop_u32("regulator-settling-time-us", 5000);
        b.prop_u32("phandle", 0x37);
        b.end_node();
        b.begin_node("regulator-sd-vcc");
        b.prop_str("compatible", "regulator-fixed");
        b.prop("enable-active-high", &[]);
        b.prop("gpio", &cells(&[EXPANDER, 6, 0]));
        if let Some((name, value)) = card_extra {
            b.prop_u32(name, value);
        }
        b.prop_u32("phandle", 0x38);
        b.end_node();
        b.begin_node("emmc2bus");
        b.begin_node("mmc@7e340000");
        b.prop_str("compatible", "brcm,bcm2711-emmc2");
        b.prop_u32("vqmmc-supply", 0x37);
        b.prop_u32("vmmc-supply", 0x38);
        b.end_node();
        b.end_node();
        b.end_node();
        b.build()
    }

    /// A delay that only advances a virtual clock and records each wait.
    #[derive(Default)]
    struct RecordingDelay {
        now_us: Cell<u64>,
        waits: RefCell<Vec<u32>>,
    }

    impl Delay for RecordingDelay {
        fn delay_us(&self, us: u32) {
            self.now_us.set(self.now_us.get() + u64::from(us));
            self.waits.borrow_mut().push(us);
        }

        fn now_us(&self) -> u64 {
            self.now_us.get()
        }
    }

    #[test]
    fn the_pi4_rails_are_the_firmware_expander_lines_the_tree_names() {
        let blob = wiring("raspberrypi,firmware-gpio", None);
        let fdt = Fdt::new(&blob).expect("fdt");
        let supplies = find_sd_supplies(&fdt).expect("both rails");
        assert_eq!(supplies.signalling_line, 4);
        assert_eq!(supplies.power_line, 6);
    }

    #[test]
    fn a_rail_on_another_controller_offers_no_supply() {
        let blob = wiring("brcm,bcm2711-gpio", None);
        let fdt = Fdt::new(&blob).expect("fdt");
        assert!(find_sd_supplies(&fdt).is_none());
    }

    #[test]
    fn a_power_rail_kept_on_offers_no_supply() {
        let blob = wiring(
            "raspberrypi,firmware-gpio",
            Some(("regulator-always-on", 0)),
        );
        let fdt = Fdt::new(&blob).expect("fdt");
        assert!(find_sd_supplies(&fdt).is_none());
    }

    #[test]
    fn signalling_drives_the_select_line_and_waits_for_it_to_settle() {
        let blob = wiring("raspberrypi,firmware-gpio", None);
        let fdt = Fdt::new(&blob).expect("fdt");
        let mut firmware = MockFirmware::healthy();
        let delay = RecordingDelay::default();
        let mut rails = FirmwareSdSupply::new(
            &mut firmware,
            find_sd_supplies(&fdt).expect("rails"),
            &delay,
        );
        rails.set_signalling(1_800_000).expect("1.8 V");
        assert_eq!(*delay.waits.borrow(), [5000]);
        assert_eq!(
            rails.set_signalling(2_500_000),
            Err(DriverError::Unsupported),
            "a voltage the regulator has no state for"
        );
        assert!(firmware.gpio_high(4), "line 4 high selects 1.8 V");
    }

    #[test]
    fn power_back_on_waits_out_the_rest_of_the_off_on_delay() {
        let blob = wiring("raspberrypi,firmware-gpio", Some(("off-on-delay-us", 3000)));
        let fdt = Fdt::new(&blob).expect("fdt");
        let mut firmware = MockFirmware::healthy();
        firmware.gpio_levels = 1 << 6;
        let delay = RecordingDelay::default();
        let mut rails = FirmwareSdSupply::new(
            &mut firmware,
            find_sd_supplies(&fdt).expect("rails"),
            &delay,
        );
        rails.set_power(false).expect("off");
        delay.delay_us(1000);
        rails.set_power(true).expect("on");
        assert_eq!(
            *delay.waits.borrow(),
            [1000, 2000, 0],
            "the rest of the off-on delay, then the undeclared start-up"
        );
        assert!(firmware.gpio_high(6), "enable-active-high: on is high");
    }

    #[test]
    fn a_refused_switch_is_the_firmwares_error() {
        let blob = wiring("raspberrypi,firmware-gpio", None);
        let fdt = Fdt::new(&blob).expect("fdt");
        let mut firmware = MockFirmware::healthy();
        firmware.gpio_lines = 4;
        let delay = RecordingDelay::default();
        let mut rails = FirmwareSdSupply::new(
            &mut firmware,
            find_sd_supplies(&fdt).expect("rails"),
            &delay,
        );
        assert_eq!(
            rails.set_signalling(1_800_000),
            Err(DriverError::DeviceFault)
        );
        assert_eq!(rails.set_power(false), Err(DriverError::DeviceFault));
        assert!(
            delay.waits.borrow().is_empty(),
            "nothing settles that never moved"
        );
    }
}
