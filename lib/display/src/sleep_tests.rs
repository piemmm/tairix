//! Host tests of a display's sleep: what each answer from the display means,
//! and which display is switched back on.

use alloc::vec::Vec;

use tairix_abi::driver::display::{Display, DisplayFormat, DisplayMode, DisplayPower};
use tairix_abi::DriverError;

use super::{DisplaySleep, SwitchedOff};

/// A panel that records the switches asked of it and refuses as told.
struct Panel {
    refuse: Option<DriverError>,
    switches: Vec<DisplayPower>,
}

impl Panel {
    fn switchable() -> Self {
        Self {
            refuse: None,
            switches: Vec::new(),
        }
    }

    fn refusing(refusal: DriverError) -> Self {
        Self {
            refuse: Some(refusal),
            ..Self::switchable()
        }
    }
}

impl Display for Panel {
    fn mode_info(&self) -> Result<DisplayMode, DriverError> {
        Ok(DisplayMode {
            width_px: 4,
            height_px: 4,
            stride_bytes: 16,
            format: DisplayFormat::Rgba8888,
        })
    }

    fn present(&mut self, _frame: &[u8]) -> Result<(), DriverError> {
        Ok(())
    }

    fn set_power(&mut self, power: DisplayPower) -> Result<(), DriverError> {
        if let Some(refusal) = self.refuse {
            return Err(refusal);
        }
        self.switches.push(power);
        Ok(())
    }
}

#[test]
fn a_display_that_switches_off_is_off_until_it_is_switched_back_on() {
    let mut panel = Panel::switchable();
    let mut sleep = DisplaySleep::new();
    assert!(sleep.is_awake());
    assert_eq!(
        sleep.switch_off(Some(&mut panel), true),
        Some(SwitchedOff::Off)
    );
    assert!(sleep.is_off());
    assert!(!sleep.is_awake());
    assert_eq!(
        sleep.switch_off(Some(&mut panel), true),
        None,
        "a sleeping display is not asked again"
    );
    assert_eq!(sleep.wake(Some(&mut panel)), Ok(true));
    assert!(sleep.is_awake());
    assert_eq!(panel.switches, [DisplayPower::Off, DisplayPower::On]);
    assert_eq!(sleep.wake(Some(&mut panel)), Ok(false), "awake already");
    assert_eq!(panel.switches.len(), 2, "an awake display is not switched");
}

/// No power control, or none implemented, is not a fault: the presenter keeps
/// the screen black instead, and nothing is switched back on.
#[test]
fn a_display_with_no_power_control_sleeps_black() {
    for refusal in [DriverError::Unsupported, DriverError::NotImplemented] {
        let mut panel = Panel::refusing(refusal);
        let mut sleep = DisplaySleep::new();
        assert_eq!(
            sleep.switch_off(Some(&mut panel), true),
            Some(SwitchedOff::Blanked)
        );
        assert!(!sleep.is_awake());
        assert!(!sleep.is_off(), "a black screen is still presented");
        panel.refuse = None;
        assert_eq!(sleep.wake(Some(&mut panel)), Ok(true));
        assert!(panel.switches.is_empty(), "nothing to switch back on");
    }
}

#[test]
fn a_refusal_is_named_and_the_display_sleeps_black() {
    let mut panel = Panel::refusing(DriverError::DeviceFault);
    let mut sleep = DisplaySleep::new();
    assert_eq!(
        sleep.switch_off(Some(&mut panel), true),
        Some(SwitchedOff::Refused(DriverError::DeviceFault))
    );
    assert!(!sleep.is_awake());
    assert!(!sleep.is_off());
}

/// With nothing to keep black over it, a display that would not switch off
/// is simply left awake: its presenter carries on, and input is its own.
#[test]
fn a_refusal_with_nothing_to_blank_leaves_the_display_awake() {
    for mut panel in [
        Panel::refusing(DriverError::Unsupported),
        Panel::refusing(DriverError::DeviceFault),
    ] {
        let mut sleep = DisplaySleep::new();
        assert!(sleep.switch_off(Some(&mut panel), false).is_some());
        assert!(sleep.is_awake());
    }
}

/// A display that will not light again stays off, and the next wake asks
/// again rather than presenting to a screen nobody can see.
#[test]
fn a_display_that_will_not_switch_back_on_stays_off_and_is_asked_again() {
    let mut panel = Panel::switchable();
    let mut sleep = DisplaySleep::new();
    let _ = sleep.switch_off(Some(&mut panel), true);
    panel.refuse = Some(DriverError::DeviceFault);
    assert_eq!(sleep.wake(Some(&mut panel)), Err(DriverError::DeviceFault));
    assert!(sleep.is_off());
    panel.refuse = None;
    assert_eq!(sleep.wake(Some(&mut panel)), Ok(true));
    assert_eq!(panel.switches, [DisplayPower::Off, DisplayPower::On]);
}

/// Without a display to ask there is no power control: the sleep is black,
/// and it ends without switching anything.
#[test]
fn without_a_display_the_sleep_is_black_and_ends_without_a_switch() {
    let mut sleep = DisplaySleep::new();
    assert_eq!(sleep.switch_off(None, true), Some(SwitchedOff::Blanked));
    assert!(!sleep.is_awake());
    assert_eq!(sleep.wake(None), Ok(true));
    assert!(sleep.is_awake());

    let mut panel = Panel::switchable();
    let _ = sleep.switch_off(Some(&mut panel), true);
    assert_eq!(
        sleep.wake(None),
        Ok(true),
        "a display handed on is lit for its next owner"
    );
    assert_eq!(panel.switches, [DisplayPower::Off]);
}
